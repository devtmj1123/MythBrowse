// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Browser UI a page can cause, engine-agnostic so it is unit-tested without Servo:
//!
//! - **Context menus**: which items a right-click on a page / link / image / selection /
//!   text field gets ([`page_menu`]), the tab strip menu's actions ([`TabAction`]).
//! - **JS dialogs and permission prompts**: a per-tab queue ([`DialogQueue`]) shown one at a
//!   time as a glass sheet anchored to the page card, with "Prevent this page from creating
//!   more dialogs" ([`DialogQueue::suppress`]), and what happens when one fires while Myth
//!   works in the page ([`agent_policy`]: the AI must never answer them silently).
//! - **Pop-up blocking**: a page may open a window only right after the user's own click or
//!   key press, or when the site is allowed ([`popup_allowed`]).
//! - **IME caret**: the script that measures the caret of the focused text field, so the
//!   system candidate window sits at the caret ([`CARET_RECT_SCRIPT`], [`parse_caret_rect`]).

use std::collections::HashMap;

use serde::Serialize;

use crate::tabs::TabId;

// ------------------------------------------------------------------------- context menus

/// What was under the pointer when the user right-clicked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuContext {
    pub link: Option<String>,
    pub image: Option<String>,
    /// The page has a selection under the pointer, and its text (read from the page;
    /// empty when it could not be read).
    pub selection: Option<String>,
    pub editable: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// The click was in the browser's own UI (address bar), not a web page: only the
    /// editing items apply.
    pub in_chrome: bool,
}

/// A context-menu command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuAction {
    Back,
    Forward,
    Reload,
    ViewSource,
    OpenLinkNewTab,
    CopyLink,
    AskMythLink,
    OpenImageNewTab,
    CopyImageUrl,
    Copy,
    SearchSelection,
    AskMythSelection,
    Cut,
    Paste,
    SelectAll,
}

impl MenuAction {
    pub const ALL: [MenuAction; 15] = [
        MenuAction::Back,
        MenuAction::Forward,
        MenuAction::Reload,
        MenuAction::ViewSource,
        MenuAction::OpenLinkNewTab,
        MenuAction::CopyLink,
        MenuAction::AskMythLink,
        MenuAction::OpenImageNewTab,
        MenuAction::CopyImageUrl,
        MenuAction::Copy,
        MenuAction::SearchSelection,
        MenuAction::AskMythSelection,
        MenuAction::Cut,
        MenuAction::Paste,
        MenuAction::SelectAll,
    ];

    pub fn id(self) -> &'static str {
        match self {
            MenuAction::Back => "back",
            MenuAction::Forward => "forward",
            MenuAction::Reload => "reload",
            MenuAction::ViewSource => "view_source",
            MenuAction::OpenLinkNewTab => "open_link_new_tab",
            MenuAction::CopyLink => "copy_link",
            MenuAction::AskMythLink => "ask_myth_link",
            MenuAction::OpenImageNewTab => "open_image_new_tab",
            MenuAction::CopyImageUrl => "copy_image_url",
            MenuAction::Copy => "copy",
            MenuAction::SearchSelection => "search_selection",
            MenuAction::AskMythSelection => "ask_myth_selection",
            MenuAction::Cut => "cut",
            MenuAction::Paste => "paste",
            MenuAction::SelectAll => "select_all",
        }
    }

    pub fn parse(id: &str) -> Option<MenuAction> {
        MenuAction::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// One row of a context menu (`id` "-" is a separator).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuItem {
    pub id: &'static str,
    pub label: String,
    pub enabled: bool,
    /// Shortcut hint shown on the right ("Ctrl+C").
    #[serde(skip_serializing_if = "str::is_empty")]
    pub key: &'static str,
}

fn item(a: MenuAction, label: impl Into<String>, enabled: bool, key: &'static str) -> MenuItem {
    MenuItem {
        id: a.id(),
        label: label.into(),
        enabled,
        key,
    }
}

fn separator() -> MenuItem {
    MenuItem {
        id: "-",
        label: String::new(),
        enabled: false,
        key: "",
    }
}

/// Selection text as it appears inside a menu label: one line, at most `n` characters.
pub fn label_snippet(text: &str, n: usize) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n {
        one
    } else {
        let mut s: String = one.chars().take(n.saturating_sub(1)).collect();
        s.push('\u{2026}');
        s
    }
}

