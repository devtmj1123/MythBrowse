// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
//! Middle-click autoscroll (as in Firefox / Chrome on Windows).
//!
//! A middle press on a page, not on a link or a form field, drops an anchor; the page then
//! scrolls towards the pointer at a speed that grows with its distance from the anchor (a
//! small dead zone around the anchor scrolls nothing), in both axes, until any click, Esc, a
//! wheel turn or a tab change ends it. Two ways to use it, as in Firefox:
//!
//! - **click** (press and release without moving out of the dead zone): the mode stays on
//!   after the release, until the next click;
//! - **drag** (hold the button and move): releasing the button ends it.
//!
//! The shell drives it once per presented frame and sends the deltas on Servo's compositor
//! scroll path (`WebView::notify_scroll_event`, no script round trip), the same path as the
//! wheel. Pure state machine, the clock is passed in (ms, `f64`), unit-tested.

/// Dead zone radius around the anchor (CSS px): nothing scrolls inside it.
pub const DEAD_ZONE_CSS: f64 = 12.0;
/// Speed cap (CSS px per second).
pub const MAX_SPEED_CSS: f64 = 12_000.0;
/// Longest step one frame may integrate (a stalled frame must not jump the page).
pub const MAX_STEP_MS: f64 = 50.0;

/// Scroll speed (CSS px / s, signed) for a pointer `d` CSS px away from the anchor on one
/// axis: 0 in the dead zone, then about 3.5 px/s per px plus a quadratic term, so small
/// offsets read slowly and a pointer near the window edge races (50 px past the dead zone:
/// ~290 px/s, 150 px: ~1.5k px/s, 300 px: ~5k px/s).
pub fn speed_css(d: f64) -> f64 {
    let a = d.abs() - DEAD_ZONE_CSS;
    if a <= 0.0 {
        return 0.0;
    }
    let v = (3.5 * a + 0.045 * a * a).min(MAX_SPEED_CSS);
    v.copysign(d)
}

/// Which way the pointer points (for the cursor): `(dx, dy)` signs, each -1, 0 or 1.
pub fn direction(dx_css: f64, dy_css: f64) -> (i8, i8) {
    let s = |v: f64| {
        if v.abs() <= DEAD_ZONE_CSS {
            0
        } else if v < 0.0 {
            -1
        } else {
            1
        }
    };
    (s(dx_css), s(dy_css))
}

/// How a release of the middle button is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// Keep scrolling (it was a click: the pointer stayed in the dead zone).
    Stay,
    /// End (it was a drag).
    End,
}

/// One autoscroll session. Positions in device px (window coordinates), `scale` = device px
/// per CSS px.
#[derive(Debug, Clone, PartialEq)]
pub struct Autoscroll {
    anchor: (f64, f64),
    pointer: (f64, f64),
    scale: f64,
    last_ms: f64,
    carry: (f64, f64),
    /// The pointer left the dead zone while the button was still held (drag mode).
    dragged: bool,
    held: bool,
}

impl Autoscroll {
    /// Start at `anchor` (where the middle button went down), button still held.
    pub fn new(anchor: (f64, f64), scale: f64, now_ms: f64) -> Autoscroll {
        Autoscroll {
            anchor,
            pointer: anchor,
            scale: if scale > 0.0 { scale } else { 1.0 },
            last_ms: now_ms,
            carry: (0.0, 0.0),
            dragged: false,
            held: true,
        }
    }

    pub fn anchor(&self) -> (f64, f64) {
        self.anchor
    }

    /// Offset of the pointer from the anchor, CSS px.
    pub fn offset_css(&self) -> (f64, f64) {
        (
            (self.pointer.0 - self.anchor.0) / self.scale,
            (self.pointer.1 - self.anchor.1) / self.scale,
        )
    }

    /// Cursor direction for the current pointer.
    pub fn direction(&self) -> (i8, i8) {
        let (x, y) = self.offset_css();
        direction(x, y)
    }

    pub fn on_move(&mut self, pointer: (f64, f64)) {
        self.pointer = pointer;
        if self.held && self.direction() != (0, 0) {
            self.dragged = true;
        }
    }

    /// The middle button came up.
    pub fn on_release(&mut self) -> Release {
        self.held = false;
        if self.dragged {
            Release::End
        } else {
            Release::Stay
        }
    }

