// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
//! Page input Servo 0.1.4 does not do itself, as a user script in every web page
//! ([`PAGE_INPUT_SCRIPT`], `js/page_input.js`):
//!
//! - **Text selection** in page text (drag, double click = word, triple click = block,
//!   Shift+click extends, Ctrl+A) with a painted highlight, the DOM `Selection` kept in step,
//!   and **copy** (Ctrl+C / the context menu's Copy put the selected text on the clipboard
//!   through the trusted `copy` event). Servo only selects inside form fields, has no
//!   `caretPositionFromPoint`, and its `Range.getClientRects` returns whole text nodes, so
//!   the script hit-tests text itself (text-node line boxes + canvas text metrics).
//! - **Links**: Ctrl+click / middle click open a background tab, Ctrl+Shift+click /
//!   Shift+middle click a foreground one. The page script cancels the click and reports it
//!   on the console channel ([`parse_open_request`]); the shell honours it only right after
//!   a real mouse click in that page ([`OpenGate`]), so a page cannot open tabs by logging.
//! - **Wheel listeners** (v0.4.2): the shell scrolls on Servo's compositor path, which fires
//!   no DOM `wheel` events. The script reports a page (or frame) that registers a wheel
//!   listener able to call `preventDefault` ([`parse_wheel_report`]); for such pages the
//!   shell asks [`wheel_blocking_script`] at the start of a gesture and sends that gesture
//!   as DOM wheel events when the pointer is over such an element ([`WheelAnswer`]).
//! - **Middle-click autoscroll**: a middle press that is not on a link / form field / media,
//!   in a page that can scroll, is reported ([`is_autoscroll_request`]); the shell honours
//!   it only right after a real middle press there (same gate as links).

use serde::Deserialize;

/// The user script (document start, every web page and frame).
pub const PAGE_INPUT_SCRIPT: &str = include_str!("js/page_input.js");

/// Console prefix of a link-open request from [`PAGE_INPUT_SCRIPT`].
pub const OPEN_MARKER: &str = "\u{1}mythbrowse:open ";

/// A link the user Ctrl- or middle-clicked.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OpenRequest {
    pub href: String,
    /// Ctrl+Shift+click / Shift+middle click: switch to the new tab.
    #[serde(default)]
    pub foreground: bool,
}

/// Parse a console line from [`PAGE_INPUT_SCRIPT`]; `None` for any other line or a URL that
/// is not http(s).
pub fn parse_open_request(message: &str) -> Option<OpenRequest> {
    let json = message.strip_prefix(OPEN_MARKER)?;
    let req: OpenRequest = serde_json::from_str(json).ok()?;
    let url = url::Url::parse(&req.href).ok()?;
    (matches!(url.scheme(), "http" | "https") && req.href.len() <= 8192).then_some(req)
}

/// Console prefix of a selection report (the Myth selection pill): box + length, no text.
pub const SEL_MARKER: &str = "\u{1}mythbrowse:sel ";

/// The page's text selection as the page script reported it: its length in characters and
/// bounding box in viewport CSS px (`len == 0`: no selection any more).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Default)]
pub struct SelReport {
    pub len: u32,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default)]
    pub w: f32,
    #[serde(default)]
    pub h: f32,
}

/// Parse a selection report; `None` for other lines or nonsense values.
pub fn parse_sel_report(message: &str) -> Option<SelReport> {
    let json = message.strip_prefix(SEL_MARKER)?;
    if json.len() > 256 {
        return None;
    }
    let r: SelReport = serde_json::from_str(json).ok()?;
    let sane = |v: f32| v.is_finite() && v.abs() <= 100_000.0;
    (sane(r.x) && sane(r.y) && sane(r.w) && sane(r.h) && r.w >= 0.0 && r.h >= 0.0).then_some(r)
}

/// Console prefix of a wheel-listener report.
pub const WHEEL_MARKER: &str = "\u{1}mythbrowse:wheel ";

/// A document registered a wheel listener that may block scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct WheelReport {
    /// From the top-level document (else a frame).
    #[serde(default)]
    pub top: bool,
}

/// Parse a wheel-listener report; `None` for other lines.
pub fn parse_wheel_report(message: &str) -> Option<WheelReport> {
    let json = message.strip_prefix(WHEEL_MARKER)?;
    if json.len() > 64 {
        return None;
    }
    serde_json::from_str(json).ok()
}