/// The context menu for `ctx`: groups (link, image, editing / selection, page) separated by
/// separators, never two separators in a row or one at either end. Save / Print / Inspect
/// are not offered (Servo 0.1.4 and this shell have no downloads, printing or devtools).
pub fn page_menu(ctx: &MenuContext) -> Vec<MenuItem> {
    let mut groups: Vec<Vec<MenuItem>> = Vec::new();
    if !ctx.in_chrome {
        if ctx.link.is_some() {
            groups.push(vec![
                item(MenuAction::OpenLinkNewTab, "Open link in new tab", true, ""),
                item(MenuAction::CopyLink, "Copy link address", true, ""),
                item(
                    MenuAction::AskMythLink,
                    "Ask Myth about this link",
                    true,
                    "",
                ),
            ]);
        }
        if ctx.image.is_some() {
            groups.push(vec![
                item(
                    MenuAction::OpenImageNewTab,
                    "Open image in new tab",
                    true,
                    "",
                ),
                item(MenuAction::CopyImageUrl, "Copy image address", true, ""),
            ]);
        }
    }
    let selected = ctx.selection.as_deref().map(str::trim).unwrap_or("");
    let has_selection = ctx.selection.is_some();
    if ctx.editable {
        groups.push(vec![
            item(MenuAction::Cut, "Cut", has_selection, "Ctrl+X"),
            item(MenuAction::Copy, "Copy", has_selection, "Ctrl+C"),
            item(MenuAction::Paste, "Paste", true, "Ctrl+V"),
            item(MenuAction::SelectAll, "Select all", true, "Ctrl+A"),
        ]);
    } else if has_selection {
        groups.push(vec![item(MenuAction::Copy, "Copy", true, "Ctrl+C")]);
    }
    if has_selection && !selected.is_empty() && !ctx.in_chrome {
        let snip = label_snippet(selected, 28);
        groups.push(vec![
            item(
                MenuAction::SearchSelection,
                format!("Search the web for \u{201c}{snip}\u{201d}"),
                true,
                "",
            ),
            item(
                MenuAction::AskMythSelection,
                "Ask Myth about this",
                true,
                "",
            ),
        ]);
    }
    if !ctx.in_chrome && ctx.link.is_none() && ctx.image.is_none() && !ctx.editable {
        groups.push(vec![
            item(MenuAction::Back, "Back", ctx.can_go_back, "Alt+Left"),
            item(
                MenuAction::Forward,
                "Forward",
                ctx.can_go_forward,
                "Alt+Right",
            ),
            item(MenuAction::Reload, "Reload", true, "F5"),
        ]);
        groups.push(vec![item(
            MenuAction::ViewSource,
            "View page source",
            true,
            "",
        )]);
    }
    let mut out = Vec::new();
    for g in groups.into_iter().filter(|g| !g.is_empty()) {
        if !out.is_empty() {
            out.push(separator());
        }
        out.extend(g);
    }
    out
}

/// The tab strip's context menu commands (the chrome builds the menu; the shell checks the
/// command here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAction {
    NewTab,
    Reload,
    Duplicate,
    Close,
    CloseOthers,
    CloseRight,
}

impl TabAction {
    pub fn parse(s: &str) -> Option<TabAction> {
        Some(match s {
            "new_tab" => TabAction::NewTab,
            "reload" => TabAction::Reload,
            "duplicate" => TabAction::Duplicate,
            "close" => TabAction::Close,
            "close_others" => TabAction::CloseOthers,
            "close_right" => TabAction::CloseRight,
            _ => return None,
        })
    }
}

/// The tabs a "close" command removes, given the strip order and the tab it was opened on.
pub fn tabs_to_close(order: &[TabId], on: TabId, action: TabAction) -> Vec<TabId> {
    let Some(i) = order.iter().position(|t| *t == on) else {
        return Vec::new();
    };
    match action {
        TabAction::Close => vec![on],
        TabAction::CloseOthers => order.iter().copied().filter(|t| *t != on).collect(),
        TabAction::CloseRight => order[i + 1..].to_vec(),
        _ => Vec::new(),
    }
}

// ------------------------------------------------------------------------- dialogs

