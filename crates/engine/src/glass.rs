// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Liquid glass, engine side (pure, no Servo, unit-tested): the protocol by which the UI pages
//! report their glass surfaces, their conversion to device pixels, the sky palette the shell
//! draws behind everything, and when the sky animates.
//!
//! Servo 0.1.4 has no `backdrop-filter`, so the shell draws the glass (`crates/shell/src/
//! glass.rs`): it renders the sky itself, puts the page in, blurs that backdrop under every
//! reported surface and then composes the UI webviews, cleared to transparent, on top. The UI
//! keeps drawing each surface's tint, hairline rim and (floating panels only) shadow in CSS,
//! so the look is the same with or without the shell's blur, only the backdrop differs.
//!
//! **Report** (`/api/glass`, from the chrome and from internal pages):
//! `r=x,y,w,h,radius,kind,alpha[,cx,cy,cw,ch];…` in CSS px of the reporting webview, where `kind` is
//! `c` (chrome-level: tabs, address bar, pill at rest), `p` (a panel/bubble) or `f` (a floating
//! panel), `alpha` the surface's own opacity 0–1 (a fading panel fades its backdrop too), and the optional `c*` is the clip rectangle (a scroll container), plus `h=` the
//! UI's hour of day (for the sky palette) and `mv=1` while something moves (the sky waits).

use crate::frame::PxRect;

/// Most surfaces one webview may report (more are dropped; the UI orders them by priority).
pub const MAX_REPORTED: usize = 24;
/// Largest accepted coordinate / size, CSS px.
const MAX_CSS: f32 = 16384.0;
/// Longest accepted `r=` parameter.
pub const MAX_REPORT_LEN: usize = 4096;

/// Which glass the UI and the shell draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlassMode {
    /// The shell draws the sky and blurs it (and the page) under every glass surface; the UI
    /// webviews are transparent.
    Blur,
    /// The shell draws the sky and the UI is transparent, but the blur is unavailable (a GL
    /// failure while running): the UI uses its denser fallback tint.
    Flat,
    /// The UI draws its own sky (sky.js) and uses the fallback tint; the shell composes the
    /// way it always has. Used when the GL pieces or the transparent webviews are missing.
    Off,
}

impl GlassMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GlassMode::Blur => "blur",
            GlassMode::Flat => "flat",
            GlassMode::Off => "off",
        }
    }

    /// The shell draws the sky and the UI webviews are transparent.
    pub fn shell_sky(self) -> bool {
        self != GlassMode::Off
    }
}

/// What a surface is, which sets its blur radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlassKind {
    /// Tabs, address bar, Myth pill at rest, small controls on the sky (`--lg-blur` 20px).
    Chrome,
    /// Panels and bubbles flush with the page (24px).
    Panel,
    /// Floating panels: Tasks, compat, settings sheet, approval card (28px).
    Float,
}

impl GlassKind {
    /// Backdrop blur in CSS px (the prototype's `--lg-blur` per surface).
    pub fn blur_css(self) -> f32 {
        match self {
            GlassKind::Chrome => 20.0,
            GlassKind::Panel => 24.0,
            GlassKind::Float => 28.0,
        }
    }
}

/// One reported surface, CSS px of the reporting webview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassRectCss {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub kind: GlassKind,
    /// The surface's opacity (0–1), so a fading panel fades its blurred backdrop with it.
    pub alpha: f32,
    /// Visible part (a scroll container); the surface is cut off outside it.
    pub clip: Option<[f32; 4]>,
}

/// Parse the `r=` parameter. Malformed entries make the whole report invalid (the UI builds
/// it, so a bad one is a bug worth noticing); empty means "no glass now".
pub fn parse_report(s: &str) -> Result<Vec<GlassRectCss>, String> {
    if s.len() > MAX_REPORT_LEN {
        return Err("glass report too long".into());
    }
    let mut out = Vec::new();
    for entry in s.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let parts: Vec<&str> = entry.split(',').map(str::trim).collect();
        if parts.len() != 7 && parts.len() != 11 {
            return Err(format!("bad glass entry {entry:?}"));
        }
        let num = |i: usize| -> Result<f32, String> {
            parts[i]
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite() && v.abs() <= MAX_CSS)
                .ok_or_else(|| format!("bad number in glass entry {entry:?}"))
        };
        let kind = match parts[5] {
            "c" => GlassKind::Chrome,
            "p" => GlassKind::Panel,
            "f" => GlassKind::Float,
            _ => return Err(format!("bad kind in glass entry {entry:?}")),
        };
        let (w, h) = (num(2)?, num(3)?);
        if w < 0.0 || h < 0.0 {
            return Err(format!("negative size in glass entry {entry:?}"));
        }
        let alpha = num(6)?.clamp(0.0, 1.0);
        let clip = if parts.len() == 11 {
            let (cw, ch) = (num(9)?, num(10)?);
            if cw < 0.0 || ch < 0.0 {
                return Err(format!("negative clip in glass entry {entry:?}"));
            }
            Some([num(7)?, num(8)?, cw, ch])
        } else {
            None
        };
        if out.len() < MAX_REPORTED {
            out.push(GlassRectCss {
                x: num(0)?,
                y: num(1)?,
                w,
                h,
                radius: num(4)?.max(0.0),
                kind,
                alpha,
                clip,
            });
        }
    }
    Ok(out)
}

/// A surface in window device px (top-left origin), clipped to `clip`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassRectPx {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub kind: GlassKind,
    /// Opacity 0–1 (see [`GlassRectCss::alpha`]).
    pub alpha: f32,
    /// Visible part: x0, y0, x1, y1 (window device px, top-left origin).
    pub clip: [f32; 4],
}

