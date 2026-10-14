// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Settings (v0.4) on the shell side: `/api/settings*`, `site_data*` and `update_*`
//! (parsed in `mythbrowse_engine::settings_api`), and the start-up half
//! ([`AppState::settings_init`]): apply the saved search engine, downloads folder and
//! "ask where to save", and the once-a-day update check.
//!
//! Kept to this file (plus one `mod` line, one `ApiCall::Settings` arm and the
//! `settings_init()` call in main.rs) so it never touches input, rendering or packaging.
//!
//! * Search engine: `omnibox::set_search_engine` (process-wide), saved as profile pref
//!   `search_engine`. Suggestions stay on DuckDuckGo.
//! * Downloads: pref `downloads_dir` ("" = the system Downloads folder) -> `Manager::set_dir`;
//!   pref `downloads_ask` = "1" -> `Manager::set_ask` with the native Save-as dialog (on the
//!   transfer's worker thread). Choosing a folder runs the native folder chooser on its own
//!   thread; the page polls `settings` (`picking`) until it is closed.
//! * Cookies and site data: Servo's `SiteDataManager` (cookies, localStorage,
//!   sessionStorage, per site or all) and `NetworkManager::clear_cache` (HTTP cache).
//! * Updates: `mythbrowse_update::Updater` for the public repository's releases (read-only
//!   API, no token). Pref `update_auto` ("0" = off, default on) allows one automatic check a
//!   day at start-up; never in private windows, pinned-theme screenshot runs or the CI
//!   automation build. "Download & install" is the user's consent; Windows Installer then
//!   shows its own UI.
//! * Theme: pref `theme` (time / light / dark), read by every UI page (design/theme.js).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use http::StatusCode;
use log::{info, warn};
use mythbrowse_engine::chrome::{json_error, json_ok};
use mythbrowse_engine::omnibox::{self, SearchEngine};
use mythbrowse_engine::servo::StorageType;
use mythbrowse_engine::settings_api::{SettingsOp, Theme};
use mythbrowse_files::{os, picker};
use mythbrowse_update::{Config, RELEASE_REPO, Updater};
use serde_json::{Value, json};

use crate::AppState;

const PREF_SEARCH_ENGINE: &str = "search_engine";
const PREF_DOWNLOADS_DIR: &str = "downloads_dir";
const PREF_DOWNLOADS_ASK: &str = "downloads_ask";
const PREF_UPDATE_AUTO: &str = "update_auto";
const PREF_THEME: &str = "theme";

/// The build's commit, when the build sets one (`MYTHBROWSE_COMMIT`, or GitHub Actions'
/// `GITHUB_SHA`).
const BUILD_COMMIT: Option<&str> = match option_env!("MYTHBROWSE_COMMIT") {
    Some(c) => Some(c),
    None => option_env!("GITHUB_SHA"),
};

static UPDATER: OnceLock<Updater> = OnceLock::new();
/// A folder chosen in the folder chooser, not yet saved as the pref (main thread does that).
static PICKED: Mutex<Option<PathBuf>> = Mutex::new(None);
static PICKING: Mutex<bool> = Mutex::new(false);

