// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Window compositing: the chrome (window-sized, it draws the sky everywhere) and the active
//! page (a floating card, see `mythbrowse_engine::frame`) are rendered by Servo into their
//! own offscreen framebuffers; this puts them into the window.
//!
//! * The chrome is copied only where the page does not cover it (the strip and the sky ring),
//!   so a page frame costs one window's worth of pixels, not two.
//! * The page goes in with Servo's own blit (a plain rectangle; Servo 0.1.4 has no way to
//!   clip a webview to a rounded rectangle).
//! * The card's corners are then rounded by re-drawing each `radius`² corner square from the
//!   chrome's colour texture, blended by how much of each pixel lies outside the rounded
//!   corner (anti-aliased). That is four tiny draws, only on frames Servo produced anyway;
//!   nothing runs between frames. If the shader cannot be built (old GL), the corners are cut
//!   with per-row framebuffer blits instead (same shape, not anti-aliased).
//!
//! GL state: like servoshell's egui overlay we draw with the window context's `glow` handle
//! between Servo's frames, and leave program / VAO / texture / blend / scissor unbound or
//! disabled afterwards. WebRender re-binds its own state at the start of every frame.

use std::num::NonZeroU32;

use glow::HasContext;
use log::{info, warn};
use mythbrowse_engine::frame::{OverlayRect, PxRect, WindowLayout, corner_cut};
use mythbrowse_engine::glow::GlowFrame;

/// Framebuffers and texture this frame composes from / into (raw GL names; 0 = none or the
/// default framebuffer).
pub struct Sources {
    pub chrome_fb: u32,
    pub chrome_tex: u32,
    pub window_fb: u32,
}

/// The currently bound `GL_FRAMEBUFFER` (after a context's `prepare_for_rendering`).
pub fn bound_framebuffer(gl: &glow::Context) -> u32 {
    // SAFETY: a plain state query on the current context.
    unsafe { gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING).max(0) as u32 }
}

/// The colour texture attached to the bound framebuffer (0 if it is not a texture).
pub fn bound_color_texture(gl: &glow::Context) -> u32 {
    // SAFETY: state queries on the current context and its bound framebuffer.
    unsafe {
        let kind = gl.get_framebuffer_attachment_parameter_i32(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
        );
        if kind as u32 != glow::TEXTURE {
            return 0;
        }
        gl.get_framebuffer_attachment_parameter_i32(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
        )
        .max(0) as u32
    }
}

fn fb(name: u32) -> Option<glow::NativeFramebuffer> {
    NonZeroU32::new(name).map(glow::NativeFramebuffer)
}

/// Copy `rects` (window coordinates, top-left origin) 1:1 from the chrome's framebuffer into
/// the window's. Both are window-sized, so source and target rectangles are the same.
pub fn copy_from_chrome(gl: &glow::Context, src: &Sources, window_h: i32, rects: &[PxRect]) {
    // SAFETY: framebuffer binds and blits between two live framebuffers of the current
    // context; the draw framebuffer is restored to the window's.
    unsafe {
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, fb(src.chrome_fb));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, fb(src.window_fb));
        for r in rects.iter().filter(|r| !r.is_empty()) {
            let g = r.to_gl(window_h);
            gl.blit_framebuffer(
                g.x,
                g.y,
                g.x + g.w,
                g.y + g.h,
                g.x,
                g.y,
                g.x + g.w,
                g.y + g.h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, fb(src.window_fb));
    }
}

/// The rows of sky that cut each corner of the card (fallback without the shader).
fn corner_cut_rects(layout: &WindowLayout) -> Vec<PxRect> {
    let r = layout.radius;
    let c = layout.content;
    let mut out = Vec::new();
    for row in 0..r {
        let cut = corner_cut(r, row);
        if cut <= 0 {
            continue;
        }
        let (top, bottom) = (c.y + row, c.y + c.h - 1 - row);
        let (left, right) = (c.x, c.x + c.w - cut);
        for (x, y) in [(left, top), (right, top), (left, bottom), (right, bottom)] {
            out.push(PxRect::new(x, y, cut, 1));
        }
    }
    out
}

