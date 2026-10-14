// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! MythBrowse site-compat layer (see `docs/COMPAT.md`).
//!
//! * [`detect`]: low-cost breakage detector (passive bootstrap + one probe per page load).
//! * [`rules`] / [`matcher`]: the declarative per-site ruleset (`rules/*.toml`).
//! * [`bundle`]: rules -> Servo user scripts (all pages, self-gating) and per-page scripts.
//! * [`sign`]: Ed25519 signatures for rulesets delivered from outside the binary.
//! * [`repair`]: AI repair prompt / reply / static review (the shell calls the model).
//!
//! [`Compat`] ties them together for the shell. Nothing here touches Servo, so all of it is
//! unit-tested without the engine.

pub mod bundle;
pub mod crash;
pub mod detect;
pub mod matcher;
pub mod repair;
pub mod rules;
pub mod sign;
pub mod ua;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

pub use crash::{CrashLog, CrashReport};
pub use detect::{Assessment, ConsoleStats, Finding, Level, ProbeReport, Severity, assess};
pub use repair::{Candidate, Review};
pub use rules::{Mode, Rule, RuleKind, RulesError, RulesetFile, Source};

/// The core ruleset compiled into the binary. The repo is private, so these rules are not
/// published with the public docs / SDK repo.
pub const CORE_RULES: &str = include_str!("../../../rules/core.toml");

/// Where the compat files live under the profile directory.
pub const DIR: &str = "compat";
pub const LOCAL_FILE: &str = "local.toml";
pub const STATE_FILE: &str = "state.json";
pub const REMOTE_FILE: &str = "remote.toml";
pub const REMOTE_SIG_FILE: &str = "remote.toml.sig";

/// The user scripts `file`'s rules turn into (as trusted rules), without the bootstrap. For
/// `mb-rules scripts`, so CI can `node --check` exactly what the browser installs.
pub fn bundle_for_check(file: &RulesetFile) -> Vec<String> {
    let key = session_key(0);
    file.rules
        .iter()
        .filter_map(|r| bundle::rule_script(r, Source::Builtin, &[], &key))
        .collect()
}

/// Whether `js` is exactly the JS of a rule built into this binary (`rules/core.toml`): the only
/// executable code a remote ruleset may carry. Remote rules can still add CSS, block patterns,
/// UA presets and reader mode (all declarative).
pub fn is_reviewed_js(js: &str) -> bool {
    static BUILTIN: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    BUILTIN
        .get_or_init(|| {
            toml::from_str::<RulesetFile>(CORE_RULES)
                .map(|f| f.rules.into_iter().filter_map(|r| r.js).collect())
                .unwrap_or_default()
        })
        .iter()
        .any(|b| b == js)
}

/// The user's per-site switches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalState {
    /// Hosts (exact) where no compat rule runs at all.
    pub paused: BTreeSet<String>,
    /// Rule id -> hosts (exact) where the user turned that rule off.
    pub off: BTreeMap<String, BTreeSet<String>>,
    /// Version of the last remote ruleset accepted (anti-rollback).
    pub remote_version: Option<u64>,
    /// Settings > Site compatibility "Automatically update site-fix rules": the browser downloads the
    /// signed cloud ruleset now and then (nothing about the user is sent). Default on.
    #[serde(default = "auto_update_default")]
    pub auto_update: bool,
}

fn auto_update_default() -> bool {
    true
}

impl Default for LocalState {
    fn default() -> Self {
        LocalState {
            paused: BTreeSet::new(),
            off: BTreeMap::new(),
            remote_version: None,
            auto_update: true,
        }
    }
}

/// A rule that applies to a page, for the compat panel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppliedRule {
    pub id: String,
    pub title: String,
    pub kind: RuleKind,
    pub source: Source,
    /// What it does: any of "ua", "css", "js", "block", "reader", "simplified".
    pub does: Vec<&'static str>,
    /// The user turned it off on this host.
    pub off: bool,
}

#[derive(Debug, Clone)]
struct Loaded {
    file: RulesetFile,
    source: Source,
}

