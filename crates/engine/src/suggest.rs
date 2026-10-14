// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Address-bar suggestions: what the dropdown under the omnibox lists while the user types.
//!
//! Sources: history (ranked by *frecency*: how often and how recently a page was visited),
//! bookmarks, open tabs ("Switch to tab"), web search suggestions (DuckDuckGo's suggest
//! endpoint, fetched by the shell off the main thread; can be turned off in Settings for
//! privacy) and an "Ask Myth" row. Ranking, merging, inline autocomplete and parsing the
//! suggest response are pure functions here, so they are unit-tested without Servo.

use serde::Serialize;
use url::Url;

use crate::omnibox::{self, OmniboxTarget};

/// Most rows the dropdown shows.
pub const MAX_ROWS: usize = 8;
/// Most search-suggestion rows among them.
pub const MAX_SEARCH_ROWS: usize = 4;
/// Most history / bookmark / tab rows among them.
pub const MAX_PAGE_ROWS: usize = 5;
/// DuckDuckGo's suggest endpoint (OpenSearch JSON with `type=list`).
pub const SUGGEST_ENDPOINT: &str = "https://duckduckgo.com/ac/";
/// Longest query sent to the suggest endpoint (characters).
pub const MAX_REMOTE_QUERY: usize = 120;
/// Half-life of a visit's weight in the frecency score, in days.
pub const FRECENCY_HALF_LIFE_DAYS: f64 = 14.0;

/// What a row does when chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Navigate to the typed address (first row when the text looks like a URL).
    Go,
    History,
    Bookmark,
    /// An open tab: switch to it instead of loading the page again.
    Tab,
    /// A web search suggestion (or the typed text as a search).
    Search,
    /// Send the typed text to Myth.
    AskMyth,
}

/// One dropdown row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub kind: Kind,
    /// Main text (page title, search phrase, the prompt).
    pub title: String,
    /// The URL the row opens (`Go`, `History`, `Bookmark`, `Tab`, `Search`); empty for
    /// `AskMyth`.
    pub url: String,
    /// The text the address bar submits when this row is chosen (`navigate` input).
    pub input: String,
    /// `Tab`: the tab to switch to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<u64>,
    /// `History`: the user may delete it (Shift+Delete).
    pub deletable: bool,
    #[serde(skip)]
    pub score: f64,
}

/// A history row as the store has it.
#[derive(Debug, Clone, PartialEq)]
pub struct PageHit {
    pub url: String,
    pub title: String,
    pub visit_count: u32,
    pub last_visit_ms: i64,
}

/// An open tab.
#[derive(Debug, Clone, PartialEq)]
pub struct TabHit {
    pub id: u64,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// Everything the ranker merges.
#[derive(Debug, Clone, Default)]
pub struct Sources<'a> {
    pub history: &'a [PageHit],
    pub bookmarks: &'a [(String, String)],
    pub tabs: &'a [TabHit],
    /// Search phrases from the suggest endpoint for this query (already parsed).
    pub remote: &'a [String],
}

/// The dropdown's content for one query.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ranked {
    pub query: String,
    pub items: Vec<Suggestion>,
    /// Inline autocomplete: the full text the address bar should show (typed text + the
    /// selected completion), when the best page match starts with what was typed.
    pub inline: Option<String>,
}

/// Frecency of a page: visits weigh logarithmically, recency halves the weight every
/// [`FRECENCY_HALF_LIFE_DAYS`]. Always finite and >= 0.
pub fn frecency(visit_count: u32, last_visit_ms: i64, now_ms: i64) -> f64 {
    let age_days = ((now_ms - last_visit_ms).max(0) as f64) / 86_400_000.0;
    let decay = 0.5f64.powf(age_days / FRECENCY_HALF_LIFE_DAYS);
    100.0 * (1.0 + visit_count as f64).ln() * decay
}

