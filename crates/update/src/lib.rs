// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! MythBrowse updates from the public GitHub repository's releases.
//!
//! * [`version`]: tag parsing and SemVer ordering (`v0.4.0` > `0.4.0-preview.2` > `0.3.9`).
//! * [`release`]: the read-only `releases/latest` answer, which `.msi` to install for this
//!   machine, its `.sha256`, and the only download URLs allowed (this repository's release
//!   files on github.com).
//! * [`updater`]: the check / download / verify / `msiexec /i` state machine, at most one
//!   automatic check a day.
//!
//! Nothing here writes to GitHub or uses a token.

pub mod release;
pub mod updater;
pub mod version;

pub use release::{Arch, Asset, Channel, Release};
pub use updater::{Config, Phase, Status, Updater};
pub use version::{Version, is_newer};

/// The public repository MythBrowse releases are published in (open and official builds; see
/// [`Channel`] for which installers each takes).
pub const RELEASE_REPO: &str = "devtmj1123/MythBrowse";
