// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Liquid glass, drawn by the shell (Servo 0.1.4 has no `backdrop-filter`). Ported from the
//! headless proof of concept (flat revision); see `mythbrowse_engine::glass` for the
//! protocol and docs/DESIGN.md for the look.
//!
//! When glass is on (`GlassMode::Blur` / `Flat`), every Servo webview is cleared to
//! transparent (`shell_background_color_rgba` alpha 0) and a frame is composed as:
//!
//! 1. **Sky** ([`SkyPainter`]): the MythDesk sky with the calmer title-bar cloud deck, the
//!    same maths as `ui/design/sky.js`, rendered at ½ resolution into a cached texture
//!    (re-rendered only when the sky moves, about 15 fps with full motion) and drawn
//!    upscaled with a ±1/255 dither.
//! 2. **Page** ([`LayerPainter`]): the active tab's texture. A web page in the card is put
//!    on an opaque white underlay (what a browser canvas is) and cut to the card's rounded
//!    rectangle (anti-aliased); an internal page (AI home, Tasks, history) is blended over
//!    the sky, premultiplied.
//! 3. **Glass** ([`GlassRenderer`]): the composed window (sky + page) is blurred
//!    (dual-Kawase, cached until the backdrop generation changes) and drawn into every
//!    reported surface, with the saturation and brightness of the Transparency setting
//!    (`GlassLook`: 1.5 / ×1.08 light, ×0.92 dark at the default) and a ≤4 px edge bend on
//!    surfaces ≥44 px. The radius follows the setting too (`GlassLook::blur_css`), turned into
//!    a pyramid depth and a continuous pass offset by `kawase_for_blur` so every radius really
//!    changes the blur. Reduce transparency draws no blur at all. The tint, the hairline rim
//!    and the floating shadow stay CSS (the UI draws them over).
//! 4. **UI**: the chrome's texture over everything, premultiplied (one pass; the chrome is
//!    transparent except where it draws something). The card glow follows (`compose`).
//!
//! The blur is skipped on frames where only a surface moved (the cached pyramid is reused).
//!
//! Costs: framebuffers and textures are created once per window size; nothing is allocated
//! per frame. GLSL is `#version 300 es` (ANGLE on Windows, Mesa) or `#version 150`/`130` on
//! desktop GL (`compose::build_program`). GL state is left as `compose.rs` leaves it.

use std::num::NonZeroU32;

use glow::HasContext;
use mythbrowse_engine::glass::{
    GlassKind, GlassLook, GlassRectPx, KAWASE_MAX_DEPTH, Palette, SKY_BANK_PUFFS, SKY_CLOUD_PUFFS,
    SkyScene, kawase_for_blur,
};

use crate::compose::build_program;

/// Most surfaces composed per frame (the chrome's and the page's together).
pub const MAX_RECTS: usize = 32;
/// Pyramid levels below ½ resolution (¼ … 1/32).
const MAX_DEPTH: usize = KAWASE_MAX_DEPTH;

/// How a surface looks, apart from its shape. Values mirror the prototype's `--lg-*` tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// sRGB tint over the backdrop, straight alpha.
    pub tint: [f32; 4],
    /// Opacity of the white hairline rim (`--lg-rim`), uniform all round.
    pub rim: f32,
    /// Opacity of the ambient shadow (`--lg-float`); 0 for chrome (tabs, omnibox).
    pub shadow: f32,
    /// Backdrop saturation (1 = unchanged; the Transparency setting's `GlassLook`).
    pub saturation: f32,
    /// Backdrop brightness (1 = unchanged; `GlassLook`: above 1 light, below 1 dark).
    pub brightness: f32,
}

#[allow(dead_code)] // LIGHT / DARK / floating: the proof's own materials, kept for tests
impl Material {
    /// Light tone (`--lg-tint: rgba(255,255,255,.26)`, `--lg-rim: rgba(255,255,255,.4)`).
    pub const LIGHT: Material = Material {
        tint: [1.0, 1.0, 1.0, 0.26],
        rim: 0.40,
        shadow: 0.0,
        saturation: 1.2,
        brightness: 1.0,
    };
    /// Dark tone (`--lg-tint: rgba(22,28,56,.34)`, `--lg-rim: rgba(255,255,255,.13)`).
    pub const DARK: Material = Material {
        tint: [0.086, 0.110, 0.220, 0.34],
        rim: 0.13,
        shadow: 0.0,
        saturation: 1.2,
        brightness: 1.0,
    };

    /// What the shell draws when the UI draws tint, rim and shadow itself (the app): only
    /// the blurred, slightly saturated backdrop.
    pub const BACKDROP: Material = Material {
        tint: [0.0, 0.0, 0.0, 0.0],
        rim: 0.0,
        shadow: 0.0,
        saturation: 1.2,
        brightness: 1.0,
    };

    /// The same material with a different tint opacity (`--lg-tint-strong` etc.).
    pub const fn with_tint_alpha(mut self, a: f32) -> Material {
        self.tint[3] = a;
        self
    }

    /// The same material as a floating panel (`.lg.float`): `--lg-float` is
    /// `0 4px 18px` at .08 (light) / .20 (dark).
    pub const fn floating(mut self, shadow: f32) -> Material {
        self.shadow = shadow;
        self
    }
}

/// Surfaces whose short side is below this (CSS px) get no refraction at all.
pub const REFRACT_MIN_SIDE: f32 = 44.0;

/// One glass surface, in device px with a top-left origin (like `frame::PxRect`, but
/// fractional so springs can move it smoothly).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Corner radius (clamped to half the short side).
    pub radius: f32,
    /// Backdrop blur, px (like CSS `blur()`).
    pub blur: f32,
    /// Largest edge displacement, px (0 = no refraction). The prototype: `min(4, 0.7·band)`
    /// with `band = min(6, 0.2·short side)`, and none below [`REFRACT_MIN_SIDE`].
    pub refract: f32,
    pub material: Material,
    /// Visible part (x0, y0, x1, y1, device px, top-left origin): nothing is drawn outside.
    pub clip: Option<[f32; 4]>,
    /// The surface's own opacity (0–1): a fading panel fades its backdrop with it.
    pub opacity: f32,
}

impl GlassRect {
    /// A surface with the prototype's refraction and blur for its size, in device px at
    /// `scale`.
    pub fn new(x: f32, y: f32, w: f32, h: f32, radius: f32, scale: f32, m: Material) -> Self {
        let mut r = GlassRect {
            x,
            y,
            w,
            h,
            radius,
            blur: 24.0 * scale,
            refract: 0.0,
            material: m,
            clip: None,
            opacity: 1.0,
        };
        if w.min(h) >= REFRACT_MIN_SIDE * scale {
            r.refract = (4.0 * scale).min(0.7 * r.band(scale));
        }
        r
    }

    /// A surface the UI reported (already in window device px), drawn as the backdrop only,
    /// with the blur radius, saturation and brightness of the Transparency setting.
    pub fn from_report(g: &GlassRectPx, scale: f32, look: &GlassLook) -> Self {
        let mut m = Material::BACKDROP;
        m.saturation = look.saturation;
        m.brightness = look.brightness;
        let mut r = GlassRect::new(g.x, g.y, g.w, g.h, g.radius, scale, m);
        r.blur = look.blur_css(g.kind) * scale;
        r.clip = Some(g.clip);
        r.opacity = g.alpha.clamp(0.0, 1.0);
        if g.kind == GlassKind::Chrome {
            r.refract = 0.0;
        }
        r
    }

    fn band(&self, scale: f32) -> f32 {
        (6.0 * scale).min(0.2 * self.w.min(self.h)).max(1.0)
    }
}

/// Dual-Kawase depth for a blur radius (px at full resolution). Each level doubles the
/// reach; the pass offset (see [`kawase_for_blur`]) fills in between, calibrated against
/// CSS `blur()` by simulating these exact passes (engine test `kawase_matches_css_blur`).
/// (The renderer calls `kawase_for_blur` itself; this stays for the tests' readability.)
#[cfg(test)]
pub fn depth_for_blur(blur: f32) -> usize {
    kawase_for_blur(blur).0
}

