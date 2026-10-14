// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Account + E2E sync in the shell: owns the [`SyncService`] thread (its own SQLite handle on
//! the profile, the Appwrite client) and answers the library page's `/api/sync_*` RPCs.
//! Passwords, passphrases and recovery codes arrive in the `x-mb-secret` header as a form and
//! go straight to the worker; they are never logged or written anywhere.
//!
//! Only built with the `cloud_sync` cargo feature (off by default, so the Microsoft Store
//! preview has none of it): without it [`SyncHost::new`] starts no thread, makes no network
//! call and touches no secret, and the `/api/sync_*` RPCs do not exist (`chrome::Features`).

use std::path::PathBuf;
use std::sync::Arc;

use http::StatusCode;
use mythbrowse_ai::Secret;
use mythbrowse_engine::chrome::{self, SyncOp};
use mythbrowse_sync::service::{Command, SyncService};
use serde_json::json;
use zeroize::Zeroizing;

#[cfg(feature = "cloud_sync")]
use mythbrowse_ai::{PlatformStore, SecretStore};
#[cfg(feature = "cloud_sync")]
use mythbrowse_sync::service::KeyStore;

/// The OS secret store (DPAPI on Windows, Secret Service / encrypted files on Linux).
#[cfg(feature = "cloud_sync")]
struct OsKeys(PlatformStore);

#[cfg(feature = "cloud_sync")]
impl KeyStore for OsKeys {
    fn get(&self, name: &str) -> Option<Zeroizing<String>> {
        self.0
            .get(name)
            .ok()
            .flatten()
            .map(|s| Zeroizing::new(s.expose().to_owned()))
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.0
            .set(name, &Secret::new(value))
            .map_err(|e| e.to_string())
    }
    fn delete(&self, name: &str) {
        let _ = self.0.delete(name);
    }
}

pub struct SyncHost {
    service: Option<SyncService>,
}

impl SyncHost {
    /// Without the `cloud_sync` feature: no service at all.
    #[cfg(not(feature = "cloud_sync"))]
    pub fn new(_profile_dir: Option<PathBuf>, _wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        log::info!("cloud sync is not part of this build");
        SyncHost { service: None }
    }

    /// `wake` is called (from the sync thread) whenever the status changes.
    #[cfg(feature = "cloud_sync")]
    pub fn new(profile_dir: Option<PathBuf>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        use mythbrowse_storage::Store;
        use mythbrowse_sync::KdfParams;
        use mythbrowse_sync::appwrite::{Appwrite, AppwriteConfig};
        use std::path::Path;

        let Some(dir) = profile_dir else {
            log::warn!("sync unavailable: no profile directory");
            return SyncHost { service: None };
        };
        let store_dir: PathBuf = dir.clone();
        let service = SyncService::spawn(
            Appwrite::new(AppwriteConfig::default()),
            move || Store::open_in_dir(&store_dir).map_err(|e| e.to_string()),
            Box::new(OsKeys(PlatformStore::new(Path::new(&dir)))),
            KdfParams::default(),
            Arc::new(move |_changed| wake()),
        );
        SyncHost {
            service: Some(service),
        }
    }

    pub fn api(&self, op: SyncOp, secret: Option<Secret>) -> (StatusCode, String) {
        let Some(svc) = &self.service else {
            let why = if cfg!(feature = "cloud_sync") {
                "sync is unavailable (no profile directory)"
            } else {
                "sync is not part of this build"
            };
            return (StatusCode::SERVICE_UNAVAILABLE, chrome::json_error(why));
        };
        // `password=…&passphrase=…&code=…`, percent-encoded by the page.
        let field = |name: &str| -> Option<Zeroizing<String>> {
            let s = secret.as_ref()?;
            url::form_urlencoded::parse(s.expose().as_bytes())
                .find(|(k, _)| k == name)
                .map(|(_, v)| Zeroizing::new(v.into_owned()))
                .filter(|v| !v.is_empty())
        };
        let missing = |what: &str| {
            (
                StatusCode::BAD_REQUEST,
                chrome::json_error(&format!("missing {what}")),
            )
        };
        let cmd = match op {
            SyncOp::Status => None,
            SyncOp::SignUp { email } => match field("password") {
                Some(password) => Some(Command::SignUp { email, password }),
                None => return missing("password"),
            },
            SyncOp::SignIn { email } => match field("password") {
                Some(password) => Some(Command::SignIn { email, password }),
                None => return missing("password"),
            },
            SyncOp::SetUp => match field("passphrase") {
                Some(passphrase) => Some(Command::SetUp { passphrase }),
                None => return missing("passphrase"),
            },
            SyncOp::Unlock => match field("passphrase") {
                Some(passphrase) => Some(Command::Unlock { passphrase }),
                None => return missing("passphrase"),
            },
            SyncOp::Recover => match (field("code"), field("passphrase")) {
                (Some(code), Some(passphrase)) => Some(Command::Recover { code, passphrase }),
                _ => return missing("recovery code or passphrase"),
            },
            SyncOp::Now => Some(Command::SyncNow),
            SyncOp::Ack => Some(Command::AckRecovery),
            SyncOp::SignOut => Some(Command::SignOut),
        };
        if let Some(c) = cmd {
            svc.send(c);
        }
        (
            StatusCode::OK,
            chrome::json_ok(json!({ "sync": svc.status() })),
        )
    }
}
