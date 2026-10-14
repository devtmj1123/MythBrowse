// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Cookies (v0.4.2) on the shell side: the `cookies*` / `cookie_*` settings ops (parsed in
//! `mythbrowse_engine::cookies_api`), the per-site policy (pref `cookie_policy`) and its
//! enforcement.
//!
//! * Listing, delete-one and blocking go through Servo's `SiteDataManager` with the control
//!   URLs of the vendored servo-net patch 0002 (`third_party/servo-net/mythbrowse_cookies.rs`):
//!   `cookies_for_url(mythbrowse-cookie-jar://<site>/)` lists every cookie of a site with its
//!   value already redacted to `mbmeta.<size>.<host-only>`; `set_cookie_for_url` on
//!   `mythbrowse-cookie-delete://` / `mythbrowse-cookie-block://` deletes one / (un)blocks a
//!   site. Clearing a site or everything is the stock `clear_site_data` / `clear_cookies`.
//! * **Cookie values never leave this file**: rows carry name, domain, path, expiry, flags
//!   and size only; logs name the site, never a cookie. Myth / agent tabs cannot reach these
//!   ops (internal pages only, `settings_api`).
//! * Block: the site is put in net's blocked set at start-up and when chosen (its cookies are
//!   then neither stored nor sent, existing ones cleared). Clear on exit (per site, or global
//!   with "Allow" as the exception): applied in `shutdown()` and again at the next start in
//!   case MythBrowse did not close cleanly.
//! * Third-party cookie blocking: not offered (`cookies_api::THIRD_PARTY_NOTE`).
//! * Keeping cookies after restart (v0.4.2 prep, `ui.json` `persist_cookies`, **default
//!   off**): only then Servo gets `<profile>/engine-data` as its `config_dir`, and before it
//!   starts [`persistent_engine_dir`] installs the jar encryption (servo-net patch 0003:
//!   AES-256-GCM, key in the OS key store via `mythbrowse_passwords::crypto::JarSealer`).
//!   Without it every cookie ends with the session (shown in the UI). See docs/COOKIES.md.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use http::StatusCode;
use log::{info, warn};
use mythbrowse_engine::chrome::{json_error, json_ok};
use mythbrowse_engine::cookies_api::{
    self, CookieOp, CookiePolicies, CookieRow, PREF_COOKIE_POLICY, SitePolicy,
};
use mythbrowse_engine::servo::StorageType;
use serde_json::{Value, json};
use url::Url;

use crate::AppState;

/// Sub-folder of the profile Servo keeps cookies and site storage in (persist on).
pub(crate) const ENGINE_DATA_DIR: &str = "engine-data";

const JAR_SCHEME: &str = "mythbrowse-cookie-jar";
const DELETE_SCHEME: &str = "mythbrowse-cookie-delete";
const BLOCK_SCHEME: &str = "mythbrowse-cookie-block";

/// Whether this run keeps cookies after MythBrowse closes (set at start-up by
/// [`persistent_engine_dir`]).
static PERSISTENT: AtomicBool = AtomicBool::new(false);

/// Start-up, before the engine exists: with `persist_cookies` on (never in a private
/// session), install the cookie-jar encryption and return the folder Servo keeps its data in.
/// `None` (nothing kept) when the pref is off, there is no profile, or the key store fails
/// (then cookies are not saved rather than saved in the clear).
pub(crate) fn persistent_engine_dir(
    profile: Option<&Path>,
    enabled: bool,
    private: bool,
) -> Option<PathBuf> {
    net::mythbrowse_cookies::set_jar_codec(None);
    PERSISTENT.store(false, Ordering::Relaxed);
    if private {
        return None; // a private session neither reads nor deletes what is kept
    }
    let profile = profile?;
    let dir = profile.join(ENGINE_DATA_DIR);
    if !enabled {
        // Turned off: what an earlier run kept (cookies, site storage) goes.
        if dir.exists() {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => info!("cookies: kept cookies and site storage deleted (setting off)"),
                Err(e) => warn!("cookies: could not delete {ENGINE_DATA_DIR} ({e})"),
            }
        }
        return None;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("cookies: cannot create {ENGINE_DATA_DIR} ({e}); not kept after restart");
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let store = mythbrowse_ai::PlatformStore::new(profile);
    let sealer = match mythbrowse_passwords::crypto::JarSealer::load_or_create(&store) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!("cookies: key store unavailable ({e}); not kept after restart");
            return None;
        }
    };
    let (a, b) = (sealer.clone(), sealer);
    net::mythbrowse_cookies::set_jar_codec(Some(net::mythbrowse_cookies::JarCodec {
        seal: Arc::new(move |plain: &[u8]| a.seal(plain).ok()),
        open: Arc::new(move |blob: &[u8]| b.open(blob).ok().map(|p| p.to_vec())),
    }));
    PERSISTENT.store(true, Ordering::Relaxed);
    info!("cookies: kept after restart (encrypted jar in {ENGINE_DATA_DIR})");
    Some(dir)
}

