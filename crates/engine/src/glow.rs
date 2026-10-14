// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The light around the page card ("glow"): what it shows and when it needs frames.
//! Engine-agnostic so the state machine and the frame budget are unit-tested without Servo
//! or GL; the shell draws [`GlowFrame`]s with a small shader (`compose.rs`) **on the card's
//! own edge**: a 2 px solid gradient line just inside the edge, a faint inner glow, and a
//! soft outer glow (~16 px, fading out across the sky gap, which widens to
//! `frame::CARD_GAP_AI_CSS` while Myth works so the light has room).
//!
//! States, highest priority first (only for the page card; internal pages never glow):
//!
//! | state        | when                                              | look                                       |
//! |--------------|---------------------------------------------------|--------------------------------------------|
//! | `Approval`   | Myth waits for the user's OK on this tab          | amber `#f0a020`, clear pulse (1.6 s)       |
//! | `Controlling`| Myth has acted on this tab (click/type/navigate)  | blue -> warm orange -> a touch of magenta, flowing (5 s) |
//! | `Thinking`   | Myth works on this tab but has only read it so far | blue breathing (3.2 s), no flow           |
//! | `Loading`    | the page is loading                               | thin light sweeping along the top edge     |
//! | flash        | a run on this tab ended: done / error             | one 0.9 s fade: green `#34c47c` / soft red |
//!
//! Colours are the MythDesk sky/horizon hues, pushed to contrast with the sky they sit on
//! (CI 37759078770: sky-on-sky was invisible); at night (dark tone) a brighter set is used.
//! A click or keystroke by Myth adds a short bright pulse on the edge next to the element.
//!
//! **Frame budget.** [`Glow::next_frame`] says when the next frame is due: never while
//! nothing is shown or everything shown is static, at most every [`FRAME_INTERVAL`] (30 fps)
//! while something moves. With reduced motion every state is drawn static (one frame when
//! it changes); flashes and pulses then show as a static tint for their duration.

use std::time::{Duration, Instant};

/// 30 fps while the ring animates.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// One lap of the flowing gradient.
pub const FLOW_PERIOD: Duration = Duration::from_millis(5000);
pub const BREATHE_PERIOD: Duration = Duration::from_millis(3200);
pub const APPROVAL_PERIOD: Duration = Duration::from_millis(1600);
pub const SWEEP_PERIOD: Duration = Duration::from_millis(1600);
pub const FLASH: Duration = Duration::from_millis(900);
pub const PULSE: Duration = Duration::from_millis(700);

/// The user's motion setting combined with the OS's ("reduce animations").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    Full,
    /// No movement: static glow, fades only.
    Reduced,
    /// No animation at all (glow still shows the state, statically).
    Off,
}

impl Motion {
    pub fn parse(s: &str) -> Option<Motion> {
        match s {
            "full" => Some(Motion::Full),
            "reduced" => Some(Motion::Reduced),
            "off" => Some(Motion::Off),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Motion::Full => "full",
            Motion::Reduced => "reduced",
            Motion::Off => "off",
        }
    }
    /// The saved setting, or `None` to follow the OS ("Animation effects" on Windows,
    /// `enable-animations` on GNOME): the OS setting gives `full` or `reduced` (fades only).
    /// A setting the user chose wins over the OS.
    pub fn resolve(setting: Option<Motion>, os_reduced: bool) -> Motion {
        match setting {
            Some(m) => m,
            None if os_reduced => Motion::Reduced,
            None => Motion::Full,
        }
    }
    /// Setting + OS preference: an OS "reduce animations" turns `full` into `reduced`.
    pub fn effective(setting: Motion, os_reduced: bool) -> Motion {
        match (setting, os_reduced) {
            (Motion::Full, true) => Motion::Reduced,
            (m, _) => m,
        }
    }
    pub fn animates(self) -> bool {
        self == Motion::Full
    }
}

/// What Myth is doing on the tab shown in the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiActivity {
    Thinking,
    Controlling,
    Approval,
}

/// The steady (non-flash) state of the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlowState {
    Off,
    Loading,
    Thinking,
    Controlling,
    Approval,
}