/// Every ruleset the browser has, plus the user's switches.
#[derive(Debug, Clone)]
pub struct Compat {
    key: String,
    sets: Vec<Loaded>,
    state: LocalState,
    dir: Option<PathBuf>,
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// A per-session property name for the bootstrap's state (not guessable by pages).
pub fn session_key(random: u64) -> String {
    format!("__mbc_{random:016x}")
}

impl Compat {
    /// Load the built-in rules, the user's local rules and switches from `profile_dir`
    /// (`None`: in-memory only) and a signed remote ruleset if one is present and verifies.
    /// Problems are returned as warnings (a bad local file never stops the browser).
    pub fn load(profile_dir: Option<&Path>, key: String) -> (Compat, Vec<String>) {
        let mut warnings = Vec::new();
        let builtin = RulesetFile::parse(CORE_RULES, Source::Builtin).unwrap_or_else(|e| {
            warnings.push(format!("built-in compat rules: {e}"));
            RulesetFile::empty("core")
        });
        let dir = profile_dir.map(|d| d.join(DIR));
        let mut compat = Compat {
            key,
            sets: vec![Loaded {
                file: builtin,
                source: Source::Builtin,
            }],
            state: LocalState::default(),
            dir: dir.clone(),
        };
        let Some(dir) = dir else {
            compat.sets.push(Loaded {
                file: RulesetFile::empty("local"),
                source: Source::Local,
            });
            return (compat, warnings);
        };
        if let Some(text) = read(&dir.join(STATE_FILE)) {
            match serde_json::from_str(&text) {
                Ok(s) => compat.state = s,
                Err(e) => warnings.push(format!("compat state ignored: {e}")),
            }
        }
        if let (Some(toml), Some(sig)) = (
            read(&dir.join(REMOTE_FILE)),
            read(&dir.join(REMOTE_SIG_FILE)),
        ) && let Err(e) = compat.accept_remote(&toml, &sig, sign::TRUSTED_KEYS)
        {
            warnings.push(format!("remote compat rules refused: {e}"));
        }
        let local = match read(&dir.join(LOCAL_FILE)) {
            Some(text) => RulesetFile::parse(&text, Source::Local).unwrap_or_else(|e| {
                warnings.push(format!("local compat rules ignored: {e}"));
                RulesetFile::empty("local")
            }),
            None => RulesetFile::empty("local"),
        };
        compat.sets.push(Loaded {
            file: local,
            source: Source::Local,
        });
        (compat, warnings)
    }

    /// Verify and install a remote ruleset (signature, schema, rules, no rollback).
    pub fn accept_remote(
        &mut self,
        toml: &str,
        sig: &str,
        keys: &[sign::TrustedKey],
    ) -> Result<String, String> {
        let key = sign::verify(toml.as_bytes(), sig, keys).map_err(|e| e.to_string())?;
        let file = RulesetFile::parse(toml, Source::Remote).map_err(|e| e.to_string())?;
        sign::check_version(file.version, self.state.remote_version).map_err(|e| e.to_string())?;
        self.state.remote_version = Some(file.version);
        self.sets.retain(|s| s.source != Source::Remote);
        // Remote sits between built-in and local, so local rules are listed last.
        let at = self
            .sets
            .iter()
            .position(|s| s.source == Source::Local)
            .unwrap_or(self.sets.len());
        self.sets.insert(
            at,
            Loaded {
                file,
                source: Source::Remote,
            },
        );
        Ok(key)
    }