/// Dual-Kawase downsample: centre ×4 plus four diagonal bilinear taps.
const DOWN_FRAGMENT: &str = "
uniform sampler2D u_src;
uniform vec2 u_dst;       // target size, px
uniform vec2 u_off;       // source texel × offset
out vec4 o_color;
void main() {
    vec2 uv = gl_FragCoord.xy / u_dst;
    vec4 s = texture(u_src, uv) * 4.0;
    s += texture(u_src, uv - u_off);
    s += texture(u_src, uv + u_off);
    s += texture(u_src, uv + vec2(u_off.x, -u_off.y));
    s += texture(u_src, uv - vec2(u_off.x, -u_off.y));
    o_color = s / 8.0;
}
";

/// Dual-Kawase upsample: eight taps on a diamond, edges ×1, diagonals ×2.
const UP_FRAGMENT: &str = "
uniform sampler2D u_src;
uniform vec2 u_dst;
uniform vec2 u_off;
out vec4 o_color;
void main() {
    vec2 uv = gl_FragCoord.xy / u_dst;
    vec2 o = u_off;
    vec4 s = texture(u_src, uv + vec2(-2.0 * o.x, 0.0));
    s += texture(u_src, uv + vec2(-o.x, o.y)) * 2.0;
    s += texture(u_src, uv + vec2(0.0, 2.0 * o.y));
    s += texture(u_src, uv + vec2(o.x, o.y)) * 2.0;
    s += texture(u_src, uv + vec2(2.0 * o.x, 0.0));
    s += texture(u_src, uv + vec2(o.x, -o.y)) * 2.0;
    s += texture(u_src, uv + vec2(0.0, -2.0 * o.y));
    s += texture(u_src, uv + vec2(-o.x, -o.y)) * 2.0;
    o_color = s / 12.0;
}
";

/// One glass surface (see the module docs).
const GLASS_FRAGMENT: &str = "
uniform sampler2D u_heavy;  // dual-Kawase result, 1/2 res
uniform sampler2D u_light;  // 1/2-res box downsample
uniform vec2 u_view;        // target size, px
uniform vec4 u_rect;        // x0, y0, x1, y1 (GL px, y up)
uniform vec4 u_shape;       // radius, refraction px, band px, heavy-blur mix
uniform vec4 u_tint;        // straight alpha
uniform vec4 u_mat;         // rim opacity, saturation, shadow opacity, device px per CSS px
uniform float u_alpha;      // the surface's own opacity
uniform float u_bright;     // backdrop brightness (Transparency setting)
out vec4 o_color;