impl GlowState {
    /// Pick the state for the card from what is going on (see the module table).
    pub fn pick(card: bool, ai: Option<AiActivity>, loading: bool) -> GlowState {
        if !card {
            return GlowState::Off;
        }
        match ai {
            Some(AiActivity::Approval) => GlowState::Approval,
            Some(AiActivity::Controlling) => GlowState::Controlling,
            Some(AiActivity::Thinking) => GlowState::Thinking,
            None if loading => GlowState::Loading,
            None => GlowState::Off,
        }
    }
    fn animated(self) -> bool {
        !matches!(self, GlowState::Off)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flash {
    Done,
    Error,
}

/// Shader mode of a frame (matches `compose.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlowMode {
    /// Whole ring, colour flowing along the perimeter by `phase`.
    Flow = 0,
    /// Whole ring, one colour (breathing / pulsing via `alpha`).
    Ring = 1,
    /// Top edge only, a bright segment at `phase` (0 = left, 1 = right).
    Sweep = 2,
}

/// What to draw this frame. Colours are linear-ish sRGB 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowFrame {
    pub mode: GlowMode,
    pub colors: [[f32; 3]; 3],
    /// Overall strength 0..1.
    pub alpha: f32,
    /// Flow position / sweep position, 0..1.
    pub phase: f32,
    /// A pulse on the ring: point in card coordinates (0..1, 0..1) and its strength 0..1.
    pub pulse: Option<(f32, f32, f32)>,
}

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 3] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}
/// MythDesk sky blue and warm horizon, saturated enough to read against the sky and on a
/// white page (alpha peaks at ~0.85, so never neon).
pub const BLUE: [f32; 3] = rgb(0x3d, 0x7f, 0xd9);
pub const BLUE_LIGHT: [f32; 3] = rgb(0x7f, 0xb0, 0xec);
pub const ORANGE: [f32; 3] = rgb(0xf2, 0xa6, 0x5a);
pub const MAGENTA: [f32; 3] = rgb(0xd0, 0x5c, 0xa8);
pub const AMBER: [f32; 3] = rgb(0xf0, 0xa0, 0x20);
pub const AMBER_LIGHT: [f32; 3] = rgb(0xff, 0xc8, 0x5c);
pub const GREEN: [f32; 3] = rgb(0x34, 0xc4, 0x7c);
pub const SOFT_RED: [f32; 3] = rgb(0xe0, 0x5f, 0x5f);
/// Night (dark tone): the same hues, brighter, so they glow on the dark sky.
pub const BLUE_NIGHT: [f32; 3] = rgb(0x6c, 0xaa, 0xff);
pub const BLUE_LIGHT_NIGHT: [f32; 3] = rgb(0xa8, 0xcc, 0xff);
pub const ORANGE_NIGHT: [f32; 3] = rgb(0xff, 0xbe, 0x7a);
pub const MAGENTA_NIGHT: [f32; 3] = rgb(0xf0, 0x80, 0xc8);
pub const GREEN_NIGHT: [f32; 3] = rgb(0x5c, 0xe0, 0x9c);
pub const SOFT_RED_NIGHT: [f32; 3] = rgb(0xff, 0x80, 0x80);
/// Peak strength of the glow.
pub const PEAK_ALPHA: f32 = 0.85;

#[derive(Debug, Clone, Copy)]
pub struct Glow {
    state: GlowState,
    since: Instant,
    flash: Option<(Flash, Instant)>,
    pulse: Option<(f32, f32, Instant)>,
    /// A static frame must be drawn once (state changed while not animating).
    dirty: bool,
    /// Dark tone (night / evening sky): brighter colours.
    night: bool,
}

fn frac(d: Duration, period: Duration) -> f32 {
    (d.as_secs_f64() / period.as_secs_f64()).fract() as f32
}

impl Glow {
    pub fn new(now: Instant) -> Glow {
        Glow {
            state: GlowState::Off,
            since: now,
            flash: None,
            pulse: None,
            dirty: false,
            night: false,
        }
    }

    pub fn state(&self) -> GlowState {
        self.state
    }

    /// The chrome's tone changed (dark sky: brighter glow). Returns true if it changed.
    pub fn set_night(&mut self, night: bool) -> bool {
        if self.night == night {
            return false;
        }
        self.night = night;
        self.dirty = true;
        true
    }

    /// Set the steady state. Returns true if it changed (the caller redraws).
    pub fn set(&mut self, state: GlowState, now: Instant) -> bool {
        if state == self.state {
            return false;
        }
        self.state = state;
        self.since = now;
        self.dirty = true;
        true
    }

    /// A run ended on the card's tab.
    pub fn flash(&mut self, kind: Flash, now: Instant) {
        self.flash = Some((kind, now));
        self.dirty = true;
    }

