// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Site compat in the shell (see `docs/COMPAT.md` and `crates/compat`).
//!
//! * One `UserContentManager` is shared by every web-page webview (user tabs, popups, agent
//!   tabs; never the chrome or internal pages). It holds the bootstrap and one self-gating
//!   script per trusted rule (+ data-only scripts for local rules). After a rule change the
//!   scripts are swapped and the page reloads (Servo applies user content on the next load).
//! * Local rules' JS runs via `evaluate_javascript` when a page's head is parsed, only in pages
//!   whose URL Rust matched.
//! * `block` rules cancel matching subresource loads in `load_web_resource`.
//! * The detector probe runs once per load (`LoadStatus::Complete`) in user tabs, and again
//!   only when the user asks. No timers.
//! * AI repair runs only when the user presses "Ask Myth to fix": a worker thread calls the
//!   chat model; the patch is previewed in that tab only, and saved for that exact host only
//!   after the user tried it and pressed "Keep".
//!
//! Every Servo callback here only posts a message; the work happens in [`AppState::compat_pump`]
//! on the event loop (never re-entering Servo from inside one of its callbacks).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use http::StatusCode;
use log::{info, warn};
use mythbrowse_ai::ChatMessage;
use mythbrowse_compat::detect::DOM_SUMMARY_SCRIPT;
use mythbrowse_compat::{
    Assessment, Candidate, Compat, ConsoleStats, ProbeReport, Review, assess, bundle, repair,
};
use mythbrowse_engine::chrome::{self, CompatOp};
use mythbrowse_engine::servo::{
    ConsoleLogLevel, JSValue, LoadStatus, UserContentManager, UserScript, WebView,
};
use mythbrowse_engine::tabs::TabId;
use serde_json::{Value, json};
use url::Url;

use crate::AppState;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// Messages from Servo callbacks and the model worker to the event loop.
enum Msg {
    /// Probe result for a tab (`after`: re-check after a preview).
    Probe {
        tab: TabId,
        url: String,
        result: Result<String, String>,
        after: bool,
    },
    /// DOM summary ready: start the model call.
    Dom {
        tab: TabId,
        run: u64,
        result: Result<String, String>,
    },
    /// The model answered.
    Fix {
        tab: TabId,
        run: u64,
        result: Result<Candidate, String>,
    },
    /// A preview was applied (or failed).
    Tried {
        tab: TabId,
        run: u64,
        result: Result<String, String>,
    },
    /// Top-sites run: the screenshot of site `index` was written (or failed).
    Shot {
        index: usize,
        result: Result<String, String>,
    },
    /// The cloud ruleset was downloaded (toml, signature).
    Remote { toml: String, sig: String },
}

/// How often the signed cloud ruleset is checked (and once shortly after start).
const REMOTE_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
const REMOTE_FIRST: std::time::Duration = std::time::Duration::from_secs(20);

