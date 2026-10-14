// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Turning rules into what Servo runs.
//!
//! Servo 0.1.4's `UserContentManager` holds user scripts / stylesheets for every page of a
//! webview; it has no per-URL matching. So each rule becomes its **own** user script that
//! gates itself on `location` (via the bootstrap's `match`, same semantics as
//! [`crate::matcher`]) before doing anything. One script per rule means a broken rule cannot
//! take the others down.
//!
//! Only trusted rules (built in, or signed remote) put code into these all-pages scripts.
//! For local rules (the user's own, including AI patches) only *data* goes in: CSS text, a UA
//! string and a mode name, each embedded as a JSON string literal, so it cannot escape the
//! gate. Their JS is run by the shell with `evaluate_javascript` in pages whose URL Rust
//! itself matched ([`page_script`]), never in the shared bundle.

use crate::rules::{Rule, Source};

pub(crate) const BOOTSTRAP_TEMPLATE: &str = include_str!("js/bootstrap.js");
/// `(function (mode) {...})`: reader / simplified / off.
pub const READER_FUNCTION: &str = include_str!("js/reader.js");

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned())
}

fn js_list(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_owned())
}

pub(crate) fn bootstrap(key: &str) -> String {
    BOOTSTRAP_TEMPLATE
        .replace("__MB_READER__", READER_FUNCTION.trim())
        .replace("__MB_KEY__", key)
}

/// A standalone script applying `mode` ("reader" / "simplified" / "off") to the page now.
pub fn mode_script(mode: &str) -> String {
    format!("({})({});", READER_FUNCTION.trim(), js_str(mode))
}

/// One rule's all-pages user script. `extra_exclude` adds host patterns (paused sites,
/// sites where the user turned this rule off). `None` if nothing is left to put in it.
pub(crate) fn rule_script(
    rule: &Rule,
    source: Source,
    extra_exclude: &[String],
    key: &str,
) -> Option<String> {
    let with_code = source.trusted();
    let css = rule.css.as_deref().filter(|c| !c.trim().is_empty());
    let js = rule
        .js
        .as_deref()
        .filter(|j| with_code && !j.trim().is_empty());
    let ua = rule.user_agent();
    if css.is_none() && js.is_none() && ua.is_none() && rule.mode.is_none() {
        return None;
    }
    let mut exclude = rule.exclude_hosts.clone();
    exclude.extend(extra_exclude.iter().cloned());
    let id = js_str(&rule.id);
    let mut s = String::with_capacity(256 + css.map_or(0, str::len) + js.map_or(0, str::len));
    s.push_str("// MythBrowse compat rule ");
    s.push_str(&rule.id.replace(['\n', '\r'], " "));
    s.push_str("\n(function () {\n\"use strict\";\n");
    s.push_str(&format!(
        "var C = window[{}];\nif (!C || !C.match({}, {}, {})) return;\nC.ran({id});\n",
        js_str(key),
        js_list(&rule.hosts),
        js_list(&exclude),
        js_list(&rule.paths),
    ));
    if let Some(ua) = &ua {
        s.push_str(&format!(
            "try {{ C.ua({}, {}, {}); }} catch (e) {{ C.fail({id}, e); }}\n",
            js_str(ua),
            js_str(&crate::ua::app_version(ua)),
            js_str(crate::ua::platform_for(ua))
        ));
    }
    if let Some(css) = css {
        s.push_str(&format!(
            "try {{ C.css({id}, {}); }} catch (e) {{ C.fail({id}, e); }}\n",
            js_str(css)
        ));
    }
    if let Some(mode) = rule.mode {
        s.push_str(&format!("C.mode({});\n", js_str(mode.as_str())));
    }
    if let Some(js) = js {
        s.push_str("try {\n(function () {\n");
        s.push_str(js);
        s.push_str(&format!(
            "\n}}).call(window);\n}} catch (e) {{ C.fail({id}, e); }}\n"
        ));
    }
    s.push_str("})();\n");
    Some(s)
}

/// JS of local rules for one page, run by the shell via `evaluate_javascript` after Rust
/// matched the page's URL. Each rule is isolated in its own try block.
pub(crate) fn page_script(rules: &[&Rule], key: &str) -> Option<String> {
    let parts: Vec<String> = rules
        .iter()
        .filter_map(|r| {
            let js = r.js.as_deref().filter(|j| !j.trim().is_empty())?;
            let id = js_str(&r.id);
            Some(format!(
                "try {{\n(function () {{\n{js}\n}}).call(window);\nif (C) C.ran({id});\n}} catch (e) {{ if (C) C.fail({id}, e); }}\n"
            ))
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "(function () {{\n\"use strict\";\nvar C = window[{}];\n{}}})();\n",
        js_str(key),
        parts.concat()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RuleKind;

    fn rule() -> Rule {
        Rule {
            id: "t.a".into(),
            title: String::new(),
            kind: RuleKind::Site,
            hosts: vec!["example.com".into()],
            exclude_hosts: vec![],
            paths: vec![],
            ua: Some("firefox".into()),
            css: Some("body{color:red}</script>".into()),
            js: Some("window.x = 1;".into()),
            block: vec![],
            mode: None,
            enabled: true,
            fills: vec![],
            note: String::new(),
            created_ms: None,
        }
    }

    #[test]
    fn local_rules_never_put_code_in_the_bundle() {
        let s = rule_script(&rule(), Source::Local, &[], "k").unwrap();
        assert!(!s.contains("window.x = 1"));
        assert!(s.contains("C.css(\"t.a\", \"body{color:red}</script>\")"));
        assert!(s.contains("Firefox/"));
        let t = rule_script(&rule(), Source::Builtin, &["=a.example.com".into()], "k").unwrap();
        assert!(t.contains("window.x = 1"));
        assert!(t.contains("[\"=a.example.com\"]"));
    }

    #[test]
    fn data_is_escaped() {
        let mut r = rule();
        r.css = Some("\"); alert(1); (\"".into());
        let s = rule_script(&r, Source::Local, &[], "k").unwrap();
        assert!(s.contains(r#"C.css("t.a", "\"); alert(1); (\"")"#));
    }

    #[test]
    fn empty_local_rule_has_no_script() {
        let mut r = rule();
        r.css = None;
        r.ua = None;
        assert!(rule_script(&r, Source::Local, &[], "k").is_none());
        let page = page_script(&[&r], "k").unwrap();
        assert!(page.contains("window.x = 1;"));
        r.js = None;
        assert!(page_script(&[&r], "k").is_none());
    }

    #[test]
    fn bootstrap_is_filled_in() {
        let b = bootstrap("__mbk_123");
        assert!(b.contains("\"__mbk_123\""));
        assert!(!b.contains("__MB_READER__") && !b.contains("__MB_KEY__"));
        assert!(mode_script("reader").ends_with("(\"reader\");"));
    }
}
