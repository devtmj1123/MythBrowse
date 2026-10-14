// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Records, conflict resolution and the pull-then-push sync loop.
//!
//! The engine knows nothing about SQLite or HTTP: the browser's profile store implements
//! [`LocalStore`], the Appwrite client (or a test double) implements [`Remote`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SyncKind;
use crate::crypto::{CryptoError, RecordKeys};

/// One synced item in the clear (only ever on a device).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(rename = "k", with = "kind_name")]
    pub kind: SyncKind,
    /// Local id: the URL for history/bookmarks, a key name for settings, ...
    pub id: String,
    /// Last change on the device that made it (unix ms).
    #[serde(rename = "m")]
    pub modified_ms: i64,
    /// Tombstone: the item was deleted. Kept so the deletion reaches other devices.
    #[serde(rename = "d", default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

mod kind_name {
    use crate::SyncKind;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(k: &SyncKind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(k.collection_name())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SyncKind, D::Error> {
        let name = String::deserialize(d)?;
        SyncKind::from_name(&name).ok_or_else(|| D::Error::custom(format!("unknown kind {name}")))
    }
}

impl Record {
    pub fn new(kind: SyncKind, id: impl Into<String>, modified_ms: i64, data: Value) -> Self {
        Record {
            kind,
            id: id.into(),
            modified_ms,
            deleted: false,
            data,
        }
    }

    pub fn tombstone(kind: SyncKind, id: impl Into<String>, modified_ms: i64) -> Self {
        Record {
            kind,
            id: id.into(),
            modified_ms,
            deleted: true,
            data: Value::Null,
        }
    }

    pub fn row_id(&self, keys: &RecordKeys) -> String {
        keys.row_id(self.kind.collection_name(), &self.id)
    }

    pub fn seal(&self, keys: &RecordKeys) -> Result<(String, String), CryptoError> {
        let row = self.row_id(keys);
        let plain = serde_json::to_vec(self).expect("records always serialize");
        let ct = keys.encrypt(&row, &plain)?;
        Ok((row, ct))
    }

    /// Decrypt a row. Also checks the row id matches the record inside, so a buggy or hostile
    /// writer cannot make one row overwrite a different item.
    pub fn open(keys: &RecordKeys, row_id: &str, ct: &str) -> Result<Record, CryptoError> {
        let plain = keys.decrypt(row_id, ct)?;
        let rec: Record = serde_json::from_slice(&plain).map_err(|_| CryptoError::Format)?;
        if rec.row_id(keys) != row_id {
            return Err(CryptoError::Format);
        }
        Ok(rec)
    }

    /// Does `self` (incoming) win over `other` (what we have)? Last writer wins by device
    /// clock; ties go to the deletion, then to the larger body, so every device picks the same.
    pub fn beats(&self, other: &Record) -> bool {
        if self.modified_ms != other.modified_ms {
            return self.modified_ms > other.modified_ms;
        }
        if self.deleted != other.deleted {
            return self.deleted;
        }
        let a = serde_json::to_string(&self.data).unwrap_or_default();
        let b = serde_json::to_string(&other.data).unwrap_or_default();
        a > b
    }
}

/// A row as the server stores it: opaque id, ciphertext and the server's own change time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRow {
    pub row_id: String,
    pub ct: String,
    /// Server timestamp (ISO 8601) of the last write: drives the pull cursor, so device clocks
    /// never decide what gets downloaded.
    pub updated_at: String,
}

/// Where the last pull stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Newest server timestamp seen.
    pub since: Option<String>,
    /// Rows already seen at exactly `since` (the next pull asks `>= since` so a row written in
    /// the same millisecond is never missed).
    pub seen_at_since: Vec<String>,
}