/// What `__mbWheelBlockingAt` said about the point under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAnswer {
    /// No blocking listener there: the compositor (fast) path.
    No,
    /// An element with a blocking listener: DOM wheel events for this gesture.
    Yes,
    /// An iframe: blocking only if a frame reported a listener.
    Frame,
}

/// The query for the start of a wheel gesture at `(x, y)` (viewport CSS px).
pub fn wheel_blocking_script(x: f64, y: f64) -> String {
    format!(
        "(function () {{ try {{ return window.__mbWheelBlockingAt ? String(window.__mbWheelBlockingAt({x:.1}, {y:.1})) : 'no'; }} catch (e) {{ return 'no'; }} }})()"
    )
}

/// Read [`wheel_blocking_script`]'s answer (anything unexpected counts as `No`).
pub fn parse_wheel_answer(s: &str) -> WheelAnswer {
    match s.trim().trim_matches('"') {
        "yes" => WheelAnswer::Yes,
        "frame" => WheelAnswer::Frame,
        _ => WheelAnswer::No,
    }
}

/// Console prefix of an autoscroll request (middle press on a scrollable page).
pub const AUTOSCROLL_MARKER: &str = "\u{1}mythbrowse:autoscroll ";

pub fn is_autoscroll_request(message: &str) -> bool {
    message
        .strip_prefix(AUTOSCROLL_MARKER)
        .is_some_and(|rest| rest.trim() == "{}")
}

/// Console prefix of a click on a link whose scheme the engine cannot load (v0.4.2).
pub const EXTERNAL_MARKER: &str = "\u{1}mythbrowse:external ";

#[derive(Deserialize)]
struct ExternalRequest {
    href: String,
}

/// Parse an external-link report: the link's URL when it parses and is not something the
/// engine loads itself (http(s), data, blob...); `None` otherwise. What happens to it (OS
/// handler, refused) is `browse::scheme_action`'s call, and only right after a real click.
pub fn parse_external_request(message: &str) -> Option<url::Url> {
    let json = message.strip_prefix(EXTERNAL_MARKER)?;
    if json.len() > 9000 {
        return None;
    }
    let req: ExternalRequest = serde_json::from_str(json).ok()?;
    let url = url::Url::parse(&req.href).ok()?;
    (crate::browse::scheme_action(&url) != crate::browse::SchemeAction::Load).then_some(url)
}

/// How long after a real mouse button press in a page an open request is honoured.
pub const OPEN_GATE_MS: u64 = 1500;

/// User-activation gate for open requests: armed by a real Ctrl+left or middle mouse press
/// in the page, consumed by one request.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenGate {
    armed_at_ms: Option<u64>,
}

impl OpenGate {
    /// A mouse press that may open a link (middle button, or left with Ctrl / Cmd).
    pub fn arm(&mut self, now_ms: u64) {
        self.armed_at_ms = Some(now_ms);
    }

