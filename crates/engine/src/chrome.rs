// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The browser chrome protocol: how the HTML/CSS UI in `ui/` talks to the Rust shell.
//!
//! The chrome (tab strip + toolbar), the AI chat home and the library page are served from
//! a reserved internal origin, [`INTERNAL_ORIGIN`]. The shell intercepts every request to
//! it in `WebViewDelegate::load_web_resource` (nothing reaches the network):
//!
//! * `GET /<file>` serves a UI file (from `ui/` on disk if present, else the copy embedded
//!   at build time).
//! * `GET /api/<op>?t=<token>&<params>` is an RPC from UI JavaScript (`fetch`) to the
//!   shell; the reply is JSON. See [`route`] for the access model (per-session token,
//!   top-level internal documents only, browser-driving ops only from the chrome webview).
//!
//! Rust → UI updates go the other way through `WebView::evaluate_javascript`, calling
//! `window.mb.render(state)` in the chrome with a [`ChromeState`] snapshot.
//!
//! Everything here is engine-agnostic so it is unit-tested without Servo.

use std::borrow::Cow;
use std::path::Path;

use serde::Serialize;
use url::Url;

use crate::agent_api::{self, AgentCall};
use crate::tabs::{TabId, TabManager};

/// Host of the internal origin. `.internal` is reserved for private use (ICANN, 2024), so it
/// can never collide with a public site.
pub const INTERNAL_HOST: &str = "mythbrowse.internal";
/// Origin serving the chrome, the AI home and the library page.
pub const INTERNAL_ORIGIN: &str = "https://mythbrowse.internal";

/// Height of the chrome strip in CSS pixels: the custom title bar (tabs + window controls,
/// [`TITLEBAR_CSS_HEIGHT`]) plus the toolbar (48). Must match `--chrome-height` in
/// `ui/chrome.css`.
pub const CHROME_CSS_HEIGHT: f32 = 88.0;

/// Height of the title-bar row (tabs, drag area, window controls) in CSS pixels. Must match
/// `--titlebar-height` in `ui/chrome.css`.
pub const TITLEBAR_CSS_HEIGHT: f32 = 40.0;

/// Width of the invisible resize border around the frameless window, in CSS pixels.
pub const RESIZE_BORDER_CSS: f32 = 6.0;

/// Product name, used for the window title.
pub const APP_NAME: &str = "MythBrowse";

fn internal(path: &str) -> Url {
    Url::parse(INTERNAL_ORIGIN)
        .and_then(|u| u.join(path))
        .expect("valid internal URL")
}

/// The chrome UI page (loaded in its own webview at the top of the window).
pub fn chrome_url() -> Url {
    internal("/chrome.html")
}

/// The AI chat home. With `prompt`, the home page asks the AI right away.
pub fn home_url(prompt: Option<&str>) -> Url {
    let mut url = internal("/home.html");
    if let Some(p) = prompt {
        url.query_pairs_mut().append_pair("q", p);
    }
    url
}

/// The AI home showing conversation `conv` (replayed from its event log).
pub fn conversation_url(conv: u64) -> Url {
    let mut url = internal("/home.html");
    url.query_pairs_mut().append_pair("c", &conv.to_string());
    url
}

/// Tasks panel: what the AI is doing in background tabs.
pub fn tasks_url() -> Url {
    internal("/tasks.html")
}

/// History and bookmarks page.
pub fn library_url() -> Url {
    internal("/library.html")
}

pub fn is_internal(url: &Url) -> bool {
    url.scheme() == "https" && url.host_str() == Some(INTERNAL_HOST) && url.port().is_none()
}

pub fn is_internal_str(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| is_internal(&u))
}

pub fn is_home(url: &Url) -> bool {
    is_internal(url) && url.path() == "/home.html"
}

/// Text shown in the address bar for a page URL: empty on the AI home (so the bar shows its
/// "Ask AI or enter URL" placeholder), the URL otherwise.
pub fn display_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) if is_home(&u) => String::new(),
        _ => url.to_owned(),
    }
}

/// Product switches fixed at build time.
///
/// `cloud_sync` (cargo feature `cloud_sync`, **off by default**): the account + end-to-end
/// encrypted sync UI and its `/api/sync_*` RPCs. The Microsoft Store preview ships without it;
/// the code stays in the tree and comes back with `--features cloud_sync` on the shell. With it
/// off, the sync parts of the UI files (between [`CLOUD_SYNC_BEGIN`] / [`CLOUD_SYNC_END`]
/// marker lines) are cut out when served and every `sync_*` op is an unknown op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    pub cloud_sync: bool,
}

impl Features {
    /// What this binary was built with.
    pub const BUILD: Features = Features {
        cloud_sync: cfg!(feature = "cloud_sync"),
    };
}

/// Marker lines around cloud-sync-only parts of `ui/` files (HTML or JS comments).
pub const CLOUD_SYNC_BEGIN: &str = "cloud_sync:begin";
pub const CLOUD_SYNC_END: &str = "cloud_sync:end";

/// Remove every line from one containing `begin` through the next line containing `end`
/// (both included). An unclosed block runs to the end of the text (fails closed).
pub fn strip_marked(text: &str, begin: &str, end: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for line in text.split_inclusive('\n') {
        if !inside && line.contains(begin) {
            inside = true;
        }
        if !inside {
            out.push_str(line);
        } else if line.contains(end) {
            inside = false;
        }
    }
    out
}

/// A finite CSS length from the chrome (px, |v| <= 100000).
/// The UI's local date for the system prompt (`new Date().toDateString()`): trimmed, at most
/// 40 chars, control characters dropped; empty means "not given".
fn today_param(v: Option<String>) -> Option<String> {
    let t: String = v?.chars().filter(|c| !c.is_control()).take(40).collect();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

fn css_num(v: String, name: &'static str) -> Result<f32, ApiError> {
    v.parse::<f32>()
        .ok()
        .filter(|n| n.is_finite() && n.abs() <= 100_000.0)
        .ok_or(ApiError::BadParam(name))
}

/// UI files the internal origin serves, embedded at build time.
const ASSETS: &[(&str, &[u8])] = &[
    ("chrome.html", include_bytes!("../../../ui/chrome.html")),
    ("chrome.css", include_bytes!("../../../ui/chrome.css")),
    ("chrome.js", include_bytes!("../../../ui/chrome.js")),
    ("popups.js", include_bytes!("../../../ui/popups.js")),
    ("source.html", include_bytes!("../../../ui/source.html")),
    ("source.js", include_bytes!("../../../ui/source.js")),
    (
        "sitesettings.js",
        include_bytes!("../../../ui/sitesettings.js"),
    ),
    ("cookies.js", include_bytes!("../../../ui/cookies.js")),
    ("home.html", include_bytes!("../../../ui/home.html")),
    ("home.css", include_bytes!("../../../ui/home.css")),
    ("home.js", include_bytes!("../../../ui/home.js")),
    ("onboarding.js", include_bytes!("../../../ui/onboarding.js")),
    (
        "design/logo.svg",
        include_bytes!("../../../ui/design/logo.svg"),
    ),
    ("library.html", include_bytes!("../../../ui/library.html")),
    ("library.js", include_bytes!("../../../ui/library.js")),
    (
        "downloads.html",
        include_bytes!("../../../ui/downloads.html"),
    ),
    ("downloads.js", include_bytes!("../../../ui/downloads.js")),
    (
        "passwords.html",
        include_bytes!("../../../ui/passwords.html"),
    ),
    ("passwords.js", include_bytes!("../../../ui/passwords.js")),
    ("tasks.html", include_bytes!("../../../ui/tasks.html")),
    ("tasks.js", include_bytes!("../../../ui/tasks.js")),
    ("chat.js", include_bytes!("../../../ui/chat.js")),
    ("assist.js", include_bytes!("../../../ui/assist.js")),
    ("assist.css", include_bytes!("../../../ui/assist.css")),
    ("chatlist.js", include_bytes!("../../../ui/chatlist.js")),
    ("settings.js", include_bytes!("../../../ui/settings.js")),
    (
        "design/tokens.css",
        include_bytes!("../../../ui/design/tokens.css"),
    ),
    (
        "design/theme.js",
        include_bytes!("../../../ui/design/theme.js"),
    ),
    (
        "design/icons.js",
        include_bytes!("../../../ui/design/icons.js"),
    ),
    ("design/sky.js", include_bytes!("../../../ui/design/sky.js")),
    (
        "design/motion.js",
        include_bytes!("../../../ui/design/motion.js"),
    ),
    (
        "design/select.js",
        include_bytes!("../../../ui/design/select.js"),
    ),
];

pub fn mime_for(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// Look up a UI file by its internal path (`/chrome.html`). Only allowlisted names are
/// served, so there is no path traversal. When `ui_dir` is given and holds the file, the
/// on-disk copy wins (edit-and-reload during development); otherwise the embedded copy.
pub fn asset(path: &str, ui_dir: Option<&Path>) -> Option<(&'static str, Cow<'static, [u8]>)> {
    asset_with(path, ui_dir, Features::BUILD)
}

/// [`asset`] for the given [`Features`] (tests check both builds).
pub fn asset_with(
    path: &str,
    ui_dir: Option<&Path>,
    features: Features,
) -> Option<(&'static str, Cow<'static, [u8]>)> {
    let name = path.strip_prefix('/').unwrap_or(path);
    let name = if name.is_empty() { "home.html" } else { name };
    let (name, embedded) = ASSETS.iter().find(|(n, _)| *n == name)?;
    let body = ui_dir
        .and_then(|dir| std::fs::read(dir.join(name)).ok())
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed(*embedded));
    let mime = mime_for(name);
    let text_ui = mime.starts_with("text/html") || mime.starts_with("text/javascript");
    if !features.cloud_sync
        && text_ui
        && let Ok(text) = std::str::from_utf8(&body)
        && text.contains(CLOUD_SYNC_BEGIN)
    {
        let cut = strip_marked(text, CLOUD_SYNC_BEGIN, CLOUD_SYNC_END);
        return Some((mime, Cow::Owned(cut.into_bytes())));
    }
    Some((mime, body))
}

/// An RPC from UI JavaScript to the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiCall {
    // --- chrome webview only -------------------------------------------------------
    /// Current [`ChromeState`] (the chrome also receives pushes).
    State,
    /// Address-bar submit: URL, `? search`, or AI prompt. `shot`: "Ask about this page",
    /// a prompt sent with a screenshot of the current tab (vision models).
    Navigate {
        input: String,
        shot: bool,
    },
    Back,
    Forward,
    Reload,
    NewTab,
    CloseTab(TabId),
    ActivateTab(TabId),
    ToggleBookmark,
    /// Give keyboard focus back to the page (Esc in the address bar).
    FocusContent,
    OpenLibrary,
    /// Open the full Tasks page, or switch to it ("All tasks" in the Tasks panel).
    OpenTasks,
    /// The floating Tasks panel (toolbar Tasks button, Ctrl+J): `open` = show / hide /
    /// toggle (`None`: unchanged), `h` = its measured content height in CSS px.
    TasksPanel {
        open: Option<PanelOpen>,
        h: Option<u32>,
    },
    /// The size the chrome measured for Myth's surface: the pill's content width `w` or the
    /// approval card's content height `h` (CSS px); the shell sizes the overlay to it.
    MythSize {
        w: Option<u32>,
        h: Option<u32>,
    },
    /// Start moving the window (mouse-down on an empty part of the custom title bar).
    WindowDrag,
    WindowMinimize,
    /// Maximize, or restore if already maximized (title-bar button or double-click).
    WindowToggleMaximize,
    WindowClose,
    /// Myth pill (Myth working on the page in the card): open the conversation in a tab.
    MythOpen {
        conv: u64,
    },
    /// Myth pill: hide the finished run's pill on the active tab.
    MythDismiss,
    /// The chrome's sky tone changed (`dark`: dusk / evening / night): the card glow
    /// brightens on a dark sky.
    ChromeTone {
        dark: bool,
    },
    /// The Myth panel over web pages and the selection pill (`docs/AI-OVERLAY.md`).
    Assist(AssistCall),
    // --- any internal page ---------------------------------------------------------
    /// AI conversation, model settings and Tasks panel (see `agent_api`).
    Agent(AgentCall),
    History {
        query: Option<String>,
        limit: usize,
    },
    HistoryDelete {
        url: String,
    },
    /// Clear browsing history (`pages`), saved chats (`chats`) or both.
    HistoryClear {
        scope: ClearScope,
    },
    Bookmarks,
    BookmarkRemove {
        url: String,
    },
    /// UI preferences (`motion`: full / reduced / off / system, and the effective value;
    /// `transparency` 0–100 and `reduce_transparency` on / off / system).
    Prefs,
    /// Save preferences; each is optional but at least one must be given.
    /// `motion`: full / reduced / off, or `system` to follow the OS animation setting again.
    /// `transparency`: the Appearance slider, 0–100. `reduce_transparency`: on / off /
    /// system. Saving the appearance also ends a live preview ([`ApiCall::GlassParams`]).
    /// `onboarded`: 1 once the first-run welcome was finished or skipped (0 shows it again).
    PrefsSave {
        motion: Option<String>,
        transparency: Option<u8>,
        reduce_transparency: Option<Option<bool>>,
        onboarded: Option<bool>,
        /// Which user-agent string to send: `firefox` (default), `chrome` or `servo`.
        ua_mode: Option<String>,
        /// Settings > Site compatibility "Automatically update site-fix rules" (`1` / `0`).
        rules_auto_update: Option<bool>,
    },
    /// Live preview of the Appearance setting while the slider moves (not saved): the shell
    /// re-blurs with the new radius / saturation / brightness and pushes the look to the
    /// chrome and the active page. `reset`: drop the preview, back to the saved setting
    /// (Cancel). Any internal page may ask (the setting lives in the new tab's sheet).
    GlassParams {
        transparency: Option<u8>,
        reduce: Option<bool>,
        reset: bool,
    },
    /// The page's glass surfaces (see `glass`): `r` is the report (checked by
    /// [`crate::glass::parse_report`] here), `hour` the UI's hour of day for the sky
    /// palette, `moving` while a spring moves a surface.
    Glass {
        report: String,
        hour: Option<String>,
        moving: bool,
    },
    /// Site compat indicator / panel (chrome only), see [`CompatOp`].
    Compat(CompatOp),
    // --- chrome UI features (chrome only unless noted) ------------------------------
    /// Address-bar suggestions for `q` (the dropdown; `seq` orders replies). Returns the
    /// ranked rows now; web suggestions, when on, arrive later as `mb.suggest(...)`.
    Suggest {
        q: String,
        seq: u64,
    },
    /// Shift+Delete on a history row of the dropdown.
    SuggestDelete {
        url: String,
    },
    /// The chrome's open popup (dropdown, context menu, dialog / permission sheet) in window
    /// CSS px, so the shell composes it over the page and routes its pointer events to the
    /// chrome; `None` = closed.
    Popup {
        rect: Option<crate::frame::PopupCss>,
    },
    /// The context menu `id` was answered: a [`crate::page_ui::MenuAction`] id, or `None`
    /// (dismissed).
    MenuPick {
        id: u64,
        action: Option<String>,
    },
    /// Tab strip context menu command on tab `id`.
    TabMenu {
        id: TabId,
        action: crate::page_ui::TabAction,
    },
    /// The user answered dialog / permission sheet `id`: `ok` (OK / Allow / Leave),
    /// `value` (prompt text), `suppress` ("Prevent this page from creating more
    /// dialogs"), `remember` (permission: remember for this site).
    DialogAnswer {
        id: u64,
        ok: bool,
        value: Option<String>,
        suppress: bool,
        remember: bool,
    },
    /// The pop-up blocked indicator: `always` = always allow pop-ups from this site,
    /// else just hide the indicator.
    Popups {
        always: bool,
    },
    /// Where the title bar's maximize button is (window CSS px), for the Windows 11 Snap
    /// Layouts flyout (`None`: hidden).
    MaxButton {
        rect: Option<(f32, f32, f32, f32)>,
    },
    /// Page zoom of the active tab (v0.4.2): `dir` > 0 in, < 0 out, 0 = back to 100 % (the
    /// address bar's zoom indicator; Ctrl + / - / 0 go straight to the shell).
    Zoom {
        dir: i32,
    },
    /// Find in page (v0.4.2): search the active tab for `query` (`dir`: a new / edited
    /// search, next or previous match). The answer arrives as `ChromeState::find`.
    Find {
        query: String,
        dir: crate::browse::FindDir,
    },
    /// Close the find bar (clears the highlights; the keyboard goes back to the page).
    FindClose,
    /// Any internal page: remembered site permissions and the search-suggestions switch.
    SiteSettings,
    /// Any internal page: set `origin`'s `feature` to allow / block, or `None` (ask).
    SiteSettingSet {
        origin: String,
        feature: String,
        decision: Option<bool>,
    },
    /// Any internal page: forget everything for `origin`.
    SiteClear {
        origin: String,
    },
    /// Any internal page: turn web search suggestions in the address bar on / off.
    SuggestPref {
        on: bool,
    },
    /// Any internal page: the page source captured for "View page source" (`id`).
    Source {
        id: u64,
    },
    /// Account + E2E sync (library page), see [`SyncOp`]. Passwords and passphrases come in
    /// the `x-mb-secret` header as a form (`password=…&passphrase=…&code=…`), never the URL.
    Sync(SyncOp),
    /// Downloads, the "On startup" setting, pinned tabs (see [`crate::files_api`]).
    Files(crate::files_api::FilesOp),
    /// Settings (v0.4): preferences without an op of their own, site data, updates.
    Settings(crate::settings_api::SettingsOp),
    /// Password manager + browser import, see [`crate::passwords_api::PasswordOp`]. The
    /// toolbar panel ops are chrome only; a new password comes in the `x-mb-secret` header.
    Passwords(crate::passwords_api::PasswordOp),
}