/// Background thread: download the signed ruleset from the Appwrite bucket now and then and
/// hand it to the event loop, which verifies it before anything is used. Nothing to fetch
/// while this build trusts no signing key.
fn spawn_remote_fetch(tx: Sender<Msg>, wake: Wake, auto: Arc<AtomicBool>) {
    if mythbrowse_compat::sign::TRUSTED_KEYS.is_empty()
        || !mythbrowse_sync::appwrite::cloud_configured()
        || std::env::var_os("MYTHBROWSE_NO_REMOTE_RULES").is_some()
    {
        info!("compat: cloud ruleset off (no signing key or HTTPS cloud project in this build)");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("mythbrowse-rules".into())
        .spawn(move || {
            let client = mythbrowse_sync::appwrite::Appwrite::new(Default::default());
            std::thread::sleep(REMOTE_FIRST);
            loop {
                // Settings > Site compatibility "Automatically update site-fix rules" (checked each time).
                if !auto.load(Ordering::Relaxed) {
                    log::debug!("compat: automatic rule updates are off");
                    std::thread::sleep(REMOTE_EVERY);
                    continue;
                }
                match client.compat_ruleset() {
                    Ok(Some((toml, sig))) => {
                        if tx.send(Msg::Remote { toml, sig }).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(None) => log::debug!("compat: no cloud ruleset published"),
                    Err(e) => log::debug!("compat: cloud ruleset check failed: {e}"),
                }
                std::thread::sleep(REMOTE_EVERY);
            }
        });
    if let Err(e) = spawned {
        warn!("compat: cloud ruleset thread: {e}");
    }
}

enum Fix {
    Idle,
    Running {
        run: u64,
        cancel: Arc<AtomicBool>,
    },
    Ready {
        run: u64,
        candidate: Candidate,
        review: Review,
    },
    Failed(String),
}

/// Compat state of one user tab's current page (reset on every new load).
struct Page {
    url: String,
    console: ConsoleStats,
    report: Option<ProbeReport>,
    assessment: Option<Assessment>,
    checking: bool,
    fix: Fix,
    /// The candidate applied to this document (preview), and the re-check after it.
    preview: Option<Candidate>,
    after: Option<Assessment>,
    /// Reader / simplified applied by the user on this document.
    mode: Option<String>,
}

impl Page {
    fn new(url: String) -> Self {
        Page {
            url,
            console: ConsoleStats::default(),
            report: None,
            assessment: None,
            checking: false,
            fix: Fix::Idle,
            preview: None,
            after: None,
            mode: None,
        }
    }
}

pub(crate) struct CompatHost {
    enabled: bool,
    /// Settings "Automatically update site-fix rules", shared with the download thread.
    auto_update: Arc<AtomicBool>,
    compat: RefCell<Compat>,
    pub(crate) user_content: Rc<UserContentManager>,
    installed: RefCell<Vec<Rc<UserScript>>>,
    pages: RefCell<BTreeMap<u64, Page>>,
    /// The panel is open (over the active web page).
    pub(crate) open: Cell<bool>,
    /// One-line notice for a host, shown in its panel (e.g. after "Keep" reloaded the page).
    notice: RefCell<Option<(String, String)>>,
    /// Web content crashes this session, by host (reported once per host).
    crashes: RefCell<mythbrowse_compat::CrashLog>,
    next_run: Cell<u64>,
    /// Top-sites compat run (`MYTHBROWSE_COMPAT_RUN`), CI only.
    run: RefCell<Option<SiteRun>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    wake: Wake,
}

fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .filter(mythbrowse_compat::matcher::is_web_url)
        .and_then(|u| u.host_str().map(str::to_owned))
}

impl CompatHost {
    pub(crate) fn new(
        user_content: Rc<UserContentManager>,
        profile_dir: Option<&Path>,
        wake: Wake,
    ) -> Self {
        let enabled = std::env::var("MYTHBROWSE_COMPAT").map_or(true, |v| v != "0");
        let random = u64::from_str_radix(&chrome::new_token()[..16], 16).unwrap_or(0x6d62);
        let (compat, warnings) = Compat::load(profile_dir, mythbrowse_compat::session_key(random));
        for w in warnings {
            warn!("compat: {w}");
        }
        info!(
            "compat: {} ({:?})",
            if enabled {
                "on"
            } else {
                "off (MYTHBROWSE_COMPAT=0)"
            },
            compat.counts()
        );
        let auto_update = Arc::new(AtomicBool::new(compat.auto_update()));
        let (tx, rx) = channel();
        let host = CompatHost {
            enabled,
            auto_update: auto_update.clone(),
            compat: RefCell::new(compat),
            user_content,
            installed: RefCell::new(Vec::new()),
            pages: RefCell::new(BTreeMap::new()),
            open: Cell::new(false),
            notice: RefCell::new(None),
            crashes: RefCell::new(mythbrowse_compat::CrashLog::new()),
            next_run: Cell::new(1),
            run: RefCell::new(SiteRun::from_env()),
            tx,
            rx,
            wake,
        };
        host.install();
        if enabled {
            let (tx, wake) = host.post();
            spawn_remote_fetch(tx, wake, auto_update);
        }
        host
    }

    fn run_id(&self) -> u64 {
        let id = self.next_run.get();
        self.next_run.set(id + 1);
        id
    }

    /// (Re)install the user scripts. Pages pick them up on their next load.
    fn install(&self) {
        let scripts = if self.enabled {
            self.compat.borrow().user_scripts()
        } else {
            Vec::new()
        };
        let mut installed = self.installed.borrow_mut();
        for s in installed.drain(..) {
            self.user_content.remove_script(s);
        }
        for s in scripts {
            let script = Rc::new(UserScript::new(s, None));
            self.user_content.add_script(script.clone());
            installed.push(script);
        }
    }

    fn post(&self) -> (Sender<Msg>, Wake) {
        (self.tx.clone(), self.wake.clone())
    }

    /// The probe script for this session.
    pub(crate) fn probe_script(&self) -> String {
        self.compat.borrow().probe_script()
    }
}

fn js_string(
    result: Result<JSValue, mythbrowse_engine::servo::JavaScriptEvaluationError>,
) -> Result<String, String> {
    match result {
        Ok(JSValue::String(s)) => Ok(s),
        Ok(other) => Err(format!("unexpected result {other:?}")),
        Err(e) => Err(format!("{e:?}")),
    }
}

impl AppState {
    // ----------------------------------------------------------------- page lifecycle

    /// Load-status hook for every non-chrome webview (user and agent tabs).
    pub(crate) fn compat_load_status(&self, webview: &WebView, status: LoadStatus) {
        if !self.compat.enabled {
            return;
        }
        let Some(url) = webview.url().filter(mythbrowse_compat::matcher::is_web_url) else {
            if let Some(id) = self.tab_id_of(webview) {
                self.compat.pages.borrow_mut().remove(&id.0);
            }
            return;
        };
        let tab = self.tab_id_of(webview);
        match status {
            LoadStatus::Started => {
                if let Some(id) = tab {
                    self.compat
                        .pages
                        .borrow_mut()
                        .insert(id.0, Page::new(url.to_string()));
                    self.mark_chrome_dirty();
                }
            }
            LoadStatus::HeadParsed => {
                // Local rules' JS: only in pages whose URL Rust matched.
                let script = self.compat.compat.borrow().page_script(&url);
                if let Some(script) = script {
                    webview.evaluate_javascript(script, |r| {
                        if let Err(e) = r {
                            log::debug!("compat page script failed: {e:?}");
                        }
                    });
                }
            }
            LoadStatus::Complete => {
                if let Some(id) = tab {
                    self.compat_probe(id, webview, false);
                }
            }
        }
    }

    fn compat_probe(&self, tab: TabId, webview: &WebView, after: bool) {
        {
            let mut pages = self.compat.pages.borrow_mut();
            let url = webview.url().map(|u| u.to_string()).unwrap_or_default();
            let page = pages.entry(tab.0).or_insert_with(|| Page::new(url));
            page.checking = true;
        }
        let url = webview.url().map(|u| u.to_string()).unwrap_or_default();
        let (tx, wake) = self.compat.post();
        webview.evaluate_javascript(self.compat.probe_script(), move |result| {
            let result = js_string(result);
            if tx
                .send(Msg::Probe {
                    tab,
                    url,
                    result,
                    after,
                })
                .is_ok()
            {
                wake();
            }
        });
        self.mark_chrome_dirty();
    }

    /// Console message from a page (counted for the detector; user tabs only).
    pub(crate) fn compat_console(&self, webview: &WebView, level: &ConsoleLogLevel, message: &str) {
        if !self.compat.enabled {
            return;
        }
        let level = match level {
            ConsoleLogLevel::Error => "error",
            ConsoleLogLevel::Warn => "warn",
            _ => return,
        };
        if let Some(id) = self.tab_id_of(webview)
            && let Some(page) = self.compat.pages.borrow_mut().get_mut(&id.0)
        {
            page.console.record(level, message);
        }
    }

    /// The compat rule that blocks this subresource load, if any.
    pub(crate) fn compat_blocks(&self, webview: &WebView, request: &Url) -> Option<String> {
        if !self.compat.enabled {
            return None;
        }
        let page = webview.url()?;
        self.compat
            .compat
            .borrow()
            .blocked_by(&page, request)
            .map(str::to_owned)
    }

    /// A web page crashed Servo (`notify_crashed`). The first crash of a host in this session
    /// is reported (log with backtrace + a notice in that host's compat panel); repeats (crash
    /// loops, self-reloading pages) are only counted.
    pub(crate) fn compat_crashed(&self, webview: &WebView, reason: &str, backtrace: Option<&str>) {
        let host = webview
            .url()
            .filter(mythbrowse_compat::matcher::is_web_url)
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| "(internal page)".to_owned());
        let report = self.compat.crashes.borrow_mut().record(&host, reason);
        if !report.first {
            log::debug!(
                "web content crashed again on {host} (#{}): {}",
                report.count,
                report.summary
            );
            return;
        }
        let trace: String = backtrace
            .unwrap_or("")
            .lines()
            .take(24)
            .collect::<Vec<_>>()
            .join("\n");
        warn!("web content crashed on {host}: {reason}\n{trace}");
        let mut notice = format!("This page crashed the engine ({}).", report.summary);
        if let Some(known) = report.known {
            notice.push(' ');
            notice.push_str(known);
            notice.push('.');
        }
        notice.push_str(" Reload to try again.");
        *self.compat.notice.borrow_mut() = Some((host, notice));
        self.mark_chrome_dirty();
    }

    pub(crate) fn compat_tab_closed(&self, tab: TabId) {
        self.compat.pages.borrow_mut().remove(&tab.0);
    }

    // --------------------------------------------------------------------------- pump

    /// Handle what Servo callbacks and the model worker posted. Called on every wake-up.
    pub(crate) fn compat_pump(&self) {
        while let Ok(msg) = self.compat.rx.try_recv() {
            match msg {
                Msg::Probe {
                    tab,
                    url,
                    result,
                    after,
                } => self.compat_on_probe(tab, url, result, after),
                Msg::Dom { tab, run, result } => self.compat_on_dom(tab, run, result),
                Msg::Fix { tab, run, result } => {
                    let mut pages = self.compat.pages.borrow_mut();
                    if let Some(page) = pages.get_mut(&tab.0)
                        && matches!(page.fix, Fix::Running { run: r, .. } if r == run)
                    {
                        page.fix = match result {
                            Ok(candidate) => {
                                let review = repair::review(&candidate);
                                Fix::Ready {
                                    run,
                                    candidate,
                                    review,
                                }
                            }
                            Err(e) => Fix::Failed(e),
                        };
                        drop(pages);
                        self.mark_chrome_dirty();
                    }
                }
                Msg::Tried { tab, run, result } => self.compat_on_tried(tab, run, result),
                Msg::Shot { index, result } => self.compat_run_shot(index, result),
                Msg::Remote { toml, sig } => {
                    let r = self.compat.compat.borrow_mut().install_remote(
                        &toml,
                        &sig,
                        mythbrowse_compat::sign::TRUSTED_KEYS,
                    );
                    match r {
                        Ok(Some(key)) => {
                            info!(
                                "compat: cloud ruleset v{:?} installed (key {key}); applies on next page load",
                                self.compat.compat.borrow().remote_version()
                            );
                            self.compat.install();
                            self.mark_chrome_dirty();
                        }
                        Ok(None) => {}
                        Err(e) => warn!("compat: cloud ruleset refused: {e}"),
                    }
                }
            }
        }
    }

    fn compat_on_probe(
        &self,
        tab: TabId,
        url: String,
        result: Result<String, String>,
        after: bool,
    ) {
        let mut pages = self.compat.pages.borrow_mut();
        let Some(page) = pages.get_mut(&tab.0) else {
            return;
        };
        if page.url != url && !page.url.is_empty() {
            return; // the tab moved on
        }
        page.checking = false;
        match result.and_then(|json| ProbeReport::parse(&json).map_err(|e| e.to_string())) {
            Ok(report) => {
                let assessment = assess(&report, &page.console);
                if after {
                    page.after = Some(assessment);
                } else {
                    info!(
                        "compat: {} score {} ({:?})",
                        host_of(&url).unwrap_or_default(),
                        assessment.score,
                        assessment.level
                    );
                    page.assessment = Some(assessment);
                    page.report = Some(report);
                }
            }
            Err(e) => log::debug!("compat probe failed on {url}: {e}"),
        }
        drop(pages);
        self.mark_chrome_dirty();
        self.redraw();
    }

    fn compat_on_dom(&self, tab: TabId, run: u64, result: Result<String, String>) {
        let mut pages = self.compat.pages.borrow_mut();
        let Some(page) = pages.get_mut(&tab.0) else {
            return;
        };
        let cancel = match &page.fix {
            Fix::Running { run: r, cancel } if *r == run => cancel.clone(),
            _ => return,
        };
        let (Some(report), Some(assessment)) = (page.report.clone(), page.assessment.clone())
        else {
            page.fix = Fix::Failed("Check the page first.".into());
            return;
        };
        let dom =
            result.unwrap_or_else(|e| format!("{{\"error\": \"DOM summary unavailable: {e}\"}}"));
        let prompt = repair::user_prompt(&page.url, &assessment, &report, &page.console, &dom);
        drop(pages);
        let endpoint = self.agent.chat_endpoint();
        info!(
            "compat: asking `{}` ({}) for a patch",
            endpoint.model,
            if endpoint.is_local() {
                "local"
            } else {
                "remote"
            }
        );
        let (tx, wake) = self.compat.post();
        let spawned = std::thread::Builder::new()
            .name("compat-repair".into())
            .spawn(move || {
                let messages = vec![
                    ChatMessage::system(repair::SYSTEM_PROMPT),
                    ChatMessage::user(prompt),
                ];
                let result = endpoint
                    .stream_chat(&messages, None, &cancel, &mut |_| {})
                    .map_err(|e| e.to_string())
                    .and_then(|turn| repair::parse_reply(&turn.content));
                if tx.send(Msg::Fix { tab, run, result }).is_ok() {
                    wake();
                }
            });
        if let Err(e) = spawned {
            if let Some(page) = self.compat.pages.borrow_mut().get_mut(&tab.0) {
                page.fix = Fix::Failed(format!("Could not start: {e}"));
            }
        }
        self.mark_chrome_dirty();
    }

    fn compat_on_tried(&self, tab: TabId, run: u64, result: Result<String, String>) {
        let view = {
            let mut pages = self.compat.pages.borrow_mut();
            let Some(page) = pages.get_mut(&tab.0) else {
                return;
            };
            let Fix::Ready {
                run: r, candidate, ..
            } = &page.fix
            else {
                return;
            };
            if *r != run {
                return;
            }
            match result {
                Ok(applied) => {
                    info!("compat: preview applied ({applied})");
                    page.preview = Some(candidate.clone());
                    page.after = None;
                }
                Err(e) => {
                    page.fix = Fix::Failed(format!("The patch could not be applied: {e}"));
                    return;
                }
            }
            drop(pages);
            self.tabs.borrow().get(tab).and_then(|t| t.view.clone())
        };
        if let Some(view) = view {
            self.compat_probe(tab, &view, true);
        }
        self.mark_chrome_dirty();
    }

    // ------------------------------------------------------------------------ chrome

    /// `ChromeState::compat` for the active tab (web pages only).
    pub(crate) fn compat_state(&self) -> Option<Value> {
        if !self.compat.enabled {
            return None;
        }
        let (id, url) = {
            let tabs = self.tabs.borrow();
            let t = tabs.active()?;
            (t.id, t.url.clone())
        };
        let parsed = Url::parse(&url)
            .ok()
            .filter(mythbrowse_compat::matcher::is_web_url)?;
        let host = parsed.host_str().unwrap_or("").to_owned();
        let compat = self.compat.compat.borrow();
        let rules: Vec<Value> = compat
            .rules_for(&parsed, false)
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.id, "title": r.title, "kind": r.kind, "source": r.source,
                    "does": r.does, "off": r.off,
                    "deletable": compat.is_local_rule(&r.id),
                })
            })
            .collect();
        let pages = self.compat.pages.borrow();
        let page = pages.get(&id.0);
        let polyfills: Vec<String> = page
            .and_then(|p| p.report.as_ref())
            .map(|r| compat.active_polyfills(&r.rules, &r.missing))
            .unwrap_or_default();
        let assessment = page.and_then(|p| p.assessment.as_ref());
        let fix = match page.map(|p| &p.fix) {
            None | Some(Fix::Idle) => json!({ "state": "idle" }),
            Some(Fix::Running { .. }) => json!({ "state": "running" }),
            Some(Fix::Failed(e)) => json!({ "state": "failed", "error": e }),
            Some(Fix::Ready {
                candidate, review, ..
            }) => json!({
                "state": "ready",
                "candidate": candidate,
                "review": review,
            }),
        };
        let endpoint = self.agent.chat_endpoint();
        let notice = self
            .compat
            .notice
            .borrow()
            .as_ref()
            .filter(|(h, _)| *h == host)
            .map(|(_, n)| n.clone());
        Some(json!({
            "open": self.compat.open.get(),
            "host": host,
            "checking": page.is_some_and(|p| p.checking),
            "checked": assessment.is_some(),
            "level": assessment.map(|a| a.level),
            "score": assessment.map(|a| a.score),
            "findings": assessment.map(|a| a.findings.clone()).unwrap_or_default(),
            "rules": rules,
            "polyfills": polyfills,
            "paused": compat.is_paused(&host),
            "fix": fix,
            "preview": page.and_then(|p| p.preview.as_ref()).is_some(),
            "after": page.and_then(|p| p.after.as_ref()).map(|a| json!({ "score": a.score, "level": a.level })),
            "mode": page.and_then(|p| p.mode.clone()),
            "notice": notice,
            "ai": {
                "model": endpoint.model,
                "local": endpoint.is_local(),
                "ready": endpoint.is_local() || endpoint.key.as_ref().is_some_and(|k| !k.is_empty()),
            },
        }))
    }

    /// Whether the panel should float over the card now.
    /// User tabs whose current page is a human-verification check (Cloudflare, captcha):
    /// the tab strip marks them while they are in the background.
    pub(crate) fn compat_check_tabs(&self) -> Vec<u64> {
        self.compat
            .pages
            .borrow()
            .iter()
            .filter(|(_, p)| p.assessment.as_ref().is_some_and(|a| a.blocked))
            .map(|(id, _)| *id)
            .collect()
    }

    pub(crate) fn compat_panel_open(&self) -> bool {
        self.compat.enabled && self.compat.open.get() && self.card_mode()
    }

    /// Close the panel (click on the page, Esc, tab switch to an internal page...).
    pub(crate) fn compat_close_panel(&self) {
        if self.compat.open.replace(false) {
            self.mark_chrome_dirty();
            self.window.request_redraw();
        }
    }

    fn active_page(&self) -> Option<(TabId, WebView, String)> {
        let tabs = self.tabs.borrow();
        let t = tabs.active()?;
        let view = t.view.clone()?;
        let host = host_of(&t.url)?;
        Some((t.id, view, host))
    }

    /// Rules changed: swap the user scripts and reload the page so they apply.
    fn compat_rules_changed(&self, view: &WebView) {
        self.compat.install();
        view.reload();
    }

    pub(crate) fn compat_api(&self, op: CompatOp) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, chrome::json_ok(v));
        let err = |m: &str| (StatusCode::CONFLICT, chrome::json_error(m));
        if let CompatOp::Panel { open } = op {
            if open {
                self.pw_close_panel(); // one panel at a time over the page
            }
            self.compat.open.set(open && self.compat.enabled);
            self.mark_chrome_dirty();
            // `update_glow` (next event-loop pass) moves the overlay in or out.
            self.window.request_redraw();
            return ok(Value::Null);
        }
        let Some((tab, view, host)) = self.active_page() else {
            return err("Site compat works on web pages only.");
        };
        match op {
            CompatOp::Panel { .. } => unreachable!("handled above"),
            CompatOp::Recheck => {
                self.compat_probe(tab, &view, false);
                ok(Value::Null)
            }
            CompatOp::Fix => {
                let run = self.compat.run_id();
                {
                    let mut pages = self.compat.pages.borrow_mut();
                    let Some(page) = pages.get_mut(&tab.0) else {
                        return err("The page has not finished loading.");
                    };
                    if page.assessment.is_none() {
                        return err("Check the page first.");
                    }
                    if matches!(page.fix, Fix::Running { .. }) {
                        return err("Myth is already looking at this page.");
                    }
                    page.fix = Fix::Running {
                        run,
                        cancel: Arc::new(AtomicBool::new(false)),
                    };
                }
                let (tx, wake) = self.compat.post();
                view.evaluate_javascript(DOM_SUMMARY_SCRIPT, move |result| {
                    if tx
                        .send(Msg::Dom {
                            tab,
                            run,
                            result: js_string(result),
                        })
                        .is_ok()
                    {
                        wake();
                    }
                });
                self.mark_chrome_dirty();
                ok(Value::Null)
            }
            CompatOp::FixCancel | CompatOp::Discard => {
                if let Some(page) = self.compat.pages.borrow_mut().get_mut(&tab.0) {
                    if let Fix::Running { cancel, .. } = &page.fix {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    page.fix = Fix::Idle;
                }
                self.mark_chrome_dirty();
                ok(Value::Null)
            }
            CompatOp::Try => {
                let (run, script) = {
                    let pages = self.compat.pages.borrow();
                    match pages.get(&tab.0).map(|p| &p.fix) {
                        Some(Fix::Ready {
                            run,
                            candidate,
                            review,
                        }) if review.allowed() => (*run, repair::preview_script(candidate)),
                        Some(Fix::Ready { .. }) => {
                            return err("This patch is blocked by the safety check.");
                        }
                        _ => return err("No patch to try."),
                    }
                };
                if self
                    .compat
                    .pages
                    .borrow()
                    .get(&tab.0)
                    .is_some_and(|p| p.preview.is_some())
                {
                    return err("A patch is already applied. Undo first.");
                }
                let (tx, wake) = self.compat.post();
                view.evaluate_javascript(script, move |result| {
                    let result = js_string(result);
                    if tx.send(Msg::Tried { tab, run, result }).is_ok() {
                        wake();
                    }
                });
                ok(Value::Null)
            }
            CompatOp::Keep => {
                let candidate = {
                    let pages = self.compat.pages.borrow();
                    let Some(page) = pages.get(&tab.0) else {
                        return err("Nothing to keep.");
                    };
                    match (&page.fix, &page.preview) {
                        (Fix::Ready { review, .. }, Some(c)) if review.allowed() => c.clone(),
                        _ => return err("Try the patch on this page before keeping it."),
                    }
                };
                let now = mythbrowse_storage::unix_now_ms() as u64;
                let rule = repair::to_rule(&candidate, &host, now, now);
                let id = rule.id.clone();
                let saved = self.compat.compat.borrow_mut().save_local_rule(rule);
                match saved {
                    Ok(()) => {
                        info!("compat: kept patch {id} for {host}");
                        *self.compat.notice.borrow_mut() =
                            Some((host.clone(), format!("Saved as a rule for {host}.")));
                        self.compat_rules_changed(&view);
                        ok(json!({ "rule": id }))
                    }
                    Err(e) => err(&format!("Could not save the rule: {e}")),
                }
            }
            CompatOp::Undo => {
                *self.compat.notice.borrow_mut() = None;
                view.reload();
                ok(Value::Null)
            }
            CompatOp::Rule { id, on } => {
                let changed = self
                    .compat
                    .compat
                    .borrow_mut()
                    .set_rule_off(&id, &host, !on);
                if changed {
                    self.compat_rules_changed(&view);
                }
                ok(json!({ "changed": changed }))
            }
            CompatOp::RuleDelete { id } => {
                let removed = self.compat.compat.borrow_mut().remove_local_rule(&id);
                match removed {
                    Ok(true) => {
                        self.compat_rules_changed(&view);
                        ok(Value::Null)
                    }
                    Ok(false) => err("Only your own rules can be deleted."),
                    Err(e) => err(&format!("Could not delete the rule: {e}")),
                }
            }
            CompatOp::Pause { on } => {
                let changed = self.compat.compat.borrow_mut().set_paused(&host, on);
                if changed {
                    self.compat_rules_changed(&view);
                }
                ok(json!({ "changed": changed }))
            }
            CompatOp::Mode { mode } => {
                if let Some(page) = self.compat.pages.borrow_mut().get_mut(&tab.0) {
                    page.mode = (mode != "off").then(|| mode.clone());
                }
                view.evaluate_javascript(bundle::mode_script(&mode), |_| {});
                self.mark_chrome_dirty();
                ok(Value::Null)
            }
        }
    }
}

