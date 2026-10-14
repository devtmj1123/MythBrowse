// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The declarative per-site ruleset (`rules/*.toml`, schema in `rules/schema.json`).
//!
//! ```toml
//! schema = 1
//! id = "core"
//! version = 2026100801
//!
//! [[rule]]
//! id = "polyfill.request-idle-callback"
//! title = "requestIdleCallback fallback"
//! kind = "polyfill"
//! hosts = ["*"]
//! js = """ if (!("requestIdleCallback" in window)) { ... } """
//! ```
//!
//! What a rule can do: override the user agent the page's scripts see (`ua`), add CSS
//! (`css`), run a JS shim (`js`), stop certain subresources from loading (`block`), and open
//! the page in reader / simplified mode (`mode`).
//!
//! Trust depends on where a ruleset comes from ([`Source`]): rules built into the binary and
//! signed remote rulesets may target every site (`hosts = ["*"]`, polyfills only); local
//! rules (the user's own, including AI patches they kept) must name their sites, and their
//! JS never goes into the all-sites bundle (the shell runs it only in pages it matches).

use serde::{Deserialize, Serialize};

use crate::matcher::{BlockPattern, HostPattern, valid_path_pattern};

/// Ruleset file format version understood by this build.
pub const SCHEMA_VERSION: u32 = 1;
/// Size caps (bytes) per rule field.
pub const MAX_CSS: usize = 64 * 1024;
pub const MAX_JS: usize = 64 * 1024;
pub const MAX_RULES: usize = 2000;

/// Where a ruleset came from; decides what it may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Compiled into the binary (`rules/core.toml`).
    Builtin,
    /// Downloaded, Ed25519-signed by a trusted key (see [`crate::sign`]).
    Remote,
    /// The user's own rules (`<profile>/compat/local.toml`), e.g. AI patches they kept.
    Local,
}

