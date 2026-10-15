// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Local secret storage for API keys. Keys never go into `ai.toml`, logs or the UI.
//!
//! * Windows: the key is encrypted with DPAPI (`CryptProtectData`, current user scope) and the
//!   blob is kept in `<profile>/secrets/<name>.dpapi`. Only the same Windows user on the same
//!   machine can decrypt it.
//! * Linux and others: the Secret Service (GNOME Keyring / KWallet) through `secret-tool`
//!   when it is installed; otherwise `<profile>/secrets/<name>.enc`, ChaCha20-Poly1305 with a
//!   random key in `<profile>/secrets/.key` (both 0600). The file fallback keeps keys out of
//!   plain text (backups, sync, screen shares), not away from other programs of the same user.
//!
//! [`Secret`] wraps the value: `Debug`/`Display` print `[redacted]` and memory is zeroed on drop.

use std::path::{Path, PathBuf};

use zeroize::Zeroize;

/// A secret string. Never printed; zeroed when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Only for putting it on the wire (Authorization header).
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Replace every occurrence of `secret` in `text` (error bodies can echo headers back).
pub fn redact(text: &str, secret: Option<&Secret>) -> String {
    match secret {
        Some(s) if s.expose().len() >= 4 => text.replace(s.expose(), "[redacted]"),
        _ => text.to_owned(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("invalid secret name `{0}`")]
    BadName(String),
    #[error("secret storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("secret storage error: {0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError>;
    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError>;
    fn delete(&self, name: &str) -> Result<(), SecretError>;
    /// Human-readable description of where secrets live ("Windows DPAPI", ...).
    fn backend(&self) -> String;
}

/// Secret names become file names: `[a-z0-9_.-]`, 1..=64 chars.
fn check_name(name: &str) -> Result<(), SecretError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(SecretError::BadName(name.to_owned()))
    }
}

/// In-memory store (tests, or when no profile directory is available).
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<std::collections::BTreeMap<String, Secret>>);

impl SecretStore for MemoryStore {
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        check_name(name)?;
        Ok(self.0.lock().expect("lock").get(name).cloned())
    }
    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        check_name(name)?;
        self.0
            .lock()
            .expect("lock")
            .insert(name.to_owned(), value.clone());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        check_name(name)?;
        self.0.lock().expect("lock").remove(name);
        Ok(())
    }
    fn backend(&self) -> String {
        "memory (not saved)".to_owned()
    }
}

/// The platform store described in the module docs.
pub struct PlatformStore {
    dir: PathBuf,
    #[cfg(not(windows))]
    use_secret_tool: bool,
}

impl PlatformStore {
    /// `profile_dir/secrets` holds the encrypted files.
    pub fn new(profile_dir: &Path) -> Self {
        Self {
            dir: profile_dir.join("secrets"),
            #[cfg(not(windows))]
            use_secret_tool: std::env::var_os("MYTHBROWSE_NO_SECRET_SERVICE").is_none()
                && secret_tool::available(),
        }
    }

    /// File-only variant (no Secret Service), for tests.
    #[cfg(not(windows))]
    pub fn files_only(profile_dir: &Path) -> Self {
        Self {
            dir: profile_dir.join("secrets"),
            use_secret_tool: false,
        }
    }