// ------------------------------------------------------------------ top-sites compat run

/// `MYTHBROWSE_COMPAT_RUN=<file with one URL per line>` + `MYTHBROWSE_COMPAT_OUT=<dir>`: load
/// each URL in the active tab, wait for it to load and settle, run the detector, save a
/// screenshot and append one JSON line to `<dir>/report.jsonl`, then exit. Used by the
/// manual top-sites compat workflow (`.github/workflows/compat.yml`, `ci/compat/`).
pub(crate) struct SiteRun {
    urls: Vec<Url>,
    out: std::path::PathBuf,
    settle: std::time::Duration,
    timeout: std::time::Duration,
    index: usize,
    phase: Phase,
    since: std::time::Instant,
    started: std::time::Instant,
    timed_out: bool,
    record: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Navigating; waiting for the load-complete probe.
    Loading,
    /// Loaded; letting late scripts run.
    Settling,
    /// Final probe in flight.
    Probing,
    /// Screenshot in flight.
    Shooting,
    Done,
}

const STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl SiteRun {
    fn from_env() -> Option<SiteRun> {
        let list = std::env::var_os("MYTHBROWSE_COMPAT_RUN")?;
        let out = std::env::var_os("MYTHBROWSE_COMPAT_OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("compat-out"));
        let text = std::fs::read_to_string(&list)
            .map_err(|e| warn!("compat run: cannot read {}: {e}", list.to_string_lossy()))
            .ok()?;
        let urls: Vec<Url> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let with_scheme = if l.contains("://") {
                    l.to_owned()
                } else {
                    format!("https://{l}/")
                };
                Url::parse(&with_scheme).ok()
            })
            .collect();
        if let Err(e) = std::fs::create_dir_all(&out) {
            warn!("compat run: cannot create {}: {e}", out.display());
            return None;
        }
        let ms = |k: &str, d: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        info!("compat run: {} sites -> {}", urls.len(), out.display());
        let now = std::time::Instant::now();
        Some(SiteRun {
            urls,
            out,
            settle: std::time::Duration::from_millis(ms("MYTHBROWSE_COMPAT_SETTLE_MS", 3000)),
            timeout: std::time::Duration::from_millis(ms("MYTHBROWSE_COMPAT_TIMEOUT_MS", 25000)),
            index: 0,
            phase: Phase::Done,
            since: now,
            started: now,
            timed_out: false,
            record: None,
        })
    }
}

