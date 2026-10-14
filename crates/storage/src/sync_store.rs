// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The profile as a sync [`LocalStore`]: history, bookmarks and saved chats, with versions and
//! tombstones from the `sync_state` table. Chats can be turned off on their own
//! ([`Store::set_sync_chats`]): their changes then stay pending and incoming chat rows are
//! skipped.

use mythbrowse_sync::SyncKind;
use mythbrowse_sync::engine::{Cursor, LocalStore, Record};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::chats::{MAX_MESSAGE_BYTES, MAX_TOOL_CALLS_BYTES, clip_bytes, is_chat_id};
use crate::{KIND_BOOKMARKS, KIND_CHATS, KIND_HISTORY, Result, Store, unix_now_ms};

/// Kinds this store syncs today.
pub const SYNCED: [SyncKind; 3] = [SyncKind::Bookmarks, SyncKind::History, SyncKind::Chats];

fn kind_name(kind: SyncKind) -> Option<&'static str> {
    match kind {
        SyncKind::History => Some(KIND_HISTORY),
        SyncKind::Bookmarks => Some(KIND_BOOKMARKS),
        SyncKind::Chats => Some(KIND_CHATS),
        _ => None,
    }
}

/// A chats item id: `<conv>` (metadata) or `<conv>/<seq>` (one message).
fn chat_item(id: &str) -> Option<(&str, Option<i64>)> {
    match id.split_once('/') {
        None => is_chat_id(id).then_some((id, None)),
        Some((conv, seq)) => {
            let seq = seq.parse::<i64>().ok().filter(|s| *s >= 0)?;
            is_chat_id(conv).then_some((conv, Some(seq)))
        }
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Upload counts for the sync panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct SyncCounts {
    pub pending: usize,
    pub tracked: usize,
}

impl Store {
    fn item_data(&self, kind: &str, id: &str) -> Result<Option<Value>> {
        Ok(match kind {
            KIND_HISTORY => self
                .conn
                .query_row(
                    "SELECT title, visit_count, last_visit_ms, conv FROM history WHERE url = ?1",
                    params![id],
                    |r| {
                        let mut v = json!({
                            "title": r.get::<_, String>(0)?,
                            "visits": r.get::<_, i64>(1)?,
                            "last": r.get::<_, i64>(2)?,
                        });
                        if let Some(c) = r.get::<_, Option<String>>(3)? {
                            v["conv"] = json!(c);
                        }
                        Ok(v)
                    },
                )
                .optional()?,
            KIND_BOOKMARKS => self
                .conn
                .query_row(
                    "SELECT title, created_ms FROM bookmarks WHERE url = ?1",
                    params![id],
                    |r| {
                        Ok(json!({
                            "title": r.get::<_, String>(0)?,
                            "created": r.get::<_, i64>(1)?,
                        }))
                    },
                )
                .optional()?,
            KIND_CHATS => match chat_item(id) {
                Some((conv, None)) => self
                    .conn
                    .query_row(
                        "SELECT title, model, created_ms, updated_ms, pinned, title_auto
                         FROM conversations WHERE id = ?1 AND deleted = 0",
                        params![conv],
                        |r| {
                            Ok(json!({
                                "title": r.get::<_, String>(0)?,
                                "model": r.get::<_, String>(1)?,
                                "created": r.get::<_, i64>(2)?,
                                "updated": r.get::<_, i64>(3)?,
                                "pinned": r.get::<_, bool>(4)?,
                                "auto": r.get::<_, bool>(5)?,
                            }))
                        },
                    )
                    .optional()?,
                Some((conv, Some(seq))) => self
                    .conn
                    .query_row(
                        "SELECT role, content, tool_calls, tool_call_id, name, created_ms
                         FROM messages WHERE conv = ?1 AND seq = ?2",
                        params![conv, seq],
                        |r| {
                            Ok(json!({
                                "role": r.get::<_, String>(0)?,
                                "content": r.get::<_, String>(1)?,
                                "calls": r.get::<_, Option<String>>(2)?,
                                "call_id": r.get::<_, Option<String>>(3)?,
                                "name": r.get::<_, Option<String>>(4)?,
                                "at": r.get::<_, i64>(5)?,
                            }))
                        },
                    )
                    .optional()?,
                None => None,
            },
            _ => None,
        })
    }

    /// Write a history / bookmark record that came from another device.
    fn apply_remote_page(
        tx: &rusqlite::Transaction<'_>,
        name: &str,
        rec: &Record,
    ) -> std::result::Result<(), String> {
        let title = rec.data.get("title").and_then(Value::as_str).unwrap_or("");
        let conv = rec
            .data
            .get("conv")
            .and_then(Value::as_str)
            .filter(|c| is_chat_id(c));
        match (name, rec.deleted) {
            (KIND_HISTORY, false) => tx.execute(
                "INSERT INTO history (url, title, visit_count, last_visit_ms, conv) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(url) DO UPDATE SET title = excluded.title,
                    visit_count = excluded.visit_count, last_visit_ms = excluded.last_visit_ms,
                    conv = excluded.conv",
                params![rec.id, title, i64_of(&rec.data, "visits").max(1), i64_of(&rec.data, "last"), conv],
            ),
            (KIND_BOOKMARKS, false) => tx.execute(
                "INSERT INTO bookmarks (url, title, created_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(url) DO UPDATE SET title = excluded.title, created_ms = excluded.created_ms",
                params![rec.id, title, i64_of(&rec.data, "created")],
            ),
            (KIND_HISTORY, true) => tx.execute("DELETE FROM history WHERE url = ?1", params![rec.id]),
            (_, true) => tx.execute("DELETE FROM bookmarks WHERE url = ?1", params![rec.id]),
            _ => Ok(0),
        }
        .map(|_| ())
        .map_err(err)
    }

    /// Write a chats record that came from another device (inside the caller's transaction).
    fn apply_remote_chat(
        tx: &rusqlite::Transaction<'_>,
        rec: &Record,
    ) -> std::result::Result<bool, String> {
        let Some((conv, seq)) = chat_item(&rec.id) else {
            return Ok(false); // not an id this build writes: ignore
        };
        let d = &rec.data;
        let text = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let opt = |k: &str| d.get(k).and_then(Value::as_str).map(str::to_owned);
        let flag = |k: &str| d.get(k).and_then(Value::as_bool).unwrap_or(false);
        let retention: Option<i64> = tx
            .query_row(
                "SELECT value FROM prefs WHERE key = 'chat_retention_days'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(err)?
            .and_then(|v| crate::chats::Retention::parse(&v))
            .and_then(|r| r.cutoff_ms(unix_now_ms()));
        match (seq, rec.deleted) {
            (None, false) => {
                let updated = i64_of(d, "updated");
                if retention.is_some_and(|cut| updated < cut) && !flag("pinned") {
                    return Ok(false); // older than this device keeps chats
                }
                tx.execute(
                    "INSERT INTO conversations (id, title, model, created_ms, updated_ms, pinned, deleted, title_auto)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)
                     ON CONFLICT(id) DO UPDATE SET title = excluded.title, model = excluded.model,
                        created_ms = excluded.created_ms, updated_ms = excluded.updated_ms,
                        pinned = excluded.pinned, deleted = 0, title_auto = excluded.title_auto",
                    params![
                        conv,
                        clip_bytes(&text("title"), 300),
                        text("model"),
                        i64_of(d, "created"),
                        updated,
                        flag("pinned"),
                        d.get("auto").and_then(Value::as_bool).unwrap_or(true),
                    ],
                )
                .map_err(err)?;
            }
            (Some(seq), false) => {
                let at = i64_of(d, "at");
                let conv_state: Option<bool> = tx
                    .query_row(
                        "SELECT deleted FROM conversations WHERE id = ?1",
                        params![conv],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(err)?;
                match conv_state {
                    Some(true) => return Ok(false), // deleted here: stays deleted
                    None if retention.is_some_and(|cut| at < cut) => return Ok(false),
                    _ => {}
                }
                let calls = opt("calls").filter(|c| c.len() <= MAX_TOOL_CALLS_BYTES);
                tx.execute(
                    "INSERT INTO messages (conv, seq, role, content, tool_calls, tool_call_id, name, created_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(conv, seq) DO UPDATE SET role = excluded.role, content = excluded.content,
                        tool_calls = excluded.tool_calls, tool_call_id = excluded.tool_call_id,
                        name = excluded.name, created_ms = excluded.created_ms",
                    params![
                        conv,
                        seq,
                        text("role"),
                        clip_bytes(&text("content"), MAX_MESSAGE_BYTES),
                        calls,
                        opt("call_id"),
                        opt("name"),
                        at
                    ],
                )
                .map_err(err)?;
            }
            (None, true) => {
                tx.execute(
                    "UPDATE conversations SET deleted = 1, title = '', pinned = 0 WHERE id = ?1",
                    params![conv],
                )
                .map_err(err)?;
                tx.execute("DELETE FROM messages WHERE conv = ?1", params![conv])
                    .map_err(err)?;
                tx.execute(
                    "UPDATE history SET conv = NULL WHERE conv = ?1",
                    params![conv],
                )
                .map_err(err)?;
            }
            (Some(seq), true) => {
                tx.execute(
                    "DELETE FROM messages WHERE conv = ?1 AND seq = ?2",
                    params![conv, seq],
                )
                .map_err(err)?;
            }
        }
        Ok(true)
    }

    fn record(&self, kind: SyncKind, id: &str) -> Result<Option<Record>> {
        let Some(name) = kind_name(kind) else {
            return Ok(None);
        };
        let state: Option<(i64, bool)> = self
            .conn
            .query_row(
                "SELECT m, deleted FROM sync_state WHERE kind = ?1 AND id = ?2",
                params![name, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((m, deleted)) = state else {
            return Ok(None);
        };
        if deleted {
            return Ok(Some(Record::tombstone(kind, id, m)));
        }
        Ok(self
            .item_data(name, id)?
            .map(|data| Record::new(kind, id, m, data)))
    }

    /// Start over with a (new) sync account: everything is uploaded again and the next pull
    /// downloads everything.
    pub fn reset_sync(&self) -> Result<()> {
        self.conn.execute_batch(
            "UPDATE sync_state SET dirty = 1;
             DELETE FROM sync_meta WHERE key = 'cursor';",
        )?;
        Ok(())
    }

    pub fn sync_counts(&self) -> Result<SyncCounts> {
        let (p, t): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(dirty), 0), COUNT(*) FROM sync_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(SyncCounts {
            pending: p as usize,
            tracked: t as usize,
        })
    }
}

fn i64_of(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

impl LocalStore for Store {
    fn get(&self, kind: SyncKind, id: &str) -> std::result::Result<Option<Record>, String> {
        self.record(kind, id).map_err(err)
    }

    fn apply_remote(&mut self, rec: &Record) -> std::result::Result<(), String> {
        let Some(name) = kind_name(rec.kind) else {
            return Ok(()); // a kind this build does not keep yet
        };
        if name == KIND_CHATS && !self.sync_chats_enabled().map_err(err)? {
            return Ok(()); // chat sync is off on this device (pulled again when turned on)
        }
        let tx = self.conn.unchecked_transaction().map_err(err)?;
        if name == KIND_CHATS {
            if !Self::apply_remote_chat(&tx, rec)? {
                return tx.commit().map_err(err);
            }
        } else {
            Self::apply_remote_page(&tx, name, rec)?;
        }
        tx.execute(
            "INSERT INTO sync_state (kind, id, m, deleted, dirty) VALUES (?1, ?2, ?3, ?4, 0)
             ON CONFLICT(kind, id) DO UPDATE SET m = excluded.m, deleted = excluded.deleted, dirty = 0",
            params![name, rec.id, rec.modified_ms, rec.deleted],
        )
        .map_err(err)?;
        tx.commit().map_err(err)
    }

    fn pending(&self) -> std::result::Result<Vec<Record>, String> {
        let chats_on = self.sync_chats_enabled().map_err(err)?;
        let ids: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT kind, id FROM sync_state WHERE dirty = 1")
                .map_err(err)?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(err)?
                .collect::<rusqlite::Result<_>>()
                .map_err(err)?
        };
        let mut out = Vec::with_capacity(ids.len());
        for (k, id) in ids {
            let Some(kind) = SyncKind::from_name(&k) else {
                continue;
            };
            if kind == SyncKind::Chats && !chats_on {
                continue; // stays pending until chat sync is turned on again
            }
            if let Some(rec) = self.record(kind, &id).map_err(err)? {
                out.push(rec);
            }
        }
        Ok(out)
    }

    fn mark_synced(&mut self, done: &[(SyncKind, String, i64)]) -> std::result::Result<(), String> {
        let tx = self.conn.unchecked_transaction().map_err(err)?;
        for (kind, id, m) in done {
            if let Some(name) = kind_name(*kind) {
                tx.execute(
                    "UPDATE sync_state SET dirty = 0 WHERE kind = ?1 AND id = ?2 AND m = ?3",
                    params![name, id, m],
                )
                .map_err(err)?;
            }
        }
        tx.commit().map_err(err)
    }

    fn cursor(&self) -> std::result::Result<Cursor, String> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM sync_meta WHERE key = 'cursor'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        Ok(raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    fn reset_sync(&mut self) -> std::result::Result<(), String> {
        Store::reset_sync(self).map_err(err)
    }

    fn set_cursor(&mut self, c: &Cursor) -> std::result::Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO sync_meta (key, value) VALUES ('cursor', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![serde_json::to_string(c).map_err(err)?],
            )
            .map(|_| ())
            .map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythbrowse_sync::engine::memory::MemoryRemote;
    use mythbrowse_sync::{SyncKey, sync};

    #[test]
    fn local_changes_are_tracked() {
        let s = Store::open_in_memory().unwrap();
        s.record_visit("https://a/", "A", 10).unwrap();
        s.add_bookmark("https://b/", "B", 11).unwrap();
        assert_eq!(
            s.sync_counts().unwrap(),
            SyncCounts {
                pending: 2,
                tracked: 2
            }
        );
        s.remove_bookmark("https://b/").unwrap();
        let p = s.pending().unwrap();
        assert!(p.iter().any(|r| r.id == "https://b/" && r.deleted));
        assert!(
            p.iter()
                .any(|r| r.id == "https://a/" && r.data["title"] == "A")
        );
    }

    #[test]
    fn two_profiles_sync_history_and_bookmarks() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let mut a = Store::open_in_memory().unwrap();
        let mut b = Store::open_in_memory().unwrap();
        a.record_visit("https://a/", "A", 10).unwrap();
        a.add_bookmark("https://bm/", "Bm", 11).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!(a.sync_counts().unwrap().pending, 0);
        let r = sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!(r.applied, 2);
        assert!(b.is_bookmarked("https://bm/").unwrap());
        assert_eq!(b.history_entry("https://a/").unwrap().unwrap().title, "A");
        // Nothing echoed back as a local change.
        assert_eq!(b.sync_counts().unwrap().pending, 0);

        // Delete on B, reaches A.
        std::thread::sleep(std::time::Duration::from_millis(2));
        b.remove_bookmark("https://bm/").unwrap();
        b.clear_history().unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        assert!(!a.is_bookmarked("https://bm/").unwrap());
        assert_eq!(a.history_len().unwrap(), 0);

        // Re-adding after a deletion wins (newer version).
        std::thread::sleep(std::time::Duration::from_millis(2));
        a.add_bookmark("https://bm/", "Again", 20).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        assert!(b.is_bookmarked("https://bm/").unwrap());
    }

    #[test]
    fn cursor_persists_and_reset_reuploads() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let mut a = Store::open_in_memory().unwrap();
        a.record_visit("https://a/", "A", 10).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        // The next sync pulls our own upload and moves the cursor past it.
        let r = sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!(r.applied, 0);
        assert!(a.cursor().unwrap().since.is_some());
        a.reset_sync().unwrap();
        assert_eq!(a.cursor().unwrap(), Cursor::default());
        assert_eq!(a.sync_counts().unwrap().pending, 1);
    }

    #[test]
    fn prune_drops_bookkeeping_but_not_as_deletion() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..5 {
            s.record_visit(&format!("https://e/{i}"), "", i).unwrap();
        }
        s.prune_history(2).unwrap();
        assert_eq!(s.sync_counts().unwrap().tracked, 2);
        assert!(s.pending().unwrap().iter().all(|r| !r.deleted));
    }

    fn chat_msg(seq: i64, role: &str, content: &str) -> crate::chats::StoredMessage {
        crate::chats::StoredMessage {
            seq,
            role: role.into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            created_ms: unix_now_ms(),
        }
    }

    #[test]
    fn chats_round_trip_rename_pin_and_delete() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let mut a = Store::open_in_memory().unwrap();
        let mut b = Store::open_in_memory().unwrap();
        let id = crate::chats::new_chat_id();
        a.create_chat(&id, "Tea trails", "qwen3:8b", unix_now_ms())
            .unwrap();
        a.append_chat_message(
            &id,
            &chat_msg(0, "user", "Tea trails near Brinchang?"),
            None,
        )
        .unwrap();
        let mut reply = chat_msg(1, "assistant", "");
        reply.tool_calls = Some(
            r#"[{"id":"c1","name":"open_tab","arguments":"{\"url\":\"https://x/\"}"}]"#.into(),
        );
        a.append_chat_message(&id, &reply, None).unwrap();
        let mut tool = chat_msg(2, "tool", "Opened https://x/");
        tool.tool_call_id = Some("c1".into());
        tool.name = Some("open_tab".into());
        a.append_chat_message(&id, &tool, None).unwrap();
        a.record_visit("https://x/", "X", unix_now_ms()).unwrap();
        a.link_history_conv("https://x/", &id).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!(a.sync_counts().unwrap().pending, 0);

        let r = sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!(r.applied, 5, "meta + 3 messages + 1 history row");
        assert_eq!(b.chat(&id).unwrap().unwrap().title, "Tea trails");
        let msgs = b.chat_messages(&id).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1].tool_calls, reply.tool_calls);
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(
            b.history_entry("https://x/").unwrap().unwrap().conv,
            Some(id.clone())
        );
        assert_eq!(b.sync_counts().unwrap().pending, 0, "nothing echoed back");

        // Rename + pin on B reach A.
        std::thread::sleep(std::time::Duration::from_millis(2));
        b.rename_chat(&id, "Cameron Highlands").unwrap();
        b.pin_chat(&id, true).unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        let meta = a.chat(&id).unwrap().unwrap();
        assert_eq!(
            (meta.title.as_str(), meta.pinned, meta.title_auto),
            ("Cameron Highlands", true, false)
        );

        // Delete on A: tombstones for the chat and every message reach B.
        std::thread::sleep(std::time::Duration::from_millis(2));
        a.delete_chat(&id).unwrap();
        let p = a.pending().unwrap();
        assert_eq!(
            p.iter()
                .filter(|r| r.deleted && r.kind == SyncKind::Chats)
                .count(),
            4
        );
        sync(&keys, &mut a, &mut server).unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        assert!(b.chat(&id).unwrap().is_none());
        assert!(b.chat_messages(&id).unwrap().is_empty());
        assert_eq!(b.chats_len().unwrap(), 0);
    }

    #[test]
    fn chat_sync_can_be_turned_off_separately() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let mut a = Store::open_in_memory().unwrap();
        let mut b = Store::open_in_memory().unwrap();
        a.set_sync_chats(false).unwrap();
        assert!(!a.sync_chats_enabled().unwrap());
        let id = crate::chats::new_chat_id();
        a.create_chat(&id, "Private-ish", "", unix_now_ms())
            .unwrap();
        a.add_bookmark("https://bm/", "Bm", 1).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        assert_eq!(a.sync_counts().unwrap().pending, 1, "the chat waits");
        sync(&keys, &mut b, &mut server).unwrap();
        assert!(b.is_bookmarked("https://bm/").unwrap());
        assert_eq!(b.chats_len().unwrap(), 0);

        // B (chats off) skips incoming chats; turning it on pulls them again.
        b.set_sync_chats(false).unwrap();
        a.set_sync_chats(true).unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!(b.chats_len().unwrap(), 0);
        b.set_sync_chats(true).unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        assert_eq!(b.chats_len().unwrap(), 1);
    }

    #[test]
    fn largest_chat_message_fits_the_row_budget() {
        // 100 rows per page must stay under the client's 8 MB response cap.
        let keys = SyncKey::generate().unwrap().record_keys();
        let s = Store::open_in_memory().unwrap();
        let id = crate::chats::new_chat_id();
        s.create_chat(&id, &"t".repeat(400), "m", 1).unwrap();
        let mut m = chat_msg(0, "assistant", &"\u{1F600}".repeat(MAX_MESSAGE_BYTES));
        m.tool_calls = Some(format!(
            "[{}]",
            "\"a\",".repeat(MAX_TOOL_CALLS_BYTES / 4 - 2) + "\"a\""
        ));
        s.append_chat_message(&id, &m, None).unwrap();
        let rec = s
            .record(SyncKind::Chats, &format!("{id}/0"))
            .unwrap()
            .unwrap();
        let (_, ct) = rec.seal(&keys).unwrap();
        assert!(ct.len() < 72 * 1024, "sealed row is {} bytes", ct.len());
    }

    #[test]
    fn retention_skips_old_incoming_chats() {
        let keys = SyncKey::generate().unwrap().record_keys();
        let mut server = MemoryRemote::default();
        let mut a = Store::open_in_memory().unwrap();
        let mut b = Store::open_in_memory().unwrap();
        let old = unix_now_ms() - 60 * 24 * 3600 * 1000;
        a.create_chat(&crate::chats::new_chat_id(), "old", "", old)
            .unwrap();
        a.create_chat(&crate::chats::new_chat_id(), "new", "", unix_now_ms())
            .unwrap();
        sync(&keys, &mut a, &mut server).unwrap();
        b.set_chat_retention(crate::chats::Retention::Days(30))
            .unwrap();
        sync(&keys, &mut b, &mut server).unwrap();
        let titles: Vec<String> = b
            .list_chats("", 10)
            .unwrap()
            .into_iter()
            .map(|c| c.title)
            .collect();
        assert_eq!(titles, ["new"]);
    }
}
