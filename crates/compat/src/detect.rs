// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Breakage detector.
//!
//! Cost model: nothing runs while a page is in use. The bootstrap user script records errors
//! passively (event listeners only); Servo's console messages are counted by the shell as
//! they arrive ([`ConsoleStats`]); and the [`PROBE_SCRIPT`] is evaluated once when the page
//! finishes loading (plus when the user asks to re-check). [`assess`] turns that into a
//! 0-100 score, a level and human-readable findings.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// The probe; `__MB_KEY__` is replaced with the session key (see [`crate::Compat::probe_script`]).
pub(crate) const PROBE_TEMPLATE: &str = include_str!("js/probe.js");
/// The human-verification detector: a JS function expression `(function (doc) {...})` that
/// returns the matched phrase / challenge host or `null`. Shared by the probe and by
/// `mythbrowse-ai`'s hand-off check (which includes the same file). Detection only.
pub const CHALLENGE_FN: &str = include_str!("js/challenge.js");

/// The probe with the challenge detector filled in (`__MB_KEY__` still to replace).
pub(crate) fn probe_template() -> String {
    PROBE_TEMPLATE.replace("__CHALLENGE__", CHALLENGE_FN.trim())
}
/// Structure-only DOM summary for AI repair.
pub const DOM_SUMMARY_SCRIPT: &str = include_str!("js/dom_summary.js");

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct JsError {
    #[serde(default)]
    pub m: String,
    #[serde(default)]
    pub s: String,
    #[serde(default)]
    pub l: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Layout {
    pub vw: f64,
    pub vh: f64,
    pub doc_h: f64,
    pub body_h: f64,
    pub text_len: u64,
    pub elements: u64,
    pub media: u64,
    pub samples: u32,
    pub blank_samples: u32,
    pub overflow_hidden: bool,
}

/// What the probe returns.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ProbeReport {
    pub url: String,
    pub ready: String,
    pub bootstrap: bool,
    pub errors: Vec<JsError>,
    pub error_count: u32,
    pub rejection_count: u32,
    pub resource_failures: Vec<String>,
    pub resource_failure_count: u32,
    pub missing: Vec<String>,
    pub css_missing: Vec<String>,
    pub api: Vec<String>,
    pub ua_block: Option<String>,
    /// A bot / security check the site shows (captcha, "Just a moment", IP block).
    pub bot_check: Option<String>,
    pub rules: Vec<String>,
    pub failed_rules: Vec<String>,
    pub mode: Option<String>,
    pub layout: Layout,
}

impl ProbeReport {
    pub fn parse(json: &str) -> Result<ProbeReport, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Servo console messages for one page (counted by the shell as they arrive).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ConsoleStats {
    pub errors: u32,
    pub warnings: u32,
    /// Last few error / warning lines, clipped.
    pub recent: VecDeque<String>,
}

const CONSOLE_KEEP: usize = 12;

impl ConsoleStats {
    /// `level`: "error", "warn", or anything else (ignored).
    pub fn record(&mut self, level: &str, message: &str) {
        match level {
            "error" => self.errors = self.errors.saturating_add(1),
            "warn" => self.warnings = self.warnings.saturating_add(1),
            _ => return,
        }
        if self.recent.len() == CONSOLE_KEEP {
            self.recent.pop_front();
        }
        self.recent.push_back(clip(message, 240));
    }
}

pub(crate) fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Degraded,
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable machine code (`blank`, `ua_block`, `js_errors`, `unsupported_api`, ...).
    pub code: &'static str,
    pub severity: Severity,
    /// One line for people.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assessment {
    pub score: u8,
    pub level: Level,
    pub findings: Vec<Finding>,
    /// The site is showing a bot / security check: the page is gated, not broken here, so
    /// the score says nothing about this engine and repair is not offered.
    pub blocked: bool,
}

impl Assessment {
    /// Whether "Ask Myth to fix" is worth offering prominently.
    pub fn needs_repair(&self) -> bool {
        self.level != Level::Ok && !self.blocked
    }
}