float rbox(vec2 p) {
    vec2 c = (u_rect.xy + u_rect.zw) * 0.5;
    vec2 h = (u_rect.zw - u_rect.xy) * 0.5;
    float r = min(u_shape.x, min(h.x, h.y));
    vec2 q = abs(p - c) - h + vec2(r);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

vec3 backdrop(vec2 p, float k) {
    vec2 uv = clamp(p / u_view, vec2(0.0), vec2(1.0));
    return mix(texture(u_light, uv).rgb, texture(u_heavy, uv).rgb, k);
}

void main() {
    vec2 p = gl_FragCoord.xy;
    float px = u_mat.w;
    float d = rbox(p);
    float cov = clamp(0.5 - d, 0.0, 1.0);

    // Ambient shadow (--lg-float: 0 4px 18px), floating panels only.
    float sh = 0.0;
    if (u_mat.z > 0.0) {
        float ds = rbox(p + vec2(0.0, 4.0 * px));
        sh = u_mat.z * exp(-pow(max(ds, 0.0) / (9.0 * px), 2.0));
    }
    if (cov <= 0.0) {
        if (sh < 0.003) { discard; }
        o_color = vec4(0.02, 0.04, 0.12, sh * u_alpha);
        return;
    }

    // A thin lens rim: only the last few px bend, and only on larger surfaces.
    vec2 off = vec2(0.0);
    if (u_shape.y > 0.0) {
        float e = 0.75;
        vec2 g = vec2(rbox(p + vec2(e, 0.0)) - rbox(p - vec2(e, 0.0)),
                      rbox(p + vec2(0.0, e)) - rbox(p - vec2(0.0, e)));
        vec2 n = g / max(length(g), 1e-4);
        float x = clamp(1.0 - max(-d, 0.0) / u_shape.z, 0.0, 1.0);
        off = n * x * x * (3.0 - 2.0 * x) * u_shape.y;
    }
    vec3 col = backdrop(p - off, u_shape.w);

    // Saturation and brightness (the Transparency setting), then the tint.
    float l = dot(col, vec3(0.2126, 0.7152, 0.0722));
    col = clamp(mix(vec3(l), col, u_mat.y) * u_bright, 0.0, 1.0);
    col = mix(col, u_tint.rgb, u_tint.a);

    // One uniform hairline just inside the edge (1 CSS px wide, like inset 0 0 0 1px).
    float line = clamp(0.5 - d, 0.0, 1.0) - clamp(0.5 - (d + px), 0.0, 1.0);
    col = mix(col, vec3(1.0), u_mat.x * line);

    float alpha = cov + sh * (1.0 - cov);
    vec3 outc = (col * cov + vec3(0.02, 0.04, 0.12) * sh * (1.0 - cov)) / max(alpha, 1e-4);
    o_color = vec4(outc, alpha * u_alpha);
}
";

fn fb(name: u32) -> Option<glow::NativeFramebuffer> {
    NonZeroU32::new(name).map(glow::NativeFramebuffer)
}

/// An integer pixel rectangle in GL coordinates (origin bottom-left), `x1`/`y1` exclusive.
/// Used to limit work to where it can change pixels (see [`GlassRenderer::render`] and the
/// sky hole in [`Compositor::paint`]); every limit is computed so the visible result is the
/// same as drawing everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Region {
    pub fn full(w: i32, h: i32) -> Self {
        Region {
            x0: 0,
            y0: 0,
            x1: w,
            y1: h,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn union(&self, o: &Region) -> Region {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Region {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
    pub fn grow(&self, m: i32) -> Region {
        Region {
            x0: self.x0 - m,
            y0: self.y0 - m,
            x1: self.x1 + m,
            y1: self.y1 + m,
        }
    }
    pub fn clamp(&self, w: i32, h: i32) -> Region {
        Region {
            x0: self.x0.clamp(0, w),
            y0: self.y0.clamp(0, h),
            x1: self.x1.clamp(0, w),
            y1: self.y1.clamp(0, h),
        }
    }
    pub fn intersects(&self, o: &Region) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.x0 < o.x1
            && o.x0 < self.x1
            && self.y0 < o.y1
            && o.y0 < self.y1
    }
    pub fn contains(&self, o: &Region) -> bool {
        o.is_empty() || (self.x0 <= o.x0 && self.y0 <= o.y0 && self.x1 >= o.x1 && self.y1 >= o.y1)
    }
    /// The same area one pyramid level down (½ size), rounded outwards.
    pub fn half(&self) -> Region {
        Region {
            x0: self.x0.div_euclid(2),
            y0: self.y0.div_euclid(2),
            x1: (self.x1 + 1).div_euclid(2),
            y1: (self.y1 + 1).div_euclid(2),
        }
    }
    /// The same area one pyramid level up (2× size).
    pub fn double(&self) -> Region {
        Region {
            x0: self.x0 * 2,
            y0: self.y0 * 2,
            x1: self.x1 * 2,
            y1: self.y1 * 2,
        }
    }
    /// `outer` minus this rectangle, as at most four bands (bottom, top, left, right).
    pub fn bands_around(&self, outer: &Region) -> Vec<Region> {
        let hole = Region {
            x0: self.x0.max(outer.x0),
            y0: self.y0.max(outer.y0),
            x1: self.x1.min(outer.x1),
            y1: self.y1.min(outer.y1),
        };
        if hole.is_empty() {
            return vec![*outer];
        }
        [
            Region {
                y1: hole.y0,
                ..*outer
            },
            Region {
                y0: hole.y1,
                ..*outer
            },
            Region {
                x1: hole.x0,
                y0: hole.y0,
                y1: hole.y1,
                ..*outer
            },
            Region {
                x0: hole.x1,
                y0: hole.y0,
                y1: hole.y1,
                ..*outer
            },
        ]
        .into_iter()
        .filter(|r| !r.is_empty())
        .collect()
    }
    /// # Safety
    /// Needs a current GL context.
    unsafe fn scissor(&self, gl: &glow::Context) {
        // SAFETY: see the function contract.
        unsafe {
            gl.scissor(
                self.x0,
                self.y0,
                (self.x1 - self.x0).max(0),
                (self.y1 - self.y0).max(0),
            )
        };
    }
}

/// Extra full-resolution px blurred around the surfaces, so a control that moves a little
/// (springs, a growing pill) keeps reusing the cached blur instead of blurring again.
const BLUR_PAD_PX: f32 = 48.0;

/// Where each blur level must be right so every reported surface reads the same backdrop it
/// would read from a whole-window blur (`needed` is in full-resolution GL px). Walks the
/// passes backwards: the composite samples ½-res `downs[0]` / `ups[0]` (bilinear, refraction
/// included in `needed`); an up pass reads its source (½ size) up to `2·off` source texels
/// away; a down pass reads its source (2× size) up to `off` source texels away; +2 texels
/// each for bilinear taps and rounding. Returns (`downs` regions, `ups` regions), one per
/// level, clamped to the level sizes.
fn blur_regions(
    needed: Region,
    sizes: &[(i32, i32)],
    depth: usize,
    off: f32,
) -> (Vec<Region>, Vec<Region>) {
    let n = sizes.len();
    let up_m = (2.0 * off).ceil() as i32 + 2;
    let down_m = off.ceil() as i32 + 2;
    let clamp = |r: Region, i: usize| r.clamp(sizes[i].0, sizes[i].1);
    let mut ups = vec![
        Region {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0
        };
        n
    ];
    let mut downs = vec![
        Region {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0
        };
        n
    ];
    let n0 = clamp(needed.half().grow(2), 0);
    ups[0] = n0;
    // ups[i] is made from level i + 1 (ups[i + 1], or downs[depth] for the last one).
    let mut need = n0;
    for i in 0..depth {
        need = clamp(need.half().grow(up_m), i + 1);
        if i + 1 == depth {
            downs[depth] = need;
        } else {
            ups[i + 1] = need;
        }
    }
    // downs[k] is made from downs[k - 1].
    for k in (1..=depth).rev() {
        let src = clamp(downs[k].double().grow(down_m), k - 1);
        downs[k - 1] = downs[k - 1].union(&src);
    }
    downs[0] = clamp(downs[0].union(&n0), 0);
    (downs, ups)
}

/// One render target of the blur pyramid.
struct Target {
    fbo: glow::NativeFramebuffer,
    tex: glow::NativeTexture,
    w: i32,
    h: i32,
}

impl Target {
    fn new(gl: &glow::Context, w: i32, h: i32) -> Result<Self, String> {
        // SAFETY: creates and configures a texture + framebuffer on the current context;
        // both are deleted if the framebuffer is incomplete.
        unsafe {
            let tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                w,
                h,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            for (k, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            let fbo = match gl.create_framebuffer() {
                Ok(f) => f,
                Err(e) => {
                    gl.delete_texture(tex);
                    return Err(e);
                }
            };
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(tex);
                return Err(format!("blur target incomplete: {status:#x}"));
            }
            Ok(Target { fbo, tex, w, h })
        }
    }

    fn delete(self, gl: &glow::Context) {
        // SAFETY: deletes objects this target owns.
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.tex);
        }
    }
}

struct Pass {
    program: glow::NativeProgram,
    u_src: Option<glow::NativeUniformLocation>,
    u_dst: Option<glow::NativeUniformLocation>,
    u_off: Option<glow::NativeUniformLocation>,
}

/// Uniforms of `GLASS_FRAGMENT`, in declaration order.
const GLASS_UNIFORMS: [&str; 9] = [
    "u_heavy", "u_light", "u_view", "u_rect", "u_shape", "u_tint", "u_mat", "u_alpha", "u_bright",
];

/// What the last blur was computed from (to skip it when nothing changed).
#[derive(Clone, Copy, PartialEq, Eq)]
struct BlurKey {
    generation: u64,
    /// The web page's frame counter, only when the blur reads page pixels (`None`: the
    /// surfaces sit over the sky only, so a new page frame, e.g. scrolling, keeps the blur).
    page_generation: Option<u64>,
    w: i32,
    h: i32,
    depth: usize,
    /// Pass offset (`f32::to_bits`): a new radius at the same depth blurs again.
    off: u32,
}

/// The cached blur: what it was made from and the full-resolution area it is right in.
#[derive(Clone, Copy)]
struct BlurCache {
    key: BlurKey,
    valid: Region,
}

thread_local! {
    /// Perf A/B only: blur and draw the sky over the whole window, as before the region
    /// limits. Set only by the headless compositor benchmark (docs/PERF.md); the shell never
    /// sets it.
    pub(crate) static FULL_BLUR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The full-resolution area (GL px) whose blurred backdrop the surfaces read: every
/// surface (cut to its clip), grown by its refraction (the lens samples that far outside)
/// and 2 px for bilinear taps.
fn needed_region(rects: &[GlassRect], w: i32, h: i32) -> Region {
    let mut out = Region {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };
    for r in rects
        .iter()
        .filter(|r| r.w > 0.5 && r.h > 0.5 && r.opacity > 0.004)
    {
        let (mut x0, mut x1) = (r.x, r.x + r.w);
        let (mut ty0, mut ty1) = (r.y, r.y + r.h); // top-left origin
        if let Some([cx0, cy0, cx1, cy1]) = r.clip {
            x0 = x0.max(cx0);
            x1 = x1.min(cx1);
            ty0 = ty0.max(cy0);
            ty1 = ty1.min(cy1);
        }
        if x1 <= x0 || ty1 <= ty0 {
            continue;
        }
        let m = r.refract.max(0.0) + 2.0;
        let g = Region {
            x0: (x0 - m).floor() as i32,
            x1: (x1 + m).ceil() as i32,
            y0: (h as f32 - ty1 - m).floor() as i32,
            y1: (h as f32 - ty0 + m).ceil() as i32,
        };
        out = out.union(&g);
    }
    out.clamp(w, h)
}

/// See the module docs.
pub struct GlassRenderer {
    vao: glow::NativeVertexArray,
    down: Pass,
    up: Pass,
    glass: glow::NativeProgram,
    u: [Option<glow::NativeUniformLocation>; 9],
    /// `down[0]` is ½ resolution, `down[i]` is 1/2^(i+1).
    downs: Vec<Target>,
    /// `ups[i]` matches `downs[i]` in size (`ups[MAX_DEPTH]` is never used).
    ups: Vec<Target>,
    size: (i32, i32),
    cache: Option<BlurCache>,
}

/// Frame inputs that are not surfaces.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Framebuffer holding the composed background (read).
    pub source_fb: u32,
    /// Framebuffer the glass is drawn into (often the same one).
    pub target_fb: u32,
    /// Size of both, device px.
    pub width: i32,
    pub height: i32,
    /// Device px per CSS px.
    pub scale: f32,
    /// Bumped by the caller whenever the background changed (the sky, a layout change, a
    /// resize). The blur is reused while it stays the same.
    pub generation: u64,
    /// Bumped on every new frame of the web page drawn into the background.
    pub page_generation: u64,
    /// Where that page is (full-resolution GL px), `None` when it is not in the background.
    pub(crate) page_rect: Option<Region>,
}

impl GlassRenderer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        let (down_p, vao) = build_program(gl, DOWN_FRAGMENT)?;
        let cleanup = |progs: &[glow::NativeProgram]| {
            // SAFETY: deletes objects created just above.
            unsafe {
                for p in progs {
                    gl.delete_program(*p);
                }
                gl.delete_vertex_array(vao);
            }
        };
        let (up_p, vao2) = build_program(gl, UP_FRAGMENT).inspect_err(|_| cleanup(&[down_p]))?;
        let (glass, vao3) =
            build_program(gl, GLASS_FRAGMENT).inspect_err(|_| cleanup(&[down_p, up_p]))?;
        // SAFETY: uniform lookups on the programs just linked; the spare VAOs are deleted.
        unsafe {
            gl.delete_vertex_array(vao2);
            gl.delete_vertex_array(vao3);
            let pass = |program| Pass {
                program,
                u_src: gl.get_uniform_location(program, "u_src"),
                u_dst: gl.get_uniform_location(program, "u_dst"),
                u_off: gl.get_uniform_location(program, "u_off"),
            };
            let mut u: [Option<glow::NativeUniformLocation>; 9] = Default::default();
            for (slot, name) in u.iter_mut().zip(GLASS_UNIFORMS) {
                *slot = gl.get_uniform_location(glass, name);
            }
            Ok(GlassRenderer {
                vao,
                down: pass(down_p),
                up: pass(up_p),
                glass,
                u,
                downs: Vec::with_capacity(MAX_DEPTH + 1),
                ups: Vec::with_capacity(MAX_DEPTH + 1),
                size: (0, 0),
                cache: None,
            })
        }
    }

    /// Free every GL object.
    #[allow(dead_code)] // the renderer lives as long as the window
    pub fn destroy(mut self, gl: &glow::Context) {
        self.free_targets(gl);
        // SAFETY: deletes objects this renderer owns.
        unsafe {
            gl.delete_program(self.down.program);
            gl.delete_program(self.up.program);
            gl.delete_program(self.glass);
            gl.delete_vertex_array(self.vao);
        }
    }

    fn free_targets(&mut self, gl: &glow::Context) {
        for t in self.downs.drain(..).chain(self.ups.drain(..)) {
            t.delete(gl);
        }
        self.size = (0, 0);
        self.cache = None;
    }

    /// (Re)create the pyramid for a window size. Only allocates when the size changed.
    fn ensure_targets(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<(), String> {
        if self.size == (w, h) && !self.downs.is_empty() {
            return Ok(());
        }
        self.free_targets(gl);
        for i in 0..=MAX_DEPTH {
            let (lw, lh) = ((w >> (i + 1)).max(1), (h >> (i + 1)).max(1));
            let d = Target::new(gl, lw, lh);
            let u = Target::new(gl, lw, lh);
            match (d, u) {
                (Ok(d), Ok(u)) => {
                    self.downs.push(d);
                    self.ups.push(u);
                }
                (d, u) => {
                    let e = d.as_ref().err().or(u.as_ref().err()).cloned();
                    if let Ok(t) = d {
                        t.delete(gl);
                    }
                    if let Ok(t) = u {
                        t.delete(gl);
                    }
                    self.free_targets(gl);
                    return Err(e.unwrap_or_default());
                }
            }
        }
        self.size = (w, h);
        Ok(())
    }

    /// Blur the background (if needed) and draw `rects` (at most [`MAX_RECTS`]) into
    /// `frame.target_fb`. On return the target framebuffer is bound and state is clean.
    pub fn render(
        &mut self,
        gl: &glow::Context,
        frame: &Frame,
        rects: &[GlassRect],
    ) -> Result<(), String> {
        let rects = &rects[..rects.len().min(MAX_RECTS)];
        if rects.is_empty() || frame.width <= 0 || frame.height <= 0 {
            return Ok(());
        }
        self.ensure_targets(gl, frame.width, frame.height)?;
        let max_blur = rects.iter().fold(0.0f32, |m, r| m.max(r.blur));
        let (depth, off) = kawase_for_blur(max_blur);
        let needed = needed_region(rects, frame.width, frame.height);
        // Does the blur under the surfaces read any page pixels? (The ½-res level 0 is a
        // linear blit of the full-res backdrop: its region, doubled, plus the filter taps.)
        let reads_page = frame.page_rect.is_some_and(|page| {
            if FULL_BLUR.with(|f| f.get()) {
                return true;
            }
            let sizes: Vec<(i32, i32)> = self.downs.iter().map(|t| (t.w, t.h)).collect();
            let (down_r, _) = blur_regions(needed, &sizes, depth, off);
            down_r
                .first()
                .is_some_and(|r0| r0.double().grow(2).intersects(&page))
        });
        let key = BlurKey {
            generation: frame.generation,
            page_generation: reads_page.then_some(frame.page_generation),
            w: frame.width,
            h: frame.height,
            depth,
            off: off.to_bits(),
        };
        // SAFETY: draws with our own programs, VAO, textures and framebuffers; every piece
        // of state touched is reset before returning.
        unsafe {
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.color_mask(true, true, true, true);
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            let reuse = self
                .cache
                .is_some_and(|c| c.key == key && c.valid.contains(&needed));
            if !reuse && !needed.is_empty() {
                // Blur where the surfaces are (plus a margin for small moves), not the whole
                // window: the same pixels inside every surface, a fraction of the work when
                // the glass sits in the title bar over a web page.
                let pad = (BLUR_PAD_PX * frame.scale.max(0.5)).ceil() as i32;
                let valid = if FULL_BLUR.with(|f| f.get()) {
                    Region::full(frame.width, frame.height)
                } else {
                    needed.grow(pad).clamp(frame.width, frame.height)
                };
                self.blur(gl, frame, depth, off, valid);
                self.cache = Some(BlurCache { key, valid });
            }
            self.composite(gl, frame, rects, depth, max_blur);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(frame.target_fb));
            gl.viewport(0, 0, frame.width, frame.height);
        }
        Ok(())
    }

    /// # Safety
    /// Called from `render` with the VAO bound and blending off.
    unsafe fn blur(
        &self,
        gl: &glow::Context,
        frame: &Frame,
        depth: usize,
        off: f32,
        valid: Region,
    ) {
        let sizes: Vec<(i32, i32)> = self.downs.iter().map(|t| (t.w, t.h)).collect();
        let (down_r, up_r) = blur_regions(valid, &sizes, depth, off);
        // SAFETY: see the function contract; binds only objects this renderer owns plus
        // the caller's source framebuffer for reading.
        unsafe {
            // ½ resolution: a linear blit is a 2×2 box filter.
            let l0 = &self.downs[0];
            // Every pass below only shades its level's region (the scissor box also limits
            // the blit's writes; its source/destination mapping is unchanged).
            gl.enable(glow::SCISSOR_TEST);
            down_r[0].scissor(gl);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, fb(frame.source_fb));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(l0.fbo));
            gl.blit_framebuffer(
                0,
                0,
                frame.width,
                frame.height,
                0,
                0,
                l0.w,
                l0.h,
                glow::COLOR_BUFFER_BIT,
                glow::LINEAR,
            );
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            // Down: downs[i] -> downs[i + 1].
            gl.use_program(Some(self.down.program));
            gl.uniform_1_i32(self.down.u_src.as_ref(), 0);
            for i in 0..depth {
                let (src, dst) = (&self.downs[i], &self.downs[i + 1]);
                down_r[i + 1].scissor(gl);
                self.pass(gl, &self.down, src, dst, off);
            }
            // Up: (downs[depth] | ups[i + 1]) -> ups[i], ending at ½ resolution.
            gl.use_program(Some(self.up.program));
            gl.uniform_1_i32(self.up.u_src.as_ref(), 0);
            for i in (0..depth).rev() {
                let src = if i + 1 == depth {
                    &self.downs[depth]
                } else {
                    &self.ups[i + 1]
                };
                up_r[i].scissor(gl);
                self.pass(gl, &self.up, src, &self.ups[i], off);
            }
            gl.disable(glow::SCISSOR_TEST);
        }
    }

    /// # Safety
    /// The pass's program is in use and the VAO bound.
    unsafe fn pass(&self, gl: &glow::Context, p: &Pass, src: &Target, dst: &Target, off: f32) {
        // SAFETY: see the function contract.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(dst.fbo));
            gl.viewport(0, 0, dst.w, dst.h);
            gl.bind_texture(glow::TEXTURE_2D, Some(src.tex));
            gl.uniform_2_f32(p.u_dst.as_ref(), dst.w as f32, dst.h as f32);
            gl.uniform_2_f32(p.u_off.as_ref(), off / src.w as f32, off / src.h as f32);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }

    /// # Safety
    /// Called from `render` after the blur, VAO bound.
    unsafe fn composite(
        &self,
        gl: &glow::Context,
        frame: &Frame,
        rects: &[GlassRect],
        depth: usize,
        max_blur: f32,
    ) {
        let (w, h) = (frame.width, frame.height);
        let s = frame.scale.max(0.5);
        let heavy = &self.ups[0];
        let light = &self.downs[0];
        let u = |i: usize| self.u[i].as_ref();
        // SAFETY: see the function contract.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(frame.target_fb));
            gl.viewport(0, 0, w, h);
            gl.enable(glow::BLEND);
            // Keep the target's alpha as it is (opaque window).
            gl.blend_func_separate(
                glow::SRC_ALPHA,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ZERO,
                glow::ONE,
            );
            gl.enable(glow::SCISSOR_TEST);
            gl.use_program(Some(self.glass));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(
                glow::TEXTURE_2D,
                Some(if depth > 0 { heavy.tex } else { light.tex }),
            );
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(light.tex));
            gl.uniform_1_i32(u(0), 0);
            gl.uniform_1_i32(u(1), 1);
            gl.uniform_2_f32(u(2), w as f32, h as f32);
            let margin = 32.0 * s;
            for r in rects
                .iter()
                .filter(|r| r.w > 0.5 && r.h > 0.5 && r.opacity > 0.004)
            {
                let m = &r.material;
                // GL coordinates: y up.
                let (x0, x1) = (r.x, r.x + r.w);
                let (y0, y1) = (h as f32 - (r.y + r.h), h as f32 - r.y);
                gl.uniform_4_f32(u(3), x0, y0, x1, y1);
                let mix = if max_blur > 0.0 {
                    (r.blur / max_blur).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                gl.uniform_4_f32(u(4), r.radius, r.refract, r.band(s), mix);
                gl.uniform_4_f32(u(5), m.tint[0], m.tint[1], m.tint[2], m.tint[3]);
                gl.uniform_4_f32(u(6), m.rim, m.saturation, m.shadow, s);
                gl.uniform_1_f32(u(7), r.opacity);
                gl.uniform_1_f32(u(8), m.brightness);
                let sx0 = ((x0 - margin).floor() as i32).clamp(0, w);
                let sx1 = ((x1 + margin).ceil() as i32).clamp(0, w);
                // Only floating panels have a shadow; it falls a little below (GL y0).
                let (mb, mt) = if m.shadow > 0.0 {
                    (margin * 1.2, margin * 0.8)
                } else {
                    (2.0, 2.0)
                };
                let (sx0, sx1) = if m.shadow > 0.0 {
                    (sx0, sx1)
                } else {
                    (
                        ((x0 - 2.0).floor() as i32).clamp(0, w),
                        ((x1 + 2.0).ceil() as i32).clamp(0, w),
                    )
                };
                let sy0 = ((y0 - mb).floor() as i32).clamp(0, h);
                let sy1 = ((y1 + mt).ceil() as i32).clamp(0, h);
                // Cut off at the clip (top-left origin -> GL).
                let (sx0, sx1, sy0, sy1) = match r.clip {
                    Some([cx0, cy0, cx1, cy1]) => (
                        sx0.max(cx0.floor() as i32),
                        sx1.min(cx1.ceil() as i32),
                        sy0.max((h as f32 - cy1).floor() as i32),
                        sy1.min((h as f32 - cy0).ceil() as i32),
                    ),
                    None => (sx0, sx1, sy0, sy1),
                };
                if sx1 <= sx0 || sy1 <= sy0 {
                    continue;
                }
                gl.scissor(sx0, sy0, sx1 - sx0, sy1 - sy0);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
        }
    }
}