    /// Whether a request arriving now is allowed; disarms the gate when it is.
    pub fn take(&mut self, now_ms: u64) -> bool {
        match self.armed_at_ms {
            Some(t) if now_ms.saturating_sub(t) <= OPEN_GATE_MS => {
                self.armed_at_ms = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_requests() {
        let r = parse_open_request(
            "\u{1}mythbrowse:open {\"href\":\"https://example.com/a?b=1\",\"foreground\":true}",
        )
        .unwrap();
        assert_eq!(r.href, "https://example.com/a?b=1");
        assert!(r.foreground);
        let r = parse_open_request("\u{1}mythbrowse:open {\"href\":\"http://x.test/\"}").unwrap();
        assert!(!r.foreground);
        assert!(
            parse_open_request("\u{1}mythbrowse:open {\"href\":\"javascript:alert(1)\"}").is_none()
        );
        assert!(
            parse_open_request("\u{1}mythbrowse:open {\"href\":\"file:///etc/passwd\"}").is_none()
        );
        assert!(parse_open_request("\u{1}mythbrowse:open nope").is_none());
        assert!(parse_open_request("hello").is_none());
        let long = format!(
            "\u{1}mythbrowse:open {{\"href\":\"https://e.test/{}\"}}",
            "a".repeat(9000)
        );
        assert!(parse_open_request(&long).is_none());
    }

    #[test]
    fn gate_needs_a_recent_click_and_allows_one_tab() {
        let mut g = OpenGate::default();
        assert!(!g.take(1000), "no click, no tab");
        g.arm(1000);
        assert!(g.take(1000 + OPEN_GATE_MS));
        assert!(!g.take(1001 + OPEN_GATE_MS), "one tab per click");
        g.arm(5000);
        assert!(!g.take(5001 + OPEN_GATE_MS), "too late");
    }

    #[test]
    fn parses_selection_reports() {
        let r = parse_sel_report(
            "\u{1}mythbrowse:sel {\"len\":42,\"x\":10,\"y\":20,\"w\":300,\"h\":18}",
        )
        .unwrap();
        assert_eq!((r.len, r.x, r.w), (42, 10.0, 300.0));
        assert_eq!(
            parse_sel_report("\u{1}mythbrowse:sel {\"len\":0}")
                .unwrap()
                .len,
            0
        );
        assert!(parse_sel_report("\u{1}mythbrowse:sel {\"len\":1,\"w\":-5}").is_none());
        assert!(parse_sel_report("\u{1}mythbrowse:sel {\"len\":1,\"x\":1e30}").is_none());
        assert!(parse_sel_report("\u{1}mythbrowse:sel {\"text\":\"secret\"}").is_none());
        assert!(parse_sel_report("hello").is_none());
        assert!(PAGE_INPUT_SCRIPT.contains("\\u0001mythbrowse:sel "));
    }

    #[test]
    fn wheel_reports_and_answers() {
        assert_eq!(
            parse_wheel_report("\u{1}mythbrowse:wheel {\"top\":true}"),
            Some(WheelReport { top: true })
        );
        assert_eq!(
            parse_wheel_report("\u{1}mythbrowse:wheel {}"),
            Some(WheelReport { top: false })
        );
        assert_eq!(parse_wheel_report("mythbrowse:wheel {}"), None);
        assert_eq!(parse_wheel_answer("\"yes\""), WheelAnswer::Yes);
        assert_eq!(parse_wheel_answer("frame"), WheelAnswer::Frame);
        assert_eq!(parse_wheel_answer("no"), WheelAnswer::No);
        assert_eq!(parse_wheel_answer("garbage"), WheelAnswer::No);
        let q = wheel_blocking_script(10.0, 20.5);
        assert!(q.contains("__mbWheelBlockingAt(10.0, 20.5)"), "{q}");
        assert!(PAGE_INPUT_SCRIPT.contains("\\u0001mythbrowse:wheel "));
        assert!(PAGE_INPUT_SCRIPT.contains("window.__mbWheelBlockingAt ="));
    }

    #[test]
    fn autoscroll_requests() {
        assert!(is_autoscroll_request("\u{1}mythbrowse:autoscroll {}"));
        assert!(!is_autoscroll_request(
            "\u{1}mythbrowse:autoscroll {\"x\":1}"
        ));
        assert!(!is_autoscroll_request("mythbrowse:autoscroll {}"));
        assert!(PAGE_INPUT_SCRIPT.contains("\\u0001mythbrowse:autoscroll "));
    }

    #[test]
    fn script_reports_on_the_marker() {
        assert!(PAGE_INPUT_SCRIPT.contains("\\u0001mythbrowse:open "));
        assert_eq!(OPEN_MARKER, "\u{1}mythbrowse:open ");
        assert!(PAGE_INPUT_SCRIPT.contains("__mbPageInput"));
    }

    #[test]
    fn external_links_and_find() {
        let m = |h: &str| format!("{EXTERNAL_MARKER}{{\"href\":\"{h}\"}}");
        assert_eq!(
            parse_external_request(&m("mailto:a@b.com")).map(|u| u.to_string()),
            Some("mailto:a@b.com".into())
        );
        assert!(parse_external_request(&m("zoommtg://zoom.us/join")).is_some());
        assert!(parse_external_request(&m("https://example.com/")).is_none());
        assert!(parse_external_request(&m("not a url")).is_none());
        assert!(parse_external_request("mailto:a@b.com").is_none());
        assert!(PAGE_INPUT_SCRIPT.contains("\\u0001mythbrowse:external "));
        assert!(PAGE_INPUT_SCRIPT.contains("window.__mbFind = function"));
        assert!(PAGE_INPUT_SCRIPT.contains("window.__mbFindClear = function"));
    }
}