    /// A ruleset downloaded from the cloud: verify it like [`Self::accept_remote`] and, when it
    /// is new, keep it in the profile so the next start uses it without the network.
    /// `Ok(None)`: identical to the copy already installed.
    pub fn install_remote(
        &mut self,
        toml: &str,
        sig: &str,
        keys: &[sign::TrustedKey],
    ) -> Result<Option<String>, String> {
        let dir = self.dir.clone();
        if let Some(dir) = &dir
            && read(&dir.join(REMOTE_FILE)).as_deref() == Some(toml)
            && read(&dir.join(REMOTE_SIG_FILE)).as_deref() == Some(sig)
            && self.sets.iter().any(|s| s.source == Source::Remote)
        {
            return Ok(None);
        }
        let key = self.accept_remote(toml, sig, keys)?;
        if let Some(dir) = &dir {
            // Signature last: a crash in between leaves a pair that fails to verify (ignored)
            // rather than an old signature over a new file.
            write_atomic(&dir.join(REMOTE_FILE), toml).map_err(|e| e.to_string())?;
            write_atomic(&dir.join(REMOTE_SIG_FILE), sig).map_err(|e| e.to_string())?;
            self.save_state();
        }
        Ok(Some(key))
    }

    /// Settings: download the signed cloud ruleset automatically.
    pub fn auto_update(&self) -> bool {
        self.state.auto_update
    }

    /// Change and save the setting. Turning it off stops downloads; a ruleset already
    /// installed stays in use until the user turns the rules off or deletes the profile.
    pub fn set_auto_update(&mut self, on: bool) {
        if self.state.auto_update != on {
            self.state.auto_update = on;
            self.save_state();
        }
    }