    /// Myth clicked / typed at this point of the card (0..1 each way).
    pub fn pulse(&mut self, x: f32, y: f32, now: Instant) {
        self.pulse = Some((x.clamp(0.0, 1.0), y.clamp(0.0, 1.0), now));
        self.dirty = true;
    }

    /// Hide everything at once (tab switched to an internal page, closed...).
    pub fn clear(&mut self, now: Instant) -> bool {
        let had = self.state != GlowState::Off || self.flash.is_some() || self.pulse.is_some();
        self.set(GlowState::Off, now);
        self.flash = None;
        self.pulse = None;
        had
    }

    fn live_flash(&self, now: Instant) -> Option<(Flash, f32)> {
        let (k, at) = self.flash?;
        let t = now.saturating_duration_since(at);
        (t < FLASH).then(|| (k, t.as_secs_f32() / FLASH.as_secs_f32()))
    }

    fn live_pulse(&self, now: Instant) -> Option<(f32, f32, f32)> {
        let (x, y, at) = self.pulse?;
        let t = now.saturating_duration_since(at);
        (t < PULSE).then(|| (x, y, t.as_secs_f32() / PULSE.as_secs_f32()))
    }

    /// Anything to draw at all?
    pub fn visible(&self, now: Instant) -> bool {
        self.state != GlowState::Off
            || self.live_flash(now).is_some()
            || self.live_pulse(now).is_some()
    }

