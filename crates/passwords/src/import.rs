// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Import from Chromium browsers (Google Chrome, Microsoft Edge, Chromium).
//!
//! * **Profiles**: `<user data>/Local State` (`profile.info_cache`: folder -> display name),
//!   else every folder with a `Preferences` file (`Default`, `Profile 1`, ...).
//! * **Bookmarks**: `<profile>/Bookmarks` (JSON; roots `bookmark_bar`, `other`, `synced`).
//! * **History**: `<profile>/History` (SQLite `urls` table). The browser keeps it open, so it
//!   is copied (with its `-wal` / `-journal`) to a private temp folder and read from there.
//! * **Passwords** (Windows only): `<profile>/Login Data` (SQLite `logins`). Values starting
//!   with `v10` / `v11` are AES-256-GCM (`v10` | nonce 12 | ciphertext | tag 16) with the key
//!   in `Local State` `os_crypt.encrypted_key` (base64, `DPAPI` prefix, then a DPAPI blob of the
//!   current Windows user). Values starting with `v20` use Chrome 127+ **app-bound
//!   encryption**: the key is wrapped by Chrome's elevated service and bound to Chrome's own
//!   executable, so no other app can decrypt it (by design); those are counted and the user is
//!   told to export a CSV from the browser instead. Values with no prefix (Chrome < 80) are
//!   plain DPAPI blobs.
//! * On Linux, `v10`/`v11` mean AES-128-CBC with a key from the desktop keyring; MythBrowse
//!   does not read other apps' keyring entries, so passwords come in through CSV only.
//!
//! Times are WebKit timestamps: microseconds since 1601-01-01 UTC.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use mythbrowse_ai::secrets::Secret;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

use crate::csv::CsvLogin;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0} is not installed or has no profile here")]
    NotFound(String),
    #[error("could not read {what}: {why}")]
    Read { what: String, why: String },
    #[error("could not copy {0}; close the browser and try again")]
    Locked(String),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, ImportError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Browser {
    Chrome,
    Edge,
    Chromium,
}

impl Browser {
    pub const ALL: [Browser; 3] = [Browser::Chrome, Browser::Edge, Browser::Chromium];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "chrome" => Some(Self::Chrome),
            "edge" => Some(Self::Edge),
            "chromium" => Some(Self::Chromium),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Chromium => "chromium",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Chrome => "Google Chrome",
            Self::Edge => "Microsoft Edge",
            Self::Chromium => "Chromium",
        }
    }

    /// The browser's "User Data" folder on this OS.
    pub fn user_data_dir(self) -> Option<PathBuf> {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        if cfg!(windows) {
            let local = var("LOCALAPPDATA")?;
            return Some(match self {
                Self::Chrome => local.join("Google").join("Chrome").join("User Data"),
                Self::Edge => local.join("Microsoft").join("Edge").join("User Data"),
                Self::Chromium => local.join("Chromium").join("User Data"),
            });
        }
        if cfg!(target_os = "macos") {
            let support = var("HOME")?.join("Library").join("Application Support");
            return Some(match self {
                Self::Chrome => support.join("Google").join("Chrome"),
                Self::Edge => support.join("Microsoft Edge"),
                Self::Chromium => support.join("Chromium"),
            });
        }
        let config = var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))?;
        Some(match self {
            Self::Chrome => config.join("google-chrome"),
            Self::Edge => config.join("microsoft-edge"),
            Self::Chromium => config.join("chromium"),
        })
    }
}

/// One browser profile that can be imported from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub browser: Browser,
    /// Folder name (`Default`, `Profile 1`): what the import call names.
    pub dir: String,
    /// The name the browser shows ("Person 1", "Work").
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub bookmarks: bool,
    pub history: bool,
    pub passwords: bool,
}

