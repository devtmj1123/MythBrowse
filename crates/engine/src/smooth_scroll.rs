// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
//! Mouse-wheel / touchpad scrolling for the shell, frame-paced and smoothed.
//!
//! Before (v0.3) every winit wheel event went straight to Servo as its own `WheelEvent`: a
//! wheel notch jumped 76 device px in one frame (no animation, and only half a notch's worth
//! on a 150 % display), and a precision touchpad's stream of small events reached the page
//! one by one, each a DOM `wheel` dispatch on the script thread.
//!
//! Now:
//! - **Wheel notches** (whole line deltas: Windows reports ±1.0 per notch) animate over
//!   [`SMOOTH_MS`] with an ease-out curve (as Firefox / Chrome smooth scrolling), 100 CSS px a
//!   notch (Chrome's and Edge's default). A notch during the animation adds to what is left
//!   and restarts the curve from the current speed, so fast wheeling accelerates smoothly.
//! - **Fine deltas** (fractional line deltas from precision touchpads / free-spinning
//!   wheels, and pixel deltas) are applied as they come, already smooth.
//! - Everything is **coalesced to at most one event per frame** ([`FRAME_MS`]), so the page
//!   and Servo's compositor see one wheel event per frame however fast the device reports.
//! - Reduced motion: notches are applied at once (still coalesced).
//!
//! Time is passed in (ms, `f64`), so this is unit-tested without a clock.

/// Duration of the smooth animation of one wheel notch.
pub const SMOOTH_MS: f64 = 150.0;
/// Minimum spacing of the events sent to the page (≈120 Hz: never slower than the display).
pub const FRAME_MS: f64 = 8.0;
/// CSS px scrolled per wheel notch.
pub const NOTCH_CSS_PX: f64 = 100.0;
/// Minimum spacing of steps on the compositor (fast) path, which is paced by the display's
/// own frames (one step per presented frame): only a guard against a busy loop when present
/// does not block (vsync off, occluded window). Below every real refresh interval (240 Hz).
pub const PACED_MIN_GAP_MS: f64 = 3.0;

/// One winit wheel event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelInput {
    /// Notches / lines (`MouseScrollDelta::LineDelta`).
    Lines(f64, f64),
    /// Physical px (`MouseScrollDelta::PixelDelta`).
    Pixels(f64, f64),
}

/// Whether a line delta is a whole wheel notch (smoothed) rather than touchpad / free-wheel
/// precision input (applied directly).
pub fn is_notch(dx: f64, dy: f64) -> bool {
    let whole = |v: f64| v.abs() >= 0.99 && (v - v.round()).abs() < 0.02;
    let zero = |v: f64| v.abs() < 1e-6;
    (whole(dy) && zero(dx)) || (whole(dx) && zero(dy)) || (whole(dx) && whole(dy))
}

fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Anim {
    start_ms: f64,
    total: (f64, f64),
    done: (f64, f64),
}

/// Per-target scroller (the shell keeps one for the view under the pointer).
#[derive(Debug, Clone, Default)]
pub struct SmoothScroller {
    anim: Option<Anim>,
    pending: (f64, f64),
    last_emit_ms: Option<f64>,
}

impl SmoothScroller {
    /// Feed one wheel event. `px_per_notch`: device px per notch (CSS px × scale);
    /// `smooth`: animate notches (off with reduced motion).
    pub fn on_wheel(&mut self, input: WheelInput, px_per_notch: f64, smooth: bool, now_ms: f64) {
        match input {
            WheelInput::Pixels(x, y) => self.add_direct(x, y),
            WheelInput::Lines(x, y) => {
                let (px, py) = (x * px_per_notch, y * px_per_notch);
                if smooth && is_notch(x, y) {
                    let left = self
                        .anim
                        .map_or((0.0, 0.0), |a| (a.total.0 - a.done.0, a.total.1 - a.done.1));
                    // What the old curve already emitted stays emitted; the new curve covers
                    // what was left plus this notch.
                    self.anim = Some(Anim {
                        start_ms: now_ms,
                        total: (left.0 + px, left.1 + py),
                        done: (0.0, 0.0),
                    });
                } else {
                    self.add_direct(px, py);
                }
            }
        }
    }

    fn add_direct(&mut self, x: f64, y: f64) {
        self.pending.0 += x;
        self.pending.1 += y;
    }

    fn progress(&self, a: &Anim, now_ms: f64) -> f64 {
        ease_out_cubic((now_ms - a.start_ms) / SMOOTH_MS)
    }

    /// Drop any animation and pending delta (a click, a key, a new page).
    pub fn cancel(&mut self) {
        self.anim = None;
        self.pending = (0.0, 0.0);
    }

    pub fn is_idle(&self) -> bool {
        self.anim.is_none() && self.pending == (0.0, 0.0)
    }

    /// The delta to send now (device px), if any is due: at most once per [`FRAME_MS`].
    pub fn tick(&mut self, now_ms: f64) -> Option<(f64, f64)> {
        self.tick_paced(now_ms, FRAME_MS)
    }

