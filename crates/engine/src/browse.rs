// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Small browsing behaviours with no Servo dependency (v0.4.2), unit-tested here and used by
//! the shell (`crates/shell/src/browse_host.rs`):
//!
//! * page zoom steps (Ctrl + / - / 0, Ctrl+wheel) and the per-site zoom pref ([`ZoomSites`]);
//! * what to do with a link whose scheme the engine cannot load (`mailto:`, `tel:`, app
//!   links): [`scheme_action`], handed to the OS only right after a real click
//!   ([`external_allowed`]);
//! * find in page (Ctrl+F): Servo 0.1.4 has no native find, so the page script
//!   (`js/page_input.js`, `window.__mbFind`) searches and paints the matches; [`find_script`]
//!   asks it and [`parse_find_answer`] reads the answer.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use url::Url;

/// Page zoom levels Ctrl + / Ctrl - step through (Firefox's).
pub const ZOOM_LEVELS: &[f32] = &[
    0.3, 0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.2, 1.33, 1.5, 1.7, 2.0, 2.4, 3.0,
];

/// The next zoom level from `current` in direction `dir` (> 0 in, < 0 out, 0 = reset to 1).
/// A level between two steps moves to the nearest step in that direction.
pub fn zoom_step(current: f32, dir: i32) -> f32 {
    const EPS: f32 = 0.005;
    if dir == 0 || !current.is_finite() {
        return 1.0;
    }
    if dir > 0 {
        ZOOM_LEVELS
            .iter()
            .copied()
            .find(|z| *z > current + EPS)
            .unwrap_or(ZOOM_LEVELS[ZOOM_LEVELS.len() - 1])
    } else {
        ZOOM_LEVELS
            .iter()
            .rev()
            .copied()
            .find(|z| *z < current - EPS)
            .unwrap_or(ZOOM_LEVELS[0])
    }
}

/// A zoom factor as the whole percent the address bar shows (110 for 1.1).
pub fn zoom_percent(z: f32) -> u16 {
    if !z.is_finite() {
        return 100;
    }
    (z * 100.0).round().clamp(1.0, 1000.0) as u16
}

/// Whether `z` is (close enough to) 100 %.
pub fn is_default_zoom(z: f32) -> bool {
    (z - 1.0).abs() < 0.005
}

/// Physical pixels of a touchpad Ctrl+scroll that make one zoom step.
pub const WHEEL_ZOOM_PX: f64 = 120.0;

/// Ctrl+wheel: how many zoom steps `dy` makes (> 0 zooms in: wheel away from the user, as in
/// other browsers). `lines`: a mouse wheel (one step per notch, whatever the notch size);
/// otherwise touchpad pixels, accumulated in `acc` until they reach [`WHEEL_ZOOM_PX`].
pub fn wheel_zoom_steps(acc: &mut f64, dy: f64, lines: bool) -> i32 {
    if !dy.is_finite() || dy == 0.0 {
        return 0;
    }
    if lines {
        *acc = 0.0;
        return if dy > 0.0 { 1 } else { -1 };
    }
    if acc.signum() != dy.signum() {
        *acc = 0.0; // direction changed
    }
    *acc += dy;
    let steps = (*acc / WHEEL_ZOOM_PX).trunc();
    *acc -= steps * WHEEL_ZOOM_PX;
    steps.clamp(-3.0, 3.0) as i32
}

/// The profile pref holding per-site zoom levels (`{"v":1,"sites":{"example.com":1.2}}`).
pub const PREF_ZOOM_SITES: &str = "zoom_sites";
/// At most this many sites are remembered (new ones beyond it are not).
pub const ZOOM_SITES_MAX: usize = 500;

/// Per-site page zoom (the registrable domain, as the cookie jar keys sites): a site zoomed
/// to anything but 100 % opens at that zoom in every tab, as in Firefox and Chrome.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ZoomSites {
    sites: BTreeMap<String, f32>,
}

impl ZoomSites {
    /// Read the pref; anything unreadable is "no sites", never an error.
    pub fn from_pref(raw: Option<&str>) -> ZoomSites {
        let mut z = ZoomSites::default();
        let Some(v) = raw.and_then(|r| serde_json::from_str::<Value>(r).ok()) else {
            return z;
        };
        if let Some(m) = v["sites"].as_object() {
            for (site, level) in m.iter().take(ZOOM_SITES_MAX) {
                if let Some(l) = level.as_f64().map(|l| l as f32) {
                    z.set(site, l);
                }
            }
        }
        z
    }