/// Server side of sync.
pub trait Remote {
    /// Create or replace rows (row id, ciphertext).
    fn put_rows(&mut self, rows: &[(String, String)]) -> Result<(), String>;
    /// Rows with `updated_at >= since` (all when `None`), oldest first, ordered by
    /// (`updated_at`, `row_id`), at most `limit`, starting after `after_row` within that order.
    fn list_rows(
        &mut self,
        since: Option<&str>,
        after_row: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RemoteRow>, String>;
}

/// The device's profile store.
pub trait LocalStore {
    fn get(&self, kind: SyncKind, id: &str) -> Result<Option<Record>, String>;
    /// Write a record that came from another device (it won over the local version). Must not
    /// mark it as a local change, and replaces any pending local change to the same item.
    fn apply_remote(&mut self, rec: &Record) -> Result<(), String>;
    /// Local changes not yet uploaded (including tombstones).
    fn pending(&self) -> Result<Vec<Record>, String>;
    /// These versions are uploaded; clear them from `pending` unless changed again since.
    fn mark_synced(&mut self, done: &[(SyncKind, String, i64)]) -> Result<(), String>;
    fn cursor(&self) -> Result<Cursor, String>;
    fn set_cursor(&mut self, c: &Cursor) -> Result<(), String>;
    /// Start over (a key was just unlocked, maybe for another account): every item becomes a
    /// pending upload and the next pull starts from the beginning.
    fn reset_sync(&mut self) -> Result<(), String>;
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("local profile: {0}")]
    Local(String),
    #[error("sync server: {0}")]
    Remote(String),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Rows downloaded.
    pub pulled: usize,
    /// Downloaded records that changed this device.
    pub applied: usize,
    /// Local changes uploaded.
    pub pushed: usize,
    /// Local changes dropped because a newer version came from another device.
    pub superseded: usize,
    /// Rows this key cannot read (another key, damaged, or a newer format).
    pub unreadable: usize,
}

pub const PAGE: usize = 100;
/// Upload batch size.
pub const PUSH_BATCH: usize = 50;

/// One full sync: pull everything new, apply what wins, then push local changes that still win.
pub fn sync(
    keys: &RecordKeys,
    local: &mut dyn LocalStore,
    remote: &mut dyn Remote,
) -> Result<SyncReport, SyncError> {
    let mut report = SyncReport::default();
    let mut cursor = local.cursor().map_err(SyncError::Local)?;

    // Pull.
    let mut after: Option<String> = None;
    loop {
        let rows = remote
            .list_rows(cursor.since.as_deref(), after.as_deref(), PAGE)
            .map_err(SyncError::Remote)?;
        let n = rows.len();
        for row in rows {
            after = Some(row.row_id.clone());
            if cursor.since.as_deref() == Some(row.updated_at.as_str())
                && cursor.seen_at_since.contains(&row.row_id)
            {
                continue;
            }
            report.pulled += 1;
            match Record::open(keys, &row.row_id, &row.ct) {
                Ok(rec) => {
                    let mine = local.get(rec.kind, &rec.id).map_err(SyncError::Local)?;
                    let wins = match &mine {
                        Some(m) => rec.beats(m),
                        None => !rec.deleted,
                    };
                    if wins {
                        local.apply_remote(&rec).map_err(SyncError::Local)?;
                        report.applied += 1;
                    }
                }
                Err(e) => {
                    log::warn!("sync: skipping unreadable row: {e}");
                    report.unreadable += 1;
                }
            }
            if cursor.since.as_deref() == Some(row.updated_at.as_str()) {
                cursor.seen_at_since.push(row.row_id);
            } else {
                cursor.since = Some(row.updated_at);
                cursor.seen_at_since = vec![row.row_id];
            }
        }
        if n < PAGE {
            break;
        }
    }
    local.set_cursor(&cursor).map_err(SyncError::Local)?;

    // Push.
    let pending = local.pending().map_err(SyncError::Local)?;
    let mut done = Vec::new();
    let mut batch = Vec::new();
    for rec in pending {
        // A newer version may have just arrived from another device.
        let current = local.get(rec.kind, &rec.id).map_err(SyncError::Local)?;
        if let Some(cur) = &current {
            if cur != &rec && cur.beats(&rec) {
                report.superseded += 1;
                done.push((rec.kind, rec.id.clone(), rec.modified_ms));
                continue;
            }
        }
        batch.push(rec.seal(keys)?);
        done.push((rec.kind, rec.id.clone(), rec.modified_ms));
        if batch.len() == PUSH_BATCH {
            remote.put_rows(&batch).map_err(SyncError::Remote)?;
            report.pushed += batch.len();
            batch.clear();
        }
    }
    if !batch.is_empty() {
        remote.put_rows(&batch).map_err(SyncError::Remote)?;
        report.pushed += batch.len();
    }
    local.mark_synced(&done).map_err(SyncError::Local)?;
    Ok(report)
}