    /// As [`SmoothScroller::tick`], with the caller's minimum spacing (`min_gap_ms`): the
    /// compositor path ticks once per presented frame ([`PACED_MIN_GAP_MS`]).
    pub fn tick_paced(&mut self, now_ms: f64, min_gap_ms: f64) -> Option<(f64, f64)> {
        if self.is_idle() {
            return None;
        }
        if let Some(last) = self.last_emit_ms
            && now_ms - last < min_gap_ms - 0.5
        {
            return None;
        }
        let mut out = self.pending;
        self.pending = (0.0, 0.0);
        if let Some(mut a) = self.anim {
            let p = self.progress(&a, now_ms);
            let target = (a.total.0 * p, a.total.1 * p);
            out.0 += target.0 - a.done.0;
            out.1 += target.1 - a.done.1;
            a.done = target;
            self.anim = (p < 1.0).then_some(a);
        }
        if out.0.abs() < 0.01 && out.1.abs() < 0.01 {
            // Too small to send yet: carry it (nothing is lost), unless the curve is done.
            if self.anim.is_some() || out == (0.0, 0.0) {
                self.pending = out;
                return None;
            }
        }
        self.last_emit_ms = Some(now_ms);
        Some(out)
    }

    /// When [`SmoothScroller::tick`] next has something to send (`None`: idle).
    pub fn next_due(&self, now_ms: f64) -> Option<f64> {
        if self.is_idle() {
            return None;
        }
        Some(
            self.last_emit_ms
                .map_or(now_ms, |l| (l + FRAME_MS).max(now_ms)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut SmoothScroller, from: f64, to: f64) -> (f64, f64, usize) {
        let (mut x, mut y, mut n) = (0.0, 0.0, 0);
        let mut t = from;
        while t <= to {
            if let Some((dx, dy)) = s.tick(t) {
                x += dx;
                y += dy;
                n += 1;
            }
            t += 1.0;
        }
        (x, y, n)
    }

    #[test]
    fn notches_and_fine_deltas_are_told_apart() {
        assert!(is_notch(0.0, 1.0));
        assert!(is_notch(0.0, -2.0));
        assert!(is_notch(1.0, 0.0));
        assert!(!is_notch(0.0, 0.25), "precision touchpad");
        assert!(!is_notch(0.0, 1.5));
        assert!(!is_notch(0.3, 1.0));
    }

    #[test]
    fn a_notch_is_spread_over_frames_and_lands_exactly() {
        let mut s = SmoothScroller::default();
        s.on_wheel(WheelInput::Lines(0.0, -1.0), 150.0, true, 0.0);
        let first = s.tick(0.0);
        assert!(first.is_none() || first.unwrap().1.abs() < 1.0);
        let (x, y, n) = run(&mut s, 1.0, 400.0);
        assert!((y - -150.0).abs() < 1e-6, "total {y}");
        assert_eq!(x, 0.0);
        assert!(n >= 10, "{n} frames");
        assert!(s.is_idle());
    }

    #[test]
    fn frames_are_coalesced() {
        let mut s = SmoothScroller::default();
        for i in 0..10 {
            s.on_wheel(WheelInput::Lines(0.0, 0.1), 100.0, true, i as f64 * 0.5);
        }
        assert_eq!(s.tick(5.0).map(|d| (d.1 * 100.0).round()), Some(10000.0));
        s.on_wheel(WheelInput::Pixels(0.0, 3.0), 100.0, true, 6.0);
        assert_eq!(s.tick(6.0), None, "within the frame");
        assert_eq!(s.next_due(6.0), Some(5.0 + FRAME_MS));
        assert_eq!(s.tick(13.0), Some((0.0, 3.0)));
        assert_eq!(s.next_due(14.0), None);
    }

    #[test]
    fn quick_notches_accumulate() {
        let mut s = SmoothScroller::default();
        s.on_wheel(WheelInput::Lines(0.0, 1.0), 100.0, true, 0.0);
        let (_, a, _) = run(&mut s, 0.0, 40.0);
        s.on_wheel(WheelInput::Lines(0.0, 1.0), 100.0, true, 41.0);
        let (_, b, _) = run(&mut s, 41.0, 400.0);
        assert!((a + b - 200.0).abs() < 1e-6, "{a} + {b}");
    }

    #[test]
    fn paced_ticks_follow_the_display() {
        // 144 Hz frames (6.9 ms) all get a step on the paced path; the 8 ms gate would skip
        // every other one (uneven steps = judder).
        let mut s = SmoothScroller::default();
        s.on_wheel(WheelInput::Lines(0.0, -1.0), 100.0, true, 0.0);
        let (mut t, mut n, mut sum) = (6.9, 0, 0.0);
        while t < 200.0 {
            if let Some((_, dy)) = s.tick_paced(t, PACED_MIN_GAP_MS) {
                n += 1;
                sum += dy;
            }
            t += 6.9;
        }
        assert!((sum - -100.0).abs() < 1e-6, "{sum}");
        assert!(n >= 21, "{n} steps for 22 frames in 150 ms");
        assert!(s.is_idle());
        // The guard still holds back a busy loop.
        s.on_wheel(WheelInput::Pixels(0.0, 5.0), 100.0, true, 300.0);
        assert!(s.tick_paced(300.0, PACED_MIN_GAP_MS).is_some());
        s.on_wheel(WheelInput::Pixels(0.0, 5.0), 100.0, true, 300.5);
        assert!(s.tick_paced(301.0, PACED_MIN_GAP_MS).is_none());
        assert!(s.tick_paced(303.0, PACED_MIN_GAP_MS).is_some());
    }

    #[test]
    fn reduced_motion_applies_at_once() {
        let mut s = SmoothScroller::default();
        s.on_wheel(WheelInput::Lines(0.0, 1.0), 100.0, false, 0.0);
        assert_eq!(s.tick(0.0), Some((0.0, 100.0)));
        assert!(s.is_idle());
        s.on_wheel(WheelInput::Lines(0.0, 1.0), 100.0, true, 20.0);
        s.cancel();
        assert_eq!(s.tick(40.0), None);
    }
}
