// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The update state machine behind Settings > About.
//!
//! ```text
//! idle --check--> checking --> up_to_date | available | no_installer | error
//! available --install (the user's click)--> downloading --> verifying --> installing
//!                                                        \-> error (size / SHA-256 mismatch, network)
//! ```
//!
//! Work runs on short-lived threads; `on_change` fires after every state change (and every
//! ~250 ms while downloading) so the shell can wake its loop and push the status to the page.
//! Nothing is ever uploaded and no token is used: the one API call is the public, read-only
//! `GET /repos/{repo}/releases/latest`. The installer is launched only from [`Updater::install`],
//! which the shell calls only for the user's "Download & install" click; Windows Installer then
//! shows its own UI (and UAC prompt).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{info, warn};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::release::{self, Arch, Asset, Release};
use crate::version::Version;

/// At most one automatic check per this long.
pub const CHECK_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;
/// Checksum files are tiny; anything bigger is not one.
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
/// GitHub's API answer for a release is a few KB (the notes can be long).
const MAX_API_BYTES: u64 = 4 * 1024 * 1024;

pub type Launcher = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;
pub type UrlCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct Config {
    /// `owner/name` of the public repository whose releases are checked.
    pub repo: String,
    /// This build's version (`CARGO_PKG_VERSION`).
    pub current: String,
    /// `https://api.github.com` (tests point it at a local server).
    pub api_base: String,
    /// Where `last_check_ms` is kept between runs (profile dir); `None` = memory only.
    pub state_file: Option<PathBuf>,
    /// Where the installer is downloaded (a temp folder).
    pub download_dir: PathBuf,
    /// Which download URLs may be fetched (default: [`release::allowed_download`] for `repo`).
    pub allow_url: UrlCheck,
    /// Starts the installer (default: `msiexec /i <path>` on Windows).
    pub launcher: Launcher,
    pub arch: Arch,
}

impl Config {
    pub fn new(repo: &str, current: &str) -> Config {
        let r = repo.to_owned();
        Config {
            repo: repo.to_owned(),
            current: current.to_owned(),
            api_base: "https://api.github.com".into(),
            state_file: None,
            download_dir: std::env::temp_dir().join("MythBrowse-update"),
            allow_url: Arc::new(move |u| release::allowed_download(u, &r)),
            launcher: Arc::new(launch_msiexec),
            arch: Arch::current(),
        }
    }
}

/// `msiexec /i "<path>"`: Windows Installer's normal UI (the user sees and confirms the
/// upgrade, UAC elevates). Not available elsewhere.
pub fn launch_msiexec(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::process::Command::new("msiexec")
            .arg("/i")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Could not start Windows Installer ({e})."))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Installing updates is only available on Windows.".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available,
    /// A newer release exists but has no installer for this machine.
    NoInstaller,
    Downloading,
    Verifying,
    Installing,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub phase: Phase,
    pub current: String,
    /// The latest release's version (`0.4.0`), once checked.
    pub latest: Option<String>,
    pub release_name: Option<String>,
    pub release_url: Option<String>,
    pub published_at: Option<String>,
    pub installer: Option<String>,
    pub installer_size: Option<u64>,
    /// The release publishes a SHA-256 for the installer (verified after download).
    pub checksum: bool,
    pub received: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// When the last check finished (ms since the epoch), from any run.
    pub checked_ms: Option<i64>,
}

struct Inner {
    status: Status,
    installer: Option<Asset>,
    checksum: Option<Asset>,
    busy: bool,
}

#[derive(Clone)]
pub struct Updater {
    cfg: Config,
    inner: Arc<Mutex<Inner>>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Is an automatic check due? Once a day; a clock that went backwards counts as due.
pub fn due(last_check_ms: Option<i64>, now_ms: i64) -> bool {
    match last_check_ms {
        None => true,
        Some(t) => now_ms < t || now_ms - t >= CHECK_INTERVAL_MS,
    }
}

fn read_state(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("last_check_ms")?.as_i64()
}

fn write_state(path: &Path, t: i64) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = serde_json::json!({ "last_check_ms": t }).to_string();
    if let Err(e) = std::fs::write(path, body) {
        warn!("update: could not save the last check time: {e}");
    }
}

