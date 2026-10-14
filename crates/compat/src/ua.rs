// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
//! Which user-agent string MythBrowse sends, and what `navigator` says to match it.
//!
//! **Policy (2026-10-13).** Sites should treat MythBrowse like a mainstream browser instead of
//! refusing it ("upgrade to a supported browser"). Servo's own UA carries a `Servo/x.y.z` token
//! that UA-sniffing sites reject, so the default is a standard **Firefox desktop** UA with the
//! same `rv:` Servo itself claims (140). Firefox, not Chrome: Servo shares Gecko's JS engine
//! and much of its feature set, while a Chrome UA makes sites ship Chrome-only code paths.
//!
//! Choices (`ua_mode` pref, `/api/prefs`): `firefox` (default), `chrome`, `servo` (Servo's own
//! string). A compat rule can override the string per site (`ua`: a preset name or a full
//! string), e.g. Chrome only for a site that gates purely on the UA and works with it.
//!
//! **What this is not.** Only the UA string (and the `navigator.userAgent` / `appVersion` /
//! `platform` values that must agree with it) is chosen here. MythBrowse does not forge
//! fingerprints, `navigator.webdriver`, TLS / HTTP-2 fingerprints, client hints, or anything
//! else aimed at evading bot detection.
//!
//! Std-only so it is unit-tested without Servo (`cargo test -p mythbrowse-compat ua`).

/// The Gecko / Firefox version Servo 0.1.4 claims in its own UA (`rv:140.0`).
pub const GECKO_RV: &str = "140.0";
/// A Chrome major version contemporary with `GECKO_RV` (Firefox 140 ESR: June 2025).
pub const CHROME_VERSION: &str = "140.0.0.0";
/// The Servo release we embed (its own UA names it).
pub const SERVO_VERSION: &str = "0.1.4";

/// The desktop OS a UA string describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    /// The OS this binary is built for.
    pub const fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }

    /// `navigator.platform` as Firefox and Chrome report it on this OS (x86-64 builds).
    pub fn platform(self) -> &'static str {
        match self {
            Os::Windows => "Win32",
            Os::Mac => "MacIntel",
            Os::Linux => "Linux x86_64",
        }
    }
}

/// The global choice (`ua_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UaMode {
    #[default]
    Firefox,
    Chrome,
    Servo,
}

impl UaMode {
    pub fn parse(s: &str) -> Option<UaMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "firefox" => Some(UaMode::Firefox),
            "chrome" => Some(UaMode::Chrome),
            "servo" => Some(UaMode::Servo),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            UaMode::Firefox => "firefox",
            UaMode::Chrome => "chrome",
            UaMode::Servo => "servo",
        }
    }

    /// The UA string for this mode on `os`.
    pub fn user_agent(self, os: Os) -> String {
        match self {
            UaMode::Firefox => firefox(os),
            UaMode::Chrome => chrome(os),
            UaMode::Servo => servo(os),
        }
    }
}

fn gecko_os(os: Os) -> &'static str {
    match os {
        Os::Windows => "Windows NT 10.0; Win64; x64",
        Os::Mac => "Macintosh; Intel Mac OS X 10.15",
        Os::Linux => "X11; Linux x86_64",
    }
}

/// `Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0`.
pub fn firefox(os: Os) -> String {
    format!(
        "Mozilla/5.0 ({}; rv:{GECKO_RV}) Gecko/20100101 Firefox/{GECKO_RV}",
        gecko_os(os)
    )
}

/// `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko)
/// Chrome/140.0.0.0 Safari/537.36` (Chrome's reduced UA).
pub fn chrome(os: Os) -> String {
    let p = match os {
        Os::Windows => "Windows NT 10.0; Win64; x64",
        Os::Mac => "Macintosh; Intel Mac OS X 10_15_7",
        Os::Linux => "X11; Linux x86_64",
    };
    format!(
        "Mozilla/5.0 ({p}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROME_VERSION} Safari/537.36"
    )
}

/// Servo's own desktop UA (what `servo_config` builds by default).
pub fn servo(os: Os) -> String {
    format!(
        "Mozilla/5.0 ({}; rv:{GECKO_RV}) Servo/{SERVO_VERSION} Firefox/{GECKO_RV}",
        gecko_os(os)
    )
}

/// A compat-rule `ua` preset name, resolved for `os`. `None`: not a preset (a full string).
pub fn preset(name: &str, os: Os) -> Option<String> {
    match name {
        "firefox" => Some(firefox(os)),
        "chrome" => Some(chrome(os)),
        "servo" => Some(servo(os)),
        "firefox-windows" => Some(firefox(Os::Windows)),
        "firefox-linux" => Some(firefox(Os::Linux)),
        "chrome-windows" => Some(chrome(Os::Windows)),
        _ => None,
    }
}

/// Whether `s` is acceptable as a custom UA string (printable ASCII, 10-512 chars).
pub fn valid_custom(s: &str) -> bool {
    (10..=512).contains(&s.len()) && s.chars().all(|c| (' '..='~').contains(&c))
}

/// The UA for one page: the site's compat-rule override (a preset or a valid custom string)
/// when there is one, else the global mode.
pub fn select(site_override: Option<&str>, mode: UaMode, os: Os) -> String {
    match site_override.map(str::trim).filter(|s| !s.is_empty()) {
        Some(o) => preset(o, os)
            .or_else(|| valid_custom(o).then(|| o.to_owned()))
            .unwrap_or_else(|| mode.user_agent(os)),
        None => mode.user_agent(os),
    }
}

