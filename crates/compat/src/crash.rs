// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Crash-once reporting for web content crashes (`WebViewDelegate::notify_crashed`).
//!
//! A page that crashes Servo's script thread usually crashes it again on every reload, and
//! some sites reload themselves. The shell reports the first crash of a host in a browser
//! session (log with the backtrace + a one-line notice in that host's compat panel) and only
//! counts the repeats, so a crash loop never floods the log or the user. Nothing leaves the
//! machine.

use std::collections::BTreeMap;

/// What the shell should do with one crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    /// First crash of this host in this session: report it.
    pub first: bool,
    /// Crashes of this host in this session, this one included.
    pub count: u32,
    /// Short, single-line reason for the panel (no file paths, at most 160 chars).
    pub summary: String,
    /// A known engine bug this crash matches, if any (shown with the notice).
    pub known: Option<&'static str>,
}

/// Per-session crash counts by host.
#[derive(Debug, Default)]
pub struct CrashLog {
    seen: BTreeMap<String, u32>,
}

/// Crash signatures we have reproduced, with what they mean for the user.
const KNOWN: &[(&str, &str)] = &[
    (
        "can_run_script",
        "known Servo bug: script was run in a frame that may not run script (MythBrowse carries a fix; report the site if you still see this)",
    ),
    (
        "already mutably borrowed",
        "known Servo bug class: re-entrant DOM borrow (please report the site)",
    ),
    ("out of memory", "the page ran out of memory"),
];

impl CrashLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a crash of `host` with Servo's panic `reason`.
    pub fn record(&mut self, host: &str, reason: &str) -> CrashReport {
        let key = host.trim().to_ascii_lowercase();
        let n = self.seen.entry(key).or_insert(0);
        *n = n.saturating_add(1);
        CrashReport {
            first: *n == 1,
            count: *n,
            summary: summarize(reason),
            known: known(reason),
        }
    }

    /// Crashes of `host` so far in this session.
    pub fn count(&self, host: &str) -> u32 {
        self.seen
            .get(&host.trim().to_ascii_lowercase())
            .copied()
            .unwrap_or(0)
    }
}

/// The known-bug note for a panic reason, if it matches one.
pub fn known(reason: &str) -> Option<&'static str> {
    let lower = reason.to_ascii_lowercase();
    KNOWN
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map(|(_, note)| *note)
}

/// First non-empty line of a panic reason, without ` (thread …, at path:line)` / `at path`
/// tails, at most 160 characters.
pub fn summarize(reason: &str) -> String {
    let line = reason
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown crash");
    let mut s = line;
    for cut in [" (thread ", " at /", " at components/", " at C:\\"] {
        if let Some(i) = s.find(cut) {
            s = &s[..i];
        }
    }
    let s = s.trim();
    if s.chars().count() > 160 {
        let mut out: String = s.chars().take(159).collect();
        out.push('\u{2026}');
        out
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_crash_per_host_is_reported_once() {
        let mut log = CrashLog::new();
        let a = log.record("www.amazon.com", "assertion failed: self.can_run_script()");
        assert!(a.first);
        assert_eq!(a.count, 1);
        let b = log.record("WWW.AMAZON.COM", "assertion failed: self.can_run_script()");
        assert!(!b.first);
        assert_eq!(b.count, 2);
        let c = log.record("zoom.us", "boom");
        assert!(c.first);
        assert_eq!(log.count("www.amazon.com"), 2);
        assert_eq!(log.count("example.com"), 0);
    }

    #[test]
    fn summary_is_one_short_line_without_paths() {
        assert_eq!(
            summarize(
                "\n assertion failed: self.can_run_script() (thread Script(1,1), at components/script/dom/globalscope.rs:2880)\nmore"
            ),
            "assertion failed: self.can_run_script()"
        );
        assert_eq!(summarize("x at /home/u/servo/src/a.rs:1"), "x");
        assert_eq!(summarize(""), "unknown crash");
        let long = "a".repeat(400);
        assert_eq!(summarize(&long).chars().count(), 160);
    }

    #[test]
    fn known_signatures() {
        assert!(known("assertion failed: self.can_run_script()").is_some());
        assert!(known("already mutably borrowed: BorrowError").is_some());
        assert!(known("index out of bounds").is_none());
        let mut log = CrashLog::new();
        assert!(
            log.record("a.com", "Already Mutably Borrowed")
                .known
                .is_some()
        );
    }
}