/// A page's dialog or permission prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt {
        default: String,
    },
    /// "Leave site?" (the page has a beforeunload handler).
    BeforeUnload,
    /// A permission prompt: `feature` is a site-settings feature name (e.g. "geolocation").
    Permission {
        feature: String,
    },
}

impl DialogKind {
    /// alert / confirm / prompt: the kinds "Prevent this page from creating more dialogs"
    /// applies to. Permission prompts and "Leave site?" are never silenced by it.
    pub fn is_simple(&self) -> bool {
        matches!(
            self,
            DialogKind::Alert | DialogKind::Confirm | DialogKind::Prompt { .. }
        )
    }
}

/// A dialog waiting for the user. `handle` is the engine object that answers it (kept until
/// the user answers; dropping it answers the page's default: dismissed / denied).
#[derive(Debug)]
pub struct PendingDialog<H> {
    pub id: u64,
    pub tab: TabId,
    pub kind: DialogKind,
    pub message: String,
    /// The site that asked (`scheme://host`), shown on the sheet so a page's text can never
    /// pass for the browser's.
    pub origin: String,
    pub handle: H,
}

/// What [`DialogQueue::push`] did.
#[derive(Debug)]
pub enum Pushed<H> {
    /// Queued for the user (shown when it reaches the front of its tab's queue).
    Queued(u64),
    /// The user silenced this page's dialogs: answer it with the default now.
    Suppressed(H),
}

#[derive(Debug, Default, Clone)]
struct TabDialogs {
    /// Simple dialogs this page showed since it loaded.
    shown: u32,
    suppressed: bool,
}

/// Every tab's pending dialogs, oldest first; each tab shows only its own front one (a
/// dialog is modal to its tab, not to the browser).
#[derive(Debug)]
pub struct DialogQueue<H> {
    items: Vec<PendingDialog<H>>,
    tabs: HashMap<TabId, TabDialogs>,
    next_id: u64,
}

impl<H> Default for DialogQueue<H> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            tabs: HashMap::new(),
            next_id: 1,
        }
    }
}

/// From the second simple dialog of a page on, its sheet offers "Prevent this page from
/// creating more dialogs".
pub const SUPPRESS_OFFER_AFTER: u32 = 1;

impl<H> DialogQueue<H> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(
        &mut self,
        tab: TabId,
        origin: &str,
        kind: DialogKind,
        message: &str,
        handle: H,
    ) -> Pushed<H> {
        let t = self.tabs.entry(tab).or_default();
        if kind.is_simple() {
            if t.suppressed {
                return Pushed::Suppressed(handle);
            }
            t.shown += 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(PendingDialog {
            id,
            tab,
            kind,
            // Pages can send megabytes; the sheet shows a readable amount.
            message: message.chars().take(2000).collect(),
            origin: origin.to_owned(),
            handle,
        });
        Pushed::Queued(id)
    }

    /// The dialog `tab` shows now.
    pub fn front(&self, tab: TabId) -> Option<&PendingDialog<H>> {
        self.items.iter().find(|d| d.tab == tab)
    }

    pub fn has(&self, tab: TabId) -> bool {
        self.front(tab).is_some()
    }

    /// Remove dialog `id` (the user answered it).
    pub fn take(&mut self, id: u64) -> Option<PendingDialog<H>> {
        let i = self.items.iter().position(|d| d.id == id)?;
        Some(self.items.remove(i))
    }

    /// Whether `tab`'s front dialog should offer "Prevent this page from creating more
    /// dialogs".
    pub fn offers_suppress(&self, tab: TabId) -> bool {
        self.front(tab).is_some_and(|d| d.kind.is_simple())
            && self
                .tabs
                .get(&tab)
                .is_some_and(|t| t.shown > SUPPRESS_OFFER_AFTER)
    }

    /// Silence `tab`'s page: its later alert / confirm / prompt are answered with the
    /// default at once. Returns the simple dialogs still queued for it (answer them too).
    pub fn suppress(&mut self, tab: TabId) -> Vec<PendingDialog<H>> {
        self.tabs.entry(tab).or_default().suppressed = true;
        self.drain(|d| d.tab == tab && d.kind.is_simple())
    }

    /// The tab loaded a new page: counting and silencing start over.
    pub fn navigated(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
    }

    /// The tab closed: everything it had queued (answer with the defaults).
    pub fn close_tab(&mut self, tab: TabId) -> Vec<PendingDialog<H>> {
        self.tabs.remove(&tab);
        self.drain(|d| d.tab == tab)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn drain(&mut self, pred: impl Fn(&PendingDialog<H>) -> bool) -> Vec<PendingDialog<H>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if pred(&self.items[i]) {
                out.push(self.items.remove(i));
            } else {
                i += 1;
            }
        }
        out
    }
}

