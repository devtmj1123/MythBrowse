// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Everyday browsing behaviours (v0.4.2) on the shell side; the pure halves are in
//! `mythbrowse_engine::browse`.
//!
//! * **Pop-ups that close themselves** (OAuth sign-in with Google / GitHub / Microsoft):
//!   Servo 0.1.4 reports `window.close()` through `WebViewDelegate::notify_closed` after its
//!   own "script-closable" check (opened by script, or a one-entry history). The tab is
//!   closed and the tab that opened it (remembered when `request_create_new` made it) comes
//!   back to the front. Openers are kept awake while their pop-up is open, so
//!   `window.opener.postMessage(...)` from the pop-up still reaches them.
//! * **Page zoom**: Ctrl + / - / 0, Ctrl+wheel, the address-bar indicator (click = 100 %).
//!   `WebView::set_page_zoom`; remembered per site (pref `zoom_sites`) and applied when a tab
//!   lands on that site.
//! * **External links**: `mailto:` / `tel:` / `sms:` go to the OS (`mythbrowse_files::os::
//!   open_url`) only right after the user's own click in that page; other app schemes are
//!   refused with a short note in the address bar.
//! * **Find in page** (Ctrl+F, F3 / Shift+F3, Enter / Shift+Enter in the bar, Esc): the page
//!   script's `__mbFind` does the searching and painting; the chrome's find bar shows
//!   "current of total".
//! * **Fullscreen API**: `notify_fullscreen_state_changed` makes the window borderless
//!   fullscreen with the page filling it (no chrome); Esc, switching or closing the tab
//!   leaves.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use log::{info, warn};
use mythbrowse_engine::browse::{
    self, FindCount, FindDir, PREF_ZOOM_SITES, SchemeAction, ZoomSites,
};
use mythbrowse_engine::servo::{JSValue, WebView};
use mythbrowse_engine::smooth_scroll::WheelInput;
use mythbrowse_engine::tabs::TabId;
use serde_json::json;
use url::Url;
use winit::window::Fullscreen;

use crate::{AppState, Area};

/// The find bar of one tab.
struct FindBar {
    tab: TabId,
    query: String,
    count: Option<FindCount>,
    /// Bumped on every Ctrl+F so the chrome focuses the field again.
    focus: u64,
    /// Answers to older calls are dropped.
    seq: u64,
}

#[derive(Default)]
pub(crate) struct BrowseHost {
    zoom_sites: RefCell<Option<ZoomSites>>,
    /// The zoom each awake tab is at (what we last gave Servo).
    zoom_of: RefCell<HashMap<TabId, f32>>,
    wheel_acc: Cell<f64>,
    find: RefCell<Option<FindBar>>,
    find_seq: Cell<u64>,
    /// The tab whose page is fullscreen.
    fullscreen: Cell<Option<TabId>>,
    toast: RefCell<Option<(u64, String)>>,
    toast_seq: Cell<u64>,
    /// Pop-up tab -> the tab whose page opened it.
    openers: RefCell<HashMap<TabId, TabId>>,
}

impl AppState {
    // ------------------------------------------------------------------ chrome state

    pub(crate) fn browse_state(&self, state: &mut mythbrowse_engine::chrome::ChromeState) {
        let active = self.tabs.borrow().active_id();
        state.zoom = active
            .filter(|_| self.card_mode())
            .and_then(|id| self.browse.zoom_of.borrow().get(&id).copied())
            .filter(|z| !browse::is_default_zoom(*z))
            .map(browse::zoom_percent);
        state.find = self.browse.find.borrow().as_ref().and_then(|f| {
            (Some(f.tab) == active).then(|| browse::find_json(&f.query, f.count, f.focus))
        });
        state.fullscreen = self.browse.fullscreen.get().is_some();
        state.toast = self
            .browse
            .toast
            .borrow()
            .as_ref()
            .map(|(id, text)| json!({ "id": id, "text": text }));
    }

    fn browse_toast(&self, text: String) {
        let id = self.browse.toast_seq.get() + 1;
        self.browse.toast_seq.set(id);
        *self.browse.toast.borrow_mut() = Some((id, text));
        self.mark_chrome_dirty();
    }

    // ------------------------------------------------------------------ tabs

    /// `request_create_new` opened `popup` for the page in `opener`.
    pub(crate) fn browse_popup_opened(&self, popup: TabId, opener: Option<TabId>) {
        if let Some(o) = opener {
            self.browse.openers.borrow_mut().insert(popup, o);
        }
    }

