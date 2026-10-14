// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Semantic versions as release tags write them: `v0.4.0`, `0.4`, `v1.2.3-preview.2`,
//! `1.0.0+build.7`. Ordering follows SemVer 2.0 section 11 (build metadata ignored; a
//! pre-release sorts before its release; numeric identifiers compare as numbers and before
//! alphanumeric ones).

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated pre-release identifiers (`["preview", "2"]`), empty for a release.
    pub pre: Vec<String>,
}

impl Version {
    /// Parse a tag or version string. A leading `v` / `V`, surrounding spaces and `+build`
    /// metadata are accepted; missing minor / patch count as 0. `None` when it is not a
    /// version at all (`nightly`, `1.x`, empty).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let s = s.split_once('+').map_or(s, |(v, _)| v);
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = core.split('.');
        let num = |p: Option<&str>| -> Option<u64> {
            match p {
                None => Some(0),
                Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok(),
                Some(_) => None,
            }
        };
        let first = nums.next().filter(|p| !p.is_empty())?;
        let major = num(Some(first))?;
        let minor = num(nums.next())?;
        let patch = num(nums.next())?;
        if nums.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let ids: Vec<String> = p.split('.').map(str::to_owned).collect();
                if ids.iter().any(|i| {
                    i.is_empty() || !i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }) {
                    return None;
                }
                ids
            }
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

fn cmp_ident(a: &str, b: &str) -> Ordering {
    let num = |s: &str| {
        (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse::<u128>().ok())
            .flatten()
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(other.pre.iter()) {
                        let o = cmp_ident(a, b);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// Is `latest` (a release tag) newer than `current` (this build)? `None` when either is not a
/// version (then nothing is offered).
pub fn is_newer(current: &str, latest: &str) -> Option<bool> {
    Some(Version::parse(latest)? > Version::parse(current)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} parses"))
    }

    #[test]
    fn parses_tags() {
        assert_eq!(v("v0.4.0").to_string(), "0.4.0");
        assert_eq!(v("0.4").to_string(), "0.4.0");
        assert_eq!(v("V2").to_string(), "2.0.0");
        assert_eq!(v(" v1.2.3-preview.2+ci.55 ").to_string(), "1.2.3-preview.2");
        assert_eq!(v("1.0.0-rc-1").pre, vec!["rc-1".to_string()]);
        for bad in [
            "",
            "v",
            "nightly",
            "1.x",
            "1..2",
            "1.2.3.4",
            "1.2.3-",
            "1.2.3-a..b",
            "-1.0",
            "1.0 beta",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn semver_order() {
        // SemVer 2.0 section 11 example chain.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("1.0.0+a").cmp(&v("1.0.0+b")), Ordering::Equal);
        assert!(v("0.10.0") > v("0.9.9"));
    }

    #[test]
    fn newer() {
        assert_eq!(is_newer("0.0.1", "v0.4.0"), Some(true));
        assert_eq!(is_newer("0.4.0", "v0.4.0"), Some(false));
        assert_eq!(is_newer("0.4.1", "v0.4.0"), Some(false));
        assert_eq!(is_newer("0.4.0-preview.1", "v0.4.0"), Some(true));
        assert_eq!(is_newer("0.4.0", "v0.4.1-preview.1"), Some(true));
        assert_eq!(is_newer("0.4.0", "latest"), None);
    }
}