fn png_bytes(image: &mythbrowse_engine::servo::RgbaImage) -> Option<Vec<u8>> {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return None;
    }
    let mut data = image.as_raw().clone();
    for px in data.chunks_mut(4) {
        px[3] = 255;
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&data).ok()?;
    }
    Some(out)
}

impl AppState {
    /// Start the top-sites run, if configured (after the first tab exists).
    pub(crate) fn compat_run_start(&self) {
        let first = {
            let mut run = self.compat.run.borrow_mut();
            let Some(run) = run.as_mut() else { return };
            if run.urls.is_empty() {
                run.phase = Phase::Done;
                None
            } else {
                Some(run.urls[0].clone())
            }
        };
        match first {
            Some(url) => self.compat_run_load(0, url),
            None => self.compat_run_finish(),
        }
    }

    fn compat_run_load(&self, index: usize, url: Url) {
        info!("compat run: [{}] {url}", index + 1);
        if let Some(run) = self.compat.run.borrow_mut().as_mut() {
            run.index = index;
            run.phase = Phase::Loading;
            run.since = std::time::Instant::now();
            run.started = run.since;
            run.timed_out = false;
            run.record = None;
        }
        if let Some((tab, view)) = self.active_tab_view() {
            self.compat.pages.borrow_mut().remove(&tab.0);
            view.load(url);
        }
    }

