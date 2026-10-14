// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Registrable domain ("eTLD+1") lookup with the Public Suffix List.
//!
//! `data/public_suffix_list.dat` is the list Servo 0.1.4 ships (`servo-default-resources`,
//! `public_domains.txt`, from <https://publicsuffix.org/list/>, MPL-2.0): comments stripped,
//! IDN entries already in punycode, so it matches `url::Url::host_str()` directly. Parsed once
//! on first use (~9k rules).

use std::collections::HashSet;
use std::sync::LazyLock;

static LIST: &str = include_str!("../data/public_suffix_list.dat");

struct Rules {
    rules: HashSet<&'static str>,
    /// `*.foo` stored as `foo`.
    wildcards: HashSet<&'static str>,
    /// `!bar.foo` stored as `bar.foo`.
    exceptions: HashSet<&'static str>,
}

static RULES: LazyLock<Rules> = LazyLock::new(|| {
    let mut r = Rules {
        rules: HashSet::new(),
        wildcards: HashSet::new(),
        exceptions: HashSet::new(),
    };
    for line in LIST.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(s) = line.strip_prefix('!') {
            r.exceptions.insert(s);
        } else if let Some(s) = line.strip_prefix("*.") {
            r.wildcards.insert(s);
        } else {
            r.rules.insert(line);
        }
    }
    r
});

/// Whether `host` is an IP address literal (v4, or v6 with or without brackets).
pub fn is_ip(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.parse::<std::net::IpAddr>().is_ok()
}

/// The public suffix of `host` (`co.uk` for `www.bbc.co.uk`). Unknown TLDs count as a
/// one-label suffix (the PSL's implicit `*` rule).
pub fn public_suffix(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || is_ip(&host) {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.iter().any(|l| l.is_empty()) {
        return None;
    }
    let rules = &*RULES;
    for i in 0..labels.len() {
        let candidate = labels[i..].join(".");
        if rules.exceptions.contains(candidate.as_str()) {
            return Some(labels[i + 1..].join("."));
        }
        if rules.rules.contains(candidate.as_str()) {
            return Some(candidate);
        }
        if i + 1 < labels.len() && rules.wildcards.contains(labels[i + 1..].join(".").as_str()) {
            return Some(candidate);
        }
    }
    labels.last().map(|l| (*l).to_owned())
}

/// The registrable domain (eTLD+1) of `host`: `bbc.co.uk` for `www.bbc.co.uk`. `None` for IP
/// addresses, single-label hosts (`localhost`) and hosts that are themselves a public suffix.
pub fn registrable_domain(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let suffix = public_suffix(&host)?;
    if host == suffix || !host.contains('.') {
        return None;
    }
    let rest = host.strip_suffix(&suffix)?.strip_suffix('.')?;
    let label = rest.rsplit('.').next()?;
    Some(format!("{label}.{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domains() {
        assert_eq!(
            registrable_domain("www.example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            registrable_domain("example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            registrable_domain("a.b.bbc.co.uk").as_deref(),
            Some("bbc.co.uk")
        );
        assert_eq!(
            registrable_domain("login.maybank2u.com.my").as_deref(),
            Some("maybank2u.com.my")
        );
        // Private-section suffixes: two GitHub Pages sites are different sites.
        assert_eq!(
            registrable_domain("alice.github.io").as_deref(),
            Some("alice.github.io")
        );
        assert_eq!(registrable_domain("github.io"), None);
        assert_eq!(registrable_domain("co.uk"), None);
        assert_eq!(registrable_domain("localhost"), None);
        assert_eq!(registrable_domain("127.0.0.1"), None);
        assert_eq!(registrable_domain("[::1]"), None);
        // Unknown TLD: implicit `*` rule.
        assert_eq!(
            registrable_domain("a.b.example.zzzunknown").as_deref(),
            Some("example.zzzunknown")
        );
        assert_eq!(
            registrable_domain("WWW.Example.COM.").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn wildcard_and_exception_rules() {
        // `*.ck` and `!www.ck` are in the list.
        assert_eq!(public_suffix("foo.bar.ck").as_deref(), Some("bar.ck"));
        assert_eq!(
            registrable_domain("foo.bar.ck").as_deref(),
            Some("foo.bar.ck")
        );
        assert_eq!(registrable_domain("www.ck").as_deref(), Some("www.ck"));
    }
}