const VERTEX: &str = "
void main() {
    // One triangle that covers the viewport; the scissor box picks the corner.
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

const FRAGMENT: &str = "
uniform sampler2D u_chrome;
uniform vec2 u_center;
uniform float u_radius;
out vec4 o_color;
void main() {
    vec4 sky = texelFetch(u_chrome, ivec2(gl_FragCoord.xy), 0);
    float outside = clamp(length(gl_FragCoord.xy - u_center) - u_radius + 0.5, 0.0, 1.0);
    o_color = vec4(sky.rgb, outside);
}
";

/// Rounds the card's corners (see the module docs). Built lazily on the first card frame.
pub struct CornerPainter {
    program: glow::NativeProgram,
    vao: glow::NativeVertexArray,
    u_chrome: Option<glow::NativeUniformLocation>,
    u_center: Option<glow::NativeUniformLocation>,
    u_radius: Option<glow::NativeUniformLocation>,
}

/// Shader state: not tried yet, ready, or unavailable (use the blit fallback).
pub enum Corners {
    Untried,
    Ready(CornerPainter),
    Fallback,
}

impl Corners {
    /// Round the card's corners in the window framebuffer (bound on return).
    pub fn paint(&mut self, gl: &glow::Context, layout: &WindowLayout, src: &Sources) {
        if layout.radius <= 0 || src.chrome_fb == 0 {
            return;
        }
        if matches!(self, Corners::Untried) {
            *self = match CornerPainter::new(gl) {
                Ok(painter) => {
                    info!("card corners: anti-aliased shader");
                    Corners::Ready(painter)
                }
                Err(e) => {
                    warn!("card corners: shader unavailable ({e}); using per-row blits");
                    Corners::Fallback
                }
            };
        }
        match self {
            Corners::Ready(painter) if src.chrome_tex != 0 => painter.draw(gl, layout, src),
            _ => copy_from_chrome(gl, src, layout.height, &corner_cut_rects(layout)),
        }
    }
}

/// Compile and link `VERTEX` + `fragment` for the context's GLSL dialect, plus an empty VAO
/// (the vertex shader makes its own triangle). Everything is deleted again on error.
pub(crate) fn build_program(
    gl: &glow::Context,
    fragment: &str,
) -> Result<(glow::NativeProgram, glow::NativeVertexArray), String> {
    let version = gl.version();
    let header = if version.is_embedded {
        if version.major < 3 {
            return Err(format!("OpenGL ES {}.{}", version.major, version.minor));
        }
        "#version 300 es\nprecision highp float;\nprecision highp int;\n"
    } else if version.major > 3 || (version.major == 3 && version.minor >= 2) {
        "#version 150\n"
    } else if version.major == 3 {
        "#version 130\n"
    } else {
        return Err(format!("OpenGL {}.{}", version.major, version.minor));
    };
    // SAFETY: shader/program/VAO creation on the current context; everything created
    // here is deleted on every error path.
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        let mut error = None;
        for (kind, body) in [
            (glow::VERTEX_SHADER, VERTEX),
            (glow::FRAGMENT_SHADER, fragment),
        ] {
            let shader = match gl.create_shader(kind) {
                Ok(s) => s,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            };
            gl.shader_source(shader, &format!("{header}{body}"));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                error = Some(gl.get_shader_info_log(shader));
                gl.delete_shader(shader);
                break;
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        if error.is_none() {
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                error = Some(gl.get_program_info_log(program));
            }
        }
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        if let Some(e) = error {
            gl.delete_program(program);
            return Err(e);
        }
        match gl.create_vertex_array() {
            Ok(vao) => Ok((program, vao)),
            Err(e) => {
                gl.delete_program(program);
                Err(e)
            }
        }
    }
}

impl CornerPainter {
    fn new(gl: &glow::Context) -> Result<Self, String> {
        let (program, vao) = build_program(gl, FRAGMENT)?;
        // SAFETY: uniform lookups on the program just linked.
        unsafe {
            Ok(CornerPainter {
                u_chrome: gl.get_uniform_location(program, "u_chrome"),
                u_center: gl.get_uniform_location(program, "u_center"),
                u_radius: gl.get_uniform_location(program, "u_radius"),
                program,
                vao,
            })
        }
    }