    fn ensure_dir(&self) -> Result<(), SecretError> {
        std::fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

fn write_private(path: &Path, data: &[u8]) -> Result<(), SecretError> {
    let tmp = path.with_extension("tmp");
    {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_opt(path: &Path) -> Result<Option<Vec<u8>>, SecretError> {
    match std::fs::read(path) {
        Ok(d) => Ok(Some(d)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn remove_opt(path: &Path) -> Result<(), SecretError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn to_secret(mut bytes: Vec<u8>) -> Result<Secret, SecretError> {
    let s = String::from_utf8(std::mem::take(&mut bytes))
        .map_err(|_| SecretError::Backend("stored secret is not UTF-8".into()))?;
    Ok(Secret(s))
}

// ------------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod dpapi {
    use super::SecretError;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    /// Extra entropy so other apps using DPAPI for the same user cannot trivially decrypt.
    /// Differs between the open build and official builds, so neither can open the other's keys
    /// (the vault key and the sealed cookie-jar key live in this store).
    const ENTROPY: &[u8] = if cfg!(feature = "official") {
        b"MythBrowse/secrets/v1"
    } else {
        b"MythBrowse Open Build/secrets/v1"
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if out.pbData.is_null() {
            return Vec::new();
        }
        // SAFETY: DPAPI returned a LocalAlloc'd buffer of cbData bytes; copied, then freed.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as _);
            v
        }
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, SecretError> {
        let input = blob(plain);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: all pointers are valid for the duration of the call.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(SecretError::Backend(format!(
                "CryptProtectData failed ({code})"
            )));
        }
        Ok(take(out))
    }

    pub fn unprotect(cipher: &[u8]) -> Result<Vec<u8>, SecretError> {
        unprotect_with(cipher, Some(ENTROPY))
    }

    /// `entropy`: `None` for blobs other programs made (Chrome's `Local State` key).
    pub fn unprotect_with(cipher: &[u8], entropy: Option<&[u8]>) -> Result<Vec<u8>, SecretError> {
        let input = blob(cipher);
        let entropy = entropy.map(blob);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as above.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                entropy.as_ref().map_or(std::ptr::null(), |e| e as *const _),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(SecretError::Backend(format!(
                "CryptUnprotectData failed ({code})"
            )));
        }
        Ok(take(out))
    }
}

/// Decrypt a DPAPI blob of the current Windows user made by another program without extra
/// entropy (the browser import reads Chrome / Edge's `os_crypt` key with it). Same user, same
/// machine only; the plaintext is the caller's to zero.
#[cfg(windows)]
pub fn dpapi_unprotect_user(blob: &[u8]) -> Result<Vec<u8>, SecretError> {
    dpapi::unprotect_with(blob, None)
}

#[cfg(windows)]
impl SecretStore for PlatformStore {
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        check_name(name)?;
        match read_opt(&self.dir.join(format!("{name}.dpapi")))? {
            Some(blob) => Ok(Some(to_secret(dpapi::unprotect(&blob)?)?)),
            None => Ok(None),
        }
    }
    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        check_name(name)?;
        self.ensure_dir()?;
        let blob = dpapi::protect(value.expose().as_bytes())?;
        write_private(&self.dir.join(format!("{name}.dpapi")), &blob)
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        check_name(name)?;
        remove_opt(&self.dir.join(format!("{name}.dpapi")))
    }
    fn backend(&self) -> String {
        "Windows (DPAPI, this user only)".to_owned()
    }
}

// ------------------------------------------------------------------------- Linux & others

#[cfg(not(windows))]
mod secret_tool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use super::{Secret, SecretError};

    /// Secret Service `service` attribute: separate for the open build and official builds.
    const SERVICE: &str = if cfg!(feature = "official") {
        "mythbrowse"
    } else {
        "mythbrowse-open"
    };
    const LABEL: &str = if cfg!(feature = "official") {
        "MythBrowse"
    } else {
        "MythBrowse (Open Build)"
    };

    pub fn available() -> bool {
        Command::new("secret-tool")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    pub fn store(name: &str, value: &Secret) -> Result<(), SecretError> {
        let mut child = Command::new("secret-tool")
            .args(["store", "--label", &format!("{LABEL}: {name}")])
            .args(["service", SERVICE, "account", name])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(value.expose().as_bytes())?;
        }
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(SecretError::Backend("secret-tool store failed".into()))
        }
    }

    pub fn lookup(name: &str) -> Option<Secret> {
        let out = Command::new("secret-tool")
            .args(["lookup", "service", SERVICE, "account", name])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() || out.stdout.is_empty() {
            return None;
        }
        String::from_utf8(out.stdout).ok().map(Secret::new)
    }

    pub fn clear(name: &str) {
        let _ = Command::new("secret-tool")
            .args(["clear", "service", SERVICE, "account", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(not(windows))]
impl PlatformStore {
    fn file_key(&self) -> Result<chacha20poly1305::Key, SecretError> {
        let path = self.dir.join(".key");
        if let Some(k) = read_opt(&path)?
            && k.len() == 32
        {
            return Ok(*chacha20poly1305::Key::from_slice(&k));
        }
        self.ensure_dir()?;
        let mut k = [0u8; 32];
        getrandom::getrandom(&mut k).map_err(|e| SecretError::Backend(e.to_string()))?;
        write_private(&path, &k)?;
        let key = *chacha20poly1305::Key::from_slice(&k);
        k.zeroize();
        Ok(key)
    }

    fn file_get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        use chacha20poly1305::aead::{Aead, KeyInit};
        let Some(data) = read_opt(&self.dir.join(format!("{name}.enc")))? else {
            return Ok(None);
        };
        if data.len() < 12 + 16 {
            return Err(SecretError::Backend("stored secret is truncated".into()));
        }
        let cipher = chacha20poly1305::ChaCha20Poly1305::new(&self.file_key()?);
        let (nonce, body) = data.split_at(12);
        let plain = cipher
            .decrypt(chacha20poly1305::Nonce::from_slice(nonce), body)
            .map_err(|_| SecretError::Backend("stored secret could not be decrypted".into()))?;
        Ok(Some(to_secret(plain)?))
    }

    fn file_set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        use chacha20poly1305::aead::{Aead, KeyInit};
        self.ensure_dir()?;
        let cipher = chacha20poly1305::ChaCha20Poly1305::new(&self.file_key()?);
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|e| SecretError::Backend(e.to_string()))?;
        let body = cipher
            .encrypt(
                chacha20poly1305::Nonce::from_slice(&nonce),
                value.expose().as_bytes(),
            )
            .map_err(|_| SecretError::Backend("encryption failed".into()))?;
        let mut data = nonce.to_vec();
        data.extend_from_slice(&body);
        write_private(&self.dir.join(format!("{name}.enc")), &data)
    }
}

