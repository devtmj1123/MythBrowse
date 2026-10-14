// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The login store: `logins.sqlite3` in the profile directory, apart from `profile.sqlite3`
//! (history, bookmarks, chats) so it is never part of sync, history exports or chat
//! retention, and can be wiped on its own.
//!
//! Plain columns: the origin (needed to look logins up), its registrable domain, times and
//! counters. Encrypted columns ([`crate::crypto`], AES-256-GCM bound to origin + field):
//! username, password, note. A `meta.check` value sealed with the vault key tells "wrong /
//! missing key" (e.g. the profile was copied to another Windows account) apart from an empty
//! store, so a lost key never silently starts a second vault over the old rows.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use mythbrowse_ai::secrets::{Secret, SecretStore};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::crypto::{CryptoError, VaultKey, aad};
use crate::csv::CsvLogin;
use crate::origin::{MatchKind, Origin, match_kind};

const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS logins (
    id           INTEGER PRIMARY KEY,
    origin       TEXT NOT NULL,
    site         TEXT NOT NULL,
    action       TEXT NOT NULL DEFAULT '',
    username     BLOB NOT NULL,
    password     BLOB NOT NULL,
    note         BLOB,
    created_ms   INTEGER NOT NULL,
    updated_ms   INTEGER NOT NULL,
    last_used_ms INTEGER NOT NULL DEFAULT 0,
    times_used   INTEGER NOT NULL DEFAULT 0,
    source       TEXT NOT NULL DEFAULT 'saved'
);
CREATE INDEX IF NOT EXISTS logins_by_site ON logins (site);
CREATE TABLE IF NOT EXISTS never_save (
    origin     TEXT PRIMARY KEY,
    created_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
) WITHOUT ROWID;
";
const CHECK_PLAIN: &[u8] = b"mythbrowse vault check v1";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// Rows exist but the key in this OS account's key store cannot open them.
    #[error("saved passwords are locked: this profile's key is not available for this OS user")]
    Locked,
    #[error("not a web address: {0}")]
    BadOrigin(String),
    #[error("no such login")]
    NotFound,
    #[error("password store version {0} is newer than this build supports")]
    TooNew(i64),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A saved login without its password (what lists and suggestions show).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoginMeta {
    pub id: i64,
    pub origin: String,
    pub username: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub last_used_ms: i64,
    pub times_used: i64,
    pub source: String,
    pub has_note: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Match {
    #[serde(flatten)]
    pub login: LoginMeta,
    pub kind: MatchKind,
}

/// What to offer after the user submitted a login form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "offer", rename_all = "snake_case")]
pub enum SaveCheck {
    /// "Save password?"
    New,
    /// "Update password?" for this login.
    Update { id: i64 },
    /// Already saved exactly so: nothing to ask.
    Same { id: i64 },
    /// The user chose "Never for this site".
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Saved {
    Added(i64),
    Updated(i64),
    Unchanged(i64),
}

impl Saved {
    pub fn id(self) -> i64 {
        match self {
            Saved::Added(i) | Saved::Updated(i) | Saved::Unchanged(i) => i,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ImportCounts {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
}

pub struct LoginStore {
    conn: Connection,
    key: VaultKey,
}

struct Row {
    id: i64,
    origin: String,
    username: Vec<u8>,
    note: Option<Vec<u8>>,
    created_ms: i64,
    updated_ms: i64,
    last_used_ms: i64,
    times_used: i64,
    source: String,
}

const ROW_COLS: &str =
    "id, origin, username, note, created_ms, updated_ms, last_used_ms, times_used, source";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        origin: r.get(1)?,
        username: r.get(2)?,
        note: r.get(3)?,
        created_ms: r.get(4)?,
        updated_ms: r.get(5)?,
        last_used_ms: r.get(6)?,
        times_used: r.get(7)?,
        source: r.get(8)?,
    })
}

impl LoginStore {
    /// Open `<dir>/logins.sqlite3` with the vault key from `keys` (created on first use).
    pub fn open_in_dir(dir: &Path, keys: &dyn SecretStore) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("logins.sqlite3"))?;
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA secure_delete = ON;")?;
        Self::init(conn, keys)
    }

    /// In-memory store with the given key (tests).
    pub fn open_in_memory(key: VaultKey) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        let s = Self { conn, key };
        s.write_check()?;
        Ok(s)
    }

    fn init(conn: Connection, keys: &dyn SecretStore) -> Result<Self> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::TooNew(version));
        }
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        let check: Option<Vec<u8>> = conn
            .query_row("SELECT value FROM meta WHERE key = 'check'", [], |r| {
                r.get(0)
            })
            .optional()?;
        let key = VaultKey::load(keys)?;
        match (check, key) {
            (Some(check), Some(key)) => {
                key.open(&check, &aad("", "check"))
                    .map_err(|_| StoreError::Locked)?;
                Ok(Self { conn, key })
            }
            (Some(_), None) => Err(StoreError::Locked),
            (None, key) => {
                let rows: i64 = conn.query_row("SELECT COUNT(*) FROM logins", [], |r| r.get(0))?;
                if rows > 0 {
                    return Err(StoreError::Locked);
                }
                let key = match key {
                    Some(k) => k,
                    None => {
                        let k = VaultKey::generate()?;
                        k.save(keys)?;
                        k
                    }
                };
                let s = Self { conn, key };
                s.write_check()?;
                Ok(s)
            }
        }
    }

    /// Wipe a locked store: delete every login and start over with a new key. Only on the
    /// user's explicit request ("Reset saved passwords").
    pub fn reset_in_dir(dir: &Path, keys: &dyn SecretStore) -> Result<Self> {
        for name in ["logins.sqlite3", "logins.sqlite3-wal", "logins.sqlite3-shm"] {
            match std::fs::remove_file(dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let _ = keys.delete(crate::crypto::KEY_NAME);
        Self::open_in_dir(dir, keys)
    }

    fn write_check(&self) -> Result<()> {
        let blob = self.key.seal(CHECK_PLAIN, &aad("", "check"))?;
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('check', ?1)",
            params![blob],
        )?;
        Ok(())
    }

    fn meta(&self, r: Row) -> Result<LoginMeta> {
        let username = self
            .key
            .open_secret(&r.username, &aad(&r.origin, "username"))?;
        Ok(LoginMeta {
            id: r.id,
            username: username.expose().to_owned(),
            origin: r.origin,
            created_ms: r.created_ms,
            updated_ms: r.updated_ms,
            last_used_ms: r.last_used_ms,
            times_used: r.times_used,
            source: r.source,
            has_note: r.note.is_some(),
        })
    }

    fn rows(&self, sql_where: &str, p: impl rusqlite::Params) -> Result<Vec<Row>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {ROW_COLS} FROM logins {sql_where}"))?;
        let rows = st
            .query_map(p, read_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn len(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM logins", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Every login (no passwords), filtered by `query` on origin and username
    /// (case-insensitive), sorted by site then username.
    pub fn list(&self, query: &str) -> Result<Vec<LoginMeta>> {
        let q = query.trim().to_lowercase();
        let mut out = Vec::new();
        for r in self.rows("", [])? {
            let m = self.meta(r)?;
            if q.is_empty()
                || m.origin.to_lowercase().contains(&q)
                || m.username.to_lowercase().contains(&q)
            {
                out.push(m);
            }
        }
        out.sort_by(|a, b| {
            let host = |o: &str| o.split("://").nth(1).unwrap_or(o).to_owned();
            host(&a.origin)
                .cmp(&host(&b.origin))
                .then_with(|| a.username.to_lowercase().cmp(&b.username.to_lowercase()))
        });
        Ok(out)
    }

    pub fn get(&self, id: i64) -> Result<LoginMeta> {
        let r = self
            .rows("WHERE id = ?1", params![id])?
            .into_iter()
            .next()
            .ok_or(StoreError::NotFound)?;
        self.meta(r)
    }

    /// Logins that may be offered on `page_url` (see [`crate::origin`]): exact matches
    /// first, then most recently used.
    pub fn matches(&self, page_url: &str) -> Result<Vec<Match>> {
        let Some(page) = Origin::parse(page_url) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for r in self.rows("WHERE site = ?1", params![page.site()])? {
            let Some(saved) = Origin::parse(&r.origin) else {
                continue;
            };
            if let Some(kind) = match_kind(&saved, &page) {
                out.push(Match {
                    login: self.meta(r)?,
                    kind,
                });
            }
        }
        out.sort_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then(b.login.last_used_ms.cmp(&a.login.last_used_ms))
                .then(a.login.id.cmp(&b.login.id))
        });
        Ok(out)
    }

    /// The password of login `id`. Only for filling a page the user chose, or showing it on
    /// the passwords page after re-authentication; never for the AI agent.
    pub fn password(&self, id: i64) -> Result<Secret> {
        let (origin, blob): (String, Vec<u8>) = self
            .conn
            .query_row(
                "SELECT origin, password FROM logins WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(self.key.open_secret(&blob, &aad(&origin, "password"))?)
    }

    pub fn note(&self, id: i64) -> Result<String> {
        let (origin, blob): (String, Option<Vec<u8>>) = self
            .conn
            .query_row(
                "SELECT origin, note FROM logins WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        match blob {
            Some(b) => Ok(self
                .key
                .open_secret(&b, &aad(&origin, "note"))?
                .expose()
                .to_owned()),
            None => Ok(String::new()),
        }
    }

    fn find_exact(&self, origin: &str, username: &str) -> Result<Option<i64>> {
        for r in self.rows("WHERE origin = ?1", params![origin])? {
            let id = r.id;
            if self.meta(r)?.username == username {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// What to ask after a login form on `page_url` was submitted with these values.
    pub fn check_submit(
        &self,
        page_url: &str,
        username: &str,
        password: &Secret,
    ) -> Result<SaveCheck> {
        let origin = Origin::parse(page_url)
            .ok_or_else(|| StoreError::BadOrigin(page_url.to_owned()))?
            .to_string();
        if self.is_never(&origin)? {
            return Ok(SaveCheck::Never);
        }
        match self.find_exact(&origin, username)? {
            Some(id) if self.password(id)?.expose() == password.expose() => {
                Ok(SaveCheck::Same { id })
            }
            Some(id) => Ok(SaveCheck::Update { id }),
            None => Ok(SaveCheck::New),
        }
    }

    /// Save (or update the password of) the login for `page_url` + `username`.
    pub fn save(
        &self,
        page_url: &str,
        action: &str,
        username: &str,
        password: &Secret,
        source: &str,
    ) -> Result<Saved> {
        let o =
            Origin::parse(page_url).ok_or_else(|| StoreError::BadOrigin(page_url.to_owned()))?;
        let origin = o.to_string();
        let now = now_ms();
        if let Some(id) = self.find_exact(&origin, username)? {
            if self.password(id)?.expose() == password.expose() {
                return Ok(Saved::Unchanged(id));
            }
            let blob = self
                .key
                .seal_str(password.expose(), &aad(&origin, "password"))?;
            self.conn.execute(
                "UPDATE logins SET password = ?1, updated_ms = ?2 WHERE id = ?3",
                params![blob, now, id],
            )?;
            return Ok(Saved::Updated(id));
        }
        let action = Origin::parse(action)
            .map(|a| a.to_string())
            .unwrap_or_default();
        self.conn.execute(
            "INSERT INTO logins (origin, site, action, username, password, created_ms, updated_ms, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![
                origin,
                o.site(),
                action,
                self.key.seal_str(username, &aad(&origin, "username"))?,
                self.key.seal_str(password.expose(), &aad(&origin, "password"))?,
                now,
                source
            ],
        )?;
        Ok(Saved::Added(self.conn.last_insert_rowid()))
    }

    /// Edit a login from the passwords page. `None` keeps a field as it is.
    pub fn update(
        &self,
        id: i64,
        username: Option<&str>,
        password: Option<&Secret>,
        note: Option<&str>,
    ) -> Result<()> {
        let origin = self.get(id)?.origin;
        let now = now_ms();
        if let Some(u) = username {
            let blob = self.key.seal_str(u, &aad(&origin, "username"))?;
            self.conn.execute(
                "UPDATE logins SET username = ?1, updated_ms = ?2 WHERE id = ?3",
                params![blob, now, id],
            )?;
        }
        if let Some(p) = password.filter(|p| !p.expose().is_empty()) {
            let blob = self.key.seal_str(p.expose(), &aad(&origin, "password"))?;
            self.conn.execute(
                "UPDATE logins SET password = ?1, updated_ms = ?2 WHERE id = ?3",
                params![blob, now, id],
            )?;
        }
        if let Some(n) = note {
            let blob = if n.is_empty() {
                None
            } else {
                Some(self.key.seal_str(n, &aad(&origin, "note"))?)
            };
            self.conn.execute(
                "UPDATE logins SET note = ?1, updated_ms = ?2 WHERE id = ?3",
                params![blob, now, id],
            )?;
        }
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM logins WHERE id = ?1", params![id])?
            > 0)
    }

    pub fn mark_used(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE logins SET last_used_ms = ?1, times_used = times_used + 1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------- never save

    pub fn is_never(&self, origin: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM never_save WHERE origin = ?1",
                params![origin],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// "Never for this site": `page_url`'s origin is not offered for saving again.
    pub fn add_never(&self, page_url: &str) -> Result<()> {
        let origin = Origin::parse(page_url)
            .ok_or_else(|| StoreError::BadOrigin(page_url.to_owned()))?
            .to_string();
        self.conn.execute(
            "INSERT OR IGNORE INTO never_save (origin, created_ms) VALUES (?1, ?2)",
            params![origin, now_ms()],
        )?;
        Ok(())
    }

    pub fn remove_never(&self, origin: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM never_save WHERE origin = ?1", params![origin])?
            > 0)
    }

    pub fn never_list(&self) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT origin FROM never_save ORDER BY origin")?;
        let v = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    // ------------------------------------------------------------- import / export

    /// Import logins (CSV or another browser). Rows whose URL is not a web address are
    /// skipped; an existing origin + username gets the imported password.
    pub fn import(&self, rows: &[CsvLogin], source: &str) -> Result<ImportCounts> {
        let mut c = ImportCounts::default();
        let tx = self.conn.unchecked_transaction()?;
        for r in rows {
            if Origin::parse(&r.url).is_none() || r.password.expose().is_empty() {
                c.skipped += 1;
                continue;
            }
            match self.save(&r.url, "", &r.username, &r.password, source)? {
                Saved::Added(id) => {
                    c.added += 1;
                    if !r.note.is_empty() {
                        self.update(id, None, None, Some(&r.note))?;
                    }
                }
                Saved::Updated(_) => c.updated += 1,
                Saved::Unchanged(_) => c.unchanged += 1,
            }
        }
        tx.commit()?;
        Ok(c)
    }

    /// Every login with its password, for a CSV export the user asked for.
    pub fn export(&self) -> Result<Vec<CsvLogin>> {
        let mut out = Vec::new();
        for m in self.list("")? {
            out.push(CsvLogin {
                name: m.origin.split("://").nth(1).unwrap_or(&m.origin).to_owned(),
                url: m.origin.clone(),
                username: m.username.clone(),
                password: self.password(m.id)?,
                note: if m.has_note {
                    self.note(m.id)?
                } else {
                    String::new()
                },
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythbrowse_ai::secrets::MemoryStore;

    fn store() -> LoginStore {
        LoginStore::open_in_memory(VaultKey::generate().unwrap()).unwrap()
    }

    #[test]
    fn save_match_update_and_never() {
        let s = store();
        let pw = Secret::new("hunter2");
        assert_eq!(
            s.check_submit("https://example.com/login", "alice", &pw)
                .unwrap(),
            SaveCheck::New
        );
        let id = s
            .save(
                "https://example.com/login",
                "https://example.com/session",
                "alice",
                &pw,
                "saved",
            )
            .unwrap();
        assert!(matches!(id, Saved::Added(_)));
        let id = id.id();
        assert_eq!(
            s.check_submit("https://example.com/x", "alice", &pw)
                .unwrap(),
            SaveCheck::Same { id }
        );
        assert_eq!(
            s.check_submit("https://example.com/x", "alice", &Secret::new("new"))
                .unwrap(),
            SaveCheck::Update { id }
        );
        assert_eq!(
            s.save(
                "https://example.com",
                "",
                "alice",
                &Secret::new("new"),
                "saved"
            )
            .unwrap(),
            Saved::Updated(id)
        );
        assert_eq!(s.password(id).unwrap().expose(), "new");

        // Same site, other host: offered as SameSite; http page: never.
        let m = s.matches("https://www.example.com/").unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].kind, MatchKind::SameSite);
        assert_eq!(m[0].login.username, "alice");
        assert!(s.matches("http://example.com/").unwrap().is_empty());
        assert!(s.matches("https://example.org/").unwrap().is_empty());
        assert_eq!(
            s.matches("https://example.com/a").unwrap()[0].kind,
            MatchKind::Exact
        );

        s.add_never("https://never.example/login").unwrap();
        assert_eq!(
            s.check_submit("https://never.example/", "x", &pw).unwrap(),
            SaveCheck::Never
        );
        assert_eq!(
            s.never_list().unwrap(),
            vec!["https://never.example".to_owned()]
        );
        assert!(s.remove_never("https://never.example").unwrap());
    }

    #[test]
    fn values_are_encrypted_at_rest() {
        let s = store();
        s.save(
            "https://bank.example",
            "",
            "bob@example.com",
            &Secret::new("S3cr3t-pa55"),
            "saved",
        )
        .unwrap();
        let (u, p): (Vec<u8>, Vec<u8>) = s
            .conn
            .query_row("SELECT username, password FROM logins", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let has = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        assert!(!has(&u, b"bob@example.com"));
        assert!(!has(&p, b"S3cr3t-pa55"));
        // A password blob moved to another row does not open there.
        s.save(
            "https://other.example",
            "",
            "eve",
            &Secret::new("x"),
            "saved",
        )
        .unwrap();
        s.conn
            .execute(
                "UPDATE logins SET password = ?1 WHERE origin = 'https://other.example'",
                params![p],
            )
            .unwrap();
        let other = s.list("other").unwrap()[0].id;
        assert!(s.password(other).is_err());
    }

    #[test]
    fn list_edit_delete_export_import() {
        let s = store();
        let a = s
            .save("https://a.example", "", "ann", &Secret::new("1"), "saved")
            .unwrap()
            .id();
        s.save("https://b.example", "", "bea", &Secret::new("2"), "saved")
            .unwrap();
        assert_eq!(s.list("").unwrap().len(), 2);
        assert_eq!(s.list("BEA").unwrap().len(), 1);
        assert_eq!(s.list("a.example").unwrap()[0].id, a);
        s.update(a, Some("anna"), Some(&Secret::new("one")), Some("pin 1234"))
            .unwrap();
        let m = s.get(a).unwrap();
        assert_eq!((m.username.as_str(), m.has_note), ("anna", true));
        assert_eq!(s.password(a).unwrap().expose(), "one");
        assert_eq!(s.note(a).unwrap(), "pin 1234");

        let rows = s.export().unwrap();
        assert_eq!(rows.len(), 2);
        let text = crate::csv::write_logins(&rows);
        let s2 = store();
        let (parsed, _) = crate::csv::parse_logins(&text).unwrap();
        let c = s2.import(&parsed, "csv").unwrap();
        assert_eq!((c.added, c.skipped), (2, 0));
        assert_eq!(s2.import(&parsed, "csv").unwrap().unchanged, 2);
        let id = s2.list("anna").unwrap()[0].id;
        assert_eq!(s2.password(id).unwrap().expose(), "one");
        assert_eq!(s2.note(id).unwrap(), "pin 1234");
        let bad = vec![CsvLogin {
            name: String::new(),
            url: "android://x@com.app".into(),
            username: "u".into(),
            password: Secret::new("p"),
            note: String::new(),
        }];
        assert_eq!(s2.import(&bad, "csv").unwrap().skipped, 1);

        assert!(s.delete(a).unwrap());
        assert!(matches!(s.password(a), Err(StoreError::NotFound)));
    }

    #[test]
    fn missing_or_wrong_key_locks_instead_of_starting_over() {
        let dir =
            std::env::temp_dir().join(format!("mb-pw-test-{}-{}", std::process::id(), now_ms()));
        let keys = MemoryStore::default();
        {
            let s = LoginStore::open_in_dir(&dir, &keys).unwrap();
            s.save("https://example.com", "", "a", &Secret::new("p"), "saved")
                .unwrap();
        }
        // Same key: opens and reads.
        let s = LoginStore::open_in_dir(&dir, &keys).unwrap();
        assert_eq!(s.len().unwrap(), 1);
        drop(s);
        // Another OS user (no key) or another key: locked, rows untouched.
        let empty = MemoryStore::default();
        assert!(matches!(
            LoginStore::open_in_dir(&dir, &empty),
            Err(StoreError::Locked)
        ));
        VaultKey::generate().unwrap().save(&empty).unwrap();
        assert!(matches!(
            LoginStore::open_in_dir(&dir, &empty),
            Err(StoreError::Locked)
        ));
        // Explicit reset: empty store with a fresh key.
        let s = LoginStore::reset_in_dir(&dir, &empty).unwrap();
        assert_eq!(s.len().unwrap(), 0);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
