// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The native file chooser for `<input type=file>`.
//!
//! [`choose`] blocks until the user picks or cancels, so the shell runs it on its own thread
//! and answers Servo's `FilePicker` control from the main thread afterwards.
//!
//! * Windows: the common Open dialog (`GetOpenFileNameW`, Explorer style), owned by the
//!   browser window so it is modal to it; `multiple` turns on multi-select; the page's
//!   `accept` becomes an "Accepted files" filter with "All files" as the second choice (as
//!   other browsers do, the filter is a hint, not a rule).
//! * Linux: `zenity --file-selection`, else `kdialog --getopenfilename`.
//!
//! Servo's filter patterns are bare extensions (`png`, from `.png` or `image/png` in
//! `accept`, see `file_input_type.rs::filter_from_accept`).
//!
//! Drag and drop of files onto a page is **not** possible on Servo 0.1.4: its `InputEvent`
//! has no drag / drop variant, so there is no way to hand dropped files to a page.

use std::path::PathBuf;

/// At most this many extensions go into the filter.
const MAX_EXTENSIONS: usize = 64;

/// What to ask for.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Bare extensions from the page's `accept` (any case, with or without `.` / `*.`).
    pub extensions: Vec<String>,
    pub multiple: bool,
    /// Window title, e.g. "Choose a file for example.com".
    pub title: String,
    /// Native owner window (Windows `HWND` as an integer; 0 = none).
    pub owner: isize,
}

/// Show the native chooser. `None` = cancelled (or no chooser available).
pub fn choose(opts: &Options) -> Option<Vec<PathBuf>> {
    let exts = clean_extensions(&opts.extensions);
    let picked = platform_choose(opts, &exts)?;
    let picked: Vec<PathBuf> = picked.into_iter().filter(|p| p.is_file()).collect();
    if picked.is_empty() {
        return None;
    }
    if opts.multiple {
        Some(picked)
    } else {
        Some(picked.into_iter().take(1).collect())
    }
}

/// Lowercase, without `.` / `*.`, only `[a-z0-9_+-]`, deduplicated, capped.
pub fn clean_extensions(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in raw {
        let e = e
            .trim()
            .trim_start_matches('*')
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if e.is_empty()
            || e.len() > 16
            || !e
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-'))
        {
            continue;
        }
        if !out.contains(&e) {
            out.push(e);
        }
        if out.len() == MAX_EXTENSIONS {
            break;
        }
    }
    out
}

/// The `lpstrFilter` text for `GetOpenFileNameW` (pairs separated and ended by NUL).
pub fn windows_filter(exts: &[String]) -> String {
    let mut s = String::new();
    if !exts.is_empty() {
        let pattern = exts
            .iter()
            .map(|e| format!("*.{e}"))
            .collect::<Vec<_>>()
            .join(";");
        s.push_str(&format!("Accepted files ({pattern})\0{pattern}\0"));
    }
    s.push_str("All files (*.*)\0*.*\0\0");
    s
}

/// Split the dialog's result buffer (already cut at the double NUL into parts): one part is
/// a full path; several are a folder followed by file names.
pub fn split_multi(parts: &[String]) -> Vec<PathBuf> {
    match parts {
        [] => Vec::new(),
        [one] => vec![PathBuf::from(one)],
        [dir, files @ ..] => files.iter().map(|f| PathBuf::from(dir).join(f)).collect(),
    }
}

/// `zenity` arguments for these options.
pub fn zenity_args(opts: &Options, exts: &[String]) -> Vec<String> {
    let mut a = vec!["--file-selection".to_owned()];
    if !opts.title.is_empty() {
        a.push(format!("--title={}", opts.title));
    }
    if opts.multiple {
        a.push("--multiple".into());
        a.push("--separator=\n".into());
    }
    if !exts.is_empty() {
        let pats = exts
            .iter()
            .map(|e| format!("*.{e}"))
            .collect::<Vec<_>>()
            .join(" ");
        a.push(format!("--file-filter=Accepted files | {pats}"));
        a.push("--file-filter=All files | *".into());
    }
    a
}

#[cfg(windows)]
fn platform_choose(opts: &Options, exts: &[String]) -> Option<Vec<PathBuf>> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let filter: Vec<u16> = windows_filter(exts).encode_utf16().collect();
    let title = wide(if opts.title.is_empty() {
        "Open"
    } else {
        &opts.title
    });
    // Room for many selected files (the dialog fails with FNERR_BUFFERTOOSMALL otherwise).
    let mut buf = vec![0u16; if opts.multiple { 64 * 1024 } else { 4096 }];
    let mut flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if opts.multiple {
        flags |= OFN_ALLOWMULTISELECT;
    }
    // SAFETY: OPENFILENAMEW is a plain C struct; zeroed is its documented initial state.
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = opts.owner as _;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.nFilterIndex = 1;
    ofn.lpstrFile = buf.as_mut_ptr();
    ofn.nMaxFile = buf.len() as u32;
    ofn.lpstrTitle = title.as_ptr();
    ofn.Flags = flags;
    // SAFETY: every pointer in `ofn` points into buffers that outlive the call; COM is
    // initialised on this (our own) thread for the shell namespace the dialog uses.
    let ok = unsafe {
        let co = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let ok = GetOpenFileNameW(&mut ofn);
        if co >= 0 {
            CoUninitialize();
        }
        ok
    };
    if ok == 0 {
        return None;
    }
    let mut parts = Vec::new();
    for part in buf.split(|c| *c == 0) {
        if part.is_empty() {
            break;
        }
        parts.push(
            std::ffi::OsString::from_wide(part)
                .to_string_lossy()
                .into_owned(),
        );
    }
    Some(split_multi(&parts))
}

