// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! MythBrowse engine layer: a thin wrapper over the `servo` crate plus engine-agnostic
//! browser logic (tab model with tab sleeping, omnibox input resolution, and the protocol
//! between the HTML chrome in `ui/` and the shell).
//!
//! The pure modules compile without Servo (`--no-default-features`), so they can be
//! unit-tested on small machines and in fast CI jobs.

pub mod agent_api;
pub mod autoscroll;
pub mod browse;
pub mod chrome;
pub mod cookies_api;
pub mod files_api;
pub mod frame;
pub mod glass;
pub mod glow;
pub mod load_watch;
pub mod omnibox;
pub mod page_input;
pub mod page_ui;
pub mod passwords_api;
pub mod perf;
pub mod settings_api;
pub mod smooth_scroll;
pub mod suggest;
pub mod tabs;

#[cfg(feature = "servo")]
pub mod servo_engine;

#[cfg(feature = "servo")]
pub use servo_engine::*;
