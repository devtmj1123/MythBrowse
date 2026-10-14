// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! MythBrowse password manager (no Servo dependency; unit-tested on its own).
//!
//! * [`store`]: encrypted login store (`logins.sqlite3`), save / update / never-save, list,
//!   edit, delete, CSV export and import.
//! * [`crypto`]: per-profile vault key in the OS key store + AES-256-GCM values.
//! * [`origin`] + [`psl`]: which logins may be offered on a page (exact origin, http->https
//!   upgrade, same registrable domain; never https->http).
//! * [`csv`]: Chrome / Edge / Firefox / Safari / Bitwarden CSV.
//! * [`import`]: Chrome / Edge / Chromium profiles, bookmarks, history, passwords.
//! * [`page`]: the page script that notices login forms and asks the shell to fill them.
//!
//! The AI agent never gets a password: it has no tool that reads this store, its page
//! reading redacts password fields, the shell scrubs filled passwords from anything the
//! agent's scripts return, and filling needs a real user click (see `docs/PASSWORDS.md`).

pub mod crypto;
pub mod csv;
pub mod import;
pub mod origin;
pub mod page;
pub mod psl;
pub mod store;

pub use origin::{MatchKind, Origin};
pub use store::{LoginMeta, LoginStore, Match, SaveCheck, Saved, StoreError};