    fn draw(&self, gl: &glow::Context, layout: &WindowLayout, src: &Sources) {
        let Some(tex) = NonZeroU32::new(src.chrome_tex).map(glow::NativeTexture) else {
            return;
        };
        let (h, r) = (layout.height, layout.radius as f32);
        let c = layout.content;
        // SAFETY: draws into the window framebuffer with our own program/VAO/texture binding;
        // every piece of state touched is reset before returning.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(src.window_fb));
            gl.viewport(0, 0, layout.width, h);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
            gl.enable(glow::BLEND);
            // Keep the window's alpha as it is (opaque).
            gl.blend_func_separate(
                glow::SRC_ALPHA,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ZERO,
                glow::ONE,
            );
            gl.enable(glow::SCISSOR_TEST);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(self.u_chrome.as_ref(), 0);
            gl.uniform_1_f32(self.u_radius.as_ref(), r);
            for (_, square) in layout.corners() {
                // Circle centre: the square's inner corner (edge coordinates, GL y-up).
                let cx = if square.x == c.x {
                    (c.x as f32) + r
                } else {
                    (c.x + c.w) as f32 - r
                };
                let cy_top = if square.y == c.y {
                    (c.y as f32) + r
                } else {
                    (c.y + c.h) as f32 - r
                };
                gl.uniform_2_f32(self.u_center.as_ref(), cx, h as f32 - cy_top);
                let g = square.to_gl(h);
                gl.scissor(g.x, g.y, g.w, g.h);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
        }
    }
}

// ---------------------------------------------------------------------------- Myth overlay

/// Myth's pill / approval card (`WindowLayout::overlay`) is part of the chrome page, which is
/// window-sized and draws it at exactly the rectangle the layout gives (the shell passes it
/// in CSS px through `ChromeState::frame`). After the page is blitted, the surface is copied
/// from the chrome's colour texture over the page, masked to its rounded rectangle
/// (anti-aliased), with a soft drop shadow on the page around it. Pointer events inside it go
/// to the chrome (`WindowLayout::region_at`). Without the shader, the bounding rectangle is
/// copied as is (square corners).
const OVERLAY_FRAGMENT: &str = "
uniform sampler2D u_chrome;
uniform vec4 u_rect;      // x0, y0, x1, y1 (GL window px, y up)
uniform float u_radius;
uniform float u_px;       // device px per CSS px
out vec4 o_color;
float rbox(vec2 p, vec4 r, float rad) {
    vec2 c = (r.xy + r.zw) * 0.5;
    vec2 h = (r.zw - r.xy) * 0.5;
    vec2 q = abs(p - c) - h + vec2(rad);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - rad;
}
void main() {
    vec2 p = gl_FragCoord.xy;
    float d = rbox(p, u_rect, u_radius);
    float cov = clamp(0.5 - d, 0.0, 1.0);
    // Shadow: the same shape 4 px lower, blurred over ~14 px.
    float ds = rbox(p + vec2(0.0, 4.0 * u_px), u_rect, u_radius);
    float sh = 0.20 * exp(-pow(max(ds, 0.0) / (9.0 * u_px), 2.0));
    vec3 ui = texelFetch(u_chrome, ivec2(p), 0).rgb;
    float a = cov + sh * (1.0 - cov);
    vec3 col = a > 0.0 ? (ui * cov + vec3(0.04, 0.06, 0.16) * sh * (1.0 - cov)) / a : ui;
    o_color = vec4(col, a);
}
";

pub struct OverlayPainter {
    program: glow::NativeProgram,
    vao: glow::NativeVertexArray,
    u: [Option<glow::NativeUniformLocation>; 4],
}

/// Overlay shader state (like [`Corners`]).
pub enum Overlay {
    Untried,
    Ready(OverlayPainter),
    Fallback,
}

/// Where the overlay shader may draw: the surface plus its shadow, clipped to the page card.
#[cfg(test)]
pub fn overlay_scissor(layout: &WindowLayout, scale: f32) -> Option<PxRect> {
    surface_scissor(layout, layout.overlay?, scale)
}

/// [`overlay_scissor`] for any floating surface (the pill / card or the Tasks panel).
pub fn surface_scissor(layout: &WindowLayout, o: OverlayRect, scale: f32) -> Option<PxRect> {
    let m = (16.0 * scale).ceil() as i32;
    let r = o.rect;
    let c = layout.content;
    let x0 = (r.x - m).max(c.x);
    let y0 = (r.y - m).max(c.y);
    let x1 = (r.x + r.w + m).min(c.x + c.w);
    let y1 = (r.y + r.h + m).min(c.y + c.h);
    let s = PxRect::new(x0, y0, x1 - x0, y1 - y0);
    (!s.is_empty()).then_some(s)
}

