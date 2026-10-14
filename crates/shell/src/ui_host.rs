// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Browser UI that pages and the address bar cause, on the shell side:
//!
//! - **Omnibox suggestions**: history (frecency), bookmarks, open tabs, web search
//!   suggestions (DuckDuckGo, fetched on a worker thread; off in Settings for privacy) and the
//!   Ask Myth row, ranked by `mythbrowse_engine::suggest`.
//! - **Context menus**: Servo 0.1.4's context-menu delegate (`EmbedderControl::ContextMenu`)
//!   shown as a glass menu drawn by the chrome; the shell runs the chosen command.
//! - **JS dialogs and permission prompts**: `alert` / `confirm` / `prompt`
//!   (`EmbedderControl::SimpleDialog`), "Leave site?" (`request_unload`) and
//!   `request_permission`, queued per tab (`page_ui::DialogQueue`) and shown as a sheet
//!   anchored to the page card; remembered per site (`site_permissions`). While Myth works
//!   in a page, these go to Myth's approval card instead (alerts are dismissed and logged):
//!   the AI never answers one by itself.
//! - **Pop-up blocking**: `request_create_new` without a recent user click / key press in
//!   that page is blocked; a small indicator offers "Always allow pop-ups from this site".
//! - **IME**: pre-edit text is sent to the page as composition updates (Servo shows it inline
//!   and selected), and the candidate window is put at the caret.
//! - **Chrome popups**: the rectangle the chrome reports for its open dropdown / menu / sheet,
//!   composed over the page (`frame::WindowLayout::popup`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

use log::{info, warn};
use mythbrowse_ai::ApprovalRequest;
use mythbrowse_engine::chrome;
use mythbrowse_engine::frame::{PopupCss, PxRect};
use mythbrowse_engine::omnibox;
use mythbrowse_engine::page_ui::{
    self, AgentDialogPolicy, DialogKind, DialogQueue, MenuAction, MenuContext, Pushed, TabAction,
};
use mythbrowse_engine::servo::{
    AllowOrDenyRequest, CompositionEvent, CompositionState, ContextMenu, ContextMenuAction,
    ContextMenuElementInformationFlags, ContextMenuItem, EmbedderControlId, ImeEvent, InputEvent,
    JSValue, PermissionFeature, PermissionRequest, SimpleDialog, WebView,
};
use mythbrowse_engine::suggest::{self, PageHit, TabHit};
use mythbrowse_engine::tabs::TabId;
use mythbrowse_storage::site_settings::{Decision, origin_key};
use serde_json::{Value, json};
use url::Url;
use winit::dpi::{PhysicalPosition, PhysicalSize};

use crate::{AppState, Area};

/// What answers a queued dialog (dropping it gives the page the default answer).
pub(crate) enum DialogHandle {
    Simple(SimpleDialog),
    Unload(AllowOrDenyRequest),
    Permission {
        request: PermissionRequest,
        origin: String,
        feature: &'static str,
    },
}

impl DialogHandle {
    /// Answer: OK / Allow / Leave (`ok`), else Cancel / Block / Stay. `value`: prompt text.
    fn answer(self, ok: bool, value: Option<&str>) {
        match self {
            DialogHandle::Simple(SimpleDialog::Prompt(mut p)) => {
                if ok {
                    if let Some(v) = value {
                        p.set_current_value(v);
                    }
                    p.confirm();
                } else {
                    p.dismiss();
                }
            }
            DialogHandle::Simple(d) => {
                if ok {
                    d.confirm()
                } else {
                    d.dismiss()
                }
            }
            DialogHandle::Unload(r) => {
                if ok {
                    r.allow()
                } else {
                    r.deny()
                }
            }
            DialogHandle::Permission { request, .. } => {
                if ok {
                    request.allow()
                } else {
                    request.deny()
                }
            }
        }
    }
}

/// A context menu on screen.
struct OpenMenu {
    id: u64,
    view: WebView,
    servo: Option<ContextMenu>,
    ctx: MenuContext,
    json: Value,
}

/// A page dialog waiting on Myth's approval card.
struct AgentWait {
    answer: Receiver<bool>,
    handle: DialogHandle,
    what: String,
}

/// A finished web-suggestion fetch.
struct Remote {
    q: String,
    seq: u64,
    phrases: Vec<String>,
}

