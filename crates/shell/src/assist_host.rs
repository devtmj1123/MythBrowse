// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The Myth panel over web pages and the selection pill, on the shell side
//! (`docs/AI-OVERLAY.md`).
//!
//! - **Panel**: docked right of the page card or floating over it (`frame::AssistCss`); the
//!   chrome draws it, the shell lays it out and composes it like the Tasks panel.
//! - **Per-tab conversation**: the first message from the panel on a tab starts a normal
//!   saved chat; later ones continue it, also after the tab navigates elsewhere.
//! - **Page context**: read from the tab only when a message is sent
//!   (`mythbrowse_ai::page_chat::context_script`), re-capped here, sent as a context attachment
//!   (never saved; never names trusted sites for navguard).
//! - **Actions**: free-form questions and one-click actions are chat messages with the page
//!   context attached; the open build never acts on the page.
//! - **Don't share with Myth** per site (`page_chat::SitePrivacy`): no context, no pill, and the
//!   tab is not lent to the run.
//! - **Selection pill**: placed from the page script's box report (no text); the text is read
//!   only when the user picks an action.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use http::StatusCode;
use log::{info, warn};
use mythbrowse_ai::page_chat::{
    self as assist, Action, COMPARE_MAX_TABS, COMPARE_TAB_TEXT_MAX, Include, PAGE_TEXT_MAX_LOCAL,
    PAGE_TEXT_MAX_REMOTE, PageContext, SitePrivacy, TabDigest,
};
use mythbrowse_engine::chrome::{self, AssistCall, AssistMode, PanelOpen};
use mythbrowse_engine::frame::{self, AssistCss, PopupCss};
use mythbrowse_engine::page_input::SelReport;
use mythbrowse_engine::servo::{JSValue, WebView};
use mythbrowse_engine::tabs::TabId;
use serde_json::{Value, json};

use crate::AppState;
use crate::agent_host::PanelSend;

/// The selection pill as last reported by the active page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Pill {
    tab: TabId,
    /// Window CSS px (the layout keeps it inside the card).
    rect: PopupCss,
    len: u32,
}

pub(crate) struct AssistHost {
    pub(crate) open: Cell<bool>,
    pub(crate) mode: Cell<AssistMode>,
    pub(crate) dock_w: Cell<f32>,
    /// Where the floating card was dragged to (window CSS px), this session.
    pub(crate) float_rect: Cell<Option<PopupCss>>,
    /// Conversation of each tab's panel.
    convs: RefCell<HashMap<TabId, u64>>,
    /// Runs started from the panel (the chrome polls when this changes).
    runs: Cell<u64>,
    /// A message is waiting for its page context (`{tab, action}` for the UI).
    reading: RefCell<Option<(TabId, String)>>,
    privacy: RefCell<SitePrivacy>,
    pill: Cell<Option<Pill>>,
    /// The selection chip: words in the selection the next message carries (set by the
    /// pill's Ask Myth).
    sel_words: Cell<Option<(TabId, usize)>>,
    /// Last error to show in the panel (cleared on the next send).
    error: RefCell<Option<String>>,
    /// The user's local date as the panel last sent it (`Date.toDateString()` in the UI, so
    /// the system prompt follows the user's time zone; the shell itself only knows UTC).
    today: RefCell<Option<String>>,
}

impl AssistHost {
    pub(crate) fn new(
        profile: Option<&std::path::Path>,
        private: bool,
        mode: Option<&str>,
        dock_w: Option<f32>,
    ) -> Self {
        let privacy = match profile {
            Some(p) if !private => SitePrivacy::load(p),
            _ => SitePrivacy::in_memory(),
        };
        AssistHost {
            open: Cell::new(false),
            mode: Cell::new(
                mode.and_then(AssistMode::parse)
                    .unwrap_or(AssistMode::Docked),
            ),
            dock_w: Cell::new(
                dock_w
                    .filter(|w| w.is_finite())
                    .unwrap_or(frame::ASSIST_DOCK_W_CSS),
            ),
            float_rect: Cell::new(None),
            convs: RefCell::new(HashMap::new()),
            runs: Cell::new(0),
            reading: RefCell::new(None),
            privacy: RefCell::new(privacy),
            pill: Cell::new(None),
            sel_words: Cell::new(None),
            error: RefCell::new(None),
            today: RefCell::new(None),
        }
    }
}

