// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Tab model with tab sleeping, generic over the engine's view handle so it is testable
//! without Servo.
//!
//! A tab is either **awake** (it owns a live view) or **sleeping** (the view was released to
//! free memory; only the URL, title and last scroll position are kept). Time is passed in
//! explicitly as milliseconds on a monotonic clock, so the sleeping policy is deterministic
//! in tests.

/// Stable tab identifier (never reused within a session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

/// Page scroll offset in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScrollPos {
    pub x: f64,
    pub y: f64,
}

impl ScrollPos {
    pub fn is_origin(&self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }
}

#[derive(Debug)]
pub struct Tab<H> {
    pub id: TabId,
    /// `None` while the tab is sleeping.
    pub view: Option<H>,
    pub title: String,
    pub url: String,
    /// True between load start and load complete.
    pub loading: bool,
    /// Last known scroll position (captured when the tab was deactivated).
    pub scroll: ScrollPos,
    /// Scroll position to re-apply once a woken tab finishes loading.
    pub pending_restore: Option<ScrollPos>,
    /// When the tab was last the active tab (ms, monotonic). The active tab is always "now".
    pub last_active_ms: u64,
    /// The page's colour (theme-color or background) the chrome is tinted with while this
    /// tab is active; `None` until the page has loaded, and for pages without one.
    pub tint: Option<[u8; 3]>,
    /// Pinned tabs sit at the start of the strip (narrow, no close button) and are restored
    /// with the session.
    pub pinned: bool,
    /// The live view's own (Servo's) session history and current index, as last reported.
    pub history: Vec<String>,
    pub history_index: usize,
    /// History the live view does not hold: entries from before a sleep, a restart, or an
    /// [`TabManager::step_extra_back`] (Servo 0.1.4 cannot be given a history list). Older
    /// first.
    pub extra_back: Vec<String>,
    /// Entries after the live view's last one; nearest first.
    pub extra_forward: Vec<String>,
}

impl<H> Tab<H> {
    pub fn is_sleeping(&self) -> bool {
        self.view.is_none()
    }

    /// Everything Back can reach, oldest first.
    pub fn back_list(&self) -> Vec<String> {
        let mut v = self.extra_back.clone();
        if !self.history.is_empty() {
            v.extend(
                self.history[..self.history_index.min(self.history.len())]
                    .iter()
                    .cloned(),
            );
        }
        v
    }

    /// Everything Forward can reach, nearest first.
    pub fn forward_list(&self) -> Vec<String> {
        let mut v: Vec<String> = if self.history.is_empty() {
            Vec::new()
        } else {
            self.history[(self.history_index + 1).min(self.history.len())..].to_vec()
        };
        v.extend(self.extra_forward.iter().cloned());
        v
    }

    /// The live view itself can go back (else Back steps into [`Tab::extra_back`]).
    pub fn native_can_go_back(&self) -> bool {
        self.history_index > 0 && !self.history.is_empty()
    }

    pub fn native_can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }

    pub fn can_go_back(&self) -> bool {
        self.native_can_go_back() || !self.extra_back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        self.native_can_go_forward() || !self.extra_forward.is_empty()
    }

    /// Servo reported the view's history. A change of the entries themselves (not just the
    /// index) is a new navigation, which ends any forward history.
    pub fn set_history(&mut self, entries: Vec<String>, index: usize) {
        let moved_only = entries == self.history;
        if !moved_only && !self.history.is_empty() {
            self.extra_forward.clear();
        }
        self.history_index = index.min(entries.len().saturating_sub(1));
        self.history = entries;
    }

    /// The live view is going away (sleep, replacement): keep its history in the extra
    /// lists so Back / Forward still work on the next view.
    pub fn fold_history(&mut self) {
        let back = self.back_list();
        let forward = self.forward_list();
        self.extra_back = back;
        self.extra_forward = forward;
        self.history.clear();
        self.history_index = 0;
    }
}

