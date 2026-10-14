/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

mod base_fragment;
mod box_fragment;
mod containing_block;
mod fragment;
#[allow(clippy::module_inception)]
mod fragment_tree;
mod hoisted_shared_fragment;
mod positioning_fragment;

pub(crate) use base_fragment::*;
pub(crate) use box_fragment::*;
pub(crate) use containing_block::*;
pub(crate) use fragment::*;
pub(crate) use fragment_tree::*;
pub(crate) use hoisted_shared_fragment::*;
pub(crate) use positioning_fragment::*;

/// MythBrowse patch (servo-layout-0001): logged (once per process) when a fragment's
/// scrollable overflow is read before the overflow pass reached it; the callers fall back to a
/// safe rectangle instead of panicking.
pub(crate) fn warn_overflow_not_calculated(what: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("layout: {what} read before overflow was calculated; using a fallback rect");
    }
}
