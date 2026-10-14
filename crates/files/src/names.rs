// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Download file names: where a name comes from (`Content-Disposition`, the page's
//! `<a download>`, the URL, the MIME type) and how it is made safe to write on Windows and
//! Linux.
//!
//! A server or page chooses the name, so it is untrusted: [`sanitize`] removes path parts and
//! characters Windows refuses, reserved device names (`CON`, `NUL`, `COM1`…), trailing dots and
//! spaces (Windows drops them, which can change the extension), control characters and the
//! Unicode direction overrides used to disguise `exe` files (`invoice\u{202e}fdp.exe` shows as
//! `invoiceexe.pdf`), and keeps names under 200 bytes with their extension.

use std::path::{Path, PathBuf};

/// Longest name we write, in UTF-8 bytes (NTFS and ext4 allow 255; room for ` (99)` and `.part`).
pub const MAX_NAME_BYTES: usize = 200;

/// Name used when nothing better is known.
pub const FALLBACK_NAME: &str = "download";

/// Suffix of a file still being downloaded (renamed when complete).
pub const PART_SUFFIX: &str = ".part";

/// The file name from a `Content-Disposition` header value, if it has one.
///
/// `filename*` (RFC 5987/6266, `UTF-8''caf%C3%A9.txt`) wins over `filename`. Quoted strings
/// may contain `;` and backslash escapes. The result is not sanitised yet.
pub fn content_disposition_filename(value: &str) -> Option<String> {
    let mut plain: Option<String> = None;
    let mut extended: Option<String> = None;
    for (key, val) in disposition_params(value) {
        match key.as_str() {
            "filename*" => {
                if let Some(v) = decode_ext_value(&val) {
                    extended = Some(v);
                }
            }
            "filename" if plain.is_none() => plain = Some(val),
            _ => {}
        }
    }
    extended.or(plain).filter(|s| !s.trim().is_empty())
}

/// True if the header says the response is an attachment (should be saved, not shown).
pub fn is_attachment(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("attachment"))
}

/// `key=value` parameters of a `Content-Disposition` value (keys lower-cased, quotes removed).
fn disposition_params(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes: Vec<char> = value.chars().collect();
    let mut i = 0;
    // Skip the disposition type.
    while i < bytes.len() && bytes[i] != ';' {
        i += 1;
    }
    while i < bytes.len() {
        // at ';'
        i += 1;
        while i < bytes.len() && bytes[i].is_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != '=' && bytes[i] != ';' {
            i += 1;
        }
        let key: String = bytes[start..i]
            .iter()
            .collect::<String>()
            .trim()
            .to_ascii_lowercase();
        if i >= bytes.len() || bytes[i] == ';' {
            continue;
        }
        i += 1; // '='
        while i < bytes.len() && bytes[i].is_whitespace() {
            i += 1;
        }
        let mut val = String::new();
        if i < bytes.len() && bytes[i] == '"' {
            i += 1;
            while i < bytes.len() && bytes[i] != '"' {
                if bytes[i] == '\\' && i + 1 < bytes.len() {
                    i += 1;
                }
                val.push(bytes[i]);
                i += 1;
            }
            // closing quote, then up to the next ';'
            while i < bytes.len() && bytes[i] != ';' {
                i += 1;
            }
        } else {
            let s = i;
            while i < bytes.len() && bytes[i] != ';' {
                i += 1;
            }
            val = bytes[s..i].iter().collect::<String>().trim().to_owned();
        }
        if !key.is_empty() {
            out.push((key, val));
        }
    }
    out
}

/// RFC 5987 `charset'lang'percent-encoded` (UTF-8 and ISO-8859-1 only).
fn decode_ext_value(v: &str) -> Option<String> {
    let mut parts = v.splitn(3, '\'');
    let charset = parts.next()?.trim().to_ascii_lowercase();
    let _lang = parts.next()?;
    let encoded = parts.next()?;
    let raw = percent_decode(encoded);
    match charset.as_str() {
        "utf-8" => String::from_utf8(raw).ok(),
        "iso-8859-1" | "latin1" => Some(raw.into_iter().map(char::from).collect()),
        _ => None,
    }
}

/// Decode `%XX` escapes (invalid escapes are kept as written).
pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// The last path segment of a URL, percent-decoded (`https://x/a/report%20v2.pdf?x=1` ->
/// `report v2.pdf`); `None` for `/` or non-UTF-8 names.
pub fn name_from_url(url: &url::Url) -> Option<String> {
    if url.scheme() == "data" {
        return None;
    }
    let seg = url.path_segments()?.rev().find(|s| !s.is_empty())?;
    let name = String::from_utf8(percent_decode(seg)).ok()?;
    (!name.trim().is_empty()).then_some(name)
}

/// The usual extension for a MIME type we may need to add to a name without one.
pub fn ext_for_mime(mime: &str) -> Option<&'static str> {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match essence.as_str() {
        "application/pdf" => "pdf",
        "application/zip" | "application/x-zip-compressed" => "zip",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-7z-compressed" => "7z",
        "application/vnd.rar" | "application/x-rar-compressed" => "rar",
        "application/x-tar" => "tar",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/epub+zip" => "epub",
        "application/x-msdownload" | "application/vnd.microsoft.portable-executable" => "exe",
        "application/x-msi" | "application/x-ms-installer" => "msi",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "text/html" => "html",
        "text/calendar" => "ics",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => return None,
    })
}