    /// The scroll to apply now (device px, positive = towards the bottom / right, i.e.
    /// Servo's `Scroll::Delta` convention), `None` while the pointer is in the dead zone.
    /// Fractions are carried so slow speeds still move, a pixel at a time.
    pub fn tick(&mut self, now_ms: f64) -> Option<(f64, f64)> {
        let dt = (now_ms - self.last_ms).clamp(0.0, MAX_STEP_MS) / 1000.0;
        self.last_ms = now_ms;
        let (ox, oy) = self.offset_css();
        let vx = speed_css(ox) * self.scale;
        let vy = speed_css(oy) * self.scale;
        if vx == 0.0 && vy == 0.0 {
            self.carry = (0.0, 0.0);
            return None;
        }
        let x = vx * dt + self.carry.0;
        let y = vy * dt + self.carry.1;
        let (ix, iy) = (x.trunc(), y.trunc());
        self.carry = (x - ix, y - iy);
        if ix == 0.0 && iy == 0.0 {
            return None;
        }
        Some((ix, iy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_grows_with_distance_and_keeps_its_sign() {
        assert_eq!(speed_css(0.0), 0.0);
        assert_eq!(speed_css(DEAD_ZONE_CSS), 0.0);
        assert_eq!(speed_css(-DEAD_ZONE_CSS + 1.0), 0.0);
        let a = speed_css(DEAD_ZONE_CSS + 50.0);
        let b = speed_css(DEAD_ZONE_CSS + 150.0);
        let c = speed_css(DEAD_ZONE_CSS + 300.0);
        assert!(a > 250.0 && a < 330.0, "{a}");
        assert!(b > 1400.0 && b < 1700.0, "{b}");
        assert!(c > 4800.0 && c < 5400.0, "{c}");
        assert_eq!(speed_css(-(DEAD_ZONE_CSS + 50.0)), -a);
        assert_eq!(speed_css(1e6), MAX_SPEED_CSS);
    }

    #[test]
    fn scrolls_towards_the_pointer_both_ways() {
        let mut a = Autoscroll::new((500.0, 400.0), 1.5, 0.0);
        assert_eq!(a.tick(16.0), None, "in the dead zone");
        a.on_move((500.0, 400.0 + 1.5 * 112.0)); // 100 CSS px past the dead zone, down
        let mut y = 0.0;
        let mut t = 16.0;
        for _ in 0..60 {
            t += 16.0;
            if let Some((dx, dy)) = a.tick(t) {
                assert_eq!(dx, 0.0);
                y += dy;
            }
        }
        // ~800 CSS px/s * 1.5 for 0.96 s.
        let want = speed_css(112.0) * 1.5 * 0.96;
        assert!((y - want).abs() <= 2.0, "{y} vs {want}");
        a.on_move((500.0, 400.0 - 1.5 * 112.0)); // up
        let mut up = 0.0;
        for _ in 0..10 {
            t += 16.0;
            if let Some((_, dy)) = a.tick(t) {
                up += dy;
            }
        }
        assert!(up < 0.0);
        assert_eq!(a.direction(), (0, -1));
    }

    #[test]
    fn slow_speeds_still_move_and_a_stall_does_not_jump() {
        let mut a = Autoscroll::new((0.0, 0.0), 1.0, 0.0);
        a.on_move((0.0, DEAD_ZONE_CSS + 1.0)); // 3.5 px/s
        let mut y = 0.0;
        for i in 1..=1000 {
            if let Some((_, dy)) = a.tick(i as f64) {
                y += dy;
            }
        }
        assert!((2.0..=4.0).contains(&y), "{y}");
        a.on_move((0.0, 400.0));
        let jump = a.tick(5000.0).unwrap().1;
        assert!(
            jump <= speed_css(400.0) * MAX_STEP_MS / 1000.0 + 1.0,
            "{jump}"
        );
    }

    #[test]
    fn click_mode_stays_drag_mode_ends() {
        let mut a = Autoscroll::new((100.0, 100.0), 1.0, 0.0);
        a.on_move((103.0, 104.0));
        assert_eq!(a.on_release(), Release::Stay);
        // Moving after the release is steering, not a drag.
        a.on_move((100.0, 300.0));
        assert_eq!(a.on_release(), Release::Stay);
        let mut b = Autoscroll::new((100.0, 100.0), 1.0, 0.0);
        b.on_move((100.0, 160.0));
        assert_eq!(b.on_release(), Release::End);
    }
}