/// Score a probe report plus console counts. Weights are first guesses; the top-sites compat
/// job (`ci/compat/`) exists to tune them on real sites.
pub fn assess(report: &ProbeReport, console: &ConsoleStats) -> Assessment {
    if let Some(phrase) = &report.bot_check {
        return Assessment {
            score: 0,
            level: Level::Ok,
            findings: vec![Finding {
                code: "bot_check",
                severity: Severity::Info,
                text: format!(
                    "The site is showing a security check (\u{201c}{phrase}\u{201d}), not a browser problem"
                ),
            }],
            blocked: true,
        };
    }
    // Tuned on the 2026-10-09 top-sites run: script errors and failed subresources are
    // common on pages that render fine (ads, trackers, analytics), so they count for less
    // when the page visibly has content.
    let l0 = &report.layout;
    let rendered = l0.text_len >= 500 && (l0.samples == 0 || l0.blank_samples * 4 < l0.samples);
    let soften = |p: u32| if rendered { p / 2 } else { p };
    let mut score: u32 = 0;
    let mut findings = Vec::new();
    let mut add = |code, severity, text: String, points: u32| {
        score += points;
        findings.push(Finding {
            code,
            severity,
            text,
        });
    };
    let l = &report.layout;
    let empty_layout = l.body_h < 1.0 && l.doc_h <= l.vh + 1.0;
    let little_content = l.text_len < 40 && l.media == 0;
    if (empty_layout || little_content) && l.elements < 400 {
        add(
            "blank",
            Severity::Bad,
            if empty_layout {
                "The page body has no height: nothing is laid out".to_owned()
            } else {
                "The page shows almost no text or images".to_owned()
            },
            55,
        );
    } else if l.samples > 0 && l.blank_samples * 10 >= l.samples * 9 && l.text_len < 400 {
        add(
            "mostly_blank",
            Severity::Warn,
            "Most of the visible area is empty".to_owned(),
            20,
        );
    }
    if let Some(phrase) = &report.ua_block {
        add(
            "ua_block",
            Severity::Bad,
            format!("The site says the browser is not supported (\u{201c}{phrase}\u{201d})"),
            60,
        );
    }
    let uncaught = report.error_count + report.rejection_count;
    if uncaught > 0 {
        add(
            "js_errors",
            if uncaught >= 3 {
                Severity::Bad
            } else {
                Severity::Warn
            },
            format!(
                "{uncaught} uncaught script error{}",
                if uncaught == 1 { "" } else { "s" }
            ),
            soften(uncaught.min(10) * 2),
        );
    }
    if !report.api.is_empty() {
        add(
            "unsupported_api",
            Severity::Bad,
            format!(
                "Scripts call APIs this engine lacks: {}",
                report.api.join(", ")
            ),
            (report.api.len() as u32).min(4) * 12,
        );
    }
    if report.resource_failure_count > 0 {
        add(
            "resources",
            Severity::Warn,
            format!(
                "{} resource{} failed to load",
                report.resource_failure_count,
                if report.resource_failure_count == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            soften(report.resource_failure_count.min(20)),
        );
    }
    if console.errors > 0 {
        add(
            "console",
            Severity::Info,
            format!(
                "{} console error{}",
                console.errors,
                if console.errors == 1 { "" } else { "s" }
            ),
            console.errors.min(10),
        );
    }
    if !report.failed_rules.is_empty() {
        add(
            "rule_failed",
            Severity::Warn,
            format!("Compat rules threw: {}", report.failed_rules.join(", ")),
            5,
        );
    }
    if l.overflow_hidden && l.doc_h > l.vh * 1.5 {
        add(
            "scroll_locked",
            Severity::Warn,
            "Scrolling is locked (overflow hidden on a long page)".to_owned(),
            10,
        );
    }
    if !report.missing.is_empty() {
        add(
            "missing_api",
            Severity::Info,
            format!("Not built into this engine: {}", report.missing.join(", ")),
            0,
        );
    }
    if !report.css_missing.is_empty() {
        add(
            "css_missing",
            Severity::Info,
            format!("CSS not supported: {}", report.css_missing.join(", ")),
            0,
        );
    }
    let score = score.min(100) as u8;
    let level = match score {
        0..=19 => Level::Ok,
        20..=49 => Level::Degraded,
        _ => Level::Broken,
    };
    Assessment {
        score,
        level,
        findings,
        blocked: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> ProbeReport {
        ProbeReport {
            bootstrap: true,
            layout: Layout {
                vw: 1280.0,
                vh: 800.0,
                doc_h: 3000.0,
                body_h: 3000.0,
                text_len: 5000,
                elements: 900,
                media: 12,
                samples: 16,
                blank_samples: 2,
                overflow_hidden: false,
            },
            ..Default::default()
        }
    }

    #[test]
    fn healthy_page_is_ok() {
        let a = assess(&healthy(), &ConsoleStats::default());
        assert_eq!(a.level, Level::Ok);
        assert_eq!(a.score, 0);
    }

    #[test]
    fn blank_page_is_broken() {
        let mut r = healthy();
        r.layout.body_h = 0.0;
        r.layout.doc_h = 800.0;
        r.layout.text_len = 0;
        r.layout.media = 0;
        r.layout.elements = 30;
        r.error_count = 2;
        let a = assess(&r, &ConsoleStats::default());
        assert_eq!(a.level, Level::Broken);
        assert!(a.findings.iter().any(|f| f.code == "blank"));
    }

    #[test]
    fn ua_block_is_broken() {
        let mut r = healthy();
        r.layout.text_len = 300;
        r.ua_block = Some("unsupported browser".into());
        assert_eq!(assess(&r, &ConsoleStats::default()).level, Level::Broken);
    }

    #[test]
    fn unsupported_api_and_errors_degrade() {
        let mut r = healthy();
        r.error_count = 2;
        r.api = vec!["requestIdleCallback".into(), "adoptedStyleSheets".into()];
        let a = assess(&r, &ConsoleStats::default());
        assert_eq!(a.level, Level::Degraded);
        assert_eq!(a.score, 26);
    }

    #[test]
    fn noise_on_a_rendered_page_stays_ok() {
        // github.com on 2026-10-09: renders perfectly, two script errors, one missing API.
        let mut r = healthy();
        r.error_count = 2;
        r.api = vec!["adoptedStyleSheets".into()];
        r.resource_failure_count = 2;
        let a = assess(&r, &ConsoleStats::default());
        assert_eq!((a.score, a.level), (15, Level::Ok));
        // Lots of failed subresources alone (apple.com: 51) do not make a rendered page bad.
        let mut r = healthy();
        r.resource_failure_count = 51;
        assert_eq!(assess(&r, &ConsoleStats::default()).level, Level::Ok);
    }

    #[test]
    fn bot_check_is_not_scored() {
        let mut r = healthy();
        r.layout.text_len = 120;
        r.error_count = 9;
        r.bot_check = Some("just a moment".into());
        let a = assess(&r, &ConsoleStats::default());
        assert!(a.blocked);
        assert_eq!((a.score, a.level), (0, Level::Ok));
        assert!(!a.needs_repair());
        assert_eq!(a.findings[0].code, "bot_check");
    }

    #[test]
    fn informational_findings_do_not_score() {
        let mut r = healthy();
        r.missing = vec!["Element.animate".into()];
        r.css_missing = vec!["text-wrap:balance".into()];
        let a = assess(&r, &ConsoleStats::default());
        assert_eq!((a.score, a.level), (0, Level::Ok));
        assert_eq!(a.findings.len(), 2);
    }

    #[test]
    fn console_keeps_recent_lines() {
        let mut c = ConsoleStats::default();
        for i in 0..20 {
            c.record("error", &format!("e{i}"));
        }
        c.record("log", "ignored");
        assert_eq!(c.errors, 20);
        assert_eq!(c.recent.len(), CONSOLE_KEEP);
        assert_eq!(c.recent.back().unwrap(), "e19");
    }

    #[test]
    fn probe_embeds_the_shared_challenge_detector() {
        let p = probe_template();
        assert!(!p.contains("__CHALLENGE__"));
        assert!(p.contains("challenges.cloudflare.com"));
        assert!(p.contains("google.com/recaptcha"));
        // A function expression, so both the probe and the agent can call it with `document`.
        assert!(CHALLENGE_FN.trim_start().starts_with("(function (doc)"));
        assert!(CHALLENGE_FN.trim_end().ends_with("})"));
        // Detection only: nothing in it clicks, types, dispatches events or loads anything.
        for bad in [
            ".click(",
            "dispatchEvent",
            "postMessage",
            "fetch(",
            "XMLHttpRequest",
            "setTimeout",
            ".submit(",
        ] {
            assert!(
                !CHALLENGE_FN.contains(bad),
                "challenge.js must not use {bad}"
            );
        }
    }

    #[test]
    fn parses_probe_json() {
        let json = r#"{"url":"https://a.com/","bootstrap":true,"errors":[{"m":"x","s":"","l":1}],
            "error_count":1,"api":["IntersectionObserver"],"layout":{"vw":10,"vh":10},"extra":1}"#;
        let r = ProbeReport::parse(json).unwrap();
        assert_eq!(r.api, vec!["IntersectionObserver"]);
        assert_eq!(r.errors[0].l, 1);
        assert_eq!(r.layout.vw, 10.0);
    }
}
