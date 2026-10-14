// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Downloads, the file chooser and session restore, glued to Servo 0.1.4.
//!
//! **Downloads** (Servo has no download API, see `mythbrowse_files::detect`):
//! * a navigation to a URL that looks like a file (`request_navigation`) is denied and the
//!   file is fetched by `mythbrowse_files::download::Manager` instead (with the page's
//!   cookies for that host and its URL as referrer); if the server answers with a web page
//!   after all, the tab loads it normally;
//! * Servo's inline "Unknown content type" page is detected when a page finishes loading:
//!   that URL is downloaded and the tab goes back;
//! * `<a download>` clicks come in on the console channel from a user script in every page;
//! * an address typed into the omnibox that looks like a file is downloaded directly.
//!
//! History rows go to SQLite (schema v4) unless the window is private. The Downloads page and
//! the toolbar button get pushes (no polling).
//!
//! **File chooser**: `<input type=file>` opens the native dialog on its own thread
//! (`mythbrowse_files::picker`); the page gets the files when the dialog closes.
//! Drag-and-drop of files is not possible on Servo 0.1.4 (no drag / drop input event).
//!
//! **Myth**: a download or file chooser started in a page Myth controls goes on the run's
//! approval card first (`agent_side_approval`); nothing new is exposed as an agent tool.
//!
//! **Session restore**: the window is saved about 2 s after a change (and once more, marked
//! clean, on a normal exit); private windows never save or restore.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use http::StatusCode;
use log::{info, warn};
use mythbrowse_engine::chrome::{self, json_error, json_ok};
use mythbrowse_engine::files_api::{self, DownloadOp, FilesOp};
use mythbrowse_engine::servo::{FilePicker, JSValue, NavigationRequest, UserScript, WebView};
use mythbrowse_engine::tabs::{RestoredTab, ScrollPos, TabId};
use mythbrowse_files::download::{self, Event, Item, Manager, Request, State};
use mythbrowse_files::session::{self, Decision, Session, SessionTab, Startup};
use mythbrowse_files::{detect, os, picker};
use mythbrowse_storage::files_store::{DownloadRow, MAX_DOWNLOAD_ROWS};
use serde_json::{Value, json};
use url::Url;

use crate::AppState;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// The session is saved this long after the last change.
const SESSION_DEBOUNCE: Duration = Duration::from_secs(2);
/// One URL is fetched at most once in this window, whichever hook saw it first
/// (`Manager::start_once`).
const ONCE_WINDOW: Duration = Duration::from_secs(10);
/// A running download's history row is rewritten at most this often.
const PERSIST_EVERY: Duration = Duration::from_secs(5);

/// Something waiting for the user's answer on Myth's approval card.
enum Pending {
    Download(Request),
    Picker(u64),
}

pub(crate) struct FilesHost {
    pub(crate) manager: Manager,
    events: Receiver<Event>,
    private: bool,
    /// There has been a download (this session or in history): the toolbar button shows.
    any: Cell<bool>,
    /// Open file choosers, by our key: the Servo control and its options.
    pickers: RefCell<HashMap<u64, (FilePicker, picker::Options)>>,
    picker_tx: Sender<(u64, Option<Vec<PathBuf>>)>,
    picker_rx: Receiver<(u64, Option<Vec<PathBuf>>)>,
    next_picker: Cell<u64>,
    /// Requests in Myth-controlled pages waiting on the approval card.
    waiting: RefCell<Vec<(Receiver<bool>, Pending)>>,
    /// URLs allowed to load once (a "download" that turned out to be a page). Consumed by the
    /// main-frame load hook (`files_takeover`), which every top-level load goes through.
    bypass: RefCell<HashSet<String>>,
    /// Tabs whose navigation became a download, settled from the pump (never from inside a
    /// Servo callback): back to the page before, or closed when they were opened only for
    /// the file.
    settle: RefCell<Vec<(WebView, detect::TabAfterDownload)>>,
    /// Which webview asked for a download (to load it there if it is a page after all).
    origins: RefCell<HashMap<download::Id, WebView>>,
    last_persist: RefCell<HashMap<download::Id, Instant>>,
    /// The Downloads page(s) need a push.
    pages_dirty: Cell<bool>,
    session_due: Cell<Option<Instant>>,
    last_session: RefCell<String>,
    wake: Wake,
}

