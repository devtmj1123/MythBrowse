// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The operating system side of files: the user's Downloads folder, opening a file with its
//! default app, and showing it selected in the file manager.
//!
//! * Windows: `SHGetKnownFolderPath(FOLDERID_Downloads)` (follows a moved Downloads folder),
//!   `ShellExecuteW("open")`, `SHOpenFolderAndSelectItems` (Explorer with the file selected).
//! * Linux: `XDG_DOWNLOAD_DIR` (environment, then `~/.config/user-dirs.dirs`), else
//!   `~/Downloads`; `xdg-open`; the FileManager1 D-Bus `ShowItems` call (Nautilus, Dolphin,
//!   Nemo…) via `dbus-send`, else `xdg-open` on the folder.
//!
//! Opening and showing run on a short-lived thread: a slow shell extension never blocks the
//! browser.
//!
//! [`open_url`] (v0.4.2) hands a `mailto:` / `tel:` / `sms:` / `callto:` link to the app the
//! OS has for it (`ShellExecuteW("open")` / `xdg-open`). Only those schemes: arbitrary app
//! schemes are a known attack surface (`ms-msdt:` and friends), so they are refused here too,
//! whatever the caller decided. The shell calls it only right after a real user click.

use std::path::{Path, PathBuf};

/// `$MYTHBROWSE_DOWNLOAD_DIR` (tests, portable setups), else the platform Downloads folder,
/// else `None` (the caller falls back to the profile directory).
pub fn downloads_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("MYTHBROWSE_DOWNLOAD_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(d));
    }
    platform_downloads_dir()
}

#[cfg(windows)]
fn platform_downloads_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };
    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: documented contract: on success `raw` is a NUL-terminated wide string we free
    // with CoTaskMemFree (also on failure, where it may be null).
    let hr = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_Downloads,
            KF_FLAG_DEFAULT as u32,
            std::ptr::null_mut(),
            &mut raw,
        )
    };
    let path = if hr >= 0 && !raw.is_null() {
        // SAFETY: `raw` is NUL-terminated (see above).
        let len = (0..).take_while(|&i| unsafe { *raw.add(i) } != 0).count();
        let wide = unsafe { std::slice::from_raw_parts(raw, len) };
        Some(PathBuf::from(std::ffi::OsString::from_wide(wide)))
    } else {
        None
    };
    // SAFETY: CoTaskMemFree accepts null.
    unsafe { CoTaskMemFree(raw as *const _) };
    path.or_else(|| {
        std::env::var_os("USERPROFILE")
            .filter(|v| !v.is_empty())
            .map(|p| PathBuf::from(p).join("Downloads"))
    })
}

#[cfg(not(windows))]
fn platform_downloads_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)?;
    if let Some(v) = std::env::var_os("XDG_DOWNLOAD_DIR").filter(|v| !v.is_empty()) {
        return Some(expand_home(&v.to_string_lossy(), &home));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    if let Ok(text) = std::fs::read_to_string(config.join("user-dirs.dirs"))
        && let Some(dir) = parse_user_dirs(&text, &home)
    {
        return Some(dir);
    }
    Some(home.join("Downloads"))
}

/// `XDG_DOWNLOAD_DIR="$HOME/Downloads"` out of `user-dirs.dirs`.
pub fn parse_user_dirs(text: &str, home: &Path) -> Option<PathBuf> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(v) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
            let v = v.trim().trim_matches('"');
            if v.is_empty() {
                return None;
            }
            let p = expand_home(v, home);
            // `$HOME/` alone means "disabled" (the spec): use the default instead.
            return (p != home).then_some(p);
        }
    }
    None
}

fn expand_home(v: &str, home: &Path) -> PathBuf {
    if let Some(rest) = v.strip_prefix("$HOME") {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        }
    } else if let Some(rest) = v.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(v)
    }
}

/// Open `path` with its default application (never waits for it).
pub fn open_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("The file is no longer there.".into());
    }
    let path = path.to_path_buf();
    spawn("mb-open", move || platform_open(&path))
}

/// Show `path` selected in the file manager (its folder if selection is not supported).
pub fn reveal_path(path: &Path) -> Result<(), String> {
    let target = if path.exists() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent().filter(|p| p.exists()) {
        parent.to_path_buf()
    } else {
        return Err("The folder is no longer there.".into());
    };
    spawn("mb-reveal", move || platform_reveal(&target))
}

/// Schemes [`open_url`] hands to the OS.
pub const EXTERNAL_SCHEMES: &[&str] = &["mailto", "tel", "sms", "callto"];

/// `url` if it is safe to give the OS as one argument: an [`EXTERNAL_SCHEMES`] URL that
/// parses, at most 2048 bytes, no control characters, quotes or backslashes (nothing a
/// command line or ShellExecute could read as a second argument or a path).
pub fn external_url_ok(url: &str) -> Option<url::Url> {
    if url.len() > 2048 || url.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    EXTERNAL_SCHEMES
        .contains(&parsed.scheme())
        .then_some(parsed)
}

/// Open a `mailto:` / `tel:` / `sms:` / `callto:` link with the OS's handler (never waits).
pub fn open_url(url: &str) -> Result<(), String> {
    let Some(parsed) = external_url_ok(url) else {
        return Err("MythBrowse only opens email, phone and SMS links in other apps.".into());
    };
    let target = parsed.to_string();
    spawn("mb-open-url", move || platform_open_url(&target))
}

