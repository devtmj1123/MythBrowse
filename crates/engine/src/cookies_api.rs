// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Cookies (v0.4.2): RPC ops for Settings > Cookies and the address bar's site popover, the
//! per-site cookie policy kept in the profile prefs, and the pure halves of enforcing it.
//! The shell side is `crates/shell/src/cookies_host.rs`.
//!
//! Internal pages only (they are `SettingsOp`s, see [`crate::settings_api`]): never a web page
//! or one of Myth's agent tabs. **Cookie values never cross this API**: the vendored
//! servo-net (patch 0002) answers the full listing with each value replaced by
//! `mbmeta.<size>.<host-only>`, and nothing here has a field for a value.
//!
//! | op | params |
//! |---|---|
//! | `cookies` | (sites with cookies: count, bytes, policy; the global prefs) |
//! | `cookies_list` | `site` (every cookie of one registrable domain, no values) |
//! | `cookie_delete` | `site`, `name`, `domain`, `path` |
//! | `cookie_policy` | `site`, `mode` = allow / block / clear_exit / default |
//! | `cookies_prefs_save` | `clear_on_exit` = 1 / 0 and / or `persist` = 1 / 0 (keep cookies after restart, [`PREF_PERSIST_COOKIES`]) |
//! | `cookie_site` | (the active tab's site: cookie count and policy, for the site popover) |
//!
//! Clearing a site's cookies, or all cookies, stays `site_data_clear?what=cookies[&site=]`.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::chrome::ApiError;

/// `<profile>/ui.json` key: keep cookies and site storage after MythBrowse closes (Servo gets
/// a profile `config_dir`, the cookie jar is encrypted at rest). Read before the engine
/// starts, so a change applies at the next start. **On by default** (see
/// [`persist_cookies_from_pref`]); never in a private session (docs/COOKIES.md, "Staying
/// signed in").
pub const PREF_PERSIST_COOKIES: &str = "persist_cookies";

/// "Stay signed in" when `ui.json` has no [`PREF_PERSIST_COOKIES`] value (new profile, or a
/// profile from before 0.4.2 where the switch was never touched).
pub const PERSIST_COOKIES_DEFAULT: bool = true;

/// Whether cookies are kept after restart, from the saved [`PREF_PERSIST_COOKIES`] value:
/// only an explicit `"0"` (the user turned the switch off) turns it off; `"1"`, a missing key
/// or an unreadable value give [`PERSIST_COOKIES_DEFAULT`]. A private session never keeps
/// cookies whatever this says (checked by the shell).
pub fn persist_cookies_from_pref(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        Some("0") => false,
        Some("1") => true,
        _ => PERSIST_COOKIES_DEFAULT,
    }
}

/// Profile pref holding [`CookiePolicies`] as JSON.
pub const PREF_COOKIE_POLICY: &str = "cookie_policy";
/// Prefix of a redacted cookie value (servo-net 0002 `mythbrowse_cookies::META_PREFIX`).
pub const META_PREFIX: &str = "mbmeta.";

/// What a site may do with cookies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SitePolicy {
    /// Keep them, even when "Clear cookies when MythBrowse closes" is on.
    Allow,
    /// Store none and send none (the vendored net drops them); existing ones are cleared.
    Block,
    /// Keep them while MythBrowse runs; cleared when it closes (and at the next start, in
    /// case it did not close cleanly).
    ClearOnExit,
}

impl SitePolicy {
    pub fn parse(s: &str) -> Option<SitePolicy> {
        match s {
            "allow" => Some(SitePolicy::Allow),
            "block" => Some(SitePolicy::Block),
            "clear_exit" => Some(SitePolicy::ClearOnExit),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SitePolicy::Allow => "allow",
            SitePolicy::Block => "block",
            SitePolicy::ClearOnExit => "clear_exit",
        }
    }
}

/// Per-site policies plus the global "clear on exit" switch (pref [`PREF_COOKIE_POLICY`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CookiePolicies {
    pub clear_on_exit: bool,
    sites: BTreeMap<String, SitePolicy>,
}

impl CookiePolicies {
    /// Read the pref; anything unreadable is the default (no rules), never an error.
    pub fn from_pref(raw: Option<&str>) -> CookiePolicies {
        let mut p = CookiePolicies::default();
        let Some(v) = raw.and_then(|r| serde_json::from_str::<Value>(r).ok()) else {
            return p;
        };
        p.clear_on_exit = v["clear_on_exit"].as_bool().unwrap_or(false);
        if let Some(m) = v["sites"].as_object() {
            for (site, mode) in m {
                if let (true, Some(mode)) =
                    (valid_site(site), mode.as_str().and_then(SitePolicy::parse))
                {
                    p.sites.insert(site.clone(), mode);
                }
            }
        }
        p
    }

