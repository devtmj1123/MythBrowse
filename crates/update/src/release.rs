// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! A GitHub release (`GET /repos/{owner}/{repo}/releases/latest`, read-only, no token) and the
//! choice of what to install from it.
//!
//! * Channel: the open-source Open Build installs only assets whose name carries the
//!   `OpenBuild` token (`MythBrowse-OpenBuild-0.4.2-x64.msi`); official builds (feature
//!   `official`) ignore those. Both read the same repository's releases.
//! * Installer: the `.msi` asset for this machine. Names with another architecture
//!   (`arm64`, `aarch64`, `x86`/`i686`/`win32` on x64) are skipped; among the rest an asset
//!   naming this architecture wins over a neutral one, and a `MythBrowse…` name over others.
//! * Checksum: `<installer>.sha256` (a bare hash, or `hash  name` lines), else a sums file
//!   (`SHA256SUMS`, `sha256sums.txt`, `checksums.txt`) with a line for the installer.
//! * Every download URL must be this repository's release download path on `github.com`
//!   (GitHub then redirects to its object storage), so a tampered API answer cannot send the
//!   updater anywhere else.

use serde::Deserialize;
use url::Url;

use crate::version::Version;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    pub browser_download_url: String,
    #[serde(default)]
    pub content_type: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn parse(json: &str) -> Result<Release, String> {
        serde_json::from_str(json)
            .map_err(|e| format!("GitHub sent a release we can't read ({e})."))
    }

    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag_name)
    }
}

/// CPU architecture names as they appear in installer file names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
    X86,
}

impl Arch {
    pub fn current() -> Arch {
        if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else if cfg!(target_arch = "x86") {
            Arch::X86
        } else {
            Arch::X64
        }
    }

    fn tags(self) -> &'static [&'static str] {
        match self {
            Arch::X64 => &["x64", "x86_64", "x86-64", "amd64", "win64"],
            Arch::Arm64 => &["arm64", "aarch64"],
            Arch::X86 => &["x86", "i686", "i386", "win32"],
        }
    }
}

/// Split a file name into lowercase word tokens (`MythBrowse-0.4.0-x64.msi` ->
/// `mythbrowse`, `0`, `4`, `0`, `x64`, `msi`; `x86_64` stays one token).
fn tokens(name: &str) -> Vec<String> {
    name.to_ascii_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

fn names_arch(name: &str, arch: Arch) -> bool {
    let t = tokens(name);
    let lower = name.to_ascii_lowercase();
    arch.tags()
        .iter()
        .any(|tag| t.iter().any(|x| x == tag) || (tag.contains('-') && lower.contains(tag)))
}

/// Token in the installer name of open-source builds: `MythBrowse-OpenBuild-0.4.2-x64.msi`.
pub const OPEN_BUILD_TOKEN: &str = "openbuild";

/// Which installers a build may update itself with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The open-source "MythBrowse (Open Build)": only `…OpenBuild…` installers.
    Open,
    /// Official JustMyth builds: every installer except the open build's.
    Official,
}

impl Channel {
    /// This build's channel (feature `official`).
    pub const fn current() -> Channel {
        if cfg!(feature = "official") {
            Channel::Official
        } else {
            Channel::Open
        }
    }

    /// Whether an asset named `name` belongs to this channel.
    pub fn accepts(self, name: &str) -> bool {
        let open = tokens(name).iter().any(|t| t == OPEN_BUILD_TOKEN);
        open == (self == Channel::Open)
    }
}

/// The installer to download for `arch` on this build's [`Channel`], if the release has one.
pub fn pick_installer(assets: &[Asset], arch: Arch) -> Option<&Asset> {
    pick_installer_for(assets, arch, Channel::current())
}

/// The installer to download for `arch` on `channel`, if the release has one.
pub fn pick_installer_for(assets: &[Asset], arch: Arch, channel: Channel) -> Option<&Asset> {
    let others: Vec<Arch> = [Arch::X64, Arch::Arm64, Arch::X86]
        .into_iter()
        .filter(|a| *a != arch)
        .collect();
    assets
        .iter()
        .filter(|a| a.name.to_ascii_lowercase().ends_with(".msi"))
        .filter(|a| channel.accepts(&a.name))
        .filter(|a| {
            // "x86_64" also contains the x86 tag as a token prefix only, never as a token.
            names_arch(&a.name, arch) || !others.iter().any(|o| names_arch(&a.name, *o))
        })
        .max_by_key(|a| {
            (
                names_arch(&a.name, arch),
                a.name.to_ascii_lowercase().starts_with("mythbrowse"),
                std::cmp::Reverse(a.name.len()),
            )
        })
}