    /// When the next frame is due after a frame drawn at `last` (`None`: no frame needed
    /// until something changes). Idle and static states cost no frames.
    pub fn next_frame(&self, last: Instant, now: Instant, motion: Motion) -> Option<Instant> {
        if self.dirty {
            return Some(now);
        }
        let flash_end = self
            .live_flash(now)
            .map(|_| self.flash.expect("live").1 + FLASH);
        let pulse_end = self
            .live_pulse(now)
            .map(|_| self.pulse.expect("live").2 + PULSE);
        if motion.animates() {
            let moving = self.state.animated() || flash_end.is_some() || pulse_end.is_some();
            return moving.then_some(last + FRAME_INTERVAL);
        }
        // Static: one frame at the end of a flash / pulse to remove it.
        match (flash_end, pulse_end) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// Note that a frame was drawn (clears the "draw once" flag).
    pub fn drawn(&mut self) {
        self.dirty = false;
    }

    /// The frame to draw now, or `None` if nothing shows.
    pub fn frame(&self, now: Instant, motion: Motion) -> Option<GlowFrame> {
        let anim = motion.animates();
        let t = now.saturating_duration_since(self.since);
        let wave = |period: Duration| {
            if anim {
                0.5 - 0.5 * (frac(t, period) * std::f32::consts::TAU).cos()
            } else {
                0.6
            }
        };
        let pulse = self.live_pulse(now).map(|(x, y, p)| {
            let a = if anim { (1.0 - p) * (1.0 - p) } else { 0.7 };
            (x, y, a)
        });
        let n = self.night;
        let pick = |day: [f32; 3], night: [f32; 3]| if n { night } else { day };
        let (blue, blue_light) = (pick(BLUE, BLUE_NIGHT), pick(BLUE_LIGHT, BLUE_LIGHT_NIGHT));
        let steady = match self.state {
            GlowState::Off => None,
            GlowState::Controlling => Some(GlowFrame {
                mode: GlowMode::Flow,
                colors: [
                    blue,
                    pick(ORANGE, ORANGE_NIGHT),
                    pick(MAGENTA, MAGENTA_NIGHT),
                ],
                alpha: PEAK_ALPHA,
                phase: if anim { frac(t, FLOW_PERIOD) } else { 0.0 },
                pulse,
            }),
            GlowState::Thinking => Some(GlowFrame {
                mode: GlowMode::Ring,
                colors: [blue, blue_light, blue],
                alpha: 0.5 + 0.35 * wave(BREATHE_PERIOD),
                phase: 0.0,
                pulse,
            }),
            GlowState::Approval => Some(GlowFrame {
                mode: GlowMode::Ring,
                colors: [AMBER, AMBER_LIGHT, AMBER],
                // A clear pulse: from half strength to full.
                alpha: 0.5 + 0.5 * wave(APPROVAL_PERIOD),
                phase: 0.0,
                pulse,
            }),
            GlowState::Loading => Some(GlowFrame {
                mode: GlowMode::Sweep,
                colors: [blue_light, blue, ORANGE],
                alpha: 0.7,
                phase: if anim { frac(t, SWEEP_PERIOD) } else { 0.5 },
                pulse,
            }),
        };
        if let Some((kind, p)) = self.live_flash(now) {
            // A flash replaces the steady ring while it lasts (runs end -> state drops anyway).
            let color = match kind {
                Flash::Done => pick(GREEN, GREEN_NIGHT),
                Flash::Error => pick(SOFT_RED, SOFT_RED_NIGHT),
            };
            // Quick rise, slow fade.
            let alpha = if anim {
                PEAK_ALPHA * (p / 0.15).min(1.0) * (1.0 - p).powf(1.5)
            } else {
                0.7
            };
            return Some(GlowFrame {
                mode: GlowMode::Ring,
                colors: [color, color, color],
                alpha,
                phase: 0.0,
                pulse,
            });
        }
        steady.or_else(|| {
            pulse.map(|pu| GlowFrame {
                mode: GlowMode::Ring,
                colors: [blue, blue, blue],
                alpha: 0.0,
                phase: 0.0,
                pulse: Some(pu),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn priorities_and_internal_pages() {
        use AiActivity::*;
        assert_eq!(GlowState::pick(false, Some(Approval), true), GlowState::Off);
        assert_eq!(GlowState::pick(true, None, false), GlowState::Off);
        assert_eq!(GlowState::pick(true, None, true), GlowState::Loading);
        assert_eq!(
            GlowState::pick(true, Some(Thinking), true),
            GlowState::Thinking
        );
        assert_eq!(
            GlowState::pick(true, Some(Controlling), true),
            GlowState::Controlling
        );
        assert_eq!(
            GlowState::pick(true, Some(Approval), false),
            GlowState::Approval
        );
    }

    #[test]
    fn idle_costs_no_frames() {
        let t0 = Instant::now();
        let mut g = Glow::new(t0);
        assert_eq!(g.next_frame(t0, t0, Motion::Full), None);
        assert!(g.frame(t0, Motion::Full).is_none());
        assert!(!g.set(GlowState::Off, t0), "no change, no redraw");
        assert_eq!(g.next_frame(t0, t0 + ms(500), Motion::Full), None);
    }

    #[test]
    fn animates_at_30fps_only_while_active() {
        let t0 = Instant::now();
        let mut g = Glow::new(t0);
        assert!(g.set(GlowState::Controlling, t0));
        assert_eq!(g.next_frame(t0, t0, Motion::Full), Some(t0), "draw now");
        g.drawn();
        let f = t0 + ms(10);
        assert_eq!(g.next_frame(f, f, Motion::Full), Some(f + FRAME_INTERVAL));
        // Flow moves.
        let a = g.frame(t0 + ms(100), Motion::Full).unwrap();
        let b = g.frame(t0 + ms(1100), Motion::Full).unwrap();
        assert_eq!(a.mode, GlowMode::Flow);
        assert!((b.phase - a.phase - 0.2).abs() < 0.01, "5 s per lap");
        // Back to idle: no more frames after the one that clears it.
        assert!(g.set(GlowState::Off, f));
        assert_eq!(g.next_frame(f, f, Motion::Full), Some(f));
        g.drawn();
        assert_eq!(g.next_frame(f, f + ms(40), Motion::Full), None);
    }

    #[test]
    fn reduced_motion_is_static() {
        let t0 = Instant::now();
        for motion in [Motion::Reduced, Motion::Off] {
            let mut g = Glow::new(t0);
            g.set(GlowState::Approval, t0);
            assert_eq!(g.next_frame(t0, t0, motion), Some(t0));
            g.drawn();
            assert_eq!(
                g.next_frame(t0, t0 + ms(5), motion),
                None,
                "static: no timer"
            );
            let a = g.frame(t0, motion).unwrap();
            let b = g.frame(t0 + ms(1234), motion).unwrap();
            assert_eq!(a, b, "does not change over time");
        }
        assert_eq!(Motion::effective(Motion::Full, true), Motion::Reduced);
        assert_eq!(Motion::effective(Motion::Off, false), Motion::Off);
        assert_eq!(Motion::effective(Motion::Full, false), Motion::Full);
        assert_eq!(Motion::resolve(None, true), Motion::Reduced);
        assert_eq!(Motion::resolve(None, false), Motion::Full);
        assert_eq!(Motion::resolve(Some(Motion::Full), true), Motion::Full);
        assert_eq!(Motion::resolve(Some(Motion::Off), false), Motion::Off);
    }

    #[test]
    fn flash_is_one_shot() {
        let t0 = Instant::now();
        let mut g = Glow::new(t0);
        g.set(GlowState::Controlling, t0);
        g.drawn();
        // The run ends: state drops and a done flash plays once.
        g.set(GlowState::Off, t0 + ms(1000));
        g.flash(Flash::Done, t0 + ms(1000));
        g.drawn();
        let mid = t0 + ms(1300);
        let f = g.frame(mid, Motion::Full).unwrap();
        assert!(f.alpha > 0.0 && f.alpha < PEAK_ALPHA);
        assert_eq!(f.colors[0], GREEN, "done flashes green");
        assert_eq!(
            g.next_frame(mid, mid, Motion::Full),
            Some(mid + FRAME_INTERVAL)
        );
        let after = t0 + ms(2000);
        assert!(g.frame(after, Motion::Full).is_none());
        assert_eq!(g.next_frame(after, after, Motion::Full), None);
        assert!(!g.visible(after));
        // Reduced motion: shown static, one frame to remove it at the end.
        g.flash(Flash::Error, after);
        g.drawn();
        assert_eq!(
            g.next_frame(after, after + ms(1), Motion::Reduced),
            Some(after + FLASH)
        );
        assert_eq!(g.frame(after, Motion::Reduced).unwrap().colors[0], SOFT_RED);
    }

    #[test]
    fn pulse_marks_the_ring_and_fades() {
        let t0 = Instant::now();
        let mut g = Glow::new(t0);
        g.set(GlowState::Controlling, t0);
        g.pulse(0.25, 1.4, t0);
        let f = g.frame(t0 + ms(100), Motion::Full).unwrap();
        let (x, y, a) = f.pulse.unwrap();
        assert_eq!((x, y), (0.25, 1.0));
        assert!(a > 0.5);
        assert!(g.frame(t0 + ms(800), Motion::Full).unwrap().pulse.is_none());
    }

    #[test]
    fn colours_contrast_with_the_sky_and_brighten_at_night() {
        let t0 = Instant::now();
        let mut g = Glow::new(t0);
        g.set(GlowState::Controlling, t0);
        let day = g.frame(t0, Motion::Full).unwrap();
        assert_eq!(day.colors, [BLUE, ORANGE, MAGENTA]);
        assert!(day.alpha <= PEAK_ALPHA + 1e-6 && day.alpha >= 0.8);
        // Far from the day sky (#89b4de / #c3d8ec): sky-on-sky was invisible before.
        let sky = [
            0x89 as f32 / 255.0,
            0xb4 as f32 / 255.0,
            0xde as f32 / 255.0,
        ];
        let dist = |c: [f32; 3]| {
            ((c[0] - sky[0]).powi(2) + (c[1] - sky[1]).powi(2) + (c[2] - sky[2]).powi(2)).sqrt()
        };
        assert!(dist(BLUE) > 0.2 && dist(ORANGE) > 0.4 && dist(AMBER) > 0.4);
        assert!(g.set_night(true));
        assert!(!g.set_night(true));
        let night = g.frame(t0, Motion::Full).unwrap();
        assert_eq!(night.colors[0], BLUE_NIGHT);
        let sum = |c: [f32; 3]| c[0] + c[1] + c[2];
        assert!(sum(BLUE_NIGHT) > sum(BLUE) && sum(ORANGE_NIGHT) > sum(ORANGE));
    }

    #[test]
    fn approval_pulses_clearly_in_amber() {
        let t0 = Instant::now();
        let mut g = Glow::new(t0);
        g.set(GlowState::Approval, t0);
        let lo = g.frame(t0, Motion::Full).unwrap();
        let hi = g.frame(t0 + APPROVAL_PERIOD / 2, Motion::Full).unwrap();
        assert_eq!(lo.colors[0], AMBER);
        assert!(hi.alpha - lo.alpha > 0.4, "{} -> {}", lo.alpha, hi.alpha);
        assert!(hi.alpha <= 1.0);
        // Static under reduced motion, still clearly visible.
        let s = g.frame(t0 + ms(300), Motion::Reduced).unwrap();
        assert!(s.alpha >= 0.75);
    }

    #[test]
    fn clear_hides_everything() {
        let t0 = Instant::now();
        let mut g = Glow::new(t0);
        g.set(GlowState::Thinking, t0);
        g.flash(Flash::Done, t0);
        assert!(g.clear(t0));
        assert!(g.frame(t0, Motion::Full).is_none());
    }
}
