// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Saved Myth conversations (schema v3): one `conversations` row per chat and its messages in
//! `messages`, plus the chat preferences (retention, sync on/off) in `prefs`.
//!
//! A conversation's id is a random 128-bit hex string, stable across devices, so a synced chat
//! is the same chat everywhere. What is kept is what the model needs to continue: the user's
//! and Myth's text, the tool calls Myth made and a short summary of each tool's output (never a
//! screenshot or an image; see [`summarize_tool_output`]).
//!
//! Sync (kind `chats`, see `sync_store`): the conversation's metadata is item `<id>` and each
//! message is item `<id>/<seq>`, so a new message uploads one small row and a long chat never
//! becomes one huge row. Deleting a chat leaves tombstones for the metadata and every message.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{KIND_CHATS, Result, Store, unix_now_ms};

/// Longest stored message text (bytes; the same as the longest message the chat box sends).
/// With [`MAX_TOOL_CALLS_BYTES`] it keeps every synced message row under ~72 KB of
/// ciphertext, so a sync page (100 rows) stays well under the client's 8 MB response cap.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
/// Longest stored tool-calls JSON of one message (bytes).
pub const MAX_TOOL_CALLS_BYTES: usize = 8 * 1024;
/// Longest stored tool output summary (bytes).
pub const MAX_TOOL_SUMMARY_BYTES: usize = 2 * 1024;
const TRUNCATED: &str = "\u{2026} [truncated]";

const PREF_RETENTION: &str = "chat_retention_days";
const PREF_SYNC_CHATS: &str = "sync_chats";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How long chats are kept (pinned chats are always kept).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Retention {
    Forever,
    Days(u32),
}

impl Retention {
    pub fn parse(s: &str) -> Option<Retention> {
        match s {
            "forever" | "" | "0" => Some(Retention::Forever),
            "30" => Some(Retention::Days(30)),
            "90" => Some(Retention::Days(90)),
            _ => None,
        }
    }

    pub fn as_str(self) -> String {
        match self {
            Retention::Forever => "forever".into(),
            Retention::Days(d) => d.to_string(),
        }
    }

    /// Chats last changed before this are pruned (`None` = keep forever).
    pub fn cutoff_ms(self, now_ms: i64) -> Option<i64> {
        match self {
            Retention::Forever => None,
            Retention::Days(d) => Some(now_ms - i64::from(d) * DAY_MS),
        }
    }
}

/// A conversation as listed in the sidebar / history page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMeta {
    pub id: String,
    pub title: String,
    pub model: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub pinned: bool,
    /// The title still comes from the first message (not renamed by the user).
    pub title_auto: bool,
}

/// One stored message. `role` is system / user / assistant / tool; `tool_calls` is the JSON
/// array of the assistant's tool calls (`[{id, name, arguments}]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredMessage {
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    pub created_ms: i64,
}

