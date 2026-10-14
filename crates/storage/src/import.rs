// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Bulk import of bookmarks and history taken from another browser (Chrome / Edge /
//! Chromium, read by `mythbrowse-passwords::import`). One transaction per call; rows the user
//! already has are never overwritten (an existing bookmark keeps its title, an existing
//! history entry keeps its count unless the imported one is newer). Imported items are marked
//! for sync like local changes.

use rusqlite::params;

use crate::{KIND_BOOKMARKS, KIND_HISTORY, Result, Store, should_record, unix_now_ms};

/// A bookmark to import.
pub struct NewBookmark<'a> {
    pub url: &'a str,
    pub title: &'a str,
    /// When the other browser added it (unix ms; 0 = unknown, use now).
    pub added_ms: i64,
}

/// A history entry to import.
pub struct NewVisit<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub visit_count: i64,
    pub last_visit_ms: i64,
}

impl Store {
    /// Add bookmarks that are not bookmarked yet. Returns how many were added. URLs this
    /// browser never records (internal pages, `excluded_host`, non-web schemes) are skipped.
    pub fn import_bookmarks<'a>(
        &self,
        items: impl IntoIterator<Item = NewBookmark<'a>>,
        excluded_host: &str,
    ) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let now = unix_now_ms();
        let mut added = 0;
        for b in items {
            if !should_record(b.url, excluded_host) {
                continue;
            }
            let at = if b.added_ms > 0 { b.added_ms } else { now };
            let n = tx.execute(
                "INSERT INTO bookmarks (url, title, created_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(url) DO NOTHING",
                params![b.url, b.title, at],
            )?;
            if n > 0 {
                added += 1;
                self.touch(KIND_BOOKMARKS, b.url, false)?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Merge history entries. New URLs are added; for a URL already in history the visit
    /// count and time only move forward and an empty title is filled. Returns how many URLs
    /// were new.
    pub fn import_history<'a>(
        &self,
        items: impl IntoIterator<Item = NewVisit<'a>>,
        excluded_host: &str,
    ) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut added = 0;
        for v in items {
            if v.last_visit_ms <= 0 || !should_record(v.url, excluded_host) {
                continue;
            }
            let existed: bool = tx
                .query_row(
                    "SELECT 1 FROM history WHERE url = ?1",
                    params![v.url],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            tx.execute(
                "INSERT INTO history (url, title, visit_count, last_visit_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(url) DO UPDATE SET
                    visit_count = MAX(visit_count, excluded.visit_count),
                    last_visit_ms = MAX(last_visit_ms, excluded.last_visit_ms),
                    title = CASE WHEN title = '' THEN excluded.title ELSE title END",
                params![v.url, v.title, v.visit_count.max(1), v.last_visit_ms],
            )?;
            if !existed {
                added += 1;
                self.touch(KIND_HISTORY, v.url, false)?;
            }
        }
        tx.commit()?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_never_overwrite_and_skip_internal() {
        let s = Store::open_in_memory().unwrap();
        s.add_bookmark("https://a.example/", "Mine", 5).unwrap();
        let n = s
            .import_bookmarks(
                [
                    NewBookmark {
                        url: "https://a.example/",
                        title: "Theirs",
                        added_ms: 1,
                    },
                    NewBookmark {
                        url: "https://b.example/",
                        title: "B",
                        added_ms: 0,
                    },
                    NewBookmark {
                        url: "https://mythbrowse.internal/x",
                        title: "I",
                        added_ms: 1,
                    },
                    NewBookmark {
                        url: "chrome://settings",
                        title: "C",
                        added_ms: 1,
                    },
                ],
                "mythbrowse.internal",
            )
            .unwrap();
        assert_eq!(n, 1);
        let all = s.bookmarks().unwrap();
        assert_eq!(all.len(), 2);
        assert!(
            all.iter()
                .any(|b| b.url == "https://a.example/" && b.title == "Mine")
        );
    }

    #[test]
    fn history_merges_forward_only() {
        let s = Store::open_in_memory().unwrap();
        s.record_visit("https://a.example/", "A", 1_000).unwrap();
        let n = s
            .import_history(
                [
                    NewVisit {
                        url: "https://a.example/",
                        title: "Other",
                        visit_count: 9,
                        last_visit_ms: 500,
                    },
                    NewVisit {
                        url: "https://b.example/",
                        title: "B",
                        visit_count: 3,
                        last_visit_ms: 2_000,
                    },
                    NewVisit {
                        url: "https://c.example/",
                        title: "C",
                        visit_count: 1,
                        last_visit_ms: 0,
                    },
                ],
                "mythbrowse.internal",
            )
            .unwrap();
        assert_eq!(n, 1);
        let a = s.history_entry("https://a.example/").unwrap().unwrap();
        assert_eq!(
            (a.title.as_str(), a.visit_count, a.last_visit_ms),
            ("A", 9, 1_000)
        );
        assert!(s.history_entry("https://c.example/").unwrap().is_none());
    }
}
