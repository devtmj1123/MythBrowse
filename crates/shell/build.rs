// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Windows (MSVC) only: gives mythbrowse.exe its icon and version information.
//!
//! The resources are written as a compiled `.res` file (the format `rc.exe` produces) straight
//! from `packaging/windows/mythbrowse.ico`, and handed to `link.exe`, which accepts `.res`
//! inputs (it runs CVTRES itself). No resource compiler, no extra crate: nothing to install
//! on a build machine, and other targets are untouched.
//!
//! Resources: RT_GROUP_ICON 1 (every size in the .ico; Explorer, the Start menu, shortcuts
//! and the taskbar use the lowest-numbered group icon) with one RT_ICON per image, and an
//! RT_VERSION block (FileDescription "MythBrowse" is what Task Manager and the "Open with"
//! list show).

use std::path::PathBuf;

fn main() {
    let ico =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows/mythbrowse.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "msvc" {
        return;
    }
    let bytes = std::fs::read(&ico).unwrap_or_else(|e| panic!("{}: {e}", ico.display()));
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let res = res::build(&bytes, &version).unwrap_or_else(|e| panic!("{}: {e}", ico.display()));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("mythbrowse.res");
    std::fs::write(&out, res).expect("write mythbrowse.res");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

#[path = "build/res.rs"]
mod res;
