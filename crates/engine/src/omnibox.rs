// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Address-bar input resolution: URLs navigate, free text becomes an AI prompt, and a
//! leading `?` forces a web search.

use url::Url;

/// Search engine used for `?`-prefixed input (and as the AI stub's fallback).
pub const SEARCH_URL: &str = "https://duckduckgo.com/";

/// Schemes the address bar will navigate to directly.
const NAVIGABLE_SCHEMES: &[&str] = &["http", "https", "file", "data", "about"];

/// What the user meant by what they typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmniboxTarget {
    /// Navigate to this URL.
    Url(Url),
    /// Free text: an AI prompt (answered on the AI chat home).
    Prompt(String),
    /// Explicit web search (`? terms`).
    Search(String),
}

impl OmniboxTarget {
    /// The URL to load for a URL or search target. Prompts have no URL of their own:
    /// the shell routes them to the AI chat home.
    pub fn web_url(&self) -> Option<Url> {
        match self {
            OmniboxTarget::Url(u) => Some(u.clone()),
            OmniboxTarget::Search(q) => Some(search_url(q)),
            OmniboxTarget::Prompt(_) => None,
        }
    }
}

/// Settings > General > Search engine: where `?` searches (and "Search the web for …") go.
/// Address-bar *suggestions* stay on DuckDuckGo (crate::suggest) whatever is chosen here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchEngine {
    DuckDuckGo,
    Google,
    Bing,
    Brave,
    Startpage,
    Ecosia,
}

impl SearchEngine {
    pub const ALL: [SearchEngine; 6] = [
        SearchEngine::DuckDuckGo,
        SearchEngine::Google,
        SearchEngine::Bing,
        SearchEngine::Brave,
        SearchEngine::Startpage,
        SearchEngine::Ecosia,
    ];

    pub fn id(self) -> &'static str {
        match self {
            SearchEngine::DuckDuckGo => "duckduckgo",
            SearchEngine::Google => "google",
            SearchEngine::Bing => "bing",
            SearchEngine::Brave => "brave",
            SearchEngine::Startpage => "startpage",
            SearchEngine::Ecosia => "ecosia",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SearchEngine::DuckDuckGo => "DuckDuckGo",
            SearchEngine::Google => "Google",
            SearchEngine::Bing => "Bing",
            SearchEngine::Brave => "Brave Search",
            SearchEngine::Startpage => "Startpage",
            SearchEngine::Ecosia => "Ecosia",
        }
    }

    /// The search page; the query goes in `q`.
    pub fn base(self) -> &'static str {
        match self {
            SearchEngine::DuckDuckGo => SEARCH_URL,
            SearchEngine::Google => "https://www.google.com/search",
            SearchEngine::Bing => "https://www.bing.com/search",
            SearchEngine::Brave => "https://search.brave.com/search",
            SearchEngine::Startpage => "https://www.startpage.com/do/search",
            SearchEngine::Ecosia => "https://www.ecosia.org/search",
        }
    }

    pub fn parse(id: &str) -> Option<SearchEngine> {
        SearchEngine::ALL.into_iter().find(|e| e.id() == id)
    }
}