/// Extension of a file name (lower-case, without the dot); `None` without one.
pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && !ext.contains(' ')).then(|| ext.to_ascii_lowercase())
}

/// Windows device names that cannot be used as a file name, with or without an extension.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "com¹", "com²", "com³", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8",
    "lpt9", "lpt¹", "lpt²", "lpt³", "conin$", "conout$", "clock$",
];

/// Characters never kept in a name: direction overrides / isolates and invisible joiners that
/// can disguise the real extension.
fn is_spoofing_char(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
    )
}

/// A safe file name for both Windows and Linux (never empty, no path, <= [`MAX_NAME_BYTES`]).
pub fn sanitize(name: &str) -> String {
    // Only the last path component (servers send "../../x" and "C:\\x").
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .filter(|c| !is_spoofing_char(*c))
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Collapse whitespace runs (tabs/newlines were already replaced) and trim.
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // Windows strips trailing dots and spaces; leading dots make hidden files on Linux.
    let s = s.trim_matches(|c: char| c == '.' || c == ' ').to_owned();
    let mut s = if s.is_empty() || s.chars().all(|c| c == '_') {
        FALLBACK_NAME.to_owned()
    } else {
        s
    };
    let stem_lower = s
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_lowercase();
    if RESERVED.contains(&stem_lower.as_str()) {
        s = format!("_{s}");
    }
    clip_name(&s, MAX_NAME_BYTES)
}

/// Shorten `name` to at most `max` UTF-8 bytes, keeping its extension when it has a short one.
pub fn clip_name(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_owned();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && e.len() <= 16 => (s, Some(e)),
        _ => (name, None),
    };
    let keep = max.saturating_sub(ext.map_or(0, |e| e.len() + 1));
    let mut cut = String::new();
    for c in stem.chars() {
        if cut.len() + c.len_utf8() > keep {
            break;
        }
        cut.push(c);
    }
    let cut = cut.trim_end_matches([' ', '.']).to_owned();
    match ext {
        Some(e) => format!("{cut}.{e}"),
        None => cut,
    }
}