/// What "Clear history" removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearScope {
    Pages,
    Chats,
    Both,
}

impl ClearScope {
    pub fn pages(self) -> bool {
        matches!(self, ClearScope::Pages | ClearScope::Both)
    }
    pub fn chats(self) -> bool {
        matches!(self, ClearScope::Chats | ClearScope::Both)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOp {
    Status,
    SignUp {
        email: String,
    },
    SignIn {
        email: String,
    },
    /// First device: choose the sync passphrase.
    SetUp,
    Unlock,
    /// Recovery code + new passphrase.
    Recover,
    Now,
    /// The recovery code was saved; stop showing it.
    Ack,
    SignOut,
}

/// Compat panel actions. All act on the active tab's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatOp {
    /// Show / hide the panel.
    Panel { open: bool },
    /// Run the detector again now.
    Recheck,
    /// Ask Myth for a patch (detector report + structure-only DOM summary to the model).
    Fix,
    /// Stop waiting for the model.
    FixCancel,
    /// Drop the proposed patch.
    Discard,
    /// Apply the proposed patch to this page only (preview).
    Try,
    /// Save the tried patch as a local rule for this exact host.
    Keep,
    /// Reload the page without the preview.
    Undo,
    /// Turn one rule off / on for this host.
    Rule { id: String, on: bool },
    /// Delete a local rule.
    RuleDelete { id: String },
    /// Pause / resume every compat rule on this host.
    Pause { on: bool },
    /// Reader / simplified view of the current page (one shot), or `off`.
    Mode { mode: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    UnknownOp(String),
    MissingParam(&'static str),
    BadParam(&'static str),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::UnknownOp(op) => write!(f, "unknown op `{op}`"),
            ApiError::MissingParam(p) => write!(f, "missing parameter `{p}`"),
            ApiError::BadParam(p) => write!(f, "bad parameter `{p}`"),
        }
    }
}

const MAX_LIMIT: usize = 500;

impl ApiCall {
    /// Parse `/api/<op>?…` on the internal origin.
    pub fn parse(url: &Url) -> Result<ApiCall, ApiError> {
        ApiCall::parse_with(url, Features::BUILD)
    }

    /// [`ApiCall::parse`] for the given [`Features`]: without `cloud_sync` no `sync_*` op
    /// exists.
    pub fn parse_with(url: &Url, features: Features) -> Result<ApiCall, ApiError> {
        let op = url.path().strip_prefix("/api/").unwrap_or("");
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        let required = |name: &'static str| param(name).ok_or(ApiError::MissingParam(name));
        let tab = |name: &'static str| -> Result<TabId, ApiError> {
            required(name)?
                .parse::<u64>()
                .map(TabId)
                .map_err(|_| ApiError::BadParam(name))
        };
        Ok(match op {
            "state" => ApiCall::State,
            "navigate" => ApiCall::Navigate {
                input: required("input")?,
                shot: param("shot").as_deref() == Some("1"),
            },
            "back" => ApiCall::Back,
            "forward" => ApiCall::Forward,
            "reload" => ApiCall::Reload,
            "new_tab" => ApiCall::NewTab,
            "close_tab" => ApiCall::CloseTab(tab("id")?),
            "activate_tab" => ApiCall::ActivateTab(tab("id")?),
            "toggle_bookmark" => ApiCall::ToggleBookmark,
            "focus_content" => ApiCall::FocusContent,
            "open_library" => ApiCall::OpenLibrary,
            "open_tasks" => ApiCall::OpenTasks,
            "tasks_panel" => ApiCall::TasksPanel {
                open: match param("open").as_deref() {
                    None => None,
                    Some("1") => Some(PanelOpen::Show),
                    Some("0") => Some(PanelOpen::Hide),
                    Some("toggle") => Some(PanelOpen::Toggle),
                    Some(_) => return Err(ApiError::BadParam("open")),
                },
                h: css_len(param("h"), "h")?,
            },
            "myth_size" => ApiCall::MythSize {
                w: css_len(param("w"), "w")?,
                h: css_len(param("h"), "h")?,
            },
            "window_drag" => ApiCall::WindowDrag,
            "window_minimize" => ApiCall::WindowMinimize,
            "window_maximize" => ApiCall::WindowToggleMaximize,
            "window_close" => ApiCall::WindowClose,
            "myth_open" => ApiCall::MythOpen {
                conv: required("conv")?
                    .parse()
                    .map_err(|_| ApiError::BadParam("conv"))?,
            },
            "myth_dismiss" => ApiCall::MythDismiss,
            "assist" => {
                let rect = match (param("x"), param("y"), param("w"), param("h")) {
                    (Some(x), Some(y), Some(w), Some(h)) => Some(crate::frame::PopupCss {
                        x: css_num(x, "x")?,
                        y: css_num(y, "y")?,
                        w: css_num(w, "w")?,
                        h: css_num(h, "h")?,
                        r: 0.0,
                    }),
                    _ => None,
                };
                ApiCall::Assist(AssistCall::Panel {
                    open: match param("open").as_deref() {
                        None => None,
                        Some("1") => Some(PanelOpen::Show),
                        Some("0") => Some(PanelOpen::Hide),
                        Some("toggle") => Some(PanelOpen::Toggle),
                        Some(_) => return Err(ApiError::BadParam("open")),
                    },
                    mode: match param("mode") {
                        None => None,
                        Some(m) => Some(AssistMode::parse(&m).ok_or(ApiError::BadParam("mode"))?),
                    },
                    rect,
                    dock_w: css_len(param("dock_w"), "dock_w")?,
                })
            }
            "assist_send" => {
                let action = param("action").unwrap_or_else(|| "ask".into());
                if !ASSIST_ACTIONS.contains(&action.as_str()) {
                    return Err(ApiError::BadParam("action"));
                }
                let text = param("text").unwrap_or_default();
                if text.chars().count() > ASSIST_TEXT_MAX {
                    return Err(ApiError::BadParam("text"));
                }
                ApiCall::Assist(AssistCall::Send {
                    action,
                    text,
                    page: param("page").as_deref() != Some("0"),
                    selection: param("selection").as_deref() != Some("0"),
                    shot: param("shot").as_deref() == Some("1"),
                    lang: param("lang").unwrap_or_default().chars().take(32).collect(),
                    today: today_param(param("today")),
                })
            }
            "assist_new" => ApiCall::Assist(AssistCall::New),
            "assist_stop" => ApiCall::Assist(AssistCall::Stop),
            "assist_share" => ApiCall::Assist(AssistCall::Share {
                share: flag(param("share"), "share")?,
            }),
            "assist_reveal" => ApiCall::Assist(AssistCall::Reveal {
                block: required("block")?
                    .parse::<u32>()
                    .ok()
                    .filter(|b| (1..=100_000).contains(b))
                    .ok_or(ApiError::BadParam("block"))?,
            }),
            "assist_sel" => {
                let action = required("action")?;
                if !matches!(
                    action.as_str(),
                    "ask" | "explain" | "translate" | "summarize_selection" | "rewrite"
                ) {
                    return Err(ApiError::BadParam("action"));
                }
                ApiCall::Assist(AssistCall::Sel {
                    action,
                    lang: param("lang").unwrap_or_default().chars().take(32).collect(),
                    today: today_param(param("today")),
                })
            }
            "assist_sel_dismiss" => ApiCall::Assist(AssistCall::SelDismiss),
            "chrome_tone" => ApiCall::ChromeTone {
                dark: match required("dark")?.as_str() {
                    "1" => true,
                    "0" => false,
                    _ => return Err(ApiError::BadParam("dark")),
                },
            },
            "prefs" => ApiCall::Prefs,
            "prefs_save" => {
                let motion = match param("motion").as_deref() {
                    None => None,
                    Some(m @ ("full" | "reduced" | "off" | "system")) => Some(m.to_owned()),
                    Some(_) => return Err(ApiError::BadParam("motion")),
                };
                let transparency = match param("transparency") {
                    None => None,
                    Some(v) => Some(
                        crate::glass::parse_transparency(&v)
                            .ok_or(ApiError::BadParam("transparency"))?,
                    ),
                };
                let reduce_transparency = match param("reduce_transparency") {
                    None => None,
                    Some(v) => Some(
                        crate::glass::parse_reduce(&v)
                            .ok_or(ApiError::BadParam("reduce_transparency"))?,
                    ),
                };
                let onboarded = match param("onboarded").as_deref() {
                    None => None,
                    Some("1") => Some(true),
                    Some("0") => Some(false),
                    Some(_) => return Err(ApiError::BadParam("onboarded")),
                };
                let ua_mode = match param("ua_mode").as_deref() {
                    None => None,
                    Some(m @ ("firefox" | "chrome" | "servo")) => Some(m.to_owned()),
                    Some(_) => return Err(ApiError::BadParam("ua_mode")),
                };
                let rules_auto_update = match param("rules_auto_update").as_deref() {
                    None => None,
                    Some("1") => Some(true),
                    Some("0") => Some(false),
                    Some(_) => return Err(ApiError::BadParam("rules_auto_update")),
                };
                if motion.is_none()
                    && transparency.is_none()
                    && reduce_transparency.is_none()
                    && onboarded.is_none()
                    && ua_mode.is_none()
                    && rules_auto_update.is_none()
                {
                    return Err(ApiError::MissingParam("motion"));
                }
                ApiCall::PrefsSave {
                    motion,
                    transparency,
                    reduce_transparency,
                    onboarded,
                    ua_mode,
                    rules_auto_update,
                }
            }
            "glass_params" => {
                let reset = match param("reset").as_deref() {
                    None | Some("0") => false,
                    Some("1") => true,
                    Some(_) => return Err(ApiError::BadParam("reset")),
                };
                let transparency = match param("v") {
                    None if reset => None,
                    None => return Err(ApiError::MissingParam("v")),
                    Some(v) => {
                        Some(crate::glass::parse_transparency(&v).ok_or(ApiError::BadParam("v"))?)
                    }
                };
                let reduce = match param("reduce").as_deref() {
                    None => None,
                    Some("1") => Some(true),
                    Some("0") => Some(false),
                    Some(_) => return Err(ApiError::BadParam("reduce")),
                };
                ApiCall::GlassParams {
                    transparency,
                    reduce,
                    reset,
                }
            }
            "glass" => {
                let report = param("r").unwrap_or_default();
                crate::glass::parse_report(&report).map_err(|_| ApiError::BadParam("r"))?;
                let hour = param("h").filter(|h| parse_theme_hour(h).is_some());
                ApiCall::Glass {
                    report,
                    hour,
                    moving: param("mv").as_deref() == Some("1"),
                }
            }
            op if crate::files_api::is_files_op(op) => {
                ApiCall::Files(crate::files_api::FilesOp::parse(op, &param)?)
            }
            op if crate::settings_api::is_settings_op(op) => {
                ApiCall::Settings(crate::settings_api::SettingsOp::parse(op, &param)?)
            }
            op if op.starts_with("compat_") => ApiCall::Compat(parse_compat(op, &param)?),
            op if crate::passwords_api::is_password_op(op) => {
                ApiCall::Passwords(crate::passwords_api::parse(op, &param)?)
            }
            op if op.starts_with("sync_") && !features.cloud_sync => {
                return Err(ApiError::UnknownOp(op.to_owned()));
            }
            op if op.starts_with("sync_") => ApiCall::Sync(match op {
                "sync_status" => SyncOp::Status,
                "sync_signup" | "sync_signin" => {
                    let email = required("email")?.trim().to_owned();
                    if email.len() > 320 || !email.contains('@') {
                        return Err(ApiError::BadParam("email"));
                    }
                    if op == "sync_signup" {
                        SyncOp::SignUp { email }
                    } else {
                        SyncOp::SignIn { email }
                    }
                }
                "sync_setup" => SyncOp::SetUp,
                "sync_unlock" => SyncOp::Unlock,
                "sync_recover" => SyncOp::Recover,
                "sync_now" => SyncOp::Now,
                "sync_ack" => SyncOp::Ack,
                "sync_signout" => SyncOp::SignOut,
                other => return Err(ApiError::UnknownOp(other.to_owned())),
            }),
            op if agent_api::is_agent_op(op) => ApiCall::Agent(AgentCall::parse(op, url)?),
            "suggest" => ApiCall::Suggest {
                q: {
                    let q = required("q")?;
                    if q.chars().count() > 2000 {
                        return Err(ApiError::BadParam("q"));
                    }
                    q
                },
                seq: param("seq").and_then(|v| v.parse().ok()).unwrap_or(0),
            },
            "suggest_delete" => ApiCall::SuggestDelete {
                url: required("url")?,
            },
            "popup" => ApiCall::Popup {
                rect: if param("clear").is_some() {
                    None
                } else {
                    Some(crate::frame::PopupCss {
                        x: css_num(required("x")?, "x")?,
                        y: css_num(required("y")?, "y")?,
                        w: css_num(required("w")?, "w")?,
                        h: css_num(required("h")?, "h")?,
                        r: param("r").and_then(|v| v.parse().ok()).unwrap_or(0.0),
                    })
                },
            },
            "menu_pick" => ApiCall::MenuPick {
                id: required("id")?
                    .parse()
                    .map_err(|_| ApiError::BadParam("id"))?,
                action: match param("action") {
                    None => None,
                    Some(a) if crate::page_ui::MenuAction::parse(&a).is_some() => Some(a),
                    Some(_) => return Err(ApiError::BadParam("action")),
                },
            },
            "tab_menu" => ApiCall::TabMenu {
                id: tab("id")?,
                action: crate::page_ui::TabAction::parse(&required("action")?)
                    .ok_or(ApiError::BadParam("action"))?,
            },
            "dialog_answer" => ApiCall::DialogAnswer {
                id: required("id")?
                    .parse()
                    .map_err(|_| ApiError::BadParam("id"))?,
                ok: flag(param("ok"), "ok")?,
                value: param("value").map(|v| v.chars().take(10_000).collect()),
                suppress: param("suppress").as_deref() == Some("1"),
                remember: param("remember").as_deref() == Some("1"),
            },
            "popups" => ApiCall::Popups {
                always: param("always").as_deref() == Some("1"),
            },
            "zoom" => ApiCall::Zoom {
                dir: match required("dir")?.as_str() {
                    "in" => 1,
                    "out" => -1,
                    "reset" => 0,
                    _ => return Err(ApiError::BadParam("dir")),
                },
            },
            "find" => {
                let query = param("q").unwrap_or_default();
                if query.chars().count() > crate::browse::FIND_QUERY_MAX {
                    return Err(ApiError::BadParam("q"));
                }
                ApiCall::Find {
                    query,
                    dir: match param("dir") {
                        None => crate::browse::FindDir::New,
                        Some(d) => {
                            crate::browse::FindDir::parse(&d).ok_or(ApiError::BadParam("dir"))?
                        }
                    },
                }
            }
            "find_close" => ApiCall::FindClose,
            "max_button" => ApiCall::MaxButton {
                rect: if param("clear").is_some() {
                    None
                } else {
                    Some((
                        css_num(required("x")?, "x")?,
                        css_num(required("y")?, "y")?,
                        css_num(required("w")?, "w")?,
                        css_num(required("h")?, "h")?,
                    ))
                },
            },
            "site_settings" => ApiCall::SiteSettings,
            "site_setting_set" => ApiCall::SiteSettingSet {
                origin: required("origin")?,
                feature: required("feature")?,
                decision: match required("value")?.as_str() {
                    "allow" => Some(true),
                    "block" => Some(false),
                    "ask" => None,
                    _ => return Err(ApiError::BadParam("value")),
                },
            },
            "site_clear" => ApiCall::SiteClear {
                origin: required("origin")?,
            },
            "suggest_pref" => ApiCall::SuggestPref {
                on: flag(param("on"), "on")?,
            },
            "source" => ApiCall::Source {
                id: required("id")?
                    .parse()
                    .map_err(|_| ApiError::BadParam("id"))?,
            },
            "history" => ApiCall::History {
                query: param("q").filter(|q| !q.trim().is_empty()),
                limit: match param("limit") {
                    None => 100,
                    Some(l) => l
                        .parse::<usize>()
                        .map_err(|_| ApiError::BadParam("limit"))?
                        .clamp(1, MAX_LIMIT),
                },
            },
            "history_delete" => ApiCall::HistoryDelete {
                url: required("url")?,
            },
            "history_clear" => ApiCall::HistoryClear {
                scope: match param("scope").as_deref() {
                    None | Some("pages") => ClearScope::Pages,
                    Some("chats") => ClearScope::Chats,
                    Some("both") => ClearScope::Both,
                    Some(_) => return Err(ApiError::BadParam("scope")),
                },
            },
            "bookmarks" => ApiCall::Bookmarks,
            "bookmark_remove" => ApiCall::BookmarkRemove {
                url: required("url")?,
            },
            other => return Err(ApiError::UnknownOp(other.to_owned())),
        })
    }

    /// Ops that drive the browser itself; only the chrome webview may send them.
    pub fn chrome_only(&self) -> bool {
        if let ApiCall::Files(op) = self {
            return op.chrome_only();
        }
        if let ApiCall::Passwords(op) = self {
            return op.chrome_only();
        }
        matches!(
            self,
            ApiCall::State
                | ApiCall::Navigate { .. }
                | ApiCall::Back
                | ApiCall::Forward
                | ApiCall::Reload
                | ApiCall::NewTab
                | ApiCall::CloseTab(_)
                | ApiCall::ActivateTab(_)
                | ApiCall::ToggleBookmark
                | ApiCall::FocusContent
                | ApiCall::OpenLibrary
                | ApiCall::OpenTasks
                | ApiCall::TasksPanel { .. }
                | ApiCall::MythSize { .. }
                | ApiCall::WindowDrag
                | ApiCall::WindowMinimize
                | ApiCall::WindowToggleMaximize
                | ApiCall::WindowClose
                | ApiCall::MythOpen { .. }
                | ApiCall::MythDismiss
                | ApiCall::ChromeTone { .. }
                | ApiCall::Assist(_)
                | ApiCall::Compat(_)
                | ApiCall::Suggest { .. }
                | ApiCall::SuggestDelete { .. }
                | ApiCall::Popup { .. }
                | ApiCall::MenuPick { .. }
                | ApiCall::TabMenu { .. }
                | ApiCall::DialogAnswer { .. }
                | ApiCall::Popups { .. }
                | ApiCall::MaxButton { .. }
                | ApiCall::Zoom { .. }
                | ApiCall::Find { .. }
                | ApiCall::FindClose
        )
    }
}

/// Myth panel docked right of the card, or floating over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistMode {
    Docked,
    Floating,
}