impl Overlay {
    /// Draw Myth's surfaces (pill / card, Tasks panel) over the page (window framebuffer
    /// bound on return).
    pub fn paint(&mut self, gl: &glow::Context, layout: &WindowLayout, src: &Sources, scale: f32) {
        // The docked Myth panel sits on the sky (no page under it): only the floating one.
        let assist = layout
            .assist
            .filter(|a| a.kind == mythbrowse_engine::frame::MythOverlay::Assist);
        for o in [
            layout.overlay,
            layout.panel,
            assist,
            layout.selpill,
            layout.popup,
        ]
        .into_iter()
        .flatten()
        {
            self.paint_one(gl, layout, o, src, scale);
        }
    }

    fn paint_one(
        &mut self,
        gl: &glow::Context,
        layout: &WindowLayout,
        o: OverlayRect,
        src: &Sources,
        scale: f32,
    ) {
        if src.chrome_fb == 0 {
            return;
        }
        if matches!(self, Overlay::Untried) {
            *self = match build_program(gl, OVERLAY_FRAGMENT) {
                Ok((program, vao)) => {
                    info!("myth overlay: shader ready");
                    // SAFETY: uniform lookups on the program just linked.
                    let u = ["u_chrome", "u_rect", "u_radius", "u_px"]
                        .map(|n| unsafe { gl.get_uniform_location(program, n) });
                    Overlay::Ready(OverlayPainter { program, vao, u })
                }
                Err(e) => {
                    warn!("myth overlay: shader unavailable ({e}); square copy");
                    Overlay::Fallback
                }
            };
        }
        match self {
            Overlay::Ready(p) if src.chrome_tex != 0 => p.draw(gl, layout, o, src, scale),
            _ => copy_from_chrome(gl, src, layout.height, &[o.rect]),
        }
    }
}

impl OverlayPainter {
    fn draw(
        &self,
        gl: &glow::Context,
        layout: &WindowLayout,
        o: OverlayRect,
        src: &Sources,
        scale: f32,
    ) {
        let Some(sc) = surface_scissor(layout, o, scale) else {
            return;
        };
        let Some(tex) = NonZeroU32::new(src.chrome_tex).map(glow::NativeTexture) else {
            return;
        };
        let h = layout.height;
        let g = o.rect.to_gl(h);
        let u = |i: usize| self.u[i].as_ref();
        // SAFETY: draws into the window framebuffer with our own program/VAO/texture binding;
        // every piece of state touched is reset before returning.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(src.window_fb));
            gl.viewport(0, 0, layout.width, h);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::SRC_ALPHA,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ZERO,
                glow::ONE,
            );
            gl.enable(glow::SCISSOR_TEST);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(u(0), 0);
            gl.uniform_4_f32(
                u(1),
                g.x as f32,
                g.y as f32,
                (g.x + g.w) as f32,
                (g.y + g.h) as f32,
            );
            gl.uniform_1_f32(u(2), o.radius as f32);
            gl.uniform_1_f32(u(3), scale.max(0.5));
            let s = sc.to_gl(h);
            gl.scissor(s.x, s.y, s.w, s.h);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
        }
    }
}

// ------------------------------------------------------------------------------- glow

/// Inner and outer glow width of the card light, CSS px (the edge line is 2 px; all three
/// are mirrored in `GLOW_FRAGMENT`).
pub const GLOW_INNER_CSS: f32 = 6.0;
pub const GLOW_OUTER_CSS: f32 = 16.0;