impl FilesHost {
    pub(crate) fn new(
        store: &mythbrowse_storage::Store,
        profile: Option<&std::path::Path>,
        private: bool,
        wake: Wake,
    ) -> FilesHost {
        let dir = os::downloads_dir()
            .or_else(|| profile.map(|p| p.join("Downloads")))
            .unwrap_or_else(|| std::env::temp_dir().join("MythBrowse Downloads"));
        let first = store.max_download_id().unwrap_or(0) + 1;
        let (tx, events) = channel();
        let w = wake.clone();
        let manager = Manager::new(dir, first, move |e| {
            if tx.send(e).is_ok() {
                w();
            }
        });
        let mut any = false;
        if !private {
            match store.downloads(MAX_DOWNLOAD_ROWS) {
                Ok(rows) => {
                    let items: Vec<Item> = rows
                        .into_iter()
                        .rev()
                        .filter_map(|r| serde_json::from_str::<Item>(&r.data).ok())
                        .collect();
                    any = !items.is_empty();
                    manager.restore(items);
                }
                Err(e) => warn!("download history unreadable: {e}"),
            }
        }
        let (picker_tx, picker_rx) = channel();
        FilesHost {
            manager,
            events,
            private,
            any: Cell::new(any),
            pickers: RefCell::new(HashMap::new()),
            picker_tx,
            picker_rx,
            next_picker: Cell::new(1),
            waiting: RefCell::new(Vec::new()),
            bypass: RefCell::new(HashSet::new()),
            settle: RefCell::new(Vec::new()),
            origins: RefCell::new(HashMap::new()),
            last_persist: RefCell::new(HashMap::new()),
            pages_dirty: Cell::new(false),
            session_due: Cell::new(None),
            last_session: RefCell::new(String::new()),
            wake,
        }
    }

    /// The `<a download>` hook, for the shared user content manager.
    pub(crate) fn link_script() -> Rc<UserScript> {
        Rc::new(UserScript::new(
            detect::DOWNLOAD_LINK_SCRIPT.to_owned(),
            None,
        ))
    }
}

/// Download list for the UI: items plus the danger warning text.
fn items_json(items: &[Item]) -> Vec<Value> {
    items
        .iter()
        .map(|i| {
            let mut v = serde_json::to_value(i).unwrap_or(Value::Null);
            if let Value::Object(m) = &mut v {
                m.insert("warning".into(), Value::String(i.warning()));
            }
            v
        })
        .collect()
}

impl AppState {
    // ------------------------------------------------------------------ downloads

    /// `Cookie` header value for `url` from Servo's cookie jar (HttpOnly included).
    fn files_cookies(&self, url: &Url) -> Option<String> {
        let jar = self.engine.servo().site_data_manager();
        let cookies = jar.cookies_for_url(url.clone(), net_traits::CookieSource::HTTP);
        let header = cookies
            .iter()
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ");
        (!header.is_empty()).then_some(header)
    }

    fn files_request(
        &self,
        url: &Url,
        from: Option<&WebView>,
        suggested: Option<String>,
        sniff: bool,
    ) -> Request {
        let by_agent = from.is_some_and(|v| self.agent.owns(v))
            || from
                .and_then(|v| self.tab_id_of(v))
                .is_some_and(|id| self.agent_uses_user_tab(id));
        Request {
            url: url.to_string(),
            suggested_name: suggested.filter(|s| !s.trim().is_empty()),
            referrer: from
                .and_then(|v| v.url())
                .filter(|u| !chrome::is_internal(u))
                .map(|u| u.to_string()),
            cookies: if matches!(url.scheme(), "http" | "https") {
                self.files_cookies(url)
            } else {
                None
            },
            user_agent: None,
            sniff_html: sniff,
            private: self.files.private,
            by_agent,
        }
    }