const SUMS_FILES: &[&str] = &[
    "sha256sums",
    "sha256sums.txt",
    "checksums.txt",
    "checksums.sha256",
];

/// The checksum asset that covers `installer`: `<installer>.sha256` first, else a sums file.
pub fn pick_checksum<'a>(assets: &'a [Asset], installer: &str) -> Option<&'a Asset> {
    let own = format!("{}.sha256", installer.to_ascii_lowercase());
    assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase() == own)
        .or_else(|| {
            assets
                .iter()
                .find(|a| SUMS_FILES.contains(&a.name.to_ascii_lowercase().as_str()))
        })
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The SHA-256 for `installer` in a checksum file: a lone hash, or `hash  name` / `hash *name`
/// lines (sha256sum / PowerShell `Get-FileHash` style, any case). Lowercase hex.
pub fn parse_checksum(text: &str, installer: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    for l in &lines {
        let mut parts = l.split_whitespace();
        let (Some(a), b) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Some(name) = b {
            let name = name.trim_start_matches('*');
            let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
            if is_hex64(a) && base.eq_ignore_ascii_case(installer) {
                return Some(a.to_ascii_lowercase());
            }
            // `name  hash` (some tools)
            if is_hex64(name) && a.eq_ignore_ascii_case(installer) {
                return Some(name.to_ascii_lowercase());
            }
        }
    }
    // A file with exactly one bare hash (and nothing else) belongs to its installer.
    if lines.len() == 1 {
        let only = lines[0].split_whitespace().next().unwrap_or("");
        if is_hex64(only) && lines[0].split_whitespace().count() == 1 {
            return Some(only.to_ascii_lowercase());
        }
    }
    None
}