pub(crate) struct UiHost {
    pub(crate) dialogs: RefCell<DialogQueue<DialogHandle>>,
    menu: RefCell<Option<OpenMenu>>,
    next_menu: Cell<u64>,
    /// The chrome's open popup (window CSS px), composed over the page.
    pub(crate) popup: Cell<Option<PopupCss>>,
    /// Pop-ups blocked per tab (the indicator shows the active tab's count).
    popups_blocked: RefCell<HashMap<TabId, u32>>,
    /// The user's last click / key press per tab (shell ms; pop-up activation).
    activation: RefCell<HashMap<TabId, u64>>,
    agent_waits: RefCell<Vec<AgentWait>>,
    remote_tx: Sender<Remote>,
    remote_rx: Receiver<Remote>,
    wake: crate::agent_host::Wake,
    /// Newest suggest query (and its seq) and the cached web phrases for one query.
    last_query: RefCell<(String, u64)>,
    remote_cache: RefCell<Option<(String, Vec<String>)>>,
    pub(crate) suggest_web: Cell<bool>,
    /// "View page source" captures, by id.
    sources: RefCell<HashMap<u64, (String, String)>>,
    next_source: Cell<u64>,
    /// IME: a composition is showing in the focused field, and a pre-edit that went empty
    /// (cancel, or the commit that follows it in the same batch).
    preedit: Cell<bool>,
    preedit_cancel: Cell<bool>,
}

impl UiHost {
    pub(crate) fn new(wake: crate::agent_host::Wake, suggest_web: bool) -> Self {
        let (remote_tx, remote_rx) = channel();
        Self {
            dialogs: RefCell::new(DialogQueue::new()),
            menu: RefCell::new(None),
            next_menu: Cell::new(1),
            popup: Cell::new(None),
            popups_blocked: RefCell::new(HashMap::new()),
            activation: RefCell::new(HashMap::new()),
            agent_waits: RefCell::new(Vec::new()),
            remote_tx,
            remote_rx,
            wake,
            last_query: RefCell::new((String::new(), 0)),
            remote_cache: RefCell::new(None),
            suggest_web: Cell::new(suggest_web),
            sources: RefCell::new(HashMap::new()),
            next_source: Cell::new(1),
            preedit: Cell::new(false),
            preedit_cancel: Cell::new(false),
        }
    }
}

/// Servo's permission features as site-settings names.
fn feature_name(f: PermissionFeature) -> &'static str {
    match f {
        PermissionFeature::Geolocation => "geolocation",
        PermissionFeature::Notifications => "notifications",
        PermissionFeature::Push => "push",
        PermissionFeature::Midi => "midi",
        PermissionFeature::Camera => "camera",
        PermissionFeature::Microphone => "microphone",
        PermissionFeature::Speaker => "speaker",
        PermissionFeature::DeviceInfo => "device-info",
        PermissionFeature::BackgroundSync => "background-sync",
        PermissionFeature::Bluetooth => "bluetooth",
        PermissionFeature::PersistentStorage => "persistent-storage",
    }
}

fn js_string<E>(v: Result<JSValue, E>) -> String {
    match v {
        Ok(JSValue::String(s)) => s,
        _ => String::new(),
    }
}