static ENGINE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Choose the engine for every later [`search_url`] (the shell applies the saved setting at
/// start and when it changes).
pub fn set_search_engine(e: SearchEngine) {
    let i = SearchEngine::ALL.iter().position(|x| *x == e).unwrap_or(0);
    ENGINE.store(i as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn search_engine() -> SearchEngine {
    let i = ENGINE.load(std::sync::atomic::Ordering::Relaxed) as usize;
    SearchEngine::ALL
        .get(i)
        .copied()
        .unwrap_or(SearchEngine::DuckDuckGo)
}

/// Web-search URL for `query` on the chosen engine.
pub fn search_url(query: &str) -> Url {
    search_url_with(search_engine(), query)
}

pub fn search_url_with(engine: SearchEngine, query: &str) -> Url {
    Url::parse_with_params(engine.base(), &[("q", query)]).expect("engine URLs are valid")
}

/// Classify address-bar input. Returns `None` for empty input.
pub fn resolve(input: &str) -> Option<OmniboxTarget> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix('?') {
        let rest = rest.trim();
        return (!rest.is_empty()).then(|| OmniboxTarget::Search(rest.to_owned()));
    }

    // Explicit, navigable URL. (Note "localhost:8080" parses with scheme "localhost",
    // hence the scheme allowlist.)
    if let Ok(url) = Url::parse(text) {
        if NAVIGABLE_SCHEMES.contains(&url.scheme()) {
            return Some(OmniboxTarget::Url(url));
        }
    }

    if !text.contains(char::is_whitespace) && looks_like_host(text) {
        let host_part = text.split(['/', '?', '#']).next().unwrap_or(text);
        let scheme = if is_local_host(host_part) {
            "http"
        } else {
            "https"
        };
        if let Ok(url) = Url::parse(&format!("{scheme}://{text}")) {
            return Some(OmniboxTarget::Url(url));
        }
    }

    Some(OmniboxTarget::Prompt(text.to_owned()))
}

fn looks_like_host(text: &str) -> bool {
    let host_port = text.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_port.rsplit_once(':').map_or(host_port, |(h, port)| {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            host_port
        }
    });
    if host == "localhost" {
        return true;
    }
    // Needs a dot, no empty labels, and a TLD that is not all digits unless it is an IPv4.
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    let ok_chars = labels
        .iter()
        .all(|l| l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    let is_ipv4 = labels.len() == 4 && labels.iter().all(|l| l.parse::<u8>().is_ok());
    let tld = labels.last().unwrap();
    ok_chars && (is_ipv4 || tld.bytes().any(|b| b.is_ascii_alphabetic()))
}

fn is_local_host(host_port: &str) -> bool {
    let host = host_port.split(':').next().unwrap_or("");
    host == "localhost" || host.starts_with("127.") || host.starts_with("192.168.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_engines() {
        for e in SearchEngine::ALL {
            assert_eq!(SearchEngine::parse(e.id()), Some(e));
            let u = search_url_with(e, "a b&c");
            assert_eq!(u.scheme(), "https");
            assert_eq!(
                u.query_pairs()
                    .find(|(k, _)| k == "q")
                    .map(|(_, v)| v.into_owned()),
                Some("a b&c".to_owned())
            );
        }
        assert_eq!(SearchEngine::parse("yahoo"), None);
        assert_eq!(
            search_url_with(SearchEngine::Google, "servo").as_str(),
            "https://www.google.com/search?q=servo"
        );
        // The process-wide default stays DuckDuckGo (other tests rely on it).
        assert_eq!(search_engine(), SearchEngine::DuckDuckGo);
    }

    fn url(s: &str) -> String {
        resolve(s).unwrap().web_url().unwrap().to_string()
    }

    #[test]
    fn explicit_urls() {
        assert_eq!(url("https://servo.org"), "https://servo.org/");
        assert_eq!(
            url("  http://example.com/a?b=c "),
            "http://example.com/a?b=c"
        );
        assert_eq!(url("file:///tmp/x.html"), "file:///tmp/x.html");
        assert_eq!(url("about:blank"), "about:blank");
    }

    #[test]
    fn bare_domains_get_https() {
        assert_eq!(url("servo.org"), "https://servo.org/");
        assert_eq!(
            url("news.ycombinator.com/item?id=1"),
            "https://news.ycombinator.com/item?id=1"
        );
    }

    #[test]
    fn local_hosts_get_http() {
        assert_eq!(url("localhost:8080"), "http://localhost:8080/");
        assert_eq!(url("127.0.0.1:3000/x"), "http://127.0.0.1:3000/x");
    }

    #[test]
    fn text_becomes_prompt() {
        assert_eq!(
            resolve("what is servo").unwrap(),
            OmniboxTarget::Prompt("what is servo".into())
        );
        assert_eq!(resolve("rust 1.92").unwrap().web_url(), None);
        assert!(matches!(resolve("1.5").unwrap(), OmniboxTarget::Prompt(_)));
        assert!(matches!(
            resolve("hello").unwrap(),
            OmniboxTarget::Prompt(_)
        ));
        assert!(matches!(
            resolve("javascript:alert(1)").unwrap(),
            OmniboxTarget::Prompt(_)
        ));
    }

    #[test]
    fn question_mark_forces_search() {
        assert_eq!(
            resolve("? rust 1.92").unwrap(),
            OmniboxTarget::Search("rust 1.92".into())
        );
        assert_eq!(url("?servo.org"), "https://duckduckgo.com/?q=servo.org");
        assert_eq!(resolve(" ?  "), None);
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(resolve("   "), None);
    }
}