// ------------------------------------------------------------------------------------ sky

/// The MythDesk sky as `ui/design/sky.js` draws it in the approved prototype, one canvas
/// operation after another in the same order (source-over): the three-stop gradient, three
/// drifting washes, stars, the sun's (or moon's) glow, twelve slow rays and the disc, the
/// puffy clouds, then the calm cloud deck over the title bar and toolbar (a zenith band and
/// eleven banks, shaded undersides under lit tops) and the horizon haze. Positions come from
/// [`SkyScene::frame`] (the same seeded layout as sky.js), in CSS px with y down; the texture
/// is ½ resolution and drawn upscaled, as the prototype's 0.4× canvas is.
const SKY_FRAGMENT: &str = "
uniform vec2 u_view;      // texture size, px
uniform float u_px;       // texture px per CSS px
uniform vec3 u_top;
uniform vec3 u_mid;
uniform vec3 u_low;
uniform vec3 u_glow;
uniform vec3 u_sc;        // stars, night deck (0/1), dayish deck (0/1)
uniform vec4 u_wash[3];   // x, y, radius (CSS px)
uniform vec4 u_sun;       // x, y, glow radius, disc radius
uniform vec2 u_rays;      // rotation, day (0/1)
uniform vec4 u_raylen[3]; // 12 ray lengths
uniform vec4 u_cp[50];    // cloud puffs: x, y, rx, ry
uniform vec2 u_ca[50];    // cloud puffs: centre alpha, warm (0/1)
uniform int u_nc;
uniform vec4 u_dp[66];    // deck puffs: x, base y, r, bank
uniform vec2 u_bank[11];  // deck banks: R, alpha
uniform int u_nd;
uniform float u_dh;       // deck height, CSS px
uniform float u_t;        // seconds (star twinkle)
uniform vec3 u_key;       // deck: lit tops
uniform vec4 u_under;     // deck: undersides (rgb), strength (a)
out vec4 o_color;