impl Source {
    pub fn trusted(self) -> bool {
        matches!(self, Source::Builtin | Source::Remote)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    /// Feature-guarded shim for an API Servo lacks; safe on every site.
    Polyfill,
    /// A fix for specific sites.
    #[default]
    Site,
    /// A patch Myth proposed and the user kept.
    Ai,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Article view: main text, headings and images only.
    Reader,
    /// The page itself with fixed overlays / scroll locks removed.
    Simplified,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Reader => "reader",
            Mode::Simplified => "simplified",
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: RuleKind,
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// UA preset name (see [`ua_preset`]) or a full user-agent string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ua: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Polyfills: the missing-feature names (as the bootstrap reports them, e.g.
    /// `requestIdleCallback`, `Element.animate`) this rule fills in. Lets the panel say which
    /// shims actually did something on a page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fills: Vec<String>,
    /// Free text: why the rule exists, upstream bug, ...
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// When a local rule was saved (unix ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesetFile {
    pub schema: u32,
    pub id: String,
    /// Monotonic; a remote ruleset older than the last one accepted is refused.
    pub version: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RulesError {
    #[error("could not parse ruleset: {0}")]
    Parse(String),
    #[error("ruleset schema {0} is not supported (this build reads {SCHEMA_VERSION})")]
    Schema(u32),
    #[error("rule `{rule}`: {problem}")]
    Rule { rule: String, problem: String },
    #[error("ruleset `{0}`: {1}")]
    Ruleset(String, String),
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// Built-in user-agent presets (`firefox`, `chrome`, `servo`, `firefox-windows`,
/// `firefox-linux`, `chrome-windows`; see [`crate::ua`]), for the OS this build runs on.
pub fn ua_preset(name: &str) -> Option<String> {
    crate::ua::preset(name, crate::ua::Os::current())
}

impl Rule {
    /// The UA string this rule sets, resolving presets.
    pub fn user_agent(&self) -> Option<String> {
        let ua = self.ua.as_deref()?;
        Some(ua_preset(ua).unwrap_or_else(|| ua.to_owned()))
    }

    pub fn applies_everywhere(&self) -> bool {
        self.hosts.iter().any(|h| h == "*")
    }

    /// Check one rule against the schema and what `source` may do.
    pub fn validate(&self, source: Source) -> Result<(), RulesError> {
        let bad = |problem: &str| {
            Err(RulesError::Rule {
                rule: self.id.clone(),
                problem: problem.to_owned(),
            })
        };
        if !valid_id(&self.id) {
            return bad("id must be 1-80 chars of a-z 0-9 . - _");
        }
        if self.hosts.is_empty() {
            return bad("hosts is empty");
        }
        for h in self.hosts.iter().chain(&self.exclude_hosts) {
            if HostPattern::parse(h).is_none() {
                return bad(&format!("bad host pattern `{h}`"));
            }
        }
        if self.exclude_hosts.iter().any(|h| h == "*") {
            return bad("exclude_hosts cannot be `*`");
        }
        if let Some(p) = self.paths.iter().find(|p| !valid_path_pattern(p)) {
            return bad(&format!("bad path pattern `{p}`"));
        }
        if let Some(b) = self.block.iter().find(|b| BlockPattern::parse(b).is_none()) {
            return bad(&format!("bad block pattern `{b}`"));
        }
        if self.css.as_ref().is_some_and(|c| c.len() > MAX_CSS) {
            return bad("css is too large");
        }
        if self.js.as_ref().is_some_and(|j| j.len() > MAX_JS) {
            return bad("js is too large");
        }
        if let Some(ua) = &self.ua {
            let ok = ua_preset(ua).is_some() || crate::ua::valid_custom(ua);
            if !ok {
                return bad("ua must be a preset name or a printable ASCII user-agent string");
            }
        }
        let does_something = self.ua.is_some()
            || self.css.as_ref().is_some_and(|c| !c.trim().is_empty())
            || self.js.as_ref().is_some_and(|j| !j.trim().is_empty())
            || !self.block.is_empty()
            || self.mode.is_some();
        if !does_something {
            return bad("rule does nothing (no ua, css, js, block or mode)");
        }
        if self.applies_everywhere() {
            if !source.trusted() {
                return bad("only built-in or signed rules may apply to every site");
            }
            if self.kind != RuleKind::Polyfill
                || self.ua.is_some()
                || !self.block.is_empty()
                || self.mode.is_some()
            {
                return bad("an every-site rule must be a polyfill with only css / js");
            }
        }
        if source == Source::Remote
            && let Some(js) = self.js.as_deref().filter(|j| !j.trim().is_empty())
            && !crate::is_reviewed_js(js)
        {
            return bad("remote rules may only use JS built into this browser");
        }
        if source == Source::Local && self.kind == RuleKind::Polyfill {
            return bad("local rules cannot be polyfills");
        }
        Ok(())
    }
}

impl RulesetFile {
    pub fn parse(text: &str, source: Source) -> Result<RulesetFile, RulesError> {
        let file: RulesetFile =
            toml::from_str(text).map_err(|e| RulesError::Parse(e.to_string()))?;
        file.validate(source)?;
        Ok(file)
    }

    pub fn validate(&self, source: Source) -> Result<(), RulesError> {
        if self.schema != SCHEMA_VERSION {
            return Err(RulesError::Schema(self.schema));
        }
        if !valid_id(&self.id) {
            return Err(RulesError::Ruleset(self.id.clone(), "bad id".into()));
        }
        if self.rules.len() > MAX_RULES {
            return Err(RulesError::Ruleset(
                self.id.clone(),
                "too many rules".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for r in &self.rules {
            if !seen.insert(r.id.as_str()) {
                return Err(RulesError::Rule {
                    rule: r.id.clone(),
                    problem: "duplicate id".into(),
                });
            }
            r.validate(source)?;
        }
        Ok(())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn empty(id: &str) -> RulesetFile {
        RulesetFile {
            schema: SCHEMA_VERSION,
            id: id.to_owned(),
            version: 1,
            description: String::new(),
            rules: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(hosts: &[&str]) -> Rule {
        Rule {
            id: "t.rule".into(),
            title: String::new(),
            kind: RuleKind::Site,
            hosts: hosts.iter().map(|s| s.to_string()).collect(),
            exclude_hosts: vec![],
            paths: vec![],
            ua: None,
            css: Some("a{}".into()),
            js: None,
            block: vec![],
            mode: None,
            enabled: true,
            fills: vec![],
            note: String::new(),
            created_ms: None,
        }
    }

    #[test]
    fn local_rules_cannot_target_every_site() {
        let mut r = rule(&["*"]);
        r.kind = RuleKind::Polyfill;
        assert!(r.validate(Source::Builtin).is_ok());
        assert!(r.validate(Source::Local).is_err());
        let mut site = rule(&["*"]);
        site.ua = Some("firefox".into());
        assert!(site.validate(Source::Builtin).is_err());
        assert!(rule(&["example.com"]).validate(Source::Local).is_ok());
    }

    #[test]
    fn rejects_bad_fields() {
        let mut r = rule(&["example.com"]);
        r.css = None;
        assert!(r.validate(Source::Builtin).is_err(), "does nothing");
        let mut r = rule(&["Example.com"]);
        assert!(r.validate(Source::Builtin).is_err());
        r = rule(&["example.com"]);
        r.ua = Some("bad\nua".into());
        assert!(r.validate(Source::Builtin).is_err());
        r = rule(&["example.com"]);
        r.id = "Bad Id".into();
        assert!(r.validate(Source::Builtin).is_err());
        r = rule(&["example.com"]);
        r.block = vec!["*/x.js".into()];
        assert!(r.validate(Source::Builtin).is_err());
    }

    #[test]
    fn parses_and_rejects_unknown_fields_and_schema() {
        let ok = "schema = 1\nid = \"x\"\nversion = 3\n[[rule]]\nid = \"a\"\nhosts = [\"example.com\"]\nmode = \"reader\"\n";
        let f = RulesetFile::parse(ok, Source::Local).unwrap();
        assert_eq!(f.rules[0].mode, Some(Mode::Reader));
        assert!(f.rules[0].enabled);
        assert!(
            RulesetFile::parse(&ok.replace("schema = 1", "schema = 9"), Source::Local).is_err()
        );
        assert!(RulesetFile::parse(&format!("{ok}evil = 1\n"), Source::Local).is_err());
        let dup = format!("{ok}[[rule]]\nid = \"a\"\nhosts = [\"a.com\"]\ncss = \"x\"\n");
        assert!(RulesetFile::parse(&dup, Source::Local).is_err());
        // Round trip.
        let back = RulesetFile::parse(&f.to_toml(), Source::Local).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn remote_rules_cannot_bring_new_js() {
        let mut r = rule(&["example.com"]);
        r.css = None;
        r.js = Some("alert(1)".into());
        assert!(r.validate(Source::Builtin).is_ok());
        assert!(r.validate(Source::Local).is_ok());
        assert!(r.validate(Source::Remote).is_err());
        // A shim that ships in this build may be re-targeted by a signed remote rule.
        let core: RulesetFile = toml::from_str(crate::CORE_RULES).unwrap();
        let shipped = core.rules.iter().find_map(|x| x.js.clone()).unwrap();
        r.js = Some(shipped);
        assert!(r.validate(Source::Remote).is_ok());
        // CSS-only remote rules are declarative and stay allowed.
        let css = rule(&["example.com"]);
        assert!(css.validate(Source::Remote).is_ok());
    }

    #[test]
    fn ua_presets_resolve() {
        let mut r = rule(&["example.com"]);
        r.ua = Some("firefox".into());
        assert!(r.user_agent().unwrap().contains("Firefox/"));
        r.ua = Some("Custom/1.0 (test)".into());
        assert_eq!(r.user_agent().unwrap(), "Custom/1.0 (test)");
    }
}