    pub fn to_pref(&self) -> String {
        let sites: serde_json::Map<String, Value> = self
            .sites
            .iter()
            .map(|(s, z)| (s.clone(), json!((*z as f64 * 100.0).round() / 100.0)))
            .collect();
        json!({ "v": 1, "sites": sites }).to_string()
    }

    /// The zoom for `site` (1.0 when none is saved).
    pub fn get(&self, site: &str) -> f32 {
        self.sites.get(site).copied().unwrap_or(1.0)
    }

    /// Remember `zoom` for `site` (100 % forgets it). Out-of-range levels are clamped to the
    /// steps' range; a site key that is empty or too long is ignored.
    pub fn set(&mut self, site: &str, zoom: f32) {
        let site = site.trim().to_ascii_lowercase();
        if site.is_empty() || site.len() > 253 || !zoom.is_finite() {
            return;
        }
        let zoom = zoom.clamp(ZOOM_LEVELS[0], ZOOM_LEVELS[ZOOM_LEVELS.len() - 1]);
        if is_default_zoom(zoom) {
            self.sites.remove(&site);
            return;
        }
        if !self.sites.contains_key(&site) && self.sites.len() >= ZOOM_SITES_MAX {
            return;
        }
        self.sites.insert(site, zoom);
    }

    pub fn len(&self) -> usize {
        self.sites.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }
}

/// What a top-level navigation to `url` should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemeAction {
    /// Servo loads it (http, https, data, blob, file, about, the internal origin...).
    Load,
    /// A well-known non-web scheme the OS has a handler for (`mailto:`, `tel:`, `sms:`): hand
    /// it to the OS, but only right after the user's own click / key press.
    OpenExternally,
    /// Any other scheme (`zoommtg:`, `msteams:`, `ms-settings:`...): not loaded and not
    /// launched (apps registered for arbitrary schemes are a known attack surface; browsers
    /// ask first and MythBrowse has no such prompt yet).
    Refuse,
}

const LOADABLE: &[&str] = &[
    "http",
    "https",
    "data",
    "blob",
    "file",
    "about",
    "javascript",
    "ws",
    "wss",
];
const EXTERNAL: &[&str] = &["mailto", "tel", "sms", "callto"];

pub fn scheme_action(url: &Url) -> SchemeAction {
    let s = url.scheme();
    if LOADABLE.contains(&s) {
        SchemeAction::Load
    } else if EXTERNAL.contains(&s) {
        SchemeAction::OpenExternally
    } else {
        SchemeAction::Refuse
    }
}

/// The URL as handed to the OS for [`SchemeAction::OpenExternally`]: only for those schemes,
/// no control characters or quotes (it goes to ShellExecute / xdg-open as one argument), at
/// most 2048 bytes.
pub fn external_target(url: &Url) -> Option<String> {
    if scheme_action(url) != SchemeAction::OpenExternally {
        return None;
    }
    let s = url.as_str();
    (s.len() <= 2048 && !s.chars().any(|c| c.is_control() || c == '"')).then(|| s.to_owned())
}

/// How long after the user's click / key press in a page a `mailto:` / `tel:` link may still
/// be handed to the OS (same window as the pop-up blocker's user activation).
pub const EXTERNAL_ACTIVATION_MS: u64 = 1_000;

/// Whether an external-scheme link may be opened now: only right after a real click or key
/// press in that page (a page cannot launch the mail app on its own, e.g. from a timer).
pub fn external_allowed(last_activation_ms: Option<u64>, now_ms: u64) -> bool {
    last_activation_ms.is_some_and(|t| now_ms >= t && now_ms - t <= EXTERNAL_ACTIVATION_MS)
}

/// The short note shown when a link's scheme is refused (`zoommtg:` -> "MythBrowse can't open
/// zoommtg: links yet"). The scheme is at most 32 characters (URL schemes are ASCII).
pub fn refused_note(url: &Url) -> String {
    let scheme: String = url.scheme().chars().take(32).collect();
    format!("MythBrowse can't open {scheme}: links yet")
}

/// The note shown when an external link was handed to the OS.
pub fn external_note(url: &Url) -> &'static str {
    match url.scheme() {
        "mailto" => "Opening your email app…",
        "tel" | "callto" => "Opening your calling app…",
        "sms" => "Opening your messaging app…",
        _ => "Opening in another app…",
    }
}

// ------------------------------------------------------------------------- find in page

/// Longest query the find bar sends (characters).
pub const FIND_QUERY_MAX: usize = 500;