    fn active_tab_view(&self) -> Option<(TabId, WebView)> {
        let tabs = self.tabs.borrow();
        let t = tabs.active()?;
        Some((t.id, t.view.clone()?))
    }

    /// Advance the run; returns when it next needs a look. Called from `about_to_wait`.
    pub(crate) fn compat_run_tick(&self) -> Option<std::time::Instant> {
        let (phase, since, settle, timeout) = {
            let run = self.compat.run.borrow();
            let run = run.as_ref()?;
            (run.phase, run.since, run.settle, run.timeout)
        };
        let now = std::time::Instant::now();
        let (tab, view) = self.active_tab_view()?;
        let page_state = self
            .compat
            .pages
            .borrow()
            .get(&tab.0)
            .map(|p| (p.report.is_some(), p.checking));
        match phase {
            Phase::Loading => {
                let loaded = page_state.is_some_and(|(report, checking)| report && !checking);
                if loaded || now >= since + timeout {
                    if let Some(run) = self.compat.run.borrow_mut().as_mut() {
                        run.timed_out = !loaded;
                        run.phase = Phase::Settling;
                        run.since = now;
                    }
                    return Some(now + settle);
                }
                Some(since + timeout)
            }
            Phase::Settling => {
                if now < since + settle {
                    return Some(since + settle);
                }
                if let Some(run) = self.compat.run.borrow_mut().as_mut() {
                    run.phase = Phase::Probing;
                    run.since = now;
                }
                self.compat_probe(tab, &view, false);
                Some(now + STEP_TIMEOUT)
            }
            Phase::Probing => {
                let done = page_state.is_some_and(|(_, checking)| !checking);
                if !done && now < since + STEP_TIMEOUT {
                    return Some(since + STEP_TIMEOUT);
                }
                self.compat_run_record(tab, &view);
                let (index, path) = {
                    let mut run = self.compat.run.borrow_mut();
                    let run = run.as_mut()?;
                    run.phase = Phase::Shooting;
                    run.since = now;
                    let host = run.urls[run.index].host_str().unwrap_or("site").to_owned();
                    (
                        run.index,
                        run.out.join(format!("{:03}-{host}.png", run.index + 1)),
                    )
                };
                let (tx, wake) = self.compat.post();
                view.take_screenshot(None, move |result| {
                    // Inside Servo's paint: only encode, write and post (no Servo calls).
                    let result = match result {
                        Ok(image) => png_bytes(&image)
                            .ok_or_else(|| "empty screenshot".to_owned())
                            .and_then(|bytes| {
                                std::fs::write(&path, bytes).map_err(|e| e.to_string())
                            })
                            .map(|_| {
                                path.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            }),
                        Err(e) => Err(format!("{e:?}")),
                    };
                    if tx.send(Msg::Shot { index, result }).is_ok() {
                        wake();
                    }
                });
                self.redraw();
                Some(now + STEP_TIMEOUT)
            }
            Phase::Shooting => {
                if now >= since + STEP_TIMEOUT {
                    let index = self.compat.run.borrow().as_ref()?.index;
                    self.compat_run_shot(index, Err("screenshot timed out".into()));
                    return Some(now);
                }
                Some(since + STEP_TIMEOUT)
            }
            Phase::Done => None,
        }
    }

