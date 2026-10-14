// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Password manager + browser import, shell side (see `crates/passwords` and NOTES).
//!
//! * The page script (`mythbrowse_passwords::page`) is one more user script in the shared
//!   `UserContentManager`. Its console messages are taken out in `show_console_message`
//!   before anything else sees them (never logged, never counted by the compat detector).
//! * Messages from the AI agent's background tabs are ignored, and so are messages from a
//!   user tab while Myth works in it: the agent can never cause a save prompt or a fill.
//! * Filling happens only from the passwords panel in the browser's own UI (chrome-only RPC,
//!   which pages and the agent cannot reach), into the top-level document of the active tab,
//!   after re-checking that the login matches the page's origin. Passwords filled into a tab
//!   are remembered (in memory) and scrubbed from anything an agent script reads there.
//! * A focus on a login field opens the account list only right after real keyboard / mouse
//!   input reached the window (a page calling `el.focus()` gets a trusted focus event too).
//! * Reveal / edit / export need a fresh OS re-authentication (`mythbrowse_ai::reauth`) or,
//!   where the OS has none, an in-app confirmation; the grant lasts one minute.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use http::StatusCode;
use log::{info, warn};
use mythbrowse_ai::reauth::{self, Verdict};
use mythbrowse_ai::secrets::{MemoryStore, PlatformStore, Secret, SecretStore};
use mythbrowse_engine::chrome;
use mythbrowse_engine::passwords_api::{PasswordOp, PromptChoice};
use mythbrowse_engine::servo::{JSValue, UserContentManager, UserScript, WebView};
use mythbrowse_engine::tabs::TabId;
use mythbrowse_passwords::csv::CsvLogin;
use mythbrowse_passwords::import::{
    self, Browser, ImportedBookmark, ImportedVisit, PasswordReport,
};
use mythbrowse_passwords::page::{self, PageKeys, PageMsg};
use mythbrowse_passwords::{LoginStore, MatchKind, Origin, SaveCheck, StoreError};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::AppState;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// How long a re-authentication unlocks reveal / edit / export.
const REAUTH_FOR: Duration = Duration::from_secs(60);
/// A focus on a login field opens the account list only this soon after real user input.
const GESTURE_WINDOW: Duration = Duration::from_millis(1500);
/// A save prompt nobody answered goes away after this long.
const PROMPT_TTL: Duration = Duration::from_secs(180);
/// History entries taken from another browser.
const HISTORY_IMPORT_LIMIT: usize = 20_000;

enum Msg {
    Reauth(Verdict),
    Import(Box<ImportOutcome>),
}

#[derive(Default)]
struct ImportOutcome {
    label: String,
    bookmarks: Vec<ImportedBookmark>,
    visits: Vec<ImportedVisit>,
    logins: Vec<CsvLogin>,
    never: Vec<String>,
    report: PasswordReport,
    wanted_passwords: bool,
    errors: Vec<String>,
}

struct Prompt {
    tab: TabId,
    url: String,
    origin: Origin,
    action: String,
    username: String,
    password: Secret,
    update: Option<i64>,
    at: Instant,
}

#[derive(Default)]
struct PwPage {
    origin: Option<Origin>,
    forms: u32,
    suggested: bool,
}

pub(crate) struct PasswordHost {
    store: RefCell<Option<LoginStore>>,
    /// Why there is no store ("locked", or an error), for the UI.
    problem: RefCell<Option<String>>,
    keystore: Box<dyn SecretStore>,
    profile_dir: Option<PathBuf>,
    keys: PageKeys,
    _script: Option<Rc<UserScript>>,
    pages: RefCell<BTreeMap<u64, PwPage>>,
    prompt: RefCell<Option<Prompt>>,
    /// The accounts panel is open over the active page.
    open: Cell<bool>,
    notice: RefCell<Option<String>>,
    last_input: Cell<Option<Instant>>,
    reauth_until: Cell<Option<Instant>>,
    reauth_pending: Cell<bool>,
    /// The OS has no verification prompt (the in-app confirmation is accepted).
    os_reauth_missing: Cell<bool>,
    reauth_note: RefCell<Option<String>>,
    import_running: Cell<bool>,
    import_result: RefCell<Option<Value>>,
    /// Passwords filled into each user tab (scrubbed from agent reads there).
    filled: RefCell<BTreeMap<u64, Vec<Secret>>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    wake: Wake,
}

