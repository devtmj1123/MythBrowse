// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Telling a download from a page, with what Servo 0.1.4 gives an embedder.
//!
//! Servo 0.1.4 has **no download API**: `WebViewDelegate::load_web_resource` sees requests,
//! never responses; a response with `Content-Disposition: attachment` is rendered anyway;
//! a type Servo cannot render becomes an inline page reading "Unknown content type (…)."; and
//! `<a download>` is a TODO in `HTMLAnchorElement` (it just follows the link). So MythBrowse
//! finds downloads three ways and fetches them itself ([`crate::download`]):
//!
//! 1. **Navigation by extension** ([`looks_like_download`]): a top-level navigation to a URL
//!    whose path ends in a type no browser renders (`.zip`, `.exe`, `.msi`, `.pdf`…) is
//!    cancelled before it loads and downloaded. If the server then answers with an HTML page
//!    (a login wall, a "your download starts soon" page), the download gives up and the tab
//!    loads the URL normally ([`crate::download::Event::NotADownload`]).
//! 2. **Servo's unknown-content page** ([`UNKNOWN_CONTENT_PROBE`]): when a page finishes
//!    loading, the shell checks for that exact inline page and downloads its URL instead.
//! 3. **`<a download>` clicks** ([`DOWNLOAD_LINK_SCRIPT`], a user script in every page):
//!    trusted clicks on such links are cancelled and reported to the shell on the console
//!    channel ([`parse_console_request`]).

use serde::Deserialize;

/// Extensions a top-level navigation is downloaded for instead of being shown.
pub const DOWNLOAD_EXTENSIONS: &[&str] = &[
    // archives
    "zip",
    "7z",
    "rar",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "zst",
    "tar",
    "cab",
    // programs, installers, packages, disk images
    "exe",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "dmg",
    "pkg",
    "deb",
    "rpm",
    "apk",
    "appimage",
    "iso",
    "img",
    "vhd",
    "vhdx",
    "jar",
    // documents Servo cannot show
    "pdf",
    "doc",
    "docx",
    "docm",
    "xls",
    "xlsx",
    "xlsm",
    "ppt",
    "pptx",
    "pptm",
    "odt",
    "ods",
    "odp",
    "rtf",
    "epub",
    "mobi",
    // other binary data
    "bin",
    "dat",
    "torrent",
    "ttf",
    "otf",
    "woff",
    "woff2",
    "psd",
    "ai",
    "sketch",
    "fig",
];

/// The marker the `<a download>` script prefixes its console line with.
pub const CONSOLE_MARKER: &str = "\u{1}mythbrowse:download ";

/// True if a top-level navigation to `url` should be downloaded rather than shown, judged by
/// the path's extension (http/https only).
pub fn looks_like_download(url: &url::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(last) = url.path_segments().and_then(|mut s| s.next_back()) else {
        return false;
    };
    let decoded = String::from_utf8(crate::names::percent_decode(last)).unwrap_or_default();
    match crate::names::extension(&decoded) {
        Some(ext) => DOWNLOAD_EXTENSIONS.contains(&ext.as_str()),
        None => false,
    }
}

/// The shell's main-frame load hook takes this request over for the download manager (Servo
/// gets a `204 No Content`, which ends the navigation and keeps the page that was there):
/// a top-level load of a file-like URL that did not go through the navigation hook (a
/// redirect hop to a file, the first load of a `target=_blank` / `window.open` tab). Not
/// when the manager itself sent the URL back as a web page (`bypassed`).
pub fn takes_over_main_frame(url: &url::Url, bypassed: bool) -> bool {
    !bypassed && looks_like_download(url)
}

/// What happens to the tab after its navigation became a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAfterDownload {
    /// It still shows the page the download was started from.
    Keep,
    /// It shows Servo's "unknown content type" placeholder: back to the page before.
    GoBack,
    /// It was opened only for the file (`target=_blank`, typed into a new tab) and has
    /// nothing to show: close it instead of leaving an empty tab.
    Close,
}

/// `shows_page`: the tab still shows a real document (not blank, not Servo's placeholder).
pub fn tab_after_download(shows_page: bool, can_go_back: bool) -> TabAfterDownload {
    match (shows_page, can_go_back) {
        (true, _) => TabAfterDownload::Keep,
        (false, true) => TabAfterDownload::GoBack,
        (false, false) => TabAfterDownload::Close,
    }
}

/// Whether a response should be saved rather than shown: an attachment, or a type a browser
/// does not render. `None` content type (unknown) counts as a download.
pub fn response_is_download(content_type: Option<&str>, disposition: Option<&str>) -> bool {
    if disposition.is_some_and(crate::names::is_attachment) {
        return true;
    }
    match content_type {
        None => true,
        Some(ct) => !is_renderable(ct),
    }
}

/// MIME types Servo shows in a tab (HTML/XML, text, JSON/JS/CSS, images, audio/video).
pub fn is_renderable(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let (top, sub) = essence.split_once('/').unwrap_or((essence.as_str(), ""));
    match top {
        "text" => !matches!(sub, "calendar" | "vcard" | "x-vcard"),
        "image" => !matches!(sub, "vnd.adobe.photoshop" | "x-icns"),
        "audio" | "video" => true,
        "application" => matches!(
            sub,
            "xhtml+xml"
                | "xml"
                | "json"
                | "ld+json"
                | "manifest+json"
                | "javascript"
                | "x-javascript"
                | "ecmascript"
                | "rss+xml"
                | "atom+xml"
                | "svg+xml"
        ),
        _ => false,
    }
}