/// A tab coming back from a saved session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestoredTab {
    pub url: String,
    pub title: String,
    pub pinned: bool,
    pub scroll: ScrollPos,
    /// Older first.
    pub back: Vec<String>,
    /// Nearest first.
    pub forward: Vec<String>,
}

/// When to put background tabs to sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepPolicy {
    /// Sleep a background tab after it has been inactive this long. `0` disables idle sleeping.
    pub idle_ms: u64,
    /// Keep at most this many tabs awake (least recently active ones sleep first).
    /// The active tab always stays awake. `None` = no limit.
    pub max_awake: Option<usize>,
}

impl Default for SleepPolicy {
    fn default() -> Self {
        Self {
            idle_ms: 10 * 60 * 1000,
            max_awake: None,
        }
    }
}

/// Ordered tabs with one active tab (when non-empty).
#[derive(Debug)]
pub struct TabManager<H> {
    tabs: Vec<Tab<H>>,
    active: Option<usize>,
    next_id: u64,
}

impl<H> Default for TabManager<H> {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            next_id: 1,
        }
    }
}

impl<H> TabManager<H> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Open an awake tab after the active one and make it active.
    pub fn open(&mut self, view: H, url: impl Into<String>, now_ms: u64) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        self.touch_active(now_ms);
        let at = self.active.map_or(self.tabs.len(), |i| i + 1);
        self.tabs.insert(
            at,
            Tab {
                id,
                view: Some(view),
                title: String::new(),
                url: url.into(),
                loading: true,
                scroll: ScrollPos::default(),
                pending_restore: None,
                last_active_ms: now_ms,
                tint: None,
                pinned: false,
                history: Vec::new(),
                history_index: 0,
                extra_back: Vec::new(),
                extra_forward: Vec::new(),
            },
        );
        self.active = Some(at);
        id
    }

    /// Open an awake tab right after the active one without activating it (Ctrl / middle
    /// click on a link). Its idle clock starts now, so tab sleeping treats it like a tab
    /// the user just left.
    pub fn open_background(&mut self, view: H, url: impl Into<String>, now_ms: u64) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let at = self.active.map_or(self.tabs.len(), |i| i + 1);
        self.tabs.insert(
            at,
            Tab {
                id,
                view: Some(view),
                title: String::new(),
                url: url.into(),
                loading: true,
                scroll: ScrollPos::default(),
                pending_restore: None,
                last_active_ms: now_ms,
                tint: None,
                pinned: false,
                history: Vec::new(),
                history_index: 0,
                extra_back: Vec::new(),
                extra_forward: Vec::new(),
            },
        );
        if let Some(a) = self.active
            && a >= at
        {
            self.active = Some(a + 1);
        }
        id
    }

    /// Add a sleeping tab at the end of the strip (pinned ones at the end of the pinned
    /// group) without activating it (session restore). It loads when it is woken.
    pub fn open_sleeping(&mut self, r: RestoredTab, now_ms: u64) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let at = if r.pinned {
            self.pinned_count()
        } else {
            self.tabs.len()
        };
        self.tabs.insert(
            at,
            Tab {
                id,
                view: None,
                title: r.title,
                url: r.url,
                loading: false,
                scroll: r.scroll,
                pending_restore: None,
                last_active_ms: now_ms,
                tint: None,
                pinned: r.pinned,
                history: Vec::new(),
                history_index: 0,
                extra_back: r.back,
                extra_forward: r.forward,
            },
        );
        if let Some(a) = self.active
            && a >= at
        {
            self.active = Some(a + 1);
        }
        id
    }

    /// Number of pinned tabs (they are always the first ones).
    pub fn pinned_count(&self) -> usize {
        self.tabs.iter().take_while(|t| t.pinned).count()
    }

    /// Pin or unpin a tab; it moves to the end of the pinned group / the start of the
    /// others. Returns false if the tab is unknown.
    pub fn set_pinned(&mut self, id: TabId, on: bool) -> bool {
        let Some(idx) = self.index_of(id) else {
            return false;
        };
        if self.tabs[idx].pinned == on {
            return true;
        }
        let active_id = self.active_id();
        let mut tab = self.tabs.remove(idx);
        tab.pinned = on;
        let at = self.pinned_count();
        self.tabs.insert(at, tab);
        self.active = active_id.and_then(|a| self.index_of(a));
        true
    }

    /// Swap a tab's live view (Back / Forward into the extra history needs a fresh view).
    /// Returns the old view.
    pub fn replace_view(&mut self, id: TabId, view: H, url: impl Into<String>) -> Option<H> {
        let tab = self.get_mut(id)?;
        let old = tab.view.replace(view);
        tab.url = url.into();
        tab.loading = true;
        tab.history.clear();
        tab.history_index = 0;
        tab.tint = None;
        old
    }

    /// Back past the live view's first entry: returns the URL a fresh view should load (then
    /// call [`TabManager::replace_view`]); the live view's entries become forward history.
    pub fn step_extra_back(&mut self, id: TabId) -> Option<String> {
        let tab = self.get_mut(id)?;
        if tab.native_can_go_back() {
            return None;
        }
        let url = tab.extra_back.pop()?;
        let mut forward: Vec<String> = if tab.history.is_empty() {
            vec![tab.url.clone()]
        } else {
            tab.history[tab.history_index.min(tab.history.len() - 1)..].to_vec()
        };
        forward.append(&mut tab.extra_forward);
        tab.extra_forward = forward;
        tab.history.clear();
        tab.history_index = 0;
        Some(url)
    }

    /// Forward past the live view's last entry (see [`TabManager::step_extra_back`]).
    pub fn step_extra_forward(&mut self, id: TabId) -> Option<String> {
        let tab = self.get_mut(id)?;
        if tab.native_can_go_forward() || tab.extra_forward.is_empty() {
            return None;
        }
        let url = tab.extra_forward.remove(0);
        if tab.history.is_empty() {
            tab.extra_back.push(tab.url.clone());
        } else {
            let upto = tab.history_index.min(tab.history.len() - 1);
            tab.extra_back.extend(tab.history[..=upto].iter().cloned());
        }
        tab.history.clear();
        tab.history_index = 0;
        Some(url)
    }

    /// Close a tab, returning its view (if it was awake). The neighbour to the right
    /// (else left) becomes active when the active tab is closed.
    pub fn close(&mut self, id: TabId) -> Option<Option<H>> {
        let idx = self.index_of(id)?;
        let tab = self.tabs.remove(idx);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > idx => Some(a - 1),
            Some(a) if a == idx => Some(idx.min(self.tabs.len() - 1)),
            other => other,
        };
        Some(tab.view)
    }

    /// Make `id` the active tab. The previously active tab's idle clock starts at `now_ms`.
    pub fn activate(&mut self, id: TabId, now_ms: u64) -> bool {
        match self.index_of(id) {
            Some(i) => {
                self.touch_active(now_ms);
                self.active = Some(i);
                self.tabs[i].last_active_ms = now_ms;
                true
            }
            None => false,
        }
    }

    /// Cycle forward (`step` = 1) or backward (`step` = -1).
    pub fn cycle(&mut self, step: isize, now_ms: u64) -> Option<TabId> {
        let n = self.tabs.len() as isize;
        let cur = self.active? as isize;
        let next = (cur + step).rem_euclid(n) as usize;
        let id = self.tabs[next].id;
        self.activate(id, now_ms);
        Some(id)
    }

    pub fn active(&self) -> Option<&Tab<H>> {
        self.active.map(|i| &self.tabs[i])
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab<H>> {
        self.active.map(move |i| &mut self.tabs[i])
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active().map(|t| t.id)
    }

    pub fn get(&self, id: TabId) -> Option<&Tab<H>> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab<H>> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// Find the (awake) tab whose view matches `pred`.
    pub fn find_mut(&mut self, mut pred: impl FnMut(&H) -> bool) -> Option<&mut Tab<H>> {
        self.tabs
            .iter_mut()
            .find(|t| t.view.as_ref().is_some_and(&mut pred))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab<H>> {
        self.tabs.iter()
    }

    /// 1-based position of the active tab, for display.
    pub fn active_position(&self) -> Option<usize> {
        self.active.map(|i| i + 1)
    }

    pub fn awake_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.view.is_some()).count()
    }

    /// Tabs that should be put to sleep now under `policy`: background tabs idle for at
    /// least `idle_ms`, plus the least recently active background tabs beyond `max_awake`.
    /// Tabs that are still loading are not slept for idleness (but do count for the cap).
    pub fn sleep_candidates(&self, policy: &SleepPolicy, now_ms: u64) -> Vec<TabId> {
        let mut background: Vec<&Tab<H>> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, t)| Some(*i) != self.active && t.view.is_some())
            .map(|(_, t)| t)
            .collect();
        // Oldest first.
        background.sort_by_key(|t| (t.last_active_ms, t.id));

        let mut out: Vec<TabId> = Vec::new();
        if policy.idle_ms > 0 {
            out.extend(
                background
                    .iter()
                    .filter(|t| {
                        !t.loading && now_ms.saturating_sub(t.last_active_ms) >= policy.idle_ms
                    })
                    .map(|t| t.id),
            );
        }
        if let Some(max) = policy.max_awake {
            let awake = self.awake_count() - out.len();
            let mut excess = awake.saturating_sub(max.max(1));
            for t in &background {
                if excess == 0 {
                    break;
                }
                if !out.contains(&t.id) {
                    out.push(t.id);
                    excess -= 1;
                }
            }
        }
        out
    }

    /// Put a background tab to sleep, returning its view so the caller can release it.
    /// The active tab cannot be slept. `scroll` (if known) replaces the stored position.
    pub fn sleep(&mut self, id: TabId, scroll: Option<ScrollPos>) -> Option<H> {
        let idx = self.index_of(id)?;
        if Some(idx) == self.active {
            return None;
        }
        let tab = &mut self.tabs[idx];
        if let Some(s) = scroll {
            tab.scroll = s;
        }
        tab.loading = false;
        tab.pending_restore = None;
        tab.fold_history();
        tab.view.take()
    }

    /// Give a sleeping tab a fresh view (created for `tab.url`). The stored scroll position
    /// becomes `pending_restore`, to be applied when the new page finishes loading.
    /// Returns false (and drops nothing) if the tab is unknown or already awake.
    pub fn wake(&mut self, id: TabId, view: H) -> Result<(), H> {
        let Some(tab) = self.get_mut(id) else {
            return Err(view);
        };
        if tab.view.is_some() {
            return Err(view);
        }
        tab.view = Some(view);
        tab.loading = true;
        tab.pending_restore = (!tab.scroll.is_origin()).then_some(tab.scroll);
        Ok(())
    }

    /// Take the scroll position to restore for this tab, if any (call on load complete).
    pub fn take_pending_restore(&mut self, id: TabId) -> Option<ScrollPos> {
        self.get_mut(id)?.pending_restore.take()
    }

    fn touch_active(&mut self, now_ms: u64) {
        if let Some(i) = self.active {
            self.tabs[i].last_active_ms = now_ms;
        }
    }

    fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn history_lists_fold_and_extra_steps() {
        let mut m: TabManager<&'static str> = TabManager::new();
        let a = m.open("va", "https://a.test/3", 0);
        let t = m.get_mut(a).unwrap();
        t.set_history(
            strs(&["https://a.test/1", "https://a.test/2", "https://a.test/3"]),
            1,
        );
        assert_eq!(t.back_list(), strs(&["https://a.test/1"]));
        assert_eq!(t.forward_list(), strs(&["https://a.test/3"]));
        assert!(t.native_can_go_back() && t.native_can_go_forward());
        // Index move only: keep extra forward.
        t.extra_forward = strs(&["https://a.test/4"]);
        t.set_history(
            strs(&["https://a.test/1", "https://a.test/2", "https://a.test/3"]),
            2,
        );
        assert_eq!(t.forward_list(), strs(&["https://a.test/4"]));
        t.fold_history();
        assert_eq!(
            t.extra_back,
            strs(&["https://a.test/1", "https://a.test/2"])
        );
        assert_eq!(t.extra_forward, strs(&["https://a.test/4"]));
        assert!(t.history.is_empty());
        // Fresh view at /3: native history is just that page.
        t.url = "https://a.test/3".into();
        t.set_history(strs(&["https://a.test/3"]), 0);
        assert!(t.can_go_back() && !t.native_can_go_back());
        assert_eq!(m.step_extra_back(a).as_deref(), Some("https://a.test/2"));
        let t = m.get(a).unwrap();
        assert_eq!(t.extra_back, strs(&["https://a.test/1"]));
        assert_eq!(
            t.extra_forward,
            strs(&["https://a.test/3", "https://a.test/4"])
        );
        assert_eq!(m.replace_view(a, "vb", "https://a.test/2"), Some("va"));
        m.get_mut(a)
            .unwrap()
            .set_history(strs(&["https://a.test/2"]), 0);
        assert_eq!(m.step_extra_forward(a).as_deref(), Some("https://a.test/3"));
        let t = m.get(a).unwrap();
        assert_eq!(
            t.extra_back,
            strs(&["https://a.test/1", "https://a.test/2"])
        );
        assert_eq!(t.extra_forward, strs(&["https://a.test/4"]));
        // A new navigation ends forward history.
        let t = m.get_mut(a).unwrap();
        t.set_history(strs(&["https://a.test/3"]), 0);
        t.set_history(strs(&["https://a.test/3", "https://b.test/"]), 1);
        assert!(t.extra_forward.is_empty());
        assert!(!t.can_go_forward());
    }

    #[test]
    fn sleep_keeps_history_and_restore_opens_sleeping() {
        let mut m: TabManager<&'static str> = TabManager::new();
        let a = m.open("va", "https://a.test/", 0);
        m.get_mut(a)
            .unwrap()
            .set_history(strs(&["https://x.test/", "https://a.test/"]), 1);
        let b = m.open("vb", "https://b.test/", 1);
        assert_eq!(m.sleep(a, None), Some("va"));
        assert_eq!(m.get(a).unwrap().back_list(), strs(&["https://x.test/"]));
        let r = m.open_sleeping(
            RestoredTab {
                url: "https://r.test/".into(),
                title: "R".into(),
                pinned: true,
                back: strs(&["https://q.test/"]),
                ..Default::default()
            },
            2,
        );
        assert_eq!(
            m.iter().next().unwrap().id,
            r,
            "pinned restored tab goes first"
        );
        assert_eq!(
            m.active_id(),
            Some(b),
            "restoring does not change the active tab"
        );
        assert!(m.get(r).unwrap().is_sleeping());
        assert!(m.get(r).unwrap().can_go_back());
        assert_eq!(m.pinned_count(), 1);
    }

    #[test]
    fn pinning_moves_tabs_and_keeps_active() {
        let mut m: TabManager<&'static str> = TabManager::new();
        let a = m.open("a", "https://a.test/", 0);
        let b = m.open("b", "https://b.test/", 0);
        let c = m.open("c", "https://c.test/", 0);
        assert!(m.set_pinned(c, true));
        assert_eq!(m.iter().map(|t| t.id).collect::<Vec<_>>(), vec![c, a, b]);
        assert_eq!(m.active_id(), Some(c));
        assert!(m.set_pinned(b, true));
        assert_eq!(m.iter().map(|t| t.id).collect::<Vec<_>>(), vec![c, b, a]);
        assert!(m.set_pinned(c, false));
        assert_eq!(m.iter().map(|t| t.id).collect::<Vec<_>>(), vec![b, c, a]);
        assert_eq!(m.active_id(), Some(c));
        assert!(!m.set_pinned(TabId(99), true));
    }

    fn views(m: &TabManager<&'static str>) -> Vec<Option<&'static str>> {
        m.iter().map(|t| t.view).collect()
    }

    #[test]
    fn open_inserts_after_active_and_activates() {
        let mut m = TabManager::new();
        let a = m.open("a", "u1", 0);
        let b = m.open("b", "u2", 0);
        assert_eq!(m.active_id(), Some(b));
        m.activate(a, 0);
        let c = m.open("c", "u3", 0);
        assert_eq!(views(&m), [Some("a"), Some("c"), Some("b")]);
        assert_eq!(m.active_id(), Some(c));
        assert_ne!(a, c);
    }

    #[test]
    fn open_background_keeps_the_active_tab() {
        let mut m: TabManager<&'static str> = TabManager::new();
        let a = m.open("a", "a", 0);
        let c = m.open("c", "c", 0);
        m.activate(a, 1);
        let b = m.open_background("b", "b", 2);
        assert_eq!(m.active_id(), Some(a));
        let order: Vec<TabId> = m.iter().map(|t| t.id).collect();
        assert_eq!(order, vec![a, b, c]);
        assert!(m.get(b).unwrap().view.is_some());
        let mut e: TabManager<&'static str> = TabManager::new();
        let only = e.open_background("x", "x", 0);
        assert_eq!(e.active_id(), None);
        assert_eq!(e.len(), 1);
        assert!(e.activate(only, 1));
    }

    #[test]
    fn close_picks_neighbour() {
        let mut m = TabManager::new();
        let a = m.open("a", "", 0);
        let b = m.open("b", "", 0);
        let c = m.open("c", "", 0);
        m.activate(b, 0);
        assert_eq!(m.close(b), Some(Some("b")));
        assert_eq!(m.active_id(), Some(c));
        assert_eq!(m.close(c), Some(Some("c")));
        assert_eq!(m.active_id(), Some(a));
        assert_eq!(m.close(a), Some(Some("a")));
        assert_eq!(m.active_id(), None);
        assert!(m.is_empty());
        assert_eq!(m.close(a), None);
    }

    #[test]
    fn closing_inactive_tab_keeps_active() {
        let mut m = TabManager::new();
        let a = m.open("a", "", 0);
        let b = m.open("b", "", 0);
        assert_eq!(m.close(a), Some(Some("a")));
        assert_eq!(m.active_id(), Some(b));
        assert_eq!(m.active_position(), Some(1));
    }

    #[test]
    fn cycle_wraps() {
        let mut m = TabManager::new();
        let a = m.open(1, "", 0);
        let b = m.open(2, "", 0);
        assert_eq!(m.cycle(1, 0), Some(a));
        assert_eq!(m.cycle(1, 0), Some(b));
        assert_eq!(m.cycle(-1, 0), Some(a));
        assert_eq!(m.cycle(-1, 0), Some(b));
        let mut empty: TabManager<u8> = TabManager::new();
        assert_eq!(empty.cycle(1, 0), None);
    }

    #[test]
    fn find_and_update() {
        let mut m = TabManager::new();
        m.open(10, "x", 0);
        m.open(20, "y", 0);
        m.find_mut(|v| *v == 10).unwrap().title = "Ten".into();
        assert_eq!(m.iter().next().unwrap().title, "Ten");
    }

    fn loaded(m: &mut TabManager<&'static str>) {
        let ids: Vec<_> = m.iter().map(|t| t.id).collect();
        for id in ids {
            m.get_mut(id).unwrap().loading = false;
        }
    }

    #[test]
    fn idle_background_tabs_become_candidates() {
        let policy = SleepPolicy {
            idle_ms: 1000,
            max_awake: None,
        };
        let mut m = TabManager::new();
        let a = m.open("a", "ua", 0);
        let b = m.open("b", "ub", 100); // a idle since 100
        let c = m.open("c", "uc", 500); // b idle since 500, c active
        loaded(&mut m);
        assert!(m.sleep_candidates(&policy, 1099).is_empty());
        assert_eq!(m.sleep_candidates(&policy, 1100), [a]);
        assert_eq!(m.sleep_candidates(&policy, 5000), [a, b]);
        // The active tab is never a candidate, however old.
        assert!(!m.sleep_candidates(&policy, 1_000_000).contains(&c));
        // Switching resets the clock of the newly active tab.
        m.activate(a, 5000);
        assert_eq!(m.sleep_candidates(&policy, 5500), [b]);
        assert_eq!(m.sleep_candidates(&policy, 6000), [b, c]);
    }

    #[test]
    fn loading_tabs_are_not_slept_for_idleness() {
        let policy = SleepPolicy {
            idle_ms: 10,
            max_awake: None,
        };
        let mut m = TabManager::new();
        let a = m.open("a", "", 0);
        m.open("b", "", 0);
        assert!(m.sleep_candidates(&policy, 100).is_empty());
        m.get_mut(a).unwrap().loading = false;
        assert_eq!(m.sleep_candidates(&policy, 100), [a]);
    }

    #[test]
    fn max_awake_sleeps_least_recently_used() {
        let policy = SleepPolicy {
            idle_ms: 0,
            max_awake: Some(2),
        };
        let mut m = TabManager::new();
        let a = m.open("a", "", 0);
        let b = m.open("b", "", 10);
        let c = m.open("c", "", 20);
        let d = m.open("d", "", 30);
        m.activate(a, 40); // order of last use: b(20) c(30) d(40) a(active)
        assert_eq!(m.sleep_candidates(&policy, 50), [b, c]);
        assert_eq!(m.sleep(b, None), Some("b"));
        assert_eq!(m.sleep(c, None), Some("c"));
        assert!(m.sleep_candidates(&policy, 60).is_empty());
        assert_eq!(m.awake_count(), 2);
        let _ = d;
    }

    #[test]
    fn sleep_keeps_metadata_and_wake_restores_scroll() {
        let mut m = TabManager::new();
        let a = m.open("a", "https://a.example/", 0);
        let b = m.open("b", "https://b.example/", 0);
        {
            let tab = m.get_mut(a).unwrap();
            tab.title = "A".into();
            tab.scroll = ScrollPos { x: 0.0, y: 10.0 };
        }
        // Active tab cannot be slept.
        assert_eq!(m.sleep(b, None), None);
        // Sleeping returns the view and keeps url/title; explicit scroll wins.
        assert_eq!(m.sleep(a, Some(ScrollPos { x: 3.0, y: 420.0 })), Some("a"));
        let tab = m.get(a).unwrap();
        assert!(tab.is_sleeping());
        assert_eq!(
            (tab.url.as_str(), tab.title.as_str()),
            ("https://a.example/", "A")
        );
        assert_eq!(m.sleep(a, None), None, "already asleep");
        assert!(m.find_mut(|v| *v == "a").is_none());
        assert_eq!(m.awake_count(), 1);

        // Waking hands back the scroll position once, after load.
        m.activate(a, 100);
        assert_eq!(m.wake(a, "a2"), Ok(()));
        assert_eq!(m.wake(a, "a3"), Err("a3"), "already awake");
        assert!(m.get(a).unwrap().loading);
        assert_eq!(
            m.take_pending_restore(a),
            Some(ScrollPos { x: 3.0, y: 420.0 })
        );
        assert_eq!(m.take_pending_restore(a), None);
        assert_eq!(m.find_mut(|v| *v == "a2").unwrap().id, a);
    }

    #[test]
    fn wake_without_scroll_has_nothing_to_restore() {
        let mut m = TabManager::new();
        let a = m.open("a", "", 0);
        m.open("b", "", 0);
        m.sleep(a, None);
        assert_eq!(m.wake(TabId(999), "x"), Err("x"));
        assert_eq!(m.wake(a, "a2"), Ok(()));
        assert_eq!(m.take_pending_restore(a), None);
    }

    #[test]
    fn closing_sleeping_tab_returns_no_view() {
        let mut m = TabManager::new();
        let a = m.open("a", "", 0);
        m.open("b", "", 0);
        m.sleep(a, None);
        assert_eq!(m.close(a), Some(None));
        assert_eq!(m.len(), 1);
    }
}