/// The name to save a download under, best source first: `Content-Disposition`, the page's
/// `<a download="…">`, the URL's last segment, else [`FALLBACK_NAME`]. Adds an extension from
/// the MIME type when the name has none. Always sanitised.
pub fn choose_name(
    disposition: Option<&str>,
    suggested: Option<&str>,
    url: Option<&url::Url>,
    mime: Option<&str>,
) -> String {
    let raw = disposition
        .and_then(content_disposition_filename)
        .or_else(|| {
            suggested
                .map(str::to_owned)
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| url.and_then(name_from_url))
        .unwrap_or_else(|| FALLBACK_NAME.to_owned());
    let mut name = sanitize(&raw);
    if extension(&name).is_none()
        && let Some(ext) = mime.and_then(ext_for_mime)
    {
        name = clip_name(&format!("{name}.{ext}"), MAX_NAME_BYTES);
    }
    name
}

/// `name`, or `name (1).ext`, `name (2).ext`… : the first that `taken` says is free.
/// `taken` should check the file itself and its `.part` file (and names other downloads hold).
pub fn unique_name(name: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    // `archive.tar.gz` -> `archive (1).tar.gz`
    let (stem, ext) = split_for_counter(name);
    for n in 1..10_000 {
        let candidate = clip_name(&format!("{stem} ({n}){ext}"), MAX_NAME_BYTES + 8);
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("{stem} ({}){ext}", std::process::id())
}

fn split_for_counter(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for double in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(double) && name.len() > double.len() {
            let at = name.len() - double.len();
            return (&name[..at], &name[at..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// A free path for `name` in `dir`: neither the file nor its `.part` exists and `reserved`
/// (paths other running downloads hold) does not contain it.
pub fn unique_path(dir: &Path, name: &str, reserved: &[PathBuf]) -> PathBuf {
    let chosen = unique_name(name, |n| {
        let p = dir.join(n);
        p.exists() || part_path(&p).exists() || reserved.iter().any(|r| r == &p)
    });
    dir.join(chosen)
}

/// `report.pdf` -> `report.pdf.part`.
pub fn part_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_names() {
        let f = content_disposition_filename;
        assert_eq!(
            f("attachment; filename=\"a b.pdf\""),
            Some("a b.pdf".into())
        );
        assert_eq!(
            f("attachment; filename=plain.zip"),
            Some("plain.zip".into())
        );
        assert_eq!(
            f("attachment; filename=\"fallback.txt\"; filename*=UTF-8''caf%C3%A9%20menu.txt"),
            Some("café menu.txt".into())
        );
        assert_eq!(
            f("attachment; filename*=utf-8'en'%E4%B8%AD%E6%96%87.docx"),
            Some("中文.docx".into())
        );
        assert_eq!(
            f("attachment; filename*=iso-8859-1''na%EFve.txt"),
            Some("naïve.txt".into())
        );
        assert_eq!(
            f("attachment; filename=\"semi;colon.txt\"; size=3"),
            Some("semi;colon.txt".into())
        );
        assert_eq!(
            f("attachment; filename=\"esc\\\"aped.txt\""),
            Some("esc\"aped.txt".into())
        );
        assert_eq!(f("inline"), None);
        assert_eq!(f("attachment; filename=\"\""), None);
        // Broken filename* falls back to filename.
        assert_eq!(
            f("attachment; filename*=bogus; filename=ok.txt"),
            Some("ok.txt".into())
        );
        assert!(is_attachment("Attachment; filename=x"));
        assert!(!is_attachment("inline; filename=x"));
    }

    #[test]
    fn sanitising() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\Windows\\system32\\evil.dll"), "evil.dll");
        assert_eq!(sanitize("a<b>c:d\"e|f?g*h.txt"), "a_b_c_d_e_f_g_h.txt");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("nul.txt"), "_nul.txt");
        assert_eq!(sanitize("Com1.tar.gz"), "_Com1.tar.gz");
        assert_eq!(sanitize("console.log"), "console.log");
        assert_eq!(sanitize("report.pdf. . "), "report.pdf");
        assert_eq!(sanitize(".bashrc"), "bashrc");
        assert_eq!(sanitize("   "), FALLBACK_NAME);
        assert_eq!(sanitize(""), FALLBACK_NAME);
        assert_eq!(sanitize("..."), FALLBACK_NAME);
        assert_eq!(sanitize("line\nbreak\t.txt"), "line_break_.txt");
        // Right-to-left override hiding ".exe".
        assert_eq!(sanitize("invoice\u{202e}fdp.exe"), "invoicefdp.exe");
        assert_eq!(sanitize("a\u{200b}b.txt"), "ab.txt");
        assert_eq!(sanitize("multiple   spaces.txt"), "multiple spaces.txt");
        let long = format!("{}.pdf", "x".repeat(400));
        let s = sanitize(&long);
        assert!(s.len() <= MAX_NAME_BYTES && s.ends_with(".pdf"), "{s}");
        let cjk = format!("{}.txt", "文".repeat(200));
        let s = sanitize(&cjk);
        assert!(s.len() <= MAX_NAME_BYTES && s.ends_with(".txt"));
    }

    #[test]
    fn url_and_mime_names() {
        let u = url::Url::parse("https://x.test/a/report%20v2.pdf?dl=1#f").unwrap();
        assert_eq!(name_from_url(&u), Some("report v2.pdf".into()));
        let u = url::Url::parse("https://x.test/dir/").unwrap();
        assert_eq!(name_from_url(&u), Some("dir".into()));
        let u = url::Url::parse("https://x.test/").unwrap();
        assert_eq!(name_from_url(&u), None);
        let u = url::Url::parse("https://x.test/get").unwrap();
        assert_eq!(
            choose_name(None, None, Some(&u), Some("application/pdf")),
            "get.pdf"
        );
        assert_eq!(
            choose_name(
                Some("attachment; filename=\"../x.zip\""),
                Some("y.txt"),
                Some(&u),
                None
            ),
            "x.zip"
        );
        assert_eq!(
            choose_name(None, Some("my file.csv"), Some(&u), None),
            "my file.csv"
        );
        assert_eq!(choose_name(None, None, None, None), "download");
        assert_eq!(
            choose_name(None, None, None, Some("text/csv; charset=utf-8")),
            "download.csv"
        );
        let d = url::Url::parse("data:text/plain,hi").unwrap();
        assert_eq!(
            choose_name(None, None, Some(&d), Some("text/plain")),
            "download.txt"
        );
        assert_eq!(extension("a.TAR.GZ").as_deref(), Some("gz"));
        assert_eq!(extension("noext"), None);
        assert_eq!(extension(".hidden"), None);
    }

    #[test]
    fn unique_names() {
        let taken = ["a.pdf", "a (1).pdf", "x.tar.gz", "noext"];
        let t = |n: &str| taken.contains(&n);
        assert_eq!(unique_name("b.pdf", t), "b.pdf");
        assert_eq!(unique_name("a.pdf", t), "a (2).pdf");
        assert_eq!(unique_name("x.tar.gz", t), "x (1).tar.gz");
        assert_eq!(unique_name("noext", t), "noext (1)");
    }

    #[test]
    fn unique_paths_on_disk() {
        let dir = std::env::temp_dir().join(format!("mb-files-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), b"x").unwrap();
        std::fs::write(dir.join("g.txt.part"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "f.txt", &[]), dir.join("f (1).txt"));
        assert_eq!(unique_path(&dir, "g.txt", &[]), dir.join("g (1).txt"));
        assert_eq!(
            unique_path(&dir, "h.txt", &[dir.join("h.txt")]),
            dir.join("h (1).txt")
        );
        assert_eq!(part_path(&dir.join("h.txt")), dir.join("h.txt.part"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