    /// Version of the installed remote ruleset, if any.
    pub fn remote_version(&self) -> Option<u64> {
        self.sets
            .iter()
            .find(|s| s.source == Source::Remote)
            .map(|s| s.file.version)
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    fn all(&self) -> impl Iterator<Item = (&Rule, Source)> {
        self.sets
            .iter()
            .flat_map(|s| s.file.rules.iter().map(move |r| (r, s.source)))
    }

    fn local(&self) -> &RulesetFile {
        &self
            .sets
            .iter()
            .find(|s| s.source == Source::Local)
            .expect("local set is always loaded")
            .file
    }

    fn local_mut(&mut self) -> &mut RulesetFile {
        &mut self
            .sets
            .iter_mut()
            .find(|s| s.source == Source::Local)
            .expect("local set is always loaded")
            .file
    }

    pub fn state(&self) -> &LocalState {
        &self.state
    }

    /// Host patterns a rule must not run on (paused sites + where the user turned it off).
    fn extra_exclude(&self, rule_id: &str) -> Vec<String> {
        let mut v: Vec<String> = self.state.paused.iter().map(|h| format!("={h}")).collect();
        if let Some(hosts) = self.state.off.get(rule_id) {
            v.extend(hosts.iter().map(|h| format!("={h}")));
        }
        v
    }

    fn active_on(&self, rule: &Rule, url: &Url) -> bool {
        if !rule.enabled {
            return false;
        }
        let host = url.host_str().unwrap_or("");
        if self.state.paused.contains(host)
            || self
                .state
                .off
                .get(&rule.id)
                .is_some_and(|h| h.contains(host))
        {
            return false;
        }
        matcher::rule_matches(&rule.hosts, &rule.exclude_hosts, &rule.paths, url)
    }

    /// All user scripts for the shared `UserContentManager`: the bootstrap first, then one
    /// self-gating script per enabled rule. Rebuild (and reload pages) after any change.
    pub fn user_scripts(&self) -> Vec<String> {
        let mut out = vec![bundle::bootstrap(&self.key)];
        for (rule, source) in self.all() {
            if !rule.enabled {
                continue;
            }
            if let Some(s) =
                bundle::rule_script(rule, source, &self.extra_exclude(&rule.id), &self.key)
            {
                out.push(s);
            }
        }
        out
    }

    /// The detector probe for this session.
    pub fn probe_script(&self) -> String {
        detect::probe_template().replace("__MB_KEY__", &self.key)
    }

    /// JS of local rules that apply to `url` (run by the shell once the head is parsed).
    pub fn page_script(&self, url: &Url) -> Option<String> {
        let rules: Vec<&Rule> = self
            .local()
            .rules
            .iter()
            .filter(|r| self.active_on(r, url))
            .collect();
        bundle::page_script(&rules, &self.key)
    }

    /// The rule blocking `request` on a page at `page`, if any (subresources only).
    pub fn blocked_by(&self, page: &Url, request: &Url) -> Option<&str> {
        self.all()
            .filter(|(r, _)| !r.block.is_empty() && self.active_on(r, page))
            .find(|(r, _)| {
                r.block
                    .iter()
                    .filter_map(|b| matcher::BlockPattern::parse(b))
                    .any(|b| b.matches(request))
            })
            .map(|(r, _)| r.id.as_str())
    }

    /// Rules that match `url` (including ones the user turned off there), for the panel.
    /// Every-site polyfills are listed only when `include_polyfills`.
    pub fn rules_for(&self, url: &Url, include_polyfills: bool) -> Vec<AppliedRule> {
        let host = url.host_str().unwrap_or("");
        self.all()
            .filter(|(r, _)| r.enabled)
            .filter(|(r, _)| include_polyfills || r.kind != RuleKind::Polyfill)
            .filter(|(r, _)| matcher::rule_matches(&r.hosts, &r.exclude_hosts, &r.paths, url))
            .map(|(r, source)| {
                let mut does = Vec::new();
                if r.ua.is_some() {
                    does.push("ua");
                }
                if r.css.as_ref().is_some_and(|c| !c.trim().is_empty()) {
                    does.push("css");
                }
                if r.js.as_ref().is_some_and(|j| !j.trim().is_empty()) {
                    does.push("js");
                }
                if !r.block.is_empty() {
                    does.push("block");
                }
                if let Some(m) = r.mode {
                    does.push(m.as_str());
                }
                AppliedRule {
                    id: r.id.clone(),
                    title: if r.title.is_empty() {
                        r.id.clone()
                    } else {
                        r.title.clone()
                    },
                    kind: r.kind,
                    source,
                    does,
                    off: self.state.off.get(&r.id).is_some_and(|h| h.contains(host)),
                }
            })
            .collect()
    }

    /// Of the rules that ran on a page (`ran`, from the probe), the polyfills that filled a
    /// feature the engine lacked there (`missing`, from the bootstrap).
    pub fn active_polyfills(&self, ran: &[String], missing: &[String]) -> Vec<String> {
        self.all()
            .filter(|(r, _)| r.kind == RuleKind::Polyfill && ran.contains(&r.id))
            .filter(|(r, _)| r.fills.iter().any(|f| missing.contains(f)))
            .map(|(r, _)| r.id.clone())
            .collect()
    }

    pub fn is_paused(&self, host: &str) -> bool {
        self.state.paused.contains(host)
    }

    /// Pause / resume every compat rule on `host`. Returns whether anything changed.
    pub fn set_paused(&mut self, host: &str, paused: bool) -> bool {
        let changed = if paused {
            self.state.paused.insert(host.to_owned())
        } else {
            self.state.paused.remove(host)
        };
        if changed {
            self.save_state();
        }
        changed
    }

    /// Turn one rule off / on for `host`.
    pub fn set_rule_off(&mut self, rule_id: &str, host: &str, off: bool) -> bool {
        if !self.all().any(|(r, _)| r.id == rule_id) {
            return false;
        }
        let changed = if off {
            self.state
                .off
                .entry(rule_id.to_owned())
                .or_default()
                .insert(host.to_owned())
        } else {
            let changed = self
                .state
                .off
                .get_mut(rule_id)
                .is_some_and(|h| h.remove(host));
            self.state.off.retain(|_, h| !h.is_empty());
            changed
        };
        if changed {
            self.save_state();
        }
        changed
    }

    /// Save (or replace) a local rule after validating it as a local rule.
    pub fn save_local_rule(&mut self, rule: Rule) -> Result<(), String> {
        rule.validate(Source::Local).map_err(|e| e.to_string())?;
        let local = self.local_mut();
        local.rules.retain(|r| r.id != rule.id);
        local.rules.push(rule);
        local.version += 1;
        self.save_local()
    }

    /// Delete a local rule. Returns false if there is no local rule with that id.
    pub fn remove_local_rule(&mut self, id: &str) -> Result<bool, String> {
        let local = self.local_mut();
        let before = local.rules.len();
        local.rules.retain(|r| r.id != id);
        if local.rules.len() == before {
            return Ok(false);
        }
        local.version += 1;
        self.state.off.remove(id);
        self.save_state();
        self.save_local().map(|_| true)
    }

    pub fn is_local_rule(&self, id: &str) -> bool {
        self.local().rules.iter().any(|r| r.id == id)
    }

    fn save_local(&self) -> Result<(), String> {
        let Some(dir) = &self.dir else { return Ok(()) };
        write_atomic(&dir.join(LOCAL_FILE), &self.local().to_toml()).map_err(|e| e.to_string())
    }

    fn save_state(&self) {
        let Some(dir) = &self.dir else { return };
        let text = serde_json::to_string_pretty(&self.state).unwrap_or_default();
        if let Err(e) = write_atomic(&dir.join(STATE_FILE), &text) {
            log::warn!("compat state not saved: {e}");
        }
    }

    /// Number of rules by source, for the startup log.
    pub fn counts(&self) -> Vec<(Source, usize)> {
        self.sets
            .iter()
            .map(|s| (s.source, s.file.rules.len()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mb-compat-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn site_rule(id: &str, host: &str) -> Rule {
        Rule {
            id: id.into(),
            title: "t".into(),
            kind: RuleKind::Ai,
            hosts: vec![format!("={host}")],
            exclude_hosts: vec![],
            paths: vec![],
            ua: None,
            css: Some("a{}".into()),
            js: Some("window.__x = 1;".into()),
            block: vec!["cdn.example.net/bad.js".into()],
            mode: None,
            enabled: true,
            fills: vec![],
            note: String::new(),
            created_ms: None,
        }
    }

    #[test]
    fn core_rules_parse() {
        let core = RulesetFile::parse(CORE_RULES, Source::Builtin).unwrap();
        assert!(!core.rules.is_empty());
        // Polyfills and site rules only; built-in site rules (engine-bug workarounds
        // reproduced on named sites) never target every site.
        assert!(core.rules.iter().all(|r| r.kind == RuleKind::Polyfill
            || (r.kind == RuleKind::Site && !r.applies_everywhere())));
        assert!(core.rules.iter().any(|r| r.kind == RuleKind::Site));
        // Every polyfill names a feature the bootstrap checks.
        let boot = bundle::BOOTSTRAP_TEMPLATE;
        for r in core.rules.iter().filter(|r| r.kind == RuleKind::Polyfill) {
            assert!(!r.fills.is_empty(), "{}", r.id);
            for f in &r.fills {
                assert!(boot.contains(&format!("[\"{f}\"")), "{}: {f}", r.id);
            }
        }
        let (c, _) = Compat::load(None, session_key(9));
        let ran = vec![
            "polyfill.css-escape".to_owned(),
            "polyfill.element-animate".to_owned(),
        ];
        assert_eq!(
            c.active_polyfills(&ran, &["Element.animate".to_owned()]),
            vec!["polyfill.element-animate"]
        );
    }

    /// rules/js/*.js are the sources of the larger polyfills; core.toml must carry them
    /// verbatim (run `python3 rules/js/sync_core.py` after editing one).
    #[test]
    fn core_rules_match_js_sources() {
        let sources: &[(&str, &str)] = &[
            (
                "polyfill.intersection-observer",
                include_str!("../../../rules/js/intersection-observer.js"),
            ),
            (
                "polyfill.document-fonts",
                include_str!("../../../rules/js/document-fonts.js"),
            ),
            (
                "polyfill.check-visibility",
                include_str!("../../../rules/js/check-visibility.js"),
            ),
            (
                "polyfill.visual-viewport",
                include_str!("../../../rules/js/visual-viewport.js"),
            ),
            (
                "polyfill.popover",
                include_str!("../../../rules/js/popover.js"),
            ),
            (
                "polyfill.offscreen-canvas",
                include_str!("../../../rules/js/offscreen-canvas.js"),
            ),
            (
                "polyfill.indexeddb",
                include_str!("../../../rules/js/indexeddb.js"),
            ),
            (
                "polyfill.notification",
                include_str!("../../../rules/js/notification.js"),
            ),
            (
                "polyfill.dynamics-compressor",
                include_str!("../../../rules/js/dynamics-compressor.js"),
            ),
            (
                "polyfill.svg-geometry",
                include_str!("../../../rules/js/svg-geometry.js"),
            ),
            (
                "polyfill.console-methods",
                include_str!("../../../rules/js/console-methods.js"),
            ),
            (
                "polyfill.performance-measure",
                include_str!("../../../rules/js/performance-measure.js"),
            ),
            (
                "site.css-animation-implicit-end",
                include_str!("../../../rules/js/css-animation-end.js"),
            ),
            (
                "site.broadcast-channel-frame-relay",
                include_str!("../../../rules/js/broadcast-channel-frame-relay.js"),
            ),
        ];
        let core = RulesetFile::parse(CORE_RULES, Source::Builtin).unwrap();
        for (id, js) in sources {
            let rule = core
                .rules
                .iter()
                .find(|r| r.id == *id)
                .unwrap_or_else(|| panic!("{id} missing"));
            // Compare with line endings normalised: a Windows checkout (core.autocrlf) gives
            // the .js sources CRLF while the TOML string comes back with LF.
            let lf = |s: &str| s.replace("\r\n", "\n");
            assert_eq!(
                rule.js.as_deref().map(lf),
                Some(lf(js)),
                "{id}: run rules/js/sync_core.py"
            );
        }
        // Each one is in the all-sites bundle.
        let (c, _) = Compat::load(None, session_key(3));
        let all = c.user_scripts().join("\n");
        for (id, _) in sources {
            assert!(
                all.contains(&format!("// MythBrowse compat rule {id}\n")),
                "{id}"
            );
        }
    }

    #[test]
    fn bundle_starts_with_bootstrap() {
        let (c, w) = Compat::load(None, session_key(1));
        assert!(w.is_empty(), "{w:?}");
        let scripts = c.user_scripts();
        assert!(scripts[0].contains("__mbc_0000000000000001"));
        assert!(scripts.len() > 1);
        assert!(c.probe_script().contains("__mbc_0000000000000001"));
    }

    #[test]
    fn local_rules_persist_and_apply_only_to_their_host() {
        let dir = tmp();
        let (mut c, _) = Compat::load(Some(&dir), session_key(2));
        c.save_local_rule(site_rule("ai.example-com.1", "example.com"))
            .unwrap();
        let (c2, w) = Compat::load(Some(&dir), session_key(3));
        assert!(w.is_empty(), "{w:?}");
        assert!(c2.is_local_rule("ai.example-com.1"));
        let page = u("https://example.com/a");
        assert!(c2.page_script(&page).unwrap().contains("window.__x = 1;"));
        assert!(c2.page_script(&u("https://www.example.com/")).is_none());
        assert_eq!(
            c2.blocked_by(&page, &u("https://cdn.example.net/bad.js")),
            Some("ai.example-com.1")
        );
        assert_eq!(
            c2.blocked_by(
                &u("https://other.com/"),
                &u("https://cdn.example.net/bad.js")
            ),
            None
        );
        // Local JS never reaches the shared bundle.
        assert!(!c2.user_scripts().concat().contains("window.__x = 1;"));
        let listed = c2.rules_for(&page, false);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].does, vec!["css", "js", "block"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pausing_and_turning_rules_off() {
        let dir = tmp();
        let (mut c, _) = Compat::load(Some(&dir), session_key(4));
        c.save_local_rule(site_rule("ai.a.1", "example.com"))
            .unwrap();
        let page = u("https://example.com/");
        assert!(c.set_rule_off("ai.a.1", "example.com", true));
        assert!(c.page_script(&page).is_none());
        assert!(c.rules_for(&page, false)[0].off);
        assert!(c.set_rule_off("ai.a.1", "example.com", false));
        assert!(c.page_script(&page).is_some());
        assert!(c.set_paused("example.com", true));
        assert!(c.page_script(&page).is_none());
        assert!(
            c.user_scripts()[1..]
                .iter()
                .all(|s| s.contains("\"=example.com\""))
        );
        let (c2, _) = Compat::load(Some(&dir), session_key(5));
        assert!(c2.is_paused("example.com"));
        assert!(!c.set_rule_off("no.such.rule", "example.com", true));
        let (mut c3, _) = Compat::load(Some(&dir), session_key(6));
        assert_eq!(c3.remove_local_rule("ai.a.1"), Ok(true));
        assert_eq!(c3.remove_local_rule("ai.a.1"), Ok(false));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bad_local_file_is_a_warning_not_a_failure() {
        let dir = tmp();
        std::fs::create_dir_all(dir.join(DIR)).unwrap();
        std::fs::write(dir.join(DIR).join(LOCAL_FILE), "schema = 1\nid = \"local\"\nversion = 1\n[[rule]]\nid = \"x\"\nhosts = [\"*\"]\nkind = \"polyfill\"\njs = \"1\"\n").unwrap();
        let (c, w) = Compat::load(Some(&dir), session_key(7));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(!c.is_local_rule("x"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remote_rules_need_a_trusted_signature() {
        let (mut c, _) = Compat::load(None, session_key(8));
        let toml = "schema = 1\nid = \"remote\"\nversion = 5\n[[rule]]\nid = \"r.a\"\nhosts = [\"example.org\"]\ncss = \"a{}\"\n";
        let (private, public) = sign::generate().unwrap();
        let sig = sign::sign(&private, toml.as_bytes()).unwrap();
        // Not trusted by this build.
        assert!(c.accept_remote(toml, &sig, sign::TRUSTED_KEYS).is_err());
        let id: &'static str = Box::leak(sign::key_id(&public).into_boxed_str());
        let keys = [sign::TrustedKey {
            id,
            public: public.as_slice().try_into().unwrap(),
        }];
        assert!(c.accept_remote(toml, &sig, &keys).is_ok());
        assert_eq!(
            c.rules_for(&u("https://example.org/"), false)[0].source,
            Source::Remote
        );
        let older = toml.replace("version = 5", "version = 4");
        let sig_old = sign::sign(&private, older.as_bytes()).unwrap();
        assert!(
            c.accept_remote(&older, &sig_old, &keys)
                .unwrap_err()
                .contains("older")
        );
    }

    #[test]
    fn downloaded_rules_are_kept_and_reused() {
        let dir = tmp();
        let (mut c, _) = Compat::load(Some(&dir), session_key(9));
        let toml = "schema = 1\nid = \"remote\"\nversion = 7\n[[rule]]\nid = \"r.b\"\nhosts = [\"example.net\"]\ncss = \"b{}\"\n";
        let (private, public) = sign::generate().unwrap();
        let sig = sign::sign(&private, toml.as_bytes()).unwrap();
        let id: &'static str = Box::leak(sign::key_id(&public).into_boxed_str());
        let keys = [sign::TrustedKey {
            id,
            public: public.as_slice().try_into().unwrap(),
        }];
        assert!(c.install_remote(toml, &sig, sign::TRUSTED_KEYS).is_err());
        assert!(
            !dir.join(DIR).join(REMOTE_FILE).exists(),
            "refused rules are not kept"
        );
        assert_eq!(c.install_remote(toml, &sig, &keys), Ok(Some(id.to_owned())));
        assert_eq!(c.remote_version(), Some(7));
        assert_eq!(c.install_remote(toml, &sig, &keys), Ok(None), "unchanged");
        // A tampered download is refused and the good copy stays.
        let evil = toml.replace("b{}", "b{display:none}");
        assert!(c.install_remote(&evil, &sig, &keys).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(DIR).join(REMOTE_FILE)).unwrap(),
            toml
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