/// Which way a find call moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindDir {
    /// A new or edited query: search again (the current match is the first one at or after
    /// the old current match, so typing more letters keeps the place).
    New,
    Next,
    Prev,
}

impl FindDir {
    pub fn parse(s: &str) -> Option<FindDir> {
        match s {
            "new" => Some(FindDir::New),
            "next" => Some(FindDir::Next),
            "prev" => Some(FindDir::Prev),
            _ => None,
        }
    }

    fn as_num(self) -> i32 {
        match self {
            FindDir::New => 0,
            FindDir::Next => 1,
            FindDir::Prev => -1,
        }
    }
}

/// The script that runs a find in the page (`window.__mbFind` from `js/page_input.js`): it
/// returns `"<current>/<total>"` (current is 1-based, 0 when nothing matched), or `""` when
/// the page has no find support (a page that does not run scripts).
pub fn find_script(query: &str, dir: FindDir) -> String {
    let q: String = query.chars().take(FIND_QUERY_MAX).collect();
    let q = serde_json::to_string(&q).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(function () {{ try {{ return window.__mbFind ? String(window.__mbFind({q}, {})) : ''; }} catch (e) {{ return ''; }} }})()",
        dir.as_num()
    )
}

/// Clears the find highlights (find bar closed).
pub const FIND_CLEAR_SCRIPT: &str = "(function () { try { if (window.__mbFindClear) window.__mbFindClear(); } catch (e) {} return ''; })()";

/// A find result: 1-based current match (0 = none) and the number of matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FindCount {
    pub current: u32,
    pub total: u32,
}

/// Read [`find_script`]'s answer; `None` when the page could not search.
pub fn parse_find_answer(s: &str) -> Option<FindCount> {
    let (c, t) = s.trim().trim_matches('"').split_once('/')?;
    let current: u32 = c.trim().parse().ok()?;
    let total: u32 = t.trim().parse().ok()?;
    (current <= total).then_some(FindCount { current, total })
}

