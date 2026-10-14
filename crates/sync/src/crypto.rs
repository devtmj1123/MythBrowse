// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! End-to-end encryption for sync.
//!
//! - One random 256-bit **sync key** per account, made on the first device. It never leaves a
//!   device in the clear.
//! - The server keeps it only **wrapped**: Argon2id(sync passphrase, salt) -> wrapping key,
//!   XChaCha20-Poly1305. The passphrase is separate from the account password on purpose: the
//!   account password is sent to the server at sign-in, so a key derived from it would not be
//!   end-to-end.
//! - A **recovery code** (the raw sync key in base32 groups) restores sync when the passphrase
//!   is forgotten.
//! - Sub-keys come from the sync key with HKDF-SHA256: one encrypts records, one hashes record
//!   ids, so the server sees neither content, kind nor URL of a record.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
/// Prefix of every record ciphertext string; bump on format changes.
pub const RECORD_FORMAT: &str = "v1.";
/// Row ids: 32 hex chars (128 bits of HMAC), within Appwrite's 36-char id limit.
const ROW_ID_HEX: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CryptoError {
    #[error("wrong sync passphrase or damaged key")]
    Unwrap,
    #[error("record could not be decrypted (wrong key or tampered)")]
    Decrypt,
    #[error("unknown record format")]
    Format,
    #[error("recovery code is not valid")]
    RecoveryCode,
    #[error("passphrase must be at least {0} characters")]
    WeakPassphrase(usize),
    #[error("system random generator failed: {0}")]
    Random(String),
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

pub const MIN_PASSPHRASE: usize = 8;

fn random<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).map_err(|e| CryptoError::Random(e.to_string()))?;
    Ok(b)
}

/// The account's sync key. Zeroed on drop; `Debug` never prints it.
#[derive(Clone, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
pub struct SyncKey([u8; KEY_LEN]);

impl std::fmt::Debug for SyncKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncKey(..)")
    }
}

/// Argon2id parameters, stored next to the wrapped key so they can be raised later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for KdfParams {
    /// OWASP's Argon2id baseline (m = 19 MiB, t = 2, p = 1): well under a second on a laptop.
    fn default() -> Self {
        KdfParams {
            m: 19 * 1024,
            t: 2,
            p: 1,
        }
    }
}

/// The sync key wrapped with the passphrase: what the server stores (account prefs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKey {
    pub v: u32,
    pub kdf: KdfParams,
    /// base64url, 16 bytes.
    pub salt: String,
    /// base64url, nonce (24 bytes) || ciphertext.
    pub ct: String,
}