impl AssistMode {
    pub fn parse(s: &str) -> Option<AssistMode> {
        match s {
            "docked" => Some(AssistMode::Docked),
            "floating" => Some(AssistMode::Floating),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AssistMode::Docked => "docked",
            AssistMode::Floating => "floating",
        }
    }
}

/// Myth panel RPCs (chrome only).
#[derive(Debug, Clone, PartialEq)]
pub enum AssistCall {
    /// `assist?open=1|0|toggle&mode=docked|floating&x=&y=&w=&h=&dock_w=`: show / hide, switch
    /// mode, the floating card's rectangle after a drag (window CSS px), the docked width.
    Panel {
        open: Option<PanelOpen>,
        mode: Option<AssistMode>,
        rect: Option<crate::frame::PopupCss>,
        dock_w: Option<u32>,
    },
    /// `assist_send?action=&text=&page=1&selection=1&shot=1&lang=&today=`: a message or a
    /// one-click action from the panel (see `mythbrowse_ai::page_chat::Action`). `today` is the
    /// user's local date as the UI's clock sees it (the shell has no time zone data).
    Send {
        action: String,
        text: String,
        page: bool,
        selection: bool,
        shot: bool,
        lang: String,
        today: Option<String>,
    },
    /// Start a new conversation on this tab (the old one stays in history).
    New,
    /// Stop the panel's run on this tab.
    Stop,
    /// `assist_share?share=0|1`: the active site's "Don't share with Myth" switch.
    Share { share: bool },
    /// `assist_reveal?block=N`: scroll the page to a Find result.
    Reveal { block: u32 },
    /// `assist_sel?action=`: a selection-pill button (`ask` opens the panel with the
    /// selection chip; the others run at once).
    Sel {
        action: String,
        lang: String,
        today: Option<String>,
    },
    /// Hide the selection pill.
    SelDismiss,
}