/// `navigator.appVersion` that matches `ua`, as the browser the UA names reports it:
/// Firefox gives `5.0 (Windows)` / `5.0 (X11)` / `5.0 (Macintosh)`; Chrome (and anything
/// else) the UA without its `Mozilla/` prefix.
pub fn app_version(ua: &str) -> String {
    let gecko = ua.contains("Gecko/") && ua.contains("Firefox/") && !ua.contains("Servo/");
    if gecko {
        let os = if ua.contains("Windows") {
            "Windows"
        } else if ua.contains("Macintosh") {
            "Macintosh"
        } else {
            "X11"
        };
        return format!("5.0 ({os})");
    }
    ua.strip_prefix("Mozilla/").unwrap_or(ua).to_owned()
}

/// `navigator.platform` for `ua`: from the OS the UA names, else this machine's.
pub fn platform_for(ua: &str) -> &'static str {
    if ua.contains("Windows") {
        Os::Windows.platform()
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        Os::Mac.platform()
    } else if ua.contains("Linux") || ua.contains("X11") {
        Os::Linux.platform()
    } else {
        Os::current().platform()
    }
}

fn js_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A user script (every page, document start) making `navigator.appVersion` and
/// `navigator.platform` agree with the UA the HTTP header carries (Servo's pref already
/// drives `navigator.userAgent`; its `appVersion` is a bare `4.0` and its Linux `platform`
/// lacks the architecture). Nothing else on `navigator` is touched.
pub fn navigator_script(ua: &str) -> String {
    format!(
        "(function () {{\n  var NP = window.Navigator && Navigator.prototype;\n  if (!NP) return;\n  \
         function def(n, v) {{ try {{ Object.defineProperty(NP, n, {{ get: function () {{ return v; }}, configurable: true }}); }} catch (e) {{}} }}\n  \
         def(\"appVersion\", {});\n  def(\"platform\", {});\n}})();",
        js_str(&app_version(ua)),
        js_str(platform_for(ua)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_firefox_without_servo_token() {
        let ua = UaMode::default().user_agent(Os::Windows);
        assert_eq!(
            ua,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0"
        );
        assert!(!ua.contains("Servo"));
        assert_eq!(
            UaMode::Firefox.user_agent(Os::Linux),
            "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"
        );
    }

    #[test]
    fn modes_parse_and_round_trip() {
        for m in [UaMode::Firefox, UaMode::Chrome, UaMode::Servo] {
            assert_eq!(UaMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(UaMode::parse(" Chrome "), Some(UaMode::Chrome));
        assert_eq!(UaMode::parse("edge"), None);
        assert!(
            UaMode::Chrome
                .user_agent(Os::Windows)
                .contains("Chrome/140.0.0.0 Safari/537.36")
        );
        assert!(UaMode::Servo.user_agent(Os::Linux).contains("Servo/0.1.4"));
    }

    #[test]
    fn site_override_wins_over_mode() {
        let os = Os::Windows;
        assert_eq!(select(None, UaMode::Firefox, os), firefox(os));
        assert_eq!(select(Some("chrome"), UaMode::Firefox, os), chrome(os));
        assert_eq!(select(Some("servo"), UaMode::Chrome, os), servo(os));
        assert_eq!(select(Some("firefox"), UaMode::Servo, os), firefox(os));
        assert_eq!(
            select(Some("Custom/1.0 (test)"), UaMode::Firefox, os),
            "Custom/1.0 (test)"
        );
        // Bad custom strings fall back to the mode.
        assert_eq!(
            select(Some("bad\nua string"), UaMode::Chrome, os),
            chrome(os)
        );
        assert_eq!(select(Some("short"), UaMode::Firefox, os), firefox(os));
        assert_eq!(select(Some("  "), UaMode::Chrome, os), chrome(os));
    }

    #[test]
    fn navigator_values_agree_with_the_ua() {
        assert_eq!(app_version(&firefox(Os::Windows)), "5.0 (Windows)");
        assert_eq!(app_version(&firefox(Os::Linux)), "5.0 (X11)");
        assert_eq!(app_version(&firefox(Os::Mac)), "5.0 (Macintosh)");
        assert_eq!(
            app_version(&chrome(Os::Windows)),
            "5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
        );
        assert!(app_version(&servo(Os::Linux)).starts_with("5.0 (X11; Linux"));
        assert_eq!(platform_for(&firefox(Os::Windows)), "Win32");
        assert_eq!(platform_for(&chrome(Os::Linux)), "Linux x86_64");
        assert_eq!(platform_for(&chrome(Os::Mac)), "MacIntel");
        let s = navigator_script(&firefox(Os::Windows));
        assert!(s.contains("\"5.0 (Windows)\"") && s.contains("\"Win32\""));
        assert!(!s.contains("webdriver"), "never touches automation flags");
        assert_eq!(js_str("a\"b</c\n"), "\"a\\\"b\\u003c/c\\u000a\"");
    }

    #[test]
    fn presets_and_custom_validation() {
        assert_eq!(
            preset("firefox-linux", Os::Windows).unwrap(),
            firefox(Os::Linux)
        );
        assert!(preset("nope", Os::Linux).is_none());
        assert!(valid_custom("Custom/1.0 (test)"));
        assert!(!valid_custom("x"));
        assert!(!valid_custom("tab\there xxxxx"));
        assert!(!valid_custom(&"a".repeat(513)));
    }
}
