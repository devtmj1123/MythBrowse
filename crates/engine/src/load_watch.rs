// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
//! When a tab's "loading" state should end although Servo never reported `LoadStatus::Complete`.
//!
//! The spinner (and the tab's `loading` flag) follows Servo's load status, which reaches
//! `Complete` with the document's `load` event. Some pages never fire it in Servo: a
//! subresource that never settles holds the load event forever (v0.4.1: google.com search
//! results with a pronunciation `<audio preload>`, which the dummy media backend never
//! finished; fixed at the source by servo-script patch 0006). The page itself works; only
//! the spinner ran forever and anything waiting for "loaded" (Myth's hand-off) waited too.
//!
//! So while a tab loads, the shell looks at it every [`PROBE_EVERY_MS`] (`document.readyState`
//! and the number of resource-timing entries, [`PROBE_SCRIPT`]); once the document is parsed
//! (`interactive`) and no new resource finished for [`QUIET_PROBES`] looks in a row, at least
//! [`MIN_LOADING_MS`] after the load started, the tab counts as loaded ("settled"). A real
//! `Complete` still ends it the usual way. Pure, unit-tested.

/// Look this often at a loading tab.
pub const PROBE_EVERY_MS: u64 = 2000;
/// Never settle earlier than this after the load started.
pub const MIN_LOADING_MS: u64 = 6000;
/// Looks in a row with an unchanged resource count (with the first: 3 looks = 4 s quiet).
pub const QUIET_PROBES: u8 = 2;

/// `{"rs": readyState, "n": resource entries}` as a JSON string.
pub const PROBE_SCRIPT: &str = "(function () { var n = 0; try { n = performance.getEntriesByType('resource').length; } catch (e) {} return JSON.stringify({ rs: String(document.readyState), n: n }); })()";

/// Elements that can hold the load event, for the log line when a tab settles.
pub const BLOCKERS_SCRIPT: &str = "(function () { var out = []; try { [].forEach.call(document.querySelectorAll('img,iframe,frame,audio,video,object,embed,script[src],link[rel=stylesheet]'), function (e) { var t = e.localName, pending = false; if (t === 'img') pending = !e.complete; else if (t === 'audio' || t === 'video') pending = e.readyState < 2 && !e.error; else if (t === 'iframe' || t === 'frame') pending = true; if (pending) out.push(t + ' ' + String(e.currentSrc || e.src || e.data || '').slice(0, 120)); }); } catch (e) {} return out.slice(0, 12).join(' | '); })()";

/// What one look found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    pub ready: String,
    pub resources: u64,
}

impl Look {
    pub fn parse(json: &str) -> Option<Look> {
        let v: serde_json::Value = serde_json::from_str(json).ok()?;
        Some(Look {
            ready: v.get("rs")?.as_str()?.to_owned(),
            resources: v.get("n").and_then(|n| n.as_u64()).unwrap_or(0),
        })
    }
}

/// One tab's watch, from `LoadStatus::Started`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadWatch {
    started_ms: u64,
    last_resources: Option<u64>,
    quiet: u8,
    settled: bool,
}

impl LoadWatch {
    pub fn new(started_ms: u64) -> LoadWatch {
        LoadWatch {
            started_ms,
            last_resources: None,
            quiet: 0,
            settled: false,
        }
    }

    pub fn settled(&self) -> bool {
        self.settled
    }

    /// Whether a look is worth taking now (the tab has loaded for a while).
    pub fn due(&self, now_ms: u64) -> bool {
        !self.settled && now_ms.saturating_sub(self.started_ms) + PROBE_EVERY_MS >= MIN_LOADING_MS
    }

    /// Feed a look; `true` once (the moment the tab settles).
    pub fn on_look(&mut self, now_ms: u64, look: &Look) -> bool {
        if self.settled {
            return false;
        }
        if look.ready != "interactive" && look.ready != "complete" {
            self.quiet = 0;
            self.last_resources = None;
            return false;
        }
        if self.last_resources == Some(look.resources) {
            self.quiet = self.quiet.saturating_add(1);
        } else {
            self.quiet = 0;
        }
        self.last_resources = Some(look.resources);
        if self.quiet >= QUIET_PROBES && now_ms.saturating_sub(self.started_ms) >= MIN_LOADING_MS {
            self.settled = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(rs: &str, n: u64) -> Look {
        Look {
            ready: rs.into(),
            resources: n,
        }
    }

    #[test]
    fn parses_the_probe() {
        assert_eq!(
            Look::parse(r#"{"rs":"interactive","n":130}"#),
            Some(look("interactive", 130))
        );
        assert_eq!(Look::parse(r#"{"rs":"loading"}"#), Some(look("loading", 0)));
        assert_eq!(Look::parse("nope"), None);
        assert!(PROBE_SCRIPT.contains("readyState"));
        assert!(BLOCKERS_SCRIPT.contains("audio"));
    }

    #[test]
    fn a_parsed_quiet_page_settles() {
        // The google.com case: interactive at 130 resources, then nothing more.
        let mut w = LoadWatch::new(0);
        assert!(!w.due(2000));
        assert!(w.due(4000));
        assert!(!w.on_look(4000, &look("interactive", 126)));
        assert!(!w.on_look(6000, &look("interactive", 130)));
        assert!(!w.on_look(8000, &look("interactive", 130)));
        assert!(w.on_look(10_000, &look("interactive", 130)));
        assert!(w.settled());
        assert!(!w.on_look(12_000, &look("interactive", 130)), "only once");
        assert!(!w.due(12_000));
    }

    #[test]
    fn a_page_still_fetching_or_parsing_keeps_loading() {
        let mut w = LoadWatch::new(0);
        for (i, n) in [10, 20, 30, 40, 50, 60].iter().enumerate() {
            assert!(!w.on_look(6000 + i as u64 * 2000, &look("interactive", *n)));
        }
        let mut w = LoadWatch::new(0);
        for i in 0..6 {
            assert!(!w.on_look(6000 + i * 2000, &look("loading", 5)));
        }
        // Quiet but too early.
        let mut w = LoadWatch::new(0);
        assert!(!w.on_look(1000, &look("interactive", 3)));
        assert!(!w.on_look(2000, &look("interactive", 3)));
        assert!(!w.on_look(3000, &look("interactive", 3)));
        assert!(w.on_look(6000, &look("interactive", 3)));
    }
}