/// What `Local State` says about the profiles and the password key.
#[derive(Debug, Default)]
pub struct LocalState {
    pub profiles: Vec<(String, String)>,
    /// `os_crypt.encrypted_key`, base64-decoded (still DPAPI-wrapped, `DPAPI` prefix kept).
    pub encrypted_key: Option<Vec<u8>>,
    /// `os_crypt.app_bound_encrypted_key` is present (Chrome 127+ app-bound encryption).
    pub app_bound: bool,
}

pub fn parse_local_state(text: &str) -> LocalState {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let mut profiles: Vec<(String, String)> = v
        .pointer("/profile/info_cache")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(dir, info)| {
                    let name = info
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(dir)
                        .to_owned();
                    (dir.clone(), name)
                })
                .collect()
        })
        .unwrap_or_default();
    profiles.sort_by(|a, b| profile_order(&a.0).cmp(&profile_order(&b.0)));
    let encrypted_key = v
        .pointer("/os_crypt/encrypted_key")
        .and_then(Value::as_str)
        .and_then(|s| B64.decode(s).ok());
    let app_bound = v
        .pointer("/os_crypt/app_bound_encrypted_key")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    LocalState {
        profiles,
        encrypted_key,
        app_bound,
    }
}

fn profile_order(dir: &str) -> (u32, String) {
    if dir == "Default" {
        return (0, String::new());
    }
    let n = dir
        .strip_prefix("Profile ")
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX);
    (n.saturating_add(1), dir.to_owned())
}

/// Profiles in a browser's user-data folder (`Local State` first, folder scan as fallback).
pub fn profiles_in(user_data: &Path, browser: Browser) -> Vec<Profile> {
    let state = std::fs::read_to_string(user_data.join("Local State"))
        .map(|t| parse_local_state(&t))
        .unwrap_or_default();
    let mut dirs = state.profiles;
    if dirs.is_empty()
        && let Ok(rd) = std::fs::read_dir(user_data)
    {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().join("Preferences").is_file() {
                dirs.push((name.clone(), name));
            }
        }
        dirs.sort_by(|a, b| profile_order(&a.0).cmp(&profile_order(&b.0)));
    }
    dirs.into_iter()
        .filter_map(|(dir, name)| {
            let path = user_data.join(&dir);
            if !path.is_dir() {
                return None;
            }
            Some(Profile {
                browser,
                bookmarks: path.join("Bookmarks").is_file(),
                history: path.join("History").is_file(),
                passwords: path.join("Login Data").is_file(),
                dir,
                name,
                path,
            })
        })
        .collect()
}

/// Every importable profile of every supported browser on this computer.
pub fn find_profiles() -> Vec<Profile> {
    Browser::ALL
        .iter()
        .filter_map(|b| b.user_data_dir().map(|d| profiles_in(&d, *b)))
        .flatten()
        .filter(|p| p.bookmarks || p.history || p.passwords)
        .collect()
}

/// WebKit time (µs since 1601) to Unix ms; 0 stays 0.
pub fn webkit_to_unix_ms(t: i64) -> i64 {
    if t <= 0 {
        return 0;
    }
    t / 1000 - 11_644_473_600_000
}

// --------------------------------------------------------------------------- bookmarks

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedBookmark {
    pub url: String,
    pub title: String,
    /// Folder path, e.g. `Bookmarks bar/News`.
    pub folder: String,
    pub added_ms: i64,
}

pub fn parse_bookmarks(text: &str) -> Result<Vec<ImportedBookmark>> {
    let v: Value = serde_json::from_str(text).map_err(|e| ImportError::Read {
        what: "Bookmarks".into(),
        why: e.to_string(),
    })?;
    let roots = v
        .get("roots")
        .and_then(Value::as_object)
        .ok_or_else(|| ImportError::Read {
            what: "Bookmarks".into(),
            why: "no roots".into(),
        })?;
    let mut out = Vec::new();
    for key in ["bookmark_bar", "other", "synced"] {
        if let Some(node) = roots.get(key) {
            let name = node.get("name").and_then(Value::as_str).unwrap_or(key);
            walk(node, name, &mut out);
        }
    }
    Ok(out)
}