fn wrapping_key(pass: &str, salt: &[u8], kdf: KdfParams) -> Result<[u8; KEY_LEN], CryptoError> {
    let params = Params::new(kdf.m, kdf.t, kdf.p, Some(KEY_LEN))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let mut out = [0u8; KEY_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pass.as_bytes(), salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

fn seal(key: &[u8; KEY_LEN], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let nonce: [u8; NONCE_LEN] = random()?;
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .map_err(|_| CryptoError::Decrypt)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open(key: &[u8; KEY_LEN], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN + 16 {
        return None;
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .ok()
}

const WRAP_AAD: &[u8] = b"mythbrowse-sync wrapped-key v1";

impl SyncKey {
    pub fn generate() -> Result<Self, CryptoError> {
        Ok(SyncKey(random()?))
    }

    pub fn from_bytes(b: [u8; KEY_LEN]) -> Self {
        SyncKey(b)
    }

    /// The raw key bytes, for the device's local secret store.
    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// Wrap with the sync passphrase for storage on the server.
    pub fn wrap(&self, passphrase: &str, kdf: KdfParams) -> Result<WrappedKey, CryptoError> {
        if passphrase.chars().count() < MIN_PASSPHRASE {
            return Err(CryptoError::WeakPassphrase(MIN_PASSPHRASE));
        }
        let salt: [u8; 16] = random()?;
        let mut wk = wrapping_key(passphrase, &salt, kdf)?;
        let ct = seal(&wk, WRAP_AAD, &self.0);
        wk.zeroize();
        Ok(WrappedKey {
            v: 1,
            kdf,
            salt: BASE64URL_NOPAD.encode(&salt),
            ct: BASE64URL_NOPAD.encode(&ct?),
        })
    }

    pub fn unwrap(wrapped: &WrappedKey, passphrase: &str) -> Result<Self, CryptoError> {
        if wrapped.v != 1 {
            return Err(CryptoError::Format);
        }
        let salt = BASE64URL_NOPAD
            .decode(wrapped.salt.as_bytes())
            .map_err(|_| CryptoError::Unwrap)?;
        let ct = BASE64URL_NOPAD
            .decode(wrapped.ct.as_bytes())
            .map_err(|_| CryptoError::Unwrap)?;
        let mut wk = wrapping_key(passphrase, &salt, wrapped.kdf)?;
        let plain = open(&wk, WRAP_AAD, &ct);
        wk.zeroize();
        let mut plain = plain.ok_or(CryptoError::Unwrap)?;
        let key: [u8; KEY_LEN] = plain
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Unwrap)?;
        plain.zeroize();
        Ok(SyncKey(key))
    }

    /// Recovery code: the key in base32, groups of 4 (`ABCD-EFGH-...`, 13 groups) plus a
    /// 4-char checksum group so a mistyped code is caught before it is used.
    pub fn recovery_code(&self) -> String {
        let mut s = BASE32_NOPAD.encode(&self.0);
        let sum = Sha256::digest_bytes(&self.0);
        s.push_str(&BASE32_NOPAD.encode(&sum[..3])[..4]);
        let groups: Vec<&str> = s
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        let out = groups.join("-");
        s.zeroize();
        out
    }

    pub fn from_recovery_code(code: &str) -> Result<Self, CryptoError> {
        let mut clean: String = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            // Common misreads in base32 (which has no 0/1/8).
            .map(|c| match c {
                '0' => 'O',
                '1' => 'I',
                '8' => 'B',
                c => c,
            })
            .collect();
        // 32 bytes = 52 base32 chars (no padding) + 4 checksum chars.
        if clean.len() != 56 {
            clean.zeroize();
            return Err(CryptoError::RecoveryCode);
        }
        let (body, sum) = clean.split_at(52);
        let bytes = BASE32_NOPAD
            .decode(body.as_bytes())
            .map_err(|_| CryptoError::RecoveryCode);
        let check = bytes.as_ref().ok().map(|b| {
            let d = Sha256::digest_bytes(b);
            BASE32_NOPAD.encode(&d[..3])[..4].to_owned()
        });
        let ok = check.as_deref() == Some(sum);
        clean.zeroize();
        let mut bytes = bytes?;
        if !ok {
            bytes.zeroize();
            return Err(CryptoError::RecoveryCode);
        }
        let key: [u8; KEY_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::RecoveryCode)?;
        bytes.zeroize();
        Ok(SyncKey(key))
    }

    /// Derived sub-keys for record encryption and record-id hashing.
    pub fn record_keys(&self) -> RecordKeys {
        let hk = Hkdf::<Sha256>::new(Some(b"mythbrowse-sync v1"), &self.0);
        let mut enc = [0u8; KEY_LEN];
        let mut ids = [0u8; KEY_LEN];
        hk.expand(b"record encryption", &mut enc)
            .expect("32 bytes is a valid HKDF length");
        hk.expand(b"record ids", &mut ids)
            .expect("32 bytes is a valid HKDF length");
        RecordKeys { enc, ids }
    }
}

trait DigestBytes {
    fn digest_bytes(data: &[u8]) -> [u8; 32];
}

impl DigestBytes for Sha256 {
    fn digest_bytes(data: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        Sha256::digest(data).into()
    }
}

/// Keys that encrypt records and hide their ids.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RecordKeys {
    enc: [u8; KEY_LEN],
    ids: [u8; KEY_LEN],
}

impl std::fmt::Debug for RecordKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecordKeys(..)")
    }
}

