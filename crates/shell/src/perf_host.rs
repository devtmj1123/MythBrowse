// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Perf HUD and trace (v0.4.2): `MB_PERF=1` at start, or Ctrl+Shift+F12 at run time.
//!
//! Per presented frame the shell records how long it spent in Servo's paint of the page and
//! of the chrome, in its own composition (sky, blur, glass, layers) and in `present()`, and
//! how long a wheel / autoscroll input took to reach the screen (`engine::perf`). Once a
//! second one summary line goes to the HUD (a small box drawn by the chrome, bottom left) and
//! to `perf.log` in the profile folder (`%LOCALAPPDATA%\MythBrowse\perf.log`), together with
//! the renderer line, so a report from a real device carries numbers.
//!
//! Off by default; when off, nothing is measured beyond one `Cell` read per frame.

use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::Instant;

use log::{info, warn};
use mythbrowse_engine::perf::{FrameSample, PerfWindow, ScrollPath};

use crate::diag;

pub struct Perf {
    on: Cell<bool>,
    clock: Instant,
    window: RefCell<PerfWindow>,
    file: RefCell<Option<File>>,
}

impl Perf {
    /// Reads `MB_PERF` (any value but empty / `0` turns it on).
    pub fn new(clock: Instant) -> Perf {
        let on = std::env::var("MB_PERF").is_ok_and(|v| !v.is_empty() && v != "0");
        Perf {
            on: Cell::new(on),
            clock,
            window: RefCell::new(PerfWindow::default()),
            file: RefCell::new(None),
        }
    }

    pub fn on(&self) -> bool {
        self.on.get()
    }

    pub fn now_ms(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1000.0
    }

    /// Start of a measured span (only when on).
    pub fn mark(&self) -> Option<Instant> {
        self.on.get().then(Instant::now)
    }

    /// Ctrl+Shift+F12. Returns the new state.
    pub fn toggle(&self, renderer: &str) -> bool {
        let on = !self.on.get();
        self.on.set(on);
        *self.window.borrow_mut() = PerfWindow::default();
        if on {
            self.start(renderer);
        } else {
            self.write("perf trace off");
            *self.file.borrow_mut() = None;
        }
        on
    }

    /// Open the trace file and write the header (renderer, window size).
    pub fn start(&self, renderer: &str) {
        let path = diag::perf_log_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                *self.file.borrow_mut() = Some(f);
                info!("perf trace on: {}", path.display());
            }
            Err(e) => warn!("perf trace: cannot open {}: {e}", path.display()),
        }
        self.write(&format!(
            "perf trace on | MythBrowse {} | renderer: {renderer}",
            env!("CARGO_PKG_VERSION")
        ));
    }

    pub fn write(&self, line: &str) {
        if let Some(f) = self.file.borrow_mut().as_mut() {
            let _ = writeln!(f, "[{}] {line}", diag::stamp());
            let _ = f.flush();
        }
    }

    pub fn on_input(&self) {
        if self.on.get() {
            let now = self.now_ms();
            self.window.borrow_mut().on_input(now);
        }
    }

    pub fn on_scroll_sent(&self, path: ScrollPath) {
        if self.on.get() {
            self.window.borrow_mut().on_scroll_sent(path);
        }
    }

    /// A frame was presented; returns the summary line when one is due (for the HUD).
    pub fn on_frame(&self, sample: FrameSample) -> Option<String> {
        if !self.on.get() {
            return None;
        }
        let now = self.now_ms();
        let mut w = self.window.borrow_mut();
        w.on_frame(now, sample);
        let line = w.take_due(now)?.line();
        drop(w);
        self.write(&line);
        Some(line)
    }
}

/// Milliseconds since `t` (0 when not measured).
pub fn ms_since(t: Option<Instant>) -> f64 {
    t.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0)
}