/// How a pending message gathers its context.
struct Pending {
    tab: TabId,
    view: WebView,
    action: Action,
    include: Include,
    shot: bool,
    /// The site is set to Don't share: no context, the tab is not lent.
    blocked: bool,
}

impl AppState {
    /// The active tab when it is a web page with a live view: `(id, view, url, title)`.
    fn assist_active_web(&self) -> Option<(TabId, WebView, String, String)> {
        let tabs = self.tabs.borrow();
        let t = tabs.active()?;
        if !chrome::shows_as_card(&t.url) {
            return None;
        }
        Some((t.id, t.view.clone()?, t.url.clone(), t.title.clone()))
    }

    /// Layout input for the panel (`None` when closed or not on a web page).
    pub(crate) fn assist_layout(&self) -> (Option<AssistCss>, Option<PopupCss>) {
        if !self.card_mode() {
            return (None, None);
        }
        let panel = self
            .assist
            .open
            .get()
            .then(|| match self.assist.mode.get() {
                AssistMode::Docked => AssistCss::Docked {
                    w: self.assist.dock_w.get(),
                },
                AssistMode::Floating => AssistCss::Floating(self.assist.float_rect.get()),
            });
        let active = self.tabs.borrow().active_id();
        let pill = self
            .assist
            .pill
            .get()
            .filter(|p| Some(p.tab) == active)
            .map(|p| p.rect);
        (panel, pill)
    }

    /// The panel shows this conversation on the active tab (its pill / card over the page is
    /// not shown then: approvals appear in the panel).
    pub(crate) fn assist_shows_conv(&self, tab: TabId, conv: u64) -> bool {
        self.assist.open.get() && self.assist.convs.borrow().get(&tab) == Some(&conv)
    }

    pub(crate) fn assist_tab_closed(&self, tab: TabId) {
        self.assist.convs.borrow_mut().remove(&tab);
        if self.assist.pill.get().is_some_and(|p| p.tab == tab) {
            self.assist.pill.set(None);
        }
    }

    /// The tab loaded a new page: its selection is gone (the conversation stays).
    pub(crate) fn assist_tab_navigated(&self, tab: TabId) {
        if self.assist.pill.get().is_some_and(|p| p.tab == tab) {
            self.assist.pill.set(None);
            self.layout(false);
        }
        if self.assist.sel_words.get().is_some_and(|(t, _)| t == tab) {
            self.assist.sel_words.set(None);
        }
    }

    pub(crate) fn assist_toggle(&self) {
        self.assist_set_open(!self.assist.open.get());
    }

    fn assist_set_open(&self, open: bool) {
        if self.assist.open.replace(open) != open {
            info!("myth panel {}", if open { "opened" } else { "closed" });
            self.mark_chrome_dirty();
            self.layout(false);
            self.redraw();
        }
    }

    /// `ChromeState.assist`. Never page text or the selected text.
    pub(crate) fn assist_state(&self) -> Option<Value> {
        let (id, _, url, title) = self.assist_active_web()?;
        let conv = self.assist.convs.borrow().get(&id).copied();
        let shared = !self.assist.privacy.borrow().is_blocked(&url);
        let endpoint = self.agent.chat_endpoint();
        // The open build never sends screenshots of pages.
        let vision = false;
        let pill = self.assist.pill.get().is_some_and(|p| p.tab == id) && shared;
        let reading = self
            .assist
            .reading
            .borrow()
            .as_ref()
            .filter(|(t, _)| *t == id)
            .map(|(_, a)| a.clone());
        Some(json!({
            "open": self.assist.open.get(),
            "mode": self.assist.mode.get().as_str(),
            "dock_w": self.assist.dock_w.get(),
            "tab": id.0,
            "conv": conv,
            "runs": self.assist.runs.get(),
            "reading": reading,
            "site": assist::site_of(&url),
            "shared": shared,
            "title": title,
            "url": url,
            "model": endpoint.model,
            "local": endpoint.is_local(),
            "vision": vision,
            "selection": self.assist.sel_words.get().filter(|(t, _)| *t == id && shared).map(|(_, w)| json!({ "words": w })),
            "pill": pill,
            "error": self.assist.error.borrow().clone(),
        }))
    }