/// `https://www.example.com/a` -> `example.com/a` (what people type).
pub fn strip_url(url: &str) -> &str {
    let s = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    s.strip_prefix("www.").unwrap_or(s)
}

/// How well `query` matches a page: 0 = no match. Prefix of the bare URL (what inline
/// autocomplete completes) beats a host-word prefix, which beats a substring of the URL
/// or of every word in the title.
pub fn match_quality(query: &str, url: &str, title: &str) -> f64 {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return 0.0;
    }
    let bare = strip_url(url).to_lowercase();
    if bare.starts_with(&q) {
        return 4.0;
    }
    let lower_url = url.to_lowercase();
    let host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .unwrap_or_default();
    if host.split('.').any(|part| part.starts_with(&q)) {
        return 3.0;
    }
    let title = title.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    if title.split_whitespace().any(|w| w.starts_with(&q)) {
        return 2.5;
    }
    if lower_url.contains(&q) {
        return 2.0;
    }
    if !words.is_empty()
        && words
            .iter()
            .all(|w| title.contains(w) || lower_url.contains(w))
    {
        return 1.5;
    }
    0.0
}

/// Parse the suggest endpoint's response. Accepts OpenSearch (`["q", ["a", "b"]]`, what
/// `type=list` returns) and DuckDuckGo's object list (`[{"phrase": "a"}]`). Drops empty and
/// duplicate phrases and the query itself; at most [`MAX_SEARCH_ROWS`] * 2 phrases.
pub fn parse_remote(body: &str, query: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let raw: Vec<String> = match &v {
        serde_json::Value::Array(a) => match (a.first(), a.get(1)) {
            (Some(serde_json::Value::String(_)), Some(serde_json::Value::Array(list))) => list
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect(),
            _ => a
                .iter()
                .filter_map(|x| x.get("phrase").and_then(|p| p.as_str()).map(str::to_owned))
                .collect(),
        },
        _ => Vec::new(),
    };
    let q = query.trim().to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for p in raw {
        let p = p.trim().chars().take(200).collect::<String>();
        if p.is_empty() || p.to_lowercase() == q || out.iter().any(|o| o.eq_ignore_ascii_case(&p)) {
            continue;
        }
        out.push(p);
        if out.len() >= MAX_SEARCH_ROWS * 2 {
            break;
        }
    }
    out
}

/// The suggest URL for `query`, or `None` when nothing should be sent: empty, too long,
/// a URL or address (no point, and it may carry private paths), or starting with `?`
/// handled as a forced search (the phrase after `?` is sent).
pub fn remote_url(query: &str) -> Option<Url> {
    let q = query.trim();
    let q = q.strip_prefix('?').map(str::trim).unwrap_or(q);
    if q.is_empty() || q.chars().count() > MAX_REMOTE_QUERY {
        return None;
    }
    if matches!(omnibox::resolve(q), Some(OmniboxTarget::Url(_))) || looks_like_address(q) {
        return None;
    }
    Url::parse_with_params(SUGGEST_ENDPOINT, &[("q", q), ("type", "list")]).ok()
}

/// "example.com", "localhost:3000/x", "192.168.0.1": no spaces and a dot or a port.
fn looks_like_address(q: &str) -> bool {
    !q.contains(char::is_whitespace)
        && (q.contains('/')
            || q.starts_with("localhost")
            || q.split('.').filter(|p| !p.is_empty()).count() >= 2)
}

/// Whether the text reads as a question / request for Myth (not an address).
fn reads_as_prompt(q: &str) -> bool {
    matches!(omnibox::resolve(q), Some(OmniboxTarget::Prompt(_))) && q.contains(' ')
}