/// The `persist_cookies` value in `<profile>/ui.json` (off when missing).
pub(crate) fn persist_pref(ui_json: Option<&Path>) -> bool {
    ui_json
        .and_then(|p| crate::load_ui_pref(p, cookies_api::PREF_PERSIST_COOKIES))
        .is_some_and(|v| v == "1")
}

/// The registrable domain the cookie jar files `host`'s cookies under (servo-net's
/// `reg_host`): an IP address as-is, else its eTLD+1, lowercase.
pub(crate) fn site_of_host(host: &str) -> String {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return ip.to_string().to_lowercase();
    }
    net_traits::pub_domains::reg_suffix(host).to_lowercase()
}

fn control_url(scheme: &str, site: &str) -> Option<Url> {
    Url::parse(&format!("{scheme}://{site}/")).ok()
}

impl AppState {
    fn cookie_policies(&self) -> CookiePolicies {
        let raw = self.store.pref(PREF_COOKIE_POLICY).ok().flatten();
        CookiePolicies::from_pref(raw.as_deref())
    }

    fn cookie_policies_save(&self, p: &CookiePolicies) -> Result<(), String> {
        self.store
            .set_pref(PREF_COOKIE_POLICY, &p.to_pref())
            .map_err(|e| format!("Could not save the cookie setting ({e})."))
    }

    /// Every cookie of `site`, without values.
    fn cookie_rows(&self, site: &str) -> Vec<CookieRow> {
        let Some(url) = control_url(JAR_SCHEME, site) else {
            return Vec::new();
        };
        let servo = self.engine.servo();
        let cookies = servo
            .site_data_manager()
            .cookies_for_url(url, net_traits::CookieSource::HTTP);
        let mut rows: Vec<CookieRow> = cookies
            .into_iter()
            .map(|c| {
                // An unpatched engine would answer with real values: count, then drop.
                let (size, host_only) = cookies_api::decode_meta(c.value())
                    .unwrap_or((c.name().len() + c.value().len(), true));
                CookieRow {
                    name: c.name().to_owned(),
                    domain: c.domain().unwrap_or(site).to_owned(),
                    host_only,
                    path: c.path().unwrap_or("/").to_owned(),
                    expires: c.expires_datetime().map(|t| t.unix_timestamp()),
                    secure: c.secure().unwrap_or(false),
                    http_only: c.http_only().unwrap_or(false),
                    size,
                }
            })
            .collect();
        cookies_api::sort_rows(&mut rows);
        rows
    }

    fn cookie_set_blocked(&self, site: &str, on: bool) {
        let Some(url) = control_url(BLOCK_SCHEME, site) else {
            return;
        };
        let cookie = cookie::Cookie::new("block", if on { "1" } else { "0" });
        self.engine
            .servo()
            .site_data_manager()
            .set_cookie_for_url(url, cookie);
    }

    fn cookie_sites_with_cookies(&self) -> Vec<String> {
        self.engine
            .servo()
            .site_data_manager()
            .site_data(StorageType::Cookies)
            .into_iter()
            .map(|s| s.name())
            .collect()
    }

    fn cookies_clear_sites(&self, sites: &[String]) {
        if sites.is_empty() {
            return;
        }
        let refs: Vec<&str> = sites.iter().map(String::as_str).collect();
        self.engine
            .servo()
            .site_data_manager()
            .clear_site_data(&refs, StorageType::Cookies);
    }

    /// Start-up (from `settings_init`): put blocked sites in net's blocked set, and clear what
    /// "clear on exit" should have cleared if the last run did not close cleanly.
    pub(crate) fn cookies_init(&self) {
        let p = self.cookie_policies();
        let blocked = p.blocked();
        for site in &blocked {
            self.cookie_set_blocked(site, true);
        }
        let stale = p.sites_to_clear_on_exit(&self.cookie_sites_with_cookies());
        self.cookies_clear_sites(&stale);
        if !blocked.is_empty() || !stale.is_empty() {
            info!(
                "cookies: {} blocked site(s); cleared {} site(s) left from the last run",
                blocked.len(),
                stale.len()
            );
        }
    }

    /// `shutdown()`: clear the sites set to "Clear on exit" (all but "Allow" with the global
    /// switch on). Servo is still running.
    pub(crate) fn cookies_on_exit(&self) {
        let p = self.cookie_policies();
        if !p.clear_on_exit && !p.sites().any(|(_, m)| m != SitePolicy::Allow) {
            return;
        }
        let sites = p.sites_to_clear_on_exit(&self.cookie_sites_with_cookies());
        self.cookies_clear_sites(&sites);
        info!("cookies: cleared {} site(s) on exit", sites.len());
    }

    /// The active tab's site, when it is an http(s) page.
    fn cookie_active_site(&self) -> Option<(String, String)> {
        let tabs = self.tabs.borrow();
        let tab = tabs.active()?;
        let url = match tab.view.as_ref().and_then(|v| v.url()) {
            Some(u) => u,
            None => Url::parse(&tab.url).ok()?, // a sleeping tab
        };
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let host = url.host_str()?.to_owned();
        Some((site_of_host(&host), host))
    }

