// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! MythBrowse files: everything between a web page and the user's disk.
//!
//! * [`detect`]: telling a download from a page with what Servo 0.1.4 gives an embedder (it
//!   has no download API).
//! * [`names`]: the file name a download is saved under (Content-Disposition, sanitising for
//!   Windows, unique names, `.part` files).
//! * [`danger`]: file types that can run code and need the user's Keep before any byte is
//!   written.
//! * [`download`]: the transfer manager (one worker thread per transfer, pause / resume with
//!   `Range` + `If-Range`, cancel, retry).
//! * [`picker`]: the native file chooser for `<input type=file>`.
//! * [`session`]: the saved window (tabs, pinned, back/forward) and the restore decision.
//! * [`os`]: Downloads folder, open a file, show it in its folder.
//!
//! No Servo and no SQLite here: the crate is unit-tested on its own; history rows live in
//! `mythbrowse-storage`, and `crates/shell/src/files_host.rs` glues it to Servo.

pub mod danger;
pub mod detect;
pub mod download;
pub mod names;
pub mod os;
pub mod picker;
pub mod session;

/// Milliseconds since the Unix epoch (0 if the clock is before 1970).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
