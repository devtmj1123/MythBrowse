// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The password vault key and value encryption.
//!
//! * One random 256-bit key per profile (per OS user). It lives in the same secure key store
//!   as the AI API keys ([`mythbrowse_ai::secrets`]): Windows DPAPI (this Windows user only),
//!   Linux Secret Service or the 0600 encrypted-file fallback. It never touches the database.
//! * Each value (username, password, note) is sealed with AES-256-GCM, a fresh 96-bit random
//!   nonce, and associated data binding it to its login's origin and field, so a ciphertext
//!   copied into another row or field fails to open. Blob layout: `b"mb1"` | nonce (12) |
//!   ciphertext+tag.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use mythbrowse_ai::secrets::{Secret, SecretStore};
use zeroize::{Zeroize, Zeroizing};

/// Name of the vault key in the secret store.
pub const KEY_NAME: &str = "passwords-vault";
const MAGIC: &[u8] = b"mb1";
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("the saved passwords could not be decrypted (wrong key or damaged data)")]
    Open,
    #[error("unknown encrypted value format")]
    Format,
    #[error("random number generator failed")]
    Rng,
    #[error("key storage: {0}")]
    Store(String),
}

/// The vault key. Zeroed on drop, never printed.
pub struct VaultKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey([redacted])")
    }
}

impl VaultKey {
    pub fn generate() -> Result<Self, CryptoError> {
        let mut k = Zeroizing::new([0u8; 32]);
        getrandom::getrandom(&mut k[..]).map_err(|_| CryptoError::Rng)?;
        Ok(Self(k))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The key from `store`, if there is one.
    pub fn load(store: &dyn SecretStore) -> Result<Option<Self>, CryptoError> {
        Self::load_named(store, KEY_NAME)
    }

    /// The key saved under `name` in `store`, if there is one.
    pub fn load_named(store: &dyn SecretStore, name: &str) -> Result<Option<Self>, CryptoError> {
        let Some(s) = store
            .get(name)
            .map_err(|e| CryptoError::Store(e.to_string()))?
        else {
            return Ok(None);
        };
        let mut raw = B64
            .decode(s.expose().trim())
            .map_err(|_| CryptoError::Store("stored vault key is not base64".into()))?;
        if raw.len() != 32 {
            raw.zeroize();
            return Err(CryptoError::Store(
                "stored vault key has the wrong length".into(),
            ));
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&raw);
        raw.zeroize();
        let key = Self::from_bytes(k);
        k.zeroize();
        Ok(Some(key))
    }

    /// Save this key in `store` (replacing any old one).
    pub fn save(&self, store: &dyn SecretStore) -> Result<(), CryptoError> {
        self.save_named(store, KEY_NAME)
    }

    /// Save this key under `name` in `store` (replacing any old one).
    pub fn save_named(&self, store: &dyn SecretStore, name: &str) -> Result<(), CryptoError> {
        let s = Secret::new(B64.encode(&self.0[..]));
        store
            .set(name, &s)
            .map_err(|e| CryptoError::Store(e.to_string()))
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new_from_slice(&self.0[..]).expect("32-byte key")
    }

    /// Encrypt `plain`, bound to `aad`.
    pub fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).map_err(|_| CryptoError::Rng)?;
        let ct = self
            .cipher()
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
            .map_err(|_| CryptoError::Open)?;
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a [`seal`](Self::seal)ed blob; fails if it was sealed with another key or `aad`.
    pub fn open(&self, blob: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let rest = blob.strip_prefix(MAGIC).ok_or(CryptoError::Format)?;
        if rest.len() < NONCE_LEN + 16 {
            return Err(CryptoError::Format);
        }
        let (nonce, ct) = rest.split_at(NONCE_LEN);
        self.cipher()
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
            .map(Zeroizing::new)
            .map_err(|_| CryptoError::Open)
    }

    pub fn seal_str(&self, plain: &str, aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.seal(plain.as_bytes(), aad)
    }

    pub fn open_secret(&self, blob: &[u8], aad: &[u8]) -> Result<Secret, CryptoError> {
        let plain = self.open(blob, aad)?;
        let s = std::str::from_utf8(&plain).map_err(|_| CryptoError::Format)?;
        Ok(Secret::new(s))
    }
}

