// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Session restore: what the window looked like, and whether to bring it back.
//!
//! The shell saves a [`Session`] (debounced, ~2 s after a change) with `clean = 0`, and once
//! more with `clean = 1` on a normal exit. At startup [`decide`]:
//!
//! * a **private** window never restores and never saves;
//! * after a **crash** (the last save is not clean) the session always comes back;
//! * after a **normal exit** the "On startup" setting decides: [`Startup::Continue`]
//!   (default) or [`Startup::NewTab`].
//!
//! Restored tabs open **sleeping** except the active one (no page loads until clicked), with
//! their title, pinned state and back / forward lists. Only web pages (http, https, file) and
//! a few internal pages (new tab, library, downloads) are kept; a new tab that showed a saved
//! conversation keeps `?c=` so it reopens it, a one-off prompt (`?q=`) is dropped.

use serde::{Deserialize, Serialize};
use url::Url;

/// The reserved internal origin's host (same as `mythbrowse_engine::chrome::INTERNAL_HOST`).
const INTERNAL_HOST: &str = "mythbrowse.internal";
/// At most this many tabs are saved.
pub const MAX_TABS: usize = 500;
/// At most this many back (and forward) entries per tab.
pub const MAX_HISTORY: usize = 50;
const MAX_TITLE: usize = 300;
const MAX_URL: usize = 8 * 1024;

/// One saved tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionTab {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pinned: bool,
    /// The tab was asleep when saved (it is restored asleep anyway unless active).
    #[serde(default)]
    pub sleeping: bool,
    #[serde(default)]
    pub scroll_y: f64,
    /// Older entries first; the last one is what Back goes to.
    #[serde(default)]
    pub back: Vec<String>,
    /// Nearest first: the first one is what Forward goes to.
    #[serde(default)]
    pub forward: Vec<String>,
}

/// The saved window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Session {
    #[serde(default = "version")]
    pub version: u32,
    pub tabs: Vec<SessionTab>,
    /// Index into `tabs`.
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub saved_ms: i64,
}

fn version() -> u32 {
    1
}