float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
vec3 over(vec3 c, vec3 col, float a) { return mix(c, col, clamp(a, 0.0, 1.0)); }
float rayLen(int i) {
    vec4 v = u_raylen[i / 4];
    int k = i - (i / 4) * 4;
    return k == 0 ? v.x : k == 1 ? v.y : k == 2 ? v.z : v.w;
}
void main() {
    vec2 tp = vec2(gl_FragCoord.x, u_view.y - gl_FragCoord.y);
    vec2 p = tp / u_px;
    vec2 size = u_view / u_px;
    float y = p.y / size.y;
    // 1. gradient: top, mid at .55, low
    vec3 c = y < 0.55 ? mix(u_top, u_mid, y / 0.55) : mix(u_mid, u_low, (y - 0.55) / 0.45);
    // 2. washes
    vec3 wc = mix(u_glow, u_top, 0.5);
    for (int i = 0; i < 3; i++) {
        float t = distance(p, u_wash[i].xy) / u_wash[i].z;
        c = over(c, wc, 0.16 * max(0.0, 1.0 - t));
    }
    // 3. stars (a hashed field; sky.js has 140 small twinkling dots in the upper 70 %)
    if (u_sc.x > 0.01) {
        float cell = 44.0;
        vec2 g = floor(p / cell);
        float h = hash(g);
        if (h > 0.78) {
            vec2 sp = (g + vec2(hash(g + 3.1), hash(g + 7.7))) * cell;
            // sky.js draws them under a pixel wide on its 0.4x canvas: soft, faint dots.
            float r = (0.4 + hash(g + 1.3) * 1.1) * 0.9;
            float tw = 0.4 + 0.6 * sin(u_t / 0.7 + h * 18.0);
            float a = u_sc.x * max(0.1, tw) * 0.7 * smoothstep(r + 1.4, max(r - 0.8, 0.0), distance(p, sp));
            c = over(c, vec3(1.0), a * (1.0 - step(0.7, sp.y / size.y)));
        }
    }
    // 4. glow around the sun / moon
    bool day = u_rays.y > 0.5;
    float sd = distance(p, u_sun.xy) / u_sun.z;
    float g0 = day ? 0.24 : 0.22, g1 = day ? 0.07 : 0.07;
    c = over(c, u_glow, sd < 0.25 ? mix(g0, g1, sd / 0.25) : mix(g1, 0.0, clamp((sd - 0.25) / 0.75, 0.0, 1.0)));
    // 5. rays (by day): 12 thin triangles fading outwards, slowly turning
    if (day) {
        vec2 d = p - u_sun.xy;
        float L = length(d);
        float step6 = 0.5235988;
        float a = atan(d.y, d.x) - u_rays.x - step6;
        float k = floor(a / step6 + 0.5);
        float diff = a - k * step6;
        int i = int(mod(k, 12.0) + 0.5);
        if (i > 11) i = 0;
        float len = rayLen(i);
        float u = L * cos(diff), v = abs(L * sin(diff));
        float edge = smoothstep(0.0, 2.5, u * 0.06 - v);
        c = over(c, u_glow, 0.035 * max(0.0, 1.0 - u / len) * edge * step(0.0, u));
    }
    // 6. disc
    vec3 disc = day ? mix(u_glow, vec3(1.0), 0.35) : vec3(0.922, 0.933, 1.0);
    c = over(c, disc, (day ? 0.7 : 0.85) * smoothstep(u_sun.w + 1.25, u_sun.w - 1.25, distance(p, u_sun.xy)));
    // 7. clouds: soft elliptical puffs
    vec3 lit = mix(u_glow, vec3(1.0), 0.5);
    vec3 warm = mix(lit, u_low, 0.35), cool = mix(lit, u_mid, 0.25);
    for (int i = 0; i < 50; i++) {
        if (i >= u_nc) break;
        vec4 q = u_cp[i];
        vec2 e = (p - q.xy) / vec2(q.z, max(q.w, 0.01));
        float t = length(e);
        if (t >= 1.0) continue;
        float a0 = u_ca[i].x;
        float a = t < 0.6 ? mix(0.45 * a0, 0.14 * a0, t / 0.6) : mix(0.14 * a0, 0.0, (t - 0.6) / 0.4);
        c = over(c, u_ca[i].y > 0.5 ? warm : cool, a);
    }
    // 8. the deck over the chrome strip
    float zy = p.y / (u_dh * 1.25);
    if (zy < 1.0) {
        bool night = u_sc.y > 0.5;
        vec3 z0 = mix(u_top, vec3(0.0), night ? 0.25 : 0.08);
        float a0 = night ? 0.30 : 0.16, ak = night ? 0.03 : 0.06;
        vec3 zc; float za;
        if (zy < 0.55) { float s = zy / 0.55; zc = mix(z0, u_top, s); za = mix(a0, 0.0, s); }
        else if (zy < 0.82) { float s = (zy - 0.55) / 0.27; zc = mix(u_top, u_key, s); za = mix(0.0, ak, s); }
        else { float s = (zy - 0.82) / 0.18; zc = u_key; za = mix(ak, 0.0, s); }
        c = over(c, zc, za);
    }
    if (p.y < u_dh * 1.6) {
        vec3 under = mix(u_under.rgb, u_key, 0.45);
        float ka = u_under.a;
        int prev = -1;
        // Per bank: its undersides first, then its lit tops slightly higher.
        for (int i = 0; i < 66; i++) {
            if (i >= u_nd) break;
            int b = int(u_dp[i].w + 0.5);
            if (b == prev) continue;
            prev = b;
            vec2 bank = u_bank[b];
            for (int layer = 0; layer < 2; layer++) {
                float dy = layer == 0 ? 0.30 : -0.10;
                float a = (layer == 0 ? ka * 0.30 : ka) * bank.y;
                vec3 col = layer == 0 ? under : u_key;
                for (int j = i; j < 66; j++) {
                    if (j >= u_nd || int(u_dp[j].w + 0.5) != b) break;
                    vec4 q = u_dp[j];
                    vec2 e = (p - vec2(q.x, q.y + dy * bank.x * 0.55)) / vec2(q.z, q.z * 0.52);
                    float t = length(e);
                    if (t >= 1.0) continue;
                    c = over(c, col, t < 0.5 ? mix(a, 0.45 * a, t / 0.5) : mix(0.45 * a, 0.0, (t - 0.5) / 0.5));
                }
            }
        }
    }
    // 9. horizon haze
    if (y > 0.65) {
        float s = (y - 0.65) / 0.35;
        c = over(c, mix(u_low, mix(u_low, u_glow, 0.4), s), 0.4 * s);
    }
    o_color = vec4(clamp(c, 0.0, 1.0), 1.0);
}
";

