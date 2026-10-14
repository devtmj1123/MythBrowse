// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Which saved logins may be offered on a page.
//!
//! A login is saved for one **origin** (scheme, host, port). On a page it is offered when:
//! * **Exact**: the page has the same origin;
//! * **Upgrade**: the login was saved on `http://host` and the page is `https://host`
//!   (same host, default ports or the same port);
//! * **SameSite**: both are `https`, same port, different hosts with the same registrable
//!   domain (eTLD+1 from the Public Suffix List): `accounts.example.com` vs
//!   `www.example.com`. Shown with the saved host so the user sees where it came from and only
//!   filled when the user picks it. Never for IP addresses or single-label hosts.
//!
//! Never: a login saved on `https` is never offered on an `http` page (a network attacker
//! could read it), never across schemes other than the upgrade above, never on non-web
//! schemes. Cross-origin iframes are excluded by the page script and by filling only the
//! top-level document (see `js/pw.js`).

use serde::Serialize;
use url::Url;

use crate::psl;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl Origin {
    /// `http(s)` URLs only; the host is lower-cased (the URL parser already did) and the
    /// port defaulted.
    pub fn parse(url: &str) -> Option<Self> {
        let u = Url::parse(url.trim()).ok()?;
        Self::from_url(&u)
    }

    pub fn from_url(u: &Url) -> Option<Self> {
        let scheme = u.scheme();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let host = u.host_str()?.trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return None;
        }
        Some(Self {
            scheme: scheme.to_owned(),
            port: u.port_or_known_default()?,
            host,
        })
    }

    fn default_port(&self) -> u16 {
        if self.scheme == "https" { 443 } else { 80 }
    }

    /// The registrable domain, or the host itself when it has none (IPs, `localhost`).
    pub fn site(&self) -> String {
        psl::registrable_domain(&self.host).unwrap_or_else(|| self.host.clone())
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.port == self.default_port() {
            write!(f, "{}://{}", self.scheme, self.host)
        } else {
            write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Exact,
    Upgrade,
    SameSite,
}

/// How a login saved for `saved` may be used on a page at `page` (`None`: not at all).
pub fn match_kind(saved: &Origin, page: &Origin) -> Option<MatchKind> {
    if saved == page {
        return Some(MatchKind::Exact);
    }
    // Never hand an https login to an http page.
    if saved.scheme == "https" && page.scheme != "https" {
        return None;
    }
    if saved.scheme == "http" && page.scheme == "https" {
        let ports_ok = saved.port == page.port || (saved.port == 80 && page.port == 443);
        return (saved.host == page.host && ports_ok).then_some(MatchKind::Upgrade);
    }
    if saved.scheme != "https" || page.scheme != "https" || saved.port != page.port {
        return None;
    }
    if psl::is_ip(&saved.host) || psl::is_ip(&page.host) {
        return None;
    }
    match (
        psl::registrable_domain(&saved.host),
        psl::registrable_domain(&page.host),
    ) {
        (Some(a), Some(b)) if a == b => Some(MatchKind::SameSite),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(s: &str) -> Origin {
        Origin::parse(s).unwrap()
    }

    #[test]
    fn parse_and_display() {
        assert_eq!(
            o("https://Example.COM/login?x=1").to_string(),
            "https://example.com"
        );
        assert_eq!(
            o("http://example.com:8080/").to_string(),
            "http://example.com:8080"
        );
        assert_eq!(
            o("https://example.com:443/").to_string(),
            "https://example.com"
        );
        assert!(Origin::parse("ftp://example.com").is_none());
        assert!(Origin::parse("android://abc@com.example").is_none());
        assert!(Origin::parse("not a url").is_none());
        assert_eq!(o("https://www.bbc.co.uk").site(), "bbc.co.uk");
        assert_eq!(o("http://127.0.0.1:5000").site(), "127.0.0.1");
    }

    #[test]
    fn exact_upgrade_and_same_site() {
        let m = |a: &str, b: &str| match_kind(&o(a), &o(b));
        assert_eq!(
            m("https://a.example.com", "https://a.example.com/x"),
            Some(MatchKind::Exact)
        );
        assert_eq!(
            m("http://example.com", "https://example.com"),
            Some(MatchKind::Upgrade)
        );
        assert_eq!(
            m("http://example.com:8080", "https://example.com:8080"),
            Some(MatchKind::Upgrade)
        );
        assert_eq!(m("http://example.com:8080", "https://example.com"), None);
        assert_eq!(
            m("https://accounts.example.com", "https://www.example.com"),
            Some(MatchKind::SameSite)
        );
        assert_eq!(
            m("https://example.com", "https://login.example.com"),
            Some(MatchKind::SameSite)
        );
    }

    #[test]
    fn never_matches() {
        let m = |a: &str, b: &str| match_kind(&o(a), &o(b));
        // https-saved login on an http page: never (also not the same host).
        assert_eq!(m("https://example.com", "http://example.com"), None);
        assert_eq!(m("https://a.example.com", "http://b.example.com"), None);
        // http logins are not shared across subdomains.
        assert_eq!(m("http://a.example.com", "http://b.example.com"), None);
        // Different registrable domains / public-suffix siblings.
        assert_eq!(m("https://example.com", "https://example.org"), None);
        assert_eq!(m("https://alice.github.io", "https://bob.github.io"), None);
        assert_eq!(m("https://bbc.co.uk", "https://evil.co.uk"), None);
        assert_eq!(m("https://example.com", "https://notexample.com"), None);
        // Ports must match for same-site.
        assert_eq!(
            m("https://a.example.com:8443", "https://b.example.com"),
            None
        );
        // IPs and single-label hosts: exact only.
        assert_eq!(m("https://10.0.0.1", "https://10.0.0.2"), None);
        assert_eq!(m("http://localhost:3000", "http://localhost:3001"), None);
        assert_eq!(
            m("http://localhost:3000", "http://localhost:3000"),
            Some(MatchKind::Exact)
        );
    }
}