impl Session {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"tabs\":[]}".into())
    }

    /// Parse a saved session; anything unreadable is an empty session.
    pub fn from_json(s: &str) -> Session {
        serde_json::from_str::<Session>(s)
            .map(Session::cleaned)
            .unwrap_or_default()
    }

    /// Only restorable tabs and entries, caps applied, `active` in range; pinned tabs first
    /// (in their order), as the tab strip shows them.
    pub fn cleaned(self) -> Session {
        let active_url = self.tabs.get(self.active).map(|t| t.url.clone());
        let active_was = self.active;
        let mut kept: Vec<(usize, SessionTab)> = Vec::new();
        for (i, t) in self.tabs.into_iter().enumerate() {
            let Some(url) = restorable(&t.url) else {
                continue;
            };
            let clean_list = |v: Vec<String>| -> Vec<String> {
                v.into_iter().filter_map(|u| restorable(&u)).collect()
            };
            let mut back = clean_list(t.back);
            if back.len() > MAX_HISTORY {
                back.drain(..back.len() - MAX_HISTORY);
            }
            let mut forward = clean_list(t.forward);
            forward.truncate(MAX_HISTORY);
            kept.push((
                i,
                SessionTab {
                    url,
                    title: t.title.chars().take(MAX_TITLE).collect(),
                    pinned: t.pinned,
                    sleeping: t.sleeping,
                    scroll_y: if t.scroll_y.is_finite() {
                        t.scroll_y.max(0.0)
                    } else {
                        0.0
                    },
                    back,
                    forward,
                },
            ));
            if kept.len() == MAX_TABS {
                break;
            }
        }
        // Pinned first, stable.
        kept.sort_by_key(|(_, t)| !t.pinned);
        let active = kept
            .iter()
            .position(|(i, _)| *i == active_was)
            .or_else(|| active_url.and_then(|u| kept.iter().position(|(_, t)| t.url == u)))
            .unwrap_or(0);
        Session {
            version: version(),
            tabs: kept.into_iter().map(|(_, t)| t).collect(),
            active,
            saved_ms: self.saved_ms,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

/// The URL to save for a tab showing `url`, or `None` if it is not worth restoring.
pub fn restorable(url: &str) -> Option<String> {
    if url.len() > MAX_URL {
        return None;
    }
    let mut u = Url::parse(url).ok()?;
    match u.scheme() {
        "http" | "https" if u.host_str() == Some(INTERNAL_HOST) => {
            if u.scheme() != "https" || u.port().is_some() {
                return None;
            }
            match u.path() {
                "/home.html" => {
                    // Keep a saved conversation (?c=), drop a one-off prompt (?q=).
                    let conv = u
                        .query_pairs()
                        .find(|(k, _)| k == "c")
                        .map(|(_, v)| v.into_owned())
                        .filter(|v| {
                            !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit())
                        });
                    u.set_query(None);
                    if let Some(c) = conv {
                        u.query_pairs_mut().append_pair("c", &c);
                    }
                    u.set_fragment(None);
                    Some(u.to_string())
                }
                "/library.html" | "/downloads.html" => {
                    let id = u
                        .query_pairs()
                        .find(|(k, _)| k == "id")
                        .map(|(_, v)| v.into_owned())
                        .filter(|v| v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit()));
                    let frag = u.fragment().map(str::to_owned);
                    u.set_query(None);
                    if let Some(id) = id {
                        u.query_pairs_mut().append_pair("id", &id);
                    }
                    u.set_fragment(frag.as_deref().filter(|f| f.len() <= 32));
                    Some(u.to_string())
                }
                _ => None,
            }
        }
        "http" | "https" | "file" => Some(u.to_string()),
        _ => None,
    }
}

/// The "On startup" setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Startup {
    /// Continue where you left off (default).
    #[default]
    Continue,
    /// Open a new tab.
    NewTab,
}

impl Startup {
    pub fn as_str(self) -> &'static str {
        match self {
            Startup::Continue => "continue",
            Startup::NewTab => "newtab",
        }
    }

    /// Unknown values fall back to the default.
    pub fn parse(s: &str) -> Startup {
        match s.trim() {
            "newtab" | "new_tab" => Startup::NewTab,
            _ => Startup::Continue,
        }
    }
}

/// What to open at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Restore,
    Fresh,
}

/// `was_clean`: the last save came from a normal exit. `has_session`: something to restore.
pub fn decide(private: bool, has_session: bool, was_clean: bool, startup: Startup) -> Decision {
    if private || !has_session {
        return Decision::Fresh;
    }
    if !was_clean {
        return Decision::Restore;
    }
    match startup {
        Startup::Continue => Decision::Restore,
        Startup::NewTab => Decision::Fresh,
    }
}

