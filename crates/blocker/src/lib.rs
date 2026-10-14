// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Built-in content blocker, Phase 0: a domain blocklist matcher.
//!
//! A rule like `doubleclick.net` blocks that host and every subdomain of it
//! (`ad.doubleclick.net`), but not lookalikes (`notdoubleclick.net`).
//! Allow rules (`@@||example.com^`) win over block rules.
//!
//! Accepted list syntax, one rule per line:
//! - plain domains: `example.com`
//! - hosts-file lines: `0.0.0.0 example.com` / `127.0.0.1 example.com`
//! - the domain-anchor subset of Adblock syntax: `||example.com^` and `@@||example.com^`
//! - comments starting with `#` or `!`, and blank lines, are ignored.
//!
//! Anything else (paths, cosmetic filters, regexes) is skipped and counted, so the
//! full Adblock engine can replace this later without changing callers.

use std::collections::HashSet;

/// Outcome of a lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Block,
}

/// Counts from parsing a list.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ParseStats {
    pub blocked: usize,
    pub allowed: usize,
    pub skipped: usize,
}

/// A domain blocklist with an allowlist for exceptions.
#[derive(Debug, Default, Clone)]
pub struct Blocklist {
    block: HashSet<String>,
    allow: HashSet<String>,
}

impl Blocklist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a list in any of the supported syntaxes.
    pub fn parse(text: &str) -> (Self, ParseStats) {
        let mut list = Self::new();
        let stats = list.extend_from_str(text);
        (list, stats)
    }

    /// Add rules from text to this list, returning what was added.
    pub fn extend_from_str(&mut self, text: &str) -> ParseStats {
        let mut stats = ParseStats::default();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            match parse_rule(line) {
                Some((Verdict::Block, domain)) => {
                    self.block.insert(domain);
                    stats.blocked += 1;
                }
                Some((Verdict::Allow, domain)) => {
                    self.allow.insert(domain);
                    stats.allowed += 1;
                }
                None => stats.skipped += 1,
            }
        }
        stats
    }

    /// Block a domain and its subdomains. Returns false if the domain is invalid.
    pub fn block_domain(&mut self, domain: &str) -> bool {
        normalize_domain(domain)
            .map(|d| self.block.insert(d))
            .is_some()
    }

    /// Exempt a domain and its subdomains. Returns false if the domain is invalid.
    pub fn allow_domain(&mut self, domain: &str) -> bool {
        normalize_domain(domain)
            .map(|d| self.allow.insert(d))
            .is_some()
    }

    pub fn len(&self) -> usize {
        self.block.len()
    }

    pub fn is_empty(&self) -> bool {
        self.block.is_empty()
    }

    /// Decide for a host name (no scheme, no port, no path).
    pub fn check_host(&self, host: &str) -> Verdict {
        let Some(host) = normalize_domain(host) else {
            return Verdict::Allow;
        };
        if matches_suffix(&self.allow, &host) {
            return Verdict::Allow;
        }
        if matches_suffix(&self.block, &host) {
            Verdict::Block
        } else {
            Verdict::Allow
        }
    }

    pub fn is_blocked(&self, host: &str) -> bool {
        self.check_host(host) == Verdict::Block
    }
}

/// True if `host` or any parent domain of it (on a label boundary) is in `set`.
fn matches_suffix(set: &HashSet<String>, host: &str) -> bool {
    if set.is_empty() {
        return false;
    }
    let mut candidate = host;
    loop {
        if set.contains(candidate) {
            return true;
        }
        match candidate.find('.') {
            Some(i) => candidate = &candidate[i + 1..],
            None => return false,
        }
    }
}

fn parse_rule(line: &str) -> Option<(Verdict, String)> {
    // Adblock domain anchors.
    if let Some(rest) = line.strip_prefix("@@||") {
        return adblock_domain(rest).map(|d| (Verdict::Allow, d));
    }
    if let Some(rest) = line.strip_prefix("||") {
        return adblock_domain(rest).map(|d| (Verdict::Block, d));
    }
    // Cosmetic / element-hiding filters (`example.com##.ad`, `#@#`, `#?#`, `#$#`) are not
    // network rules; skip them rather than misreading the domain in front of them.
    if ["##", "#@#", "#?#", "#$#"].iter().any(|m| line.contains(m)) {
        return None;
    }
    // Strip trailing inline comments for hosts/plain formats (`domain # note`).
    let line = match line.find('#') {
        Some(i) => line[..i].trim(),
        None => line,
    };
    let mut parts = line.split_whitespace();
    let first = parts.next()?;
    let domain = match parts.next() {
        // hosts-file format: "<ip> <domain>"
        Some(second) if is_sink_ip(first) => second,
        Some(_) => return None,
        None => first,
    };
    if parts.next().is_some() {
        return None;
    }
    let d = normalize_domain(domain)?;
    // Hosts files often list "localhost" etc.; never block local names.
    if d == "localhost" || d == "local" || d == "broadcasthost" {
        return None;
    }
    Some((Verdict::Block, d))
}