    /// Start a download, asking on Myth's card first when Myth controls the page.
    pub(crate) fn files_download(
        &self,
        url: &Url,
        from: Option<&WebView>,
        suggested: Option<String>,
        sniff: bool,
    ) {
        let request = self.files_request(url, from, suggested, sniff);
        if let Some(view) = from {
            let user_tab = self.tab_id_of(view);
            let name = mythbrowse_files::names::choose_name(
                None,
                request.suggested_name.as_deref(),
                Some(url),
                None,
            );
            if let Some(rx) = self.agent_side_approval(
                view,
                user_tab,
                &format!("Download {name}"),
                "The page Myth is working in started a download. It is saved to your Downloads folder only if you allow it.",
                url.as_str(),
            ) {
                self.files.waiting.borrow_mut().push((rx, Pending::Download(request)));
                return;
            }
        }
        self.files_start(request, from.cloned());
    }

    fn files_start(&self, request: Request, from: Option<WebView>) {
        let sniff = request.sniff_html;
        // Several hooks can see the same file (navigation, main-frame load, Servo's unknown
        // content page): it is fetched once.
        let Some(id) = self.files.manager.start_once(request, ONCE_WINDOW) else {
            return;
        };
        if sniff && let Some(v) = from {
            self.files.origins.borrow_mut().insert(id, v);
        }
        self.files.any.set(true);
        self.mark_chrome_dirty();
    }

    /// A page navigation: a file-like URL becomes a download (the navigation is denied).
    /// Returns the request when it is not a download, for the next check (Myth's page
    /// navigation guard) to decide.
    pub(crate) fn files_navigation(
        &self,
        webview: &WebView,
        request: NavigationRequest,
    ) -> Option<NavigationRequest> {
        let url = request.url.clone();
        if self.is_chrome(webview) || chrome::is_internal(&url) {
            return Some(request);
        }
        // A bypassed URL (a page after all) stays in `bypass` until its load arrives in
        // `files_takeover`.
        if self.files.bypass.borrow().contains(url.as_str()) || !detect::looks_like_download(&url) {
            return Some(request);
        }
        request.deny();
        info!("download (navigation): {url}");
        self.files_download(&url, Some(webview), None, true);
        self.files_settle(webview, Self::shows_page(webview));
        None
    }

    /// The tab shows a real page (not a fresh blank tab).
    fn shows_page(webview: &WebView) -> bool {
        webview
            .url()
            .is_some_and(|u| u.scheme() != "about" && !u.as_str().is_empty())
    }

    /// Settle a tab whose navigation became a download (from the pump).
    fn files_settle(&self, webview: &WebView, shows_page: bool) {
        // Only the user's own tabs: Myth's background tabs are the agent's to manage.
        if self.tab_id_of(webview).is_none() {
            return;
        }
        let what = detect::tab_after_download(shows_page, webview.can_go_back());
        if what != detect::TabAfterDownload::Keep {
            self.files.settle.borrow_mut().push((webview.clone(), what));
            (self.files.wake)();
        }
    }

    /// Main-frame load hook (`load_web_resource`): a top-level load of a file-like URL that
    /// did not come through `request_navigation` (a redirect hop to a file, the first load of
    /// a `target=_blank` / `window.open` tab) is taken over by the download manager, so the
    /// file is fetched once (by the manager) instead of by Servo and then again by the
    /// manager from the "unknown content type" page. Returns true when the caller must
    /// answer Servo with `204 No Content` (ends the navigation, keeps the page).
    pub(crate) fn files_takeover(&self, webview: &WebView, url: &Url) -> bool {
        if self.is_chrome(webview) || chrome::is_internal(url) {
            return false;
        }
        let bypassed = self.files.bypass.borrow_mut().remove(url.as_str());
        if !detect::takes_over_main_frame(url, bypassed) {
            return false;
        }
        info!("download (main-frame load): {url}");
        self.files_download(url, Some(webview), None, true);
        self.files_settle(webview, Self::shows_page(webview));
        true
    }

    /// Address bar input that is a file URL: download it instead of loading it.
    pub(crate) fn files_typed(&self, url: &Url) -> bool {
        if !detect::looks_like_download(url) {
            return false;
        }
        let view = self.active_view();
        self.files_download(url, view.as_ref(), None, true);
        true
    }