fn updater() -> &'static Updater {
    UPDATER.get_or_init(|| {
        let mut cfg = Config::new(RELEASE_REPO, env!("CARGO_PKG_VERSION"));
        cfg.state_file =
            mythbrowse_storage::default_profile_dir().map(|d| d.join("update-check.json"));
        Updater::new(cfg, || {})
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The system Downloads folder (what `FilesHost::new` starts with).
fn default_downloads_dir() -> PathBuf {
    os::downloads_dir()
        .or_else(|| mythbrowse_storage::default_profile_dir().map(|p| p.join("Downloads")))
        .unwrap_or_else(|| std::env::temp_dir().join("MythBrowse Downloads"))
}

impl AppState {
    fn settings_pref(&self, key: &str) -> Option<String> {
        self.store
            .pref(key)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
    }

    fn settings_set_pref(&self, key: &str, value: &str) -> Result<(), String> {
        self.store
            .set_pref(key, value)
            .map_err(|e| format!("Could not save the setting ({e})."))
    }

    #[cfg(windows)]
    fn settings_owner_window(&self) -> isize {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match self.window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        }
    }

    #[cfg(not(windows))]
    fn settings_owner_window(&self) -> isize {
        0
    }

    fn settings_apply_ask(&self, on: bool) {
        let owner = self.settings_owner_window();
        self.files.manager.set_ask(on.then(|| {
            Arc::new(move |dir: &std::path::Path, name: &str| {
                picker::choose_save_path(owner, dir, name)
            }) as mythbrowse_files::download::AskPath
        }));
    }

    /// Start-up: apply the saved settings and maybe check for an update.
    pub(crate) fn settings_init(&self) {
        // Cookie policy: blocked sites, and "clear on exit" left over from an unclean exit.
        self.cookies_init();
        if let Some(e) = self
            .settings_pref(PREF_SEARCH_ENGINE)
            .and_then(|v| SearchEngine::parse(&v))
        {
            omnibox::set_search_engine(e);
        }
        if let Some(dir) = self.settings_pref(PREF_DOWNLOADS_DIR).map(PathBuf::from) {
            if dir.is_dir() {
                self.files.manager.set_dir(dir);
            } else {
                warn!(
                    "downloads folder {} is gone; using the default",
                    dir.display()
                );
            }
        }
        if self.settings_pref(PREF_DOWNLOADS_ASK).as_deref() == Some("1") {
            self.settings_apply_ask(true);
        }
        let auto = self.settings_pref(PREF_UPDATE_AUTO).as_deref() != Some("0");
        let quiet = self.agent.private
            || self.theme_hour.is_some()
            || std::env::var("MYTHBROWSE_NO_UPDATE_CHECK").is_ok_and(|v| v == "1");
        if auto && !quiet && updater().check(false) {
            info!("update: daily check started");
        }
    }

    fn settings_theme(&self) -> Theme {
        self.settings_pref(PREF_THEME)
            .and_then(|v| Theme::parse(&v))
            .unwrap_or(Theme::Time)
    }

    fn settings_json(&self) -> Value {
        if let Some(dir) = lock(&PICKED).take() {
            if let Err(e) = self.settings_set_pref(PREF_DOWNLOADS_DIR, &dir.to_string_lossy()) {
                warn!("{e}");
            }
        }
        let folder = self.files.manager.dir();
        let engine = omnibox::search_engine();
        json!({
            "search_engine": engine.id(),
            "engines": SearchEngine::ALL.iter().map(|e| json!({ "id": e.id(), "name": e.name() })).collect::<Vec<_>>(),
            "ask_save": self.files.manager.asks(),
            "downloads_folder": folder.display().to_string(),
            "downloads_folder_custom": self.settings_pref(PREF_DOWNLOADS_DIR).is_some(),
            "picking": *lock(&PICKING),
            "update_auto": self.settings_pref(PREF_UPDATE_AUTO).as_deref() != Some("0"),
            "theme": self.settings_theme().as_str(),
            "version": env!("CARGO_PKG_VERSION"),
            "commit": BUILD_COMMIT.map(|c| c.chars().take(12).collect::<String>()),
            "private": self.agent.private,
            "release_repo": RELEASE_REPO,
            "update": updater().status(),
        })
    }

    fn site_data_json(&self) -> Value {
        let servo = self.engine.servo();
        let sites: Vec<Value> = servo
            .site_data_manager()
            .site_data(StorageType::all())
            .into_iter()
            .map(|s| {
                let t = s.storage_types();
                json!({
                    "site": s.name(),
                    "cookies": t.contains(StorageType::Cookies),
                    "local": t.contains(StorageType::Local),
                    "session": t.contains(StorageType::Session),
                })
            })
            .collect();
        let cache_entries = servo.network_manager().cache_entries().len();
        json!({ "sites": sites, "cache_entries": cache_entries })
    }

    pub(crate) fn settings_api(&self, op: SettingsOp) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, json_ok(v));
        let err = |m: &str| (StatusCode::OK, json_error(m));
        match op {
            SettingsOp::Get => ok(self.settings_json()),
            SettingsOp::Save {
                search_engine,
                ask_save,
                update_auto,
                theme,
            } => {
                let mut saved = Ok(());
                if let Some(e) = search_engine {
                    omnibox::set_search_engine(e);
                    saved = saved.and(self.settings_set_pref(PREF_SEARCH_ENGINE, e.id()));
                }
                if let Some(on) = ask_save {
                    self.settings_apply_ask(on);
                    saved = saved.and(
                        self.settings_set_pref(PREF_DOWNLOADS_ASK, if on { "1" } else { "0" }),
                    );
                }
                if let Some(on) = update_auto {
                    saved = saved
                        .and(self.settings_set_pref(PREF_UPDATE_AUTO, if on { "1" } else { "0" }));
                }
                if let Some(t) = theme {
                    saved = saved.and(self.settings_set_pref(PREF_THEME, t.as_str()));
                }
                match saved {
                    Ok(()) => ok(self.settings_json()),
                    Err(e) => err(&e),
                }
            }
            SettingsOp::DownloadsFolderPick => {
                {
                    let mut p = lock(&PICKING);
                    if *p {
                        return ok(self.settings_json());
                    }
                    *p = true;
                }
                let owner = self.settings_owner_window();
                let start = self.files.manager.dir();
                let manager = self.files.manager.clone();
                let spawned = std::thread::Builder::new()
                    .name("folder-picker".into())
                    .spawn(move || {
                        if let Some(dir) = picker::choose_folder(
                            owner,
                            "Choose where downloads are saved",
                            Some(&start),
                        ) {
                            info!("downloads folder -> {}", dir.display());
                            manager.set_dir(dir.clone());
                            *lock(&PICKED) = Some(dir);
                        }
                        *lock(&PICKING) = false;
                    });
                if let Err(e) = spawned {
                    *lock(&PICKING) = false;
                    return err(&format!("Could not open the folder chooser ({e})."));
                }
                ok(self.settings_json())
            }
            SettingsOp::DownloadsFolderReset => {
                self.files.manager.set_dir(default_downloads_dir());
                *lock(&PICKED) = None;
                match self.settings_set_pref(PREF_DOWNLOADS_DIR, "") {
                    Ok(()) => ok(self.settings_json()),
                    Err(e) => err(&e),
                }
            }
            SettingsOp::SiteData => ok(self.site_data_json()),
            SettingsOp::SiteDataClear { what, site } => {
                {
                    let servo = self.engine.servo();
                    let sdm = servo.site_data_manager();
                    let mut types = StorageType::empty();
                    if what.cookies {
                        types |= StorageType::Cookies;
                    }
                    if what.storage {
                        types |= StorageType::Local | StorageType::Session;
                    }
                    match &site {
                        Some(s) if !types.is_empty() => sdm.clear_site_data(&[s.as_str()], types),
                        Some(_) => {}
                        None => {
                            if what.cookies {
                                sdm.clear_cookies();
                            }
                            if what.storage {
                                let all: Vec<String> = sdm
                                    .site_data(StorageType::Local | StorageType::Session)
                                    .into_iter()
                                    .map(|s| s.name())
                                    .collect();
                                let refs: Vec<&str> = all.iter().map(String::as_str).collect();
                                if !refs.is_empty() {
                                    sdm.clear_site_data(
                                        &refs,
                                        StorageType::Local | StorageType::Session,
                                    );
                                }
                            }
                        }
                    }
                    if what.cache {
                        servo.network_manager().clear_cache();
                    }
                }
                info!(
                    "site data cleared: cookies={} storage={} cache={} site={}",
                    what.cookies,
                    what.storage,
                    what.cache,
                    site.as_deref().unwrap_or("all")
                );
                ok(self.site_data_json())
            }
            SettingsOp::UpdateStatus => ok(json!({ "update": updater().status() })),
            SettingsOp::UpdateCheck => {
                updater().check(true);
                ok(json!({ "update": updater().status() }))
            }
            SettingsOp::Cookies(op) => self.cookies_api(op),
            SettingsOp::UpdateInstall => match updater().install() {
                Ok(()) => ok(json!({ "update": updater().status() })),
                Err(e) => err(&e),
            },
        }
    }
}
