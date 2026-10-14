// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Local profile storage: browsing history and bookmarks in one SQLite file
//! (`profile.sqlite3` in the profile directory).
//!
//! Why SQLite (bundled via `rusqlite`): it is the store PLAN.md picks for history, bookmarks
//! and settings, it updates rows in place (a JSON+zstd file would be rewritten on every
//! visit), lookups are indexed, and Servo already links the same `rusqlite`/`libsqlite3-sys`
//! versions, so it adds no new dependency to the build. WAL mode + `synchronous=NORMAL`
//! keeps a visit to one small write.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// Bumped when the schema changes; stored in `PRAGMA user_version`.
///
/// Version history (one linear chain; `init` upgrades from any older version in order):
/// 1 history + bookmarks, 2 sync bookkeeping, 3 conversations + `history.conv`,
/// 4 downloads + saved session (`files_store`), 5 per-site permissions (`site_settings`).
pub const SCHEMA_VERSION: i64 = 5;

pub mod appearance;
pub mod chats;
pub mod files_store;
pub mod import;
pub mod site_settings;
pub mod sync_store;

/// Default cap on remembered history entries (oldest are pruned).
pub const DEFAULT_HISTORY_LIMIT: usize = 50_000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS history (
    url           TEXT PRIMARY KEY,
    title         TEXT NOT NULL DEFAULT '',
    visit_count   INTEGER NOT NULL DEFAULT 0,
    last_visit_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS history_by_last_visit ON history (last_visit_ms DESC);
CREATE TABLE IF NOT EXISTS bookmarks (
    id         INTEGER PRIMARY KEY,
    url        TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL DEFAULT '',
    created_ms INTEGER NOT NULL
);
-- v2: sync bookkeeping. One row per synced item (history URL, bookmark URL): its version
-- (`m`, unix ms of the last change), whether it is deleted (tombstone) and whether that
-- version still has to be uploaded. Kept even while sync is off, so turning it on uploads
-- everything once and deletions made in between still reach other devices.
CREATE TABLE IF NOT EXISTS sync_state (
    kind    TEXT NOT NULL,
    id      TEXT NOT NULL,
    m       INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    dirty   INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (kind, id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS sync_state_dirty ON sync_state (dirty) WHERE dirty = 1;
CREATE TABLE IF NOT EXISTS sync_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
";

/// v1 -> v2: existing history and bookmarks become pending uploads.
const BACKFILL_V2: &str = "
INSERT OR IGNORE INTO sync_state (kind, id, m) SELECT 'history', url, last_visit_ms FROM history;
INSERT OR IGNORE INTO sync_state (kind, id, m) SELECT 'bookmarks', url, created_ms FROM bookmarks;
";

/// v2 -> v3: saved Myth conversations, their messages, profile preferences, and the
/// conversation a history row was opened by (Myth's own tabs).
const SCHEMA_V3: &str = "
CREATE TABLE IF NOT EXISTS conversations (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL DEFAULT '',
    model      TEXT NOT NULL DEFAULT '',
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    pinned     INTEGER NOT NULL DEFAULT 0,
    deleted    INTEGER NOT NULL DEFAULT 0,
    title_auto INTEGER NOT NULL DEFAULT 1
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS conversations_by_updated ON conversations (deleted, updated_ms DESC);
CREATE TABLE IF NOT EXISTS messages (
    conv         TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL DEFAULT '',
    tool_calls   TEXT,
    tool_call_id TEXT,
    name         TEXT,
    created_ms   INTEGER NOT NULL,
    PRIMARY KEY (conv, seq)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS prefs (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
";

pub(crate) const KIND_HISTORY: &str = "history";
pub(crate) const KIND_BOOKMARKS: &str = "bookmarks";
pub(crate) const KIND_CHATS: &str = "chats";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("profile schema version {0} is newer than this build supports ({SCHEMA_VERSION})")]
    TooNew(i64),
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visit_count: u32,
    pub last_visit_ms: i64,
    /// The Myth conversation that opened this page (its stable id), if Myth did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conv: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bookmark {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub created_ms: i64,
}

/// Milliseconds since the Unix epoch.
pub fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Only real web/file pages go into history (not `about:`, `data:`, internal UI pages…).
/// `excluded_host` is the browser's internal UI host.
pub fn should_record(url: &str, excluded_host: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    match scheme {
        "http" | "https" => !host.is_empty() && !host.eq_ignore_ascii_case(excluded_host),
        "file" => true,
        _ => false,
    }
}

/// True in official JustMyth builds (feature `official`), false in the open-source Open Build.
pub const OFFICIAL_BUILD: bool = cfg!(feature = "official");

/// Folder name of the profile on Windows (under `%LOCALAPPDATA%`) and of temp-dir fallbacks:
/// `MythBrowse` (official builds) or `MythBrowse Open Build`, so the two builds never share
/// profiles, logs, keys or cookies.
pub const PROFILE_DIR_NAME: &str = if OFFICIAL_BUILD {
    "MythBrowse"
} else {
    "MythBrowse Open Build"
};

/// Folder name of the profile under `$XDG_DATA_HOME` (Linux and others): `mythbrowse` or
/// `mythbrowse-open`.
pub const PROFILE_DIR_NAME_XDG: &str = if OFFICIAL_BUILD {
    "mythbrowse"
} else {
    "mythbrowse-open"
};

/// Where the profile lives: `$MYTHBROWSE_PROFILE_DIR`, else `%LOCALAPPDATA%\<PROFILE_DIR_NAME>`
/// on Windows (`MythBrowse Open Build` in the open build, `MythBrowse` in official builds), else
/// `$XDG_DATA_HOME/<PROFILE_DIR_NAME_XDG>` or `~/.local/share/<PROFILE_DIR_NAME_XDG>`
/// (`mythbrowse-open` / `mythbrowse`).
pub fn default_profile_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = var("MYTHBROWSE_PROFILE_DIR") {
        return Some(dir);
    }
    if cfg!(windows) {
        return var("LOCALAPPDATA").map(|d| d.join(PROFILE_DIR_NAME));
    }
    var("XDG_DATA_HOME")
        .or_else(|| var("HOME").map(|h| h.join(".local").join("share")))
        .map(|d| d.join(PROFILE_DIR_NAME_XDG))
}

/// The profile database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) `profile.sqlite3` inside `dir`.
    pub fn open_in_dir(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        Self::open(&dir.join("profile.sqlite3"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // `journal_mode` returns a row, so read it rather than execute it.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        Self::init(conn)
    }

    /// A throwaway store (tests, or when the profile directory is unusable).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StorageError::TooNew(version));
        }
        conn.execute_batch(SCHEMA)?;
        if version < 2 {
            conn.execute_batch(BACKFILL_V2)?;
        }
        if version < 3 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA_V3)?;
            let has_conv: bool = tx
                .prepare("SELECT 1 FROM pragma_table_info('history') WHERE name = 'conv'")?
                .exists([])?;
            if !has_conv {
                tx.execute_batch("ALTER TABLE history ADD COLUMN conv TEXT;")?;
            }
            tx.commit()?;
        }
        if version < 4 {
            conn.execute_batch(files_store::SCHEMA_V4)?;
        }
        if version < 5 {
            conn.execute_batch(site_settings::SCHEMA_V5)?;
        }
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        Ok(Self { conn })
    }

    /// A local change to a synced item: new version, to be uploaded.
    fn touch(&self, kind: &str, id: &str, deleted: bool) -> Result<()> {
        // Versions only move forward, even if the clock steps back.
        self.conn.execute(
            "INSERT INTO sync_state (kind, id, m, deleted, dirty) VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT(kind, id) DO UPDATE SET
                m = MAX(excluded.m, m + 1), deleted = excluded.deleted, dirty = 1",
            params![kind, id, unix_now_ms(), deleted],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- history

    /// Record a visit to `url` (upsert: bumps the count and time; keeps the old title when
    /// `title` is empty).
    pub fn record_visit(&self, url: &str, title: &str, at_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO history (url, title, visit_count, last_visit_ms) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(url) DO UPDATE SET
                visit_count = visit_count + 1,
                last_visit_ms = MAX(last_visit_ms, excluded.last_visit_ms),
                title = CASE WHEN excluded.title = '' THEN title ELSE excluded.title END",
            params![url, title, at_ms],
        )?;
        self.touch(KIND_HISTORY, url, false)
    }

    /// Update the title of an already-recorded URL (titles arrive after the URL).
    pub fn set_title(&self, url: &str, title: &str) -> Result<bool> {
        if title.is_empty() {
            return Ok(false);
        }
        let n = self.conn.execute(
            "UPDATE history SET title = ?2 WHERE url = ?1",
            params![url, title],
        )?;
        if n > 0 {
            self.touch(KIND_HISTORY, url, false)?;
        }
        Ok(n > 0)
    }

    /// Mark `url` as opened by Myth in conversation `conv` (its stable id), so the history page
    /// can link back to the chat. No-op for URLs not in history.
    pub fn link_history_conv(&self, url: &str, conv: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE history SET conv = ?2 WHERE url = ?1 AND conv IS NOT ?2",
            params![url, conv],
        )?;
        if n > 0 {
            self.touch(KIND_HISTORY, url, false)?;
        }
        Ok(n > 0)
    }

    /// Most recent first.
    pub fn recent_history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT url, title, visit_count, last_visit_ms, conv FROM history
             ORDER BY last_visit_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], history_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Case-insensitive substring match on URL or title, most recent first.
    pub fn search_history(&self, query: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.recent_history(limit);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT url, title, visit_count, last_visit_ms, conv FROM history
             WHERE instr(lower(url), ?1) > 0 OR instr(lower(title), ?1) > 0
             ORDER BY last_visit_ms DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit as i64], history_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn history_entry(&self, url: &str) -> Result<Option<HistoryEntry>> {
        Ok(self
            .conn
            .query_row(
                "SELECT url, title, visit_count, last_visit_ms, conv FROM history WHERE url = ?1",
                params![url],
                history_row,
            )
            .optional()?)
    }

    pub fn delete_history_url(&self, url: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM history WHERE url = ?1", params![url])?;
        if n > 0 {
            self.touch(KIND_HISTORY, url, true)?;
        }
        Ok(n > 0)
    }

    /// Delete all history, here and (as tombstones) on synced devices.
    pub fn clear_history(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE sync_state SET m = MAX(?1, m + 1), deleted = 1, dirty = 1
             WHERE kind = 'history' AND deleted = 0",
            params![unix_now_ms()],
        )?;
        let n = tx.execute("DELETE FROM history", [])?;
        tx.commit()?;
        Ok(n)
    }

    /// Keep only the `keep` most recent entries; returns how many were removed. This is a
    /// local size cap, not a user deletion: other devices keep their copies.
    pub fn prune_history(&self, keep: usize) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM history WHERE url NOT IN
               (SELECT url FROM history ORDER BY last_visit_ms DESC LIMIT ?1)",
            params![keep as i64],
        )?;
        if n > 0 {
            self.conn.execute(
                "DELETE FROM sync_state WHERE kind = 'history' AND deleted = 0
                   AND id NOT IN (SELECT url FROM history)",
                [],
            )?;
        }
        Ok(n)
    }

    pub fn history_len(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    // -------------------------------------------------------------- bookmarks

    /// Add a bookmark (or update its title if the URL is already bookmarked). Returns its id.
    pub fn add_bookmark(&self, url: &str, title: &str, at_ms: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO bookmarks (url, title, created_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(url) DO UPDATE SET
                title = CASE WHEN excluded.title = '' THEN title ELSE excluded.title END",
            params![url, title, at_ms],
        )?;
        self.touch(KIND_BOOKMARKS, url, false)?;
        Ok(self.conn.query_row(
            "SELECT id FROM bookmarks WHERE url = ?1",
            params![url],
            |r| r.get(0),
        )?)
    }

    pub fn remove_bookmark(&self, url: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM bookmarks WHERE url = ?1", params![url])?;
        if n > 0 {
            self.touch(KIND_BOOKMARKS, url, true)?;
        }
        Ok(n > 0)
    }

    pub fn is_bookmarked(&self, url: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM bookmarks WHERE url = ?1",
                params![url],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Bookmark `url` if it is not bookmarked, otherwise remove it. Returns the new state.
    pub fn toggle_bookmark(&self, url: &str, title: &str, at_ms: i64) -> Result<bool> {
        if self.remove_bookmark(url)? {
            Ok(false)
        } else {
            self.add_bookmark(url, title, at_ms)?;
            Ok(true)
        }
    }

    /// Newest first.
    pub fn bookmarks(&self) -> Result<Vec<Bookmark>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, url, title, created_ms FROM bookmarks ORDER BY created_ms DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Bookmark {
                id: r.get(0)?,
                url: r.get(1)?,
                title: r.get(2)?,
                created_ms: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn history_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        url: r.get(0)?,
        title: r.get(1)?,
        visit_count: r.get::<_, i64>(2)? as u32,
        last_visit_ms: r.get(3)?,
        conv: r.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn visits_upsert_and_order() {
        let s = store();
        s.record_visit("https://a.example/", "A", 100).unwrap();
        s.record_visit("https://b.example/", "", 200).unwrap();
        s.record_visit("https://a.example/", "", 300).unwrap();
        let h = s.recent_history(10).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].url, "https://a.example/");
        assert_eq!(h[0].visit_count, 2);
        assert_eq!(h[0].title, "A", "empty title keeps the old one");
        assert_eq!(h[0].last_visit_ms, 300);
        assert_eq!(h[1].visit_count, 1);
        assert_eq!(s.recent_history(1).unwrap().len(), 1);
        // An out-of-order (older) timestamp never moves last_visit backwards.
        s.record_visit("https://a.example/", "", 50).unwrap();
        assert_eq!(
            s.history_entry("https://a.example/")
                .unwrap()
                .unwrap()
                .last_visit_ms,
            300
        );
    }

    #[test]
    fn titles_arrive_later() {
        let s = store();
        assert!(!s.set_title("https://x.example/", "X").unwrap());
        s.record_visit("https://x.example/", "", 1).unwrap();
        assert!(s.set_title("https://x.example/", "X").unwrap());
        assert!(!s.set_title("https://x.example/", "").unwrap());
        assert_eq!(
            s.history_entry("https://x.example/")
                .unwrap()
                .unwrap()
                .title,
            "X"
        );
    }

    #[test]
    fn search_is_case_insensitive_on_url_and_title() {
        let s = store();
        s.record_visit("https://servo.org/", "Servo, the embeddable engine", 1)
            .unwrap();
        s.record_visit("https://rust-lang.org/", "Rust", 2).unwrap();
        s.record_visit("https://example.com/100%_real", "", 3)
            .unwrap();
        let urls = |q: &str| -> Vec<String> {
            s.search_history(q, 10)
                .unwrap()
                .into_iter()
                .map(|e| e.url)
                .collect()
        };
        assert_eq!(urls("SERVO"), ["https://servo.org/"]);
        assert_eq!(urls("embeddable"), ["https://servo.org/"]);
        assert_eq!(
            urls("%_"),
            ["https://example.com/100%_real"],
            "no LIKE wildcards"
        );
        assert_eq!(urls("  ").len(), 3);
        assert!(urls("nothing-here").is_empty());
    }

    #[test]
    fn delete_clear_prune() {
        let s = store();
        for i in 0..10 {
            s.record_visit(&format!("https://e{i}.example/"), "", i)
                .unwrap();
        }
        assert!(s.delete_history_url("https://e0.example/").unwrap());
        assert!(!s.delete_history_url("https://e0.example/").unwrap());
        assert_eq!(s.prune_history(5).unwrap(), 4);
        let h = s.recent_history(100).unwrap();
        assert_eq!(h.len(), 5);
        assert_eq!(h[0].url, "https://e9.example/");
        assert_eq!(h[4].url, "https://e5.example/");
        assert_eq!(s.clear_history().unwrap(), 5);
        assert_eq!(s.history_len().unwrap(), 0);
    }

    #[test]
    fn bookmarks_add_toggle_remove() {
        let s = store();
        let id = s.add_bookmark("https://servo.org/", "Servo", 10).unwrap();
        assert_eq!(
            s.add_bookmark("https://servo.org/", "", 11).unwrap(),
            id,
            "upsert"
        );
        assert!(s.is_bookmarked("https://servo.org/").unwrap());
        assert!(
            s.toggle_bookmark("https://rust-lang.org/", "Rust", 20)
                .unwrap()
        );
        let b = s.bookmarks().unwrap();
        assert_eq!(
            b.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(),
            ["Rust", "Servo"]
        );
        assert!(!s.toggle_bookmark("https://rust-lang.org/", "", 30).unwrap());
        assert!(!s.is_bookmarked("https://rust-lang.org/").unwrap());
        assert!(s.remove_bookmark("https://servo.org/").unwrap());
        assert!(s.bookmarks().unwrap().is_empty());
    }

    #[test]
    fn persists_on_disk_and_rejects_newer_schema() {
        let dir = std::env::temp_dir().join(format!("mb-storage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let s = Store::open_in_dir(&dir).unwrap();
            s.record_visit("https://servo.org/", "Servo", 1).unwrap();
            s.add_bookmark("https://servo.org/", "Servo", 1).unwrap();
        }
        {
            let s = Store::open_in_dir(&dir).unwrap();
            assert_eq!(s.history_len().unwrap(), 1);
            assert!(s.is_bookmarked("https://servo.org/").unwrap());
            s.conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        }
        assert!(matches!(
            Store::open_in_dir(&dir),
            Err(StorageError::TooNew(99))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_gets_recorded() {
        let host = "mythbrowse.internal";
        assert!(should_record("https://servo.org/x", host));
        assert!(should_record("http://localhost:8080/", host));
        assert!(should_record("file:///tmp/a.html", host));
        assert!(!should_record(
            "https://mythbrowse.internal/home.html",
            host
        ));
        assert!(!should_record("https://MythBrowse.Internal:443/", host));
        assert!(!should_record("about:blank", host));
        assert!(!should_record("data:text/html,hi", host));
        assert!(!should_record("https:///nohost", host));
        assert!(should_record("https://user@servo.org/", host));
    }

    #[test]
    fn profile_dir_override() {
        // Only the explicit override is asserted; platform defaults depend on the machine.
        // SAFETY: tests in this crate do not read this variable concurrently.
        unsafe { std::env::set_var("MYTHBROWSE_PROFILE_DIR", "/tmp/mb-profile") };
        assert_eq!(
            default_profile_dir(),
            Some(PathBuf::from("/tmp/mb-profile"))
        );
        unsafe { std::env::remove_var("MYTHBROWSE_PROFILE_DIR") };
    }

    #[test]
    fn open_and_official_builds_use_different_profile_folders() {
        if OFFICIAL_BUILD {
            assert_eq!(
                (PROFILE_DIR_NAME, PROFILE_DIR_NAME_XDG),
                ("MythBrowse", "mythbrowse")
            );
        } else {
            assert_eq!(
                (PROFILE_DIR_NAME, PROFILE_DIR_NAME_XDG),
                ("MythBrowse Open Build", "mythbrowse-open")
            );
        }
    }

    /// One linear schema chain: a profile left at any older version (as that build wrote
    /// it) opens at [`SCHEMA_VERSION`] with its data kept and every newer table usable.
    #[test]
    fn upgrades_from_every_older_schema_version() {
        // What each version added on top of the one before (dropped to rebuild that version).
        let added: &[(i64, &str)] = &[
            (5, "DROP TABLE site_permissions;"),
            (4, "DROP TABLE downloads; DROP TABLE session;"),
            (
                3,
                "DROP TABLE messages; DROP TABLE conversations; DROP TABLE prefs;
                 ALTER TABLE history DROP COLUMN conv;",
            ),
            (2, "DROP TABLE sync_state; DROP TABLE sync_meta;"),
        ];
        for from in 1..SCHEMA_VERSION {
            let p = std::env::temp_dir().join(format!(
                "mb-schema-chain-v{from}-{}.sqlite3",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&p);
            {
                let s = Store::open(&p).unwrap();
                s.record_visit("https://keep.test/", "Keep", 5).unwrap();
                s.add_bookmark("https://mark.test/", "Mark", 6).unwrap();
                for (v, sql) in added {
                    if *v > from {
                        s.conn.execute_batch(sql).unwrap();
                    }
                }
                s.conn
                    .execute_batch(&format!("PRAGMA user_version = {from};"))
                    .unwrap();
            }
            let s = Store::open(&p).unwrap();
            let v: i64 = s
                .conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION, "from v{from}");
            assert_eq!(
                s.recent_history(10).unwrap()[0].url,
                "https://keep.test/",
                "history kept from v{from}"
            );
            assert!(s.is_bookmarked("https://mark.test/").unwrap(), "v{from}");
            for table in [
                "sync_state",
                "conversations",
                "messages",
                "prefs",
                "downloads",
                "session",
                "site_permissions",
            ] {
                let n: i64 = s
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                        [table],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(n, 1, "{table} after upgrading from v{from}");
            }
            assert!(
                s.set_site_permission(
                    "https://a.test",
                    "notifications",
                    Some(site_settings::Decision::Block),
                    7
                )
                .unwrap()
            );
            assert_eq!(s.site_permissions().unwrap().len(), 1, "v{from}");
            s.save_session("{}", true, 8).unwrap();
            drop(s);
            // Opening again at the current version changes nothing.
            let s = Store::open(&p).unwrap();
            assert_eq!(s.site_permissions().unwrap().len(), 1);
            drop(s);
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn refuses_a_profile_from_a_newer_build() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
            .unwrap();
        assert!(matches!(
            Store::init(c),
            Err(StorageError::TooNew(v)) if v == SCHEMA_VERSION + 1
        ));
    }
}