const SKY_UNIFORMS: [&str; 21] = [
    "u_view", "u_px", "u_top", "u_mid", "u_low", "u_glow", "u_sc", "u_wash", "u_sun", "u_rays",
    "u_raylen", "u_cp", "u_ca", "u_nc", "u_dp", "u_bank", "u_nd", "u_dh", "u_t", "u_key",
    "u_under",
];

/// What the cached sky texture shows.
#[derive(Clone, Copy, PartialEq)]
struct SkyKey {
    w: i32,
    h: i32,
    hour: f32,
    t: f32,
    /// Texels left out (under the opaque page card), if any.
    hole: Option<Region>,
}

/// Renders the sky into a ½-resolution texture (cached until the hour, the clock step or the
/// size changes); [`LayerPainter`] draws it into the window.
pub struct SkyPainter {
    program: glow::NativeProgram,
    vao: glow::NativeVertexArray,
    u: [Option<glow::NativeUniformLocation>; 21],
    target: Option<Target>,
    key: Option<SkyKey>,
    scene: SkyScene,
}

fn flat4(v: &[[f32; 4]]) -> Vec<f32> {
    v.iter().flatten().copied().collect()
}

impl SkyPainter {
    fn new(gl: &glow::Context) -> Result<Self, String> {
        let (program, vao) = build_program(gl, SKY_FRAGMENT)?;
        let mut u: [Option<glow::NativeUniformLocation>; 21] = Default::default();
        // SAFETY: uniform lookups on the program just linked.
        unsafe {
            for (slot, name) in u.iter_mut().zip(SKY_UNIFORMS) {
                *slot = gl.get_uniform_location(program, name);
            }
        }
        Ok(SkyPainter {
            program,
            vao,
            u,
            target: None,
            key: None,
            scene: SkyScene::new(),
        })
    }

    /// The sky texture for a `w`×`h` window at `scale`, re-rendered if needed. `t` is the
    /// sky clock in seconds (it only advances while the sky animates).
    #[allow(clippy::too_many_arguments)]
    fn texture(
        &mut self,
        gl: &glow::Context,
        w: i32,
        h: i32,
        scale: f32,
        p: &Palette,
        t: f32,
        hole: Option<Region>,
    ) -> Result<glow::NativeTexture, String> {
        let (tw, th) = ((w / 2).max(1), (h / 2).max(1));
        if self.target.as_ref().is_none_or(|t| (t.w, t.h) != (tw, th)) {
            if let Some(old) = self.target.take() {
                old.delete(gl);
            }
            self.key = None;
            self.target = Some(Target::new(gl, tw, th)?);
        }
        let key = SkyKey {
            w: tw,
            h: th,
            hour: p.hour,
            t,
            hole,
        };
        let target = self.target.as_ref().ok_or("no sky target")?;
        if self.key == Some(key) {
            return Ok(target.tex);
        }
        let scale = scale.max(0.25);
        // Texture px per CSS px (the texture is ½ the window's device px).
        let px = tw as f32 / (w as f32 / scale);
        let f = self.scene.frame(p, t, w as f32 / scale, h as f32 / scale);
        let (deck_key, under, ka) = p.deck();
        let nc = f.cloud_puffs.len().min(SKY_CLOUD_PUFFS);
        let nd = f.bank_puffs.len().min(SKY_BANK_PUFFS);
        let wash = flat4(&f.washes);
        let raylen = f.ray_len;
        let cp = flat4(&f.cloud_puffs[..nc]);
        let ca: Vec<f32> = f.cloud_alpha[..nc].iter().flatten().copied().collect();
        let dp = flat4(&f.bank_puffs[..nd]);
        let banks: Vec<f32> = f.banks.iter().flatten().copied().collect();
        let u = |i: usize| self.u[i].as_ref();
        // SAFETY: draws with our own program into our own framebuffer; state is reset.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.fbo));
            gl.viewport(0, 0, tw, th);
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.uniform_2_f32(u(0), tw as f32, th as f32);
            gl.uniform_1_f32(u(1), px);
            gl.uniform_3_f32_slice(u(2), &p.top);
            gl.uniform_3_f32_slice(u(3), &p.mid);
            gl.uniform_3_f32_slice(u(4), &p.low);
            gl.uniform_3_f32_slice(u(5), &p.glow);
            gl.uniform_3_f32(
                u(6),
                p.stars,
                if p.stars > 0.3 { 1.0 } else { 0.0 },
                if p.cloud > 0.6 { 1.0 } else { 0.0 },
            );
            gl.uniform_4_f32_slice(u(7), &wash);
            gl.uniform_4_f32_slice(u(8), &f.sun);
            gl.uniform_2_f32_slice(u(9), &f.rays);
            gl.uniform_4_f32_slice(u(10), &raylen);
            if nc > 0 {
                gl.uniform_4_f32_slice(u(11), &cp);
                gl.uniform_2_f32_slice(u(12), &ca);
            }
            gl.uniform_1_i32(u(13), nc as i32);
            if nd > 0 {
                gl.uniform_4_f32_slice(u(14), &dp);
                gl.uniform_2_f32_slice(u(15), &banks);
            }
            gl.uniform_1_i32(u(16), nd as i32);
            gl.uniform_1_f32(u(17), f.deck_h);
            gl.uniform_1_f32(u(18), t);
            gl.uniform_3_f32_slice(u(19), &deck_key);
            gl.uniform_4_f32(u(20), under[0], under[1], under[2], ka);
            match hole {
                // Only around the hole: the sky shader (a loop over every cloud puff per
                // texel) is the costliest pass, and under the page card nobody sees it.
                Some(hole) => {
                    gl.enable(glow::SCISSOR_TEST);
                    for band in hole.bands_around(&Region::full(tw, th)) {
                        band.scissor(gl);
                        gl.draw_arrays(glow::TRIANGLES, 0, 3);
                    }
                    gl.disable(glow::SCISSOR_TEST);
                }
                None => gl.draw_arrays(glow::TRIANGLES, 0, 3),
            }
            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
        self.key = Some(key);
        Ok(target.tex)
    }
}