    /// A selection report from a page (`page_input.js`).
    pub(crate) fn assist_sel_report(&self, webview: &WebView, r: SelReport) {
        let (tab, active) = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.iter().find(|t| t.view.as_ref() == Some(webview)) else {
                return;
            };
            (
                t.id,
                tabs.active_id() == Some(t.id) && chrome::shows_as_card(&t.url),
            )
        };
        let url = self
            .tabs
            .borrow()
            .get(tab)
            .map(|t| t.url.clone())
            .unwrap_or_default();
        let next = if r.len == 0 || !active || self.assist.privacy.borrow().is_blocked(&url) {
            None
        } else {
            let layout = self.layout.get();
            let scale = self.window.scale_factor().max(0.1) as f32;
            let (cx, cy) = (
                layout.content.x as f32 / scale,
                layout.content.y as f32 / scale,
            );
            let (w, h) = (frame::SELPILL_W_CSS, frame::SELPILL_H_CSS);
            let x = cx + r.x + r.w / 2.0 - w / 2.0;
            // Above the selection; below it when there is no room.
            let above = cy + r.y - h - 10.0;
            let y = if r.y - h - 10.0 >= 4.0 {
                above
            } else {
                cy + r.y + r.h + 10.0
            };
            Some(Pill {
                tab,
                rect: PopupCss {
                    x,
                    y,
                    w,
                    h,
                    r: h / 2.0,
                },
                len: r.len,
            })
        };
        if self.assist.pill.replace(next) != next {
            self.mark_chrome_dirty();
            self.layout(false);
            self.redraw();
        }
    }

    pub(crate) fn assist_api(&self, call: AssistCall) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, chrome::json_ok(v));
        let err = |m: &str| (StatusCode::OK, chrome::json_error(m));
        match call {
            AssistCall::Panel {
                open,
                mode,
                rect,
                dock_w,
            } => {
                if let Some(m) = mode
                    && self.assist.mode.replace(m) != m
                {
                    self.save_ui_pref("assist_mode", m.as_str());
                }
                if let Some(w) = dock_w {
                    let w = (w as f32)
                        .clamp(frame::ASSIST_DOCK_MIN_W_CSS, frame::ASSIST_DOCK_MAX_W_CSS);
                    self.assist.dock_w.set(w);
                    self.save_ui_pref("assist_dock_w", &w.to_string());
                }
                if rect.is_some() {
                    self.assist.float_rect.set(rect);
                }
                let want = match open {
                    Some(PanelOpen::Show) => true,
                    Some(PanelOpen::Hide) => false,
                    Some(PanelOpen::Toggle) => !self.assist.open.get(),
                    None => self.assist.open.get(),
                };
                self.assist_set_open(want);
                self.mark_chrome_dirty();
                self.layout(false);
                self.redraw();
                ok(
                    json!({ "open": self.assist.open.get(), "mode": self.assist.mode.get().as_str() }),
                )
            }
            AssistCall::Send {
                action,
                text,
                page,
                selection,
                shot,
                lang,
                today,
            } => {
                if today.is_some() {
                    *self.assist.today.borrow_mut() = today;
                }
                match self.assist_send(&action, &text, page, selection, shot, &lang) {
                    Ok(()) => ok(Value::Null),
                    Err(e) => {
                        *self.assist.error.borrow_mut() = Some(e.clone());
                        self.mark_chrome_dirty();
                        err(&e)
                    }
                }
            }
            AssistCall::New => {
                if let Some(id) = self.tabs.borrow().active_id() {
                    self.assist.convs.borrow_mut().remove(&id);
                }
                *self.assist.error.borrow_mut() = None;
                self.mark_chrome_dirty();
                ok(Value::Null)
            }
            AssistCall::Stop => {
                let conv = self
                    .tabs
                    .borrow()
                    .active_id()
                    .and_then(|id| self.assist.convs.borrow().get(&id).copied());
                if let Some(c) = conv {
                    self.agent_stop_conv(c);
                }
                ok(Value::Null)
            }
            AssistCall::Share { share } => {
                let Some((_, _, url, _)) = self.assist_active_web() else {
                    return err("Open a web page first.");
                };
                if let Err(e) = self.assist.privacy.borrow_mut().set_blocked(&url, !share) {
                    return err(&e);
                }
                if !share {
                    self.assist.pill.set(None);
                    self.assist.sel_words.set(None);
                    self.layout(false);
                }
                info!(
                    "myth panel: {} {}",
                    if share { "sharing" } else { "not sharing" },
                    assist::site_of(&url).unwrap_or_default()
                );
                self.mark_chrome_dirty();
                ok(json!({ "shared": share }))
            }
            AssistCall::Reveal { block } => {
                let Some((_, view, url, _)) = self.assist_active_web() else {
                    return err("Open a web page first.");
                };
                if self.assist.privacy.borrow().is_blocked(&url) {
                    return err("This site is not shared with Myth.");
                }
                view.evaluate_javascript(assist::reveal_script(block as usize), |_| {});
                ok(Value::Null)
            }
            AssistCall::Sel {
                action,
                lang,
                today,
            } => {
                if today.is_some() {
                    *self.assist.today.borrow_mut() = today;
                }
                self.assist.pill.set(None);
                self.layout(false);
                self.assist_set_open(true);
                if action == "ask" {
                    self.assist_selection_chip();
                    return ok(Value::Null);
                }
                match self.assist_send(&action, "", false, true, false, &lang) {
                    Ok(()) => ok(Value::Null),
                    Err(e) => {
                        *self.assist.error.borrow_mut() = Some(e.clone());
                        self.mark_chrome_dirty();
                        err(&e)
                    }
                }
            }
            AssistCall::SelDismiss => {
                self.assist.pill.set(None);
                self.layout(false);
                self.mark_chrome_dirty();
                ok(Value::Null)
            }
        }
    }

    /// The pill's Ask Myth: count the selection's words for the chip (the text itself is read
    /// again when the message is sent).
    fn assist_selection_chip(&self) {
        let Some((id, view, _, _)) = self.assist_active_web() else {
            return;
        };
        let state = self.weak_self.clone();
        view.evaluate_javascript(assist::context_script(500, true), move |r| {
            let Some(s) = state.upgrade() else { return };
            let words = match r {
                Ok(JSValue::String(j)) => PageContext::parse(&j, 500)
                    .map(|c| PageContext::word_count(&c.selection))
                    .unwrap_or(0),
                _ => 0,
            };
            s.assist.sel_words.set((words > 0).then_some((id, words)));
            s.mark_chrome_dirty();
            s.redraw();
        });
    }

    fn assist_send(
        &self,
        action: &str,
        text: &str,
        page: bool,
        selection: bool,
        shot: bool,
        lang: &str,
    ) -> Result<(), String> {
        let Some((tab, view, url, _)) = self.assist_active_web() else {
            return Err("Open a web page to ask Myth about it.".into());
        };
        let action = Action::parse(action, text, lang).ok_or("Type a message first.")?;
        if self.assist.reading.borrow().is_some() {
            return Err("Myth is still reading the page.".into());
        }
        let running = self
            .assist
            .convs
            .borrow()
            .get(&tab)
            .is_some_and(|c| self.agent_conv_running(*c));
        if running {
            return Err("Myth is still working on the last message. Stop it first.".into());
        }
        *self.assist.error.borrow_mut() = None;
        let blocked = self.assist.privacy.borrow().is_blocked(&url);
        let mut include = action.needs();
        if matches!(action, Action::Ask(_)) {
            include.page = page;
            include.selection =
                selection && self.assist.sel_words.get().is_some_and(|(t, _)| t == tab);
        }
        if action.needs_selection() && blocked {
            return Err("This site is not shared with Myth.".into());
        }
        if matches!(action, Action::CompareTabs) {
            self.assist_compare_tabs(tab);
            return Ok(());
        }
        let p = Pending {
            tab,
            view: view.clone(),
            action,
            include,
            shot: shot && !blocked,
            blocked,
        };
        if blocked || (!p.include.page && !p.include.selection) {
            self.assist_start(p, None);
            return Ok(());
        }
        let max = if self.agent.chat_endpoint().is_local() {
            PAGE_TEXT_MAX_LOCAL
        } else {
            PAGE_TEXT_MAX_REMOTE
        };
        *self.assist.reading.borrow_mut() = Some((tab, action_name(&p.action).to_owned()));
        self.mark_chrome_dirty();
        let state = self.weak_self.clone();
        let fallback = PageContext {
            url: url.clone(),
            title: self
                .tabs
                .borrow()
                .get(tab)
                .map(|t| t.title.clone())
                .unwrap_or_default(),
            ..PageContext::default()
        };
        view.evaluate_javascript(assist::context_script(max, p.include.selection), move |r| {
            let Some(s) = state.upgrade() else { return };
            s.assist.reading.borrow_mut().take();
            let ctx = match r {
                Ok(JSValue::String(j)) => PageContext::parse(&j, max).unwrap_or_else(|e| {
                    warn!("myth panel: {e}");
                    fallback.clone()
                }),
                other => {
                    warn!("myth panel: page context script failed: {other:?}");
                    fallback.clone()
                }
            };
            // The page might have navigated meanwhile: only use the context of the same page.
            let ctx = if assist::site_of(&ctx.url) == assist::site_of(&fallback.url) {
                ctx
            } else {
                fallback.clone()
            };
            if p.action.needs_selection() && ctx.selection.is_empty() {
                *s.assist.error.borrow_mut() = Some("Select some text on the page first.".into());
                s.mark_chrome_dirty();
                s.redraw();
                return;
            }
            s.assist_start(p, Some(ctx));
            s.redraw();
        });
        Ok(())
    }

    /// Start the run with the gathered context.
    fn assist_start(&self, p: Pending, ctx: Option<PageContext>) {
        let page = if p.blocked { None } else { ctx.as_ref() };
        let context = assist::context_message(&p.action, page, p.include, &[]);
        self.assist_run(p.tab, Some(p.view), p.action, context, p.shot, p.blocked);
    }

    fn assist_run(
        &self,
        tab: TabId,
        view: Option<WebView>,
        action: Action,
        context: Option<mythbrowse_ai::ChatMessage>,
        shot: bool,
        blocked: bool,
    ) {
        let conv = self.assist.convs.borrow().get(&tab).copied();
        let tools = action.tools();
        // Only the user's own message lends the tab (and so may act on it); never on a site
        // set to Don't share.
        let here = view.filter(|_| tools && !blocked).map(|v| (tab, v));
        let panel = PanelSend {
            context,
            tools,
            today: self
                .assist
                .today
                .borrow()
                .clone()
                .unwrap_or_else(today_string),
        };
        let shot = shot && here.is_some();
        match self.agent_send_panel(conv, action.display(), here, shot, panel) {
            Ok(id) => {
                self.assist.convs.borrow_mut().insert(tab, id);
                self.assist.runs.set(self.assist.runs.get().wrapping_add(1));
                if self.assist.sel_words.get().is_some_and(|(t, _)| t == tab) {
                    self.assist.sel_words.set(None);
                }
            }
            Err(e) => {
                warn!("myth panel: {e}");
                *self.assist.error.borrow_mut() = Some(e);
            }
        }
        self.mark_chrome_dirty();
    }

    /// Compare open tabs: each shared web tab's readable text (capped), joined, then one run.
    fn assist_compare_tabs(&self, from: TabId) {
        let tabs: Vec<(String, String, Option<WebView>)> = self
            .tabs
            .borrow()
            .iter()
            .filter(|t| chrome::shows_as_card(&t.url))
            .take(COMPARE_MAX_TABS)
            .map(|t| (t.title.clone(), t.url.clone(), t.view.clone()))
            .collect();
        if tabs.len() < 2 {
            *self.assist.error.borrow_mut() =
                Some("Open at least two web pages to compare.".into());
            self.mark_chrome_dirty();
            return;
        }
        let digests: Rc<RefCell<Vec<Option<TabDigest>>>> =
            Rc::new(RefCell::new(vec![None; tabs.len()]));
        let left = Rc::new(Cell::new(tabs.len()));
        *self.assist.reading.borrow_mut() = Some((from, "compare_tabs".into()));
        self.mark_chrome_dirty();
        let finish = {
            let state = self.weak_self.clone();
            let digests = digests.clone();
            Rc::new(move || {
                let Some(s) = state.upgrade() else { return };
                s.assist.reading.borrow_mut().take();
                let list: Vec<TabDigest> = digests.borrow_mut().drain(..).flatten().collect();
                let ctx =
                    assist::context_message(&Action::CompareTabs, None, Include::default(), &list);
                let view = s.tabs.borrow().get(from).and_then(|t| t.view.clone());
                s.assist_run(from, view, Action::CompareTabs, ctx, false, false);
                s.redraw();
            })
        };
        for (i, (title, url, view)) in tabs.into_iter().enumerate() {
            let blocked = self.assist.privacy.borrow().is_blocked(&url);
            let done = |d: TabDigest| {
                digests.borrow_mut()[i] = Some(d);
                left.set(left.get() - 1);
                if left.get() == 0 {
                    finish();
                }
            };
            match view {
                Some(v) if !blocked => {
                    let digests = digests.clone();
                    let left = left.clone();
                    let finish = finish.clone();
                    v.evaluate_javascript(
                        assist::context_script(COMPARE_TAB_TEXT_MAX, false),
                        move |r| {
                            let text = match r {
                                Ok(JSValue::String(j)) => {
                                    PageContext::parse(&j, COMPARE_TAB_TEXT_MAX)
                                        .map(|c| c.text)
                                        .unwrap_or_default()
                                }
                                _ => String::new(),
                            };
                            digests.borrow_mut()[i] = Some(TabDigest {
                                title: title.clone(),
                                url: url.clone(),
                                text: Some(text),
                            });
                            left.set(left.get() - 1);
                            if left.get() == 0 {
                                finish();
                            }
                        },
                    );
                }
                Some(_) => done(TabDigest {
                    title,
                    url,
                    text: None,
                }),
                // A sleeping tab: its title and address only.
                None => done(TabDigest {
                    title,
                    url,
                    text: Some("(tab asleep: title and address only)".into()),
                }),
            }
        }
    }
}

fn action_name(a: &Action) -> &'static str {
    match a {
        Action::Ask(_) => "ask",
        Action::Explain => "explain",
        Action::Translate { .. } => "translate",
        Action::SummarizeSelection => "summarize_selection",
        Action::Rewrite => "rewrite",
        Action::SummarizePage => "summarize",
        Action::KeyPoints => "key_points",
        Action::CompareTabs => "compare_tabs",
        Action::Find(_) => "find",
    }
}

/// Fallback date for the system prompt when the panel sent none (`YYYY-MM-DD`, UTC: the
/// shell has no time zone crate). The panel normally sends the user's local date.
fn today_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn today_is_a_date() {
        let t = super::today_string();
        assert_eq!(t.len(), 10);
        assert!(t.starts_with("20"));
    }
}
