// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Download history, the saved session and the "On startup" setting (schema v4).
//!
//! The rows are opaque JSON written by the shell (`mythbrowse_files::download::Item`,
//! `mythbrowse_files::session::Session`), so this crate does not depend on the files crate.
//! Local only: neither downloads nor the open tabs are synced. Private windows write
//! nothing here.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

/// v3 -> v4.
pub(crate) const SCHEMA_V4: &str = "
CREATE TABLE IF NOT EXISTS downloads (
    id         INTEGER PRIMARY KEY,
    state      TEXT NOT NULL,
    started_ms INTEGER NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS downloads_by_started ON downloads (started_ms DESC);
CREATE TABLE IF NOT EXISTS session (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    data     TEXT NOT NULL,
    clean    INTEGER NOT NULL DEFAULT 0,
    saved_ms INTEGER NOT NULL
);
";

const PREF_STARTUP: &str = "startup";
/// Download history keeps at most this many rows (newest).
pub const MAX_DOWNLOAD_ROWS: usize = 1000;

/// A stored download: the shell's JSON plus the columns used for sorting and cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRow {
    pub id: i64,
    pub state: String,
    pub started_ms: i64,
    pub data: String,
}

/// The saved session row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSession {
    pub data: String,
    /// Written by a normal exit (`false` = the browser crashed or was killed after it).
    pub clean: bool,
    pub saved_ms: i64,
}

impl Store {
    // -------------------------------------------------------------- downloads

    pub fn upsert_download(&self, row: &DownloadRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO downloads (id, state, started_ms, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state,
                started_ms = excluded.started_ms, data = excluded.data",
            params![row.id, row.state, row.started_ms, row.data],
        )?;
        Ok(())
    }