/// The web-suggestion fetch (worker thread): at most 64 KB, 2.5 s.
fn fetch_remote(url: &Url) -> Vec<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_millis(2500)))
        .user_agent("MythBrowse")
        .build()
        .into();
    let q = url
        .query_pairs()
        .find(|(k, _)| k == "q")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    match agent.get(url.as_str()).call() {
        Ok(mut r) if r.status().is_success() => {
            match r.body_mut().with_config().limit(64 * 1024).read_to_string() {
                Ok(body) => suggest::parse_remote(&body, &q),
                Err(_) => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

impl AppState {
    // ------------------------------------------------------------------ pump & state

    /// Worker results and approval-card answers (called from the event loop).
    pub(crate) fn ui_pump(&self) {
        while let Ok(r) = self.ui.remote_rx.try_recv() {
            let current = self.ui.last_query.borrow().clone();
            *self.ui.remote_cache.borrow_mut() = Some((r.q.clone(), r.phrases));
            if r.q == current.0 && r.seq == current.1 {
                let ranked = self.suggest_rank(&r.q);
                self.push_suggest(r.seq, &ranked);
            }
        }
        let mut waits = self.ui.agent_waits.borrow_mut();
        let mut i = 0;
        while i < waits.len() {
            match waits[i].answer.try_recv() {
                Ok(allow) => {
                    let w = waits.remove(i);
                    info!(
                        "page dialog during a Myth run: {} -> {}",
                        w.what,
                        if allow { "allowed" } else { "declined" }
                    );
                    w.handle.answer(allow, None);
                }
                Err(TryRecvError::Disconnected) => {
                    let w = waits.remove(i);
                    info!(
                        "page dialog during a Myth run: {} -> declined (run ended)",
                        w.what
                    );
                    w.handle.answer(false, None);
                }
                Err(TryRecvError::Empty) => i += 1,
            }
        }
        drop(waits);
        // A pre-edit that went empty without a commit in the same batch: remove its text.
        if self.ui.preedit_cancel.replace(false) && self.ui.preedit.replace(false) {
            self.ime_clear_preedit();
        }
    }

    /// The parts of the chrome state this module owns.
    pub(crate) fn ui_state(&self, state: &mut chrome::ChromeState) {
        let active = self.tabs.borrow().active_id();
        state.dialog = active.and_then(|tab| {
            let q = self.ui.dialogs.borrow();
            let d = q.front(tab)?;
            let label = match &d.kind {
                DialogKind::Permission { feature } => page_ui::feature_label(feature),
                _ => "",
            };
            Some(json!({
                "id": d.id, "kind": d.kind, "message": d.message, "origin": d.origin,
                "suppress": q.offers_suppress(tab), "label": label,
            }))
        });
        state.menu = self.ui.menu.borrow().as_ref().map(|m| m.json.clone());
        state.popups_blocked = active.and_then(|tab| {
            let n = *self.ui.popups_blocked.borrow().get(&tab)?;
            let origin = self
                .tabs
                .borrow()
                .get(tab)
                .and_then(|t| origin_key(&t.url))?;
            Some(json!({ "count": n, "origin": origin }))
        });
        state.suggest_web = self.ui.suggest_web.get();
    }

    /// Whether the active tab is behind a dialog (pointer and keys don't reach the page).
    pub(crate) fn ui_page_blocked(&self) -> bool {
        self.tabs
            .borrow()
            .active_id()
            .is_some_and(|t| self.ui.dialogs.borrow().has(t))
    }

    /// A user click / key press in tab `tab` (pop-ups may follow it).
    pub(crate) fn ui_activation(&self, tab: Option<TabId>) {
        if let Some(t) = tab {
            self.ui.activation.borrow_mut().insert(t, self.now_ms());
        }
    }

    /// When the user last clicked / typed in tab `tab` (ms), if ever.
    pub(crate) fn ui_last_activation(&self, tab: TabId) -> Option<u64> {
        self.ui.activation.borrow().get(&tab).copied()
    }

    /// Use up tab `tab`'s user activation (one click opens one thing).
    pub(crate) fn ui_consume_activation(&self, tab: TabId) {
        self.ui.activation.borrow_mut().remove(&tab);
    }

    /// The chrome reported (or closed) its popup.
    pub(crate) fn ui_set_popup(&self, rect: Option<PopupCss>) {
        if self.ui.popup.replace(rect) != rect {
            self.layout(false);
            self.redraw();
        }
    }

    /// A tab loaded a new page.
    pub(crate) fn ui_tab_navigated(&self, tab: TabId) {
        self.ui.dialogs.borrow_mut().navigated(tab);
        self.ui.popups_blocked.borrow_mut().remove(&tab);
    }

    pub(crate) fn ui_tab_closed(&self, tab: TabId) {
        let gone = self.ui.dialogs.borrow_mut().close_tab(tab);
        for d in gone {
            d.handle.answer(false, None);
        }
        self.ui.popups_blocked.borrow_mut().remove(&tab);
        self.ui.activation.borrow_mut().remove(&tab);
        let menu_here = self
            .ui
            .menu
            .borrow()
            .as_ref()
            .is_some_and(|m| self.tab_id_of(&m.view) == Some(tab));
        if menu_here {
            self.ui_close_menu();
        }
    }

    // ------------------------------------------------------------------ suggestions

    fn suggest_rank(&self, q: &str) -> suggest::Ranked {
        let history: Vec<PageHit> = self
            .store
            .search_history(q, 60)
            .unwrap_or_default()
            .into_iter()
            .map(|h| PageHit {
                url: h.url,
                title: h.title,
                visit_count: h.visit_count,
                last_visit_ms: h.last_visit_ms,
            })
            .collect();
        let bookmarks: Vec<(String, String)> = self
            .store
            .bookmarks()
            .unwrap_or_default()
            .into_iter()
            .map(|b| (b.url, b.title))
            .collect();
        let tabs: Vec<TabHit> = {
            let t = self.tabs.borrow();
            let active = t.active_id();
            t.iter()
                .map(|x| TabHit {
                    id: x.id.0,
                    url: x.url.clone(),
                    title: x.title.clone(),
                    active: Some(x.id) == active,
                })
                .collect()
        };
        let remote = match &*self.ui.remote_cache.borrow() {
            Some((cq, phrases)) if cq == q && self.ui.suggest_web.get() => phrases.clone(),
            _ => Vec::new(),
        };
        suggest::rank(
            q,
            &suggest::Sources {
                history: &history,
                bookmarks: &bookmarks,
                tabs: &tabs,
                remote: &remote,
            },
            mythbrowse_storage::unix_now_ms(),
        )
    }

    fn push_suggest(&self, seq: u64, ranked: &suggest::Ranked) {
        if let Some(chrome) = self.chrome_view() {
            let body = json!({ "seq": seq, "q": ranked.query, "items": ranked.items,
                "inline": ranked.inline, "web": self.ui.suggest_web.get() });
            chrome.evaluate_javascript(
                format!("window.mb && window.mb.suggest && window.mb.suggest({body});"),
                |_| {},
            );
        }
    }

    /// `/api/suggest`: ranked rows now; starts the web fetch when it is on and needed.
    pub(crate) fn ui_suggest(&self, q: &str, seq: u64) -> Value {
        *self.ui.last_query.borrow_mut() = (q.to_owned(), seq);
        let ranked = self.suggest_rank(q);
        let cached = self
            .ui
            .remote_cache
            .borrow()
            .as_ref()
            .is_some_and(|(cq, _)| cq == q);
        let pending = if self.ui.suggest_web.get() && !cached {
            match suggest::remote_url(q) {
                Some(url) => {
                    let tx = self.ui.remote_tx.clone();
                    let wake = self.ui.wake.clone();
                    let q = q.to_owned();
                    std::thread::Builder::new()
                        .name("omnibox-suggest".into())
                        .spawn(move || {
                            let phrases = fetch_remote(&url);
                            if tx.send(Remote { q, seq, phrases }).is_ok() {
                                wake();
                            }
                        })
                        .is_ok()
                }
                None => false,
            }
        } else {
            false
        };
        json!({ "seq": seq, "q": ranked.query, "items": ranked.items, "inline": ranked.inline,
            "web": self.ui.suggest_web.get(), "pending": pending })
    }

    pub(crate) fn ui_suggest_delete(&self, url: &str) -> bool {
        match self.store.delete_history_url(url) {
            Ok(done) => done,
            Err(e) => {
                warn!("history delete failed: {e}");
                false
            }
        }
    }

    pub(crate) fn ui_set_suggest_web(&self, on: bool) -> Result<(), String> {
        self.store
            .set_search_suggestions(on)
            .map_err(|e| e.to_string())?;
        self.ui.suggest_web.set(on);
        if !on {
            *self.ui.remote_cache.borrow_mut() = None;
        }
        self.mark_chrome_dirty();
        Ok(())
    }

    // ------------------------------------------------------------------ context menus

    /// Servo asks for a context menu on `view`.
    pub(crate) fn ui_context_menu(&self, view: WebView, menu: ContextMenu) {
        let in_chrome = self.is_chrome(&view);
        if self.agent.owns(&view) {
            menu.dismiss();
            return;
        }
        let info = menu.element_info().clone();
        let mut ctx = MenuContext {
            link: info.link_url.as_ref().map(|u| u.to_string()),
            image: info.image_url.as_ref().map(|u| u.to_string()),
            selection: None,
            editable: info
                .flags
                .contains(ContextMenuElementInformationFlags::EditableText),
            can_go_back: view.can_go_back(),
            can_go_forward: view.can_go_forward(),
            in_chrome,
        };
        // The chrome has its own menus (tab strip); only its text fields get this one.
        if in_chrome && !ctx.editable {
            menu.dismiss();
            return;
        }
        // Form fields report their selection; page text is selected by the page-input
        // script (Servo 0.1.4 has no native selection there), so ask the page.
        let has_selection = info
            .flags
            .contains(ContextMenuElementInformationFlags::Selection)
            || (!in_chrome && !ctx.editable);
        if !has_selection {
            self.ui_open_menu(view, menu, ctx);
            return;
        }
        // Read the selected text (for "Search the web for ..." / Ask Myth).
        let weak = self.weak_self.clone();
        let v2 = view.clone();
        view.evaluate_javascript(
            "String(window.getSelection ? getSelection() : '').slice(0, 2000)",
            move |r| {
                let Some(s) = weak.upgrade() else { return };
                let text = js_string(r);
                // Only a real selection enables Copy (and "Search for ...").
                ctx.selection = (ctx.editable || !text.trim().is_empty()).then_some(text);
                s.ui_open_menu(v2, menu, ctx);
            },
        );
    }

    fn ui_open_menu(&self, view: WebView, menu: ContextMenu, ctx: MenuContext) {
        self.ui_close_menu();
        let items = page_ui::page_menu(&ctx);
        if items.is_empty() {
            menu.dismiss();
            return;
        }
        let id = self.ui.next_menu.get();
        self.ui.next_menu.set(id + 1);
        let scale = self.window.scale_factor().max(0.1);
        let p = self.cursor.get();
        let json = json!({
            "id": id,
            "x": (p.x as f64 / scale).round(),
            "y": (p.y as f64 / scale).round(),
            "items": items,
        });
        *self.ui.menu.borrow_mut() = Some(OpenMenu {
            id,
            view,
            servo: Some(menu),
            ctx,
            json,
        });
        // Keys (arrows, Enter, Esc) go to the chrome while the menu is open.
        self.set_focus(Area::Chrome);
        self.mark_chrome_dirty();
    }

    /// Close the menu (if any) without running a command.
    pub(crate) fn ui_close_menu(&self) {
        let open = self.ui.menu.borrow_mut().take();
        if let Some(m) = open {
            if let Some(menu) = m.servo {
                menu.dismiss();
            }
            self.mark_chrome_dirty();
        }
    }

    pub(crate) fn ui_menu_open(&self) -> bool {
        self.ui.menu.borrow().is_some()
    }

    /// Servo withdrew control `id` (the page navigated, the element went away...).
    pub(crate) fn ui_hide_control(&self, id: EmbedderControlId) {
        let ours = self
            .ui
            .menu
            .borrow()
            .as_ref()
            .and_then(|m| m.servo.as_ref())
            .is_some_and(|m| m.id() == id);
        if ours {
            self.ui_close_menu();
        }
    }

    /// `/api/menu_pick`: run the chosen command of menu `id` (or dismiss it).
    pub(crate) fn ui_menu_pick(&self, id: u64, action: Option<MenuAction>) -> bool {
        let open = {
            let mut m = self.ui.menu.borrow_mut();
            if m.as_ref().is_none_or(|m| m.id != id) {
                return false;
            }
            m.take()
        };
        let Some(mut open) = open else { return false };
        self.mark_chrome_dirty();
        self.set_focus(if open.ctx.in_chrome {
            Area::Chrome
        } else {
            Area::Content
        });
        let Some(action) = action else {
            if let Some(menu) = open.servo.take() {
                menu.dismiss();
            }
            return true;
        };
        let view = open.view.clone();
        // Commands Servo runs itself (clipboard, editing): pick its own item when offered.
        let servo_action = match action {
            MenuAction::CopyLink => Some(ContextMenuAction::CopyLink),
            MenuAction::CopyImageUrl => Some(ContextMenuAction::CopyImageLink),
            MenuAction::Copy => Some(ContextMenuAction::Copy),
            MenuAction::Cut => Some(ContextMenuAction::Cut),
            MenuAction::Paste => Some(ContextMenuAction::Paste),
            MenuAction::SelectAll => Some(ContextMenuAction::SelectAll),
            _ => None,
        };
        if let Some(sa) = servo_action
            && let Some(menu) = open.servo.take()
        {
            let offered = menu.items().iter().any(|i| {
                matches!(i, ContextMenuItem::Item { action, enabled: true, .. } if *action == sa)
            });
            if offered {
                menu.select(sa);
            } else {
                menu.dismiss();
                // Fall back to the editing action on the focused element.
                use mythbrowse_engine::servo::EditingActionEvent as E;
                let ev = match sa {
                    ContextMenuAction::Copy => Some(E::Copy),
                    ContextMenuAction::Cut => Some(E::Cut),
                    ContextMenuAction::Paste => Some(E::Paste),
                    _ => None,
                };
                match ev {
                    Some(ev) => {
                        view.notify_input_event(InputEvent::EditingAction(ev));
                    }
                    // Page text (Servo selects only in form fields): the page-input script.
                    None if sa == ContextMenuAction::SelectAll && !open.ctx.in_chrome => {
                        view.evaluate_javascript(
                            "window.__mbSelection && window.__mbSelection.selectAll()",
                            |_| {},
                        );
                    }
                    None => warn!("context menu: {} not available here", action.id()),
                }
            }
            return true;
        }
        if let Some(menu) = open.servo.take() {
            menu.dismiss();
        }
        let ctx = &open.ctx;
        let sel = ctx.selection.clone().unwrap_or_default();
        match action {
            MenuAction::Back if view.can_go_back() => {
                view.go_back(1);
            }
            MenuAction::Forward if view.can_go_forward() => {
                view.go_forward(1);
            }
            MenuAction::Reload => view.reload(),
            MenuAction::ViewSource => self.ui_view_source(&view),
            MenuAction::OpenLinkNewTab => {
                if let Some(u) = ctx.link.as_deref().and_then(|l| Url::parse(l).ok()) {
                    self.open_tab(u);
                }
            }
            MenuAction::OpenImageNewTab => {
                if let Some(u) = ctx.image.as_deref().and_then(|l| Url::parse(l).ok()) {
                    self.open_tab(u);
                }
            }
            MenuAction::AskMythLink => {
                if let Some(l) = &ctx.link {
                    self.navigate(&format!("Tell me about this link: {l}"), false);
                }
            }
            MenuAction::SearchSelection if !sel.trim().is_empty() => {
                self.open_tab(omnibox::search_url(sel.trim()));
            }
            MenuAction::AskMythSelection if !sel.trim().is_empty() => {
                self.navigate(
                    &format!(
                        "Explain this from the page: \u{201c}{}\u{201d}",
                        page_ui::label_snippet(&sel, 1500)
                    ),
                    false,
                );
            }
            _ => {}
        }
        true
    }

    /// "View page source": the document as it is now (Servo 0.1.4 has no view-source:),
    /// shown on an internal page.
    fn ui_view_source(&self, view: &WebView) {
        let url = view.url().map(|u| u.to_string()).unwrap_or_default();
        let weak = self.weak_self.clone();
        view.evaluate_javascript(
            "(document.doctype ? '<!DOCTYPE ' + document.doctype.name + '>\\n' : '') + document.documentElement.outerHTML",
            move |r| {
                let Some(s) = weak.upgrade() else { return };
                let src: String = js_string(r).chars().take(4_000_000).collect();
                let id = s.ui.next_source.get();
                s.ui.next_source.set(id + 1);
                {
                    let mut sources = s.ui.sources.borrow_mut();
                    // Keep only a few captures.
                    if sources.len() >= 8
                        && let Some(old) = sources.keys().min().copied()
                    {
                        sources.remove(&old);
                    }
                    sources.insert(id, (url.clone(), src));
                }
                if let Ok(page) =
                    Url::parse(&format!("{}/source.html#{id}", chrome::INTERNAL_ORIGIN))
                {
                    s.open_tab(page);
                }
            },
        );
    }

    pub(crate) fn ui_source(&self, id: u64) -> Option<Value> {
        self.ui
            .sources
            .borrow()
            .get(&id)
            .map(|(url, src)| json!({ "url": url, "source": src }))
    }

    /// Tab strip menu commands.
    pub(crate) fn ui_tab_menu(&self, id: TabId, action: TabAction) -> bool {
        if self.tabs.borrow().get(id).is_none() {
            return false;
        }
        match action {
            TabAction::NewTab => self.open_tab(chrome::home_url(None)),
            TabAction::Reload => {
                self.activate_tab(id);
                if let Some(v) = self.active_view() {
                    v.reload();
                }
            }
            TabAction::Duplicate => {
                let url = self.tabs.borrow().get(id).map(|t| t.url.clone());
                if let Some(u) = url.and_then(|u| Url::parse(&u).ok()) {
                    self.open_tab(u);
                }
            }
            TabAction::Close | TabAction::CloseOthers | TabAction::CloseRight => {
                let order: Vec<TabId> = self.tabs.borrow().iter().map(|t| t.id).collect();
                for t in page_ui::tabs_to_close(&order, id, action) {
                    self.close_tab(t);
                }
            }
        }
        true
    }

    // ------------------------------------------------------------------ dialogs

    fn origin_of(view: &WebView) -> String {
        view.url()
            .and_then(|u| origin_key(u.as_str()))
            .unwrap_or_else(|| "This page".to_owned())
    }

    /// Myth's live run in this page, if any: then the dialog goes to its approval card.
    fn route_to_agent(
        &self,
        view: &WebView,
        kind: &DialogKind,
        message: &str,
        handle: DialogHandle,
    ) -> Result<(), DialogHandle> {
        let Some(conv) = self.agent_conv_of_page(view) else {
            return Err(handle);
        };
        let origin = Self::origin_of(view);
        match page_ui::agent_policy(kind) {
            AgentDialogPolicy::DismissAndLog => {
                let text = page_ui::label_snippet(message, 200);
                info!("alert() in a page Myth works in, dismissed: {origin}: {text}");
                self.agent_note(
                    conv,
                    &format!("The page showed an alert (dismissed): \u{201c}{text}\u{201d}"),
                );
                handle.answer(true, None);
                Ok(())
            }
            AgentDialogPolicy::AskUser => {
                let (action, reason) = page_ui::agent_card_text(kind, &origin, message);
                let request = ApprovalRequest {
                    tool: "page_dialog".into(),
                    tab: None,
                    action: action.clone(),
                    reason,
                    url: view.url().map(|u| u.to_string()).unwrap_or_default(),
                    destination: None,
                };
                match self.agent_page_approval(conv, request) {
                    Some(answer) => {
                        info!("page dialog during a Myth run -> approval card: {action}");
                        self.ui.agent_waits.borrow_mut().push(AgentWait {
                            answer,
                            handle,
                            what: action,
                        });
                    }
                    None => {
                        // Another approval is already waiting: never answered silently with
                        // "yes": decline this one and say so.
                        warn!(
                            "page dialog during a Myth run declined (another approval is pending): {action}"
                        );
                        handle.answer(false, None);
                    }
                }
                Ok(())
            }
        }
    }

    fn queue_dialog(&self, view: &WebView, kind: DialogKind, message: &str, handle: DialogHandle) {
        let Some(tab) = self.tab_id_of(view) else {
            handle.answer(false, None);
            return;
        };
        let origin = Self::origin_of(view);
        let pushed = self
            .ui
            .dialogs
            .borrow_mut()
            .push(tab, &origin, kind, message, handle);
        match pushed {
            Pushed::Queued(_) => {
                if self.tabs.borrow().active_id() == Some(tab) {
                    self.ui_close_menu();
                    self.set_focus(Area::Chrome);
                }
                self.mark_chrome_dirty();
            }
            Pushed::Suppressed(h) => {
                info!("dialog from {origin} suppressed (the user silenced this page)");
                h.answer(false, None);
            }
        }
    }

    /// `alert` / `confirm` / `prompt`.
    pub(crate) fn ui_simple_dialog(&self, view: WebView, dialog: SimpleDialog) {
        if self.is_chrome(&view) {
            dialog.dismiss();
            return;
        }
        let (kind, message) = match &dialog {
            SimpleDialog::Alert(a) => (DialogKind::Alert, a.message().to_owned()),
            SimpleDialog::Confirm(c) => (DialogKind::Confirm, c.message().to_owned()),
            SimpleDialog::Prompt(p) => (
                DialogKind::Prompt {
                    default: p.current_value().to_owned(),
                },
                p.message().to_owned(),
            ),
        };
        let handle = DialogHandle::Simple(dialog);
        if let Err(handle) = self.route_to_agent(&view, &kind, &message, handle) {
            if self.agent.owns(&view) {
                handle.answer(false, None);
                return;
            }
            self.queue_dialog(&view, kind, &message, handle);
        }
    }

    /// "Leave site?" (beforeunload).
    pub(crate) fn ui_unload(&self, view: WebView, request: AllowOrDenyRequest) {
        let kind = DialogKind::BeforeUnload;
        let msg = "Changes you made may not be saved.";
        if let Err(handle) = self.route_to_agent(&view, &kind, msg, DialogHandle::Unload(request)) {
            if self.agent.owns(&view) || self.is_chrome(&view) {
                handle.answer(true, None);
                return;
            }
            self.queue_dialog(&view, kind, msg, handle);
        }
    }

    /// A permission prompt: remembered decision, Myth's approval card, or the sheet.
    pub(crate) fn ui_permission(&self, view: WebView, request: PermissionRequest) {
        let feature = feature_name(request.feature());
        let origin = view.url().and_then(|u| origin_key(u.as_str()));
        let Some(origin) = origin.filter(|_| !self.is_chrome(&view)) else {
            request.deny();
            return;
        };
        match self.store.site_permission(&origin, feature) {
            Ok(Some(Decision::Allow)) => {
                info!("permission {feature} for {origin}: allowed (site setting)");
                request.allow();
                return;
            }
            Ok(Some(Decision::Block)) => {
                info!("permission {feature} for {origin}: blocked (site setting)");
                request.deny();
                return;
            }
            Ok(None) => {}
            Err(e) => warn!("site settings unavailable: {e}"),
        }
        let kind = DialogKind::Permission {
            feature: feature.to_owned(),
        };
        let handle = DialogHandle::Permission {
            request,
            origin,
            feature,
        };
        if let Err(handle) = self.route_to_agent(&view, &kind, "", handle) {
            if self.agent.owns(&view) {
                handle.answer(false, None);
                return;
            }
            self.queue_dialog(&view, kind, "", handle);
        }
    }

    /// `/api/dialog_answer`.
    pub(crate) fn ui_dialog_answer(
        &self,
        id: u64,
        ok: bool,
        value: Option<&str>,
        suppress: bool,
        remember: bool,
    ) -> bool {
        let Some(d) = self.ui.dialogs.borrow_mut().take(id) else {
            return false;
        };
        if remember
            && let DialogHandle::Permission {
                origin, feature, ..
            } = &d.handle
        {
            let decision = if ok { Decision::Allow } else { Decision::Block };
            if let Err(e) = self.store.set_site_permission(
                origin,
                feature,
                Some(decision),
                mythbrowse_storage::unix_now_ms(),
            ) {
                warn!("could not remember the permission: {e}");
            }
        }
        let tab = d.tab;
        d.handle.answer(ok, value);
        if suppress {
            let rest = self.ui.dialogs.borrow_mut().suppress(tab);
            for r in rest {
                r.handle.answer(false, None);
            }
        }
        if !self.ui.dialogs.borrow().has(tab) {
            self.set_focus(Area::Content);
        }
        self.mark_chrome_dirty();
        true
    }

    // ------------------------------------------------------------------ pop-ups

    /// May `parent` open a new window now? Counts a blocked one for the indicator.
    pub(crate) fn ui_popup_allowed(&self, parent: &WebView) -> bool {
        let Some(tab) = self.tab_id_of(parent) else {
            return false;
        };
        let site = parent
            .url()
            .and_then(|u| origin_key(u.as_str()))
            .and_then(|o| self.store.site_permission(&o, "popups").ok().flatten())
            .map(|d| d == Decision::Allow);
        let last = self.ui.activation.borrow().get(&tab).copied();
        let allowed = page_ui::popup_allowed(last, self.now_ms(), site);
        if allowed {
            // One activation opens one window.
            self.ui.activation.borrow_mut().remove(&tab);
        } else {
            *self.ui.popups_blocked.borrow_mut().entry(tab).or_insert(0) += 1;
            info!("pop-up blocked in tab {tab:?}");
            self.mark_chrome_dirty();
        }
        allowed
    }

    /// `/api/popups`: hide the indicator; `always` also allows the site from now on.
    pub(crate) fn ui_popups(&self, always: bool) -> bool {
        let Some(tab) = self.tabs.borrow().active_id() else {
            return false;
        };
        self.ui.popups_blocked.borrow_mut().remove(&tab);
        if always {
            let origin = self.tabs.borrow().get(tab).and_then(|t| origin_key(&t.url));
            if let Some(o) = origin
                && let Err(e) = self.store.set_site_permission(
                    &o,
                    "popups",
                    Some(Decision::Allow),
                    mythbrowse_storage::unix_now_ms(),
                )
            {
                warn!("could not save the pop-up setting: {e}");
            }
        }
        self.mark_chrome_dirty();
        true
    }

    // ------------------------------------------------------------------ IME

    /// winit `Ime::Preedit`: show the composition inline (Servo inserts it selected, so the
    /// next update replaces it), and put the candidate window at the caret.
    pub(crate) fn ime_preedit(&self, text: String) {
        let Some(view) = self.focused_view() else {
            return;
        };
        if text.is_empty() {
            if self.ui.preedit.get() {
                // Cancelled, or about to be committed (winit sends an empty pre-edit before
                // the commit): decide after this batch of events (`ui_pump`).
                self.ui.preedit_cancel.set(true);
            }
            return;
        }
        self.ui.preedit_cancel.set(false);
        if !self.ui.preedit.replace(true) {
            view.notify_input_event(InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
                state: CompositionState::Start,
                data: String::new(),
            })));
            self.ime_place_at_caret(&view);
        }
        view.notify_input_event(InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
            state: CompositionState::Update,
            data: text,
        })));
    }

    /// winit `Ime::Commit`: the composition's final text replaces the pre-edit.
    pub(crate) fn ime_commit(&self, text: String) {
        self.ui.preedit_cancel.set(false);
        self.ui.preedit.set(false);
        if let Some(view) = self.focused_view() {
            view.notify_input_event(InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
                state: CompositionState::End,
                data: text,
            })));
            self.ime_place_at_caret(&view);
        }
    }

    /// The IME was turned off with a composition showing: drop its text.
    pub(crate) fn ime_disabled(&self) {
        if self.ui.preedit.replace(false) {
            self.ime_clear_preedit();
        }
        self.ui.preedit_cancel.set(false);
    }

    /// Remove the (selected) pre-edit text: Servo 0.1.4 ignores an empty composition update,
    /// so delete the selection and end the composition empty.
    fn ime_clear_preedit(&self) {
        use mythbrowse_engine::servo::{
            Code, Key, KeyState, KeyboardEvent, Location, Modifiers, NamedKey,
        };
        let Some(view) = self.focused_view() else {
            return;
        };
        for state in [KeyState::Down, KeyState::Up] {
            view.notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
                state,
                Key::Named(NamedKey::Backspace),
                Code::Backspace,
                Location::Standard,
                Modifiers::empty(),
                false,
                false,
            )));
        }
        view.notify_input_event(InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
            state: CompositionState::End,
            data: String::new(),
        })));
    }

    /// Measure the caret of `view`'s focused field and move the candidate window there.
    pub(crate) fn ime_place_at_caret(&self, view: &WebView) {
        let weak = self.weak_self.clone();
        let v2 = view.clone();
        view.evaluate_javascript(page_ui::CARET_RECT_SCRIPT, move |r| {
            let Some(s) = weak.upgrade() else { return };
            let Some((x, y, w, h)) = page_ui::parse_caret_rect(&js_string(r)) else {
                return;
            };
            let scale = s.window.scale_factor() as f32;
            let local = PxRect::new(
                (x * scale).round() as i32,
                (y * scale).round() as i32,
                (w * scale).round().max(1.0) as i32,
                (h * scale).round().max(1.0) as i32,
            );
            let at = if s.is_chrome(&v2) {
                local
            } else if s.active_view().as_ref() == Some(&v2) {
                s.layout.get().content_to_window(local)
            } else {
                return;
            };
            s.window.set_ime_cursor_area(
                PhysicalPosition::new(at.x, at.y),
                PhysicalSize::new(at.w.max(1) as u32, at.h.max(1) as u32),
            );
        });
    }
}