/// How a dialog is handled when it fires in a page Myth is working in (a background agent
/// tab, or the user's tab lent to a run). The AI never answers one by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDialogPolicy {
    /// `alert()`: dismissed at once (it only informs; a hidden tab would block the run
    /// otherwise) and written to the log and the run's steps.
    DismissAndLog,
    /// Shown to the user on Myth's approval card; nothing happens until they answer
    /// (Allow = OK / allow / leave; Don't allow = Cancel / block / stay).
    AskUser,
}

pub fn agent_policy(kind: &DialogKind) -> AgentDialogPolicy {
    match kind {
        DialogKind::Alert => AgentDialogPolicy::DismissAndLog,
        DialogKind::Confirm
        | DialogKind::Prompt { .. }
        | DialogKind::BeforeUnload
        | DialogKind::Permission { .. } => AgentDialogPolicy::AskUser,
    }
}

/// The approval card's text for a page dialog during a run: (action, reason).
pub fn agent_card_text(kind: &DialogKind, origin: &str, message: &str) -> (String, String) {
    let msg = label_snippet(message, 160);
    match kind {
        DialogKind::Permission { feature } => (
            format!("Allow {origin} to {}", feature_label(feature)),
            "the page asked for a permission while Myth was working; Myth never grants one by itself"
                .to_owned(),
        ),
        DialogKind::BeforeUnload => (
            format!("Leave {origin}"),
            "the page warns that changes you made may not be saved".to_owned(),
        ),
        DialogKind::Confirm => (
            format!("Answer OK to {origin}: \u{201c}{msg}\u{201d}"),
            "the page asked for a confirmation while Myth was working".to_owned(),
        ),
        DialogKind::Prompt { default } => (
            format!(
                "Answer {origin}'s question \u{201c}{msg}\u{201d} with \u{201c}{}\u{201d}",
                label_snippet(default, 60)
            ),
            "the page asked for input while Myth was working".to_owned(),
        ),
        DialogKind::Alert => (format!("{origin} says \u{201c}{msg}\u{201d}"), String::new()),
    }
}

/// Site-settings feature name -> what the permission sheet says the site wants to do.
pub fn feature_label(feature: &str) -> &'static str {
    match feature {
        "geolocation" => "know your location",
        "notifications" => "show notifications",
        "camera" => "use your camera",
        "microphone" => "use your microphone",
        "speaker" => "choose your speakers",
        "midi" => "use your MIDI devices",
        "bluetooth" => "use Bluetooth devices",
        "persistent-storage" => "store data persistently on this device",
        "background-sync" => "sync in the background",
        "device-info" => "see your media devices",
        "push" => "send push messages",
        "popups" => "open pop-up windows",
        _ => "use a device feature",
    }
}

// ------------------------------------------------------------------------- pop-ups

/// A page may open a new window this long after the user's own click / key press in it
/// (transient user activation).
pub const USER_ACTIVATION_MS: u64 = 1_000;

/// Whether a page's request for a new window is honoured: the site is always allowed, or the
/// user just clicked / typed in that page. A site the user blocked never opens one.
pub fn popup_allowed(last_activation_ms: Option<u64>, now_ms: u64, site: Option<bool>) -> bool {
    match site {
        Some(allowed) => allowed,
        None => last_activation_ms.is_some_and(|t| now_ms >= t && now_ms - t <= USER_ACTIVATION_MS),
    }
}

// ------------------------------------------------------------------------- IME caret