    /// Whether `id` opened a pop-up that is still open (kept awake for `window.opener`).
    pub(crate) fn browse_is_opener(&self, id: TabId) -> bool {
        self.browse.openers.borrow().values().any(|o| *o == id)
    }

    /// Servo says the page closed its own window (`window.close()`, already checked as
    /// script-closable by Servo). Close the tab and bring back the one that opened it.
    pub(crate) fn browse_page_closed(&self, webview: &WebView) {
        if self.is_chrome(webview) || self.agent.owns(webview) {
            return; // Myth's tabs: the agent manages them
        }
        let Some(id) = self.tab_id_of(webview) else {
            return;
        };
        let opener = self.browse.openers.borrow_mut().remove(&id);
        let was_active = self.tabs.borrow().active_id() == Some(id);
        info!("tab {id:?} closed itself (window.close)");
        self.close_tab(id);
        if let Some(o) = opener
            && was_active
            && self.tabs.borrow().get(o).is_some()
        {
            self.activate_tab(o);
        }
    }

    /// Bookkeeping when a tab goes away (from `close_tab`).
    pub(crate) fn browse_tab_closed(&self, id: TabId) {
        self.browse
            .openers
            .borrow_mut()
            .retain(|p, o| *p != id && *o != id);
        self.browse.zoom_of.borrow_mut().remove(&id);
        let find_here = self
            .browse
            .find
            .borrow()
            .as_ref()
            .is_some_and(|f| f.tab == id);
        if find_here {
            *self.browse.find.borrow_mut() = None;
        }
        if self.browse.fullscreen.get() == Some(id) {
            self.browse_leave_fullscreen(None);
        }
    }

    /// The active tab changed (from `show_active`): a fullscreen page that is no longer in
    /// front leaves fullscreen.
    pub(crate) fn browse_tab_shown(&self) {
        let active = self.tabs.borrow().active_id();
        if let Some(fs) = self.browse.fullscreen.get()
            && Some(fs) != active
        {
            let view = self.tabs.borrow().get(fs).and_then(|t| t.view.clone());
            self.browse_leave_fullscreen(view.as_ref());
        }
    }

    /// A user tab landed on `url` (from `notify_url_changed`): its site's zoom, and the find
    /// bar starts counting again.
    pub(crate) fn browse_url_changed(&self, id: TabId, webview: &WebView, url: &Url) {
        let want = self.browse_site_zoom(url);
        let have = self
            .browse
            .zoom_of
            .borrow()
            .get(&id)
            .copied()
            .unwrap_or(1.0);
        if (want - have).abs() > 0.004 {
            webview.set_page_zoom(want);
        }
        self.browse.zoom_of.borrow_mut().insert(id, want);
        if let Some(f) = self.browse.find.borrow_mut().as_mut()
            && f.tab == id
        {
            f.count = None;
        }
    }

    // ------------------------------------------------------------------ zoom

    fn zoom_site_key(url: &Url) -> Option<String> {
        match url.scheme() {
            "http" | "https" => url.host_str().map(crate::cookies_host::site_of_host),
            "file" => Some("file".to_owned()),
            _ => None,
        }
    }

    fn with_zoom_sites<R>(&self, f: impl FnOnce(&mut ZoomSites) -> R) -> R {
        let mut slot = self.browse.zoom_sites.borrow_mut();
        let sites = slot.get_or_insert_with(|| {
            let raw = self.store.pref(PREF_ZOOM_SITES).ok().flatten();
            ZoomSites::from_pref(raw.as_deref())
        });
        f(sites)
    }

    fn browse_site_zoom(&self, url: &Url) -> f32 {
        match Self::zoom_site_key(url) {
            Some(site) => self.with_zoom_sites(|z| z.get(&site)),
            None => 1.0,
        }
    }