/// In-memory implementations, for tests here and in the browser crates.
pub mod memory {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Default)]
    pub struct MemoryStore {
        pub items: BTreeMap<(String, String), Record>,
        pub dirty: BTreeMap<(String, String), i64>,
        pub cursor: Cursor,
    }

    impl MemoryStore {
        fn key(kind: SyncKind, id: &str) -> (String, String) {
            (kind.collection_name().to_owned(), id.to_owned())
        }

        /// A change made on this device.
        pub fn local_write(&mut self, rec: Record) {
            let k = Self::key(rec.kind, &rec.id);
            self.dirty.insert(k.clone(), rec.modified_ms);
            self.items.insert(k, rec);
        }

        pub fn live(&self, kind: SyncKind, id: &str) -> Option<&Record> {
            self.items.get(&Self::key(kind, id)).filter(|r| !r.deleted)
        }
    }

    impl LocalStore for MemoryStore {
        fn get(&self, kind: SyncKind, id: &str) -> Result<Option<Record>, String> {
            Ok(self.items.get(&Self::key(kind, id)).cloned())
        }
        fn apply_remote(&mut self, rec: &Record) -> Result<(), String> {
            let k = Self::key(rec.kind, &rec.id);
            self.dirty.remove(&k);
            self.items.insert(k, rec.clone());
            Ok(())
        }
        fn pending(&self) -> Result<Vec<Record>, String> {
            Ok(self
                .dirty
                .keys()
                .filter_map(|k| self.items.get(k).cloned())
                .collect())
        }
        fn mark_synced(&mut self, done: &[(SyncKind, String, i64)]) -> Result<(), String> {
            for (kind, id, m) in done {
                let k = Self::key(*kind, id);
                if self.dirty.get(&k) == Some(m) {
                    self.dirty.remove(&k);
                }
            }
            Ok(())
        }
        fn cursor(&self) -> Result<Cursor, String> {
            Ok(self.cursor.clone())
        }
        fn set_cursor(&mut self, c: &Cursor) -> Result<(), String> {
            self.cursor = c.clone();
            Ok(())
        }
        fn reset_sync(&mut self) -> Result<(), String> {
            self.cursor = Cursor::default();
            self.dirty = self
                .items
                .iter()
                .map(|(k, r)| (k.clone(), r.modified_ms))
                .collect();
            Ok(())
        }
    }

    /// A server double that keeps rows ordered like Appwrite does: by write time, then id.
    #[derive(Debug, Default)]
    pub struct MemoryRemote {
        pub rows: BTreeMap<String, RemoteRow>,
        pub clock: u64,
        /// Writes per call, to check batching.
        pub put_calls: usize,
    }

    impl MemoryRemote {
        fn stamp(&mut self) -> String {
            self.clock += 1;
            format!("2026-10-09T00:00:00.{:03}+00:00", self.clock)
        }
        /// A hostile or buggy server swapping ciphertexts between rows.
        pub fn swap(&mut self, a: &str, b: &str) {
            let ca = self.rows[a].ct.clone();
            let cb = self.rows[b].ct.clone();
            self.rows.get_mut(a).unwrap().ct = cb;
            self.rows.get_mut(b).unwrap().ct = ca;
        }
    }

    impl Remote for MemoryRemote {
        fn put_rows(&mut self, rows: &[(String, String)]) -> Result<(), String> {
            self.put_calls += 1;
            for (id, ct) in rows {
                let t = self.stamp();
                self.rows.insert(
                    id.clone(),
                    RemoteRow {
                        row_id: id.clone(),
                        ct: ct.clone(),
                        updated_at: t,
                    },
                );
            }
            Ok(())
        }
        fn list_rows(
            &mut self,
            since: Option<&str>,
            after_row: Option<&str>,
            limit: usize,
        ) -> Result<Vec<RemoteRow>, String> {
            let mut all: Vec<RemoteRow> = self
                .rows
                .values()
                .filter(|r| since.is_none_or(|s| r.updated_at.as_str() >= s))
                .cloned()
                .collect();
            all.sort_by(|a, b| (&a.updated_at, &a.row_id).cmp(&(&b.updated_at, &b.row_id)));
            let start = match after_row {
                Some(id) => all
                    .iter()
                    .position(|r| r.row_id == id)
                    .map(|i| i + 1)
                    .unwrap_or(0),
                None => 0,
            };
            Ok(all.into_iter().skip(start).take(limit).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::*;
    use super::*;
    use crate::crypto::SyncKey;
    use serde_json::json;

    fn bm(url: &str, title: &str, m: i64) -> Record {
        Record::new(SyncKind::Bookmarks, url, m, json!({ "title": title }))
    }

    #[test]
    fn two_devices_converge() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let (mut a, mut b) = (MemoryStore::default(), MemoryStore::default());

        a.local_write(bm("https://a.example/", "A", 10));
        let r = sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!((r.pushed, r.pulled), (1, 0));
        assert!(a.dirty.is_empty());

        b.local_write(bm("https://b.example/", "B", 11));
        let r = sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!((r.pulled, r.applied, r.pushed), (1, 1, 1));
        assert!(b.live(SyncKind::Bookmarks, "https://a.example/").is_some());

        sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!(a.items, b.items);
        // Nothing new: a sync is a no-op (own rows already seen via the cursor).
        let r = sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!(r, SyncReport::default());
    }

    #[test]
    fn newer_edit_wins_and_older_local_change_is_superseded() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let (mut a, mut b) = (MemoryStore::default(), MemoryStore::default());
        a.local_write(bm("https://x/", "v1", 10));
        sync(&keys, &mut a, &mut server).unwrap();
        sync(&keys, &mut b, &mut server).unwrap();

        // Both edit offline; B's edit is newer.
        a.local_write(bm("https://x/", "from A", 20));
        b.local_write(bm("https://x/", "from B", 30));
        sync(&keys, &mut b, &mut server).unwrap();
        let r = sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!((r.applied, r.pushed), (1, 0));
        assert!(a.dirty.is_empty());
        assert_eq!(
            a.live(SyncKind::Bookmarks, "https://x/").unwrap().data["title"],
            "from B"
        );
        sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!(a.items, b.items);
    }

    #[test]
    fn deletions_propagate() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let (mut a, mut b) = (MemoryStore::default(), MemoryStore::default());
        a.local_write(bm("https://x/", "t", 10));
        sync(&keys, &mut a, &mut server).unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        b.local_write(Record::tombstone(SyncKind::Bookmarks, "https://x/", 20));
        sync(&keys, &mut b, &mut server).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        assert!(a.live(SyncKind::Bookmarks, "https://x/").is_none());
        // A fresh device does not materialise tombstones it never had.
        let mut c = MemoryStore::default();
        let r = sync(&keys, &mut c, &mut server).unwrap();
        assert_eq!((r.pulled, r.applied), (1, 0));
        assert!(c.items.is_empty());
    }

    #[test]
    fn paging_and_batching() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let mut a = MemoryStore::default();
        for i in 0..(PAGE * 2 + 7) {
            a.local_write(Record::new(
                SyncKind::History,
                format!("https://e/{i}"),
                i as i64,
                json!({}),
            ));
        }
        let r = sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!(r.pushed, PAGE * 2 + 7);
        assert_eq!(server.put_calls, (PAGE * 2 + 7).div_ceil(PUSH_BATCH));
        let mut b = MemoryStore::default();
        let r = sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!(r.applied, PAGE * 2 + 7);
        assert_eq!(a.items, b.items);
    }

    #[test]
    fn server_sees_no_plaintext_and_cannot_swap_rows() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let mut a = MemoryStore::default();
        a.local_write(bm("https://secret.example/path", "Secret title", 10));
        a.local_write(bm("https://other.example/", "Other", 11));
        sync(&keys, &mut a, &mut server).unwrap();
        for row in server.rows.values() {
            assert!(!row.ct.contains("secret") && !row.row_id.contains("secret"));
            assert!(!row.ct.contains("bookmarks"));
        }
        let ids: Vec<String> = server.rows.keys().cloned().collect();
        server.swap(&ids[0], &ids[1]);
        let mut b = MemoryStore::default();
        let r = sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!((r.applied, r.unreadable), (0, 2));
    }

    #[test]
    fn wrong_key_rows_are_skipped_not_fatal() {
        let mut server = MemoryRemote::default();
        let mut a = MemoryStore::default();
        a.local_write(bm("https://x/", "t", 1));
        sync(
            &SyncKey::generate().unwrap().record_keys(),
            &mut a,
            &mut server,
        )
        .unwrap();
        let mut b = MemoryStore::default();
        let r = sync(
            &SyncKey::generate().unwrap().record_keys(),
            &mut b,
            &mut server,
        )
        .unwrap();
        assert_eq!(r.unreadable, 1);
    }

    #[test]
    fn record_json_is_compact() {
        let r = bm("https://x/", "t", 5);
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"k":"bookmarks","id":"https://x/","m":5,"data":{"title":"t"}}"#
        );
        let t = Record::tombstone(SyncKind::History, "u", 6);
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"k":"history","id":"u","m":6,"d":true}"#
        );
    }
}