/// Convert a webview's report to window device px: scale by `scale`, move by the webview's
/// origin in the window, and cut off at the webview's own rectangle (`area`) and each
/// surface's clip. Surfaces with nothing left visible are dropped.
pub fn to_device(rects: &[GlassRectCss], scale: f64, area: PxRect) -> Vec<GlassRectPx> {
    let s = scale.max(0.1) as f32;
    let (ox, oy) = (area.x as f32, area.y as f32);
    let a = [ox, oy, ox + area.w as f32, oy + area.h as f32];
    rects
        .iter()
        .filter_map(|r| {
            let mut clip = a;
            if let Some([cx, cy, cw, ch]) = r.clip {
                let c = [
                    ox + cx * s,
                    oy + cy * s,
                    ox + (cx + cw) * s,
                    oy + (cy + ch) * s,
                ];
                clip = [
                    clip[0].max(c[0]),
                    clip[1].max(c[1]),
                    clip[2].min(c[2]),
                    clip[3].min(c[3]),
                ];
            }
            let g = GlassRectPx {
                x: ox + r.x * s,
                y: oy + r.y * s,
                w: r.w * s,
                h: r.h * s,
                radius: (r.radius * s)
                    .min(r.w * s / 2.0)
                    .min(r.h * s / 2.0)
                    .max(0.0),
                kind: r.kind,
                alpha: r.alpha,
                clip,
            };
            // Visible part of the surface itself.
            let vis = [
                g.x.max(clip[0]),
                g.y.max(clip[1]),
                (g.x + g.w).min(clip[2]),
                (g.y + g.h).min(clip[3]),
            ];
            (vis[2] - vis[0] >= 1.0
                && vis[3] - vis[1] >= 1.0
                && g.w >= 1.0
                && g.h >= 1.0
                && g.alpha > 0.004)
                .then_some(g)
        })
        .collect()
}

// ------------------------------------------------------------------------ transparency

/// Settings > Appearance > Transparency, 0–100. The default is the approved "B" look
/// (design notes); 100 is "C"; 0 is a step and a half more solid than B,
/// i.e. more solid than "A" (half way back towards the old foggy look).
pub const TRANSPARENCY_DEFAULT: u8 = 50;

/// Where 0 and 100 sit, in "steps from B" (A = −1, C = +1).
pub const LEVEL_MIN: f32 = -1.5;
pub const LEVEL_MAX: f32 = 1.0;

/// The one number every glass token is interpolated from (`--glass-level` in tokens.css):
/// steps from the B look, piecewise linear so that 0 → [`LEVEL_MIN`], the default (50) → 0
/// and 100 → [`LEVEL_MAX`] (C). Out-of-range input is clamped.
pub fn glass_level(transparency: u8) -> f32 {
    let t = f32::from(transparency.min(100));
    let d = f32::from(TRANSPARENCY_DEFAULT);
    if t <= d {
        LEVEL_MIN * (d - t) / d
    } else {
        LEVEL_MAX * (t - d) / (100.0 - d)
    }
}

/// Parse the slider value from an RPC / preference: an integer 0–100, nothing else.
pub fn parse_transparency(s: &str) -> Option<u8> {
    let s = s.trim();
    if s.is_empty() || s.len() > 3 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u8>().ok().filter(|v| *v <= 100)
}

/// The user's Appearance setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    /// Transparency slider, 0–100.
    pub transparency: u8,
    /// Reduce transparency: `Some` when the user set the switch, `None` = follow the OS
    /// (Windows "Transparency effects").
    pub reduce: Option<bool>,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            transparency: TRANSPARENCY_DEFAULT,
            reduce: None,
        }
    }
}

impl Appearance {
    /// Whether surfaces are solid now: the user's switch, else the OS setting.
    pub fn reduced(&self, os_reduced: bool) -> bool {
        self.reduce.unwrap_or(os_reduced)
    }

    /// `on` / `off` / `system`, as the preference RPCs spell it.
    pub fn reduce_str(&self) -> &'static str {
        match self.reduce {
            Some(true) => "on",
            Some(false) => "off",
            None => "system",
        }
    }
}

/// Parse `on` / `off` / `system` (the Reduce transparency switch).
pub fn parse_reduce(s: &str) -> Option<Option<bool>> {
    match s {
        "on" | "1" => Some(Some(true)),
        "off" | "0" => Some(Some(false)),
        "system" => Some(None),
        _ => None,
    }
}

/// What the shell's blur does at a transparency level (mirrors `--lg-blur-k`, `--lg-sat` and
/// `--lg-bright` in tokens.css). Every term is clamped to a range that still looks like glass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassLook {
    /// Multiplies each surface kind's base blur ([`GlassKind::blur_css`]); B = 0.5.
    pub blur_k: f32,
    /// Backdrop saturation; B = 1.5.
    pub saturation: f32,
    /// Backdrop brightness; B = 1.08 in the light tone, 0.92 in the dark tone.
    pub brightness: f32,
    /// Reduce transparency: no blur at all (the UI draws solid surfaces).
    pub solid: bool,
}

pub const BLUR_K_RANGE: (f32, f32) = (0.3, 0.8);
pub const SATURATION_RANGE: (f32, f32) = (1.0, 1.7);
pub const BRIGHTNESS_RANGE: (f32, f32) = (0.85, 1.15);

impl GlassLook {
    pub fn at(transparency: u8, dark: bool, reduced: bool) -> GlassLook {
        let g = glass_level(transparency);
        let clamp = |v: f32, (lo, hi): (f32, f32)| v.clamp(lo, hi);
        GlassLook {
            blur_k: clamp(0.5 - 0.165 * g, BLUR_K_RANGE),
            saturation: clamp(1.5 + 0.1 * g, SATURATION_RANGE),
            brightness: clamp(
                if dark {
                    0.92 - 0.04 * g
                } else {
                    1.08 + 0.045 * g
                },
                BRIGHTNESS_RANGE,
            ),
            solid: reduced,
        }
    }