    /// A page finished loading: was it Servo's "Unknown content type" page?
    pub(crate) fn files_load_complete(&self, webview: &WebView) {
        if self.is_chrome(webview) {
            return;
        }
        let Some(url) = webview
            .url()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
        else {
            return;
        };
        let state = self.weak_self.clone();
        let view = webview.clone();
        webview.evaluate_javascript(detect::UNKNOWN_CONTENT_PROBE, move |result| {
            let Some(s) = state.upgrade() else { return };
            if let Ok(JSValue::String(ct)) = result
                && !ct.is_empty()
            {
                info!("download (unknown content type {ct}): {url}");
                // Fetched once: skipped when a hook already took this URL.
                s.files_download(&url, Some(&view), None, false);
                // The tab shows Servo's placeholder: back, or closed if it has no history
                // (a tab opened just for the file).
                s.files_settle(&view, false);
            }
        });
    }

    /// A console line: `<a download>` clicks. Returns true if it was one.
    pub(crate) fn files_console(&self, webview: &WebView, message: &str) -> bool {
        let Some(req) = detect::parse_console_request(message) else {
            return false;
        };
        match Url::parse(&req.href) {
            Ok(url) => {
                info!(
                    "download (<a download>): {}",
                    if url.scheme() == "data" {
                        "data: URL"
                    } else {
                        url.as_str()
                    }
                );
                self.files_download(&url, Some(webview), Some(req.name), false);
            }
            Err(_) => warn!("ignored <a download> with an invalid URL"),
        }
        true
    }

    // ---------------------------------------------------------------- file chooser

    /// `<input type=file>`: the native chooser (after Myth's card in a Myth-controlled page).
    pub(crate) fn files_picker(&self, webview: &WebView, picker: FilePicker) {
        let key = self.files.next_picker.get();
        self.files.next_picker.set(key + 1);
        let host = webview
            .url()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        let opts = picker::Options {
            extensions: picker
                .filter_patterns()
                .iter()
                .map(|p| p.0.clone())
                .collect(),
            multiple: picker.allow_select_multiple(),
            title: if host.is_empty() {
                "Choose a file".into()
            } else {
                format!("Choose a file for {host}")
            },
            owner: self.files_owner_window(),
        };
        self.files.pickers.borrow_mut().insert(key, (picker, opts));
        let user_tab = self.tab_id_of(webview);
        if let Some(rx) = self.agent_side_approval(
            webview,
            user_tab,
            "Choose a file to upload",
            "The page Myth is working in wants a file from your computer. If you allow it, you pick the file yourself in the next step.",
            webview.url().map(|u| u.to_string()).unwrap_or_default().as_str(),
        ) {
            self.files.waiting.borrow_mut().push((rx, Pending::Picker(key)));
            return;
        }
        self.files_open_picker(key);
    }

    fn files_open_picker(&self, key: u64) {
        let Some(opts) = self
            .files
            .pickers
            .borrow()
            .get(&key)
            .map(|(_, o)| o.clone())
        else {
            return;
        };
        let tx = self.files.picker_tx.clone();
        let wake = self.files.wake.clone();
        let spawned = std::thread::Builder::new()
            .name("mb-file-picker".into())
            .spawn(move || {
                let result = picker::choose(&opts);
                if tx.send((key, result)).is_ok() {
                    wake();
                }
            });
        if spawned.is_err()
            && let Some((p, _)) = self.files.pickers.borrow_mut().remove(&key)
        {
            p.dismiss();
        }
    }

    /// The page withdrew its request (`hide_embedder_control`).
    pub(crate) fn files_hide_control(&self, id: mythbrowse_engine::servo::EmbedderControlId) {
        let key = self
            .files
            .pickers
            .borrow()
            .iter()
            .find(|(_, (p, _))| p.id() == id)
            .map(|(k, _)| *k);
        if let Some(k) = key
            && let Some((p, _)) = self.files.pickers.borrow_mut().remove(&k)
        {
            p.dismiss();
        }
    }