/// A fresh conversation id: 128 random bits as 32 lowercase hex digits.
pub fn new_chat_id() -> String {
    let mut b = [0u8; 16];
    if getrandom::getrandom(&mut b).is_err() {
        // No OS randomness (should not happen): time + a counter, still unique per device.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let t = unix_now_ms() as u64;
        b[..8].copy_from_slice(&t.to_be_bytes());
        b[8..].copy_from_slice(&N.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A stored conversation id (as made by [`new_chat_id`]; accepts 8..64 hex digits).
pub fn is_chat_id(s: &str) -> bool {
    (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Cut `s` to at most `max` bytes on a character boundary, marking the cut.
pub fn clip_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max.saturating_sub(TRUNCATED.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &s[..end])
}

/// What is kept of a tool's output: text only, at most [`MAX_TOOL_SUMMARY_BYTES`]. Image data
/// (data: URLs, long base64 runs) is replaced by a placeholder, never stored.
pub fn summarize_tool_output(out: &str) -> String {
    let mut text = String::with_capacity(out.len().min(MAX_TOOL_SUMMARY_BYTES + 64));
    let mut rest = out;
    while let Some(i) = rest.find("data:") {
        text.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
            .unwrap_or(tail.len());
        if tail[..end].contains(";base64,") || end > 256 {
            text.push_str("[image omitted]");
        } else {
            text.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
    text.push_str(rest);
    // Any remaining long unbroken run (raw base64 without a data: prefix).
    let cleaned: Vec<String> = text
        .split(' ')
        .map(|w| {
            if w.len() > 512
                && w.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
            {
                "[binary omitted]".to_owned()
            } else {
                w.to_owned()
            }
        })
        .collect();
    clip_bytes(cleaned.join(" ").trim(), MAX_TOOL_SUMMARY_BYTES)
}

/// A title from the first user message: first line, whitespace collapsed, at most ~60
/// characters cut at a word boundary.
pub fn title_from_message(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut out = String::new();
    for w in words {
        let next = if out.is_empty() {
            w.chars().count()
        } else {
            out.chars().count() + 1 + w.chars().count()
        };
        if next > 60 {
            if out.is_empty() {
                out = w.chars().take(59).collect();
            }
            out.push('\u{2026}');
            return out;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    if out.is_empty() {
        "New chat".to_owned()
    } else {
        out
    }
}

fn msg_item(conv: &str, seq: i64) -> String {
    format!("{conv}/{seq}")
}

fn meta_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMeta> {
    Ok(ChatMeta {
        id: r.get(0)?,
        title: r.get(1)?,
        model: r.get(2)?,
        created_ms: r.get(3)?,
        updated_ms: r.get(4)?,
        pinned: r.get(5)?,
        title_auto: r.get(6)?,
    })
}

const META_COLS: &str = "id, title, model, created_ms, updated_ms, pinned, title_auto";

impl Store {
    // ------------------------------------------------------------------ prefs

    pub fn pref(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM prefs WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_pref(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO prefs (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn chat_retention(&self) -> Result<Retention> {
        Ok(self
            .pref(PREF_RETENTION)?
            .and_then(|v| Retention::parse(&v))
            .unwrap_or(Retention::Forever))
    }

    pub fn set_chat_retention(&self, r: Retention) -> Result<()> {
        self.set_pref(PREF_RETENTION, &r.as_str())
    }

    /// Chats sync (when sync is on at all). Default on.
    pub fn sync_chats_enabled(&self) -> Result<bool> {
        Ok(self.pref(PREF_SYNC_CHATS)?.as_deref() != Some("0"))
    }

    /// Turn chat sync on / off. Turning it back on pulls everything again (chats skipped while
    /// it was off) and uploads what changed meanwhile (those stayed pending).
    pub fn set_sync_chats(&self, on: bool) -> Result<()> {
        let was = self.sync_chats_enabled()?;
        self.set_pref(PREF_SYNC_CHATS, if on { "1" } else { "0" })?;
        if on && !was {
            self.conn
                .execute("DELETE FROM sync_meta WHERE key = 'cursor'", [])?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ conversations

    /// Create a conversation (no-op if `id` exists and is not deleted).
    pub fn create_chat(&self, id: &str, title: &str, model: &str, at_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conversations (id, title, model, created_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, model = excluded.model,
                created_ms = excluded.created_ms, updated_ms = excluded.updated_ms,
                pinned = 0, deleted = 0, title_auto = 1
             WHERE deleted = 1",
            params![id, title, model, at_ms],
        )?;
        self.touch(KIND_CHATS, id, false)
    }

    pub fn chat(&self, id: &str) -> Result<Option<ChatMeta>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {META_COLS} FROM conversations WHERE id = ?1 AND deleted = 0"),
                params![id],
                meta_row,
            )
            .optional()?)
    }

    /// Append a message (its `seq` decides the order; re-appending a seq replaces it). Bumps
    /// the conversation's `updated_ms` and `model` when given.
    pub fn append_chat_message(
        &self,
        conv: &str,
        m: &StoredMessage,
        model: Option<&str>,
    ) -> Result<()> {
        let content = clip_bytes(&m.content, MAX_MESSAGE_BYTES);
        let calls = m
            .tool_calls
            .as_deref()
            .filter(|c| c.len() <= MAX_TOOL_CALLS_BYTES)
            .map(str::to_owned)
            .or_else(|| m.tool_calls.as_ref().map(|_| "[]".to_owned()));
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO messages (conv, seq, role, content, tool_calls, tool_call_id, name, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(conv, seq) DO UPDATE SET role = excluded.role, content = excluded.content,
                tool_calls = excluded.tool_calls, tool_call_id = excluded.tool_call_id,
                name = excluded.name, created_ms = excluded.created_ms",
            params![conv, m.seq, m.role, content, calls, m.tool_call_id, m.name, m.created_ms],
        )?;
        tx.execute(
            "UPDATE conversations SET updated_ms = MAX(updated_ms, ?2),
                model = COALESCE(?3, model) WHERE id = ?1",
            params![conv, m.created_ms, model],
        )?;
        tx.commit()?;
        self.touch(KIND_CHATS, &msg_item(conv, m.seq), false)?;
        self.touch(KIND_CHATS, conv, false)
    }

    /// All messages of a conversation, in order.
    pub fn chat_messages(&self, conv: &str) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, role, content, tool_calls, tool_call_id, name, created_ms
             FROM messages WHERE conv = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![conv], |r| {
            Ok(StoredMessage {
                seq: r.get(0)?,
                role: r.get(1)?,
                content: r.get(2)?,
                tool_calls: r.get(3)?,
                tool_call_id: r.get(4)?,
                name: r.get(5)?,
                created_ms: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Next free message seq of a conversation.
    pub fn chat_next_seq(&self, conv: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(seq) + 1, 0) FROM messages WHERE conv = ?1",
            params![conv],
            |r| r.get(0),
        )?)
    }

    /// Pinned first, then most recently changed. `query` matches the title or any message text
    /// (case-insensitive substring, no wildcards).
    pub fn list_chats(&self, query: &str, limit: usize) -> Result<Vec<ChatMeta>> {
        let q = query.trim().to_lowercase();
        let sql = format!(
            "SELECT {META_COLS} FROM conversations c WHERE deleted = 0 AND (?1 = '' OR
                instr(lower(title), ?1) > 0 OR EXISTS (SELECT 1 FROM messages m
                    WHERE m.conv = c.id AND m.role IN ('user', 'assistant')
                    AND instr(lower(m.content), ?1) > 0))
             ORDER BY pinned DESC, updated_ms DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![q, limit as i64], meta_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_chat(&self, id: &str, title: &str) -> Result<bool> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(false);
        }
        let n = self.conn.execute(
            "UPDATE conversations SET title = ?2, title_auto = 0 WHERE id = ?1 AND deleted = 0",
            params![id, clip_bytes(title, 300)],
        )?;
        if n > 0 {
            self.touch(KIND_CHATS, id, false)?;
        }
        Ok(n > 0)
    }

    /// Set an automatic title (e.g. from a short model call) unless the user renamed the chat.
    pub fn set_auto_chat_title(&self, id: &str, title: &str) -> Result<bool> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(false);
        }
        let n = self.conn.execute(
            "UPDATE conversations SET title = ?2
             WHERE id = ?1 AND deleted = 0 AND title_auto = 1 AND title <> ?2",
            params![id, clip_bytes(title, 300)],
        )?;
        if n > 0 {
            self.touch(KIND_CHATS, id, false)?;
        }
        Ok(n > 0)
    }

    pub fn pin_chat(&self, id: &str, pinned: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE conversations SET pinned = ?2 WHERE id = ?1 AND deleted = 0 AND pinned <> ?2",
            params![id, pinned],
        )?;
        if n > 0 {
            self.touch(KIND_CHATS, id, false)?;
        }
        Ok(n > 0)
    }

    /// Delete a conversation here and (as tombstones) on synced devices. The row stays,
    /// emptied and marked deleted, so a late message from another device cannot revive it.
    pub fn delete_chat(&self, id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE conversations SET deleted = 1, title = '', pinned = 0 WHERE id = ?1 AND deleted = 0",
            params![id],
        )?;
        tx.execute("DELETE FROM messages WHERE conv = ?1", params![id])?;
        tx.execute(
            "UPDATE history SET conv = NULL WHERE conv = ?1",
            params![id],
        )?;
        // Tombstones for the metadata and every message item we know of.
        let now = unix_now_ms();
        tx.execute(
            "UPDATE sync_state SET m = MAX(?2, m + 1), deleted = 1, dirty = 1
             WHERE kind = 'chats' AND deleted = 0 AND (id = ?1 OR id LIKE ?1 || '/%')",
            params![id, now],
        )?;
        tx.commit()?;
        if n > 0 {
            self.touch(KIND_CHATS, id, true)?;
        }
        Ok(n > 0)
    }

    /// Delete every conversation (pinned ones too), here and on synced devices.
    pub fn clear_chats(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE conversations SET deleted = 1, title = '', pinned = 0 WHERE deleted = 0",
            [],
        )?;
        tx.execute("DELETE FROM messages", [])?;
        tx.execute("UPDATE history SET conv = NULL WHERE conv IS NOT NULL", [])?;
        tx.execute(
            "UPDATE sync_state SET m = MAX(?1, m + 1), deleted = 1, dirty = 1
             WHERE kind = 'chats' AND deleted = 0",
            params![unix_now_ms()],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// Apply the retention setting: drop unpinned chats last changed before the cutoff, and
    /// messages left without a conversation. Like the history size cap, this is local
    /// housekeeping, not a deletion other devices should copy. Returns chats removed.
    pub fn prune_chats(&self, now_ms: i64) -> Result<usize> {
        let Some(cutoff) = self.chat_retention()?.cutoff_ms(now_ms) else {
            return Ok(0);
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS pruned_chats (id TEXT PRIMARY KEY); DELETE FROM pruned_chats;")?;
        tx.execute(
            "INSERT INTO pruned_chats SELECT id FROM conversations
             WHERE deleted = 0 AND pinned = 0 AND updated_ms < ?1",
            params![cutoff],
        )?;
        let n = tx.execute(
            "DELETE FROM conversations WHERE id IN (SELECT id FROM pruned_chats)",
            [],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE conv IN (SELECT id FROM pruned_chats)
                OR (created_ms < ?1 AND conv NOT IN (SELECT id FROM conversations))",
            params![cutoff],
        )?;
        tx.execute(
            "UPDATE history SET conv = NULL WHERE conv IN (SELECT id FROM pruned_chats)",
            [],
        )?;
        // Forget the sync bookkeeping of what is gone (live items only: pending tombstones of
        // chats the user deleted still have to reach other devices).
        tx.execute(
            "DELETE FROM sync_state WHERE kind = 'chats' AND deleted = 0 AND (
                id IN (SELECT id FROM pruned_chats)
                OR substr(id, 1, instr(id, '/') - 1) IN (SELECT id FROM pruned_chats))",
            [],
        )?;
        tx.execute("DELETE FROM pruned_chats", [])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn chats_len(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM conversations WHERE deleted = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(seq: i64, role: &str, content: &str, at: i64) -> StoredMessage {
        StoredMessage {
            seq,
            role: role.into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            created_ms: at,
        }
    }

    #[test]
    fn ids_and_titles() {
        let a = new_chat_id();
        assert_eq!(a.len(), 32);
        assert!(is_chat_id(&a));
        assert_ne!(a, new_chat_id());
        assert!(!is_chat_id("../etc"));
        assert!(!is_chat_id("abc"));
        assert_eq!(
            title_from_message("  \n  Find me a homestay\nin Tanah Rata"),
            "Find me a homestay"
        );
        let long = "word ".repeat(40);
        let t = title_from_message(&long);
        assert!(t.chars().count() <= 61 && t.ends_with('\u{2026}'), "{t}");
        assert_eq!(title_from_message("   "), "New chat");
    }

    #[test]
    fn tool_summaries_drop_images_and_cap() {
        let shot = format!(
            "Took a screenshot data:image/png;base64,{} of the page",
            "A".repeat(5000)
        );
        let s = summarize_tool_output(&shot);
        assert_eq!(s, "Took a screenshot [image omitted] of the page");
        let raw = format!("blob {}", "QUJD".repeat(300));
        assert_eq!(summarize_tool_output(&raw), "blob [binary omitted]");
        let big = "x y ".repeat(5000);
        let s = summarize_tool_output(&big);
        assert!(s.len() <= MAX_TOOL_SUMMARY_BYTES);
        assert!(s.ends_with("[truncated]"));
        // Multi-byte text is cut on a character boundary.
        let s = clip_bytes(&"é".repeat(3000), 101);
        assert!(s.len() <= 101);
    }

    #[test]
    fn chat_crud_and_search() {
        let s = Store::open_in_memory().unwrap();
        s.create_chat("aaaa0000", "Homestays", "qwen3:8b", 100)
            .unwrap();
        s.create_chat("bbbb0000", "Rust question", "gpt", 200)
            .unwrap();
        s.append_chat_message(
            "aaaa0000",
            &msg(0, "user", "Tea trails near Brinchang?", 300),
            None,
        )
        .unwrap();
        s.append_chat_message(
            "aaaa0000",
            &msg(1, "assistant", "Try Sungai Palas.", 310),
            Some("qwen3:14b"),
        )
        .unwrap();
        assert_eq!(s.chat_next_seq("aaaa0000").unwrap(), 2);
        assert_eq!(s.chat_next_seq("bbbb0000").unwrap(), 0);
        let m = s.chat_messages("aaaa0000").unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].content, "Try Sungai Palas.");
        let c = s.chat("aaaa0000").unwrap().unwrap();
        assert_eq!(c.updated_ms, 310);
        assert_eq!(c.model, "qwen3:14b");

        let ids = |q: &str| -> Vec<String> {
            s.list_chats(q, 50)
                .unwrap()
                .into_iter()
                .map(|c| c.id)
                .collect()
        };
        assert_eq!(ids(""), ["aaaa0000", "bbbb0000"], "newest first");
        assert_eq!(ids("PALAS"), ["aaaa0000"], "message text matches");
        assert_eq!(ids("rust"), ["bbbb0000"], "title matches");
        assert!(ids("%").is_empty(), "no wildcards");

        assert!(s.pin_chat("bbbb0000", true).unwrap());
        assert!(!s.pin_chat("bbbb0000", true).unwrap());
        assert_eq!(ids(""), ["bbbb0000", "aaaa0000"], "pinned first");

        assert!(s.rename_chat("aaaa0000", "  Cameron trip ").unwrap());
        assert!(!s.rename_chat("aaaa0000", "   ").unwrap());
        assert!(
            !s.set_auto_chat_title("aaaa0000", "Auto").unwrap(),
            "renamed wins"
        );
        assert_eq!(s.chat("aaaa0000").unwrap().unwrap().title, "Cameron trip");
        assert!(
            s.set_auto_chat_title("bbbb0000", "Borrowing in Rust")
                .unwrap()
        );

        // A page Myth opened links back; deleting the chat unlinks it.
        s.record_visit("https://palas.example/", "Palas", 320)
            .unwrap();
        assert!(
            s.link_history_conv("https://palas.example/", "aaaa0000")
                .unwrap()
        );
        assert_eq!(
            s.history_entry("https://palas.example/")
                .unwrap()
                .unwrap()
                .conv
                .as_deref(),
            Some("aaaa0000")
        );
        assert!(s.delete_chat("aaaa0000").unwrap());
        assert!(!s.delete_chat("aaaa0000").unwrap());
        assert!(s.chat("aaaa0000").unwrap().is_none());
        assert!(s.chat_messages("aaaa0000").unwrap().is_empty());
        assert_eq!(
            s.history_entry("https://palas.example/")
                .unwrap()
                .unwrap()
                .conv,
            None
        );
        assert_eq!(ids(""), ["bbbb0000"]);

        assert_eq!(s.clear_chats().unwrap(), 1, "pinned chats are cleared too");
        assert_eq!(s.chats_len().unwrap(), 0);
    }

    #[test]
    fn long_messages_are_capped() {
        let s = Store::open_in_memory().unwrap();
        s.create_chat("cccc0000", "t", "", 1).unwrap();
        let mut m = msg(0, "assistant", &"z".repeat(MAX_MESSAGE_BYTES * 2), 2);
        m.tool_calls = Some(format!(
            "[{{\"a\":\"{}\"}}]",
            "q".repeat(MAX_TOOL_CALLS_BYTES)
        ));
        s.append_chat_message("cccc0000", &m, None).unwrap();
        let back = &s.chat_messages("cccc0000").unwrap()[0];
        assert!(back.content.len() <= MAX_MESSAGE_BYTES);
        assert_eq!(back.tool_calls.as_deref(), Some("[]"));
    }

    #[test]
    fn retention_prunes_unpinned_old_chats() {
        let s = Store::open_in_memory().unwrap();
        let now = 1_000 * DAY_MS;
        s.create_chat("old00000", "old", "", now - 40 * DAY_MS)
            .unwrap();
        s.append_chat_message("old00000", &msg(0, "user", "hi", now - 40 * DAY_MS), None)
            .unwrap();
        s.create_chat("pin00000", "pinned", "", now - 400 * DAY_MS)
            .unwrap();
        s.pin_chat("pin00000", true).unwrap();
        s.create_chat("new00000", "new", "", now - DAY_MS).unwrap();
        assert_eq!(s.chat_retention().unwrap(), Retention::Forever);
        assert_eq!(s.prune_chats(now).unwrap(), 0, "forever keeps everything");
        s.set_chat_retention(Retention::Days(30)).unwrap();
        assert_eq!(s.chat_retention().unwrap(), Retention::Days(30));
        assert_eq!(s.prune_chats(now).unwrap(), 1);
        let left: Vec<String> = s
            .list_chats("", 10)
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(left, ["pin00000", "new00000"]);
        assert!(s.chat_messages("old00000").unwrap().is_empty());
        // Pruning is not a deletion: no tombstones to upload for it.
        let tomb: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sync_state WHERE kind = 'chats' AND deleted = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tomb, 0);
        assert_eq!(Retention::parse("90"), Some(Retention::Days(90)));
        assert_eq!(Retention::parse("7"), None);
    }

    #[test]
    fn migrates_v2_profile() {
        let dir = std::env::temp_dir().join(format!("mb-chats-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profile.sqlite3");
        {
            // A v2 profile as the previous release wrote it.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(crate::SCHEMA).unwrap();
            c.execute_batch(
                "INSERT INTO history VALUES ('https://servo.org/', 'Servo', 3, 42);
                 PRAGMA user_version = 2;",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, crate::SCHEMA_VERSION);
        let h = s.history_entry("https://servo.org/").unwrap().unwrap();
        assert_eq!((h.visit_count, h.conv), (3, None));
        s.create_chat("dddd0000", "t", "", 1).unwrap();
        assert_eq!(s.chats_len().unwrap(), 1);
        drop(s);
        // Opening again is a no-op migration.
        let s = Store::open(&path).unwrap();
        assert_eq!(s.chats_len().unwrap(), 1);
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