    /// The backdrop blur for a surface kind, CSS px (0 when solid).
    pub fn blur_css(&self, kind: GlassKind) -> f32 {
        if self.solid {
            0.0
        } else {
            kind.blur_css() * self.blur_k
        }
    }
}

/// Lowest tint (alpha) of a surface that floats over an arbitrary web page (Myth pill /
/// approval card, Tasks over a page, compat panel, peek), whatever the slider says
/// (`--lg-tint-float` in tokens.css): A's strong tint, where B lost legibility over dark photos.
pub const FLOAT_FLOOR_LIGHT: f32 = 0.40;
pub const FLOAT_FLOOR_DARK: f32 = 0.44;

/// The tint alphas tokens.css computes at a level (for tests and the docs; the UI computes
/// them itself from `--glass-level`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tints {
    /// `--lg-tint` (bubbles, composer, sidebar, chrome).
    pub tint: f32,
    /// `--lg-tint-strong` (settings sheet, Tasks on internal pages, omnibox).
    pub strong: f32,
    /// `--lg-tint-float` (surfaces over web pages): `max(floor, strong)`.
    pub float: f32,
    /// `--scrim` behind the settings sheet.
    pub scrim: f32,
}

impl Tints {
    pub fn at(transparency: u8, dark: bool) -> Tints {
        let g = glass_level(transparency);
        let (tint, strong, scrim, floor) = if dark {
            (
                0.22 - 0.06 * g,
                0.34 - 0.09 * g,
                0.20 - 0.04 * g,
                FLOAT_FLOOR_DARK,
            )
        } else {
            (
                0.14 - 0.06 * g,
                0.30 - 0.10 * g,
                0.08 - 0.02 * g,
                FLOAT_FLOOR_LIGHT,
            )
        };
        Tints {
            tint,
            strong,
            float: strong.max(floor),
            scrim,
        }
    }
}

// ------------------------------------------------------------------------ blur passes

/// Deepest dual-Kawase level the shell builds (¼ … 1/32 below the ½-resolution copy).
pub const KAWASE_MAX_DEPTH: usize = 4;
/// Effective Gaussian sigma (full-resolution px; CSS `blur(r)` has sigma = r) of the shell's
/// pipeline at each depth with the pass offset at 1.0, measured by simulating its exact taps
/// (a blur simulation, and the `kawase_matches_css_blur` test below).
const KAWASE_SIGMA: [f32; KAWASE_MAX_DEPTH] = [5.28, 11.71, 23.97, 48.21];
/// Pass offsets outside this range start to show the taps (holes / ringing).
const KAWASE_OFF: (f32, f32) = (0.35, 1.35);

/// Depth and pass offset that make the shell's dual-Kawase blur match CSS `blur(px)` (px at
/// full resolution). The offset is continuous, so every radius really changes the blur
/// (the old depth-only mapping gave 12 px and 24 px the same depth and the same look).
pub fn kawase_for_blur(blur: f32) -> (usize, f32) {
    let b = if blur.is_finite() { blur.max(0.0) } else { 0.0 };
    for (i, sigma) in KAWASE_SIGMA.iter().enumerate() {
        if b <= sigma * KAWASE_OFF.1 {
            return (i + 1, (b / sigma).clamp(KAWASE_OFF.0, KAWASE_OFF.1));
        }
    }
    (KAWASE_MAX_DEPTH, KAWASE_OFF.1)
}

// --------------------------------------------------------------------------------- sky

/// theme.js `KEYS`: hour, top, mid, low, glow, star alpha, cloud light.
const KEYS: [(f32, u32, u32, u32, u32, f32, f32); 11] = [
    (0.0, 0x04060f, 0x0a1128, 0x1b1a3a, 0x3a2f6a, 1.0, 0.10),
    (4.5, 0x060a1c, 0x161c45, 0x3b2f5c, 0x7a4f7a, 0.8, 0.12),
    (6.0, 0x3a4677, 0xb48aa0, 0xf2b49a, 0xffc9a0, 0.2, 0.45),
    (7.2, 0x7499cb, 0xc0b9c8, 0xe0cbba, 0xeedac4, 0.0, 0.68),
    (10.0, 0x6698d2, 0xa2c1e2, 0xd0d8de, 0xece5d6, 0.0, 0.72),
    (13.0, 0x6094d2, 0x9ebfe3, 0xccd6de, 0xe9e6de, 0.0, 0.72),
    (16.5, 0x6888bf, 0xadaec3, 0xdcc5b0, 0xecd2b3, 0.0, 0.66),
    (18.3, 0x2b4386, 0x6670ab, 0xec9f7c, 0xffad6e, 0.0, 0.45),
    (19.3, 0x1f2d63, 0x4b5a98, 0xc48c86, 0xf08d68, 0.15, 0.30),
    (20.5, 0x0e1638, 0x222b5a, 0x4a3a62, 0x8a5a78, 0.6, 0.15),
    (24.0, 0x04060f, 0x0a1128, 0x1b1a3a, 0x3a2f6a, 1.0, 0.10),
];

/// sky.js: Kuala Lumpur is near the equator, about 7:10 to 19:15 all year.
const SUNRISE: f32 = 7.1;
const SUNSET: f32 = 19.2;

/// The sky's colours at an hour (theme.js `palette`), linear 0..1 sRGB values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub top: [f32; 3],
    pub mid: [f32; 3],
    pub low: [f32; 3],
    pub glow: [f32; 3],
    pub stars: f32,
    pub cloud: f32,
    pub hour: f32,
}