impl RecordKeys {
    /// Server-side row id for a record: HMAC(kind, local id), so equal records from two devices
    /// land on the same row while the server learns nothing about the URL or kind.
    pub fn row_id(&self, kind: &str, id: &str) -> String {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&self.ids).expect("HMAC takes any key length");
        mac.update(kind.as_bytes());
        mac.update(&[0]);
        mac.update(id.as_bytes());
        let tag = mac.finalize().into_bytes();
        let mut hex = String::with_capacity(ROW_ID_HEX);
        for b in &tag[..ROW_ID_HEX / 2] {
            hex.push_str(&format!("{b:02x}"));
        }
        hex
    }

    /// Encrypt a record body. The row id is bound in as associated data, so the server cannot
    /// move a ciphertext onto another row.
    pub fn encrypt(&self, row_id: &str, plain: &[u8]) -> Result<String, CryptoError> {
        let sealed = seal(&self.enc, row_id.as_bytes(), plain)?;
        Ok(format!(
            "{RECORD_FORMAT}{}",
            BASE64URL_NOPAD.encode(&sealed)
        ))
    }

    pub fn decrypt(&self, row_id: &str, ct: &str) -> Result<Vec<u8>, CryptoError> {
        let body = ct.strip_prefix(RECORD_FORMAT).ok_or(CryptoError::Format)?;
        let sealed = BASE64URL_NOPAD
            .decode(body.as_bytes())
            .map_err(|_| CryptoError::Decrypt)?;
        open(&self.enc, row_id.as_bytes(), &sealed).ok_or(CryptoError::Decrypt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap params so tests stay fast.
    const FAST: KdfParams = KdfParams { m: 64, t: 1, p: 1 };

    #[test]
    fn wrap_roundtrip_and_wrong_passphrase() {
        let k = SyncKey::generate().unwrap();
        let w = k.wrap("correct horse battery", FAST).unwrap();
        assert_eq!(SyncKey::unwrap(&w, "correct horse battery").unwrap(), k);
        assert_eq!(
            SyncKey::unwrap(&w, "wrong horse battery"),
            Err(CryptoError::Unwrap)
        );
        // Wrapped twice: different salt + nonce.
        assert_ne!(k.wrap("correct horse battery", FAST).unwrap(), w);
        assert_eq!(
            k.wrap("short", FAST),
            Err(CryptoError::WeakPassphrase(MIN_PASSPHRASE))
        );
    }

    #[test]
    fn wrapped_key_json_has_no_key_material() {
        let k = SyncKey::from_bytes([7; 32]);
        let json = serde_json::to_string(&k.wrap("passphrase!", FAST).unwrap()).unwrap();
        assert!(!json.contains(&BASE64URL_NOPAD.encode(&[7; 32])));
        let back: WrappedKey = serde_json::from_str(&json).unwrap();
        assert_eq!(SyncKey::unwrap(&back, "passphrase!").unwrap(), k);
    }

    #[test]
    fn recovery_code_roundtrip_and_typos() {
        let k = SyncKey::generate().unwrap();
        let code = k.recovery_code();
        assert_eq!(code.split('-').count(), 14);
        assert_eq!(SyncKey::from_recovery_code(&code).unwrap(), k);
        // Lower case, spaces instead of dashes.
        let sloppy = code.to_lowercase().replace('-', " ");
        assert_eq!(SyncKey::from_recovery_code(&sloppy).unwrap(), k);
        // One wrong character is caught by the checksum.
        let mut bad: Vec<char> = code.chars().collect();
        bad[0] = if bad[0] == 'A' { 'B' } else { 'A' };
        let bad: String = bad.into_iter().collect();
        assert_eq!(
            SyncKey::from_recovery_code(&bad),
            Err(CryptoError::RecoveryCode)
        );
        assert_eq!(
            SyncKey::from_recovery_code("ABCD"),
            Err(CryptoError::RecoveryCode)
        );
    }

    #[test]
    fn records_roundtrip_and_are_bound_to_their_row() {
        let rk = SyncKey::generate().unwrap().record_keys();
        let a = rk.row_id("bookmarks", "https://example.com/");
        let b = rk.row_id("history", "https://example.com/");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "kind is part of the row id");
        assert_eq!(a, rk.row_id("bookmarks", "https://example.com/"), "stable");
        let ct = rk.encrypt(&a, b"hello").unwrap();
        assert!(ct.starts_with(RECORD_FORMAT));
        assert_eq!(rk.decrypt(&a, &ct).unwrap(), b"hello");
        assert_eq!(
            rk.decrypt(&b, &ct),
            Err(CryptoError::Decrypt),
            "moved to another row"
        );
        let other = SyncKey::generate().unwrap().record_keys();
        assert_eq!(other.decrypt(&a, &ct), Err(CryptoError::Decrypt));
        assert_eq!(rk.decrypt(&a, "v0.xxxx"), Err(CryptoError::Format));
    }

    #[test]
    fn debug_never_prints_keys() {
        let k = SyncKey::from_bytes([0xAB; 32]);
        assert_eq!(format!("{k:?}"), "SyncKey(..)");
        assert_eq!(format!("{:?}", k.record_keys()), "RecordKeys(..)");
    }
}