/// Split a joint history list at `index` (the current entry) into (back, current, forward)
/// in [`SessionTab`] order.
pub fn split_history(
    entries: &[String],
    index: usize,
) -> (Vec<String>, Option<String>, Vec<String>) {
    if entries.is_empty() {
        return (Vec::new(), None, Vec::new());
    }
    let index = index.min(entries.len() - 1);
    (
        entries[..index].to_vec(),
        Some(entries[index].clone()),
        entries[index + 1..].to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(url: &str) -> SessionTab {
        SessionTab {
            url: url.into(),
            title: "t".into(),
            ..Default::default()
        }
    }

    #[test]
    fn restorable_urls() {
        assert_eq!(
            restorable("https://example.com/a?b#c").as_deref(),
            Some("https://example.com/a?b#c")
        );
        assert!(restorable("file:///C:/x.txt").is_some());
        assert_eq!(
            restorable("https://mythbrowse.internal/home.html?q=hello&c=12").as_deref(),
            Some("https://mythbrowse.internal/home.html?c=12")
        );
        assert_eq!(
            restorable("https://mythbrowse.internal/home.html?q=secret").as_deref(),
            Some("https://mythbrowse.internal/home.html")
        );
        assert_eq!(
            restorable("https://mythbrowse.internal/library.html?id=5&t=tok#chats").as_deref(),
            Some("https://mythbrowse.internal/library.html?id=5#chats")
        );
        assert!(restorable("https://mythbrowse.internal/downloads.html").is_some());
        assert!(restorable("https://mythbrowse.internal/chrome.html").is_none());
        assert!(restorable("https://mythbrowse.internal/api/x?t=tok").is_none());
        assert!(restorable("http://mythbrowse.internal/home.html").is_none());
        assert!(restorable("about:blank").is_none());
        assert!(restorable("data:text/html,hi").is_none());
        assert!(restorable("javascript:alert(1)").is_none());
        assert!(restorable(&format!("https://x.test/{}", "a".repeat(9000))).is_none());
    }

    #[test]
    fn cleaned_caps_orders_and_keeps_active() {
        let mut s = Session {
            tabs: vec![
                tab("about:blank"),
                tab("https://a.test/"),
                tab("https://b.test/"),
            ],
            active: 2,
            ..Default::default()
        };
        s.tabs[2].pinned = true;
        s.tabs[1].back = (0..80).map(|i| format!("https://a.test/{i}")).collect();
        s.tabs[1].back.push("javascript:x".into());
        s.tabs[1].forward = vec!["https://a.test/f".into(), "chrome://x".into()];
        let c = s.cleaned();
        assert_eq!(c.tabs.len(), 2);
        assert_eq!(c.tabs[0].url, "https://b.test/"); // pinned first
        assert_eq!(c.active, 0); // still b
        assert_eq!(c.tabs[1].back.len(), MAX_HISTORY);
        assert_eq!(c.tabs[1].back.last().unwrap(), "https://a.test/79");
        assert_eq!(c.tabs[1].forward, vec!["https://a.test/f".to_string()]);
        let many = Session {
            tabs: (0..600)
                .map(|i| tab(&format!("https://x.test/{i}")))
                .collect(),
            active: 599,
            ..Default::default()
        };
        let c = many.cleaned();
        assert_eq!(c.tabs.len(), MAX_TABS);
        assert_eq!(c.active, 0);
    }

    #[test]
    fn json_round_trip_and_garbage() {
        let s = Session {
            tabs: vec![SessionTab {
                url: "https://a.test/".into(),
                title: "A".into(),
                pinned: true,
                sleeping: true,
                scroll_y: 40.0,
                back: vec!["https://a.test/0".into()],
                forward: vec![],
            }],
            active: 0,
            saved_ms: 5,
            version: 1,
        };
        assert_eq!(Session::from_json(&s.to_json()), s);
        assert!(Session::from_json("not json").is_empty());
        assert!(Session::from_json("{\"tabs\":[{\"url\":\"about:blank\"}]}").is_empty());
    }

    #[test]
    fn decisions_and_history_split() {
        use Decision::*;
        assert_eq!(decide(true, true, false, Startup::Continue), Fresh);
        assert_eq!(decide(false, false, false, Startup::Continue), Fresh);
        assert_eq!(decide(false, true, false, Startup::NewTab), Restore); // crash
        assert_eq!(decide(false, true, true, Startup::NewTab), Fresh);
        assert_eq!(decide(false, true, true, Startup::Continue), Restore);
        assert_eq!(Startup::parse("newtab"), Startup::NewTab);
        assert_eq!(Startup::parse("weird"), Startup::Continue);
        let e: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            split_history(&e, 1),
            (vec!["a".into()], Some("b".into()), vec!["c".into()])
        );
        assert_eq!(split_history(&e, 9).1.as_deref(), Some("c"));
        assert_eq!(split_history(&[], 0), (vec![], None, vec![]));
    }
}
