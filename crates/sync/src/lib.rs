// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! End-to-end encrypted sync client.
//!
//! Data is encrypted on the device ([`crypto`]); the server (Appwrite Cloud, [`appwrite`]) only
//! stores opaque row ids and ciphertext. [`engine`] holds the records, conflict rules and the
//! pull-then-push loop, independent of storage and transport.

pub mod appwrite;
pub mod crypto;
pub mod engine;
pub mod service;

pub use crypto::{CryptoError, KdfParams, RecordKeys, SyncKey, WrappedKey};
pub use engine::{Cursor, LocalStore, Record, Remote, RemoteRow, SyncError, SyncReport, sync};

/// Kinds of data that will sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncKind {
    Bookmarks,
    History,
    Tabs,
    Settings,
    AiMemory,
    /// Saved Myth conversations: metadata item `<conv id>`, one item per message
    /// `<conv id>/<seq>` (so long chats never make one oversized row).
    Chats,
}

impl SyncKind {
    pub const ALL: [SyncKind; 6] = [
        SyncKind::Bookmarks,
        SyncKind::History,
        SyncKind::Tabs,
        SyncKind::Settings,
        SyncKind::AiMemory,
        SyncKind::Chats,
    ];

    pub fn from_name(name: &str) -> Option<SyncKind> {
        SyncKind::ALL
            .into_iter()
            .find(|k| k.collection_name() == name)
    }

    pub fn collection_name(self) -> &'static str {
        match self {
            SyncKind::Bookmarks => "bookmarks",
            SyncKind::History => "history",
            SyncKind::Tabs => "tabs",
            SyncKind::Settings => "settings",
            SyncKind::AiMemory => "ai_memory",
            SyncKind::Chats => "chats",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_names_unique() {
        let mut names: Vec<_> = SyncKind::ALL.iter().map(|k| k.collection_name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SyncKind::ALL.len());
        for k in SyncKind::ALL {
            assert_eq!(SyncKind::from_name(k.collection_name()), Some(k));
        }
    }
}