const ASSIST_ACTIONS: &[&str] = &[
    "ask",
    "explain",
    "translate",
    "summarize_selection",
    "rewrite",
    "summarize",
    "key_points",
    "compare_tabs",
    "find",
];

/// Most text one panel message may carry (characters).
pub const ASSIST_TEXT_MAX: usize = 8000;

/// Show, hide or toggle a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelOpen {
    Show,
    Hide,
    Toggle,
}

/// An optional length in CSS px (finite, 0 < v <= 10000), rounded up to whole px.
fn css_len(value: Option<String>, name: &'static str) -> Result<Option<u32>, ApiError> {
    match value {
        None => Ok(None),
        Some(v) => match v.parse::<f32>() {
            Ok(n) if n.is_finite() && n > 0.0 && n <= 10_000.0 => Ok(Some(n.ceil() as u32)),
            _ => Err(ApiError::BadParam(name)),
        },
    }
}

fn flag(value: Option<String>, name: &'static str) -> Result<bool, ApiError> {
    match value.as_deref() {
        Some("1") => Ok(true),
        Some("0") => Ok(false),
        Some(_) => Err(ApiError::BadParam(name)),
        None => Err(ApiError::MissingParam(name)),
    }
}

fn parse_compat(op: &str, param: &dyn Fn(&str) -> Option<String>) -> Result<CompatOp, ApiError> {
    let rule_id = || -> Result<String, ApiError> {
        let id = param("id").ok_or(ApiError::MissingParam("id"))?;
        let ok = !id.is_empty()
            && id.len() <= 80
            && id.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
            });
        if ok {
            Ok(id)
        } else {
            Err(ApiError::BadParam("id"))
        }
    };
    Ok(match op {
        "compat_panel" => CompatOp::Panel {
            open: flag(param("open"), "open")?,
        },
        "compat_recheck" => CompatOp::Recheck,
        "compat_fix" => CompatOp::Fix,
        "compat_fix_cancel" => CompatOp::FixCancel,
        "compat_discard" => CompatOp::Discard,
        "compat_try" => CompatOp::Try,
        "compat_keep" => CompatOp::Keep,
        "compat_undo" => CompatOp::Undo,
        "compat_rule" => CompatOp::Rule {
            id: rule_id()?,
            on: flag(param("on"), "on")?,
        },
        "compat_rule_delete" => CompatOp::RuleDelete { id: rule_id()? },
        "compat_pause" => CompatOp::Pause {
            on: flag(param("on"), "on")?,
        },
        "compat_mode" => CompatOp::Mode {
            mode: match param("mode").as_deref() {
                Some(m @ ("reader" | "simplified" | "off")) => m.to_owned(),
                Some(_) => return Err(ApiError::BadParam("mode")),
                None => return Err(ApiError::MissingParam("mode")),
            },
        },
        other => return Err(ApiError::UnknownOp(other.to_owned())),
    })
}

/// What the shell should do with an intercepted request.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Not ours: let Servo load it normally.
    PassThrough,
    /// Serve this UI file path.
    Asset(String),
    /// Run this RPC and answer with JSON.
    Api(ApiCall),
    /// Answer 400 with this message.
    BadRequest(String),
    /// Refuse (cancelled load).
    Deny,
    /// Unknown internal path.
    NotFound,
}

/// Placeholder in UI HTML replaced by the per-session API token when served.
pub const TOKEN_PLACEHOLDER: &str = "{{MB_TOKEN}}";

/// Placeholder in UI HTML replaced by the pinned theme hour (`MYTHBROWSE_THEME_HOUR`, used by
/// the smoke tests to screenshot each time-of-day theme), or by an empty string.
pub const THEME_HOUR_PLACEHOLDER: &str = "{{MB_THEME_HOUR}}";

/// Placeholder in UI HTML replaced by the glass mode the shell runs in (`blur` / `flat` /
/// `off`, see [`crate::glass::GlassMode`]), so a page starts in the right look (no sky flash).
pub const GLASS_PLACEHOLDER: &str = "{{MB_GLASS}}";

/// Fill [`GLASS_PLACEHOLDER`] into an HTML asset already passed through [`render_asset`];
/// other assets are returned unchanged.
pub fn fill_glass_mode(mime: &str, body: Vec<u8>, mode: crate::glass::GlassMode) -> Vec<u8> {
    fill_html(mime, body, &[(GLASS_PLACEHOLDER, mode.as_str())])
}

/// Placeholders in UI HTML replaced by the Appearance setting in effect (the Transparency
/// slider 0–100, and `1` / `0` for Reduce transparency), so a page starts with the right glass
/// before its first paint (no flash of the default look).
pub const TRANSPARENCY_PLACEHOLDER: &str = "{{MB_TRANSPARENCY}}";
pub const REDUCE_TRANSPARENCY_PLACEHOLDER: &str = "{{MB_REDUCE_TRANSPARENCY}}";

/// Fill [`TRANSPARENCY_PLACEHOLDER`] and [`REDUCE_TRANSPARENCY_PLACEHOLDER`] into an HTML
/// asset; other assets are returned unchanged.
pub fn fill_appearance(mime: &str, body: Vec<u8>, transparency: u8, reduced: bool) -> Vec<u8> {
    let v = transparency.min(100).to_string();
    fill_html(
        mime,
        body,
        &[
            (TRANSPARENCY_PLACEHOLDER, &v),
            (
                REDUCE_TRANSPARENCY_PLACEHOLDER,
                if reduced { "1" } else { "0" },
            ),
        ],
    )
}

fn fill_html(mime: &str, body: Vec<u8>, pairs: &[(&str, &str)]) -> Vec<u8> {
    if !mime.starts_with("text/html") {
        return body;
    }
    match String::from_utf8(body) {
        Ok(mut text) => {
            for (k, v) in pairs {
                text = text.replace(k, v);
            }
            text.into_bytes()
        }
        Err(e) => e.into_bytes(),
    }
}

/// Decide how to handle a request to `url`.
///
/// * `is_main_frame`: the request is a top-level navigation.
/// * `from_chrome`: the request comes from the chrome webview.
/// * `token`: the per-session secret. It is written into internal HTML when served and
///   must accompany every RPC as `t=<token>`.
///
/// Access model: internal HTML is only served as a top-level document (never into an
/// iframe or to `fetch`, so web content can neither embed it nor read the token out of it;
/// intercepted responses skip Servo's CORS filtering, so this matters). An RPC is accepted
/// only from `fetch` (never a navigation, so links cannot trigger it) carrying the token,
/// i.e. from a top-level internal page. Ops that drive the browser additionally require
/// the chrome webview.
pub fn route(url: &Url, is_main_frame: bool, from_chrome: bool, token: &str) -> Route {
    if !is_internal(url) {
        return Route::PassThrough;
    }
    if url.path().starts_with("/api/") {
        let token_ok = !token.is_empty()
            && url
                .query_pairs()
                .any(|(k, v)| k == "t" && constant_time_eq(v.as_bytes(), token.as_bytes()));
        if is_main_frame || !token_ok {
            return Route::Deny;
        }
        return match ApiCall::parse(url) {
            Ok(call) if call.chrome_only() && !from_chrome => Route::Deny,
            Ok(call) => Route::Api(call),
            Err(e) => Route::BadRequest(e.to_string()),
        };
    }
    match asset(url.path(), None) {
        Some((mime, _)) if mime.starts_with("text/html") && !is_main_frame => Route::Deny,
        Some(_) => Route::Asset(url.path().to_owned()),
        None => Route::NotFound,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Fill the token (and the pinned theme hour, if any) into an HTML asset; other assets are
/// returned unchanged.
pub fn render_asset(
    mime: &str,
    body: Cow<'static, [u8]>,
    token: &str,
    theme_hour: Option<f64>,
) -> Vec<u8> {
    if mime.starts_with("text/html")
        && let Ok(text) = std::str::from_utf8(&body)
    {
        let hour = theme_hour.map(|h| h.to_string()).unwrap_or_default();
        return text
            .replace(TOKEN_PLACEHOLDER, token)
            .replace(THEME_HOUR_PLACEHOLDER, &hour)
            .into_bytes();
    }
    body.into_owned()
}

/// Parse a theme-hour override (`"21"`, `"6.5"`); only 0 <= h < 24 is accepted.
pub fn parse_theme_hour(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|h| h.is_finite() && (0.0..24.0).contains(h))
}

/// Which edge or corner of a frameless window a point is on (for resizing it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

/// The resize edge under `(x, y)` in a `width` x `height` window, given a `border` width
/// (all in physical pixels). Corners get a larger grab area (twice the border).
pub fn resize_edge(x: f32, y: f32, width: f32, height: f32, border: f32) -> Option<Edge> {
    if border <= 0.0 || width <= border * 4.0 || height <= border * 4.0 {
        return None;
    }
    let corner = border * 2.0;
    let (n, s) = (y < border, y >= height - border);
    let (w, e) = (x < border, x >= width - border);
    let (near_n, near_s) = (y < corner, y >= height - corner);
    let (near_w, near_e) = (x < corner, x >= width - corner);
    Some(match () {
        _ if (n && near_w) || (w && near_n) => Edge::NorthWest,
        _ if (n && near_e) || (e && near_n) => Edge::NorthEast,
        _ if (s && near_w) || (w && near_s) => Edge::SouthWest,
        _ if (s && near_e) || (e && near_s) => Edge::SouthEast,
        _ if n => Edge::North,
        _ if s => Edge::South,
        _ if w => Edge::West,
        _ if e => Edge::East,
        _ => return None,
    })
}

/// Window title for the active tab: the AI home is just the product name; other pages are
/// "<page> — MythBrowse" (never "MythBrowse — MythBrowse").
pub fn window_title(title: &str, url: &str) -> String {
    let is_home_page = Url::parse(url).is_ok_and(|u| is_home(&u));
    let label = tab_label(title, url);
    if is_home_page || url.is_empty() || label == APP_NAME || label == "New tab" {
        APP_NAME.to_owned()
    } else {
        format!("{label} \u{2014} {APP_NAME}")
    }
}

/// A fresh per-session token (128 bits from the std hasher's random keys + time; good
/// enough for a same-process secret that never leaves the machine).
pub fn new_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut out = String::with_capacity(32);
    for salt in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u64(salt);
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// One tab as the chrome shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TabInfo {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub active: bool,
    /// Myth works in this tab: `thinking` / `controlling` / `waiting` (set by the shell; the
    /// tab shows a small status dot).
    pub myth: Option<&'static str>,
    pub sleeping: bool,
    pub loading: bool,
    pub pinned: bool,
    /// A background tab is showing a human-verification check (the site wants the person to
    /// prove they are human): the tab shows a small, quiet badge. Set by the shell; never set
    /// for the tab in front (no nagging there).
    pub verify: bool,
}

