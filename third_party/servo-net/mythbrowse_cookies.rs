/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! MythBrowse cookie management hooks (vendored patch servo-net-0002).
//!
//! Servo 0.1.4's embedder API (`SiteDataManager`) can list sites, clear a site's cookies and
//! read the cookies a URL would get, but it cannot enumerate every cookie of a site
//! (sub-domain host-only cookies, other paths), delete one cookie, or keep a site from
//! storing cookies. Adding `CoreResourceMsg` variants would mean patching servo-net-traits and
//! servo too, so the embedder reaches these through the two messages it already has, with
//! URLs in schemes no web page can produce (they are never fetched or navigated):
//!
//! * `GetCookiesForUrl(mythbrowse-cookie-jar://<site>/)`: every unexpired cookie stored for
//!   the registrable domain `<site>`, any host / path / flag, with its **value redacted** to
//!   `mbmeta.<name+value bytes>.<host-only 1|0>` so values never leave the cookie jar
//!   through this path. Last-access times are not touched.
//! * `SetCookieForUrl(mythbrowse-cookie-delete://<site>/, cookie)`: remove the stored
//!   cookie with exactly that name, domain and path (HttpOnly / Secure included).
//! * `SetCookieForUrl(mythbrowse-cookie-block://<site>/, cookie "block=1|0")`: add / remove
//!   `<site>` from the blocked set. A blocked site's cookies are neither stored nor sent.
//!
//! Third-party cookie blocking is not here: the cookie set / read paths (HTTP responses,
//! `document.cookie`, CookieStore) do not know the top-level site of the request.
//!
//! Patch servo-net-0003 (sealed cookie jar): with a `config_dir`, stock Servo reads and
//! writes `cookie_jar.json` and `auth_cache.json` (HTTP auth credentials) as plain JSON. When
//! the embedder installs a [`JarCodec`] before the resource threads start, the jar is kept
//! sealed instead, in [`SEALED_JAR_FILE`] (the codec's `seal` / `open`, e.g. AES-GCM with a
//! key in the OS key store), written atomically (temp file + rename, 0600 on Unix), and
//! `auth_cache.json` is neither read nor written (HTTP auth stays in memory). The HSTS list
//! is still kept as plain JSON (it holds host names only).

use std::path::Path;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use serde::de::DeserializeOwned;

pub const JAR_SCHEME: &str = "mythbrowse-cookie-jar";
pub const DELETE_SCHEME: &str = "mythbrowse-cookie-delete";
pub const BLOCK_SCHEME: &str = "mythbrowse-cookie-block";
/// Prefix of the redacted value in `JAR_SCHEME` answers.
pub const META_PREFIX: &str = "mbmeta.";

static BLOCKED: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Add (`on`) or remove a registrable domain (lowercase) from the blocked set.
pub fn set_blocked(site: &str, on: bool) {
    let site = site.to_ascii_lowercase();
    let mut b = BLOCKED.write().unwrap_or_else(|p| p.into_inner());
    b.retain(|s| *s != site);
    if on && !site.is_empty() {
        b.push(site);
    }
}

/// Whether cookies for the registrable domain `site` (as the cookie jar keys it) are blocked.
pub fn is_blocked(site: &str) -> bool {
    let b = BLOCKED.read().unwrap_or_else(|p| p.into_inner());
    !b.is_empty() && b.iter().any(|s| s.eq_ignore_ascii_case(site))
}

/// The redacted value for a cookie: its size in bytes (name + value) and host-only flag.
pub fn meta_value(name: &str, value: &str, host_only: bool) -> String {
    format!(
        "{META_PREFIX}{}.{}",
        name.len() + value.len(),
        if host_only { 1 } else { 0 }
    )
}

/// Encrypts / decrypts the cookie jar file. `None` from either means "could not": a jar that
/// does not open is ignored (start with no cookies), one that does not seal is not written.
pub type JarCodecFn = Arc<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

pub struct JarCodec {
    pub seal: JarCodecFn,
    pub open: JarCodecFn,
}

/// File name of the sealed jar in the config dir.
pub const SEALED_JAR_FILE: &str = "cookie_jar.sealed";

static CODEC: RwLock<Option<Arc<JarCodec>>> = RwLock::new(None);

/// Install (or remove) the jar codec. Call before Servo starts its resource threads.
pub fn set_jar_codec(codec: Option<JarCodec>) {
    *CODEC.write().unwrap_or_else(|p| p.into_inner()) = codec.map(Arc::new);
}

pub fn jar_codec() -> Option<Arc<JarCodec>> {
    CODEC.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Read and open the sealed jar; `None` when there is none or it does not open / parse.
pub fn read_sealed_jar<T: DeserializeOwned>(codec: &JarCodec, dir: &Path) -> Option<T> {
    let blob = std::fs::read(dir.join(SEALED_JAR_FILE)).ok()?;
    let Some(plain) = (codec.open)(&blob) else {
        log::warn!("the saved cookie jar could not be decrypted; starting without cookies");
        return None;
    };
    match serde_json::from_slice(&plain) {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("the saved cookie jar could not be read ({e}); starting without cookies");
            None
        },
    }
}

/// Seal `value` and write it to the config dir (temp file, then rename over the old one).
pub fn write_sealed_jar<T: Serialize>(codec: &JarCodec, value: &T, dir: &Path) {
    let Ok(plain) = serde_json::to_vec(value) else {
        log::warn!("cookie jar: could not serialize; not saved");
        return;
    };
    let Some(blob) = (codec.seal)(&plain) else {
        log::warn!("cookie jar: could not encrypt; not saved");
        return;
    };
    drop(plain);
    let path = dir.join(SEALED_JAR_FILE);
    let tmp = dir.join(format!("{SEALED_JAR_FILE}.tmp"));
    let written = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&blob)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = written {
        log::warn!("cookie jar: could not save ({e})");
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_jar_round_trip() {
        let dir = std::env::temp_dir().join(format!("mb-jar-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let xor = |b: &[u8]| Some(b.iter().map(|x| x ^ 0x5a).collect::<Vec<u8>>());
        let codec = JarCodec {
            seal: Arc::new(xor),
            open: Arc::new(xor),
        };
        let v: Vec<String> = vec!["sid=secret".into()];
        write_sealed_jar(&codec, &v, &dir);
        let raw = std::fs::read(dir.join(SEALED_JAR_FILE)).unwrap();
        assert!(!raw.windows(6).any(|w| w == b"secret"));
        assert_eq!(read_sealed_jar::<Vec<String>>(&codec, &dir), Some(v));
        let bad = JarCodec {
            seal: Arc::new(xor),
            open: Arc::new(|_: &[u8]| None),
        };
        assert_eq!(read_sealed_jar::<Vec<String>>(&bad, &dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_set() {
        set_blocked("Blocked-Set.TEST", true);
        assert!(is_blocked("blocked-set.test"));
        assert!(!is_blocked("other.com"));
        set_blocked("blocked-set.test", true);
        set_blocked("blocked-set.test", false);
        assert!(!is_blocked("blocked-set.test"));
    }

    #[test]
    fn meta() {
        assert_eq!(meta_value("sid", "abcd", true), "mbmeta.7.1");
        assert_eq!(meta_value("", "", false), "mbmeta.0.0");
    }
}
