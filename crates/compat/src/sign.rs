// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Ed25519 signatures for rulesets delivered from outside the binary (the cloud ruleset
//! service, week 4+).
//!
//! A signed ruleset is two files: `<name>.toml` and `<name>.toml.sig`. The signature file is
//! one line, `mbsig1 <key-id> <base64 signature>`, over the domain-separated bytes
//! `"mythbrowse-compat-ruleset-v1\n" || toml bytes`. `key-id` is the first 8 bytes (hex) of
//! SHA-256 of the 32-byte public key. The browser accepts a remote ruleset only if the
//! signature verifies with one of [`TRUSTED_KEYS`] and its `version` is not older than the
//! last one it accepted (no rollback to a ruleset with a since-fixed bad rule).
//!
//! The private key never lives in the repo: `mb-rules keygen` writes it to a file the owner
//! keeps offline (or in a CI secret for the publishing job).

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

const DOMAIN: &[u8] = b"mythbrowse-compat-ruleset-v1\n";
const MAGIC: &str = "mbsig1";

/// A public key the browser trusts for remote rulesets.
#[derive(Debug, Clone, Copy)]
pub struct TrustedKey {
    pub id: &'static str,
    pub public: [u8; 32],
}

/// Release signing keys. The private halves are kept offline by the owner (generated
/// 2026-10-10 with OpenSSL, `rules-release.pem`); never in the repo or CI.
pub const TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey {
    id: "690488fa1881bead",
    public: [
        0x3c, 0x21, 0x4b, 0x56, 0x3c, 0x69, 0x89, 0x0d, 0x05, 0x95, 0xc4, 0x5c, 0x1f, 0x6d, 0x32,
        0xbc, 0x55, 0x86, 0x19, 0xf2, 0x0a, 0x92, 0xe0, 0xc2, 0x33, 0xf6, 0xb5, 0x29, 0x3f, 0xb7,
        0x1a, 0xb9,
    ],
}];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignError {
    #[error("malformed signature file")]
    Malformed,
    #[error("signed by unknown key `{0}`")]
    UnknownKey(String),
    #[error("signature does not verify")]
    BadSignature,
    #[error("bad private key")]
    BadKey,
    #[error("key generation failed")]
    Rng,
    #[error("ruleset version {got} is older than the accepted version {have}")]
    Rollback { got: u64, have: u64 },
}

/// `hex(sha256(public)[..8])`.
pub fn key_id(public: &[u8]) -> String {
    digest(&SHA256, public).as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn message(data: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(DOMAIN.len() + data.len());
    m.extend_from_slice(DOMAIN);
    m.extend_from_slice(data);
    m
}

/// A new key pair: (PKCS#8 private key bytes, 32-byte public key).
pub fn generate() -> Result<(Vec<u8>, Vec<u8>), SignError> {
    let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| SignError::Rng)?;
    let kp = Ed25519KeyPair::from_pkcs8(doc.as_ref()).map_err(|_| SignError::BadKey)?;
    Ok((doc.as_ref().to_vec(), kp.public_key().as_ref().to_vec()))
}

/// Sign `data` with a PKCS#8 Ed25519 key; returns the signature-file line.
pub fn sign(pkcs8: &[u8], data: &[u8]) -> Result<String, SignError> {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| SignError::BadKey)?;
    let sig = kp.sign(&message(data));
    Ok(format!(
        "{MAGIC} {} {}\n",
        key_id(kp.public_key().as_ref()),
        B64.encode(sig.as_ref())
    ))
}

/// Verify a signature file against `keys`; returns the key id that signed it.
pub fn verify(data: &[u8], sig_file: &str, keys: &[TrustedKey]) -> Result<String, SignError> {
    let mut parts = sig_file.split_whitespace();
    let (Some(MAGIC), Some(id), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(SignError::Malformed);
    };
    let sig = B64.decode(sig).map_err(|_| SignError::Malformed)?;
    let key = keys
        .iter()
        .find(|k| k.id == id && key_id(&k.public) == id)
        .ok_or_else(|| SignError::UnknownKey(id.to_owned()))?;
    UnparsedPublicKey::new(&ED25519, &key.public)
        .verify(&message(data), &sig)
        .map_err(|_| SignError::BadSignature)?;
    Ok(id.to_owned())
}

/// Anti-rollback: accept `got` only if it is at least the last accepted version.
pub fn check_version(got: u64, last_accepted: Option<u64>) -> Result<(), SignError> {
    match last_accepted {
        Some(have) if got < have => Err(SignError::Rollback { got, have }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leak_key(public: &[u8]) -> TrustedKey {
        let id: &'static str = Box::leak(key_id(public).into_boxed_str());
        TrustedKey {
            id,
            public: public.try_into().unwrap(),
        }
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let (private, public) = generate().unwrap();
        let keys = [leak_key(&public)];
        let data = b"schema = 1\nid = \"remote\"\nversion = 2\n";
        let sig = sign(&private, data).unwrap();
        assert_eq!(verify(data, &sig, &keys).unwrap(), keys[0].id);
        // Tampered data, wrong key, garbage.
        assert_eq!(
            verify(b"schema = 1\nid = \"remote\"\nversion = 3\n", &sig, &keys),
            Err(SignError::BadSignature)
        );
        let (_, other) = generate().unwrap();
        assert!(matches!(
            verify(data, &sig, &[leak_key(&other)]),
            Err(SignError::UnknownKey(_))
        ));
        assert_eq!(verify(data, "nope", &keys), Err(SignError::Malformed));
        assert!(verify(data, &sig, TRUSTED_KEYS).is_err());
    }

    #[test]
    fn rollback_is_refused() {
        assert!(check_version(5, None).is_ok());
        assert!(check_version(5, Some(5)).is_ok());
        assert_eq!(
            check_version(4, Some(5)),
            Err(SignError::Rollback { got: 4, have: 5 })
        );
    }
}