/// The light on the card's edge (see `mythbrowse_engine::glow`): drawn after the page, its
/// corners and Myth's overlay, only on frames that show it. A 2 px solid gradient line just
/// inside the card edge, a faint inner glow over the page's outer few px, and a soft outer
/// glow fading across the sky gap. Same full-viewport triangle as the corners, scissored to
/// the bands along the edge ([`glow_bands`]), so only those pixels are shaded.
const GLOW_FRAGMENT: &str = "
uniform vec4 u_rect;      // card: x0, y0, x1, y1 (GL window px, y up)
uniform float u_radius;
uniform float u_px;       // device px per CSS px
uniform int u_mode;       // 0 flow, 1 ring, 2 top sweep
uniform vec3 u_c0;
uniform vec3 u_c1;
uniform vec3 u_c2;
uniform float u_alpha;
uniform float u_phase;
uniform vec3 u_pulse;     // x, y (GL px), strength (0 = none)
out vec4 o_color;
const float TAU = 6.2831853;
void main() {
    vec2 p = gl_FragCoord.xy;
    vec2 c = (u_rect.xy + u_rect.zw) * 0.5;
    vec2 h = (u_rect.zw - u_rect.xy) * 0.5;
    vec2 q = abs(p - c) - h + vec2(u_radius);
    // Signed distance to the card edge: > 0 on the sky, < 0 on the page.
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - u_radius;
    float lw = 2.0 * u_px;
    float iw = 6.0 * u_px;
    float ow = 16.0 * u_px;
    if (d > ow || d < -iw) { discard; }
    // Solid line on the card's own edge (anti-aliased both sides).
    float line = clamp(d + lw + 0.5, 0.0, 1.0) * clamp(0.5 - d, 0.0, 1.0);
    float outer = d > 0.0 ? pow(1.0 - d / ow, 2.2) : 0.0;
    float inner = d < -lw ? pow(1.0 - (-d - lw) / (iw - lw), 2.0) : 0.0;
    float k = max(line, max(0.8 * outer, 0.3 * inner));
    // Position along the perimeter, 0..1.
    float t = atan(p.y - c.y, p.x - c.x) / TAU + 0.5;
    vec3 col = u_c0;
    if (u_mode == 0) {
        // blue -> warm orange -> a little magenta -> back to blue, flowing round the card.
        float w = fract(t - u_phase);
        if (w < 0.38) col = mix(u_c0, u_c1, smoothstep(0.0, 0.38, w));
        else if (w < 0.52) col = mix(u_c1, u_c2, smoothstep(0.38, 0.52, w));
        else if (w < 0.68) col = mix(u_c2, u_c0, smoothstep(0.52, 0.68, w));
        else col = u_c0;
    } else if (u_mode == 1) {
        col = mix(u_c0, u_c1, 0.5 + 0.5 * sin(t * TAU * 2.0));
    } else {
        // Loading: a light segment sweeping along the top edge, outside the page only.
        if (d < 0.0 || p.y < u_rect.w) { discard; }
        float xs = mix(u_rect.x, u_rect.z, u_phase);
        float seg = exp(-pow((p.x - xs) / max((u_rect.z - u_rect.x) * 0.09, 1.0), 2.0));
        k = outer * (0.2 + 0.8 * seg);
        col = mix(u_c1, u_c0, seg);
    }
    float a = u_alpha * k;
    if (u_pulse.z > 0.0) {
        float pd = distance(p, u_pulse.xy);
        float pk = u_pulse.z * exp(-pow(pd / (42.0 * u_px), 2.0)) * max(line, outer);
        col = mix(col, vec3(1.0), clamp(pk, 0.0, 0.6));
        a = max(a, pk);
    }
    o_color = vec4(col, clamp(a, 0.0, 1.0));
}
";

pub struct GlowPainter {
    program: glow::NativeProgram,
    vao: glow::NativeVertexArray,
    u: [Option<glow::NativeUniformLocation>; 10],
}

/// Glow shader state (like [`Corners`]); without the shader the glow is simply not drawn.
pub enum Glow {
    Untried,
    Ready(GlowPainter),
    Unavailable,
}

/// The bands along the card edge the glow may touch (window coordinates): the sky gap
/// outside (up to the outer glow width, below the chrome strip, inside the window) and a
/// strip of the page inside the edge (`max(radius, inner)` wide, so rounded corners are
/// covered). The rest of the page is never shaded.
pub fn glow_bands(layout: &WindowLayout, scale: f32) -> Vec<PxRect> {
    let c = layout.content;
    if !layout.card || c.is_empty() {
        return Vec::new();
    }
    let out = ((GLOW_OUTER_CSS * scale).ceil() as i32).min(layout.gap.max(0));
    let inner = ((GLOW_INNER_CSS * scale).ceil() as i32).max(layout.radius);
    let inner = inner.min(c.w / 2).min(c.h / 2);
    let top = layout.chrome_h;
    let (x0, x1) = ((c.x - out).max(0), (c.x + c.w + out).min(layout.width));
    let (y0, y1) = ((c.y - out).max(top), (c.y + c.h + out).min(layout.height));
    let bands = [
        // top / bottom: full width
        PxRect::new(x0, y0, x1 - x0, c.y + inner - y0),
        PxRect::new(x0, c.y + c.h - inner, x1 - x0, y1 - (c.y + c.h - inner)),
        // left / right: between them
        PxRect::new(x0, c.y + inner, c.x + inner - x0, c.h - 2 * inner),
        PxRect::new(
            c.x + c.w - inner,
            c.y + inner,
            x1 - (c.x + c.w - inner),
            c.h - 2 * inner,
        ),
    ];
    bands.into_iter().filter(|b| !b.is_empty()).collect()
}