    /// Ctrl + (`dir` > 0), Ctrl - (< 0), Ctrl 0 (= 0) on the active web page.
    pub(crate) fn browse_zoom(&self, dir: i32) -> bool {
        let (id, view, url) = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.active() else { return false };
            let Some(v) = t.view.clone() else {
                return false;
            };
            (t.id, v, t.url.clone())
        };
        let Some(url) = Url::parse(&url).ok().filter(|_| self.card_mode()) else {
            return false; // internal pages keep 100 %
        };
        let cur = self
            .browse
            .zoom_of
            .borrow()
            .get(&id)
            .copied()
            .unwrap_or(1.0);
        let new = browse::zoom_step(cur, dir.signum());
        if (new - cur).abs() > 0.004 {
            view.set_page_zoom(new);
        }
        self.browse.zoom_of.borrow_mut().insert(id, new);
        if let Some(site) = Self::zoom_site_key(&url) {
            let pref = self.with_zoom_sites(|z| {
                z.set(&site, new);
                z.to_pref()
            });
            if let Err(e) = self.store.set_pref(PREF_ZOOM_SITES, &pref) {
                warn!("zoom: could not save ({e})");
            }
        }
        // Find highlights sit on the old layout.
        self.browse_find_refresh();
        self.mark_chrome_dirty();
        true
    }

    /// Ctrl+wheel over the page: zoom instead of scrolling. Returns true when it was taken.
    pub(crate) fn browse_wheel_zoom(&self, input: WheelInput) -> bool {
        if self.hover.get() != Area::Content || !self.card_mode() {
            return false;
        }
        let mut acc = self.browse.wheel_acc.get();
        let steps = match input {
            WheelInput::Lines(_, dy) => browse::wheel_zoom_steps(&mut acc, dy, true),
            WheelInput::Pixels(_, dy) => browse::wheel_zoom_steps(&mut acc, dy, false),
        };
        self.browse.wheel_acc.set(acc);
        for _ in 0..steps.unsigned_abs() {
            self.browse_zoom(steps.signum());
        }
        true
    }

    // ------------------------------------------------------------------ external links

    /// A navigation (or a reported link click) to a URL the engine does not load. Returns
    /// true when it was one (the caller then denies the navigation).
    pub(crate) fn browse_scheme(&self, webview: &WebView, url: &Url) -> bool {
        let action = browse::scheme_action(url);
        if action == SchemeAction::Load {
            return false;
        }
        if self.is_chrome(webview) || self.agent.owns(webview) {
            return true; // never launched for Myth's tabs or the chrome
        }
        let Some(tab) = self.tab_id_of(webview) else {
            return true;
        };
        // Only right after the user's own click / key press in this page; consumed.
        if !browse::external_allowed(self.ui_last_activation(tab), self.now_ms()) {
            info!("{}: link without a user click; ignored", url.scheme());
            return true;
        }
        self.ui_consume_activation(tab);
        match action {
            SchemeAction::OpenExternally => match browse::external_target(url) {
                Some(target) => match mythbrowse_files::os::open_url(&target) {
                    Ok(()) => {
                        info!("{}: link handed to the OS", url.scheme());
                        self.browse_toast(browse::external_note(url).to_owned());
                    }
                    Err(e) => self.browse_toast(e),
                },
                None => self.browse_toast("This link is too long to open in another app.".into()),
            },
            _ => {
                info!("refused a {}: link", url.scheme());
                self.browse_toast(browse::refused_note(url));
            }
        }
        true
    }

    // ------------------------------------------------------------------ find in page

    /// Ctrl+F: open the find bar on the active web page (or focus it again).
    pub(crate) fn browse_find_open(&self) -> bool {
        let Some(id) = self.tabs.borrow().active_id() else {
            return false;
        };
        if !self.card_mode() {
            return false;
        }
        {
            let mut slot = self.browse.find.borrow_mut();
            match slot.as_mut() {
                Some(f) if f.tab == id => f.focus += 1,
                _ => {
                    let query = slot.as_ref().map(|f| f.query.clone()).unwrap_or_default();
                    *slot = Some(FindBar {
                        tab: id,
                        query,
                        count: None,
                        focus: 1,
                        seq: 0,
                    });
                }
            }
        }
        self.set_focus(Area::Chrome);
        self.mark_chrome_dirty();
        true
    }

    /// F3 / Shift+F3 (and Enter / Shift+Enter in the bar): next / previous match.
    pub(crate) fn browse_find_step(&self, dir: FindDir) -> bool {
        let q = match self.browse.find.borrow().as_ref() {
            Some(f) if Some(f.tab) == self.tabs.borrow().active_id() => f.query.clone(),
            _ => return false,
        };
        self.browse_find(q, dir);
        true
    }

    /// Search the active tab (`/api/find`).
    pub(crate) fn browse_find(&self, query: String, dir: FindDir) {
        let Some((id, view)) = self
            .tabs
            .borrow()
            .active()
            .and_then(|t| t.view.clone().map(|v| (t.id, v)))
        else {
            return;
        };
        let seq = self.browse.find_seq.get() + 1;
        self.browse.find_seq.set(seq);
        {
            let mut slot = self.browse.find.borrow_mut();
            let focus = slot.as_ref().map_or(1, |f| f.focus);
            *slot = Some(FindBar {
                tab: id,
                query: query.clone(),
                count: slot
                    .as_ref()
                    .filter(|f| f.query == query)
                    .and_then(|f| f.count),
                focus,
                seq,
            });
        }
        if query.is_empty() {
            view.evaluate_javascript(browse::FIND_CLEAR_SCRIPT, |_| {});
            self.mark_chrome_dirty();
            return;
        }
        let state = self.weak_self.clone();
        view.evaluate_javascript(browse::find_script(&query, dir), move |r| {
            let Some(s) = state.upgrade() else { return };
            let count = match r {
                Ok(JSValue::String(a)) => browse::parse_find_answer(&a),
                _ => None,
            };
            if let Some(f) = s.browse.find.borrow_mut().as_mut()
                && f.seq == seq
            {
                // A page that cannot search (scripts off) shows "no results".
                f.count = Some(count.unwrap_or_default());
            }
            s.mark_chrome_dirty();
        });
        self.mark_chrome_dirty();
    }

    /// The page relaid out (zoom): search again so the highlights follow.
    fn browse_find_refresh(&self) {
        let q = match self.browse.find.borrow().as_ref() {
            Some(f) if !f.query.is_empty() && Some(f.tab) == self.tabs.borrow().active_id() => {
                f.query.clone()
            }
            _ => return,
        };
        self.browse_find(q, FindDir::New);
    }

    /// Esc / the bar's close button.
    pub(crate) fn browse_find_close(&self) {
        let closed = self.browse.find.borrow_mut().take();
        if let Some(f) = closed
            && let Some(view) = self.tabs.borrow().get(f.tab).and_then(|t| t.view.clone())
        {
            view.evaluate_javascript(browse::FIND_CLEAR_SCRIPT, |_| {});
        }
        self.set_focus(Area::Content);
        self.mark_chrome_dirty();
    }

    // ------------------------------------------------------------------ fullscreen

    pub(crate) fn browse_fullscreen(&self) -> bool {
        self.browse.fullscreen.get().is_some()
    }

    /// Servo: the page entered (`on`) or left fullscreen (Fullscreen API).
    pub(crate) fn browse_fullscreen_changed(&self, webview: &WebView, on: bool) {
        if self.is_chrome(webview) || self.agent.owns(webview) {
            if on {
                webview.exit_fullscreen(); // never for background / chrome pages
            }
            return;
        }
        let Some(id) = self.tab_id_of(webview) else {
            return;
        };
        if on {
            if self.tabs.borrow().active_id() != Some(id) {
                webview.exit_fullscreen(); // only the tab in front
                return;
            }
            info!("page fullscreen on (tab {id:?})");
            self.browse.fullscreen.set(Some(id));
            self.window
                .set_fullscreen(Some(Fullscreen::Borderless(None)));
            self.autoscroll_stop();
            self.layout(false);
            self.set_focus(Area::Content);
            self.mark_chrome_dirty();
        } else if self.browse.fullscreen.get() == Some(id) {
            self.browse_leave_fullscreen(None);
        }
    }

    /// Leave fullscreen: the window first (so the chrome is back at once), then tell the page
    /// (`view`, when it still thinks it is fullscreen).
    pub(crate) fn browse_leave_fullscreen(&self, view: Option<&WebView>) {
        if self.browse.fullscreen.take().is_none() {
            return;
        }
        info!("page fullscreen off");
        self.window.set_fullscreen(None);
        if let Some(v) = view {
            v.exit_fullscreen();
        }
        self.layout(false);
        self.mark_chrome_dirty();
    }

    /// Esc while a page is fullscreen.
    pub(crate) fn browse_escape_fullscreen(&self) -> bool {
        let Some(id) = self.browse.fullscreen.get() else {
            return false;
        };
        let view = self.tabs.borrow().get(id).and_then(|t| t.view.clone());
        self.browse_leave_fullscreen(view.as_ref());
        true
    }
}