/// Only `https://github.com/<owner>/<repo>/releases/download/…` (this repository's release
/// files) may be fetched.
pub fn allowed_download(url: &str, repo: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    let prefix = format!("/{}/releases/download/", repo.to_ascii_lowercase());
    u.scheme() == "https"
        && u.host_str() == Some("github.com")
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && u.path().to_ascii_lowercase().starts_with(&prefix)
        && !u.path().contains("/../")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            size: 10,
            browser_download_url: format!(
                "https://github.com/devtmj1123/MythBrowse/releases/download/v1/{name}"
            ),
            content_type: String::new(),
        }
    }

    #[test]
    fn parses_github_json() {
        let json = r#"{"url":"x","tag_name":"v0.4.0","name":"MythBrowse 0.4","draft":false,
            "prerelease":false,"html_url":"https://github.com/devtmj1123/MythBrowse/releases/tag/v0.4.0",
            "published_at":"2026-10-20T10:00:00Z","body":"Notes",
            "assets":[{"name":"MythBrowse-0.4.0-x64.msi","size":51234567,"content_type":"application/x-msi",
              "browser_download_url":"https://github.com/devtmj1123/MythBrowse/releases/download/v0.4.0/MythBrowse-0.4.0-x64.msi",
              "uploader":{"login":"a"}}]}"#;
        let r = Release::parse(json).unwrap();
        assert_eq!(r.version().unwrap().to_string(), "0.4.0");
        assert_eq!(r.assets[0].size, 51234567);
        assert!(Release::parse("{}").is_err());
        assert!(Release::parse("<html>").is_err());
    }

    #[test]
    fn picks_the_installer_for_this_arch() {
        let a = [
            asset("MythBrowse-0.4.0-arm64.msi"),
            asset("MythBrowse-0.4.0-x64.msi"),
            asset("MythBrowse-0.4.0-x64.msi.sha256"),
            asset("MythBrowse-0.4.0.zip"),
        ];
        assert_eq!(
            pick_installer_for(&a, Arch::X64, Channel::Official)
                .unwrap()
                .name,
            "MythBrowse-0.4.0-x64.msi"
        );
        assert_eq!(
            pick_installer_for(&a, Arch::Arm64, Channel::Official)
                .unwrap()
                .name,
            "MythBrowse-0.4.0-arm64.msi"
        );
        assert!(pick_installer_for(&a, Arch::X86, Channel::Official).is_none());
        // A neutral name is fine when nothing names this arch; x86_64 is x64, not x86.
        let b = [asset("MythBrowse.msi"), asset("mythbrowse-x86_64.MSI")];
        assert_eq!(
            pick_installer_for(&b, Arch::X64, Channel::Official)
                .unwrap()
                .name,
            "mythbrowse-x86_64.MSI"
        );
        assert_eq!(
            pick_installer_for(&b, Arch::X86, Channel::Official)
                .unwrap()
                .name,
            "MythBrowse.msi"
        );
        let c = [asset("Other.msi"), asset("MythBrowse.msi")];
        assert_eq!(
            pick_installer_for(&c, Arch::X64, Channel::Official)
                .unwrap()
                .name,
            "MythBrowse.msi"
        );
        assert!(
            pick_installer_for(&[asset("MythBrowse.exe")], Arch::X64, Channel::Official).is_none()
        );
    }

    #[test]
    fn open_and_official_channels_do_not_mix() {
        let a = [
            asset("MythBrowse-0.4.2-x64.msi"),
            asset("MythBrowse-OpenBuild-0.4.2-x64.msi"),
            asset("MythBrowse-OpenBuild-0.4.2-x64.msi.sha256"),
            asset("MythBrowse-OpenBuild-0.4.2-arm64.msi"),
        ];
        assert_eq!(
            pick_installer_for(&a, Arch::X64, Channel::Open)
                .unwrap()
                .name,
            "MythBrowse-OpenBuild-0.4.2-x64.msi"
        );
        assert_eq!(
            pick_installer_for(&a, Arch::Arm64, Channel::Open)
                .unwrap()
                .name,
            "MythBrowse-OpenBuild-0.4.2-arm64.msi"
        );
        assert_eq!(
            pick_installer_for(&a, Arch::X64, Channel::Official)
                .unwrap()
                .name,
            "MythBrowse-0.4.2-x64.msi"
        );
        // An official build never installs the open build, and the other way round.
        assert!(pick_installer_for(&a, Arch::Arm64, Channel::Official).is_none());
        let official_only = [asset("MythBrowse-0.4.2-x64.msi")];
        assert!(pick_installer_for(&official_only, Arch::X64, Channel::Open).is_none());
        // Case does not matter; a word merely containing it is not the token.
        assert!(Channel::Open.accepts("mythbrowse.OPENBUILD.x64.MSI"));
        assert!(!Channel::Open.accepts("MythBrowse-OpenBuilder-x64.msi"));
        assert_eq!(
            pick_installer(&a, Arch::X64).map(|x| Channel::current().accepts(&x.name)),
            Some(true)
        );
    }

    #[test]
    fn checksum_files() {
        let a = [
            asset("MythBrowse-x64.msi"),
            asset("SHA256SUMS"),
            asset("MythBrowse-x64.msi.sha256"),
        ];
        assert_eq!(
            pick_checksum(&a, "MythBrowse-x64.msi").unwrap().name,
            "MythBrowse-x64.msi.sha256"
        );
        assert_eq!(
            pick_checksum(&a[..2], "MythBrowse-x64.msi").unwrap().name,
            "SHA256SUMS"
        );
        assert!(pick_checksum(&a[..1], "MythBrowse-x64.msi").is_none());

        let h = "A".repeat(64);
        let lower = "a".repeat(64);
        assert_eq!(
            parse_checksum(&format!("{h}\n"), "x.msi"),
            Some(lower.clone())
        );
        assert_eq!(
            parse_checksum(
                &format!("{}  other.msi\n{h} *dist/x.msi\n", "b".repeat(64)),
                "x.msi"
            ),
            Some(lower.clone())
        );
        assert_eq!(parse_checksum(&format!("x.msi {h}"), "X.MSI"), Some(lower));
        assert_eq!(
            parse_checksum(&format!("{}  other.msi", "b".repeat(64)), "x.msi"),
            None
        );
        assert_eq!(parse_checksum("not a hash", "x.msi"), None);
    }

    #[test]
    fn only_this_repos_release_files() {
        let repo = "devtmj1123/MythBrowse";
        assert!(allowed_download(
            "https://github.com/devtmj1123/MythBrowse/releases/download/v0.4.0/MythBrowse-x64.msi",
            repo
        ));
        for bad in [
            "http://github.com/devtmj1123/MythBrowse/releases/download/v1/a.msi",
            "https://github.com.evil.example/devtmj1123/MythBrowse/releases/download/v1/a.msi",
            "https://github.com/someone/MythBrowse/releases/download/v1/a.msi",
            "https://github.com/devtmj1123/MythBrowse-fork/releases/download/v1/a.msi",
            "https://user@github.com/devtmj1123/MythBrowse/releases/download/v1/a.msi",
            "https://github.com:8443/devtmj1123/MythBrowse/releases/download/v1/a.msi",
            "https://evil.example/a.msi",
            "not a url",
        ] {
            assert!(!allowed_download(bad, repo), "{bad}");
        }
    }
}