    fn cookies_sites_json(&self) -> Value {
        let p = self.cookie_policies();
        let data = self
            .engine
            .servo()
            .site_data_manager()
            .site_data(StorageType::all());
        let mut sites: Vec<Value> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for s in data {
            let name = s.name();
            let t = s.storage_types();
            let rows = if t.contains(StorageType::Cookies) {
                self.cookie_rows(&name)
            } else {
                Vec::new()
            };
            sites.push(json!({
                "site": name.clone(),
                "count": rows.len(),
                "size": rows.iter().map(|r| r.size).sum::<usize>(),
                "storage": t.intersects(StorageType::Local | StorageType::Session),
                "policy": p.get(&name).map(SitePolicy::as_str),
            }));
            seen.insert(name);
        }
        // Sites with a rule but nothing stored (a blocked site keeps nothing).
        for (site, mode) in p.sites() {
            if !seen.contains(site) {
                sites.push(json!({
                    "site": site, "count": 0, "size": 0, "storage": false,
                    "policy": mode.as_str(),
                }));
            }
        }
        sites.sort_by(|a, b| a["site"].as_str().cmp(&b["site"].as_str()));
        json!({
            "sites": sites,
            "clear_on_exit": p.clear_on_exit,
            "persistent": PERSISTENT.load(Ordering::Relaxed),
            "persist_pref": persist_pref(self.prefs_path.as_deref()),
            "third_party": { "supported": false, "note": cookies_api::THIRD_PARTY_NOTE },
        })
    }

    fn cookie_site_json(&self, site: &str) -> Value {
        let rows = self.cookie_rows(site);
        json!({
            "site": site,
            "cookies": rows,
            "count": rows.len(),
            "policy": self.cookie_policies().get(site).map(SitePolicy::as_str),
        })
    }

    pub(crate) fn cookies_api(&self, op: CookieOp) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, json_ok(v));
        let err = |m: &str| (StatusCode::OK, json_error(m));
        match op {
            CookieOp::Sites => ok(self.cookies_sites_json()),
            CookieOp::List { site } => ok(self.cookie_site_json(&site)),
            CookieOp::Delete {
                site,
                name,
                domain,
                path,
            } => {
                let Some(url) = control_url(DELETE_SCHEME, &site) else {
                    return err("This site's cookies cannot be changed here.");
                };
                let cookie = cookie::Cookie::build((name, String::new()))
                    .domain(domain)
                    .path(path)
                    .build();
                self.engine
                    .servo()
                    .site_data_manager()
                    .set_cookie_for_url(url, cookie);
                info!("cookies: deleted one cookie of {site}");
                ok(self.cookie_site_json(&site))
            }
            CookieOp::Policy { site, mode } => {
                let mut p = self.cookie_policies();
                let was_blocked = p.get(&site) == Some(SitePolicy::Block);
                p.set(&site, mode);
                if let Err(e) = self.cookie_policies_save(&p) {
                    warn!("{e}");
                    return err(&e);
                }
                let blocked = mode == Some(SitePolicy::Block);
                if blocked != was_blocked {
                    self.cookie_set_blocked(&site, blocked);
                }
                if blocked {
                    // Net clears them too; this also covers the private-session jar.
                    self.cookies_clear_sites(std::slice::from_ref(&site));
                }
                info!(
                    "cookies: {site} -> {}",
                    mode.map(SitePolicy::as_str).unwrap_or("default")
                );
                ok(self.cookie_site_json(&site))
            }
            CookieOp::PrefsSave {
                clear_on_exit,
                persist,
            } => {
                if let Some(on) = persist {
                    if crate::PRIVATE.load(Ordering::Relaxed) {
                        return err("A private window never keeps cookies.");
                    }
                    self.save_ui_pref(
                        cookies_api::PREF_PERSIST_COOKIES,
                        if on { "1" } else { "0" },
                    );
                    info!("cookies: keep after restart -> {on} (from the next start)");
                }
                if let Some(on) = clear_on_exit {
                    let mut p = self.cookie_policies();
                    p.clear_on_exit = on;
                    if let Err(e) = self.cookie_policies_save(&p) {
                        return err(&e);
                    }
                }
                ok(self.cookies_sites_json())
            }
            CookieOp::ActiveSite => match self.cookie_active_site() {
                None => ok(json!({ "site": Value::Null })),
                Some((site, host)) => {
                    let rows = self.cookie_rows(&site);
                    // What this page's host can see: its own and parent-domain cookies.
                    let here = rows
                        .iter()
                        .filter(|r| {
                            r.domain == host
                                || (!r.host_only && host.ends_with(&format!(".{}", r.domain)))
                        })
                        .count();
                    ok(json!({
                        "site": site,
                        "host": host,
                        "count": rows.len(),
                        "here": here,
                        "policy": self.cookie_policies().get(&site).map(SitePolicy::as_str),
                    }))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_urls() {
        let u = control_url(JAR_SCHEME, "example.co.uk").unwrap();
        assert_eq!(u.scheme(), JAR_SCHEME);
        assert_eq!(u.host_str(), Some("example.co.uk"));
        assert!(control_url(JAR_SCHEME, "::1").is_none());
    }
}