/// Evaluated in a web page when it finishes loading: the content type if the page is Servo's
/// inline "Unknown content type (x)." page, else `""`.
pub const UNKNOWN_CONTENT_PROBE: &str = r#"(function () {
  var b = document.body;
  if (!b || document.title || b.children.length !== 1 || b.firstElementChild.tagName !== "P") return "";
  var m = /^Unknown content type \((.{1,200})\)\.$/.exec(b.textContent.trim());
  return m ? m[1] : "";
})()"#;

/// User script for every web page: a trusted click on `<a download>` (or a link the page marks
/// as a download with `download=""`) is cancelled and reported to the shell, which downloads it
/// with the suggested name. Links to `data:` are reported with their URL; `blob:` links are
/// reported too, and the shell says it cannot save them yet.
pub const DOWNLOAD_LINK_SCRIPT: &str = r#"(function () {
  if (window.__mbDownloadHook) return;
  window.__mbDownloadHook = true;
  document.addEventListener("click", function (e) {
    if (!e.isTrusted || e.defaultPrevented || e.button !== 0) return;
    var a = e.target && e.target.closest ? e.target.closest("a[download]") : null;
    if (!a || !a.href) return;
    var href = a.href;
    if (!/^(https?:|data:|blob:)/i.test(href)) return;
    e.preventDefault();
    e.stopPropagation();
    console.log("\u0001mythbrowse:download " + JSON.stringify({ href: href.slice(0, 2097152), name: String(a.getAttribute("download") || "").slice(0, 400) }));
  }, false);
})();"#;

/// A download asked for by the page script.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LinkRequest {
    pub href: String,
    #[serde(default)]
    pub name: String,
}

/// Parse a console line from [`DOWNLOAD_LINK_SCRIPT`]; `None` for any other line.
pub fn parse_console_request(message: &str) -> Option<LinkRequest> {
    let json = message.strip_prefix(CONSOLE_MARKER)?;
    let req: LinkRequest = serde_json::from_str(json).ok()?;
    let ok = req.href.len() <= 2 * 1024 * 1024
        && ["http:", "https:", "data:", "blob:"]
            .iter()
            .any(|p| req.href.len() >= p.len() && req.href[..p.len()].eq_ignore_ascii_case(p));
    ok.then_some(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_frame_takeover_and_the_tab_left_behind() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(takes_over_main_frame(
            &u("https://cdn.example/r/report.zip"),
            false
        ));
        assert!(!takes_over_main_frame(
            &u("https://cdn.example/r/report.zip"),
            true
        ));
        assert!(!takes_over_main_frame(
            &u("https://example.com/docs/"),
            false
        ));
        assert_eq!(tab_after_download(true, false), TabAfterDownload::Keep);
        assert_eq!(tab_after_download(true, true), TabAfterDownload::Keep);
        assert_eq!(tab_after_download(false, true), TabAfterDownload::GoBack);
        assert_eq!(tab_after_download(false, false), TabAfterDownload::Close);
    }

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn navigation_extensions() {
        assert!(looks_like_download(&u("https://x.test/files/setup.exe")));
        assert!(looks_like_download(&u("https://x.test/a/Report.PDF?x=1")));
        assert!(looks_like_download(&u("http://x.test/archive.tar.gz")));
        assert!(looks_like_download(&u("https://x.test/my%20file.zip")));
        assert!(!looks_like_download(&u("https://x.test/")));
        assert!(!looks_like_download(&u("https://x.test/page.html")));
        assert!(!looks_like_download(&u("https://x.test/image.png")));
        assert!(!looks_like_download(&u("https://x.test/zip")));
        assert!(!looks_like_download(&u("file:///C:/a.zip")));
        assert!(!looks_like_download(&u("https://x.test/a.zip/")));
    }

    #[test]
    fn renderable_types() {
        for t in [
            "text/html; charset=utf-8",
            "text/plain",
            "TEXT/CSS",
            "image/png",
            "image/svg+xml",
            "video/mp4",
            "audio/ogg",
            "application/json",
            "application/xhtml+xml",
            "application/javascript",
        ] {
            assert!(is_renderable(t), "{t}");
            assert!(!response_is_download(Some(t), None), "{t}");
        }
        for t in [
            "application/octet-stream",
            "application/pdf",
            "application/zip",
            "application/x-msdownload",
            "text/calendar",
            "font/woff2",
            "",
        ] {
            assert!(!is_renderable(t), "{t}");
            assert!(response_is_download(Some(t), None), "{t}");
        }
        assert!(response_is_download(
            Some("text/plain"),
            Some("attachment; filename=a.txt")
        ));
        assert!(!response_is_download(Some("text/plain"), Some("inline")));
        assert!(response_is_download(None, None));
    }

    #[test]
    fn console_requests() {
        let line =
            format!("{CONSOLE_MARKER}{{\"href\":\"https://x.test/a.zip\",\"name\":\"b.zip\"}}");
        assert_eq!(
            parse_console_request(&line),
            Some(LinkRequest {
                href: "https://x.test/a.zip".into(),
                name: "b.zip".into()
            })
        );
        let data = format!("{CONSOLE_MARKER}{{\"href\":\"data:text/plain,hi\"}}");
        assert_eq!(parse_console_request(&data).unwrap().name, "");
        assert!(
            parse_console_request(&format!(
                "{CONSOLE_MARKER}{{\"href\":\"javascript:alert(1)\"}}"
            ))
            .is_none()
        );
        assert!(
            parse_console_request(&format!(
                "{CONSOLE_MARKER}{{\"href\":\"file:///etc/passwd\"}}"
            ))
            .is_none()
        );
        assert!(parse_console_request(&format!("{CONSOLE_MARKER}not json")).is_none());
        assert!(parse_console_request("mythbrowse:download {\"href\":\"https://x\"}").is_none());
        assert!(parse_console_request("hello").is_none());
    }
}