// ---------------------------------------------------------------------------------- layer

/// Puts a texture into the window: the sky (opaque), the page (premultiplied, optionally on
/// a white underlay and cut to the card's rounded rectangle) and the chrome (premultiplied).
const LAYER_FRAGMENT: &str = "
uniform sampler2D u_tex;
uniform vec4 u_dst;       // where the texture goes: x0, y0, x1, y1 (GL px)
uniform vec4 u_card;      // rounded mask: x0, y0, x1, y1 (GL px)
uniform vec3 u_opt;       // mask radius (0 = no mask), white underlay (0/1), dither (0/1)
out vec4 o_color;
float hash(vec2 p) { return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453); }
void main() {
    vec2 p = gl_FragCoord.xy;
    vec2 uv = (p - u_dst.xy) / max(u_dst.zw - u_dst.xy, vec2(1.0));
    vec4 c = texture(u_tex, uv);
    if (u_opt.y > 0.5) { c = vec4(c.rgb + vec3(1.0 - c.a), 1.0); }
    if (u_opt.x > 0.0) {
        vec2 m = (u_card.xy + u_card.zw) * 0.5;
        vec2 h = (u_card.zw - u_card.xy) * 0.5;
        float r = min(u_opt.x, min(h.x, h.y));
        vec2 q = abs(p - m) - h + vec2(r);
        float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
        c *= clamp(0.5 - d, 0.0, 1.0);
    }
    if (u_opt.z > 0.5) { c.rgb += (hash(p) - 0.5) * (2.0 / 255.0) * c.a; }
    o_color = c;
}
";

pub struct LayerPainter {
    program: glow::NativeProgram,
    vao: glow::NativeVertexArray,
    u: [Option<glow::NativeUniformLocation>; 4],
}

/// How [`LayerPainter::draw`] blends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Blend {
    /// Replace (the sky, opaque).
    Replace,
    /// Premultiplied over, keeping the window's alpha.
    Over,
}

impl LayerPainter {
    fn new(gl: &glow::Context) -> Result<Self, String> {
        let (program, vao) = build_program(gl, LAYER_FRAGMENT)?;
        let mut u: [Option<glow::NativeUniformLocation>; 4] = Default::default();
        // SAFETY: uniform lookups on the program just linked.
        unsafe {
            for (slot, name) in u.iter_mut().zip(["u_tex", "u_dst", "u_card", "u_opt"]) {
                *slot = gl.get_uniform_location(program, name);
            }
        }
        Ok(LayerPainter { program, vao, u })
    }

    /// Draw `tex` stretched over `dst` (GL px, x0 y0 x1 y1) into the bound `target_fb`.
    /// `mask`: a rounded rectangle (GL px) and radius to cut it to.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        gl: &glow::Context,
        target_fb: u32,
        (w, h): (i32, i32),
        tex: glow::NativeTexture,
        dst: [f32; 4],
        mask: Option<([f32; 4], f32)>,
        white: bool,
        dither: bool,
        blend: Blend,
        limit: Option<Region>,
    ) {
        let u = |i: usize| self.u[i].as_ref();
        let (card, radius) = mask.unwrap_or(([0.0; 4], 0.0));
        // SAFETY: draws into the caller's framebuffer with our own program/VAO/texture;
        // blend, scissor and bindings are reset before returning.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(target_fb));
            gl.viewport(0, 0, w, h);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
            match blend {
                Blend::Replace => gl.disable(glow::BLEND),
                Blend::Over => {
                    gl.enable(glow::BLEND);
                    gl.blend_func_separate(
                        glow::ONE,
                        glow::ONE_MINUS_SRC_ALPHA,
                        glow::ZERO,
                        glow::ONE,
                    );
                }
            }
            // Only the destination rectangle is shaded.
            gl.enable(glow::SCISSOR_TEST);
            let mut x0 = (dst[0].floor() as i32).clamp(0, w);
            let mut y0 = (dst[1].floor() as i32).clamp(0, h);
            let mut x1 = (dst[2].ceil() as i32).clamp(0, w);
            let mut y1 = (dst[3].ceil() as i32).clamp(0, h);
            if let Some(l) = limit {
                (x0, y0, x1, y1) = (x0.max(l.x0), y0.max(l.y0), x1.min(l.x1), y1.min(l.y1));
            }
            gl.scissor(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0));
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(u(0), 0);
            gl.uniform_4_f32(u(1), dst[0], dst[1], dst[2], dst[3]);
            gl.uniform_4_f32(u(2), card[0], card[1], card[2], card[3]);
            gl.uniform_3_f32(
                u(3),
                radius,
                if white { 1.0 } else { 0.0 },
                if dither { 1.0 } else { 0.0 },
            );
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
        }
    }
}

// ----------------------------------------------------------------------------- compositor

/// The page this frame shows, for [`Compositor::paint`].
pub struct PageLayer {
    /// The page's colour texture (premultiplied, the webview's own size).
    pub tex: u32,
    /// Its rectangle in the window (device px, top-left origin).
    pub rect: [f32; 4],
    /// Card: white underlay and rounded corners of this radius (device px); part of the
    /// backdrop. Internal pages: `None`, composed over the glass like the chrome.
    pub card_radius: Option<f32>,
}

/// One frame's inputs.
pub struct FrameInputs<'a> {
    pub window_fb: u32,
    pub width: i32,
    pub height: i32,
    pub scale: f32,
    pub palette: Palette,
    /// Sky clock, seconds (it only advances while the sky animates).
    pub sky_t: f32,
    pub page: Option<PageLayer>,
    pub chrome_tex: u32,
    pub glass: &'a [GlassRect],
    /// Bumped whenever the backdrop (sky, layout) changed.
    pub generation: u64,
    /// Bumped on every new web-page frame; re-blurs only if the glass reads the page.
    pub page_generation: u64,
    /// Draw the blur (`GlassMode::Blur`); `false` composes sky, page and UI only.
    pub blur: bool,
}

/// What a frame managed to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Painted {
    /// Sky, page, glass and UI.
    Glass,
    /// Sky, page and UI, but the blur failed (the UI should switch to its denser tint).
    NoBlur,
}

/// Every GL piece glass needs. Built once, before Servo starts (so the shell knows whether
/// to clear the webviews to transparent); see [`Compositor::new`].
pub struct Compositor {
    sky: SkyPainter,
    layer: LayerPainter,
    glass: GlassRenderer,
}

fn tex(name: u32) -> Option<glow::NativeTexture> {
    NonZeroU32::new(name).map(glow::NativeTexture)
}