impl Glow {
    pub fn paint(
        &mut self,
        gl: &glow::Context,
        layout: &WindowLayout,
        window_fb: u32,
        scale: f32,
        f: &GlowFrame,
    ) {
        if !layout.card {
            return;
        }
        if matches!(self, Glow::Untried) {
            *self = match GlowPainter::new(gl) {
                Ok(p) => {
                    info!("card glow: shader ready");
                    Glow::Ready(p)
                }
                Err(e) => {
                    warn!("card glow: shader unavailable ({e}); no glow");
                    Glow::Unavailable
                }
            };
        }
        if let Glow::Ready(p) = self {
            p.draw(gl, layout, window_fb, scale, f);
        }
    }
}

impl GlowPainter {
    fn new(gl: &glow::Context) -> Result<Self, String> {
        let (program, vao) = build_program(gl, GLOW_FRAGMENT)?;
        let names = [
            "u_rect", "u_radius", "u_px", "u_mode", "u_c0", "u_c1", "u_c2", "u_alpha", "u_phase",
            "u_pulse",
        ];
        // SAFETY: uniform lookups on the program just linked.
        let u = names.map(|n| unsafe { gl.get_uniform_location(program, n) });
        Ok(GlowPainter { program, vao, u })
    }

    fn draw(
        &self,
        gl: &glow::Context,
        layout: &WindowLayout,
        window_fb: u32,
        scale: f32,
        f: &GlowFrame,
    ) {
        let bands = glow_bands(layout, scale);
        if bands.is_empty() {
            return;
        }
        let h = layout.height;
        let c = layout.content;
        let (x0, x1) = (c.x as f32, (c.x + c.w) as f32);
        let (y0, y1) = ((h - c.y - c.h) as f32, (h - c.y) as f32);
        // The pulse sits on the card edge nearest to the element.
        let pulse = f.pulse.map(|(px, py, a)| {
            let x = x0 + px * (x1 - x0);
            let y = y1 - py * (y1 - y0);
            let (dl, dr, db, dt) = (x - x0, x1 - x, y - y0, y1 - y);
            let m = dl.min(dr).min(db).min(dt);
            let (ex, ey) = if m == dl {
                (x0, y)
            } else if m == dr {
                (x1, y)
            } else if m == db {
                (x, y0)
            } else {
                (x, y1)
            };
            [ex, ey, a]
        });
        let u = |i: usize| self.u[i].as_ref();
        // SAFETY: draws into the window framebuffer with our own program/VAO; every piece of
        // state touched is reset before returning.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(window_fb));
            gl.viewport(0, 0, layout.width, h);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::SRC_ALPHA,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ZERO,
                glow::ONE,
            );
            gl.enable(glow::SCISSOR_TEST);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.uniform_4_f32(u(0), x0, y0, x1, y1);
            gl.uniform_1_f32(u(1), layout.radius as f32);
            gl.uniform_1_f32(u(2), scale.max(0.5));
            gl.uniform_1_i32(u(3), f.mode as i32);
            gl.uniform_3_f32_slice(u(4), &f.colors[0]);
            gl.uniform_3_f32_slice(u(5), &f.colors[1]);
            gl.uniform_3_f32_slice(u(6), &f.colors[2]);
            gl.uniform_1_f32(u(7), f.alpha);
            gl.uniform_1_f32(u(8), f.phase);
            gl.uniform_3_f32_slice(u(9), &pulse.unwrap_or([0.0, 0.0, 0.0]));
            for b in &bands {
                let g = b.to_gl(h);
                gl.scissor(g.x, g.y, g.w, g.h);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_cuts_stay_in_the_corner_squares() {
        let layout = WindowLayout::new(1280, 820, 1.0, 88.0, true, false);
        let rects = corner_cut_rects(&layout);
        assert!(!rects.is_empty());
        let squares: Vec<PxRect> = layout.corners().into_iter().map(|(_, s)| s).collect();
        for r in &rects {
            assert!(
                squares.iter().any(|s| r.x >= s.x
                    && r.y >= s.y
                    && r.x + r.w <= s.x + s.w
                    && r.y + r.h <= s.y + s.h),
                "{r:?} outside the corner squares"
            );
        }
        // The very corner pixels are cut.
        let c = layout.content;
        assert!(rects.iter().any(|r| r.contains(c.x as f32, c.y as f32)));
        assert!(
            rects
                .iter()
                .any(|r| r.contains((c.x + c.w - 1) as f32, (c.y + c.h - 1) as f32))
        );
        // No corners when the page fills the area edge to edge.
        let full = WindowLayout::new(1280, 820, 1.0, 88.0, false, false);
        assert!(corner_cut_rects(&full).is_empty());
    }

    #[test]
    fn glow_bands_hug_the_card_edge_inside_and_out() {
        use mythbrowse_engine::frame::{LayoutExtras, MythOverlay};
        for (maximized, ai) in [(false, 0.0), (true, 0.0), (true, 1.0), (false, 1.0)] {
            let extras = LayoutExtras {
                ai,
                overlay: MythOverlay::Pill,
                ..LayoutExtras::default()
            };
            let layout = WindowLayout::with_extras(1280, 820, 1.0, 88.0, true, maximized, extras);
            let bands = glow_bands(&layout, 1.0);
            assert_eq!(bands.len(), 4);
            let c = layout.content;
            let inner = (GLOW_INNER_CSS.ceil() as i32).max(layout.radius);
            for b in &bands {
                assert!(b.y >= layout.chrome_h, "{b:?} reaches into the chrome");
                assert!(b.x >= 0 && b.x + b.w <= layout.width);
                // Never deeper into the page than the edge strip.
                let core = PxRect::new(c.x + inner, c.y + inner, c.w - 2 * inner, c.h - 2 * inner);
                let ox = (b.x + b.w).min(core.x + core.w) - b.x.max(core.x);
                let oy = (b.y + b.h).min(core.y + core.h) - b.y.max(core.y);
                assert!(ox <= 0 || oy <= 0, "{b:?} covers the page");
            }
            // The edge line (inside the card) and the whole sky gap are covered.
            let gap = layout.gap;
            let pts = [
                (c.x, c.y + 300),
                (c.x + 1, c.y + 300),
                (c.x - gap, c.y + 300),
                (c.x + c.w - 1, c.y + 300),
                (c.x + c.w + gap - 1, c.y + 300),
                (c.x + 300, c.y),
                (c.x + 300, c.y - gap),
                (c.x + 300, c.y + c.h - 2),
                (c.x + 300, c.y + c.h + gap - 1),
                (c.x + 3, c.y + 3),
            ];
            for (x, y) in pts {
                assert!(
                    bands.iter().any(|b| b.contains(x as f32, y as f32)),
                    "({x},{y}) {maximized} {ai}"
                );
            }
        }
        // The AI gap gives the light room: 12 px of sky instead of 4 when maximized.
        let idle = WindowLayout::new(1280, 820, 1.0, 88.0, true, true);
        let busy = WindowLayout::with_extras(
            1280,
            820,
            1.0,
            88.0,
            true,
            true,
            LayoutExtras {
                ai: 1.0,
                overlay: MythOverlay::None,
                ..LayoutExtras::default()
            },
        );
        assert_eq!((idle.gap, busy.gap), (4, 12));
        assert!(glow_bands(&WindowLayout::new(1280, 820, 1.0, 88.0, false, false), 1.0).is_empty());
    }

    #[test]
    fn overlay_scissor_covers_the_pill_and_its_shadow_inside_the_card() {
        use mythbrowse_engine::frame::{LayoutExtras, MythOverlay};
        let layout = WindowLayout::with_extras(
            1024,
            728,
            1.0,
            88.0,
            true,
            true,
            LayoutExtras {
                ai: 1.0,
                overlay: MythOverlay::Card,
                ..LayoutExtras::default()
            },
        );
        let o = layout.overlay.unwrap().rect;
        let s = overlay_scissor(&layout, 1.0).unwrap();
        assert!(s.x <= o.x - 16 && s.y <= o.y - 16 && s.x + s.w >= o.x + o.w + 16);
        // Clipped to the card (the pill sits 16 px above its bottom edge).
        let c = layout.content;
        assert_eq!(s.y + s.h, c.y + c.h);
        assert!(
            overlay_scissor(&WindowLayout::new(1024, 728, 1.0, 88.0, true, true), 1.0).is_none()
        );
    }
}