/// Name of the cookie-jar key in the secret store (v0.4.2 `persist_cookies`).
pub const JAR_KEY_NAME: &str = "cookie-jar";
const JAR_AAD: &[u8] = b"mythbrowse-cookie-jar/v1";

/// Seals the engine's cookie jar file at rest (v0.4.2 "Stay signed in to sites after
/// restart"): AES-256-GCM with its own random key, kept in the same platform key store as
/// the vault key (Windows DPAPI for this Windows user; Linux Secret Service, else the 0600
/// encrypted-file fallback). A jar copied to another account or machine does not open.
pub struct JarSealer(VaultKey);

impl JarSealer {
    /// The jar key from `store`, created (and saved) the first time.
    pub fn load_or_create(store: &dyn SecretStore) -> Result<Self, CryptoError> {
        if let Some(k) = VaultKey::load_named(store, JAR_KEY_NAME)? {
            return Ok(Self(k));
        }
        let k = VaultKey::generate()?;
        k.save_named(store, JAR_KEY_NAME)?;
        Ok(Self(k))
    }

    pub fn from_key(key: VaultKey) -> Self {
        Self(key)
    }

    pub fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.0.seal(plain, JAR_AAD)
    }

    pub fn open(&self, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        self.0.open(blob, JAR_AAD)
    }
}

/// Associated data for one field of the login saved for `origin`.
pub fn aad(origin: &str, field: &str) -> Vec<u8> {
    format!("mythbrowse-login/v1|{origin}|{field}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythbrowse_ai::secrets::MemoryStore;

    #[test]
    fn jar_sealer() {
        let store = MemoryStore::default();
        let a = JarSealer::load_or_create(&store).unwrap();
        let blob = a.seal(br#"{"cookies":"sid=secret"}"#).unwrap();
        assert!(!blob.windows(6).any(|w| w == b"secret"));
        // Same store: same key.
        let b = JarSealer::load_or_create(&store).unwrap();
        assert_eq!(&b.open(&blob).unwrap()[..], br#"{"cookies":"sid=secret"}"#);
        // Its own key, not the vault's; another key does not open it.
        assert!(VaultKey::load(&store).unwrap().is_none());
        let other = JarSealer::from_key(VaultKey::generate().unwrap());
        assert!(other.open(&blob).is_err());
        // A blob sealed with other associated data does not open as a jar.
        let k = VaultKey::load_named(&store, JAR_KEY_NAME).unwrap().unwrap();
        assert!(a.open(&k.seal(b"x", b"other").unwrap()).is_err());
    }

    #[test]
    fn round_trip_and_binding() {
        let k = VaultKey::generate().unwrap();
        let a = aad("https://example.com", "password");
        let blob = k.seal_str("hunter2 \u{1F511}", &a).unwrap();
        assert!(blob.starts_with(b"mb1"));
        assert!(!blob.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(
            k.open_secret(&blob, &a).unwrap().expose(),
            "hunter2 \u{1F511}"
        );
        // Same plaintext twice: different nonces, different blobs.
        assert_ne!(blob, k.seal_str("hunter2 \u{1F511}", &a).unwrap());
        // Moved to another origin or field: refused.
        assert!(
            k.open(&blob, &aad("https://evil.example", "password"))
                .is_err()
        );
        assert!(
            k.open(&blob, &aad("https://example.com", "username"))
                .is_err()
        );
        // Another key: refused. Tampered: refused.
        let other = VaultKey::generate().unwrap();
        assert!(other.open(&blob, &a).is_err());
        let mut bad = blob.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(k.open(&bad, &a).is_err());
        assert!(matches!(k.open(b"xx", &a), Err(CryptoError::Format)));
    }

    #[test]
    fn key_store_round_trip() {
        let store = MemoryStore::default();
        assert!(VaultKey::load(&store).unwrap().is_none());
        let k = VaultKey::generate().unwrap();
        k.save(&store).unwrap();
        let k2 = VaultKey::load(&store).unwrap().unwrap();
        let a = aad("o", "f");
        assert_eq!(
            k2.open(&k.seal(b"x", &a).unwrap(), &a).unwrap().as_slice(),
            b"x"
        );
        assert_eq!(format!("{k:?}"), "VaultKey([redacted])");
    }
}