fn walk(node: &Value, folder: &str, out: &mut Vec<ImportedBookmark>) {
    let Some(children) = node.get("children").and_then(Value::as_array) else {
        return;
    };
    for c in children {
        let name = c.get("name").and_then(Value::as_str).unwrap_or("");
        match c.get("type").and_then(Value::as_str) {
            Some("url") => {
                let url = c.get("url").and_then(Value::as_str).unwrap_or("");
                if url.starts_with("http://") || url.starts_with("https://") {
                    let added = c
                        .get("date_added")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse::<i64>().ok())
                        .map(webkit_to_unix_ms)
                        .unwrap_or(0);
                    out.push(ImportedBookmark {
                        url: url.to_owned(),
                        title: name.to_owned(),
                        folder: folder.to_owned(),
                        added_ms: added,
                    });
                }
            }
            Some("folder") => walk(c, &format!("{folder}/{name}"), out),
            _ => {}
        }
    }
}

// --------------------------------------------------------------------------- SQLite copies

/// A private copy of a database the browser may hold open; deleted on drop.
pub struct DbCopy {
    dir: PathBuf,
    pub path: PathBuf,
}

impl DbCopy {
    pub fn new(src: &Path) -> Result<Self> {
        let mut rnd = [0u8; 8];
        let _ = getrandom::getrandom(&mut rnd);
        let tag: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("mythbrowse-import-{tag}"));
        std::fs::create_dir_all(&dir).map_err(|e| ImportError::Read {
            what: dir.display().to_string(),
            why: e.to_string(),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "db".into());
        let path = dir.join(&name);
        let copy = Self { dir, path };
        std::fs::copy(src, &copy.path).map_err(|_| ImportError::Locked(name.clone()))?;
        for ext in ["-wal", "-journal"] {
            let side = PathBuf::from(format!("{}{ext}", src.display()));
            if side.is_file() {
                let _ = std::fs::copy(&side, format!("{}{ext}", copy.path.display()));
            }
        }
        Ok(copy)
    }

    fn open(&self) -> Result<Connection> {
        // Read-write on the private copy so SQLite can replay a copied WAL.
        Ok(Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?)
    }
}

impl Drop for DbCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// --------------------------------------------------------------------------- history

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedVisit {
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    pub last_visit_ms: i64,
}