/// Everything the chrome renders, pushed after every change.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChromeState {
    pub tabs: Vec<TabInfo>,
    /// Address-bar text for the active tab (empty on the AI home).
    pub url: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub bookmarked: bool,
    pub loading: bool,
    pub internal: bool,
    /// The window is maximized (the title bar shows "restore" instead of "maximize").
    pub maximized: bool,
    /// The page is shown as a floating card on the sky (web pages; see `frame`).
    pub card: bool,
    /// Colour the chrome is tinted with (the active web page's theme-color or background);
    /// `None` on internal pages and before the page has loaded.
    pub tint: Option<[u8; 3]>,
    /// Myth working on (or just finished with) the page in the card: the Myth pill / approval
    /// card (`{conv, state, step, approval?}`), set by the shell.
    pub myth: Option<serde_json::Value>,
    /// Card geometry in CSS px, set by the shell so the chrome draws the sky gap and Myth's
    /// floating surface exactly where the compositor expects them:
    /// `{gap, overlay: {kind, x, y, w, h, r} | null, panel: {x, y, w, h, r} | null}`
    /// (`panel`: the open Tasks panel, also on internal pages).
    pub frame: Option<serde_json::Value>,
    /// Effective motion setting (`full` / `reduced` / `off`), set by the shell.
    pub motion: &'static str,
    /// Glass mode (`blur` / `flat` / `off`, see [`crate::glass::GlassMode`]), set by the shell.
    pub glass: &'static str,
    /// Appearance in effect (saved, or the live preview while the slider moves): the
    /// Transparency slider 0–100 and whether Reduce transparency is on. Set by the shell.
    pub transparency: u8,
    pub reduce_transparency: bool,
    /// Site compat for the active web page (indicator + panel), set by the shell:
    /// `{open, level, score, findings, rules, fix, ...}`; `None` on internal pages.
    pub compat: Option<serde_json::Value>,
    /// Toolbar Downloads button (`{active, frac, attention}`), set by the shell; `None` hides
    /// it (no downloads yet).
    pub downloads: Option<serde_json::Value>,
    /// Password manager for the active web page (toolbar key, "Save password?" prompt,
    /// account panel), set by the shell: `{accounts, open, prompt, ...}`. Usernames only,
    /// never a password. `None` on internal pages.
    pub passwords: Option<serde_json::Value>,
    /// The active tab's front dialog or permission sheet (`{id, kind, message, origin,
    /// suppress, label}`), set by the shell; `None` when it has none.
    pub dialog: Option<serde_json::Value>,
    /// A context menu to show (`{id, x, y, items}`, window CSS px), set by the shell.
    pub menu: Option<serde_json::Value>,
    /// Pop-ups the active page tried to open and were blocked (`{count, origin}`).
    pub popups_blocked: Option<serde_json::Value>,
    /// Web search suggestions in the address bar are on.
    pub suggest_web: bool,
    /// The Myth panel and selection pill (`{open, mode, dock_w, tab, conv, site, shared,
    /// title, url, model, vision, selection: {words} | null, pill: bool, lang}`), set by the
    /// shell. Never carries page text or the selected text itself.
    pub assist: Option<serde_json::Value>,
    /// The active web page's zoom in percent when it is not 100 % (address-bar indicator,
    /// click to reset), set by the shell.
    pub zoom: Option<u16>,
    /// The find bar (`{query, current, total, focus}`, see `browse::find_json`) while it is
    /// open on the active tab; set by the shell.
    pub find: Option<serde_json::Value>,
    /// The page is in fullscreen (Fullscreen API): the window is borderless fullscreen and
    /// the chrome is hidden behind the page (Esc leaves).
    pub fullscreen: bool,
    /// A short note to show once (`{id, text}`), e.g. a link whose scheme is refused; the
    /// chrome hides it after a few seconds and never shows the same `id` twice.
    pub toast: Option<serde_json::Value>,
}

/// Whether a tab's page is shown as a floating card (web pages) rather than edge to edge
/// under the chrome (internal pages, which draw the sky themselves, and blank tabs).
pub fn shows_as_card(url: &str) -> bool {
    !url.is_empty() && !is_internal_str(url)
}

/// Title shown for a tab: page title, else URL, else "New tab".
pub fn tab_label(title: &str, url: &str) -> String {
    if !title.trim().is_empty() {
        title.trim().to_owned()
    } else if !display_url(url).is_empty() {
        url.to_owned()
    } else {
        "New tab".to_owned()
    }
}