/// Measures the caret of the focused text field (input, textarea or contenteditable) in the
/// webview's CSS px: returns `"x,y,w,h"` or `""`. For input / textarea the caret position is
/// found with a hidden mirror element that copies the field's text styles (inputs have no
/// caret geometry API). Read-only: it adds the mirror to the document only while measuring.
pub const CARET_RECT_SCRIPT: &str = r#"(function(){try{
var el=document.activeElement;if(!el)return "";
var tag=(el.tagName||"").toLowerCase();
if(tag==="input"||tag==="textarea"){
 var cs=getComputedStyle(el),r=el.getBoundingClientRect(),m=document.createElement("div");
 var props=["boxSizing","width","fontFamily","fontSize","fontWeight","fontStyle","letterSpacing","lineHeight","paddingTop","paddingRight","paddingBottom","paddingLeft","borderTopWidth","borderRightWidth","borderBottomWidth","borderLeftWidth","textTransform","textIndent","wordSpacing","tabSize"];
 for(var i=0;i<props.length;i++)m.style[props[i]]=cs[props[i]];
 m.style.position="absolute";m.style.visibility="hidden";m.style.left="-9999px";m.style.top="0";
 m.style.whiteSpace=tag==="textarea"?"pre-wrap":"pre";m.style.overflow="hidden";
 var end=(typeof el.selectionEnd==="number")?el.selectionEnd:(el.value||"").length;
 m.textContent=(el.value||"").substring(0,end);
 var s=document.createElement("span");s.textContent="\u200b";m.appendChild(s);
 document.body.appendChild(m);
 var x=r.left+s.offsetLeft-el.scrollLeft,y=r.top+s.offsetTop-el.scrollTop,h=s.offsetHeight||parseFloat(cs.fontSize)||16;
 document.body.removeChild(m);
 x=Math.min(Math.max(x,r.left),r.right);y=Math.min(Math.max(y,r.top),Math.max(r.top,r.bottom-h));
 return [x,y,1,h].map(function(v){return Math.round(v)}).join(",");
}
var sel=window.getSelection&&getSelection();
if(sel&&sel.rangeCount){var rr=sel.getRangeAt(0).cloneRange();rr.collapse(false);
 var b=rr.getBoundingClientRect();if(b&&(b.height||b.top)){return [b.left,b.top,1,b.height||16].map(function(v){return Math.round(v)}).join(",")}}
var e=el.getBoundingClientRect();return [e.left,e.top,1,e.height].map(function(v){return Math.round(v)}).join(",");
}catch(_){return ""}})()"#;