#[cfg(windows)]
fn platform_open_url(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let target = wide(std::ffi::OsStr::new(url));
    let verb = wide(std::ffi::OsStr::new("open"));
    // SAFETY: NUL-terminated wide strings that outlive the call; null HWND/params are allowed.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (r as isize) <= 32 {
        // No handler registered (e.g. no mail app): logged without the address itself.
        log::warn!(
            "ShellExecuteW failed ({}) for a {} link",
            r as isize,
            url.split(':').next().unwrap_or("")
        );
    }
}

#[cfg(not(windows))]
fn platform_open_url(url: &str) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
        log::warn!(
            "xdg-open failed for a {} link: {e}",
            url.split(':').next().unwrap_or("")
        );
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(|_| ())
        .map_err(|e| format!("could not start: {e}"))
}

#[cfg(windows)]
fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn platform_open(path: &Path) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let file = wide(path.as_os_str());
    let verb = wide(std::ffi::OsStr::new("open"));
    // SAFETY: NUL-terminated wide strings that outlive the call; null HWND/params are allowed.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values <= 32 are errors (documented).
    if (r as isize) <= 32 {
        log::warn!(
            "ShellExecuteW failed ({}) for {}",
            r as isize,
            path.display()
        );
    }
}

#[cfg(windows)]
fn platform_reveal(path: &Path) {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
    let file = wide(path.as_os_str());
    // SAFETY: COM is initialised for this (our own, short-lived) thread; the PIDL from
    // ILCreateFromPathW is freed with ILFree; `file` outlives the calls.
    unsafe {
        let co = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let pidl = ILCreateFromPathW(file.as_ptr());
        if pidl.is_null() {
            log::warn!("ILCreateFromPathW failed for {}", path.display());
        } else {
            let hr = SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0);
            if hr < 0 {
                log::warn!("SHOpenFolderAndSelectItems failed ({hr:#x})");
            }
            ILFree(pidl);
        }
        if co >= 0 {
            CoUninitialize();
        }
    }
}

#[cfg(not(windows))]
fn platform_open(path: &Path) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(path).spawn() {
        log::warn!("xdg-open failed: {e}");
    }
}

#[cfg(not(windows))]
fn platform_reveal(path: &Path) {
    // FileManager1 selects the file; any file manager on a desktop session implements it.
    let uri = url::Url::from_file_path(path).map(|u| u.to_string());
    if let Ok(uri) = uri {
        let shown = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            .arg(format!("array:string:{uri}"))
            .arg("string:")
            .output()
            .is_ok_and(|o| o.status.success());
        if shown {
            return;
        }
    }
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    if let Err(e) = std::process::Command::new("xdg-open").arg(dir).spawn() {
        log::warn!("xdg-open failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_urls() {
        assert!(external_url_ok("mailto:a@b.com?subject=Hi%20there").is_some());
        assert!(external_url_ok("tel:+60112345678").is_some());
        assert!(external_url_ok("sms:+60112345678").is_some());
        assert!(external_url_ok("https://example.com/").is_none());
        assert!(external_url_ok("file:///C:/Windows/System32/calc.exe").is_none());
        assert!(external_url_ok("ms-msdt:/id PCWDiagnostic").is_none());
        assert!(external_url_ok("mailto:a@b.com\" --x").is_none());
        assert!(external_url_ok("mailto:a@b.com\nfoo").is_none());
        assert!(external_url_ok("mailto:a\\b").is_none());
        assert!(external_url_ok(&format!("mailto:a@b.com?body={}", "x".repeat(3000))).is_none());
        assert!(open_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn user_dirs_file() {
        let home = Path::new("/home/u");
        let text =
            "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Unduhan\"\n";
        assert_eq!(
            parse_user_dirs(text, home),
            Some(PathBuf::from("/home/u/Unduhan"))
        );
        assert_eq!(
            parse_user_dirs("XDG_DOWNLOAD_DIR=\"/data/dl\"", home),
            Some(PathBuf::from("/data/dl"))
        );
        assert_eq!(parse_user_dirs("XDG_DOWNLOAD_DIR=\"$HOME/\"", home), None);
        assert_eq!(parse_user_dirs("XDG_MUSIC_DIR=\"$HOME/Music\"", home), None);
    }

    #[test]
    fn env_override_and_missing_files() {
        // Only this test touches MYTHBROWSE_DOWNLOAD_DIR.
        // SAFETY: tests in this crate do not read this variable concurrently.
        unsafe { std::env::set_var("MYTHBROWSE_DOWNLOAD_DIR", "/tmp/mb-dl-override") };
        assert_eq!(downloads_dir(), Some(PathBuf::from("/tmp/mb-dl-override")));
        unsafe { std::env::remove_var("MYTHBROWSE_DOWNLOAD_DIR") };
        assert!(downloads_dir().is_some() || std::env::var_os("HOME").is_none());
        let gone = Path::new("/nonexistent/mb/x.txt");
        assert!(open_path(gone).is_err());
        assert!(reveal_path(gone).is_err());
    }
}