impl PasswordHost {
    pub(crate) fn new(
        user_content: &UserContentManager,
        profile_dir: Option<&Path>,
        wake: Wake,
    ) -> Self {
        let enabled = std::env::var("MYTHBROWSE_PASSWORDS").map_or(true, |v| v != "0");
        let keystore: Box<dyn SecretStore> = match profile_dir {
            Some(dir) => Box::new(PlatformStore::new(dir)),
            None => Box::new(MemoryStore::default()),
        };
        let (store, problem) = if !enabled {
            (
                None,
                Some("Saving passwords is off (MYTHBROWSE_PASSWORDS=0).".to_owned()),
            )
        } else {
            match profile_dir {
                Some(dir) => match LoginStore::open_in_dir(dir, keystore.as_ref()) {
                    Ok(s) => (Some(s), None),
                    Err(StoreError::Locked) => (None, Some("locked".to_owned())),
                    Err(e) => (None, Some(format!("Saved passwords are unavailable: {e}"))),
                },
                None => (
                    None,
                    Some("No profile folder: passwords cannot be saved.".to_owned()),
                ),
            }
        };
        match (&store, &problem) {
            (Some(s), _) => info!(
                "passwords: {} saved, key in {}",
                s.len().unwrap_or(0),
                keystore.backend()
            ),
            (None, Some(p)) => warn!("passwords: {p}"),
            _ => {}
        }
        let keys = PageKeys::random();
        let script = enabled.then(|| {
            let s = Rc::new(UserScript::new(page::page_script(&keys), None));
            user_content.add_script(s.clone());
            s
        });
        let (tx, rx) = channel();
        Self {
            store: RefCell::new(store),
            problem: RefCell::new(problem),
            keystore,
            profile_dir: profile_dir.map(Path::to_path_buf),
            keys,
            _script: script,
            pages: RefCell::new(BTreeMap::new()),
            prompt: RefCell::new(None),
            open: Cell::new(false),
            notice: RefCell::new(None),
            last_input: Cell::new(None),
            reauth_until: Cell::new(None),
            reauth_pending: Cell::new(false),
            os_reauth_missing: Cell::new(!cfg!(windows)),
            reauth_note: RefCell::new(None),
            import_running: Cell::new(false),
            import_result: RefCell::new(None),
            filled: RefCell::new(BTreeMap::new()),
            tx,
            rx,
            wake,
        }
    }

    fn reauthed(&self) -> bool {
        self.reauth_until.get().is_some_and(|t| Instant::now() < t)
    }
}

/// `%USERPROFILE%\Downloads` / `$XDG_DOWNLOAD_DIR` / `~/Downloads`.
fn downloads_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(d) = var("MYTHBROWSE_DOWNLOADS_DIR") {
        return Some(d);
    }
    if cfg!(windows) {
        return var("USERPROFILE").map(|h| h.join("Downloads"));
    }
    var("XDG_DOWNLOAD_DIR").or_else(|| var("HOME").map(|h| h.join("Downloads")))
}