    #[cfg(windows)]
    fn files_owner_window(&self) -> isize {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match self.window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        }
    }

    #[cfg(not(windows))]
    fn files_owner_window(&self) -> isize {
        0
    }

    // ------------------------------------------------------------------- pump

    /// Transfer events, chooser results and approval answers (main thread).
    pub(crate) fn files_pump(&self) {
        loop {
            match self.files.events.try_recv() {
                Ok(Event::Changed(item)) => self.files_on_changed(item),
                Ok(Event::NotADownload { id, url }) => {
                    let view = self.files.origins.borrow_mut().remove(&id);
                    if let (Some(v), Ok(u)) = (view, Url::parse(&url)) {
                        self.files.bypass.borrow_mut().insert(u.to_string());
                        v.load(u);
                    }
                    self.files.pages_dirty.set(true);
                    self.mark_chrome_dirty();
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        while let Ok((key, result)) = self.files.picker_rx.try_recv() {
            let Some((mut p, _)) = self.files.pickers.borrow_mut().remove(&key) else {
                continue;
            };
            match result {
                Some(paths) if !paths.is_empty() => {
                    info!("file chooser: {} file(s) chosen", paths.len());
                    p.select(&paths);
                    p.submit();
                }
                _ => p.dismiss(),
            }
        }
        let answered: Vec<(bool, Pending)> = {
            let mut waiting = self.files.waiting.borrow_mut();
            let mut out = Vec::new();
            let mut i = 0;
            while i < waiting.len() {
                match waiting[i].0.try_recv() {
                    Ok(allow) => out.push((allow, waiting.remove(i).1)),
                    Err(TryRecvError::Disconnected) => out.push((false, waiting.remove(i).1)),
                    Err(TryRecvError::Empty) => i += 1,
                }
            }
            out
        };
        for (allow, pending) in answered {
            match (allow, pending) {
                (true, Pending::Download(r)) => self.files_start(r, None),
                (false, Pending::Download(r)) => info!("myth: download of {} not allowed", r.url),
                (true, Pending::Picker(k)) => self.files_open_picker(k),
                (false, Pending::Picker(k)) => {
                    if let Some((p, _)) = self.files.pickers.borrow_mut().remove(&k) {
                        p.dismiss();
                    }
                }
            }
        }
        let settle: Vec<(WebView, detect::TabAfterDownload)> =
            self.files.settle.borrow_mut().drain(..).collect();
        for (view, what) in settle {
            match what {
                detect::TabAfterDownload::GoBack if view.can_go_back() => {
                    view.go_back(1);
                }
                detect::TabAfterDownload::GoBack | detect::TabAfterDownload::Close => {
                    if let Some(id) = self.tab_id_of(&view) {
                        info!("download: closing the empty tab it left");
                        self.close_tab(id);
                    }
                }
                detect::TabAfterDownload::Keep => {}
            }
        }
        if self.files.pages_dirty.replace(false) {
            self.files_push_pages();
        }
    }

    fn files_on_changed(&self, item: Item) {
        self.files.any.set(true);
        if item.state != State::Running && item.state != State::Pending {
            self.files.origins.borrow_mut().remove(&item.id);
        }
        if !self.files.private && !item.private {
            let due = {
                let mut last = self.files.last_persist.borrow_mut();
                let now = Instant::now();
                let due = item.state != State::Running
                    || last
                        .get(&item.id)
                        .is_none_or(|t| now.duration_since(*t) >= PERSIST_EVERY);
                if due {
                    last.insert(item.id, now);
                }
                due
            };
            if due {
                let row = DownloadRow {
                    id: item.id,
                    state: item.state.as_str().to_owned(),
                    started_ms: item.started_ms,
                    data: serde_json::to_string(&item).unwrap_or_default(),
                };
                if let Err(e) = self.store.upsert_download(&row) {
                    warn!("download history write failed: {e}");
                }
            }
        }
        self.files.pages_dirty.set(true);
        self.mark_chrome_dirty();
    }

    /// Push the list into every open Downloads page.
    fn files_push_pages(&self) {
        let views: Vec<WebView> = self
            .tabs
            .borrow()
            .iter()
            .filter(|t| files_api::is_downloads_page(&t.url))
            .filter_map(|t| t.view.clone())
            .collect();
        if views.is_empty() {
            return;
        }
        let list = serde_json::to_string(&items_json(&self.files.manager.list()))
            .unwrap_or_else(|_| "[]".into());
        let script = format!("window.mbDownloads && window.mbDownloads({list});");
        for v in views {
            v.evaluate_javascript(script.clone(), |_| {});
        }
    }

    /// The toolbar Downloads button state (`ChromeState::downloads`).
    pub(crate) fn files_toolbar(&self) -> Option<Value> {
        let (active, frac) = self.files.manager.progress();
        files_api::toolbar_state(
            active,
            frac,
            self.files.manager.needs_attention(),
            self.files.any.get(),
        )
    }

    /// Open the Downloads page, or switch to it if a tab already shows it.
    pub(crate) fn files_open_downloads(&self) {
        let existing = self
            .tabs
            .borrow()
            .iter()
            .find(|t| files_api::is_downloads_page(&t.url))
            .map(|t| t.id);
        match existing {
            Some(id) => self.activate_tab(id),
            None => self.open_tab(files_api::downloads_url()),
        }
    }

    // -------------------------------------------------------------------- RPC

    pub(crate) fn files_api(&self, op: FilesOp) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, json_ok(v));
        let err = |m: &str| (StatusCode::OK, json_error(m));
        let list = || json!({ "downloads": items_json(&self.files.manager.list()) });
        match op {
            FilesOp::Downloads => {
                let mut v = list();
                v["folder"] = Value::String(self.files.manager.dir().display().to_string());
                ok(v)
            }
            FilesOp::Action { id, op, confirm } => {
                let m = &self.files.manager;
                let result = match op {
                    DownloadOp::Pause => m.pause(id),
                    DownloadOp::Resume => m.resume(id),
                    DownloadOp::Retry => m.retry(id),
                    DownloadOp::Cancel => m.cancel(id),
                    DownloadOp::Keep => m.keep(id),
                    DownloadOp::Discard => m.discard(id),
                    DownloadOp::Open => match m.get(id) {
                        Some(i) if i.state == State::Done && i.danger && !confirm => {
                            Err("This file can run code. Confirm to open it.".into())
                        }
                        Some(i) if i.state == State::Done => {
                            os::open_path(std::path::Path::new(&i.path))
                        }
                        Some(_) => Err("The download has not finished.".into()),
                        None => Err("No such download.".into()),
                    },
                    DownloadOp::Show => match m.get(id) {
                        Some(i) if !i.path.is_empty() => {
                            os::reveal_path(std::path::Path::new(&i.path))
                        }
                        Some(_) => os::reveal_path(&m.dir()),
                        None => Err("No such download.".into()),
                    },
                    DownloadOp::Remove => {
                        m.remove(id);
                        if let Err(e) = self.store.delete_download(id) {
                            warn!("download history delete failed: {e}");
                        }
                        Ok(())
                    }
                };
                self.files.pages_dirty.set(true);
                self.mark_chrome_dirty();
                match result {
                    Ok(()) => ok(list()),
                    Err(e) => err(&e),
                }
            }
            FilesOp::Clear => {
                for id in self.files.manager.clear_finished() {
                    let _ = self.store.delete_download(id);
                }
                self.files.pages_dirty.set(true);
                self.mark_chrome_dirty();
                ok(list())
            }
            FilesOp::Folder => {
                let dir = self.files.manager.dir();
                let _ = std::fs::create_dir_all(&dir);
                match os::open_path(&dir) {
                    Ok(()) => ok(json!({})),
                    Err(e) => err(&e),
                }
            }
            FilesOp::OpenDownloads => {
                self.files_open_downloads();
                ok(json!({}))
            }
            FilesOp::StartupPref => ok(
                json!({ "mode": self.store.startup_pref().unwrap_or_else(|_| "continue".into()) }),
            ),
            FilesOp::StartupPrefSave { mode } => match self.store.set_startup_pref(&mode) {
                Ok(true) => ok(json!({ "mode": mode })),
                Ok(false) => err("Unknown setting."),
                Err(e) => err(&format!("Could not save the setting ({e}).")),
            },
            FilesOp::PinTab { id, on } => {
                if self.tabs.borrow_mut().set_pinned(id, on) {
                    self.mark_chrome_dirty();
                    ok(json!({}))
                } else {
                    err("No such tab.")
                }
            }
        }
    }

    // -------------------------------------------------------------- session

    /// Something about the tabs changed: save the session soon.
    pub(crate) fn files_session_dirty(&self) {
        if self.files.private {
            return;
        }
        if self.files.session_due.get().is_none() {
            self.files
                .session_due
                .set(Some(Instant::now() + SESSION_DEBOUNCE));
        }
    }

    /// When the session should be saved next (for `about_to_wait`); saves it when due.
    pub(crate) fn files_session_tick(&self) -> Option<Instant> {
        let due = self.files.session_due.get()?;
        if Instant::now() < due {
            return Some(due);
        }
        self.files.session_due.set(None);
        self.files_save_session(false);
        None
    }

    fn files_collect_session(&self) -> Session {
        let tabs = self.tabs.borrow();
        let active = tabs.active_id();
        let mut out = Session {
            version: 1,
            tabs: Vec::new(),
            active: 0,
            saved_ms: mythbrowse_files::now_ms(),
        };
        for t in tabs.iter() {
            if Some(t.id) == active {
                out.active = out.tabs.len();
            }
            out.tabs.push(SessionTab {
                url: t.url.clone(),
                title: t.title.clone(),
                pinned: t.pinned,
                sleeping: t.is_sleeping(),
                scroll_y: t.scroll.y,
                back: t.back_list(),
                forward: t.forward_list(),
            });
        }
        out.cleaned()
    }

    fn files_save_session(&self, clean: bool) {
        if self.files.private {
            return;
        }
        let mut s = self.files_collect_session();
        let saved_ms = s.saved_ms;
        s.saved_ms = 0; // compare without the timestamp
        let json = s.to_json();
        if !clean && *self.files.last_session.borrow() == json {
            return;
        }
        if let Err(e) = self.store.save_session(&json, clean, saved_ms) {
            warn!("session save failed: {e}");
            return;
        }
        *self.files.last_session.borrow_mut() = json;
    }

    /// Normal exit: save the window, marked clean.
    pub(crate) fn files_shutdown(&self) {
        self.files_save_session(true);
        for (_, (p, _)) in self.files.pickers.borrow_mut().drain() {
            p.dismiss();
        }
    }

    /// Startup: bring the last window back if [`session::decide`] says so. Returns true if
    /// any tab was restored (the caller opens its start page otherwise).
    pub(crate) fn files_restore(&self) -> bool {
        let saved = match self.store.load_session() {
            Ok(s) => s,
            Err(e) => {
                warn!("saved session unreadable: {e}");
                None
            }
        };
        let startup = Startup::parse(&self.store.startup_pref().unwrap_or_default());
        let parsed = saved
            .as_ref()
            .map(|s| Session::from_json(&s.data))
            .unwrap_or_default();
        let was_clean = saved.as_ref().is_none_or(|s| s.clean);
        let decision = session::decide(self.files.private, !parsed.is_empty(), was_clean, startup);
        if !self.files.private {
            // From now on a crash leaves an unclean session behind.
            if let Err(e) = self.store.mark_session_clean(false) {
                warn!("session flag write failed: {e}");
            }
        }
        if decision != Decision::Restore {
            info!(
                "session: start fresh (private {}, saved tabs {}, clean {was_clean}, on startup {})",
                self.files.private,
                parsed.tabs.len(),
                startup.as_str()
            );
            return false;
        }
        info!(
            "session: restoring {} tab(s){}",
            parsed.tabs.len(),
            if was_clean { "" } else { " after a crash" }
        );
        let now = self.now_ms();
        let mut ids: Vec<TabId> = Vec::new();
        {
            let mut tabs = self.tabs.borrow_mut();
            for t in &parsed.tabs {
                ids.push(tabs.open_sleeping(
                    RestoredTab {
                        url: t.url.clone(),
                        title: t.title.clone(),
                        pinned: t.pinned,
                        scroll: ScrollPos {
                            x: 0.0,
                            y: t.scroll_y,
                        },
                        back: t.back.clone(),
                        forward: t.forward.clone(),
                    },
                    now,
                ));
            }
        }
        let Some(active) = ids.get(parsed.active).or(ids.first()).copied() else {
            return false;
        };
        self.tabs.borrow_mut().activate(active, now);
        self.wake_if_sleeping(active);
        self.show_active();
        true
    }
}