#[cfg(not(windows))]
impl SecretStore for PlatformStore {
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        check_name(name)?;
        if self.use_secret_tool
            && let Some(s) = secret_tool::lookup(name)
        {
            return Ok(Some(s));
        }
        self.file_get(name)
    }
    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        check_name(name)?;
        if self.use_secret_tool && secret_tool::store(name, value).is_ok() {
            // Do not leave an older file copy behind.
            remove_opt(&self.dir.join(format!("{name}.enc")))?;
            return Ok(());
        }
        self.file_set(name, value)
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        check_name(name)?;
        if self.use_secret_tool {
            secret_tool::clear(name);
        }
        remove_opt(&self.dir.join(format!("{name}.enc")))
    }
    fn backend(&self) -> String {
        if self.use_secret_tool {
            "Secret Service (system keyring)".to_owned()
        } else {
            "encrypted file in the profile folder".to_owned()
        }
    }
}

/// Store, read back and delete a dummy secret (`mythbrowse --keystore-selftest`, CI).
pub fn self_test(store: &dyn SecretStore) -> Result<String, String> {
    let name = "selftest";
    let value = Secret::new(format!("selftest-{}", std::process::id()));
    store.set(name, &value).map_err(|e| e.to_string())?;
    let back = store.get(name).map_err(|e| e.to_string())?;
    store.delete(name).map_err(|e| e.to_string())?;
    let gone = store.get(name).map_err(|e| e.to_string())?.is_none();
    match back {
        Some(b) if b == value && gone => Ok(store.backend()),
        Some(_) if !gone => Err("secret was not deleted".into()),
        _ => Err("secret did not round-trip".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mb-secrets-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn secret_never_prints() {
        let s = Secret::new("sk-live-123456");
        assert_eq!(format!("{s}"), "[redacted]");
        assert_eq!(format!("{s:?}"), "Secret([redacted])");
        assert_eq!(
            redact("bad key sk-live-123456 given", Some(&s)),
            "bad key [redacted] given"
        );
        assert_eq!(redact("x", None), "x");
    }

    #[test]
    fn names_are_checked() {
        let m = MemoryStore::default();
        assert!(m.set("../evil", &Secret::new("x")).is_err());
        assert!(m.set(".key", &Secret::new("x")).is_err());
        assert!(m.set("Provider", &Secret::new("x")).is_err());
        assert!(m.set("provider.openai", &Secret::new("x")).is_ok());
        assert!(self_test(&m).is_ok());
    }

    #[cfg(not(windows))]
    #[test]
    fn file_store_encrypts_and_round_trips() {
        let dir = tmpdir("file");
        let store = PlatformStore::files_only(&dir);
        let key = Secret::new("sk-test-very-secret-value");
        store.set("provider.openai", &key).unwrap();
        let raw = std::fs::read(dir.join("secrets/provider.openai.enc")).unwrap();
        assert!(
            !raw.windows(10).any(|w| w == b"very-secre"),
            "plain text found on disk"
        );
        assert_eq!(store.get("provider.openai").unwrap(), Some(key));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("secrets/.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "key file must be private");
        }
        store.delete("provider.openai").unwrap();
        assert_eq!(store.get("provider.openai").unwrap(), None);
        assert!(self_test(&store).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn tampered_file_is_rejected() {
        let dir = tmpdir("tamper");
        let store = PlatformStore::files_only(&dir);
        store.set("k", &Secret::new("abcdef")).unwrap();
        let p = dir.join("secrets/k.enc");
        let mut raw = std::fs::read(&p).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        std::fs::write(&p, raw).unwrap();
        assert!(store.get("k").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