/// The find bar state the chrome renders (`ChromeState::find`).
pub fn find_json(query: &str, count: Option<FindCount>, focus: u64) -> Value {
    json!({
        "query": query,
        "current": count.map(|c| c.current),
        "total": count.map(|c| c.total),
        // Bumped by Ctrl+F so the chrome focuses (and selects) the field again.
        "focus": focus,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps() {
        assert_eq!(zoom_step(1.0, 1), 1.1);
        assert_eq!(zoom_step(1.0, -1), 0.9);
        assert_eq!(zoom_step(1.33, 1), 1.5);
        assert_eq!(zoom_step(1.25, 1), 1.33);
        assert_eq!(zoom_step(1.25, -1), 1.2);
        assert_eq!(zoom_step(3.0, 1), 3.0);
        assert_eq!(zoom_step(0.3, -1), 0.3);
        assert_eq!(zoom_step(2.0, 0), 1.0);
        assert_eq!(zoom_step(f32::NAN, 1), 1.0);
        // Round trip from 100 %.
        let mut z = 1.0;
        for _ in 0..3 {
            z = zoom_step(z, 1);
        }
        for _ in 0..3 {
            z = zoom_step(z, -1);
        }
        assert_eq!(z, 1.0);
    }

    #[test]
    fn schemes() {
        let a = |s: &str| scheme_action(&Url::parse(s).unwrap());
        assert_eq!(a("https://example.com/"), SchemeAction::Load);
        assert_eq!(a("data:text/plain,hi"), SchemeAction::Load);
        assert_eq!(a("about:blank"), SchemeAction::Load);
        assert_eq!(a("mailto:a@b.com?subject=Hi"), SchemeAction::OpenExternally);
        assert_eq!(a("tel:+60112345678"), SchemeAction::OpenExternally);
        assert_eq!(a("zoommtg://zoom.us/join?confno=1"), SchemeAction::Refuse);
        assert_eq!(a("ms-msdt:/id PCWDiagnostic"), SchemeAction::Refuse);
        assert_eq!(
            external_target(&Url::parse("mailto:a@b.com").unwrap()).as_deref(),
            Some("mailto:a@b.com")
        );
        assert_eq!(
            external_target(&Url::parse("https://example.com/").unwrap()),
            None
        );
        let long = format!("mailto:a@b.com?body={}", "x".repeat(3000));
        assert_eq!(external_target(&Url::parse(&long).unwrap()), None);
    }

    #[test]
    fn zoom_misc() {
        assert_eq!(zoom_percent(1.1), 110);
        assert_eq!(zoom_percent(0.67), 67);
        assert_eq!(zoom_percent(f32::NAN), 100);
        assert!(is_default_zoom(1.0));
        assert!(is_default_zoom(1.004));
        assert!(!is_default_zoom(1.1));
    }

    #[test]
    fn wheel_zoom() {
        let mut acc = 0.0;
        assert_eq!(wheel_zoom_steps(&mut acc, 1.0, true), 1);
        assert_eq!(wheel_zoom_steps(&mut acc, -3.0, true), -1);
        assert_eq!(wheel_zoom_steps(&mut acc, 0.0, true), 0);
        // Touchpad: accumulates.
        assert_eq!(wheel_zoom_steps(&mut acc, 50.0, false), 0);
        assert_eq!(wheel_zoom_steps(&mut acc, 50.0, false), 0);
        assert_eq!(wheel_zoom_steps(&mut acc, 30.0, false), 1);
        assert!((acc - 10.0).abs() < 1e-9); // the rest carries over
        acc = 0.0;
        // Direction change resets.
        assert_eq!(wheel_zoom_steps(&mut acc, 100.0, false), 0);
        assert_eq!(wheel_zoom_steps(&mut acc, -100.0, false), 0);
        assert_eq!(wheel_zoom_steps(&mut acc, -30.0, false), -1);
        // A huge flick is capped.
        assert_eq!(wheel_zoom_steps(&mut acc, 5000.0, false), 3);
        assert_eq!(wheel_zoom_steps(&mut acc, f64::NAN, false), 0);
    }

    #[test]
    fn zoom_sites() {
        let mut z = ZoomSites::default();
        assert_eq!(z.get("example.com"), 1.0);
        z.set("Example.com", 1.2);
        assert_eq!(z.get("example.com"), 1.2);
        z.set("big.test", 9.0);
        assert_eq!(z.get("big.test"), 3.0);
        z.set("", 1.5);
        z.set("nan.test", f32::NAN);
        assert_eq!(z.len(), 2);
        let back = ZoomSites::from_pref(Some(&z.to_pref()));
        assert_eq!(back, z);
        z.set("example.com", 1.0);
        assert_eq!(z.get("example.com"), 1.0);
        assert_eq!(z.len(), 1);
        assert!(ZoomSites::from_pref(Some("not json")).is_empty());
        assert!(ZoomSites::from_pref(None).is_empty());
        assert!(ZoomSites::from_pref(Some(r#"{"sites":{"a.test":"x"}}"#)).is_empty());
        let mut many = ZoomSites::default();
        for i in 0..ZOOM_SITES_MAX + 10 {
            many.set(&format!("s{i}.test"), 1.1);
        }
        assert_eq!(many.len(), ZOOM_SITES_MAX);
    }

    #[test]
    fn external_gate_and_notes() {
        assert!(external_allowed(Some(1000), 1500));
        assert!(!external_allowed(Some(1000), 2500));
        assert!(!external_allowed(None, 1500));
        assert!(!external_allowed(Some(2000), 1500));
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(
            refused_note(&u("zoommtg://zoom.us/join")),
            "MythBrowse can't open zoommtg: links yet"
        );
        assert_eq!(external_note(&u("mailto:a@b.c")), "Opening your email app…");
        assert_eq!(external_note(&u("tel:123")), "Opening your calling app…");
    }

    #[test]
    fn find_protocol() {
        assert_eq!(FindDir::parse("next"), Some(FindDir::Next));
        assert_eq!(FindDir::parse("x"), None);
        let s = find_script("a\"b</script>", FindDir::Prev);
        assert!(s.contains(r#"window.__mbFind("a\"b</script>", -1)"#), "{s}");
        let long = "x".repeat(FIND_QUERY_MAX + 50);
        assert!(find_script(&long, FindDir::New).len() < FIND_QUERY_MAX + 200);
        assert_eq!(
            parse_find_answer("2/7"),
            Some(FindCount {
                current: 2,
                total: 7
            })
        );
        assert_eq!(parse_find_answer("\"0/0\""), Some(FindCount::default()));
        assert_eq!(parse_find_answer(""), None);
        assert_eq!(parse_find_answer("8/7"), None);
        assert_eq!(parse_find_answer("a/b"), None);
        let j = find_json(
            "hi",
            Some(FindCount {
                current: 1,
                total: 3,
            }),
            2,
        );
        assert_eq!(j["total"], 3);
        assert_eq!(j["focus"], 2);
        assert!(find_json("", None, 0)["total"].is_null());
    }
}