    /// Build the JSON line for the current site from the latest probe.
    fn compat_run_record(&self, tab: TabId, view: &WebView) {
        let pages = self.compat.pages.borrow();
        let page = pages.get(&tab.0);
        let mut run = self.compat.run.borrow_mut();
        let Some(run) = run.as_mut() else { return };
        let assessment = page.and_then(|p| p.assessment.as_ref());
        run.record = Some(json!({
            "index": run.index + 1,
            "input": run.urls[run.index].as_str(),
            "final_url": view.url().map(|u| u.to_string()),
            "title": view.page_title(),
            "load_ms": run.started.elapsed().as_millis() as u64,
            "timed_out": run.timed_out,
            "score": assessment.map(|a| a.score),
            "level": assessment.map(|a| a.level),
            "findings": assessment.map(|a| a.findings.clone()).unwrap_or_default(),
            "blocked": assessment.is_some_and(|a| a.blocked),
            // No probe answer within the step timeout: the page's script thread is stuck.
            "hung": assessment.is_none(),
            "console_errors": page.map(|p| p.console.errors).unwrap_or(0),
            "console": page.map(|p| p.console.recent.clone()).unwrap_or_default(),
            "report": page.and_then(|p| p.report.clone()),
        }));
    }

    fn compat_run_shot(&self, index: usize, result: Result<String, String>) {
        let next = {
            let mut run = self.compat.run.borrow_mut();
            let Some(run) = run.as_mut() else { return };
            if run.phase != Phase::Shooting || run.index != index {
                return;
            }
            let mut record = run.record.take().unwrap_or_else(|| json!({}));
            match result {
                Ok(file) => record["screenshot"] = json!(file),
                Err(e) => record["screenshot_error"] = json!(e),
            }
            use std::io::Write as _;
            let line = format!("{record}\n");
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(run.out.join("report.jsonl"))
                .and_then(|mut f| f.write_all(line.as_bytes()));
            if let Err(e) = written {
                warn!("compat run: report not written: {e}");
            }
            run.phase = Phase::Done;
            run.urls.get(index + 1).cloned()
        };
        match next {
            Some(url) => self.compat_run_load(index + 1, url),
            None => self.compat_run_finish(),
        }
    }

    fn compat_run_finish(&self) {
        if let Some(run) = self.compat.run.borrow().as_ref() {
            let _ = std::fs::write(run.out.join("done"), format!("{}\n", run.urls.len()));
            info!("compat run: finished {} sites", run.urls.len());
        }
        self.exit_requested.set(true);
        self.window.request_redraw();
    }
}

impl AppState {
    /// Settings > Site compatibility: "Automatically update site-fix rules".
    pub(crate) fn compat_auto_update(&self) -> bool {
        self.compat.auto_update.load(Ordering::Relaxed)
    }

    /// Whether this build can download site-fix rules at all (signing key + HTTPS cloud
    /// project configured at build time).
    pub(crate) fn compat_cloud_rules_available(&self) -> bool {
        !mythbrowse_compat::sign::TRUSTED_KEYS.is_empty()
            && mythbrowse_sync::appwrite::cloud_configured()
    }

    pub(crate) fn compat_set_auto_update(&self, on: bool) {
        self.compat.auto_update.store(on, Ordering::Relaxed);
        self.compat.compat.borrow_mut().set_auto_update(on);
        info!(
            "compat: automatic site-fix rule updates {}",
            if on { "on" } else { "off" }
        );
    }
}