    pub fn to_pref(&self) -> String {
        let sites: serde_json::Map<String, Value> = self
            .sites
            .iter()
            .map(|(s, m)| (s.clone(), Value::from(m.as_str())))
            .collect();
        json!({ "v": 1, "clear_on_exit": self.clear_on_exit, "sites": sites }).to_string()
    }

    pub fn get(&self, site: &str) -> Option<SitePolicy> {
        self.sites.get(site).copied()
    }

    /// Set (`Some`) or forget (`None`) a site's policy.
    pub fn set(&mut self, site: &str, mode: Option<SitePolicy>) {
        match mode {
            Some(m) => {
                self.sites.insert(site.to_owned(), m);
            }
            None => {
                self.sites.remove(site);
            }
        }
    }

    pub fn sites(&self) -> impl Iterator<Item = (&str, SitePolicy)> {
        self.sites.iter().map(|(s, m)| (s.as_str(), *m))
    }

    pub fn blocked(&self) -> Vec<String> {
        self.sites()
            .filter(|(_, m)| *m == SitePolicy::Block)
            .map(|(s, _)| s.to_owned())
            .collect()
    }

    /// The sites whose cookies go when MythBrowse closes (or starts after a crash), given
    /// the sites that have cookies now: every site set to "Clear on exit" or "Block", and,
    /// with the global switch on, every other site that is not set to "Allow".
    pub fn sites_to_clear_on_exit(&self, with_cookies: &[String]) -> Vec<String> {
        let mut out: Vec<String> = with_cookies
            .iter()
            .filter(|s| match self.get(s) {
                Some(SitePolicy::Allow) => false,
                Some(SitePolicy::Block) | Some(SitePolicy::ClearOnExit) => true,
                None => self.clear_on_exit,
            })
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// One stored cookie as the UI sees it: never its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookieRow {
    pub name: String,
    /// The host (host-only cookie) or domain (sent to sub-domains too), without a leading dot.
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    /// Unix seconds; `None` for a session cookie.
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    /// Name + value, in bytes.
    pub size: usize,
}

/// Parse the redacted value `mbmeta.<size>.<1|0>` into (size, host-only). `None` when the
/// value is not one (an unpatched engine): the caller must then drop the row's value itself.
pub fn decode_meta(value: &str) -> Option<(usize, bool)> {
    let rest = value.strip_prefix(META_PREFIX)?;
    let (size, host) = rest.split_once('.')?;
    let size = size.parse().ok()?;
    let host_only = match host {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    Some((size, host_only))
}

/// Sort for display: domain, then path, then name.
pub fn sort_rows(rows: &mut [CookieRow]) {
    rows.sort_by(|a, b| {
        (a.domain.as_str(), a.path.as_str(), a.name.as_str()).cmp(&(
            b.domain.as_str(),
            b.path.as_str(),
            b.name.as_str(),
        ))
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieOp {
    Sites,
    List {
        site: String,
    },
    Delete {
        site: String,
        name: String,
        domain: String,
        path: String,
    },
    Policy {
        site: String,
        mode: Option<SitePolicy>,
    },
    /// At least one of the two is given.
    PrefsSave {
        clear_on_exit: Option<bool>,
        persist: Option<bool>,
    },
    ActiveSite,
}

pub const OPS: &[&str] = &[
    "cookies",
    "cookies_list",
    "cookie_delete",
    "cookie_policy",
    "cookies_prefs_save",
    "cookie_site",
];

pub fn is_cookie_op(op: &str) -> bool {
    OPS.contains(&op)
}

/// A registrable domain as the cookie jar keys it (`example.co.uk`, or an IPv4 address):
/// lowercase letters, digits, `-` and `.`, at most 253 bytes, no empty label.
pub fn valid_site(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// `domain` is `site` or a sub-domain of it (a cookie of that site).
fn domain_of_site(domain: &str, site: &str) -> bool {
    domain == site
        || (domain.len() > site.len()
            && domain.ends_with(site)
            && domain.as_bytes()[domain.len() - site.len() - 1] == b'.')
}

fn printable(s: &str, max: usize, forbid: &[char]) -> bool {
    s.len() <= max && !s.chars().any(|c| c.is_control() || forbid.contains(&c))
}

impl CookieOp {
    pub fn parse(op: &str, param: &dyn Fn(&str) -> Option<String>) -> Result<CookieOp, ApiError> {
        let site = || -> Result<String, ApiError> {
            let s = param("site")
                .ok_or(ApiError::MissingParam("site"))?
                .trim()
                .to_ascii_lowercase();
            if valid_site(&s) {
                Ok(s)
            } else {
                Err(ApiError::BadParam("site"))
            }
        };
        Ok(match op {
            "cookies" => CookieOp::Sites,
            "cookies_list" => CookieOp::List { site: site()? },
            "cookie_delete" => {
                let site = site()?;
                let name = param("name").ok_or(ApiError::MissingParam("name"))?;
                if !printable(&name, 4096, &[';', '=']) {
                    return Err(ApiError::BadParam("name"));
                }
                let domain = param("domain")
                    .ok_or(ApiError::MissingParam("domain"))?
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                if !valid_site(&domain) || !domain_of_site(&domain, &site) {
                    return Err(ApiError::BadParam("domain"));
                }
                let path = param("path").unwrap_or_else(|| "/".into());
                if !path.starts_with('/') || !printable(&path, 1024, &[';']) {
                    return Err(ApiError::BadParam("path"));
                }
                CookieOp::Delete {
                    site,
                    name,
                    domain,
                    path,
                }
            }
            "cookie_policy" => {
                let site = site()?;
                let mode = match param("mode").as_deref() {
                    None => return Err(ApiError::MissingParam("mode")),
                    Some("default") => None,
                    Some(m) => Some(SitePolicy::parse(m).ok_or(ApiError::BadParam("mode"))?),
                };
                CookieOp::Policy { site, mode }
            }
            "cookies_prefs_save" => {
                let flag = |name: &'static str| match param(name).as_deref() {
                    Some("1") => Ok(Some(true)),
                    Some("0") => Ok(Some(false)),
                    None => Ok(None),
                    Some(_) => Err(ApiError::BadParam(name)),
                };
                let clear_on_exit = flag("clear_on_exit")?;
                let persist = flag("persist")?;
                if clear_on_exit.is_none() && persist.is_none() {
                    return Err(ApiError::MissingParam("clear_on_exit"));
                }
                CookieOp::PrefsSave {
                    clear_on_exit,
                    persist,
                }
            }
            "cookie_site" => CookieOp::ActiveSite,
            other => return Err(ApiError::UnknownOp(other.to_owned())),
        })
    }
}

/// What the UI says about third-party cookie blocking: Servo 0.1.4's cookie paths do not
/// know the top-level site of a request, so it is not offered yet (docs/COOKIES.md).
pub const THIRD_PARTY_NOTE: &str = "Blocking third-party cookies is not supported yet: the Servo engine MythBrowse uses does not tell its cookie store which site you are on. Block a tracker's site here instead.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(op: &str, q: &[(&str, &str)]) -> Result<CookieOp, ApiError> {
        let m: HashMap<String, String> = q
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        CookieOp::parse(op, &|k| m.get(k).cloned())
    }

    #[test]
    fn ops() {
        for op in OPS {
            assert!(is_cookie_op(op));
        }
        assert!(!is_cookie_op("site_data"));
        assert_eq!(parse("cookies", &[]), Ok(CookieOp::Sites));
        assert_eq!(parse("cookie_site", &[]), Ok(CookieOp::ActiveSite));
        assert_eq!(
            parse("cookies_list", &[("site", " Example.COM ")]),
            Ok(CookieOp::List {
                site: "example.com".into()
            })
        );
        assert!(parse("cookies_list", &[]).is_err());
        assert!(parse("cookies_list", &[("site", "a b.com")]).is_err());
        assert!(matches!(
            parse("cookie_eat", &[]),
            Err(ApiError::UnknownOp(_))
        ));
    }

    #[test]
    fn delete() {
        assert_eq!(
            parse(
                "cookie_delete",
                &[
                    ("site", "example.com"),
                    ("name", "__Host-sid"),
                    ("domain", ".Mail.Example.com"),
                    ("path", "/inbox")
                ]
            ),
            Ok(CookieOp::Delete {
                site: "example.com".into(),
                name: "__Host-sid".into(),
                domain: "mail.example.com".into(),
                path: "/inbox".into()
            })
        );
        let base = [
            ("site", "example.com"),
            ("name", "a"),
            ("domain", "example.com"),
        ];
        assert!(matches!(
            parse("cookie_delete", &base),
            Ok(CookieOp::Delete { ref path, .. }) if path == "/"
        ));
        // Another site's domain, a look-alike suffix, bad names / paths.
        for (k, v) in [
            ("domain", "evil.com"),
            ("domain", "notexample.com"),
            ("name", "a;b"),
            ("name", "a\nb"),
            ("path", "inbox"),
            ("path", "/a;b"),
        ] {
            let mut q: Vec<(&str, &str)> = base.iter().copied().filter(|(n, _)| *n != k).collect();
            q.push((k, v));
            assert!(parse("cookie_delete", &q).is_err(), "{k}={v:?}");
        }
        assert!(parse("cookie_delete", &[("site", "example.com"), ("name", "a")]).is_err());
    }

    #[test]
    fn policy_ops() {
        assert_eq!(
            parse("cookie_policy", &[("site", "x.org"), ("mode", "block")]),
            Ok(CookieOp::Policy {
                site: "x.org".into(),
                mode: Some(SitePolicy::Block)
            })
        );
        assert_eq!(
            parse("cookie_policy", &[("site", "x.org"), ("mode", "default")]),
            Ok(CookieOp::Policy {
                site: "x.org".into(),
                mode: None
            })
        );
        assert!(parse("cookie_policy", &[("site", "x.org")]).is_err());
        assert!(parse("cookie_policy", &[("site", "x.org"), ("mode", "maybe")]).is_err());
        assert_eq!(
            parse("cookies_prefs_save", &[("clear_on_exit", "1")]),
            Ok(CookieOp::PrefsSave {
                clear_on_exit: Some(true),
                persist: None
            })
        );
        assert_eq!(
            parse("cookies_prefs_save", &[("persist", "0")]),
            Ok(CookieOp::PrefsSave {
                clear_on_exit: None,
                persist: Some(false)
            })
        );
        assert!(parse("cookies_prefs_save", &[("persist", "on")]).is_err());
        assert!(parse("cookies_prefs_save", &[("clear_on_exit", "yes")]).is_err());
        assert!(parse("cookies_prefs_save", &[]).is_err());
    }

    #[test]
    fn persist_cookies_default_on_explicit_off_stays_off() {
        // Missing key (new profile, or never touched before 0.4.2): on (the default).
        assert!(persist_cookies_from_pref(None));
        // Explicit choices are kept.
        assert!(persist_cookies_from_pref(Some("1")));
        assert!(!persist_cookies_from_pref(Some("0")));
        assert!(!persist_cookies_from_pref(Some(" 0 ")));
        // Anything else is not an explicit "off": the default applies.
        assert!(persist_cookies_from_pref(Some("")));
        assert!(persist_cookies_from_pref(Some("yes")));
    }

    #[test]
    fn policies_round_trip() {
        let mut p = CookiePolicies::default();
        p.set("a.com", Some(SitePolicy::Allow));
        p.set("b.com", Some(SitePolicy::Block));
        p.set("c.com", Some(SitePolicy::ClearOnExit));
        p.clear_on_exit = true;
        let back = CookiePolicies::from_pref(Some(&p.to_pref()));
        assert_eq!(back, p);
        assert_eq!(back.blocked(), vec!["b.com".to_string()]);
        p.set("b.com", None);
        assert_eq!(p.get("b.com"), None);
        // Garbage or bad entries never fail start-up.
        assert_eq!(
            CookiePolicies::from_pref(Some("not json")),
            CookiePolicies::default()
        );
        let odd = CookiePolicies::from_pref(Some(
            r#"{"sites":{"OK.com":"allow","ok.com":"nope","fine.org":"block"}}"#,
        ));
        assert_eq!(
            odd.sites().collect::<Vec<_>>(),
            vec![("fine.org", SitePolicy::Block)]
        );
        assert_eq!(CookiePolicies::from_pref(None), CookiePolicies::default());
    }

    #[test]
    fn clear_on_exit_selection() {
        let have: Vec<String> = ["a.com", "b.com", "c.com", "d.com"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut p = CookiePolicies::default();
        p.set("a.com", Some(SitePolicy::Allow));
        p.set("b.com", Some(SitePolicy::Block));
        p.set("c.com", Some(SitePolicy::ClearOnExit));
        assert_eq!(p.sites_to_clear_on_exit(&have), vec!["b.com", "c.com"]);
        p.clear_on_exit = true;
        assert_eq!(
            p.sites_to_clear_on_exit(&have),
            vec!["b.com", "c.com", "d.com"]
        );
        assert!(p.sites_to_clear_on_exit(&[]).is_empty());
    }

    #[test]
    fn meta() {
        assert_eq!(decode_meta("mbmeta.12.1"), Some((12, true)));
        assert_eq!(decode_meta("mbmeta.0.0"), Some((0, false)));
        for bad in [
            "",
            "secret",
            "mbmeta.",
            "mbmeta.x.1",
            "mbmeta.3.2",
            "mbmeta.3",
        ] {
            assert_eq!(decode_meta(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn rows_have_no_value_and_sort() {
        let row = |d: &str, p: &str, n: &str| CookieRow {
            name: n.into(),
            domain: d.into(),
            host_only: true,
            path: p.into(),
            expires: None,
            secure: false,
            http_only: false,
            size: 1,
        };
        let mut rows = vec![
            row("b.a.com", "/", "z"),
            row("a.com", "/x", "b"),
            row("a.com", "/", "c"),
        ];
        sort_rows(&mut rows);
        let order: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(order, vec!["c", "b", "z"]);
        let v = serde_json::to_value(&rows[0]).unwrap();
        assert!(v.get("value").is_none());
        assert_eq!(v["expires"], Value::Null);
    }
}