impl ChromeState {
    /// Snapshot the tab list; per-page flags come from the shell (they live in the engine).
    pub fn from_tabs<H>(
        tabs: &TabManager<H>,
        can_go_back: bool,
        can_go_forward: bool,
        bookmarked: bool,
    ) -> Self {
        let active_id = tabs.active_id();
        let active = tabs.active();
        ChromeState {
            tabs: tabs
                .iter()
                .map(|t| TabInfo {
                    id: t.id.0,
                    title: tab_label(&t.title, &t.url),
                    url: t.url.clone(),
                    active: Some(t.id) == active_id,
                    myth: None,
                    sleeping: t.is_sleeping(),
                    loading: t.loading,
                    pinned: t.pinned,
                    verify: false,
                })
                .collect(),
            url: active.map(|t| display_url(&t.url)).unwrap_or_default(),
            can_go_back,
            can_go_forward,
            bookmarked,
            loading: active.is_some_and(|t| t.loading),
            internal: active.is_some_and(|t| is_internal_str(&t.url)),
            maximized: false,
            card: active.is_some_and(|t| shows_as_card(&t.url)),
            tint: active
                .filter(|t| shows_as_card(&t.url))
                .and_then(|t| t.tint),
            myth: None,
            frame: None,
            motion: "full",
            glass: "off",
            transparency: crate::glass::TRANSPARENCY_DEFAULT,
            reduce_transparency: false,
            compat: None,
            downloads: None,
            passwords: None,
            dialog: None,
            menu: None,
            popups_blocked: None,
            suggest_web: true,
            assist: None,
            zoom: None,
            find: None,
            fullscreen: false,
            toast: None,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ChromeState serializes")
    }

    /// Script that hands this state to the chrome page.
    pub fn to_script(&self) -> String {
        format!("window.mb && window.mb.render({});", self.to_json())
    }
}

/// Script that focuses and selects the chrome address bar.
pub const FOCUS_OMNIBOX_SCRIPT: &str = "window.mb && window.mb.focusOmnibox();";

/// `{"ok":true,...}` / `{"ok":false,"error":...}` bodies for RPC replies.
pub fn json_ok(value: serde_json::Value) -> String {
    let mut obj = serde_json::json!({ "ok": true });
    if let (Some(map), serde_json::Value::Object(extra)) = (obj.as_object_mut(), value) {
        map.extend(extra);
    }
    obj.to_string()
}

pub fn json_error(message: &str) -> String {
    serde_json::json!({ "ok": false, "error": message }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNC_ON: Features = Features { cloud_sync: true };
    const SYNC_OFF: Features = Features { cloud_sync: false };

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn text(path: &str, f: Features) -> String {
        String::from_utf8(asset_with(path, None, f).unwrap().1.into_owned()).unwrap()
    }

    const SYNC_OPS: [&str; 9] = [
        "sync_status",
        "sync_signup?email=a%40b.c",
        "sync_signin?email=a%40b.c",
        "sync_setup",
        "sync_unlock",
        "sync_recover",
        "sync_now",
        "sync_ack",
        "sync_signout",
    ];

    #[test]
    fn cloud_sync_is_off_unless_built_with_it() {
        assert_eq!(Features::BUILD.cloud_sync, cfg!(feature = "cloud_sync"));
        #[cfg(not(feature = "cloud_sync"))]
        assert_eq!(Features::BUILD, SYNC_OFF);
    }

    #[test]
    fn no_sync_rpc_without_cloud_sync() {
        for op in SYNC_OPS {
            let sep = if op.contains('?') { '&' } else { '?' };
            let url = u(&format!("{INTERNAL_ORIGIN}/api/{op}{sep}t=tok"));
            let name = op.split('?').next().unwrap();
            assert_eq!(
                ApiCall::parse_with(&url, SYNC_OFF),
                Err(ApiError::UnknownOp(name.into())),
                "{op} must not exist"
            );
            assert!(
                matches!(ApiCall::parse_with(&url, SYNC_ON), Ok(ApiCall::Sync(_))),
                "{op} exists with cloud_sync"
            );
        }
        #[cfg(not(feature = "cloud_sync"))]
        {
            // The real router of this build: a token-carrying fetch is a bad request, never
            // an API call reaching the sync host.
            let url = u(&format!("{INTERNAL_ORIGIN}/api/sync_status?t=tok"));
            assert!(matches!(
                route(&url, false, false, "tok"),
                Route::BadRequest(_)
            ));
            assert!(matches!(
                route(&url, false, true, "tok"),
                Route::BadRequest(_)
            ));
        }
    }

    #[test]
    fn no_sync_ui_without_cloud_sync() {
        // Library (Settings) page: the Sync panel and its scripts are cut out.
        let html = text("/library.html", SYNC_OFF);
        let js = text("/library.js", SYNC_OFF);
        for needle in [
            "sync-panel",
            "sync-account",
            "sync-email",
            "sync-password",
            "sync-setup",
            "sync-chats",
            "Sign in",
            "Create account",
        ] {
            assert!(!html.contains(needle), "library.html still has {needle}");
        }
        for needle in ["sync_", "x-mb-secret", "syncApi", "refreshSync", "sync-"] {
            assert!(!js.contains(needle), "library.js still has {needle}");
        }
        // Nothing else on the internal origin points at sync or a sign-in.
        for (name, _) in ASSETS {
            let path = format!("/{name}");
            let mime = mime_for(name);
            if !(mime.starts_with("text/html") || mime.starts_with("text/javascript")) {
                continue;
            }
            let t = text(&path, SYNC_OFF);
            assert!(
                !t.contains("/api/sync") && !t.contains("\"sync_"),
                "{name} calls sync"
            );
            assert!(
                !t.contains(CLOUD_SYNC_BEGIN) && !t.contains(CLOUD_SYNC_END),
                "{name}"
            );
        }
        // The rest of the page is still there.
        assert!(html.contains("bookmarks-panel") && html.contains("design/tokens.css"));
        assert!(js.contains("loadBookmarks") && js.contains("chat_settings"));
        // With cloud_sync the panel is served unchanged.
        let html_on = text("/library.html", SYNC_ON);
        assert!(html_on.contains("id=\"sync-panel\"") && html_on.contains("sync-email"));
        assert!(text("/library.js", SYNC_ON).contains("\"sync_status\""));
    }

    #[test]
    fn cloud_sync_markers_are_balanced() {
        for (name, body) in ASSETS {
            let t = std::str::from_utf8(body).unwrap();
            let mut open = false;
            for line in t.lines() {
                if line.contains(CLOUD_SYNC_BEGIN) {
                    assert!(!open, "{name}: nested {CLOUD_SYNC_BEGIN}");
                    open = true;
                }
                if line.contains(CLOUD_SYNC_END) {
                    assert!(open, "{name}: {CLOUD_SYNC_END} without begin");
                    open = false;
                }
            }
            assert!(!open, "{name}: unclosed {CLOUD_SYNC_BEGIN}");
        }
    }

    #[test]
    fn strip_marked_cuts_whole_lines() {
        let t = "a\n<!-- x:begin -->\nb\n<!-- x:end -->\nc\n  // x:begin\nd\n  // x:end\ne";
        assert_eq!(strip_marked(t, "x:begin", "x:end"), "a\nc\ne");
        assert_eq!(strip_marked("a\nx:begin\nb", "x:begin", "x:end"), "a\n");
        assert_eq!(strip_marked("plain", "x:begin", "x:end"), "plain");
    }

    #[test]
    fn internal_urls() {
        assert_eq!(
            chrome_url().as_str(),
            "https://mythbrowse.internal/chrome.html"
        );
        assert_eq!(
            home_url(Some("what is servo?")).as_str(),
            "https://mythbrowse.internal/home.html?q=what+is+servo%3F"
        );
        assert!(is_home(&home_url(None)));
        assert!(is_internal(&library_url()));
        assert!(!is_internal(&u("http://mythbrowse.internal/home.html")));
        assert!(!is_internal(&u(
            "https://mythbrowse.internal:444/home.html"
        )));
        assert!(!is_internal(&u("https://mythbrowse.internal.evil.com/")));
        assert!(!is_internal_str("not a url"));
    }

    #[test]
    fn display_hides_home() {
        assert_eq!(display_url(home_url(Some("x")).as_str()), "");
        assert_eq!(display_url("https://servo.org/"), "https://servo.org/");
        assert_eq!(
            display_url(library_url().as_str()),
            "https://mythbrowse.internal/library.html"
        );
    }

    #[test]
    fn assets_are_allowlisted() {
        let (mime, body) = asset("/chrome.html", None).unwrap();
        assert_eq!(mime, "text/html; charset=utf-8");
        assert!(std::str::from_utf8(&body).unwrap().contains("omnibox"));
        assert_eq!(
            asset("/chrome.js", None).unwrap().0,
            "text/javascript; charset=utf-8"
        );
        assert_eq!(asset("/", None).unwrap().0, "text/html; charset=utf-8");
        assert!(asset("/../Cargo.toml", None).is_none());
        assert!(asset("/secret.txt", None).is_none());
    }

    #[test]
    fn disk_copy_overrides_embedded() {
        let dir = std::env::temp_dir().join(format!("mb-ui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("home.css"), b"/* dev */").unwrap();
        assert_eq!(&*asset("/home.css", Some(&dir)).unwrap().1, b"/* dev */");
        // Missing on disk -> embedded copy.
        assert!(!asset("/home.js", Some(&dir)).unwrap().1.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_chrome_ui_calls() {
        use crate::frame::PopupCss;
        use crate::page_ui::TabAction;
        let p = |s: &str| ApiCall::parse_with(&u(&format!("{INTERNAL_ORIGIN}{s}")), SYNC_ON);
        assert_eq!(
            p("/api/suggest?q=git%20hub&seq=7"),
            Ok(ApiCall::Suggest {
                q: "git hub".into(),
                seq: 7
            })
        );
        assert_eq!(p("/api/suggest"), Err(ApiError::MissingParam("q")));
        assert_eq!(
            p(&format!("/api/suggest?q={}", "a".repeat(2001))),
            Err(ApiError::BadParam("q"))
        );
        assert_eq!(
            p("/api/popup?x=10&y=20.5&w=300&h=200&r=12"),
            Ok(ApiCall::Popup {
                rect: Some(PopupCss {
                    x: 10.0,
                    y: 20.5,
                    w: 300.0,
                    h: 200.0,
                    r: 12.0
                })
            })
        );
        assert_eq!(p("/api/popup?clear=1"), Ok(ApiCall::Popup { rect: None }));
        assert_eq!(
            p("/api/popup?x=NaN&y=1&w=1&h=1"),
            Err(ApiError::BadParam("x"))
        );
        assert_eq!(
            p("/api/popup?x=1&y=1&w=1e9&h=1"),
            Err(ApiError::BadParam("w"))
        );
        assert_eq!(
            p("/api/menu_pick?id=3&action=copy_link"),
            Ok(ApiCall::MenuPick {
                id: 3,
                action: Some("copy_link".into())
            })
        );
        assert_eq!(
            p("/api/menu_pick?id=3"),
            Ok(ApiCall::MenuPick {
                id: 3,
                action: None
            })
        );
        assert_eq!(
            p("/api/menu_pick?id=3&action=format"),
            Err(ApiError::BadParam("action"))
        );
        assert_eq!(
            p("/api/tab_menu?id=4&action=close_right"),
            Ok(ApiCall::TabMenu {
                id: TabId(4),
                action: TabAction::CloseRight
            })
        );
        assert_eq!(
            p("/api/dialog_answer?id=9&ok=1&value=hi&suppress=1"),
            Ok(ApiCall::DialogAnswer {
                id: 9,
                ok: true,
                value: Some("hi".into()),
                suppress: true,
                remember: false
            })
        );
        assert_eq!(
            p("/api/dialog_answer?id=9"),
            Err(ApiError::MissingParam("ok"))
        );
        assert_eq!(
            p("/api/site_setting_set?origin=https://a.example&feature=camera&value=ask"),
            Ok(ApiCall::SiteSettingSet {
                origin: "https://a.example".into(),
                feature: "camera".into(),
                decision: None
            })
        );
        assert_eq!(
            p("/api/site_setting_set?origin=o&feature=camera&value=maybe"),
            Err(ApiError::BadParam("value"))
        );
        assert_eq!(
            p("/api/max_button?x=1&y=2&w=46&h=40"),
            Ok(ApiCall::MaxButton {
                rect: Some((1.0, 2.0, 46.0, 40.0))
            })
        );
        assert_eq!(
            p("/api/suggest_pref?on=0"),
            Ok(ApiCall::SuggestPref { on: false })
        );
        // Browser-driving ops are chrome-only; the settings page may manage site settings.
        for c in [
            p("/api/suggest?q=x").unwrap(),
            p("/api/popup?clear=1").unwrap(),
            p("/api/menu_pick?id=1").unwrap(),
            p("/api/tab_menu?id=1&action=close").unwrap(),
            p("/api/dialog_answer?id=1&ok=0").unwrap(),
            p("/api/popups?always=1").unwrap(),
            p("/api/max_button?clear=1").unwrap(),
            p("/api/suggest_delete?url=x").unwrap(),
        ] {
            assert!(c.chrome_only(), "{c:?}");
        }
        for c in [
            p("/api/site_settings").unwrap(),
            p("/api/site_clear?origin=x").unwrap(),
            p("/api/suggest_pref?on=1").unwrap(),
            p("/api/source?id=1").unwrap(),
        ] {
            assert!(!c.chrome_only(), "{c:?}");
        }
    }

    #[test]
    fn parse_compat_calls() {
        let p = |s: &str| ApiCall::parse_with(&u(&format!("{INTERNAL_ORIGIN}{s}")), SYNC_ON);
        assert_eq!(
            p("/api/compat_panel?open=1"),
            Ok(ApiCall::Compat(CompatOp::Panel { open: true }))
        );
        assert_eq!(
            p("/api/compat_rule?id=polyfill.css-escape&on=0"),
            Ok(ApiCall::Compat(CompatOp::Rule {
                id: "polyfill.css-escape".into(),
                on: false
            }))
        );
        assert_eq!(
            p("/api/compat_mode?mode=reader"),
            Ok(ApiCall::Compat(CompatOp::Mode {
                mode: "reader".into()
            }))
        );
        assert_eq!(p("/api/compat_try"), Ok(ApiCall::Compat(CompatOp::Try)));
        assert_eq!(p("/api/sync_status"), Ok(ApiCall::Sync(SyncOp::Status)));
        assert_eq!(
            p("/api/sync_signin?email=%20me%40x.my%20"),
            Ok(ApiCall::Sync(SyncOp::SignIn {
                email: "me@x.my".into()
            }))
        );
        assert_eq!(
            p("/api/sync_signup?email=nope"),
            Err(ApiError::BadParam("email"))
        );
        assert_eq!(
            p("/api/sync_bogus"),
            Err(ApiError::UnknownOp("sync_bogus".into()))
        );
        assert!(!ApiCall::Sync(SyncOp::Status).chrome_only());
        assert_eq!(
            p("/api/compat_rule?id=Bad%20Id&on=1"),
            Err(ApiError::BadParam("id"))
        );
        assert_eq!(
            p("/api/compat_mode?mode=x"),
            Err(ApiError::BadParam("mode"))
        );
        assert_eq!(p("/api/compat_panel"), Err(ApiError::MissingParam("open")));
        assert_eq!(
            p("/api/compat_nope"),
            Err(ApiError::UnknownOp("compat_nope".into()))
        );
        // Chrome only: an internal page other than the chrome may not drive compat.
        let url = u(&format!("{INTERNAL_ORIGIN}/api/compat_try?t=tok"));
        assert_eq!(route(&url, false, false, "tok"), Route::Deny);
        assert!(matches!(route(&url, false, true, "tok"), Route::Api(_)));
    }

    #[test]
    fn assist_calls_parse_and_are_chrome_only() {
        let p = |q: &str| {
            ApiCall::parse(&Url::parse(&format!("https://mythbrowse.internal/api/{q}")).unwrap())
        };
        assert_eq!(
            p("assist?open=toggle&mode=floating&x=10&y=20&w=380&h=500").unwrap(),
            ApiCall::Assist(AssistCall::Panel {
                open: Some(PanelOpen::Toggle),
                mode: Some(AssistMode::Floating),
                rect: Some(crate::frame::PopupCss {
                    x: 10.0,
                    y: 20.0,
                    w: 380.0,
                    h: 500.0,
                    r: 0.0
                }),
                dock_w: None,
            })
        );
        assert!(p("assist?mode=sideways").is_err());
        assert_eq!(
            p("assist_send?text=hi&page=0&shot=1&lang=zh-CN").unwrap(),
            ApiCall::Assist(AssistCall::Send {
                action: "ask".into(),
                text: "hi".into(),
                page: false,
                selection: true,
                shot: true,
                lang: "zh-CN".into(),
                today: None,
            })
        );
        match p("assist_send?text=hi&today=Wed%20Oct%2014%202026").unwrap() {
            ApiCall::Assist(AssistCall::Send { today, .. }) => {
                assert_eq!(today.as_deref(), Some("Wed Oct 14 2026"))
            }
            other => panic!("{other:?}"),
        }
        match p(&format!(
            "assist_sel?action=explain&today={}",
            "9".repeat(200)
        ))
        .unwrap()
        {
            ApiCall::Assist(AssistCall::Sel { today, .. }) => {
                assert_eq!(today.map(|t| t.chars().count()), Some(40))
            }
            other => panic!("{other:?}"),
        }
        assert!(p("assist_send?action=delete_everything").is_err());
        assert!(
            p(&format!(
                "assist_send?text={}",
                "a".repeat(ASSIST_TEXT_MAX + 1)
            ))
            .is_err()
        );
        assert!(p("assist_reveal?block=0").is_err());
        assert_eq!(
            p("assist_reveal?block=4").unwrap(),
            ApiCall::Assist(AssistCall::Reveal { block: 4 })
        );
        assert!(
            p("assist_sel?action=summarize").is_err(),
            "page actions are not pill actions"
        );
        assert!(p("assist_share?share=0").unwrap().chrome_only());
        assert!(p("assist_sel?action=explain").unwrap().chrome_only());
        assert!(p("assist_new").unwrap().chrome_only());
    }

    #[test]
    fn parse_api_calls() {
        let p = |s: &str| ApiCall::parse(&u(&format!("{INTERNAL_ORIGIN}{s}")));
        assert_eq!(
            p("/api/navigate?input=rust%201.92"),
            Ok(ApiCall::Navigate {
                input: "rust 1.92".into(),
                shot: false
            })
        );
        assert_eq!(
            p("/api/navigate?input=what%20is%20this&shot=1"),
            Ok(ApiCall::Navigate {
                input: "what is this".into(),
                shot: true
            })
        );
        assert_eq!(
            p("/api/activate_tab?id=7"),
            Ok(ApiCall::ActivateTab(TabId(7)))
        );
        assert_eq!(p("/api/close_tab?id=x"), Err(ApiError::BadParam("id")));
        assert_eq!(p("/api/zoom?dir=in"), Ok(ApiCall::Zoom { dir: 1 }));
        assert_eq!(p("/api/zoom?dir=reset"), Ok(ApiCall::Zoom { dir: 0 }));
        assert_eq!(p("/api/zoom?dir=up"), Err(ApiError::BadParam("dir")));
        assert_eq!(p("/api/zoom"), Err(ApiError::MissingParam("dir")));
        assert_eq!(
            p("/api/find?q=hello%20world&dir=prev"),
            Ok(ApiCall::Find {
                query: "hello world".into(),
                dir: crate::browse::FindDir::Prev
            })
        );
        assert_eq!(
            p("/api/find?q=x"),
            Ok(ApiCall::Find {
                query: "x".into(),
                dir: crate::browse::FindDir::New
            })
        );
        assert_eq!(p("/api/find?q=x&dir=up"), Err(ApiError::BadParam("dir")));
        assert_eq!(
            p(&format!("/api/find?q={}", "y".repeat(501))),
            Err(ApiError::BadParam("q"))
        );
        assert_eq!(p("/api/find_close"), Ok(ApiCall::FindClose));
        assert!(ApiCall::Zoom { dir: 0 }.chrome_only());
        assert!(ApiCall::FindClose.chrome_only());
        assert_eq!(p("/api/close_tab"), Err(ApiError::MissingParam("id")));
        assert_eq!(
            p("/api/history?q=servo&limit=100000"),
            Ok(ApiCall::History {
                query: Some("servo".into()),
                limit: MAX_LIMIT
            })
        );
        assert_eq!(
            p("/api/history?q=%20"),
            Ok(ApiCall::History {
                query: None,
                limit: 100
            })
        );
        assert_eq!(p("/api/open_tasks"), Ok(ApiCall::OpenTasks));
        assert!(ApiCall::OpenTasks.chrome_only());
        assert_eq!(
            p("/api/tasks_panel?open=toggle"),
            Ok(ApiCall::TasksPanel {
                open: Some(PanelOpen::Toggle),
                h: None
            })
        );
        assert_eq!(
            p("/api/tasks_panel?h=384.5"),
            Ok(ApiCall::TasksPanel {
                open: None,
                h: Some(385)
            })
        );
        assert_eq!(
            p("/api/tasks_panel?open=2"),
            Err(ApiError::BadParam("open"))
        );
        assert_eq!(
            p("/api/myth_size?w=343"),
            Ok(ApiCall::MythSize {
                w: Some(343),
                h: None
            })
        );
        assert_eq!(p("/api/myth_size?h=NaN"), Err(ApiError::BadParam("h")));
        assert_eq!(p("/api/myth_size?w=-4"), Err(ApiError::BadParam("w")));
        assert!(ApiCall::MythSize { w: None, h: None }.chrome_only());
        assert!(
            ApiCall::TasksPanel {
                open: None,
                h: None
            }
            .chrome_only()
        );
        assert_eq!(p("/api/nope"), Err(ApiError::UnknownOp("nope".into())));
        assert_eq!(
            p("/api/history_clear"),
            Ok(ApiCall::HistoryClear {
                scope: ClearScope::Pages
            })
        );
        assert_eq!(
            p("/api/history_clear?scope=both"),
            Ok(ApiCall::HistoryClear {
                scope: ClearScope::Both
            })
        );
        assert_eq!(
            p("/api/history_clear?scope=all"),
            Err(ApiError::BadParam("scope"))
        );
        assert_eq!(p("/api/chat_send"), Err(ApiError::MissingParam("text")));
        assert_eq!(
            p("/api/chat_stop?conv=2"),
            Ok(ApiCall::Agent(AgentCall::ChatStop { conv: 2 }))
        );
    }

    #[test]
    fn routing_and_access_control() {
        const T: &str = "secret";
        let api = |op: &str, t: &str| u(&format!("{INTERNAL_ORIGIN}/api/{op}?t={t}"));
        let css = u("https://mythbrowse.internal/home.css");
        let home = home_url(Some("hi"));

        assert_eq!(
            route(&u("https://servo.org/"), true, false, T),
            Route::PassThrough
        );
        // CSS/JS are public; HTML only as a top-level document.
        assert_eq!(
            route(&css, false, false, T),
            Route::Asset("/home.css".into())
        );
        assert_eq!(
            route(&home, true, false, T),
            Route::Asset("/home.html".into())
        );
        assert_eq!(
            route(&home, false, false, T),
            Route::Deny,
            "no iframes / fetch of HTML"
        );
        assert_eq!(route(&chrome_url(), false, true, T), Route::Deny);
        assert_eq!(
            route(&u("https://mythbrowse.internal/x.html"), true, false, T),
            Route::NotFound
        );
        // Chrome with the token may do everything.
        assert_eq!(
            route(&api("new_tab", T), false, true, T),
            Route::Api(ApiCall::NewTab)
        );
        assert!(matches!(
            route(&api("history", T), false, true, T),
            Route::Api(_)
        ));
        // Internal pages (token, not chrome): page ops only.
        assert_eq!(route(&api("new_tab", T), false, false, T), Route::Deny);
        assert!(matches!(
            route(&api("history", T), false, false, T),
            Route::Api(_)
        ));
        // Missing/wrong token, or a navigation: never.
        assert_eq!(route(&api("history", "nope"), false, false, T), Route::Deny);
        assert_eq!(
            route(
                &u("https://mythbrowse.internal/api/history"),
                false,
                true,
                T
            ),
            Route::Deny
        );
        assert_eq!(route(&api("history_clear", T), true, true, T), Route::Deny);
        assert_eq!(route(&api("history", ""), false, true, ""), Route::Deny);
        assert!(matches!(
            route(&api("zzz", T), false, true, T),
            Route::BadRequest(_)
        ));
    }

    #[test]
    fn token_is_filled_into_html_only() {
        let (mime, body) = asset("/chrome.html", None).unwrap();
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains(TOKEN_PLACEHOLDER)
        );
        let html = String::from_utf8(render_asset(mime, body.clone(), "abc123", None)).unwrap();
        assert!(html.contains("content=\"abc123\""));
        assert!(!html.contains(TOKEN_PLACEHOLDER));
        assert!(!html.contains(THEME_HOUR_PLACEHOLDER));
        assert!(html.contains("name=\"mb-theme-hour\" content=\"\""));
        let pinned = String::from_utf8(render_asset(mime, body, "abc123", Some(21.5))).unwrap();
        assert!(pinned.contains("name=\"mb-theme-hour\" content=\"21.5\""));
        let (mime, body) = asset("/chrome.js", None).unwrap();
        assert_eq!(
            render_asset(mime, body.clone(), "abc123", Some(3.0)),
            body.into_owned()
        );
    }

    #[test]
    fn tokens_are_random_hex() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(constant_time_eq(b"x", b"x") && !constant_time_eq(b"x", b"xy"));
    }

    #[test]
    fn state_snapshot() {
        let mut tabs = TabManager::new();
        let a = tabs.open((), home_url(None).as_str(), 0);
        tabs.open((), "https://servo.org/", 0);
        tabs.get_mut(a).unwrap().loading = false;
        tabs.sleep(a, None);
        let s = ChromeState::from_tabs(&tabs, true, false, true);
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].title, "New tab");
        assert!(s.tabs[0].sleeping && !s.tabs[0].active);
        assert_eq!(s.tabs[1].title, "https://servo.org/");
        assert!(s.tabs[1].active && s.tabs[1].loading);
        assert_eq!(s.url, "https://servo.org/");
        assert!(s.can_go_back && !s.can_go_forward && s.bookmarked && !s.internal);
        assert!(!s.maximized);
        // A web page floats as a card; it is tinted once its colour is known.
        assert!(s.card && s.tint.is_none());
        let b = tabs.active_id().unwrap();
        tabs.get_mut(b).unwrap().tint = Some([1, 2, 3]);
        let s = ChromeState::from_tabs(&tabs, true, false, true);
        assert_eq!(s.tint, Some([1, 2, 3]));
        assert!(s.to_json().contains("\"tint\":[1,2,3]"));
        let script = s.to_script();
        assert!(script.starts_with("window.mb && window.mb.render({\"tabs\":["));
        assert!(script.ends_with("});"));
    }

    #[test]
    fn internal_pages_are_never_cards_or_tinted() {
        let mut tabs = TabManager::new();
        let id = tabs.open((), home_url(None).as_str(), 0);
        tabs.get_mut(id).unwrap().tint = Some([9, 9, 9]);
        let s = ChromeState::from_tabs(&tabs, false, false, false);
        assert!(s.internal && !s.card && s.tint.is_none());
        assert!(!shows_as_card(""));
        assert!(!shows_as_card(library_url().as_str()));
        assert!(shows_as_card("https://example.com/"));
        assert!(shows_as_card("data:text/html,hi"));
    }

    #[test]
    fn design_assets_are_served() {
        assert_eq!(
            asset("/design/tokens.css", None).unwrap().0,
            "text/css; charset=utf-8"
        );
        assert!(asset("/design/theme.js", None).is_some());
        assert!(asset("/design/icons.js", None).is_some());
        assert!(asset("/design/sky.js", None).is_some());
        assert!(asset("/design/../chrome.html", None).is_none());
        assert_eq!(
            route(
                &u("https://mythbrowse.internal/design/tokens.css"),
                false,
                false,
                "t"
            ),
            Route::Asset("/design/tokens.css".into())
        );
        // Every internal HTML page uses the shared tokens and never an emoji/char icon font.
        for page in ["/chrome.html", "/home.html", "/library.html"] {
            let html = String::from_utf8(asset(page, None).unwrap().1.into_owned()).unwrap();
            assert!(html.contains("design/tokens.css"), "{page} links tokens");
            assert!(html.contains("design/sky.js"), "{page} draws the sky");
            assert!(
                html.contains(THEME_HOUR_PLACEHOLDER),
                "{page} has theme hour"
            );
        }
    }

    #[test]
    fn window_ops_are_chrome_only() {
        const T: &str = "secret";
        for op in [
            "window_drag",
            "window_minimize",
            "window_maximize",
            "window_close",
        ] {
            let url = u(&format!("{INTERNAL_ORIGIN}/api/{op}?t={T}"));
            assert!(matches!(route(&url, false, true, T), Route::Api(_)), "{op}");
            assert_eq!(route(&url, false, false, T), Route::Deny, "{op}");
        }
        assert_eq!(
            ApiCall::parse(&u(&format!("{INTERNAL_ORIGIN}/api/window_maximize"))),
            Ok(ApiCall::WindowToggleMaximize)
        );
    }

    #[test]
    fn myth_bar_and_prefs_ops() {
        const T: &str = "secret";
        let api = |q: &str| u(&format!("{INTERNAL_ORIGIN}/api/{q}"));
        assert_eq!(
            ApiCall::parse(&api("myth_open?conv=12")),
            Ok(ApiCall::MythOpen { conv: 12 })
        );
        assert_eq!(
            ApiCall::parse(&api("myth_open?conv=x")),
            Err(ApiError::BadParam("conv"))
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?motion=reduced")),
            Ok(ApiCall::PrefsSave {
                motion: Some("reduced".into()),
                transparency: None,
                reduce_transparency: None,
                onboarded: None,
                ua_mode: None,
                rules_auto_update: None,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?motion=wild")),
            Err(ApiError::BadParam("motion"))
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?onboarded=1")),
            Ok(ApiCall::PrefsSave {
                motion: None,
                transparency: None,
                reduce_transparency: None,
                onboarded: Some(true),
                ua_mode: None,
                rules_auto_update: None,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?ua_mode=chrome")),
            Ok(ApiCall::PrefsSave {
                motion: None,
                transparency: None,
                reduce_transparency: None,
                onboarded: None,
                ua_mode: Some("chrome".into()),
                rules_auto_update: None,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?rules_auto_update=0")),
            Ok(ApiCall::PrefsSave {
                motion: None,
                transparency: None,
                reduce_transparency: None,
                onboarded: None,
                ua_mode: None,
                rules_auto_update: Some(false),
            })
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?rules_auto_update=maybe")),
            Err(ApiError::BadParam("rules_auto_update"))
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?ua_mode=edge")),
            Err(ApiError::BadParam("ua_mode"))
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?onboarded=yes")),
            Err(ApiError::BadParam("onboarded"))
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?motion=system")),
            Ok(ApiCall::PrefsSave {
                motion: Some("system".into()),
                transparency: None,
                reduce_transparency: None,
                onboarded: None,
                ua_mode: None,
                rules_auto_update: None,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("glass?r=1,2,30,40,8,c,1&h=21.5&mv=1")),
            Ok(ApiCall::Glass {
                report: "1,2,30,40,8,c,1".into(),
                hour: Some("21.5".into()),
                moving: true,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("glass?r=&h=99")),
            Ok(ApiCall::Glass {
                report: String::new(),
                hour: None,
                moving: false,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("glass?r=1,2,3")),
            Err(ApiError::BadParam("r"))
        );
        assert!(
            !ApiCall::Glass {
                report: String::new(),
                hour: None,
                moving: false
            }
            .chrome_only(),
            "internal pages report their glass too"
        );
        let html = fill_glass_mode(
            "text/html; charset=utf-8",
            b"<meta content=\"{{MB_GLASS}}\">".to_vec(),
            crate::glass::GlassMode::Blur,
        );
        assert_eq!(html, b"<meta content=\"blur\">".to_vec());
        assert_eq!(
            ApiCall::parse(&api("chrome_tone?dark=1")),
            Ok(ApiCall::ChromeTone { dark: true })
        );
        assert_eq!(
            ApiCall::parse(&api("chrome_tone?dark=maybe")),
            Err(ApiError::BadParam("dark"))
        );
        let tone = api(&format!("chrome_tone?dark=0&t={T}"));
        assert_eq!(route(&tone, false, false, T), Route::Deny);
        // The Myth pill drives the browser: chrome only. Preferences: any internal page.
        let open = api(&format!("myth_open?conv=1&t={T}"));
        assert_eq!(route(&open, false, false, T), Route::Deny);
        assert!(matches!(route(&open, false, true, T), Route::Api(_)));
        let prefs = api(&format!("prefs?t={T}"));
        assert!(matches!(route(&prefs, false, false, T), Route::Api(_)));
        assert!(conversation_url(5).as_str().ends_with("/home.html?c=5"));
    }

    #[test]
    fn appearance_ops_are_validated() {
        const T: &str = "secret";
        let api = |q: &str| u(&format!("{INTERNAL_ORIGIN}/api/{q}"));
        assert_eq!(
            ApiCall::parse(&api(
                "prefs_save?transparency=80&reduce_transparency=system"
            )),
            Ok(ApiCall::PrefsSave {
                motion: None,
                transparency: Some(80),
                reduce_transparency: Some(None),
                onboarded: None,
                ua_mode: None,
                rules_auto_update: None,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save?reduce_transparency=on")),
            Ok(ApiCall::PrefsSave {
                motion: None,
                transparency: None,
                reduce_transparency: Some(Some(true)),
                onboarded: None,
                ua_mode: None,
                rules_auto_update: None,
            })
        );
        for bad in [
            "prefs_save?transparency=101",
            "prefs_save?transparency=-1",
            "prefs_save?transparency=50.5",
            "prefs_save?transparency=",
        ] {
            assert_eq!(
                ApiCall::parse(&api(bad)),
                Err(ApiError::BadParam("transparency")),
                "{bad}"
            );
        }
        assert_eq!(
            ApiCall::parse(&api("prefs_save?reduce_transparency=yes")),
            Err(ApiError::BadParam("reduce_transparency"))
        );
        assert_eq!(
            ApiCall::parse(&api("prefs_save")),
            Err(ApiError::MissingParam("motion"))
        );
        assert_eq!(
            ApiCall::parse(&api("glass_params?v=0&reduce=1")),
            Ok(ApiCall::GlassParams {
                transparency: Some(0),
                reduce: Some(true),
                reset: false,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("glass_params?v=100")),
            Ok(ApiCall::GlassParams {
                transparency: Some(100),
                reduce: None,
                reset: false,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("glass_params?reset=1")),
            Ok(ApiCall::GlassParams {
                transparency: None,
                reduce: None,
                reset: true,
            })
        );
        assert_eq!(
            ApiCall::parse(&api("glass_params")),
            Err(ApiError::MissingParam("v"))
        );
        for (q, p) in [
            ("glass_params?v=250", "v"),
            ("glass_params?v=abc", "v"),
            ("glass_params?v=1e2", "v"),
            ("glass_params?v=40&reduce=2", "reduce"),
            ("glass_params?reset=yes", "reset"),
        ] {
            assert_eq!(ApiCall::parse(&api(q)), Err(ApiError::BadParam(p)), "{q}");
        }
        // The settings sheet lives on the new tab (an internal page), not in the chrome; the
        // token is still required, and a navigation can never trigger it.
        let live = api(&format!("glass_params?v=30&t={T}"));
        assert!(matches!(route(&live, false, false, T), Route::Api(_)));
        assert_eq!(route(&live, true, false, T), Route::Deny);
        let no_token = api("glass_params?v=30");
        assert_eq!(route(&no_token, false, false, T), Route::Deny);
        let web = u("https://example.com/api/glass_params?v=30");
        assert_eq!(route(&web, false, false, T), Route::PassThrough);
        // The first paint gets the setting in effect.
        let html = fill_appearance(
            "text/html",
            b"<meta content=\"{{MB_TRANSPARENCY}}\"><meta content=\"{{MB_REDUCE_TRANSPARENCY}}\">"
                .to_vec(),
            72,
            true,
        );
        assert_eq!(html, b"<meta content=\"72\"><meta content=\"1\">".to_vec());
        let css = fill_appearance("text/css", b"{{MB_TRANSPARENCY}}".to_vec(), 72, true);
        assert_eq!(css, b"{{MB_TRANSPARENCY}}".to_vec());
    }

    #[test]
    fn theme_hours() {
        assert_eq!(parse_theme_hour(" 21.5 "), Some(21.5));
        assert_eq!(parse_theme_hour("0"), Some(0.0));
        assert_eq!(parse_theme_hour("24"), None);
        assert_eq!(parse_theme_hour("-1"), None);
        assert_eq!(parse_theme_hour("NaN"), None);
        assert_eq!(parse_theme_hour("noon"), None);
    }

    #[test]
    fn resize_edges() {
        let e = |x, y| resize_edge(x, y, 1000.0, 800.0, 6.0);
        assert_eq!(e(500.0, 400.0), None);
        assert_eq!(e(500.0, 0.0), Some(Edge::North));
        assert_eq!(e(500.0, 799.0), Some(Edge::South));
        assert_eq!(e(0.0, 400.0), Some(Edge::West));
        assert_eq!(e(999.0, 400.0), Some(Edge::East));
        assert_eq!(e(2.0, 2.0), Some(Edge::NorthWest));
        assert_eq!(e(10.0, 2.0), Some(Edge::NorthWest), "corner grab is wider");
        assert_eq!(e(995.0, 3.0), Some(Edge::NorthEast));
        assert_eq!(e(3.0, 795.0), Some(Edge::SouthWest));
        assert_eq!(e(999.0, 799.0), Some(Edge::SouthEast));
        assert_eq!(e(500.0, 6.0), None);
        assert_eq!(resize_edge(1.0, 1.0, 1000.0, 800.0, 0.0), None);
        assert_eq!(resize_edge(1.0, 1.0, 20.0, 20.0, 6.0), None, "tiny window");
    }

    #[test]
    fn window_titles() {
        let home = home_url(None);
        assert_eq!(window_title("MythBrowse", home.as_str()), "MythBrowse");
        assert_eq!(window_title("New tab", home.as_str()), "MythBrowse");
        assert_eq!(window_title("", ""), "MythBrowse");
        assert_eq!(
            window_title("Example Domain", "https://example.com/"),
            "Example Domain \u{2014} MythBrowse"
        );
        assert_eq!(
            window_title("", "https://example.com/"),
            "https://example.com/ \u{2014} MythBrowse"
        );
        assert_eq!(
            window_title("MythBrowse", "https://example.com/"),
            "MythBrowse"
        );
    }

    #[test]
    fn tab_labels() {
        assert_eq!(tab_label("  Servo  ", "https://servo.org/"), "Servo");
        assert_eq!(tab_label("", "https://servo.org/"), "https://servo.org/");
        assert_eq!(tab_label("", home_url(None).as_str()), "New tab");
    }

    #[test]
    fn json_bodies() {
        let parse = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap();
        assert_eq!(
            parse(json_ok(serde_json::json!({"n": 1}))),
            serde_json::json!({"ok": true, "n": 1})
        );
        assert_eq!(
            parse(json_ok(serde_json::Value::Null)),
            serde_json::json!({"ok": true})
        );
        assert_eq!(
            parse(json_error("x")),
            serde_json::json!({"ok": false, "error": "x"})
        );
    }
}
