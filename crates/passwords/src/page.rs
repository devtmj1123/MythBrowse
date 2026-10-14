// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The page side of the password manager.
//!
//! * [`page_script`]: the user script (`js/pw.js`) with this session's nonce and hook name.
//! * [`parse_message`]: the console messages it sends (`"\0mbpw:<nonce>:" + JSON`). The
//!   shell checks every console message with [`is_pw_message`] **first** and never logs,
//!   stores or forwards one (they can carry a submitted password).
//! * [`fill_script`]: the self-contained fill script, run only after the user picked an
//!   account in the browser's panel. It carries the password, so it is zeroed after use.

use mythbrowse_ai::secrets::Secret;
use serde_json::Value;
use zeroize::Zeroizing;

const PAGE_JS: &str = include_str!("js/pw.js");
const FILL_JS: &str = include_str!("js/fill.js");
const MARK: &str = "\u{0}mbpw:";

/// Random per browser session (hex). The nonce marks genuine messages; the hook name is the
/// window property through which the shell shows / hides the key icon.
#[derive(Clone)]
pub struct PageKeys {
    pub nonce: String,
    pub hook: String,
}

impl PageKeys {
    pub fn random() -> Self {
        let hex = |n: usize| {
            let mut b = vec![0u8; n];
            let _ = getrandom::getrandom(&mut b);
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        };
        Self {
            nonce: hex(16),
            hook: format!("__mb{}", hex(8)),
        }
    }
}

pub fn page_script(keys: &PageKeys) -> String {
    PAGE_JS
        .replace("__MB_PW_NONCE__", &keys.nonce)
        .replace("__MB_PW_HOOK__", &keys.hook)
}

/// A console message from the page script (whatever its nonce). Such messages are never
/// logged or kept, even forged ones.
pub fn is_pw_message(message: &str) -> bool {
    message.starts_with(MARK)
}

#[derive(Debug)]
pub enum PageMsg {
    /// The page has `count` password fields (0: none any more).
    Forms {
        url: String,
        count: u32,
        signup: bool,
    },
    /// The user focused a login field (trusted event).
    Focus { url: String },
    /// The user clicked the key icon in a login field (trusted event).
    Key { url: String },
    /// A login form was submitted with these values.
    Submit {
        url: String,
        action: String,
        username: String,
        password: Secret,
        /// More than one password field had a value (sign-up / change password form).
        new_password: bool,
    },
}

/// Parse a page-script message; `None` if it is not one or the nonce is wrong.
pub fn parse_message(message: &str, keys: &PageKeys) -> Option<PageMsg> {
    let rest = message.strip_prefix(MARK)?;
    let rest = rest.strip_prefix(keys.nonce.as_str())?.strip_prefix(':')?;
    let json = Zeroizing::new(rest.to_owned());
    let v: Value = serde_json::from_str(&json).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let url = s("o");
    Some(match v.get("t").and_then(Value::as_str)? {
        "forms" => PageMsg::Forms {
            url,
            count: v.get("n").and_then(Value::as_u64).unwrap_or(0).min(1000) as u32,
            signup: v.get("signup").and_then(Value::as_bool).unwrap_or(false),
        },
        "focus" => PageMsg::Focus { url },
        "key" => PageMsg::Key { url },
        "submit" => {
            let p = v.get("p").and_then(Value::as_str)?;
            if p.is_empty() || p.len() > 4096 {
                return None;
            }
            PageMsg::Submit {
                url,
                action: s("a"),
                username: s("u").chars().take(512).collect(),
                password: Secret::new(p),
                new_password: v.get("n").and_then(Value::as_u64).unwrap_or(1) > 1,
            }
        }
        _ => return None,
    })
}

/// JS that tells the page script how many logins it has (shows / hides the key icon).
pub fn has_script(keys: &PageKeys, count: usize) -> String {
    format!(
        "(function(){{var h=window[{}];if(h&&h.has)h.has({count});}})()",
        serde_json::to_string(&keys.hook).unwrap_or_default()
    )
}

/// The fill script for `origin` (`https://host[:port]`, as `location.origin` writes it).
pub fn fill_script(origin: &str, username: &str, password: &Secret) -> Zeroizing<String> {
    let js = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let pass = Zeroizing::new(js(password.expose()));
    Zeroizing::new(
        FILL_JS
            .replace("__ORIGIN__", &js(origin))
            .replace("__USER__", &js(username))
            .replace("__PASS__", &pass),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> PageKeys {
        PageKeys {
            nonce: "abc123".into(),
            hook: "__mbhook".into(),
        }
    }

    #[test]
    fn script_placeholders_filled() {
        let k = PageKeys::random();
        let s = page_script(&k);
        assert!(!s.contains("__MB_PW_"));
        assert!(s.contains(&k.nonce) && s.contains(&k.hook));
        assert_eq!(k.nonce.len(), 32);
        let f = fill_script(
            "https://example.com",
            "a\"b",
            &Secret::new("p'w\"</script>"),
        );
        assert!(!f.contains("__ORIGIN__") && !f.contains("__PASS__") && !f.contains("__USER__"));
        assert!(f.contains(r#""p'w\"</script>""#));
        assert!(has_script(&k, 2).contains("h.has(2)"));
    }

    #[test]
    fn messages() {
        let k = keys();
        let m = |j: &str| format!("\u{0}mbpw:abc123:{j}");
        match parse_message(
            &m(
                r#"{"t":"submit","o":"https://e.com/login","u":"al","p":"pw1","a":"https://e.com/s","n":1}"#,
            ),
            &k,
        ) {
            Some(PageMsg::Submit {
                url,
                username,
                password,
                new_password,
                action,
            }) => {
                assert_eq!(url, "https://e.com/login");
                assert_eq!(username, "al");
                assert_eq!(password.expose(), "pw1");
                assert_eq!(action, "https://e.com/s");
                assert!(!new_password);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_message(&m(r#"{"t":"forms","n":2,"signup":true,"o":"x"}"#), &k),
            Some(PageMsg::Forms {
                count: 2,
                signup: true,
                ..
            })
        ));
        assert!(matches!(
            parse_message(&m(r#"{"t":"key","o":"x"}"#), &k),
            Some(PageMsg::Key { .. })
        ));
        // Wrong nonce, not ours, empty password, unknown type: ignored.
        assert!(parse_message("\u{0}mbpw:zzz:{\"t\":\"key\"}", &k).is_none());
        assert!(parse_message("hello", &k).is_none());
        assert!(parse_message(&m(r#"{"t":"submit","p":""}"#), &k).is_none());
        assert!(parse_message(&m(r#"{"t":"rm"}"#), &k).is_none());
        // Every marked message is recognised for suppression, forged or not.
        assert!(is_pw_message("\u{0}mbpw:zzz:{}"));
        assert!(!is_pw_message("mbpw:"));
    }
}