/// A file name in `dir` that does not exist yet: `base.csv`, `base (2).csv`, ...
fn free_name(dir: &Path, base: &str) -> PathBuf {
    let first = dir.join(format!("{base}.csv"));
    if !first.exists() {
        return first;
    }
    (2..1000)
        .map(|n| dir.join(format!("{base} ({n}).csv")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    std::io::Write::write_all(&mut f, data)?;
    f.sync_all()
}

fn host_of(origin: &str) -> String {
    Origin::parse(origin).map_or_else(|| origin.to_owned(), |o| o.host)
}

impl AppState {
    // ------------------------------------------------------------------ page messages

    /// Real keyboard / mouse input reached the window (never the agent's DOM actions).
    pub(crate) fn pw_note_input(&self) {
        self.passwords.last_input.set(Some(Instant::now()));
    }

    fn pw_recent_input(&self) -> bool {
        self.passwords
            .last_input
            .get()
            .is_some_and(|t| t.elapsed() <= GESTURE_WINDOW)
    }

    fn pw_tab_of(&self, webview: &WebView) -> Option<TabId> {
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.view.as_ref() == Some(webview))
            .map(|t| t.id)
    }

    /// A console message from the page script (`page::is_pw_message`). Never logged.
    pub(crate) fn pw_console(&self, webview: &WebView, message: &str) {
        if self.is_chrome(webview) || self.agent.owns(webview) {
            return; // the agent's background tabs never save or fill
        }
        let Some(tab) = self.pw_tab_of(webview) else {
            return;
        };
        let Some(msg) = page::parse_message(message, &self.passwords.keys) else {
            return;
        };
        let current = webview.url().and_then(|u| Origin::from_url(&u));
        let same = |url: &str| current.is_some() && Origin::parse(url) == current;
        let myth_here = self.agent_activity(tab).is_some();
        match msg {
            PageMsg::Forms { url, count, .. } => {
                if !same(&url) {
                    return;
                }
                {
                    let mut pages = self.passwords.pages.borrow_mut();
                    let p = pages.entry(tab.0).or_default();
                    if p.origin != current {
                        *p = PwPage::default();
                    }
                    p.origin = current.clone();
                    p.forms = count;
                }
                let logins = self.pw_count_for(&url);
                if logins > 0 {
                    webview.evaluate_javascript(
                        page::has_script(&self.passwords.keys, logins),
                        |_| {},
                    );
                }
                self.mark_chrome_dirty();
            }
            PageMsg::Focus { url } => self.pw_suggest(tab, &url, false, same(&url) && !myth_here),
            PageMsg::Key { url } => self.pw_suggest(tab, &url, true, same(&url) && !myth_here),
            PageMsg::Submit {
                url,
                action,
                username,
                password,
                ..
            } => {
                if myth_here {
                    return; // Myth typed it (with the user's OK): no save prompt for that
                }
                let recorded = self
                    .passwords
                    .pages
                    .borrow()
                    .get(&tab.0)
                    .and_then(|p| p.origin.clone());
                let Some(origin) = Origin::parse(&url) else {
                    return;
                };
                if Some(&origin) != current.as_ref() && Some(&origin) != recorded.as_ref() {
                    return;
                }
                let check = match self.passwords.store.borrow().as_ref() {
                    Some(s) => s.check_submit(&url, &username, &password),
                    None => return,
                };
                match check {
                    Ok(SaveCheck::New) | Ok(SaveCheck::Update { .. }) => {
                        let update = match check {
                            Ok(SaveCheck::Update { id }) => Some(id),
                            _ => None,
                        };
                        *self.passwords.prompt.borrow_mut() = Some(Prompt {
                            tab,
                            url,
                            origin,
                            action,
                            username,
                            password,
                            update,
                            at: Instant::now(),
                        });
                        self.passwords.open.set(false);
                        self.compat_close_panel();
                        self.mark_chrome_dirty();
                        self.window.request_redraw();
                    }
                    Ok(SaveCheck::Same { id }) => {
                        if let Some(s) = self.passwords.store.borrow().as_ref() {
                            let _ = s.mark_used(id);
                        }
                    }
                    Ok(SaveCheck::Never) => {}
                    Err(e) => warn!("passwords: save check failed: {e}"),
                }
            }
        }
    }

    /// The user focused a login field (or clicked its key icon): open the account list once
    /// per page on focus, every time on the key icon. Only right after real user input.
    fn pw_suggest(&self, tab: TabId, url: &str, key: bool, allowed: bool) {
        let active = self.tabs.borrow().active().map(|t| t.id) == Some(tab);
        if !allowed || !active || !self.pw_recent_input() || self.pw_count_for(url) == 0 {
            return;
        }
        let first = {
            let mut pages = self.passwords.pages.borrow_mut();
            let p = pages.entry(tab.0).or_default();
            !std::mem::replace(&mut p.suggested, true)
        };
        if key || first {
            self.pw_open_panel(true);
        }
    }

    fn pw_count_for(&self, url: &str) -> usize {
        self.passwords
            .store
            .borrow()
            .as_ref()
            .and_then(|s| s.matches(url).ok())
            .map_or(0, |m| m.len())
    }

    pub(crate) fn pw_tab_closed(&self, tab: TabId) {
        self.passwords.pages.borrow_mut().remove(&tab.0);
        self.passwords.filled.borrow_mut().remove(&tab.0);
        let mut prompt = self.passwords.prompt.borrow_mut();
        if prompt.as_ref().is_some_and(|p| p.tab == tab) {
            *prompt = None;
        }
    }

    /// The passwords filled into this user tab, to scrub from what the agent reads there.
    pub(crate) fn pw_filled_secrets(&self, tab: TabId) -> Vec<Secret> {
        self.passwords
            .filled
            .borrow()
            .get(&tab.0)
            .cloned()
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------ panel

    fn pw_open_panel(&self, open: bool) {
        let open = open && self.passwords.store.borrow().is_some();
        if self.passwords.open.replace(open) != open {
            if open {
                self.compat_close_panel();
            }
            self.mark_chrome_dirty();
            self.window.request_redraw();
        }
    }

    /// Close the panel; a pending prompt for the active tab counts as "Not now".
    pub(crate) fn pw_close_panel(&self) {
        let active = self.tabs.borrow().active().map(|t| t.id);
        let had_prompt = self
            .passwords
            .prompt
            .borrow()
            .as_ref()
            .is_some_and(|p| Some(p.tab) == active);
        if had_prompt {
            *self.passwords.prompt.borrow_mut() = None;
        }
        if self.passwords.open.replace(false) || had_prompt {
            self.mark_chrome_dirty();
            self.window.request_redraw();
        }
    }

    /// The pending prompt for the active tab `(site, username, update)`, dropping a stale one
    /// (the tab moved to another site, or nobody answered for a while).
    fn pw_active_prompt(&self) -> Option<(String, String, bool)> {
        let (active, url) = {
            let tabs = self.tabs.borrow();
            let t = tabs.active()?;
            (t.id, t.url.clone())
        };
        let mut slot = self.passwords.prompt.borrow_mut();
        let p = slot.as_ref()?;
        if p.tab != active {
            return None;
        }
        let stale = p.at.elapsed() > PROMPT_TTL
            || Origin::parse(&url).is_none_or(|o| o.site() != p.origin.site());
        if stale {
            *slot = None;
            return None;
        }
        Some((
            p.origin.host.clone(),
            p.username.clone(),
            p.update.is_some(),
        ))
    }

    /// Whether the passwords panel floats over the card now.
    pub(crate) fn pw_panel_open(&self) -> bool {
        self.card_mode() && (self.passwords.open.get() || self.pw_active_prompt().is_some())
    }

    /// `ChromeState::passwords` (usernames and sites only).
    pub(crate) fn pw_state(&self) -> Option<Value> {
        let (tab, url) = {
            let tabs = self.tabs.borrow();
            let t = tabs.active()?;
            (t.id, t.url.clone())
        };
        let origin = Origin::parse(&url)?;
        let accounts: Vec<Value> = self
            .passwords
            .store
            .borrow()
            .as_ref()
            .and_then(|s| s.matches(&url).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                json!({
                    "id": m.login.id,
                    "username": m.login.username,
                    "site": host_of(&m.login.origin),
                    "kind": m.kind,
                })
            })
            .collect();
        let forms = self
            .passwords
            .pages
            .borrow()
            .get(&tab.0)
            .filter(|p| p.origin.as_ref() == Some(&origin))
            .map_or(0, |p| p.forms);
        let prompt = self
            .pw_active_prompt()
            .map(|(site, username, update)| json!({ "site": site, "username": username, "update": update }));
        Some(json!({
            "available": self.passwords.store.borrow().is_some(),
            "locked": self.passwords.problem.borrow().as_deref() == Some("locked"),
            "site": origin.host,
            "forms": forms,
            "count": accounts.len(),
            "accounts": accounts,
            "open": self.passwords.open.get(),
            "prompt": prompt,
            "myth": self.agent_activity(tab).is_some(),
            "notice": self.passwords.notice.borrow().clone(),
        }))
    }

    fn pw_fill(&self, id: i64) -> Result<(), String> {
        let (tab, view) = {
            let tabs = self.tabs.borrow();
            let t = tabs.active().ok_or("No page")?;
            (t.id, t.view.clone().ok_or("The page is not loaded.")?)
        };
        if self.agent_activity(tab).is_some() {
            return Err(
                "Myth is working in this tab. Stop it first, then fill the password.".into(),
            );
        }
        let live = view.url().ok_or("The page is not loaded.")?;
        let page_origin =
            Origin::from_url(&live).ok_or("Passwords are only filled on web pages.")?;
        let store = self.passwords.store.borrow();
        let store = store.as_ref().ok_or("Saved passwords are unavailable.")?;
        // Only a login that may be used on this exact page (origin rules).
        let m = store
            .matches(live.as_str())
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|m| m.login.id == id)
            .ok_or("That login is not for this site.")?;
        let password = store.password(id).map_err(|e| e.to_string())?;
        let script = page::fill_script(&page_origin.to_string(), &m.login.username, &password);
        self.passwords
            .filled
            .borrow_mut()
            .entry(tab.0)
            .or_default()
            .push(password);
        let _ = store.mark_used(id);
        if m.kind == MatchKind::SameSite {
            info!("passwords: filled a login saved for another host of the same site");
        }
        view.evaluate_javascript(script.as_str(), |r| {
            if !matches!(r, Ok(JSValue::String(ref s)) if s == "ok") {
                log::debug!("passwords: fill did not complete");
            }
        });
        Ok(())
    }

    fn pw_answer_prompt(&self, choice: PromptChoice) -> Result<Value, String> {
        let active = self.tabs.borrow().active().map(|t| t.id);
        let p = {
            let mut slot = self.passwords.prompt.borrow_mut();
            match slot.as_ref() {
                Some(p) if Some(p.tab) == active => slot.take(),
                _ => None,
            }
        }
        .ok_or("Nothing to save.")?;
        let out = {
            let store = self.passwords.store.borrow();
            let store = store.as_ref().ok_or("Saved passwords are unavailable.")?;
            match choice {
                PromptChoice::Save => {
                    let saved = store
                        .save(&p.url, &p.action, &p.username, &p.password, "saved")
                        .map_err(|e| e.to_string())?;
                    json!({ "saved": saved.id() })
                }
                PromptChoice::Never => {
                    store.add_never(&p.url).map_err(|e| e.to_string())?;
                    json!({ "never": p.origin.to_string() })
                }
                PromptChoice::NotNow => Value::Null,
            }
        };
        // The page's key icon learns the new count.
        if let Some(view) = self.active_view() {
            let n = self.pw_count_for(&p.url);
            view.evaluate_javascript(page::has_script(&self.passwords.keys, n), |_| {});
        }
        self.mark_chrome_dirty();
        self.window.request_redraw();
        Ok(out)
    }

    // ------------------------------------------------------------------ RPC

    fn pw_store_error(&self) -> String {
        match self.passwords.problem.borrow().as_deref() {
            Some("locked") => {
                "Saved passwords are locked: their key is not available for this OS account."
                    .to_owned()
            }
            Some(p) => p.to_owned(),
            None => "Saved passwords are unavailable.".to_owned(),
        }
    }

    pub(crate) fn pw_api(&self, op: PasswordOp, secret: Option<Secret>) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, chrome::json_ok(v));
        let err = |m: &str| (StatusCode::CONFLICT, chrome::json_error(m));
        let has_store = self.passwords.store.borrow().is_some();
        let needs_store = matches!(
            op,
            PasswordOp::List { .. }
                | PasswordOp::Reveal { .. }
                | PasswordOp::Update { .. }
                | PasswordOp::Delete { .. }
                | PasswordOp::Export
                | PasswordOp::NeverList
                | PasswordOp::NeverDelete { .. }
                | PasswordOp::ImportCsv { .. }
                | PasswordOp::ImportRun {
                    passwords: true,
                    ..
                }
        );
        if needs_store && !has_store {
            return err(&self.pw_store_error());
        }
        let needs_auth = matches!(
            op,
            PasswordOp::Reveal { .. } | PasswordOp::Update { .. } | PasswordOp::Export
        );
        if needs_auth && !self.passwords.reauthed() {
            return (StatusCode::FORBIDDEN, chrome::json_error("reauth"));
        }
        let store = || self.passwords.store.borrow();
        match op {
            PasswordOp::Panel { open } => {
                if open {
                    self.pw_open_panel(true);
                } else {
                    self.pw_close_panel();
                }
                ok(Value::Null)
            }
            PasswordOp::Fill { id } => match self.pw_fill(id) {
                Ok(()) => {
                    *self.passwords.notice.borrow_mut() = None;
                    self.pw_close_panel();
                    ok(Value::Null)
                }
                Err(e) => {
                    *self.passwords.notice.borrow_mut() = Some(e.clone());
                    self.mark_chrome_dirty();
                    err(&e)
                }
            },
            PasswordOp::Prompt { choice } => match self.pw_answer_prompt(choice) {
                Ok(v) => ok(v),
                Err(e) => err(&e),
            },
            PasswordOp::Status => ok(self.pw_status()),
            PasswordOp::List { q } => match store().as_ref().expect("checked").list(&q) {
                Ok(list) => ok(json!({ "logins": list })),
                Err(e) => err(&e.to_string()),
            },
            PasswordOp::Reauth { confirm } => self.pw_reauth(confirm),
            PasswordOp::Reveal { id } => {
                let s = store();
                let s = s.as_ref().expect("checked");
                match (s.password(id), s.note(id)) {
                    (Ok(p), Ok(note)) => ok(json!({ "password": p.expose(), "note": note })),
                    (Err(e), _) | (_, Err(e)) => err(&e.to_string()),
                }
            }
            PasswordOp::Update { id, username, note } => {
                // The page sends the new password as a form (`p=...`) in the secret header,
                // so any Unicode survives HTTP header rules.
                let password = secret.as_ref().and_then(|raw| {
                    url::form_urlencoded::parse(raw.expose().as_bytes())
                        .find(|(k, _)| k == "p")
                        .map(|(_, v)| Secret::new(v.into_owned()))
                        .filter(|p| !p.expose().is_empty())
                });
                match store().as_ref().expect("checked").update(
                    id,
                    username.as_deref(),
                    password.as_ref(),
                    note.as_deref(),
                ) {
                    Ok(()) => ok(Value::Null),
                    Err(e) => err(&e.to_string()),
                }
            }
            PasswordOp::Delete { id } => match store().as_ref().expect("checked").delete(id) {
                Ok(_) => ok(Value::Null),
                Err(e) => err(&e.to_string()),
            },
            PasswordOp::Export => {
                self.passwords.reauth_until.set(None); // one export per confirmation
                let Some(dir) = downloads_dir() else {
                    return err("No Downloads folder found.");
                };
                let rows = match store().as_ref().expect("checked").export() {
                    Ok(r) => r,
                    Err(e) => return err(&e.to_string()),
                };
                let text = mythbrowse_passwords::csv::write_logins(&rows);
                let _ = std::fs::create_dir_all(&dir);
                let path = free_name(&dir, "MythBrowse Passwords");
                match write_private(&path, text.as_bytes()) {
                    Ok(()) => {
                        info!("passwords: exported {} logins to Downloads", rows.len());
                        ok(json!({ "path": path.display().to_string(), "count": rows.len() }))
                    }
                    Err(e) => err(&format!("Could not write the file: {e}")),
                }
            }
            PasswordOp::NeverList => match store().as_ref().expect("checked").never_list() {
                Ok(v) => ok(json!({ "sites": v })),
                Err(e) => err(&e.to_string()),
            },
            PasswordOp::NeverDelete { origin } => {
                match store().as_ref().expect("checked").remove_never(&origin) {
                    Ok(_) => ok(Value::Null),
                    Err(e) => err(&e.to_string()),
                }
            }
            PasswordOp::Reset => {
                if self.passwords.problem.borrow().as_deref() != Some("locked") {
                    return err("Saved passwords are not locked.");
                }
                let Some(dir) = self.passwords.profile_dir.clone() else {
                    return err("No profile folder.");
                };
                match LoginStore::reset_in_dir(&dir, self.passwords.keystore.as_ref()) {
                    Ok(s) => {
                        *self.passwords.store.borrow_mut() = Some(s);
                        *self.passwords.problem.borrow_mut() = None;
                        ok(Value::Null)
                    }
                    Err(e) => err(&e.to_string()),
                }
            }
            PasswordOp::ImportSources => {
                let profiles: Vec<Value> = import::find_profiles()
                    .iter()
                    .map(|p| {
                        json!({
                            "browser": p.browser,
                            "label": p.browser.label(),
                            "dir": p.dir,
                            "name": p.name,
                            "bookmarks": p.bookmarks,
                            "history": p.history,
                            "passwords": p.passwords,
                        })
                    })
                    .collect();
                ok(json!({
                    "profiles": profiles,
                    "direct_passwords": import::DIRECT_PASSWORDS,
                    "running": self.passwords.import_running.get(),
                    "result": self.passwords.import_result.borrow().clone(),
                }))
            }
            PasswordOp::ImportRun {
                browser,
                profile,
                bookmarks,
                history,
                passwords,
            } => {
                if !(bookmarks || history || passwords) {
                    return err("Choose what to import.");
                }
                let Some(browser) = Browser::parse(&browser) else {
                    return err("Unknown browser.");
                };
                if self.passwords.import_running.replace(true) {
                    return err("An import is already running.");
                }
                *self.passwords.import_result.borrow_mut() = None;
                let (tx, wake) = (self.passwords.tx.clone(), self.passwords.wake.clone());
                let spawned =
                    std::thread::Builder::new()
                        .name("pw-import".into())
                        .spawn(move || {
                            let out = run_import(browser, &profile, bookmarks, history, passwords);
                            let _ = tx.send(Msg::Import(Box::new(out)));
                            wake();
                        });
                if spawned.is_err() {
                    self.passwords.import_running.set(false);
                    return err("Could not start the import.");
                }
                ok(json!({ "running": true }))
            }
            PasswordOp::ImportCsv { path } => self.pw_import_csv(&path),
            PasswordOp::ImportCsvFiles => {
                let mut files: Vec<(std::time::SystemTime, String)> = downloads_dir()
                    .and_then(|d| std::fs::read_dir(d).ok())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_lowercase();
                        let wanted = name.ends_with(".csv")
                            && (name.contains("password") || name.contains("login"));
                        if !wanted {
                            return None;
                        }
                        let t = e.metadata().and_then(|m| m.modified()).ok()?;
                        Some((t, e.path().display().to_string()))
                    })
                    .collect();
                files.sort_by(|a, b| b.0.cmp(&a.0));
                ok(json!({
                    "files": files.into_iter().take(10).map(|(_, p)| p).collect::<Vec<_>>(),
                    "downloads": downloads_dir().map(|d| d.display().to_string()),
                }))
            }
        }
    }

    fn pw_reauth(&self, confirm: bool) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, chrome::json_ok(v));
        if confirm {
            if self.passwords.os_reauth_missing.get() {
                self.passwords
                    .reauth_until
                    .set(Some(Instant::now() + REAUTH_FOR));
                return ok(json!({ "granted": true }));
            }
            return (
                StatusCode::CONFLICT,
                chrome::json_error("Confirm with Windows first."),
            );
        }
        if self.passwords.reauthed() {
            return ok(json!({ "granted": true }));
        }
        if self.passwords.os_reauth_missing.get() {
            return ok(json!({ "confirm": true }));
        }
        if !self.passwords.reauth_pending.replace(true) {
            *self.passwords.reauth_note.borrow_mut() = None;
            let (tx, wake) = (self.passwords.tx.clone(), self.passwords.wake.clone());
            let spawned = std::thread::Builder::new()
                .name("pw-reauth".into())
                .spawn(move || {
                    let v = reauth::verify_user(
                        "MythBrowse wants to show, change or export your saved passwords. Sign in to allow this.",
                    );
                    let _ = tx.send(Msg::Reauth(v));
                    wake();
                });
            if spawned.is_err() {
                self.passwords.reauth_pending.set(false);
                return (
                    StatusCode::CONFLICT,
                    chrome::json_error("Could not ask Windows to confirm it is you."),
                );
            }
        }
        ok(json!({ "pending": true }))
    }

    fn pw_import_csv(&self, path: &str) -> (StatusCode, String) {
        let err = |m: &str| (StatusCode::CONFLICT, chrome::json_error(m));
        let path = PathBuf::from(path.trim().trim_matches('"'));
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("csv"))
        {
            return err("Choose a .csv file.");
        }
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > 20 * 1024 * 1024 => return err("The file is too large."),
            Ok(_) => {}
            Err(_) => return err("File not found."),
        }
        let text = match std::fs::read(&path) {
            Ok(b) => Zeroizing::new(String::from_utf8_lossy(&b).into_owned()),
            Err(e) => return err(&format!("Could not read the file: {e}")),
        };
        let (rows, skipped) = match mythbrowse_passwords::csv::parse_logins(&text) {
            Ok(r) => r,
            Err(e) => return err(&format!("Not a password CSV: {e}")),
        };
        let store = self.passwords.store.borrow();
        match store.as_ref().expect("checked").import(&rows, "csv") {
            Ok(mut c) => {
                c.skipped += skipped;
                (StatusCode::OK, chrome::json_ok(json!({ "counts": c })))
            }
            Err(e) => err(&e.to_string()),
        }
    }

    fn pw_status(&self) -> Value {
        let store = self.passwords.store.borrow();
        json!({
            "available": store.is_some(),
            "locked": self.passwords.problem.borrow().as_deref() == Some("locked"),
            "problem": self.passwords.problem.borrow().clone(),
            "count": store.as_ref().and_then(|s| s.len().ok()),
            "backend": self.passwords.keystore.backend(),
            "reauthed": self.passwords.reauthed(),
            "reauth_pending": self.passwords.reauth_pending.get(),
            "reauth_note": self.passwords.reauth_note.borrow().clone(),
            "os_reauth": !self.passwords.os_reauth_missing.get(),
            "import_running": self.passwords.import_running.get(),
            "import_result": self.passwords.import_result.borrow().clone(),
        })
    }

    // ------------------------------------------------------------------ pump

    pub(crate) fn pw_pump(&self) {
        while let Ok(msg) = self.passwords.rx.try_recv() {
            match msg {
                Msg::Reauth(v) => {
                    self.passwords.reauth_pending.set(false);
                    match v {
                        Verdict::Verified => {
                            self.passwords
                                .reauth_until
                                .set(Some(Instant::now() + REAUTH_FOR));
                        }
                        Verdict::Denied => {
                            *self.passwords.reauth_note.borrow_mut() =
                                Some("Windows could not confirm it is you.".into());
                        }
                        Verdict::Unavailable => {
                            self.passwords.os_reauth_missing.set(true);
                            *self.passwords.reauth_note.borrow_mut() = Some("confirm".into());
                        }
                    }
                }
                Msg::Import(out) => self.pw_apply_import(*out),
            }
        }
    }

    fn pw_apply_import(&self, out: ImportOutcome) {
        self.passwords.import_running.set(false);
        let mut errors = out.errors;
        let host = chrome::INTERNAL_HOST;
        let bookmarks = if out.bookmarks.is_empty() {
            0
        } else {
            self.store
                .import_bookmarks(
                    out.bookmarks
                        .iter()
                        .map(|b| mythbrowse_storage::import::NewBookmark {
                            url: &b.url,
                            title: &b.title,
                            added_ms: b.added_ms,
                        }),
                    host,
                )
                .unwrap_or_else(|e| {
                    errors.push(format!("Bookmarks: {e}"));
                    0
                })
        };
        let history = if out.visits.is_empty() {
            0
        } else {
            self.store
                .import_history(
                    out.visits
                        .iter()
                        .map(|v| mythbrowse_storage::import::NewVisit {
                            url: &v.url,
                            title: &v.title,
                            visit_count: v.visit_count,
                            last_visit_ms: v.last_visit_ms,
                        }),
                    host,
                )
                .unwrap_or_else(|e| {
                    errors.push(format!("History: {e}"));
                    0
                })
        };
        let mut counts = None;
        if (!out.logins.is_empty() || !out.never.is_empty())
            && let Some(s) = self.passwords.store.borrow().as_ref()
        {
            match s.import(&out.logins, "import") {
                Ok(c) => counts = Some(c),
                Err(e) => errors.push(format!("Passwords: {e}")),
            }
            for o in &out.never {
                let _ = s.add_never(o);
            }
        }
        info!(
            "import from {}: {bookmarks} bookmarks, {history} history entries, {} passwords read, {} app-bound",
            out.label, out.report.read, out.report.app_bound
        );
        *self.passwords.import_result.borrow_mut() = Some(json!({
            "source": out.label,
            "bookmarks": bookmarks,
            "bookmarks_found": out.bookmarks.len(),
            "history": history,
            "history_found": out.visits.len(),
            "passwords": counts,
            "password_report": out.report,
            "wanted_passwords": out.wanted_passwords,
            "direct_passwords": import::DIRECT_PASSWORDS,
            "errors": errors,
        }));
        self.mark_chrome_dirty();
    }
}