/// The newest `limit` history entries of a Chromium `History` database (a copy is read).
pub fn read_history(history_db: &Path, limit: usize) -> Result<Vec<ImportedVisit>> {
    let copy = DbCopy::new(history_db)?;
    let conn = copy.open()?;
    let mut st = conn.prepare(
        "SELECT url, title, visit_count, last_visit_time FROM urls
         WHERE hidden = 0 AND (url LIKE 'http://%' OR url LIKE 'https://%')
         ORDER BY last_visit_time DESC LIMIT ?1",
    )?;
    let rows = st
        .query_map([limit as i64], |r| {
            Ok(ImportedVisit {
                url: r.get(0)?,
                title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                visit_count: r.get::<_, Option<i64>>(2)?.unwrap_or(1).max(1),
                last_visit_ms: webkit_to_unix_ms(r.get::<_, Option<i64>>(3)?.unwrap_or(0)),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// --------------------------------------------------------------------------- passwords

/// One row of `Login Data` (`logins` table), password still encrypted.
#[derive(Clone)]
pub struct RawLogin {
    pub origin_url: String,
    pub action_url: String,
    pub username: String,
    pub password_value: Vec<u8>,
    /// The user chose "Never" for this site in the browser.
    pub never: bool,
}

impl Drop for RawLogin {
    fn drop(&mut self) {
        self.password_value.zeroize();
    }
}

pub fn read_logins(login_db: &Path) -> Result<Vec<RawLogin>> {
    let copy = DbCopy::new(login_db)?;
    let conn = copy.open()?;
    let mut st = conn.prepare(
        "SELECT origin_url, action_url, username_value, password_value, blacklisted_by_user
         FROM logins",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(RawLogin {
                origin_url: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                action_url: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                username: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                password_value: r.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                never: r.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// How a `password_value` is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cipher {
    /// `v10` / `v11`: AES-256-GCM with the `Local State` key (Windows).
    V10,
    /// `v20`: Chrome 127+ app-bound encryption; not decryptable by other apps.
    AppBound,
    /// No version prefix: a raw DPAPI blob (Chrome < 80 on Windows).
    LegacyDpapi,
    Empty,
}

pub fn classify(blob: &[u8]) -> Cipher {
    if blob.is_empty() {
        Cipher::Empty
    } else if blob.starts_with(b"v10") || blob.starts_with(b"v11") {
        Cipher::V10
    } else if blob.starts_with(b"v20") {
        Cipher::AppBound
    } else {
        Cipher::LegacyDpapi
    }
}

/// The DPAPI blob inside `os_crypt.encrypted_key` (after the `DPAPI` prefix).
pub fn dpapi_part(encrypted_key: &[u8]) -> Option<&[u8]> {
    encrypted_key.strip_prefix(b"DPAPI")
}

/// Decrypt a `v10` / `v11` value with the 32-byte `Local State` key.
pub fn decrypt_v10(key: &[u8; 32], blob: &[u8]) -> Option<Secret> {
    let rest = blob.get(3..)?;
    if rest.len() < 12 + 16 {
        return None;
    }
    let (nonce, ct) = rest.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = Zeroizing::new(cipher.decrypt(Nonce::from_slice(nonce), ct).ok()?);
    std::str::from_utf8(&plain).ok().map(Secret::new)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PasswordReport {
    pub read: usize,
    /// Chrome 127+ app-bound (`v20`) values: need a CSV export from the browser.
    pub app_bound: usize,
    pub failed: usize,
    /// "Never save" sites carried over.
    pub never: usize,
}

/// Unwraps a raw DPAPI blob (legacy, pre-v10 Chrome passwords; Windows only).
pub type LegacyUnwrap = dyn Fn(&[u8]) -> Option<Vec<u8>>;

/// Decrypt the rows of `Login Data`. `key`: the unwrapped `Local State` key (if it could be
/// unwrapped). `legacy`: unwraps a raw DPAPI blob (Windows), `None` elsewhere.
pub fn decrypt_logins(
    rows: &[RawLogin],
    key: Option<&[u8; 32]>,
    legacy: Option<&LegacyUnwrap>,
) -> (Vec<CsvLogin>, Vec<String>, PasswordReport) {
    let mut out = Vec::new();
    let mut never = Vec::new();
    let mut rep = PasswordReport::default();
    for r in rows {
        if r.never {
            never.push(r.origin_url.clone());
            rep.never += 1;
            continue;
        }
        let pw = match classify(&r.password_value) {
            Cipher::Empty => continue,
            Cipher::AppBound => {
                rep.app_bound += 1;
                continue;
            }
            Cipher::V10 => key.and_then(|k| decrypt_v10(k, &r.password_value)),
            Cipher::LegacyDpapi => legacy.and_then(|f| f(&r.password_value)).and_then(|mut v| {
                let s = std::str::from_utf8(&v).ok().map(Secret::new);
                v.zeroize();
                s
            }),
        };
        match pw {
            Some(password) => {
                rep.read += 1;
                out.push(CsvLogin {
                    name: String::new(),
                    url: r.origin_url.clone(),
                    username: r.username.clone(),
                    password,
                    note: String::new(),
                });
            }
            None => rep.failed += 1,
        }
    }
    (out, never, rep)
}

/// Windows: unwrap the `Local State` key with DPAPI (current user).
#[cfg(windows)]
pub fn unwrap_local_state_key(state: &LocalState) -> Option<Zeroizing<[u8; 32]>> {
    let blob = dpapi_part(state.encrypted_key.as_deref()?)?;
    let mut raw = mythbrowse_ai::secrets::dpapi_unprotect_user(blob).ok()?;
    let key = (raw.len() == 32).then(|| {
        let mut k = Zeroizing::new([0u8; 32]);
        k.copy_from_slice(&raw);
        k
    });
    raw.zeroize();
    key
}

/// Linux / macOS: Chromium's password key is in the desktop keyring; not read (CSV only).
#[cfg(not(windows))]
pub fn unwrap_local_state_key(_state: &LocalState) -> Option<Zeroizing<[u8; 32]>> {
    None
}

/// Whether this build can read another browser's saved passwords directly.
pub const DIRECT_PASSWORDS: bool = cfg!(windows);

/// Read and decrypt a profile's saved passwords (Windows). Elsewhere returns an empty list.
pub fn profile_passwords(
    profile: &Profile,
) -> Result<(Vec<CsvLogin>, Vec<String>, PasswordReport)> {
    if !DIRECT_PASSWORDS || !profile.passwords {
        return Ok((Vec::new(), Vec::new(), PasswordReport::default()));
    }
    let user_data = profile.path.parent().unwrap_or(&profile.path);
    let state = std::fs::read_to_string(user_data.join("Local State"))
        .map(|t| parse_local_state(&t))
        .unwrap_or_default();
    let key = unwrap_local_state_key(&state);
    let rows = read_logins(&profile.path.join("Login Data"))?;
    #[cfg(windows)]
    let legacy_fn = |b: &[u8]| mythbrowse_ai::secrets::dpapi_unprotect_user(b).ok();
    #[cfg(windows)]
    let legacy: Option<&LegacyUnwrap> = Some(&legacy_fn);
    #[cfg(not(windows))]
    let legacy: Option<&LegacyUnwrap> = None;
    Ok(decrypt_logins(&rows, key.as_deref(), legacy))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_BOOKMARKS: &str = r#"{
      "checksum": "x", "version": 1,
      "roots": {
        "bookmark_bar": { "name": "Bookmarks bar", "type": "folder", "children": [
          { "type": "url", "name": "Example", "url": "https://example.com/", "date_added": "13370000000000000" },
          { "type": "folder", "name": "News", "children": [
            { "type": "url", "name": "Star", "url": "https://www.thestar.com.my/", "date_added": "0" },
            { "type": "url", "name": "Script", "url": "javascript:alert(1)" }
          ]}
        ]},
        "other": { "name": "Other bookmarks", "type": "folder", "children": [
          { "type": "url", "name": "Rust", "url": "https://www.rust-lang.org/" }
        ]},
        "synced": { "name": "Mobile bookmarks", "type": "folder", "children": [] }
      }
    }"#;

    #[test]
    fn bookmarks_fixture() {
        let b = parse_bookmarks(FIXTURE_BOOKMARKS).unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].url, "https://example.com/");
        assert_eq!(b[0].folder, "Bookmarks bar");
        // 13370000000000000 µs since 1601 = 1725526400000 ms since 1970.
        assert_eq!(b[0].added_ms, 1_725_526_400_000);
        assert_eq!(b[1].folder, "Bookmarks bar/News");
        assert_eq!(b[1].added_ms, 0);
        assert_eq!(b[2].folder, "Other bookmarks");
        assert!(parse_bookmarks("{}").is_err());
        assert!(parse_bookmarks("nope").is_err());
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mb-import-test-{tag}-{}-{}",
            std::process::id(),
            crate::store::now_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn history_fixture_read_from_a_copy() {
        let d = temp_dir("hist");
        let db = d.join("History");
        {
            let c = Connection::open(&db).unwrap();
            let _: String = c
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                .unwrap();
            c.execute_batch(
                "CREATE TABLE urls (id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR,
                   visit_count INTEGER DEFAULT 0 NOT NULL, typed_count INTEGER DEFAULT 0 NOT NULL,
                   last_visit_time INTEGER NOT NULL, hidden INTEGER DEFAULT 0 NOT NULL);
                 INSERT INTO urls (url, title, visit_count, last_visit_time, hidden) VALUES
                   ('https://example.com/', 'Example', 3, 13370000000000000, 0),
                   ('https://new.example/', NULL, 0, 13380000000000000, 0),
                   ('https://hidden.example/', 'H', 1, 13390000000000000, 1),
                   ('chrome://settings/', 'Settings', 9, 13390000000000000, 0);",
            )
            .unwrap();
            // Keep this connection open (like a running browser) while importing.
            let v = read_history(&db, 100).unwrap();
            assert_eq!(v.len(), 2);
            assert_eq!(v[0].url, "https://new.example/");
            assert_eq!(v[0].title, "");
            assert_eq!(v[0].visit_count, 1);
            assert_eq!(v[1].last_visit_ms, 1_725_526_400_000);
            assert_eq!(read_history(&db, 1).unwrap().len(), 1);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Encrypt like Chrome on Windows does: `v10` | nonce | AES-256-GCM(ciphertext | tag).
    fn chrome_v10(key: &[u8; 32], plain: &str) -> Vec<u8> {
        let nonce = [7u8; 12];
        let ct = Aes256Gcm::new_from_slice(key)
            .unwrap()
            .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
            .unwrap();
        let mut v = b"v10".to_vec();
        v.extend_from_slice(&nonce);
        v.extend_from_slice(&ct);
        v
    }

    #[test]
    fn login_data_fixture_v10_v20_legacy_and_never() {
        let key = [0x42u8; 32];
        let d = temp_dir("logins");
        let db = d.join("Login Data");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE logins (origin_url VARCHAR NOT NULL, action_url VARCHAR,
                   username_element VARCHAR, username_value VARCHAR, password_element VARCHAR,
                   password_value BLOB, signon_realm VARCHAR NOT NULL, date_created INTEGER,
                   blacklisted_by_user INTEGER NOT NULL DEFAULT 0);",
            )
            .unwrap();
            let ins = |o: &str, u: &str, p: &[u8], never: i64| {
                c.execute(
                    "INSERT INTO logins (origin_url, action_url, username_value, password_value,
                       signon_realm, blacklisted_by_user) VALUES (?1, ?1, ?2, ?3, ?1, ?4)",
                    rusqlite::params![o, u, p, never],
                )
                .unwrap();
            };
            ins(
                "https://example.com/login",
                "alice",
                &chrome_v10(&key, "p\u{e4}ss w0rd"),
                0,
            );
            ins(
                "https://bank.example/",
                "bob",
                b"v20\x01\x02\x03app-bound-wrapped-bytes-here",
                0,
            );
            ins(
                "https://old.example/",
                "carol",
                b"\x01\x00\x00\x00\xd0\x8c\x9d\xdflegacy-dpapi",
                0,
            );
            ins("https://never.example/", "", b"", 1);
            ins(
                "https://tampered.example/",
                "dan",
                &{
                    let mut v = chrome_v10(&key, "x");
                    let n = v.len();
                    v[n - 1] ^= 0xff;
                    v
                },
                0,
            );
        }
        let rows = read_logins(&db).unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(classify(&rows[0].password_value), Cipher::V10);
        assert_eq!(classify(&rows[1].password_value), Cipher::AppBound);
        assert_eq!(classify(&rows[2].password_value), Cipher::LegacyDpapi);

        // Legacy DPAPI stand-in: strips a fake header.
        let legacy = |b: &[u8]| {
            b.strip_prefix(b"\x01\x00\x00\x00\xd0\x8c\x9d\xdf")
                .map(<[u8]>::to_vec)
        };
        let (logins, never, rep) = decrypt_logins(&rows, Some(&key), Some(&legacy));
        assert_eq!(
            rep,
            PasswordReport {
                read: 2,
                app_bound: 1,
                failed: 1,
                never: 1
            }
        );
        assert_eq!(logins[0].url, "https://example.com/login");
        assert_eq!(logins[0].username, "alice");
        assert_eq!(logins[0].password.expose(), "p\u{e4}ss w0rd");
        assert_eq!(logins[1].password.expose(), "legacy-dpapi");
        assert_eq!(never, vec!["https://never.example/".to_owned()]);

        // Wrong key / no key: nothing decrypts, nothing panics.
        let (l, _, rep) = decrypt_logins(&rows, Some(&[0u8; 32]), None);
        assert!(l.is_empty());
        assert_eq!((rep.failed, rep.app_bound), (3, 1));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn local_state_key_and_app_bound_detection() {
        let wrapped = [b"DPAPI".as_slice(), b"\x01\x00\x00\x00blob"].concat();
        let text = format!(
            r#"{{"os_crypt":{{"encrypted_key":"{}","app_bound_encrypted_key":"QVBQQgE="}},
               "profile":{{"info_cache":{{"Profile 2":{{"name":"Work"}},"Default":{{"name":"Person 1"}},"Profile 10":{{}}}}}}}}"#,
            B64.encode(&wrapped)
        );
        let s = parse_local_state(&text);
        assert!(s.app_bound);
        assert_eq!(
            dpapi_part(s.encrypted_key.as_deref().unwrap()),
            Some(&b"\x01\x00\x00\x00blob"[..])
        );
        assert_eq!(
            s.profiles,
            vec![
                ("Default".into(), "Person 1".into()),
                ("Profile 2".into(), "Work".into()),
                ("Profile 10".into(), "Profile 10".into())
            ]
        );
        let old = parse_local_state(r#"{"os_crypt":{"encrypted_key":"RFBBUEl4"}}"#);
        assert!(!old.app_bound);
        assert!(dpapi_part(b"nope").is_none());
        let none = parse_local_state("garbage");
        assert!(none.encrypted_key.is_none() && none.profiles.is_empty());
    }

    #[test]
    fn profile_picker_scans_user_data() {
        let d = temp_dir("profiles");
        for p in ["Default", "Profile 1", "System Profile"] {
            std::fs::create_dir_all(d.join(p)).unwrap();
        }
        std::fs::write(d.join("Default").join("Bookmarks"), FIXTURE_BOOKMARKS).unwrap();
        std::fs::write(d.join("Default").join("Preferences"), "{}").unwrap();
        std::fs::write(d.join("Profile 1").join("Preferences"), "{}").unwrap();
        std::fs::write(d.join("Profile 1").join("History"), "").unwrap();
        // No Local State: folder scan (folders with Preferences only).
        let ps = profiles_in(&d, Browser::Chrome);
        assert_eq!(
            ps.iter().map(|p| p.dir.as_str()).collect::<Vec<_>>(),
            ["Default", "Profile 1"]
        );
        assert!(ps[0].bookmarks && !ps[0].history);
        assert!(ps[1].history);
        // With Local State: its names.
        std::fs::write(
            d.join("Local State"),
            r#"{"profile":{"info_cache":{"Default":{"name":"Me"},"Profile 1":{"name":"School"}}}}"#,
        )
        .unwrap();
        let ps = profiles_in(&d, Browser::Edge);
        assert_eq!(ps[1].name, "School");
        assert_eq!(ps[0].browser, Browser::Edge);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn webkit_time() {
        assert_eq!(webkit_to_unix_ms(0), 0);
        assert_eq!(webkit_to_unix_ms(11_644_473_600_000_000), 0);
    }
}