#[cfg(not(windows))]
fn platform_choose(opts: &Options, exts: &[String]) -> Option<Vec<PathBuf>> {
    use std::process::Command;
    let out = Command::new("zenity")
        .args(zenity_args(opts, exts))
        .output();
    let out = match out {
        Ok(o) => o,
        Err(_) => {
            let mut c = Command::new("kdialog");
            c.arg("--getopenfilename")
                .arg(std::env::var_os("HOME").unwrap_or_default());
            if !exts.is_empty() {
                c.arg(
                    exts.iter()
                        .map(|e| format!("*.{e}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            if opts.multiple {
                c.arg("--multiple").arg("--separate-output");
            }
            c.output().ok()?
        }
    };
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let files: Vec<PathBuf> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    (!files.is_empty()).then_some(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn extensions_are_cleaned() {
        assert_eq!(
            clean_extensions(&s(&[
                "PNG", ".jpg", "*.jpeg", "png", "", "a/b", "tar.gz", "c++"
            ])),
            s(&["png", "jpg", "jpeg", "c++"])
        );
        let many: Vec<String> = (0..100).map(|i| format!("e{i}")).collect();
        assert_eq!(clean_extensions(&many).len(), MAX_EXTENSIONS);
    }

    #[test]
    fn windows_filter_text() {
        assert_eq!(windows_filter(&[]), "All files (*.*)\0*.*\0\0");
        assert_eq!(
            windows_filter(&s(&["png", "jpg"])),
            "Accepted files (*.png;*.jpg)\0*.png;*.jpg\0All files (*.*)\0*.*\0\0"
        );
    }

    #[test]
    fn multi_select_buffer() {
        assert_eq!(
            split_multi(&s(&["C:\\a\\b.txt"])),
            vec![PathBuf::from("C:\\a\\b.txt")]
        );
        assert_eq!(
            split_multi(&s(&["/home/u", "a.txt", "b.png"])),
            vec![
                PathBuf::from("/home/u/a.txt"),
                PathBuf::from("/home/u/b.png")
            ]
        );
        assert!(split_multi(&[]).is_empty());
    }

    #[test]
    fn zenity_arguments() {
        let o = Options {
            multiple: true,
            title: "Choose".into(),
            ..Default::default()
        };
        let a = zenity_args(&o, &s(&["png"]));
        assert!(a.contains(&"--multiple".to_string()));
        assert!(a.contains(&"--file-filter=Accepted files | *.png".to_string()));
        assert!(a.contains(&"--title=Choose".to_string()));
    }
}

// ---- v0.4: Settings > Downloads ------------------------------------------------------------

/// Choose a folder (Settings > Downloads > Change). Blocks until the user picks or cancels,
/// so call it off the main thread. `None` = cancelled / no chooser.
///
/// * Windows: the shell folder browser (`SHBrowseForFolderW`, new dialog style, file-system
///   folders only), owned by the browser window.
/// * Linux: `zenity --file-selection --directory`, else `kdialog --getexistingdirectory`.
pub fn choose_folder(
    owner: isize,
    title: &str,
    start: Option<&std::path::Path>,
) -> Option<PathBuf> {
    platform_choose_folder(owner, title, start).filter(|p| p.is_dir())
}

/// "Save as" for one download (Settings > Downloads > Ask where to save each file). Blocks;
/// call it off the main thread. `None` = cancelled.
///
/// * Windows: the common Save dialog (`GetSaveFileNameW`) in `dir` with `name` filled in,
///   asking before it replaces a file.
/// * Linux: `zenity --file-selection --save --confirm-overwrite`, else
///   `kdialog --getsavefilename`.
pub fn choose_save_path(owner: isize, dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    platform_choose_save(owner, dir, name).filter(|p| p.file_name().is_some())
}

/// Arguments for `zenity` (Linux) for a folder / save chooser (tested; the dialog itself is not).
pub fn zenity_folder_args(title: &str, start: Option<&std::path::Path>) -> Vec<String> {
    let mut a = vec![
        "--file-selection".to_owned(),
        "--directory".to_owned(),
        format!(
            "--title={}",
            if title.is_empty() {
                "Choose a folder"
            } else {
                title
            }
        ),
    ];
    if let Some(s) = start {
        a.push(format!("--filename={}/", s.display()));
    }
    a
}

pub fn zenity_save_args(dir: &std::path::Path, name: &str) -> Vec<String> {
    vec![
        "--file-selection".to_owned(),
        "--save".to_owned(),
        "--confirm-overwrite".to_owned(),
        "--title=Save as".to_owned(),
        format!("--filename={}", dir.join(name).display()),
    ]
}

#[cfg(windows)]
fn wide_z(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn from_wide_z(buf: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    PathBuf::from(std::ffi::OsString::from_wide(&buf[..len]))
}

#[cfg(windows)]
fn platform_choose_folder(
    owner: isize,
    title: &str,
    _start: Option<&std::path::Path>,
) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{
        BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW,
        SHGetPathFromIDListW,
    };
    let title = wide_z(std::ffi::OsStr::new(if title.is_empty() {
        "Choose a folder"
    } else {
        title
    }));
    let mut display = vec![0u16; 260];
    // SAFETY: BROWSEINFOW is a plain C struct; zeroed is its documented initial state.
    let mut bi: BROWSEINFOW = unsafe { std::mem::zeroed() };
    bi.hwndOwner = owner as _;
    bi.pszDisplayName = display.as_mut_ptr();
    bi.lpszTitle = title.as_ptr();
    bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
    let mut path = vec![0u16; 32 * 1024];
    // SAFETY: the pointers in `bi` outlive the call; the returned PIDL is freed with
    // CoTaskMemFree; `path` is larger than MAX_PATH as SHGetPathFromIDListW requires. COM is
    // initialised (apartment) on this, our own, thread as the new dialog style requires.
    let ok = unsafe {
        let co = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let pidl = SHBrowseForFolderW(&bi);
        let ok = !pidl.is_null() && SHGetPathFromIDListW(pidl, path.as_mut_ptr()) != 0;
        if !pidl.is_null() {
            CoTaskMemFree(pidl as *const _);
        }
        if co >= 0 {
            CoUninitialize();
        }
        ok
    };
    ok.then(|| from_wide_z(&path))
}

#[cfg(windows)]
fn platform_choose_save(owner: isize, dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetSaveFileNameW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
        OPENFILENAMEW,
    };
    let mut buf = vec![0u16; 4096];
    let initial: Vec<u16> = std::ffi::OsStr::new(name)
        .to_string_lossy()
        .encode_utf16()
        .take(buf.len() - 1)
        .collect();
    buf[..initial.len()].copy_from_slice(&initial);
    let dir_w = wide_z(dir.as_os_str());
    let title = wide_z(std::ffi::OsStr::new("Save as"));
    let filter: Vec<u16> = "All files\0*.*\0\0".encode_utf16().collect();
    // SAFETY: OPENFILENAMEW is a plain C struct; zeroed is its documented initial state.
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = owner as _;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.nFilterIndex = 1;
    ofn.lpstrFile = buf.as_mut_ptr();
    ofn.nMaxFile = buf.len() as u32;
    ofn.lpstrInitialDir = dir_w.as_ptr();
    ofn.lpstrTitle = title.as_ptr();
    ofn.Flags = OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    // SAFETY: every pointer in `ofn` points into buffers that outlive the call.
    let ok = unsafe {
        let co = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let ok = GetSaveFileNameW(&mut ofn);
        if co >= 0 {
            CoUninitialize();
        }
        ok
    };
    (ok != 0).then(|| from_wide_z(&buf))
}

#[cfg(not(windows))]
fn platform_choose_folder(
    _owner: isize,
    title: &str,
    start: Option<&std::path::Path>,
) -> Option<PathBuf> {
    use std::process::Command;
    let out = Command::new("zenity")
        .args(zenity_folder_args(title, start))
        .output()
        .or_else(|_| {
            Command::new("kdialog")
                .arg("--getexistingdirectory")
                .arg(start.map(|p| p.as_os_str().to_owned()).unwrap_or_default())
                .output()
        })
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    (out.status.success() && !line.is_empty()).then(|| PathBuf::from(line))
}

#[cfg(not(windows))]
fn platform_choose_save(_owner: isize, dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    use std::process::Command;
    let out = Command::new("zenity")
        .args(zenity_save_args(dir, name))
        .output()
        .or_else(|_| {
            Command::new("kdialog")
                .arg("--getsavefilename")
                .arg(dir.join(name))
                .output()
        })
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    (out.status.success() && !line.is_empty()).then(|| PathBuf::from(line))
}

#[cfg(test)]
mod v04_tests {
    use super::*;

    #[test]
    fn zenity_folder_and_save_args() {
        let a = zenity_folder_args("", Some(std::path::Path::new("/home/u/Downloads")));
        assert_eq!(a[..2], ["--file-selection", "--directory"]);
        assert!(a.contains(&"--title=Choose a folder".to_owned()));
        assert!(a.contains(&"--filename=/home/u/Downloads/".to_owned()));
        let s = zenity_save_args(std::path::Path::new("/tmp/dl"), "a b.zip");
        assert!(s.contains(&"--save".to_owned()));
        assert!(s.contains(&"--confirm-overwrite".to_owned()));
        let want = format!(
            "--filename={}",
            std::path::Path::new("/tmp/dl").join("a b.zip").display()
        );
        assert!(s.contains(&want));
    }
}
