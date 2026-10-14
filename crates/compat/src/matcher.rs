// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Host, path and URL patterns used by ruleset entries.
//!
//! Host patterns (`hosts`, `exclude_hosts`):
//!
//! | pattern           | matches                                   |
//! |-------------------|-------------------------------------------|
//! | `*`               | every web site (http / https)             |
//! | `example.com`     | `example.com` and every subdomain         |
//! | `=example.com`    | exactly `example.com`                     |
//! | `*.example.com`   | subdomains of `example.com`, not the apex |
//!
//! Path patterns (`paths`) start with `/`; `*` matches any run of characters
//! (`/watch*`, `/*/embed/*`). An empty list matches every path.
//!
//! Block patterns (`block`) are `host/path` without a scheme
//! (`cdn.example.com/js/legacy-*.js`); the host part uses the host-pattern rules above
//! (never `*`), the path part is a path glob (missing = every path).
//!
//! The same semantics are implemented in JavaScript in `js/bootstrap.js` (`match`), so a
//! rule gates itself identically in the page; keep the two in step.

use url::Url;

/// The browser's own origin: never matched by any pattern, whatever it says.
pub const INTERNAL_HOST: &str = "mythbrowse.internal";

/// A parsed host pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPattern {
    Any,
    /// Domain and its subdomains.
    Domain(String),
    Exact(String),
    /// Subdomains only.
    Sub(String),
}

fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && d.contains('.')
        && !d.starts_with('.')
        && !d.ends_with('.')
        && !d.contains("..")
        && d.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl HostPattern {
    /// Parse a pattern; `None` if malformed (upper case, scheme, port, unicode...).
    pub fn parse(p: &str) -> Option<HostPattern> {
        if p == "*" {
            return Some(HostPattern::Any);
        }
        let (ctor, d): (fn(String) -> HostPattern, &str) = if let Some(d) = p.strip_prefix('=') {
            (HostPattern::Exact, d)
        } else if let Some(d) = p.strip_prefix("*.") {
            (HostPattern::Sub, d)
        } else {
            (HostPattern::Domain, p)
        };
        if !valid_domain(d) || d == INTERNAL_HOST {
            return None;
        }
        Some(ctor(d.to_owned()))
    }

    pub fn matches(&self, host: &str) -> bool {
        if host.is_empty() || host == INTERNAL_HOST {
            return false;
        }
        let sub_of = |d: &str| {
            host.len() > d.len()
                && host.ends_with(d)
                && host.as_bytes()[host.len() - d.len() - 1] == b'.'
        };
        match self {
            HostPattern::Any => true,
            HostPattern::Exact(d) => host == d,
            HostPattern::Domain(d) => host == d || sub_of(d),
            HostPattern::Sub(d) => sub_of(d),
        }
    }

    pub fn is_any(&self) -> bool {
        matches!(self, HostPattern::Any)
    }
}

/// `*` glob over bytes (no other metacharacters).
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

pub fn valid_path_pattern(p: &str) -> bool {
    p.starts_with('/') && p.len() <= 512 && !p.chars().any(char::is_whitespace)
}

/// A page URL the ruleset may act on: http(s), not the browser's own pages.
pub fn is_web_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url
            .host_str()
            .is_some_and(|h| !h.is_empty() && h != INTERNAL_HOST)
}

/// Does a rule with these patterns apply to `url`?
pub fn rule_matches(hosts: &[String], exclude: &[String], paths: &[String], url: &Url) -> bool {
    if !is_web_url(url) {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    let hit = |list: &[String]| {
        list.iter()
            .filter_map(|p| HostPattern::parse(p))
            .any(|p| p.matches(host))
    };
    if !hit(hosts) || hit(exclude) {
        return false;
    }
    paths.is_empty() || paths.iter().any(|p| glob(p, url.path()))
}

/// A parsed `block` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockPattern {
    pub host: HostPattern,
    pub path: String,
}

impl BlockPattern {
    pub fn parse(p: &str) -> Option<BlockPattern> {
        if p.contains("://") || p.chars().any(char::is_whitespace) || p.len() > 512 {
            return None;
        }
        let (host, path) = match p.find('/') {
            Some(i) => (&p[..i], &p[i..]),
            None => (p, "/*"),
        };
        let host = HostPattern::parse(host).filter(|h| !h.is_any())?;
        Some(BlockPattern {
            host,
            path: path.to_owned(),
        })
    }

    pub fn matches(&self, url: &Url) -> bool {
        is_web_url(url)
            && self.host.matches(url.host_str().unwrap_or(""))
            && glob(&self.path, url.path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn host_patterns() {
        let d = HostPattern::parse("example.com").unwrap();
        assert!(d.matches("example.com"));
        assert!(d.matches("www.example.com"));
        assert!(!d.matches("badexample.com"));
        assert!(!d.matches("example.com.evil.net"));
        let e = HostPattern::parse("=example.com").unwrap();
        assert!(e.matches("example.com") && !e.matches("www.example.com"));
        let s = HostPattern::parse("*.example.com").unwrap();
        assert!(!s.matches("example.com") && s.matches("a.b.example.com"));
        assert!(HostPattern::parse("*").unwrap().matches("anything.org"));
        assert!(!HostPattern::Any.matches(INTERNAL_HOST));
        for bad in [
            "",
            "Example.com",
            "https://example.com",
            "example.com:80",
            ".example.com",
            "localhost",
            "exa mple.com",
            "*.*",
            "=*",
            "mythbrowse.internal",
            "-a.com",
        ] {
            assert_eq!(HostPattern::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn globbing() {
        assert!(glob("/watch*", "/watch?v"));
        assert!(glob("/*/embed/*", "/a/embed/b"));
        assert!(!glob("/*/embed/*", "/embed/b"));
        assert!(glob("*", ""));
        assert!(glob("/a*b*c", "/aXXbYYc"));
        assert!(!glob("/a*b*c", "/aXXbYY"));
        assert!(glob("/exact", "/exact") && !glob("/exact", "/exactly"));
    }

    #[test]
    fn rules_match_web_pages_only() {
        let h = vec!["example.com".to_owned()];
        assert!(rule_matches(&h, &[], &[], &u("https://www.example.com/x")));
        assert!(!rule_matches(&h, &[], &[], &u("file:///example.com")));
        let any = vec!["*".to_owned()];
        assert!(!rule_matches(
            &any,
            &[],
            &[],
            &u("https://mythbrowse.internal/home.html")
        ));
        assert!(!rule_matches(
            &any,
            &["example.com".to_owned()],
            &[],
            &u("http://example.com/")
        ));
        assert!(rule_matches(
            &h,
            &[],
            &["/app/*".to_owned()],
            &u("https://example.com/app/1")
        ));
        assert!(!rule_matches(
            &h,
            &[],
            &["/app/*".to_owned()],
            &u("https://example.com/other")
        ));
    }

    #[test]
    fn block_patterns() {
        let b = BlockPattern::parse("cdn.example.com/js/legacy-*.js").unwrap();
        assert!(b.matches(&u("https://cdn.example.com/js/legacy-1.js?x=1")));
        assert!(!b.matches(&u("https://cdn.example.com/js/app.js")));
        assert!(
            BlockPattern::parse("example.com")
                .unwrap()
                .matches(&u("http://a.example.com/x"))
        );
        assert_eq!(BlockPattern::parse("*/x.js"), None);
        assert_eq!(BlockPattern::parse("https://a.com/x.js"), None);
    }
}