/// The HTTP client. A server on this machine (tests) is never reached through a proxy, as
/// in `mythbrowse_files::download`.
fn agent(url: &str) -> ureq::Agent {
    let loopback = url::Url::parse(url).ok().is_some_and(|u| {
        matches!(u.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(u.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
            || u.host_str() == Some("localhost")
    });
    let mut cfg = ureq::Agent::config_builder();
    if loopback {
        cfg = cfg.proxy(None);
    }
    cfg.http_status_as_error(false)
        .max_redirects(10)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .user_agent(concat!("MythBrowse-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn net_error(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Io(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => {
            "Couldn't reach GitHub. Check your connection and try again.".into()
        }
        ureq::Error::Timeout(_) => "GitHub took too long to answer. Try again later.".into(),
        other => format!("Couldn't reach GitHub ({other})."),
    }
}

impl Updater {
    pub fn new(cfg: Config, on_change: impl Fn() + Send + Sync + 'static) -> Updater {
        let checked_ms = cfg.state_file.as_deref().and_then(read_state);
        let current = Version::parse(&cfg.current)
            .map(|v| v.to_string())
            .unwrap_or_else(|| cfg.current.clone());
        Updater {
            inner: Arc::new(Mutex::new(Inner {
                status: Status {
                    phase: Phase::Idle,
                    current,
                    latest: None,
                    release_name: None,
                    release_url: None,
                    published_at: None,
                    installer: None,
                    installer_size: None,
                    checksum: false,
                    received: 0,
                    total: None,
                    error: None,
                    checked_ms,
                },
                installer: None,
                checksum: None,
                busy: false,
            })),
            cfg,
            on_change: Arc::new(on_change),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn status(&self) -> Status {
        self.lock().status.clone()
    }

    fn set(&self, f: impl FnOnce(&mut Inner)) {
        f(&mut self.lock());
        (self.on_change)();
    }

    fn fail(&self, msg: String) {
        warn!("update: {msg}");
        self.set(|i| {
            i.status.phase = Phase::Error;
            i.status.error = Some(msg);
            i.busy = false;
        });
    }

    /// Is an automatic check due now?
    pub fn due(&self) -> bool {
        due(self.lock().status.checked_ms, now_ms())
    }

    /// Check for a newer release on a worker thread. `force` = the user asked (ignores the
    /// once-a-day limit). Returns false when nothing was started (busy, or not due).
    pub fn check(&self, force: bool) -> bool {
        {
            let mut i = self.lock();
            if i.busy || (!force && !due(i.status.checked_ms, now_ms())) {
                return false;
            }
            i.busy = true;
            i.status.phase = Phase::Checking;
            i.status.error = None;
        }
        (self.on_change)();
        let me = self.clone();
        std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || me.run_check())
            .is_ok()
    }

    /// Blocking check (the worker thread's body; tests call it directly).
    pub fn run_check(&self) {
        match self.fetch_latest() {
            Ok(rel) => self.apply_release(rel),
            Err(msg) => self.fail(msg),
        }
        let t = now_ms();
        if let Some(p) = &self.cfg.state_file {
            write_state(p, t);
        }
        self.set(|i| i.status.checked_ms = Some(t));
    }

    fn fetch_latest(&self) -> Result<Option<Release>, String> {
        let url = format!(
            "{}/repos/{}/releases/latest",
            self.cfg.api_base.trim_end_matches('/'),
            self.cfg.repo
        );
        let mut resp = agent(&url)
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .map_err(|e| net_error(&e))?;
        match resp.status().as_u16() {
            200 => {}
            // No published release yet (GitHub answers 404 for /latest then).
            404 => return Ok(None),
            403 | 429 => {
                return Err(
                    "GitHub is limiting update checks from this network. Try again in an hour."
                        .into(),
                );
            }
            s => return Err(format!("GitHub answered {s} to the update check.")),
        }
        let text = resp
            .body_mut()
            .with_config()
            .limit(MAX_API_BYTES)
            .read_to_string()
            .map_err(|e| net_error(&e))?;
        Release::parse(&text).map(Some)
    }

    /// Decide what the release means for this build.
    pub fn apply_release(&self, rel: Option<Release>) {
        let current = Version::parse(&self.cfg.current);
        self.set(|i| {
            i.busy = false;
            i.installer = None;
            i.checksum = None;
            let s = &mut i.status;
            s.error = None;
            s.received = 0;
            s.total = None;
            s.installer = None;
            s.installer_size = None;
            s.checksum = false;
            let Some(rel) = rel.filter(|r| !r.draft) else {
                s.phase = Phase::UpToDate;
                s.latest = None;
                return;
            };
            let latest = rel.version();
            s.latest = latest.as_ref().map(|v| v.to_string());
            s.release_name = rel.name.clone().filter(|n| !n.trim().is_empty());
            s.release_url = Some(rel.html_url.clone()).filter(|u| !u.is_empty());
            s.published_at = rel.published_at.clone();
            let newer = matches!((&latest, &current), (Some(l), Some(c)) if l > c);
            if !newer {
                s.phase = Phase::UpToDate;
                return;
            }
            match release::pick_installer(&rel.assets, self.cfg.arch) {
                Some(a) if (self.cfg.allow_url)(&a.browser_download_url) => {
                    s.phase = Phase::Available;
                    s.installer = Some(a.name.clone());
                    s.installer_size = (a.size > 0).then_some(a.size);
                    let sum = release::pick_checksum(&rel.assets, &a.name)
                        .filter(|c| (self.cfg.allow_url)(&c.browser_download_url))
                        .cloned();
                    s.checksum = sum.is_some();
                    i.checksum = sum;
                    i.installer = Some(a.clone());
                }
                _ => s.phase = Phase::NoInstaller,
            }
        });
        let s = self.status();
        info!(
            "update: current {} latest {:?} -> {:?}",
            s.current, s.latest, s.phase
        );
    }

    /// Download, verify and launch the installer (the user's "Download & install" click).
    pub fn install(&self) -> Result<(), String> {
        {
            let mut i = self.lock();
            if i.busy {
                return Err("An update is already in progress.".into());
            }
            if i.installer.is_none() {
                return Err("There is no update to install. Check for updates first.".into());
            }
            i.busy = true;
            i.status.phase = Phase::Downloading;
            i.status.error = None;
            i.status.received = 0;
            i.status.total = i.installer.as_ref().map(|a| a.size).filter(|s| *s > 0);
        }
        (self.on_change)();
        let me = self.clone();
        std::thread::Builder::new()
            .name("update-install".into())
            .spawn(move || me.run_install())
            .map(|_| ())
            .map_err(|e| format!("Could not start the download ({e})."))
    }

    /// Blocking install (the worker thread's body; tests call it directly).
    pub fn run_install(&self) {
        let (asset, sum) = {
            let i = self.lock();
            (i.installer.clone(), i.checksum.clone())
        };
        let Some(asset) = asset else {
            return self.fail("There is no update to install.".into());
        };
        let result = self
            .download_verified(&asset, sum.as_ref())
            .and_then(|path| {
                self.set(|i| i.status.phase = Phase::Installing);
                info!("update: launching the installer {}", path.display());
                (self.cfg.launcher)(&path)
            });
        match result {
            Ok(()) => self.set(|i| i.busy = false),
            Err(msg) => self.fail(msg),
        }
    }

    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        if !(self.cfg.allow_url)(url) {
            return Err("The update points to an address MythBrowse doesn't trust.".into());
        }
        let resp = agent(url).get(url).call().map_err(|e| net_error(&e))?;
        match resp.status().as_u16() {
            200 => Ok(resp),
            s => Err(format!("The download failed (GitHub answered {s}).")),
        }
    }

    fn download_verified(&self, asset: &Asset, sum: Option<&Asset>) -> Result<PathBuf, String> {
        // The checksum first: a tampered or missing one fails before 50 MB are fetched.
        let expected_sha = match sum {
            None => None,
            Some(c) => {
                let text = self
                    .get(&c.browser_download_url)?
                    .body_mut()
                    .with_config()
                    .limit(MAX_CHECKSUM_BYTES)
                    .read_to_string()
                    .map_err(|e| net_error(&e))?;
                Some(release::parse_checksum(&text, &asset.name).ok_or_else(|| {
                    format!(
                        "The release's checksum file has no SHA-256 for {}.",
                        asset.name
                    )
                })?)
            }
        };
        let safe_name: String = asset
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || ".-_".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        std::fs::create_dir_all(&self.cfg.download_dir)
            .map_err(|e| format!("Could not create the download folder ({e})."))?;
        let target = self.cfg.download_dir.join(&safe_name);
        let part = self.cfg.download_dir.join(format!("{safe_name}.part"));
        let mut resp = self.get(&asset.browser_download_url)?;
        let expected = (asset.size > 0).then_some(asset.size);
        let mut body = resp.body_mut().as_reader();
        let mut file = std::fs::File::create(&part)
            .map_err(|e| format!("Could not save the update ({e})."))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut received: u64 = 0;
        let mut last = Instant::now();
        let outcome: Result<(), String> = loop {
            let n = match body.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) => break Err(format!("The download was interrupted ({e}).")),
            };
            received += n as u64;
            if expected.is_some_and(|e| received > e) {
                break Err(
                    "The download is bigger than the release says. It was discarded.".into(),
                );
            }
            hasher.update(&buf[..n]);
            if let Err(e) = file.write_all(&buf[..n]) {
                break Err(format!("Could not save the update ({e})."));
            }
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                self.set(|i| i.status.received = received);
            }
        };
        drop(file);
        let discard = |msg: String| {
            let _ = std::fs::remove_file(&part);
            Err(msg)
        };
        if let Err(msg) = outcome {
            return discard(msg);
        }
        self.set(|i| {
            i.status.received = received;
            i.status.phase = Phase::Verifying;
        });
        if let Some(e) = expected
            && received != e
        {
            return discard(format!(
                "The download is incomplete ({received} of {e} bytes). It was discarded."
            ));
        }
        if received == 0 {
            return discard("The download was empty. It was discarded.".into());
        }
        if let Some(want) = expected_sha {
            let got: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if got != want {
                return discard(
                    "The download doesn't match the release's SHA-256. It was discarded.".into(),
                );
            }
        }
        let _ = std::fs::remove_file(&target);
        std::fs::rename(&part, &target).map_err(|e| format!("Could not save the update ({e})."))?;
        Ok(target)
    }
}

#[cfg(test)]
mod tests;