fn rgb(c: u32) -> [f32; 3] {
    [
        ((c >> 16) & 255) as f32 / 255.0,
        ((c >> 8) & 255) as f32 / 255.0,
        (c & 255) as f32 / 255.0,
    ]
}

pub fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// theme.js `palette(hour)`; hours outside 0..24 wrap.
pub fn palette(hour: f32) -> Palette {
    let hour = if hour.is_finite() {
        hour.rem_euclid(24.0)
    } else {
        12.0
    };
    let mut i = 0;
    while i < KEYS.len() - 2 && hour >= KEYS[i + 1].0 {
        i += 1;
    }
    let (a, b) = (KEYS[i], KEYS[i + 1]);
    let t = ((hour - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    Palette {
        top: mix3(rgb(a.1), rgb(b.1), t),
        mid: mix3(rgb(a.2), rgb(b.2), t),
        low: mix3(rgb(a.3), rgb(b.3), t),
        glow: mix3(rgb(a.4), rgb(b.4), t),
        stars: a.5 + (b.5 - a.5) * t,
        cloud: a.6 + (b.6 - a.6) * t,
        hour,
    }
}

impl Palette {
    /// Sun (by day) or moon position: x, y in 0..1 of the window (y down), and whether it is
    /// day (sky.js arc).
    pub fn sun(&self) -> (f32, f32, bool) {
        let h = self.hour;
        let day = (SUNRISE - 0.6..=SUNSET + 0.6).contains(&h);
        let k = if day {
            (h - (SUNRISE - 0.6)) / (SUNSET - SUNRISE + 1.2)
        } else {
            ((h - SUNSET - 0.6 + 24.0) % 24.0) / (24.0 - (SUNSET - SUNRISE + 1.2))
        };
        (
            0.08 + 0.84 * k,
            0.95 - 0.75 * (std::f32::consts::PI * k).sin(),
            day,
        )
    }

    /// The title-bar cloud deck (the prototype's calmer deck): lit tops, undersides, and
    /// strength. Moonlit blue-grey at night, white by day, warm at dusk.
    pub fn deck(&self) -> ([f32; 3], [f32; 3], f32) {
        if self.stars > 0.3 {
            (
                mix3(self.mid, [0.541, 0.627, 0.8], 0.55),
                mix3(self.top, [0.102, 0.141, 0.29], 0.5),
                0.26,
            )
        } else if self.cloud > 0.6 {
            let lit = mix3(self.glow, [1.0; 3], 0.5);
            (
                mix3(lit, [1.0; 3], 0.55),
                mix3(self.top, self.mid, 0.35),
                0.42,
            )
        } else {
            (
                mix3(self.glow, [1.0, 0.925, 0.84], 0.35),
                mix3(self.top, self.low, 0.25),
                0.28,
            )
        }
    }

    /// Dark tone (dusk, evening, night), as theme.js `tone`.
    pub fn dark(&self) -> bool {
        let h = self.hour;
        !((6.2..17.3).contains(&h))
    }
}

// ------------------------------------------------------------------------- sky scene

/// Clouds in the sky (sky.js `clouds`).
pub const SKY_CLOUDS: usize = 10;
/// Most puffs over all clouds (3–5 each).
pub const SKY_CLOUD_PUFFS: usize = SKY_CLOUDS * 5;
/// Cloud-deck banks behind the title bar and toolbar (sky.js `deck`).
pub const SKY_BANKS: usize = 11;
/// Most puffs over all banks (4–6 each).
pub const SKY_BANK_PUFFS: usize = SKY_BANKS * 6;

#[derive(Debug, Clone, Copy)]
struct Puff {
    dx: f64,
    dy: f64,
    s: f64,
}

#[derive(Debug, Clone)]
struct Cloud {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    v: f64,
    a: f64,
    warm: bool,
    bob: f64,
    puffs: Vec<Puff>,
}

#[derive(Debug, Clone, Copy)]
struct Wash {
    y: f64,
    r: f64,
    v: f64,
    p: f64,
}

#[derive(Debug, Clone)]
struct Bank {
    x: f64,
    y: f64,
    w: f64,
    v: f64,
    a: f64,
    puffs: Vec<Puff>,
}

/// The sky's fixed layout: the same seeded random draws, in the same order, as
/// `ui/design/sky.js` (and the approved prototype), so the shell's sky has the prototype's
/// clouds, washes and deck. Built once; [`SkyScene::frame`] places it at a moment.
#[derive(Debug, Clone)]
pub struct SkyScene {
    clouds: Vec<Cloud>,
    washes: Vec<Wash>,
    deck: Vec<Bank>,
}

/// One sky frame for the shell's sky shader, in CSS px of the window (y down).
#[derive(Debug, Clone, PartialEq)]
pub struct SkyFrame {
    /// Washes: centre x, y, radius, unused.
    pub washes: [[f32; 4]; 3],
    /// Sun (by day) or moon: centre x, y, glow radius, disc radius.
    pub sun: [f32; 4],
    /// Ray rotation (radians) and day (1) / night (0).
    pub rays: [f32; 2],
    /// Ray lengths (CSS px), 12.
    pub ray_len: [f32; 12],
    /// Cloud puffs, in drawing order: centre x, y, horizontal radius, vertical radius.
    pub cloud_puffs: Vec<[f32; 4]>,
    /// Per cloud puff: centre alpha (palette cloud light × cloud alpha), warm (1) or not (0).
    pub cloud_alpha: Vec<[f32; 2]>,
    /// Deck banks: bank radius R and alpha.
    pub banks: Vec<[f32; 2]>,
    /// Deck puffs: centre x, base centre y (before the layer offset), radius, bank index.
    pub bank_puffs: Vec<[f32; 4]>,
    /// Deck height (CSS px).
    pub deck_h: f32,
}

/// sky.js's random numbers (Park–Miller, seed 7).
struct Rnd(u64);

impl Rnd {
    fn next(&mut self) -> f64 {
        self.0 = (self.0 * 16807) % 2147483647;
        self.0 as f64 / 2147483647.0
    }
}

impl Default for SkyScene {
    fn default() -> Self {
        Self::new()
    }
}

impl SkyScene {
    pub fn new() -> Self {
        let mut r = Rnd(7);
        // 140 stars (x, y, r, p): the shell draws its own star field, but the draws must be
        // made so the clouds after them come out the same.
        for _ in 0..140 * 4 {
            r.next();
        }
        let clouds = (0..SKY_CLOUDS)
            .map(|i| {
                let y = 0.15 + r.next() * 0.65;
                let x = r.next();
                let w = 0.22 + r.next() * 0.3;
                let h = 0.06 + r.next() * 0.07;
                let v = 0.006 + y * 0.012 + r.next() * 0.004;
                let a = 0.5 + r.next() * 0.5;
                let bob = r.next() * std::f64::consts::TAU;
                let n = 3 + (r.next() * 3.0).floor() as usize;
                let puffs = (0..n)
                    .map(|_| Puff {
                        dx: (r.next() - 0.5) * 1.2,
                        dy: (r.next() - 0.5) * 0.5,
                        s: 0.55 + r.next() * 0.5,
                    })
                    .collect();
                Cloud {
                    x,
                    y,
                    w,
                    h,
                    v,
                    a,
                    warm: i % 3 == 0,
                    bob,
                    puffs,
                }
            })
            .collect();
        let washes = (0..3)
            .map(|i| {
                let _x = r.next();
                Wash {
                    y: 0.2 + r.next() * 0.5,
                    r: 0.5 + r.next() * 0.4,
                    v: 0.00004 + r.next() * 0.00005,
                    p: i as f64 * 2.1,
                }
            })
            .collect();
        let deck = (0..SKY_BANKS)
            .map(|i| {
                let x = (i as f64 + r.next() * 0.6) / SKY_BANKS as f64;
                let y = 0.18 + r.next() * 0.62;
                let w = 0.07 + r.next() * 0.07;
                let v = 0.0016 + r.next() * 0.0022;
                let a = 0.6 + r.next() * 0.4;
                let n = 4 + (r.next() * 3.0).floor() as usize;
                let puffs = (0..n)
                    .map(|k| Puff {
                        dx: (k as f64 - 2.5) * 0.42 + (r.next() - 0.5) * 0.3,
                        dy: (r.next() - 0.5) * 0.35,
                        s: 0.55 + r.next() * 0.55,
                    })
                    .collect();
                Bank {
                    x,
                    y,
                    w,
                    v,
                    a,
                    puffs,
                }
            })
            .collect();
        SkyScene {
            clouds,
            washes,
            deck,
        }
    }

    /// The scene at sky clock `t` (seconds; sky.js uses the time of day in ms) for a `w`×`h`
    /// CSS px window, as sky.js `draw` would place it (its canvas is the window at 0.4×;
    /// everything here is in window CSS px).
    pub fn frame(&self, p: &Palette, t: f32, w: f32, h: f32) -> SkyFrame {
        let (w, h) = (w.max(1.0) as f64, h.max(1.0) as f64);
        let ms = t.max(0.0) as f64 * 1000.0;
        let mut washes = [[0.0f32; 4]; 3];
        for (o, c) in washes.iter_mut().zip(&self.washes) {
            let x = (0.5 + 0.45 * (ms * c.v + c.p).sin()) * w;
            let y = (c.y + 0.06 * (ms * c.v * 1.3 + c.p).cos()) * h;
            *o = [x as f32, y as f32, (c.r * w.max(h)) as f32, 0.0];
        }
        let (sx, sy, day) = p.sun();
        let big = w.max(h) * if day { 0.42 } else { 0.32 } * (1.0 + 0.04 * (ms / 1800.0).sin());
        // The disc: at least 2 px of the 0.4× canvas.
        let disc = (w.min(h) * 0.013).max(2.0 / 0.4);
        let mut ray_len = [0.0f32; 12];
        for (i, l) in ray_len.iter_mut().enumerate() {
            *l = (big * (0.6 + 0.25 * (ms / 2500.0 + i as f64 * 1.7).sin())) as f32;
        }
        let mut cloud_puffs = Vec::with_capacity(SKY_CLOUD_PUFFS);
        let mut cloud_alpha = Vec::with_capacity(SKY_CLOUD_PUFFS);
        for c in &self.clouds {
            let x = (((c.x + (ms / 1000.0) * c.v) % 1.6) - 0.3) * w;
            let y = (c.y + 0.006 * (ms / 3000.0 + c.bob).sin()) * h;
            for pf in &c.puffs {
                let r = c.w * w * 0.5 * pf.s;
                cloud_puffs.push([
                    (x + pf.dx * c.w * w * 0.5) as f32,
                    (y + pf.dy * c.h * h) as f32,
                    r as f32,
                    (r * c.h / c.w) as f32,
                ]);
                cloud_alpha.push([p.cloud * c.a as f32, if c.warm { 1.0 } else { 0.0 }]);
            }
        }
        let deck_h = (h * 0.2).max(150.0);
        let mut banks = Vec::with_capacity(SKY_BANKS);
        let mut bank_puffs = Vec::with_capacity(SKY_BANK_PUFFS);
        for (i, b) in self.deck.iter().enumerate() {
            let x = (((b.x + (ms / 1000.0) * b.v * 0.25) % 1.2) - 0.1) * w;
            let y = b.y * deck_h;
            let rr = b.w * w;
            banks.push([rr as f32, b.a as f32]);
            for pf in &b.puffs {
                bank_puffs.push([
                    (x + pf.dx * rr) as f32,
                    (y + pf.dy * rr * 0.55) as f32,
                    (rr * pf.s) as f32,
                    i as f32,
                ]);
            }
        }
        SkyFrame {
            washes,
            sun: [
                (sx as f64 * w) as f32,
                (sy as f64 * h) as f32,
                big as f32,
                disc as f32,
            ],
            rays: [(ms / 40000.0) as f32, if day { 1.0 } else { 0.0 }],
            ray_len,
            cloud_puffs,
            cloud_alpha,
            banks,
            bank_puffs,
            deck_h: deck_h as f32,
        }
    }
}

/// Sky frame spacing while it animates (sky.js: about 15 fps).
pub const SKY_FRAME_MS: u64 = 66;
/// A redraw at least this often even without motion (the palette follows the clock).
pub const SKY_STILL_MS: u64 = 60_000;
/// After the UI reports movement, the sky waits this long (a sky frame re-blurs every glass
/// surface, which would compete with the moving control for frames).
pub const SKY_HOLD_MS: u64 = 250;

/// When the next sky frame is due, in ms after `last` (the last sky frame), or `None` when
/// the sky should not draw at all now (hidden window).
pub fn sky_next_ms(animate: bool, visible: bool, moving_ago_ms: Option<u64>) -> Option<u64> {
    if !visible {
        return None;
    }
    if !animate {
        return Some(SKY_STILL_MS);
    }
    match moving_ago_ms {
        Some(ago) if ago < SKY_HOLD_MS => Some(SKY_FRAME_MS.max(SKY_HOLD_MS - ago)),
        _ => Some(SKY_FRAME_MS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reports_and_rejects_bad_ones() {
        let r = parse_report("10,20,300,38,19,c,1; 0,0,372,500.5,26,f,0.5,0,40,800,600").unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].kind, GlassKind::Chrome);
        assert_eq!(r[0].clip, None);
        assert_eq!(r[1].kind, GlassKind::Float);
        assert_eq!(r[1].clip, Some([0.0, 40.0, 800.0, 600.0]));
        assert_eq!(r[1].alpha, 0.5);
        assert_eq!(parse_report("1,2,3,4,5,c,7").unwrap()[0].alpha, 1.0);
        assert!(parse_report("").unwrap().is_empty());
        assert!(parse_report("1,2,3,4,5,x,1").is_err());
        assert!(parse_report("1,2,3,4,5").is_err());
        assert!(parse_report("1,2,-3,4,5,c,1").is_err());
        assert!(parse_report("1,2,NaN,4,5,c,1").is_err());
        assert!(parse_report("1,2,99999,4,5,c,1").is_err());
        assert!(parse_report(&"1,1,1,1,1,c,1;".repeat(1000)).is_err());
        let many = "1,1,10,10,1,p,1;".repeat(40);
        assert_eq!(parse_report(&many).unwrap().len(), MAX_REPORTED);
    }

    #[test]
    fn device_rects_are_scaled_moved_and_clipped() {
        let area = PxRect::new(0, 88, 1000, 600);
        let r = parse_report(
            "10,20,100,50,40,p,1;0,-200,100,50,10,p,1;0,0,100,100,8,p,1,0,50,1000,500;5,5,50,50,4,p,0",
        )
            .unwrap();
        let d = to_device(&r, 2.0, area);
        // The second one is entirely above the webview, the last one invisible: dropped.
        assert_eq!(d.len(), 2);
        assert_eq!(
            (d[0].x, d[0].y, d[0].w, d[0].h),
            (20.0, 128.0, 200.0, 100.0)
        );
        // Radius clamped to half the short side.
        assert_eq!(d[0].radius, 50.0);
        assert_eq!(d[0].clip, [0.0, 88.0, 1000.0, 688.0]);
        // Clip intersected with the webview.
        assert_eq!(d[1].clip, [0.0, 188.0, 1000.0, 688.0]);
    }

    #[test]
    fn palette_matches_theme_js_keys() {
        let p = palette(10.0);
        assert!((p.top[0] - 0x66 as f32 / 255.0).abs() < 1e-5);
        assert_eq!(p.stars, 0.0);
        assert!(!p.dark());
        let n = palette(23.0);
        assert!(n.stars > 0.9 && n.dark());
        assert!(palette(25.0).hour < 24.0);
        assert!(palette(f32::NAN).hour == 12.0);
        let (_, _, day) = palette(12.0).sun();
        assert!(day);
        let (_, _, day) = palette(2.0).sun();
        assert!(!day);
        // Deck strengths: night .26, day .42, dusk .28.
        assert_eq!(palette(23.0).deck().2, 0.26);
        assert_eq!(palette(11.0).deck().2, 0.42);
        assert_eq!(palette(18.4).deck().2, 0.28);
    }

    #[test]
    fn sky_animates_only_when_visible_and_waits_for_moving_controls() {
        assert_eq!(sky_next_ms(true, false, None), None);
        assert_eq!(sky_next_ms(false, true, None), Some(SKY_STILL_MS));
        assert_eq!(sky_next_ms(true, true, None), Some(SKY_FRAME_MS));
        assert_eq!(sky_next_ms(true, true, Some(10)), Some(240));
        assert_eq!(sky_next_ms(true, true, Some(1000)), Some(SKY_FRAME_MS));
    }

    #[test]
    fn sky_scene_matches_sky_js() {
        let s = SkyScene::new();
        let n: usize = s.clouds.iter().map(|c| c.puffs.len()).sum();
        assert!((SKY_CLOUDS * 3..=SKY_CLOUD_PUFFS).contains(&n));
        let n: usize = s.deck.iter().map(|b| b.puffs.len()).sum();
        assert!((SKY_BANKS * 4..=SKY_BANK_PUFFS).contains(&n));
        // First cloud of sky.js with seed 7 (checked in Chromium): y 0.5280, x 0.9511.
        assert!(
            (s.clouds[0].y - 0.528_04).abs() < 1e-4 && (s.clouds[0].x - 0.951_05).abs() < 1e-4,
            "{}",
            s.clouds[0].y
        );
        let f = s.frame(&palette(11.5), 3600.0, 1440.0, 900.0);
        assert_eq!(f.cloud_puffs.len(), f.cloud_alpha.len());
        assert_eq!(f.deck_h, 180.0);
        assert!(f.rays[1] == 1.0 && f.sun[3] > 11.0 && f.sun[3] < 12.5);
        // Deck banks stay in the band over the title bar and toolbar.
        assert!(f.bank_puffs.iter().all(|p| p[1] > 0.0 && p[1] < 180.0));
        let night = s.frame(&palette(22.5), 0.0, 1440.0, 900.0);
        assert_eq!(night.rays[1], 0.0);
    }

    #[test]
    fn transparency_maps_to_steps_from_b() {
        assert_eq!(glass_level(TRANSPARENCY_DEFAULT), 0.0);
        assert_eq!(glass_level(0), LEVEL_MIN);
        assert_eq!(glass_level(100), LEVEL_MAX);
        assert_eq!(glass_level(255), LEVEL_MAX, "clamped");
        // Monotonic: more transparency, higher level, never a jump.
        let levels: Vec<f32> = (0..=100).map(glass_level).collect();
        assert!(
            levels
                .windows(2)
                .all(|w| w[1] > w[0] && w[1] - w[0] < 0.031)
        );
        // The NOTES variants: A sits a third of the way up, C at the top.
        assert!((glass_level(17) + 1.0).abs() < 0.03);
    }

    #[test]
    fn tints_interpolate_the_notes_variants() {
        let close = |a: f32, b: f32| (a - b).abs() < 0.006;
        // B (default) exactly.
        let b = Tints::at(TRANSPARENCY_DEFAULT, false);
        assert!(close(b.tint, 0.14) && close(b.strong, 0.30) && close(b.scrim, 0.08));
        let bd = Tints::at(TRANSPARENCY_DEFAULT, true);
        assert!(close(bd.tint, 0.22) && close(bd.strong, 0.34) && close(bd.scrim, 0.20));
        // C at 100.
        let c = Tints::at(100, false);
        assert!(close(c.tint, 0.08) && close(c.strong, 0.20) && close(c.scrim, 0.06));
        let cd = Tints::at(100, true);
        assert!(close(cd.tint, 0.16) && (cd.strong - 0.26).abs() < 0.011);
        // 0 is more solid than A (.20 / .40 light, .28 / .44 dark) but not the old fog (.58).
        let s = Tints::at(0, false);
        assert!(s.tint > 0.20 && s.strong > 0.40 && s.strong < 0.58);
        let sd = Tints::at(0, true);
        assert!(sd.tint > 0.28 && sd.strong > 0.44 && sd.strong < 0.56);
        // Every value moves the right way along the slider.
        for t in 1..=100u8 {
            for dark in [false, true] {
                let (p, q) = (Tints::at(t - 1, dark), Tints::at(t, dark));
                assert!(q.tint < p.tint && q.strong < p.strong && q.scrim < p.scrim);
                assert!(q.float <= p.float);
            }
        }
    }

    #[test]
    fn surfaces_over_pages_keep_the_floor() {
        for t in 0..=100u8 {
            let l = Tints::at(t, false);
            let d = Tints::at(t, true);
            assert!(l.float >= FLOAT_FLOOR_LIGHT && l.float >= l.strong);
            assert!(d.float >= FLOAT_FLOOR_DARK && d.float >= d.strong);
        }
        // At the default and above the floor is what shows; well below it the slider wins.
        assert_eq!(Tints::at(100, false).float, FLOAT_FLOOR_LIGHT);
        assert_eq!(
            Tints::at(TRANSPARENCY_DEFAULT, true).float,
            FLOAT_FLOOR_DARK
        );
        assert!(Tints::at(0, false).float > FLOAT_FLOOR_LIGHT);
    }

    #[test]
    fn look_follows_the_slider_within_its_ranges() {
        let b = GlassLook::at(TRANSPARENCY_DEFAULT, false, false);
        assert_eq!((b.blur_k, b.saturation, b.brightness), (0.5, 1.5, 1.08));
        assert_eq!(
            GlassLook::at(TRANSPARENCY_DEFAULT, true, false).brightness,
            0.92
        );
        let c = GlassLook::at(100, false, false);
        assert!((c.blur_k - 0.335).abs() < 1e-4 && (c.saturation - 1.6).abs() < 1e-4);
        assert!((c.brightness - 1.125).abs() < 1e-4);
        let s = GlassLook::at(0, true, false);
        assert!(s.blur_k > 0.7 && s.saturation < 1.4 && s.brightness > 0.95);
        for t in 0..=100u8 {
            for dark in [false, true] {
                let l = GlassLook::at(t, dark, false);
                assert!((BLUR_K_RANGE.0..=BLUR_K_RANGE.1).contains(&l.blur_k));
                assert!((SATURATION_RANGE.0..=SATURATION_RANGE.1).contains(&l.saturation));
                assert!((BRIGHTNESS_RANGE.0..=BRIGHTNESS_RANGE.1).contains(&l.brightness));
            }
        }
        // Panel 24 px at B → 12 px (NOTES: Chrome 10, Panel 12, Float 14).
        assert_eq!(b.blur_css(GlassKind::Chrome), 10.0);
        assert_eq!(b.blur_css(GlassKind::Panel), 12.0);
        assert_eq!(b.blur_css(GlassKind::Float), 14.0);
        let solid = GlassLook::at(TRANSPARENCY_DEFAULT, false, true);
        assert!(solid.solid && solid.blur_css(GlassKind::Float) == 0.0);
    }

    #[test]
    fn appearance_setting_parses_and_resolves() {
        assert_eq!(parse_transparency("0"), Some(0));
        assert_eq!(parse_transparency("100"), Some(100));
        assert_eq!(parse_transparency(" 42 "), Some(42));
        for bad in ["", "101", "-1", "4.5", "1e2", "abc", "0100", "999"] {
            assert_eq!(parse_transparency(bad), None, "{bad:?}");
        }
        assert_eq!(parse_reduce("on"), Some(Some(true)));
        assert_eq!(parse_reduce("off"), Some(Some(false)));
        assert_eq!(parse_reduce("system"), Some(None));
        assert_eq!(parse_reduce("maybe"), None);
        let a = Appearance::default();
        assert_eq!(a.transparency, TRANSPARENCY_DEFAULT);
        assert!(
            a.reduced(true) && !a.reduced(false),
            "follows the OS by default"
        );
        let on = Appearance {
            reduce: Some(true),
            ..a
        };
        assert!(on.reduced(false) && on.reduce_str() == "on");
        let off = Appearance {
            reduce: Some(false),
            ..a
        };
        assert!(!off.reduced(true) && off.reduce_str() == "off");
        assert_eq!(a.reduce_str(), "system");
    }

    /// The shell's blur passes, simulated on a vertical edge (only the taps' x offsets
    /// matter): GL_LINEAR sampling with clamp-to-edge, the same taps and weights as
    /// DOWN_FRAGMENT / UP_FRAGMENT in crates/shell/src/glass.rs.
    fn kawase_sigma(depth: usize, off: f64) -> f64 {
        fn sample(src: &[f64], x: f64) -> f64 {
            let t = x - 0.5;
            let i0 = t.floor();
            let f = t - i0;
            let at = |i: f64| src[(i.max(0.0) as usize).min(src.len() - 1)];
            at(i0) * (1.0 - f) + at(i0 + 1.0) * f
        }
        fn resample(src: &[f64], n: usize, taps: &[(f64, f64)]) -> Vec<f64> {
            let total: f64 = taps.iter().map(|t| t.1).sum();
            (0..n)
                .map(|i| {
                    let x = (i as f64 + 0.5) / n as f64 * src.len() as f64;
                    taps.iter()
                        .map(|(o, w)| sample(src, x + o) * w)
                        .sum::<f64>()
                        / total
                })
                .collect()
        }
        let w = 2048;
        let full: Vec<f64> = (0..w).map(|i| if i >= w / 2 { 1.0 } else { 0.0 }).collect();
        let mut downs = vec![resample(&full, w / 2, &[(0.0, 1.0)])];
        for _ in 0..depth {
            let src = downs.last().unwrap();
            downs.push(resample(
                src,
                src.len() / 2,
                &[(0.0, 4.0), (-off, 2.0), (off, 2.0)],
            ));
        }
        let mut cur = downs[depth].clone();
        for i in (0..depth).rev() {
            cur = resample(
                &cur,
                downs[i].len(),
                &[
                    (-2.0 * off, 1.0),
                    (-off, 4.0),
                    (0.0, 2.0),
                    (off, 4.0),
                    (2.0 * off, 1.0),
                ],
            );
        }
        let out = resample(&cur, w, &[(0.0, 1.0)]);
        // The edge's gradient is the blur kernel: its standard deviation is sigma.
        let g: Vec<f64> = out.windows(2).map(|p| p[1] - p[0]).collect();
        let sum: f64 = g.iter().sum();
        let mean: f64 = g.iter().enumerate().map(|(i, v)| i as f64 * v).sum::<f64>() / sum;
        (g.iter()
            .enumerate()
            .map(|(i, v)| (i as f64 - mean).powi(2) * v)
            .sum::<f64>()
            / sum)
            .sqrt()
    }

    #[test]
    fn kawase_matches_css_blur() {
        // The table the mapping is built on.
        for (d, s) in KAWASE_SIGMA.iter().enumerate() {
            let got = kawase_sigma(d + 1, 1.0);
            assert!((got - *s as f64).abs() < 0.1, "depth {} sigma {got}", d + 1);
        }
        // Every radius the slider can ask for (Chrome 20 × .3 = 6 px … Float 28 × .8 = 22.4,
        // at 1× and 2×) comes out within 12 % of CSS blur(), and bigger asks blur more.
        let mut last = 0.0;
        for b in (5..=60).map(|b| b as f32) {
            let (d, off) = kawase_for_blur(b);
            let got = kawase_sigma(d, off as f64);
            assert!(
                ((got - b as f64) / b as f64).abs() < 0.12,
                "blur {b}: depth {d} off {off} sigma {got}"
            );
            assert!(got > last, "blur {b} is not stronger than {}", b - 1.0);
            last = got;
        }
        // 12 px and 24 px no longer look the same (the old depth-only mapping gave both depth 3).
        assert!(kawase_for_blur(12.0) != kawase_for_blur(24.0));
        assert_eq!(kawase_for_blur(f32::NAN).0, 1);
        assert_eq!(kawase_for_blur(1e6), (KAWASE_MAX_DEPTH, KAWASE_OFF.1));
    }

    #[test]
    fn modes() {
        assert!(GlassMode::Blur.shell_sky() && GlassMode::Flat.shell_sky());
        assert!(!GlassMode::Off.shell_sky());
        assert_eq!(GlassMode::Flat.as_str(), "flat");
        assert!(GlassKind::Float.blur_css() > GlassKind::Chrome.blur_css());
    }
}