/// Worker thread: read and decrypt what was asked for (no profile-store access here).
fn run_import(
    browser: Browser,
    profile: &str,
    bookmarks: bool,
    history: bool,
    passwords: bool,
) -> ImportOutcome {
    let mut out = ImportOutcome {
        wanted_passwords: passwords,
        ..Default::default()
    };
    let Some(user_data) = browser.user_data_dir() else {
        out.errors
            .push(format!("{} was not found.", browser.label()));
        return out;
    };
    let Some(p) = import::profiles_in(&user_data, browser)
        .into_iter()
        .find(|p| p.dir == profile)
    else {
        out.errors
            .push(format!("Profile \u{201c}{profile}\u{201d} was not found."));
        return out;
    };
    out.label = format!("{} \u{2013} {}", browser.label(), p.name);
    if bookmarks && p.bookmarks {
        match std::fs::read_to_string(p.path.join("Bookmarks"))
            .map_err(|e| e.to_string())
            .and_then(|t| import::parse_bookmarks(&t).map_err(|e| e.to_string()))
        {
            Ok(b) => out.bookmarks = b,
            Err(e) => out.errors.push(format!("Bookmarks: {e}")),
        }
    }
    if history && p.history {
        match import::read_history(&p.path.join("History"), HISTORY_IMPORT_LIMIT) {
            Ok(v) => out.visits = v,
            Err(e) => out.errors.push(format!("History: {e}")),
        }
    }
    if passwords && p.passwords && import::DIRECT_PASSWORDS {
        match import::profile_passwords(&p) {
            Ok((logins, never, report)) => {
                out.logins = logins;
                out.never = never;
                out.report = report;
            }
            Err(e) => out.errors.push(format!("Passwords: {e}")),
        }
    }
    out
}