    /// Newest first.
    pub fn downloads(&self, limit: usize) -> Result<Vec<DownloadRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, state, started_ms, data FROM downloads ORDER BY started_ms DESC, id DESC LIMIT ?1",
        )?;
        let rows = st
            .query_map([limit as i64], |r| {
                Ok(DownloadRow {
                    id: r.get(0)?,
                    state: r.get(1)?,
                    started_ms: r.get(2)?,
                    data: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_download(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM downloads WHERE id = ?1", [id])?
            > 0)
    }

    /// Delete rows in any of `states` (e.g. done / failed / cancelled). Returns how many.
    pub fn clear_downloads_in(&self, states: &[&str]) -> Result<usize> {
        let mut n = 0;
        for s in states {
            n += self
                .conn
                .execute("DELETE FROM downloads WHERE state = ?1", [s])?;
        }
        Ok(n)
    }

    /// Keep only the newest `keep` rows.
    pub fn prune_downloads(&self, keep: usize) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM downloads WHERE id NOT IN
                (SELECT id FROM downloads ORDER BY started_ms DESC, id DESC LIMIT ?1)",
            [keep as i64],
        )?)
    }

    /// The largest id used so far (0 when empty): new downloads are numbered after it.
    pub fn max_download_id(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM downloads", [], |r| {
                r.get(0)
            })?)
    }

    // ---------------------------------------------------------------- session

    /// Save the window. `clean` is true only for the save made on a normal exit.
    pub fn save_session(&self, data: &str, clean: bool, at_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session (id, data, clean, saved_ms) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data, clean = excluded.clean,
                saved_ms = excluded.saved_ms",
            params![data, clean, at_ms],
        )?;
        Ok(())
    }

    pub fn load_session(&self) -> Result<Option<SavedSession>> {
        Ok(self
            .conn
            .query_row(
                "SELECT data, clean, saved_ms FROM session WHERE id = 1",
                [],
                |r| {
                    Ok(SavedSession {
                        data: r.get(0)?,
                        clean: r.get(1)?,
                        saved_ms: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Mark the stored session as from a running browser (`false`, right after startup) or a
    /// normal exit (`true`) without rewriting it.
    pub fn mark_session_clean(&self, clean: bool) -> Result<()> {
        self.conn
            .execute("UPDATE session SET clean = ?1 WHERE id = 1", [clean])?;
        Ok(())
    }

    pub fn clear_session(&self) -> Result<()> {
        self.conn.execute("DELETE FROM session", [])?;
        Ok(())
    }

    // ---------------------------------------------------------------- startup

    /// "On startup": `continue` (default) or `newtab`.
    pub fn startup_pref(&self) -> Result<String> {
        Ok(match self.pref(PREF_STARTUP)?.as_deref() {
            Some("newtab") => "newtab".into(),
            _ => "continue".into(),
        })
    }

    /// Unknown values are refused (returns false).
    pub fn set_startup_pref(&self, mode: &str) -> Result<bool> {
        if !matches!(mode, "continue" | "newtab") {
            return Ok(false);
        }
        self.set_pref(PREF_STARTUP, mode)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::{SCHEMA_VERSION, Store};
    use rusqlite::Connection;

    fn row(id: i64, state: &str, at: i64) -> super::DownloadRow {
        super::DownloadRow {
            id,
            state: state.into(),
            started_ms: at,
            data: format!("{{\"id\":{id}}}"),
        }
    }

    #[test]
    fn downloads_round_trip_prune_and_clear() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.max_download_id().unwrap(), 0);
        s.upsert_download(&row(1, "done", 10)).unwrap();
        s.upsert_download(&row(2, "running", 20)).unwrap();
        s.upsert_download(&row(3, "failed", 30)).unwrap();
        s.upsert_download(&row(2, "paused", 20)).unwrap();
        let l = s.downloads(10).unwrap();
        assert_eq!(l.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert_eq!(l[1].state, "paused");
        assert_eq!(s.max_download_id().unwrap(), 3);
        assert_eq!(s.prune_downloads(2).unwrap(), 1);
        assert_eq!(s.downloads(10).unwrap().len(), 2);
        assert_eq!(
            s.clear_downloads_in(&["done", "failed", "cancelled"])
                .unwrap(),
            1
        );
        assert!(s.delete_download(2).unwrap());
        assert!(!s.delete_download(2).unwrap());
    }

    #[test]
    fn session_and_startup_pref() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.load_session().unwrap().is_none());
        s.save_session("{\"tabs\":[]}", false, 5).unwrap();
        let got = s.load_session().unwrap().unwrap();
        assert!(!got.clean);
        assert_eq!(got.saved_ms, 5);
        s.mark_session_clean(true).unwrap();
        assert!(s.load_session().unwrap().unwrap().clean);
        s.save_session("{\"tabs\":[1]}", false, 6).unwrap();
        let got = s.load_session().unwrap().unwrap();
        assert_eq!((got.data.as_str(), got.clean), ("{\"tabs\":[1]}", false));
        s.clear_session().unwrap();
        assert!(s.load_session().unwrap().is_none());

        assert_eq!(s.startup_pref().unwrap(), "continue");
        assert!(s.set_startup_pref("newtab").unwrap());
        assert_eq!(s.startup_pref().unwrap(), "newtab");
        assert!(!s.set_startup_pref("weird").unwrap());
        assert_eq!(s.startup_pref().unwrap(), "newtab");
    }

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mb-files-store-{tag}-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn migrates_v3_to_current() {
        let p = temp_db("v3");
        {
            // A v3 profile: what the previous build created.
            let s = Store::open(&p).unwrap();
            s.record_visit("https://a.test/", "A", 1).unwrap();
            s.conn
                .execute_batch("DROP TABLE downloads; DROP TABLE session; DROP TABLE site_permissions;\n                     PRAGMA user_version = 3;")
                .unwrap();
        }
        let s = Store::open(&p).unwrap();
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(s.recent_history(10).unwrap().len(), 1);
        s.upsert_download(&row(1, "done", 1)).unwrap();
        s.save_session("{}", true, 1).unwrap();
        drop(s);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn migrates_v1_to_current() {
        let p = temp_db("v1");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE history (url TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '',
                    visit_count INTEGER NOT NULL DEFAULT 1, last_visit_ms INTEGER NOT NULL);
                 CREATE TABLE bookmarks (id INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL UNIQUE,
                    title TEXT NOT NULL DEFAULT '', created_ms INTEGER NOT NULL);
                 INSERT INTO history (url, title, visit_count, last_visit_ms) VALUES ('https://old.test/', 'Old', 2, 7);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let s = Store::open(&p).unwrap();
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(s.recent_history(10).unwrap()[0].url, "https://old.test/");
        assert_eq!(s.downloads(10).unwrap().len(), 0);
        assert_eq!(s.startup_pref().unwrap(), "continue");
        drop(s);
        let _ = std::fs::remove_file(&p);
    }
}