impl Compositor {
    /// Build every program (and check the blur targets can be made at a small size). Any
    /// failure means no glass: the shell then keeps its classic composition.
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        let sky = SkyPainter::new(gl)?;
        let layer = LayerPainter::new(gl)?;
        let mut glass = GlassRenderer::new(gl)?;
        glass.ensure_targets(gl, 64, 64)?;
        Ok(Compositor { sky, layer, glass })
    }

    /// Compose a frame into `f.window_fb` (see the module docs). `Err` only when the sky or
    /// a layer could not be drawn at all; a blur failure gives `Ok(Painted::NoBlur)`.
    pub fn paint(&mut self, gl: &glow::Context, f: &FrameInputs<'_>) -> Result<Painted, String> {
        let (w, h) = (f.width, f.height);
        if w <= 0 || h <= 0 {
            return Ok(Painted::Glass);
        }
        let full = [0.0, 0.0, w as f32, h as f32];
        // 1. Sky. Under a web page's card (opaque: white underlay) the sky is never seen, so
        // neither the sky texture nor the window draws it there: `hole` is the card's
        // rectangle minus its rounded corners and 2 px (the ½-res sky is sampled bilinearly).
        let hole = f
            .page
            .as_ref()
            .and_then(|p| p.card_radius.map(|rad| (p.rect, rad)))
            .filter(|_| !FULL_BLUR.with(|x| x.get()))
            .map(|(r, rad)| {
                let inset = rad.max(0.0) + 2.0;
                Region {
                    x0: (r[0] + inset).ceil() as i32,
                    x1: (r[2] - inset).floor() as i32,
                    y0: (h as f32 - r[3] + inset).ceil() as i32,
                    y1: (h as f32 - r[1] - inset).floor() as i32,
                }
            })
            .filter(|r| !r.is_empty());
        let sky_hole = hole
            .map(|r| {
                let q = Region {
                    x0: (r.x0 + 1).div_euclid(2),
                    y0: (r.y0 + 1).div_euclid(2),
                    x1: r.x1.div_euclid(2),
                    y1: r.y1.div_euclid(2),
                };
                q.grow(-2)
            })
            .filter(|r| !r.is_empty());
        let sky = self
            .sky
            .texture(gl, w, h, f.scale, &f.palette, f.sky_t, sky_hole)?;
        let bands = match (hole, sky_hole) {
            (Some(hole), Some(_)) => hole.bands_around(&Region::full(w, h)),
            _ => vec![Region::full(w, h)],
        };
        for band in bands {
            self.layer.draw(
                gl,
                f.window_fb,
                (w, h),
                sky,
                full,
                None,
                false,
                true,
                Blend::Replace,
                Some(band),
            );
        }
        // 2. Page: a web page (card) is part of the backdrop. An internal page is UI: its glass
        // surfaces blur the sky behind it, and its own content goes on top of them (step 4),
        // otherwise its text would be blurred into its own glass.
        if let Some(page) = f.page.as_ref().filter(|p| p.card_radius.is_some())
            && let Some(t) = tex(page.tex)
        {
            let r = page.rect;
            let gl_rect = [r[0], h as f32 - r[3], r[2], h as f32 - r[1]];
            let mask = page.card_radius.map(|rad| (gl_rect, rad));
            self.layer.draw(
                gl,
                f.window_fb,
                (w, h),
                t,
                gl_rect,
                mask,
                page.card_radius.is_some(),
                false,
                Blend::Over,
                None,
            );
        }
        // 3. Glass.
        let mut painted = Painted::Glass;
        if f.blur && !f.glass.is_empty() {
            let frame = Frame {
                source_fb: f.window_fb,
                target_fb: f.window_fb,
                width: w,
                height: h,
                scale: f.scale,
                generation: f.generation,
                page_generation: f.page_generation,
                page_rect: f
                    .page
                    .as_ref()
                    .filter(|p| p.card_radius.is_some())
                    .map(|p| Region {
                        x0: p.rect[0].floor() as i32,
                        x1: p.rect[2].ceil() as i32,
                        y0: (h as f32 - p.rect[3]).floor() as i32,
                        y1: (h as f32 - p.rect[1]).ceil() as i32,
                    }),
            };
            if let Err(e) = self.glass.render(gl, &frame, f.glass) {
                log::warn!("liquid glass: {e}; this frame has no blur");
                painted = Painted::NoBlur;
            }
        }
        // 4. UI: an internal page's content, then the chrome.
        if let Some(page) = f.page.as_ref().filter(|p| p.card_radius.is_none())
            && let Some(t) = tex(page.tex)
        {
            let r = page.rect;
            let gl_rect = [r[0], h as f32 - r[3], r[2], h as f32 - r[1]];
            self.layer.draw(
                gl,
                f.window_fb,
                (w, h),
                t,
                gl_rect,
                None,
                false,
                false,
                Blend::Over,
                None,
            );
        }
        if let Some(t) = tex(f.chrome_tex) {
            self.layer.draw(
                gl,
                f.window_fb,
                (w, h),
                t,
                full,
                None,
                false,
                false,
                Blend::Over,
                None,
            );
        }
        // SAFETY: rebinds the window framebuffer with a full viewport (as compose leaves it).
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(f.window_fb));
            gl.viewport(0, 0, w, h);
        }
        Ok(painted)
    }
}

/// Whether the chrome webview really came out transparent: alpha at a few points where the
/// chrome draws nothing in glass mode (the middle of the page area). `None` if it could not
/// be read. One synchronous read, done once after the chrome switched its sky off.
pub fn chrome_is_transparent(gl: &glow::Context, chrome_fb: u32, w: i32, h: i32) -> Option<bool> {
    if chrome_fb == 0 || w < 8 || h < 8 {
        return None;
    }
    let points = [(w / 2, h / 2), (w / 4, h / 2), (w * 3 / 4, h * 2 / 5)];
    let mut max_alpha = 0u8;
    // SAFETY: reads 1×1 pixels from a live framebuffer of the current context; the read
    // framebuffer binding is restored to none.
    unsafe {
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, fb(chrome_fb));
        for (x, y) in points {
            let mut px = [0u8; 4];
            gl.read_pixels(
                x,
                y,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut px)),
            );
            max_alpha = max_alpha.max(px[3]);
        }
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
    }
    Some(max_alpha < 250)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refraction_is_thin_and_only_on_large_surfaces() {
        let omni = GlassRect::new(0.0, 0.0, 778.0, 38.0, 19.0, 1.0, Material::LIGHT);
        assert_eq!(omni.refract, 0.0);
        let panel = GlassRect::new(0.0, 0.0, 380.0, 352.0, 22.0, 1.0, Material::DARK);
        assert_eq!(panel.band(1.0), 6.0);
        assert_eq!(panel.refract, 4.0);
        let big = GlassRect::new(0.0, 0.0, 800.0, 800.0, 44.0, 2.0, Material::DARK);
        assert_eq!(big.refract, 8.0);
        let pill = GlassRect::new(0.0, 0.0, 340.0, 48.0, 24.0, 1.0, Material::DARK);
        assert!(pill.refract > 0.0 && pill.refract <= 4.0);
    }

    #[test]
    fn reported_surfaces_draw_the_backdrop_only() {
        let g = GlassRectPx {
            x: 10.0,
            y: 20.0,
            w: 400.0,
            h: 300.0,
            radius: 26.0,
            kind: GlassKind::Float,
            alpha: 1.0,
            clip: [0.0, 0.0, 800.0, 600.0],
        };
        let b = GlassLook::at(mythbrowse_engine::glass::TRANSPARENCY_DEFAULT, false, false);
        let r = GlassRect::from_report(&g, 2.0, &b);
        assert_eq!(r.material.tint, Material::BACKDROP.tint);
        assert_eq!((r.material.saturation, r.material.brightness), (1.5, 1.08));
        // Float 28 px × 0.5 at the default, at 2×.
        assert_eq!(r.blur, 28.0);
        let clear = GlassRect::from_report(&g, 2.0, &GlassLook::at(100, false, false));
        let solid_ish = GlassRect::from_report(&g, 2.0, &GlassLook::at(0, false, false));
        assert!(clear.blur < r.blur && r.blur < solid_ish.blur);
        assert!(r.refract > 0.0);
        assert_eq!(r.clip, Some([0.0, 0.0, 800.0, 600.0]));
        let chrome = GlassRect::from_report(
            &GlassRectPx {
                kind: GlassKind::Chrome,
                ..g
            },
            2.0,
            &b,
        );
        assert_eq!(chrome.refract, 0.0, "chrome surfaces never bend");
        let backdrop = Material::BACKDROP;
        assert_eq!((backdrop.tint[3], backdrop.shadow), (0.0, 0.0));
    }

    #[test]
    fn depth_grows_with_blur() {
        assert_eq!(depth_for_blur(0.0), 1);
        assert_eq!(depth_for_blur(12.0), 2);
        assert_eq!(depth_for_blur(18.0), 3);
        assert_eq!(depth_for_blur(24.0), 3);
        assert_eq!(depth_for_blur(36.0), MAX_DEPTH);
        assert!((1..=60).all(|b| depth_for_blur(b as f32) <= MAX_DEPTH));
    }
}