fn adblock_domain(rest: &str) -> Option<String> {
    // Only the pure domain form `example.com^`. Rules with `$options` (third-party, etc.)
    // or paths change semantics, so they are skipped until the full engine lands.
    let rest = rest.strip_suffix('^').unwrap_or(rest);
    if rest.contains(['/', '$', '*', '^', '|']) {
        return None;
    }
    normalize_domain(rest)
}

fn is_sink_ip(s: &str) -> bool {
    matches!(s, "0.0.0.0" | "127.0.0.1" | "::" | "::1" | "::0")
}

/// Lowercase, strip a trailing dot, and validate DNS-name characters.
fn normalize_domain(input: &str) -> Option<String> {
    let d = input.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() || d.len() > 253 {
        return None;
    }
    let valid = d.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    });
    valid.then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(text: &str) -> Blocklist {
        Blocklist::parse(text).0
    }

    #[test]
    fn exact_and_subdomain_match() {
        let l = list("doubleclick.net");
        assert!(l.is_blocked("doubleclick.net"));
        assert!(l.is_blocked("ad.doubleclick.net"));
        assert!(l.is_blocked("a.b.c.doubleclick.net"));
    }

    #[test]
    fn no_partial_label_match() {
        let l = list("doubleclick.net");
        assert!(!l.is_blocked("notdoubleclick.net"));
        assert!(!l.is_blocked("doubleclick.network"));
        assert!(!l.is_blocked("net"));
    }

    #[test]
    fn case_and_trailing_dot_insensitive() {
        let l = list("Tracker.Example.COM.");
        assert!(l.is_blocked("tracker.example.com"));
        assert!(l.is_blocked("X.TRACKER.example.com."));
    }

    #[test]
    fn hosts_file_format() {
        let (l, stats) = Blocklist::parse(
            "# comment\n127.0.0.1 localhost\n0.0.0.0 ads.example.org\n127.0.0.1 t.example.net # inline\n",
        );
        assert!(l.is_blocked("ads.example.org"));
        assert!(l.is_blocked("t.example.net"));
        assert!(!l.is_blocked("localhost"));
        assert_eq!(stats.blocked, 2);
    }

    #[test]
    fn adblock_anchor_and_exception() {
        let (l, stats) = Blocklist::parse(
            "! EasyList-ish\n||ads.example.com^\n||example.com^\n@@||good.example.com^\n||x.com/path\n##.banner\n||y.com^$third-party\nz.com##.ad\n",
        );
        // `##.banner` starts with '#' so it is a comment line, not a skipped rule.
        assert_eq!(stats.blocked, 2);
        assert_eq!(stats.allowed, 1);
        assert_eq!(stats.skipped, 3);
        assert!(l.is_blocked("ads.example.com"));
        assert!(l.is_blocked("www.example.com"));
        assert!(!l.is_blocked("good.example.com"));
        assert!(!l.is_blocked("cdn.good.example.com"));
        assert!(!l.is_blocked("x.com"));
        assert!(!l.is_blocked("y.com"));
        // Cosmetic filter must not turn into a domain block.
        assert!(!l.is_blocked("z.com"));
    }

    #[test]
    fn invalid_input_is_skipped_or_allowed() {
        let (l, stats) = Blocklist::parse("bad domain here\n-bad-.com\n..\n");
        assert_eq!(stats.blocked, 0);
        assert_eq!(stats.skipped, 3);
        assert_eq!(l.check_host(""), Verdict::Allow);
        assert_eq!(l.check_host("exa mple.com"), Verdict::Allow);
    }

    #[test]
    fn programmatic_rules() {
        let mut l = Blocklist::new();
        assert!(l.is_empty());
        assert!(l.block_domain("tracker.io"));
        assert!(!l.block_domain("not valid"));
        assert!(l.is_blocked("cdn.tracker.io"));
        assert!(l.allow_domain("ok.tracker.io"));
        assert!(!l.is_blocked("ok.tracker.io"));
        assert_eq!(l.len(), 1);
    }
}