/// Rank everything for `query` (at `now_ms`). Order: the typed address (if it is one), then
/// page rows (tabs, bookmarks, history by match quality × frecency, deduplicated by URL),
/// then search rows, and the Ask Myth row (first when the text reads as a prompt, else
/// after the pages). At most [`MAX_ROWS`] rows.
pub fn rank(query: &str, src: &Sources<'_>, now_ms: i64) -> Ranked {
    let q = query.trim();
    let mut items: Vec<Suggestion> = Vec::new();
    if q.is_empty() {
        return Ranked {
            query: query.to_owned(),
            items,
            inline: None,
        };
    }
    let target = omnibox::resolve(q);

    // Page rows.
    let mut pages: Vec<Suggestion> = Vec::new();
    for t in src.tabs.iter().filter(|t| !t.active) {
        let m = match_quality(q, &t.url, &t.title);
        if m > 0.0 && !t.url.is_empty() && !crate::chrome::is_internal_str(&t.url) {
            pages.push(Suggestion {
                kind: Kind::Tab,
                title: if t.title.is_empty() {
                    strip_url(&t.url).to_owned()
                } else {
                    t.title.clone()
                },
                url: t.url.clone(),
                input: t.url.clone(),
                tab: Some(t.id),
                deletable: false,
                // An open tab is a strong signal; same scale as a well-visited page.
                score: m * 1000.0 + 150.0,
            });
        }
    }
    for (url, title) in src.bookmarks {
        let m = match_quality(q, url, title);
        if m > 0.0 {
            let visits = src
                .history
                .iter()
                .find(|h| h.url == *url)
                .map_or(0.0, |h| frecency(h.visit_count, h.last_visit_ms, now_ms));
            pages.push(Suggestion {
                kind: Kind::Bookmark,
                title: if title.is_empty() {
                    strip_url(url).to_owned()
                } else {
                    title.clone()
                },
                url: url.clone(),
                input: url.clone(),
                tab: None,
                deletable: false,
                score: m * 1000.0 + 100.0 + visits,
            });
        }
    }
    for h in src.history {
        let m = match_quality(q, &h.url, &h.title);
        if m > 0.0 {
            pages.push(Suggestion {
                kind: Kind::History,
                title: if h.title.is_empty() {
                    strip_url(&h.url).to_owned()
                } else {
                    h.title.clone()
                },
                url: h.url.clone(),
                input: h.url.clone(),
                tab: None,
                deletable: true,
                score: m * 1000.0 + frecency(h.visit_count, h.last_visit_ms, now_ms),
            });
        }
    }
    pages.sort_by(|a, b| b.score.total_cmp(&a.score));
    // One row per URL: the open tab wins, then the bookmark, then history.
    let mut seen: Vec<String> = Vec::new();
    pages.retain(|p| {
        let key = strip_url(&p.url).trim_end_matches('/').to_lowercase();
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    pages.truncate(MAX_PAGE_ROWS);

    // Inline autocomplete: the best page whose bare URL starts with the typed text.
    let inline = if q.contains(char::is_whitespace) || q.starts_with('?') {
        None
    } else {
        pages.iter().find_map(|p| inline_completion(q, &p.url))
    };

    // The typed address itself.
    if let Some(OmniboxTarget::Url(u)) = &target {
        items.push(Suggestion {
            kind: Kind::Go,
            title: q.to_owned(),
            url: u.to_string(),
            input: q.to_owned(),
            tab: None,
            deletable: false,
            score: f64::MAX,
        });
        // A page row for that very URL would repeat it.
        let key = strip_url(u.as_str()).trim_end_matches('/').to_lowercase();
        pages.retain(|p| strip_url(&p.url).trim_end_matches('/').to_lowercase() != key);
    }

    let ask = Suggestion {
        kind: Kind::AskMyth,
        title: q.strip_prefix('?').unwrap_or(q).trim().to_owned(),
        url: String::new(),
        input: q.strip_prefix('?').unwrap_or(q).trim().to_owned(),
        tab: None,
        deletable: false,
        score: 0.0,
    };
    let prompt_first = reads_as_prompt(q);
    if prompt_first {
        items.push(ask.clone());
    }
    items.extend(pages);

    // Search rows: the typed text as a search first, then the remote phrases.
    let phrase = q.strip_prefix('?').unwrap_or(q).trim();
    let mut searches = vec![phrase.to_owned()];
    for r in src.remote {
        if !searches.iter().any(|s| s.eq_ignore_ascii_case(r)) {
            searches.push(r.clone());
        }
    }
    let search_rows: Vec<Suggestion> = searches
        .into_iter()
        .take(MAX_SEARCH_ROWS)
        .map(|s| Suggestion {
            kind: Kind::Search,
            url: omnibox::search_url(&s).to_string(),
            input: format!("? {s}"),
            title: s,
            tab: None,
            deletable: false,
            score: 0.0,
        })
        .collect();
    if !prompt_first {
        // Ask Myth stays visible even when the list is long: right after the pages.
        let room = MAX_ROWS.saturating_sub(items.len() + 1);
        items.extend(search_rows.into_iter().take(room.max(1)));
        items.push(ask);
    } else {
        let room = MAX_ROWS.saturating_sub(items.len());
        items.extend(search_rows.into_iter().take(room));
    }
    items.truncate(MAX_ROWS);
    Ranked {
        query: query.to_owned(),
        items,
        inline,
    }
}

/// If the bare form of `url` starts with `typed` (case-insensitive), what the address bar
/// should show: `typed` followed by the rest, completed up to the end of the host or of the
/// next path segment (like other browsers: "exa" -> "example.com/", "example.com/do" ->
/// "example.com/docs/").
pub fn inline_completion(typed: &str, url: &str) -> Option<String> {
    let bare = strip_url(url);
    if typed.is_empty() || bare.len() <= typed.len() {
        return None;
    }
    if !bare.is_char_boundary(typed.len()) || !bare[..typed.len()].eq_ignore_ascii_case(typed) {
        return None;
    }
    let rest = &bare[typed.len()..];
    // Complete to the next '/', inclusive; or the whole rest when there is none.
    let cut = rest.find('/').map_or(rest.len(), |i| i + 1);
    let completion = &rest[..cut];
    if completion.is_empty() {
        return None;
    }
    Some(format!("{typed}{completion}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const NOW: i64 = 1_800_000_000_000;

    fn h(url: &str, title: &str, visits: u32, days_ago: i64) -> PageHit {
        PageHit {
            url: url.into(),
            title: title.into(),
            visit_count: visits,
            last_visit_ms: NOW - days_ago * DAY,
        }
    }

    #[test]
    fn frecency_rewards_visits_and_recency() {
        assert!(frecency(10, NOW, NOW) > frecency(2, NOW, NOW));
        assert!(frecency(5, NOW, NOW) > frecency(5, NOW - 30 * DAY, NOW));
        // One half-life halves it.
        let a = frecency(3, NOW, NOW);
        let b = frecency(3, NOW - 14 * DAY, NOW);
        assert!((a / b - 2.0).abs() < 1e-9);
        // Future timestamps (clock skew) do not blow up.
        assert!(frecency(3, NOW + DAY, NOW).is_finite());
        assert_eq!(frecency(0, NOW, NOW), 0.0);
    }

    #[test]
    fn match_quality_orders_prefix_host_title_substring() {
        let u = "https://www.github.com/servo/servo";
        assert_eq!(match_quality("git", u, ""), 4.0);
        assert_eq!(match_quality("GitHub.com/se", u, ""), 4.0);
        assert_eq!(match_quality("docs", "https://docs.rs/x", ""), 4.0);
        assert_eq!(match_quality("rs", "https://docs.rs/x", ""), 3.0);
        assert_eq!(match_quality("serv", u, "nothing"), 2.0);
        assert_eq!(match_quality("brow", "https://a.b/", "Servo browser"), 2.5);
        assert_eq!(
            match_quality("servo engine", "https://a.b/servo", "Engine"),
            1.5
        );
        assert_eq!(match_quality("zzz", u, "x"), 0.0);
        assert_eq!(match_quality("  ", u, "x"), 0.0);
    }

    #[test]
    fn parse_remote_accepts_both_formats_and_dedups() {
        let open = r#"["rust", ["rust", "rust lang", "Rust Lang", "rustup", ""]]"#;
        assert_eq!(parse_remote(open, "rust"), vec!["rust lang", "rustup"]);
        let ddg = r#"[{"phrase":"servo browser"},{"phrase":"servo motor"},{"x":1}]"#;
        assert_eq!(
            parse_remote(ddg, "servo"),
            vec!["servo browser", "servo motor"]
        );
        assert!(parse_remote("not json", "x").is_empty());
        assert!(parse_remote(r#"{"a":1}"#, "x").is_empty());
        let many: Vec<String> = (0..30).map(|i| format!("q{i}")).collect();
        let body = serde_json::json!(["q", many]).to_string();
        assert_eq!(parse_remote(&body, "q").len(), MAX_SEARCH_ROWS * 2);
    }

    #[test]
    fn remote_url_skips_addresses_and_long_text() {
        let u = remote_url("how tall is kl tower").unwrap();
        assert!(u.as_str().starts_with(SUGGEST_ENDPOINT));
        assert!(u.query().unwrap().contains("type=list"));
        assert!(
            remote_url("? rust")
                .unwrap()
                .query()
                .unwrap()
                .contains("q=rust")
        );
        assert!(remote_url("").is_none());
        assert!(remote_url("https://bank.example/account?id=1").is_none());
        assert!(remote_url("example.com").is_none());
        assert!(remote_url("localhost:3000").is_none());
        assert!(remote_url("intranet/secret").is_none());
        assert!(remote_url(&"a ".repeat(100)).is_none());
        assert!(remote_url("rust").is_some());
    }

    #[test]
    fn inline_completion_completes_to_segment() {
        assert_eq!(
            inline_completion("exa", "https://www.example.com/docs/a").as_deref(),
            Some("exa".to_owned() + "mple.com/").as_deref()
        );
        assert_eq!(
            inline_completion("example.com/do", "https://example.com/docs/a").as_deref(),
            Some("example.com/docs/")
        );
        // Keeps the user's own casing for what they typed.
        assert_eq!(
            inline_completion("EXA", "https://example.com/").as_deref(),
            Some("EXAmple.com/")
        );
        assert_eq!(
            inline_completion("example.com/", "https://example.com/"),
            None
        );
        assert_eq!(inline_completion("zzz", "https://example.com/"), None);
        assert_eq!(inline_completion("", "https://example.com/"), None);
        // Multi-byte text never splits a character.
        assert_eq!(inline_completion("é", "https://ex.com/"), None);
    }

    #[test]
    fn rank_merges_sources_dedups_and_caps() {
        let history = vec![
            h("https://github.com/servo/servo", "Servo", 2, 40),
            h("https://github.com/", "GitHub", 50, 1),
            h("https://gitlab.com/", "GitLab", 1, 100),
            h("https://news.ycombinator.com/", "Hacker News", 30, 0),
        ];
        let bookmarks = vec![("https://github.com/".to_owned(), "GitHub".to_owned())];
        let tabs = vec![
            TabHit {
                id: 7,
                url: "https://github.com/servo/servo".into(),
                title: "Servo".into(),
                active: false,
            },
            TabHit {
                id: 8,
                url: "https://github.com/me".into(),
                title: "Me".into(),
                active: true,
            },
        ];
        let remote = vec!["github copilot".to_owned(), "github desktop".to_owned()];
        let r = rank(
            "git",
            &Sources {
                history: &history,
                bookmarks: &bookmarks,
                tabs: &tabs,
                remote: &remote,
            },
            NOW,
        );
        let kinds: Vec<Kind> = r.items.iter().map(|i| i.kind).collect();
        // The much-visited bookmark first (bookmark bonus + its frecency), then the open
        // tab (which replaces the history row of the same URL), then history; the active
        // tab is never offered.
        assert_eq!(kinds[0], Kind::Bookmark);
        assert_eq!(kinds[1], Kind::Tab);
        assert_eq!(r.items[1].tab, Some(7));
        assert_eq!(kinds[2], Kind::History);
        assert_eq!(r.items[2].url, "https://gitlab.com/");
        assert!(r.items.iter().all(|i| i.tab != Some(8)));
        // The servo URL appears once (as the tab), github.com once (as the bookmark).
        let n = |u: &str| r.items.iter().filter(|i| i.url == u).count();
        assert_eq!(n("https://github.com/servo/servo"), 1);
        assert_eq!(n("https://github.com/"), 1);
        // Search rows: the typed text, then remote phrases; Ask Myth is the last row.
        assert!(
            r.items
                .iter()
                .any(|i| i.kind == Kind::Search && i.title == "github copilot")
        );
        assert_eq!(r.items.last().unwrap().kind, Kind::AskMyth);
        assert!(r.items.len() <= MAX_ROWS);
        // Hacker News does not match "git".
        assert_eq!(n("https://news.ycombinator.com/"), 0);
        // Inline autocomplete from the best page (completes to the end of the host).
        assert_eq!(r.inline.as_deref(), Some("github.com/"));
        // History rows can be deleted, tabs and bookmarks not.
        assert!(
            r.items
                .iter()
                .filter(|i| i.kind == Kind::History)
                .all(|i| i.deletable)
        );
        assert!(!r.items[0].deletable && !r.items[1].deletable);
    }

    #[test]
    fn rank_puts_ask_myth_first_for_prompts_and_go_first_for_urls() {
        let empty = Sources::default();
        let r = rank("what is servo browser", &empty, NOW);
        assert_eq!(r.items[0].kind, Kind::AskMyth);
        assert_eq!(r.items[0].input, "what is servo browser");
        assert!(r.inline.is_none());
        assert_eq!(r.items[1].kind, Kind::Search);
        assert_eq!(r.items[1].input, "? what is servo browser");

        let history = vec![h("https://example.com/", "Example", 3, 0)];
        let r = rank(
            "example.com",
            &Sources {
                history: &history,
                ..Default::default()
            },
            NOW,
        );
        assert_eq!(r.items[0].kind, Kind::Go);
        // The same URL is not repeated as a history row.
        assert!(!r.items.iter().any(|i| i.kind == Kind::History));
        assert_eq!(rank("   ", &empty, NOW).items.len(), 0);
    }

    #[test]
    fn rank_frecency_beats_recency_alone() {
        // Many visits a week ago beat one visit today, at equal match quality.
        let history = vec![
            h("https://alpha.example/one", "Alpha one", 1, 0),
            h("https://alpha.example/two", "Alpha two", 40, 7),
        ];
        let r = rank(
            "alpha",
            &Sources {
                history: &history,
                ..Default::default()
            },
            NOW,
        );
        let pages: Vec<&str> = r
            .items
            .iter()
            .filter(|i| i.kind == Kind::History)
            .map(|i| i.url.as_str())
            .collect();
        assert_eq!(
            pages,
            vec!["https://alpha.example/two", "https://alpha.example/one"]
        );
    }

    #[test]
    fn rank_skips_internal_tabs() {
        let tabs = vec![TabHit {
            id: 1,
            url: "https://mythbrowse.internal/home.html".into(),
            title: "mythbrowse".into(),
            active: false,
        }];
        let r = rank(
            "myth",
            &Sources {
                tabs: &tabs,
                ..Default::default()
            },
            NOW,
        );
        assert!(r.items.iter().all(|i| i.kind != Kind::Tab));
    }
}