/// Parse [`CARET_RECT_SCRIPT`]'s result into a CSS-px rectangle (x, y, w, h). Rejects
/// non-finite, negative-size or absurd values.
pub fn parse_caret_rect(s: &str) -> Option<(f32, f32, f32, f32)> {
    let v: Vec<f32> = s
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    let [x, y, w, h] = v.as_slice() else {
        return None;
    };
    let ok = |n: f32| n.is_finite() && n.abs() < 100_000.0;
    if !(ok(*x) && ok(*y) && ok(*w) && ok(*h)) || *w < 0.0 || *h <= 0.0 {
        return None;
    }
    Some((*x, *y, w.max(1.0), *h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(m: &[MenuItem]) -> Vec<&str> {
        m.iter().map(|i| i.id).collect()
    }

    #[test]
    fn menu_for_plain_page() {
        let m = page_menu(&MenuContext {
            can_go_back: true,
            ..Default::default()
        });
        assert_eq!(
            ids(&m),
            vec!["back", "forward", "reload", "-", "view_source"]
        );
        assert!(m[0].enabled);
        assert!(!m[1].enabled);
    }

    #[test]
    fn menu_for_link_image_selection() {
        let m = page_menu(&MenuContext {
            link: Some("https://a.example/".into()),
            image: Some("https://a.example/i.png".into()),
            selection: Some("  Servo   is\na browser engine written in Rust  ".into()),
            ..Default::default()
        });
        assert_eq!(
            ids(&m),
            vec![
                "open_link_new_tab",
                "copy_link",
                "ask_myth_link",
                "-",
                "open_image_new_tab",
                "copy_image_url",
                "-",
                "copy",
                "-",
                "search_selection",
                "ask_myth_selection",
            ]
        );
        let search = m.iter().find(|i| i.id == "search_selection").unwrap();
        assert_eq!(
            search.label,
            "Search the web for \u{201c}Servo is a browser engine w\u{2026}\u{201d}"
        );
        // No page items (back / reload) on a link: never a separator at an end or doubled.
        assert_ne!(m.first().unwrap().id, "-");
        assert_ne!(m.last().unwrap().id, "-");
        assert!(m.windows(2).all(|w| !(w[0].id == "-" && w[1].id == "-")));
    }

    #[test]
    fn menu_for_editable_and_chrome() {
        let m = page_menu(&MenuContext {
            editable: true,
            ..Default::default()
        });
        assert_eq!(ids(&m), vec!["cut", "copy", "paste", "select_all"]);
        assert!(!m[0].enabled && !m[1].enabled && m[2].enabled);
        assert_eq!(m[2].key, "Ctrl+V");
        let m = page_menu(&MenuContext {
            editable: true,
            selection: Some("abc".into()),
            in_chrome: true,
            link: Some("https://x.example/".into()),
            ..Default::default()
        });
        // The address bar: editing only, no link / search / Ask Myth items.
        assert_eq!(ids(&m), vec!["cut", "copy", "paste", "select_all"]);
        assert!(m[0].enabled);
        // Selection whose text could not be read: Copy only.
        let m = page_menu(&MenuContext {
            selection: Some(String::new()),
            ..Default::default()
        });
        assert_eq!(ids(&m)[0], "copy");
        assert!(!ids(&m).contains(&"search_selection"));
    }

    #[test]
    fn menu_action_ids_round_trip() {
        for a in MenuAction::ALL {
            assert_eq!(MenuAction::parse(a.id()), Some(a));
        }
        assert_eq!(MenuAction::parse("rm -rf"), None);
    }

    #[test]
    fn tab_actions() {
        let order = [TabId(1), TabId(2), TabId(3), TabId(4)];
        assert_eq!(
            tabs_to_close(&order, TabId(2), TabAction::CloseOthers),
            vec![TabId(1), TabId(3), TabId(4)]
        );
        assert_eq!(
            tabs_to_close(&order, TabId(2), TabAction::CloseRight),
            vec![TabId(3), TabId(4)]
        );
        assert!(tabs_to_close(&order, TabId(4), TabAction::CloseRight).is_empty());
        assert_eq!(
            tabs_to_close(&order, TabId(3), TabAction::Close),
            vec![TabId(3)]
        );
        assert!(tabs_to_close(&order, TabId(9), TabAction::Close).is_empty());
        assert!(tabs_to_close(&order, TabId(1), TabAction::Reload).is_empty());
        assert_eq!(TabAction::parse("close_right"), Some(TabAction::CloseRight));
        assert_eq!(TabAction::parse("format_disk"), None);
    }

    #[test]
    fn dialog_queue_is_per_tab_and_fifo() {
        let mut q: DialogQueue<&str> = DialogQueue::new();
        let (a, b) = (TabId(1), TabId(2));
        let Pushed::Queued(id1) = q.push(a, "https://a.example", DialogKind::Alert, "one", "h1")
        else {
            panic!()
        };
        let Pushed::Queued(id2) = q.push(b, "https://b.example", DialogKind::Confirm, "two", "h2")
        else {
            panic!()
        };
        let Pushed::Queued(id3) = q.push(
            a,
            "https://a.example",
            DialogKind::Prompt {
                default: "x".into(),
            },
            "three",
            "h3",
        ) else {
            panic!()
        };
        assert_eq!(q.front(a).unwrap().id, id1);
        assert_eq!(q.front(b).unwrap().id, id2);
        assert_eq!(q.take(id1).unwrap().handle, "h1");
        assert_eq!(q.front(a).unwrap().id, id3);
        assert!(q.take(id1).is_none());
        assert_eq!(q.len(), 2);
        // Closing a tab returns what it had queued.
        let gone = q.close_tab(a);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].handle, "h3");
        assert!(!q.has(a) && q.has(b));
    }

    #[test]
    fn dialog_suppression_offer_and_reset() {
        let mut q: DialogQueue<u32> = DialogQueue::new();
        let t = TabId(5);
        let o = "https://spam.example";
        let Pushed::Queued(first) = q.push(t, o, DialogKind::Alert, "1", 1) else {
            panic!()
        };
        assert!(!q.offers_suppress(t), "the first dialog does not offer it");
        q.take(first);
        q.push(t, o, DialogKind::Alert, "2", 2);
        assert!(q.offers_suppress(t), "the second one does");
        q.push(
            t,
            o,
            DialogKind::Permission {
                feature: "camera".into(),
            },
            "",
            3,
        );
        q.push(t, o, DialogKind::Confirm, "3", 4);
        // Suppress: queued simple dialogs come back (to dismiss); the permission stays.
        let dropped: Vec<u32> = q.suppress(t).into_iter().map(|d| d.handle).collect();
        assert_eq!(dropped, vec![2, 4]);
        assert!(matches!(
            q.front(t).unwrap().kind,
            DialogKind::Permission { .. }
        ));
        assert!(!q.offers_suppress(t), "never offered on a permission sheet");
        assert!(matches!(
            q.push(t, o, DialogKind::Alert, "4", 5),
            Pushed::Suppressed(5)
        ));
        // Permission prompts and "Leave site?" are never silenced.
        assert!(matches!(
            q.push(t, o, DialogKind::BeforeUnload, "", 6),
            Pushed::Queued(_)
        ));
        // A new page starts over.
        q.navigated(t);
        assert!(matches!(
            q.push(t, o, DialogKind::Alert, "5", 7),
            Pushed::Queued(_)
        ));
    }

    #[test]
    fn dialog_message_is_capped() {
        let mut q: DialogQueue<()> = DialogQueue::new();
        q.push(
            TabId(1),
            "https://a.example",
            DialogKind::Alert,
            &"x".repeat(10_000),
            (),
        );
        assert_eq!(q.front(TabId(1)).unwrap().message.chars().count(), 2000);
    }

    #[test]
    fn agent_never_answers_silently() {
        assert_eq!(
            agent_policy(&DialogKind::Alert),
            AgentDialogPolicy::DismissAndLog
        );
        for k in [
            DialogKind::Confirm,
            DialogKind::Prompt {
                default: String::new(),
            },
            DialogKind::BeforeUnload,
            DialogKind::Permission {
                feature: "geolocation".into(),
            },
        ] {
            assert_eq!(agent_policy(&k), AgentDialogPolicy::AskUser, "{k:?}");
        }
        let (action, reason) = agent_card_text(
            &DialogKind::Permission {
                feature: "geolocation".into(),
            },
            "https://maps.example",
            "",
        );
        assert_eq!(action, "Allow https://maps.example to know your location");
        assert!(reason.contains("never grants"));
        let (action, _) = agent_card_text(&DialogKind::BeforeUnload, "https://a.example", "");
        assert_eq!(action, "Leave https://a.example");
    }

    #[test]
    fn popups_need_activation_or_site_permission() {
        assert!(popup_allowed(Some(1_000), 1_500, None));
        assert!(popup_allowed(Some(1_000), 2_000, None));
        assert!(!popup_allowed(Some(1_000), 2_001, None));
        assert!(!popup_allowed(None, 5, None));
        // A clock going backwards never counts as activation.
        assert!(!popup_allowed(Some(5_000), 1_000, None));
        assert!(popup_allowed(None, 5, Some(true)));
        assert!(!popup_allowed(Some(1_000), 1_001, Some(false)));
    }

    #[test]
    fn caret_rect_parsing() {
        assert_eq!(
            parse_caret_rect("10,20,1,18"),
            Some((10.0, 20.0, 1.0, 18.0))
        );
        assert_eq!(
            parse_caret_rect(" -5, 3.5, 0, 16 "),
            Some((-5.0, 3.5, 1.0, 16.0))
        );
        assert_eq!(parse_caret_rect(""), None);
        assert_eq!(parse_caret_rect("1,2,3"), None);
        assert_eq!(parse_caret_rect("1,2,3,0"), None);
        assert_eq!(parse_caret_rect("NaN,2,3,4"), None);
        assert_eq!(parse_caret_rect("1,2,-3,4"), None);
        assert_eq!(parse_caret_rect("1e9,2,3,4"), None);
        assert!(CARET_RECT_SCRIPT.starts_with("(function(){"));
    }

    #[test]
    fn label_snippet_flattens_and_cuts() {
        assert_eq!(label_snippet("a\n\n b\tc", 10), "a b c");
        assert_eq!(label_snippet("abcdef", 4), "abc\u{2026}");
        assert_eq!(label_snippet("中文输入法测试", 4), "中文输\u{2026}");
    }

    #[test]
    fn feature_labels() {
        assert_eq!(feature_label("camera"), "use your camera");
        assert_eq!(feature_label("weird"), "use a device feature");
    }
}
