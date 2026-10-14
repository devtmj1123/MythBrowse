// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Dangerous download types: files that run code when opened (programs, installers, scripts,
//! shortcuts, macro documents, disk images that mount programs). Such a download waits for
//! the user's explicit "Keep" before a single byte is written, and opening one later asks
//! again. The list errs on the side of asking; it is about what Windows (and Linux desktops)
//! would execute on a double-click, not about whether the file is malware.

use crate::names;

/// Extensions (lower-case, no dot) that run code when opened.
pub const DANGEROUS_EXTENSIONS: &[&str] = &[
    // Windows programs and installers
    "exe",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "appinstaller",
    "msp",
    "mst",
    "com",
    "scr",
    "pif",
    "cpl",
    "dll",
    "sys",
    "drv",
    "ocx",
    "application",
    "gadget",
    "xbap",
    // Scripts run by Windows Script Host, PowerShell, cmd
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "psd1",
    "ps1xml",
    "psc1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "wsc",
    "hta",
    "msc",
    "scf",
    "sct",
    "ws",
    // Shortcuts, settings and registry
    "lnk",
    "url",
    "reg",
    "inf",
    "settingcontent-ms",
    "library-ms",
    "search-ms",
    "desktopthemepackfile",
    // Help files and Office documents with macros
    "chm",
    "hlp",
    "docm",
    "dotm",
    "xlsm",
    "xltm",
    "xlam",
    "pptm",
    "potm",
    "ppam",
    "ppsm",
    "sldm",
    // Disk images (mount and auto-run programs)
    "iso",
    "img",
    "vhd",
    "vhdx",
    // Java and other runtimes
    "jar",
    "jnlp",
    "py",
    "pyw",
    "pyc",
    "pl",
    "rb",
    // Linux / macOS executables and packages
    "sh",
    "bash",
    "zsh",
    "run",
    "bin",
    "appimage",
    "deb",
    "rpm",
    "desktop",
    "command",
    "dmg",
    "pkg",
    "apk",
    "xapk",
];

/// True when a file with this name could run code when opened.
pub fn is_dangerous_name(name: &str) -> bool {
    match names::extension(name) {
        Some(ext) => DANGEROUS_EXTENSIONS.contains(&ext.as_str()),
        None => false,
    }
}

/// True for MIME types that are executables or installers whatever the name says.
pub fn is_dangerous_mime(mime: &str) -> bool {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "application/x-msdownload"
            | "application/x-msdos-program"
            | "application/vnd.microsoft.portable-executable"
            | "application/x-msi"
            | "application/x-ms-installer"
            | "application/x-ms-shortcut"
            | "application/hta"
            | "application/x-sh"
            | "application/x-shellscript"
            | "application/java-archive"
            | "application/vnd.android.package-archive"
            | "application/x-apple-diskimage"
            | "application/x-iso9660-image"
    )
}

/// A dangerous download: by name or by type.
pub fn is_dangerous(name: &str, mime: Option<&str>) -> bool {
    is_dangerous_name(name) || mime.is_some_and(is_dangerous_mime)
}

/// One plain sentence for the warning card.
pub fn warning(name: &str) -> String {
    let what = match names::extension(name).as_deref() {
        Some("exe" | "msi" | "msix" | "appx" | "com" | "scr" | "pif" | "msp") => {
            "This is a program. It can change your computer"
        }
        Some("bat" | "cmd" | "ps1" | "vbs" | "js" | "jse" | "wsf" | "hta" | "sh" | "py") => {
            "This is a script. It can run commands on your computer"
        }
        Some("docm" | "xlsm" | "pptm" | "dotm" | "xltm" | "potm") => {
            "This document contains macros, which can run code"
        }
        Some("iso" | "img" | "vhd" | "vhdx" | "dmg") => {
            "This disk image can contain programs that run when opened"
        }
        Some("lnk" | "url" | "reg" | "inf" | "scf") => {
            "This file can change settings or start programs"
        }
        _ => "This type of file can harm your computer",
    };
    format!("{what}. Keep it only if you trust where it came from.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_types() {
        for n in [
            "setup.exe",
            "SETUP.EXE",
            "x.msi",
            "run.bat",
            "a.cmd",
            "s.ps1",
            "v.vbs",
            "j.js",
            "h.hta",
            "l.lnk",
            "r.reg",
            "m.docm",
            "d.iso",
            "i.sh",
            "p.appimage",
            "x.jar",
            "invoice.pdf.exe",
            "shortcut.url",
        ] {
            assert!(is_dangerous_name(n), "{n}");
        }
        for n in [
            "report.pdf",
            "photo.jpg",
            "a.zip",
            "b.docx",
            "c.txt",
            "noext",
            ".exe",
            "exe",
            "setup.exe.txt",
            "notes.md",
            "data.csv",
        ] {
            assert!(!is_dangerous_name(n), "{n}");
        }
        assert!(is_dangerous("download", Some("application/x-msdownload")));
        assert!(is_dangerous("x.bin", Some("application/octet-stream")));
        assert!(!is_dangerous("x.zip", Some("application/zip")));
        assert!(is_dangerous_mime("application/x-msi; charset=binary"));
        assert!(warning("a.exe").starts_with("This is a program"));
        assert!(warning("a.ps1").starts_with("This is a script"));
        assert!(warning("a.weird").starts_with("This type of file"));
    }
}
