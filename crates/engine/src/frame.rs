// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Window layout ("the page floats on the sky") and page tint, engine-agnostic so both are
//! unit-tested without Servo.
//!
//! The chrome webview covers the **whole window** and draws the sky everywhere. Web pages are
//! shown as a rounded card that floats on it: a thin ring of sky ([`CARD_GAP_CSS`], smaller
//! when maximized) to the left, right and bottom, the same gap under the toolbar, and
//! [`CARD_RADIUS_CSS`] rounded corners. Internal pages (AI home, library) draw the sky
//! themselves and fill everything under the toolbar edge to edge, as before.
//!
//! Servo 0.1.4 cannot clip a webview to a rounded rectangle (an offscreen context is blitted
//! into the window as a plain rectangle), so the shell rounds the card itself: after the page
//! is blitted it re-draws the four corner squares from the chrome's framebuffer, blended by
//! the coverage of the rounded corner ([`WindowLayout::corners`]); see `crates/shell`.
//!
//! While Myth works in the card the gap eases wider ([`CARD_GAP_AI_CSS`]) so the glow on the
//! card's edge has room, and Myth's pill / approval card ([`MythOverlay`]) floats over the
//! bottom centre of the card. The chrome webview draws it at [`WindowLayout::overlay`]; the
//! shell copies that rounded rectangle over the page and routes pointer events inside it to
//! the chrome ([`WindowLayout::region_at`]). The page keeps its size under it.
//!
//! All rectangles here are in physical pixels with a **top-left** origin (window
//! coordinates, like pointer events); [`PxRect::to_gl`] flips one for OpenGL.

/// Sky gap around the page card, CSS px. Must match `--card-gap` in `ui/chrome.css`.
pub const CARD_GAP_CSS: f32 = 8.0;
/// Sky gap around the page card while the window is maximized, CSS px. Must match
/// `--card-gap` under `body.maximized` in `ui/chrome.css`.
pub const CARD_GAP_MAXIMIZED_CSS: f32 = 4.0;
/// Corner radius of the page card, CSS px. Must match `--card-radius` in `ui/chrome.css`.
pub const CARD_RADIUS_CSS: f32 = 14.0;
/// Sky gap around the card while Myth works in it (maximized or not), CSS px: room for the
/// glow outside the card edge. The shell eases between this and the normal gap
/// ([`LayoutExtras::ai`]); the chrome follows through `ChromeState::frame`.
pub const CARD_GAP_AI_CSS: f32 = 12.0;

/// Myth's status pill floating over the bottom centre of the page card, CSS px.
pub const MYTH_PILL_H_CSS: f32 = 50.0;
pub const MYTH_PILL_MAX_W_CSS: f32 = 420.0;
/// The pill's least width when the chrome asks for a content-sized one
/// ([`LayoutExtras::overlay_size`]).
pub const MYTH_PILL_MIN_W_CSS: f32 = 160.0;
/// The approval card the pill expands into: its height when the chrome has not measured its
/// content yet, and the range a measured height is kept in.
pub const MYTH_CARD_H_CSS: f32 = 256.0;
pub const MYTH_CARD_MIN_H_CSS: f32 = 150.0;
pub const MYTH_CARD_MAX_H_CSS: f32 = 420.0;
pub const MYTH_CARD_MAX_W_CSS: f32 = 440.0;
pub const MYTH_CARD_RADIUS_CSS: f32 = 28.0;
/// The compat panel (opened from the toolbar's compat indicator), floating over the top right
/// of the page card under the indicator.
pub const COMPAT_PANEL_W_CSS: f32 = 380.0;
pub const COMPAT_PANEL_MAX_H_CSS: f32 = 520.0;
pub const COMPAT_PANEL_RADIUS_CSS: f32 = 26.0;
/// Space between the compat panel and the card's top / right edges.
pub const COMPAT_PANEL_INSET_CSS: f32 = 10.0;
/// The Tasks panel (toolbar Tasks button / Ctrl+J): floats over the top right of the page
/// area (the card, or an internal page), like the approved design's `#tasks`.
pub const TASKS_PANEL_W_CSS: f32 = 372.0;
/// Its height until the chrome has measured the content, and the least height.
pub const TASKS_PANEL_H_CSS: f32 = 420.0;
pub const TASKS_PANEL_MIN_H_CSS: f32 = 120.0;
pub const TASKS_PANEL_RADIUS_CSS: f32 = 26.0;
/// Space above and to the right of the panel: inside the card (the design's 12 / 22 px from
/// the content area, less the card's own 2 / 8 px inset), or inside an internal page.
pub const TASKS_PANEL_TOP_CSS: f32 = 10.0;
pub const TASKS_PANEL_RIGHT_CSS: f32 = 14.0;
pub const TASKS_PANEL_TOP_INTERNAL_CSS: f32 = 12.0;
pub const TASKS_PANEL_RIGHT_INTERNAL_CSS: f32 = 22.0;
/// Space between the pill / card and the card's bottom edge, and the least space to its sides.
pub const MYTH_BOTTOM_CSS: f32 = 28.0;
pub const MYTH_SIDE_CSS: f32 = 24.0;

/// Which Myth surface floats over the page card (drawn by the chrome webview, composed over
/// the page by the shell; see [`WindowLayout::overlay`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MythOverlay {
    #[default]
    None,
    /// Status pill: dot, "Myth", the current step, Stop (or Done + Open answer).
    Pill,
    /// Approval card: what Myth wants to do, where, Allow / Don't allow.
    Card,
    /// Compat panel: detector findings, applied rules, "Ask Myth to fix", undo (top right).
    Compat,
    /// Passwords panel: "Save password?" or the saved accounts to fill (top right, same
    /// place and size as the compat panel; the two never show together).
    Passwords,
    /// Tasks panel (top right; [`WindowLayout::panel`], shown together with the pill).
    Tasks,
    /// A chrome popup over the page: the omnibox suggestions, a context menu, a page's
    /// dialog or permission sheet ([`WindowLayout::popup`]; placed by the chrome itself).
    Popup,
    /// The Myth panel, floating over the card ([`WindowLayout::assist`]).
    Assist,
    /// The Myth panel docked right of the card (over the sky, not the page).
    AssistDocked,
    /// The selection pill ([`WindowLayout::selpill`]).
    SelPill,
}

/// The Myth panel (`docs/AI-OVERLAY.md`). Docked: a column right of the card, the card
/// shrinks to make room. Floating: a card over the page placed by the chrome (dragged by its
/// header), clamped inside the page card.
pub const ASSIST_DOCK_W_CSS: f32 = 380.0;
pub const ASSIST_DOCK_MIN_W_CSS: f32 = 320.0;
pub const ASSIST_DOCK_MAX_W_CSS: f32 = 560.0;
/// Least page-card width beside a docked panel; narrower windows float the panel instead.
pub const ASSIST_DOCK_MIN_CARD_W_CSS: f32 = 520.0;
pub const ASSIST_FLOAT_W_CSS: f32 = 380.0;
pub const ASSIST_FLOAT_H_CSS: f32 = 560.0;
pub const ASSIST_FLOAT_MIN_W_CSS: f32 = 300.0;
pub const ASSIST_FLOAT_MIN_H_CSS: f32 = 280.0;
/// Space between a floating panel and the card's edges when it is pushed back inside.
pub const ASSIST_FLOAT_INSET_CSS: f32 = 10.0;
pub const ASSIST_RADIUS_CSS: f32 = 22.0;
/// The selection pill (Ask Myth / Explain / Translate / Summarize / Rewrite) over the page.
pub const SELPILL_H_CSS: f32 = 38.0;
pub const SELPILL_W_CSS: f32 = 452.0;

/// Where the Myth panel goes (from the shell's state; CSS px).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssistCss {
    /// A column `w` wide right of the card.
    Docked { w: f32 },
    /// A floating card at this window rectangle (`None`: the default spot, top right).
    Floating(Option<PopupCss>),
}

/// Most a chrome popup may cover, CSS px (a bogus report never takes the whole window).
pub const POPUP_MAX_W_CSS: f32 = 720.0;
pub const POPUP_MAX_H_CSS: f32 = 720.0;

/// A chrome popup's rectangle as the chrome measured it (window CSS px, top-left origin)
/// and its corner radius.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PopupCss {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

/// Layout inputs beyond the window itself.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutExtras {
    /// 0..1: how far the sky gap has eased from the normal gap to [`CARD_GAP_AI_CSS`].
    pub ai: f32,
    pub overlay: MythOverlay,
    /// The size (CSS px) the chrome measured for the pill / approval card's content: the
    /// pill's width (its height stays [`MYTH_PILL_H_CSS`]), the card's height (its width
    /// stays [`MYTH_CARD_MAX_W_CSS`]). Kept within the min / max constants and the card.
    pub overlay_size: Option<(f32, f32)>,
    /// The Tasks panel is open: its measured content height (CSS px), if known.
    pub tasks: Option<Option<f32>>,
    /// A chrome popup is open over the page (see [`MythOverlay::Popup`]).
    pub popup: Option<PopupCss>,
    /// The Myth panel is open (web pages only).
    pub assist: Option<AssistCss>,
    /// The selection pill, window CSS px (the shell places it by the selection).
    pub selpill: Option<PopupCss>,
}

/// The floating Myth surface: where it is (window px) and its corner radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayRect {
    pub kind: MythOverlay,
    pub rect: PxRect,
    pub radius: i32,
}

impl OverlayRect {
    /// Whether a pointer position falls on the (rounded) surface; pixel centres, like the
    /// compositor's coverage.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        rounded_outside(self.rect, self.radius as f32, x, y) <= 0.0
    }
}

/// Signed distance-ish test for a pixel centre against a rounded rectangle: > 0 outside.
fn rounded_outside(r: PxRect, radius: f32, x: f32, y: f32) -> f32 {
    let (px, py) = (x.floor() + 0.5, y.floor() + 0.5);
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let (hx, hy) = (r.w as f32 / 2.0, r.h as f32 / 2.0);
    let qx = (px - cx).abs() - hx + radius;
    let qy = (py - cy).abs() - hy + radius;
    let outer = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outer + qx.max(qy).min(0.0) - radius
}

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl PxRect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        PxRect { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Whether the point (a pointer position; pixel `(i, j)` covers `[i, i+1)`) is inside.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && y >= self.y as f32
            && x < (self.x + self.w) as f32
            && y < (self.y + self.h) as f32
    }

    /// The same rectangle with a bottom-left origin (OpenGL), in a window `window_h` tall.
    pub fn to_gl(&self, window_h: i32) -> PxRect {
        PxRect::new(self.x, window_h - self.y - self.h, self.w, self.h)
    }
}

/// What is under a window point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// The chrome strip (title bar + toolbar).
    Chrome,
    /// The sky around the page card (and the rounded-off card corners): inert.
    Frame,
    /// The page.
    Content,
}

/// Which corner of the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Where the chrome and the page go in a window, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowLayout {
    pub width: i32,
    pub height: i32,
    /// Height of the chrome strip (title bar + toolbar).
    pub chrome_h: i32,
    /// The page's rectangle (the active tab's webview is exactly this size).
    pub content: PxRect,
    /// Corner radius of the page card (0 when the page fills the area edge to edge).
    pub radius: i32,
    /// The page is shown as a floating card (web pages) rather than edge to edge (internal
    /// pages).
    pub card: bool,
    /// Width of the sky gap around the card (0 edge to edge).
    pub gap: i32,
    /// Myth's pill / approval card over the bottom of the card, if shown. Pointer events on it
    /// go to the chrome, which draws it.
    pub overlay: Option<OverlayRect>,
    /// The Tasks panel over the top right of the page area (card or internal page), if
    /// open. Like `overlay`: drawn by the chrome, pointer events go to the chrome.
    pub panel: Option<OverlayRect>,
    /// A chrome popup (suggestions, context menu, dialog / permission sheet) over the page,
    /// clipped to the window. Drawn by the chrome, pointer events go to the chrome.
    pub popup: Option<OverlayRect>,
    /// The Myth panel: docked right of the card ([`MythOverlay::AssistDocked`]; the card is
    /// narrower) or floating over it ([`MythOverlay::Assist`]). Drawn by the chrome, pointer
    /// events go to the chrome.
    pub assist: Option<OverlayRect>,
    /// The selection pill over the page, clipped to the card.
    pub selpill: Option<OverlayRect>,
}

impl WindowLayout {
    /// Lay out a `width` x `height` window (physical px) at `scale` device px per CSS px.
    /// `chrome_css_h` is the chrome strip's height in CSS px.
    pub fn new(
        width: u32,
        height: u32,
        scale: f64,
        chrome_css_h: f32,
        card: bool,
        maximized: bool,
    ) -> Self {
        Self::with_extras(
            width,
            height,
            scale,
            chrome_css_h,
            card,
            maximized,
            LayoutExtras::default(),
        )
    }

    /// [`WindowLayout::new`] plus Myth's widened gap and floating surface.
    pub fn with_extras(
        width: u32,
        height: u32,
        scale: f64,
        chrome_css_h: f32,
        card: bool,
        maximized: bool,
        extras: LayoutExtras,
    ) -> Self {
        let width = width.max(1) as i32;
        let height = height.max(2) as i32;
        let px = |css: f32| ((css as f64) * scale).round() as i32;
        let chrome_h = px(chrome_css_h).clamp(1, height - 1);
        let below = height - chrome_h;
        if !card {
            let content = PxRect::new(0, chrome_h, width, below.max(1));
            return WindowLayout {
                width,
                height,
                chrome_h,
                content,
                radius: 0,
                card: false,
                gap: 0,
                overlay: None,
                panel: Self::place_panel(extras.tasks, content, scale, false),
                popup: Self::place_popup(extras.popup, width, height, scale),
                assist: None,
                selpill: None,
            };
        }
        let base_css = if maximized {
            CARD_GAP_MAXIMIZED_CSS
        } else {
            CARD_GAP_CSS
        };
        let ai = if extras.ai.is_finite() {
            extras.ai.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let gap_css = base_css + (CARD_GAP_AI_CSS - base_css) * ai;
        // Keep at least a 1 px page even in absurdly small windows.
        let gap = px(gap_css)
            .max(1)
            .min((width - 1) / 2)
            .min((below - 1).max(0) / 2)
            .max(0);
        let mut content = PxRect::new(
            gap,
            chrome_h + gap,
            (width - 2 * gap).max(1),
            (below - 2 * gap).max(1),
        );
        // A docked Myth panel takes a column on the right; the card shrinks (the page relays
        // out, nothing is covered). Too narrow a window floats the panel instead.
        let mut assist_css = extras.assist;
        let mut docked = None;
        if let Some(AssistCss::Docked { w }) = assist_css {
            let w = if w.is_finite() { w } else { ASSIST_DOCK_W_CSS };
            let dock_w = px(w.clamp(ASSIST_DOCK_MIN_W_CSS, ASSIST_DOCK_MAX_W_CSS));
            if content.w - dock_w - gap >= px(ASSIST_DOCK_MIN_CARD_W_CSS) {
                content.w -= dock_w + gap;
                docked = Some(OverlayRect {
                    kind: MythOverlay::AssistDocked,
                    rect: PxRect::new(content.x + content.w + gap, content.y, dock_w, content.h),
                    radius: px(ASSIST_RADIUS_CSS).min(dock_w / 2).min(content.h / 2),
                });
            } else {
                assist_css = Some(AssistCss::Floating(None));
            }
        }
        let radius = px(CARD_RADIUS_CSS)
            .min(content.w / 2)
            .min(content.h / 2)
            .max(0);
        let overlay = Self::place_overlay(extras.overlay, content, scale, extras.overlay_size);
        let assist = docked.or_else(|| match assist_css {
            Some(AssistCss::Floating(rect)) => Self::place_assist_float(rect, content, scale),
            _ => None,
        });
        let selpill = Self::place_selpill(extras.selpill, content, scale);
        WindowLayout {
            width,
            height,
            chrome_h,
            content,
            radius,
            card: true,
            gap,
            overlay,
            panel: Self::place_panel(extras.tasks, content, scale, true),
            popup: Self::place_popup(extras.popup, width, height, scale),
            assist,
            selpill,
        }
    }

    /// The floating Myth panel: the chrome's rectangle (or the default spot, top right of
    /// the card), its size kept in range and the whole panel pushed back inside the card.
    fn place_assist_float(
        rect: Option<PopupCss>,
        content: PxRect,
        scale: f64,
    ) -> Option<OverlayRect> {
        let px = |css: f32| ((css as f64) * scale).round() as i32;
        let inset = px(ASSIST_FLOAT_INSET_CSS);
        let room_w = content.w - 2 * inset;
        let room_h = content.h - 2 * inset;
        if room_w < px(ASSIST_FLOAT_MIN_W_CSS) || room_h < px(ASSIST_FLOAT_MIN_H_CSS) {
            return None;
        }
        let r = rect.filter(|r| [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite()));
        let w = px(r
            .map_or(ASSIST_FLOAT_W_CSS, |r| r.w)
            .max(ASSIST_FLOAT_MIN_W_CSS))
        .min(room_w);
        let h = px(r
            .map_or(ASSIST_FLOAT_H_CSS, |r| r.h)
            .max(ASSIST_FLOAT_MIN_H_CSS))
        .min(room_h);
        let (x, y) = match r {
            Some(r) => (px(r.x), px(r.y)),
            None => (content.x + content.w - inset - w, content.y + inset),
        };
        let x = x.clamp(content.x + inset, content.x + content.w - inset - w);
        let y = y.clamp(content.y + inset, content.y + content.h - inset - h);
        Some(OverlayRect {
            kind: MythOverlay::Assist,
            rect: PxRect::new(x, y, w, h),
            radius: px(ASSIST_RADIUS_CSS).min(w / 2).min(h / 2),
        })
    }

    /// The selection pill, kept inside the card (moved, never cut).
    fn place_selpill(pill: Option<PopupCss>, content: PxRect, scale: f64) -> Option<OverlayRect> {
        let p = pill?;
        if ![p.x, p.y, p.w, p.h].iter().all(|v| v.is_finite()) {
            return None;
        }
        let px = |css: f32| ((css as f64) * scale).round() as i32;
        let w = px(p.w.clamp(120.0, SELPILL_W_CSS * 1.5));
        let h = px(p.h.clamp(24.0, SELPILL_H_CSS * 2.0));
        if w > content.w || h > content.h {
            return None;
        }
        let x = px(p.x).clamp(content.x, content.x + content.w - w);
        let y = px(p.y).clamp(content.y, content.y + content.h - h);
        Some(OverlayRect {
            kind: MythOverlay::SelPill,
            rect: PxRect::new(x, y, w, h),
            radius: h / 2,
        })
    }

    /// A chrome popup in window px: clipped to the window and to the size caps; `None` when
    /// absent, not finite, or empty after clipping.
    fn place_popup(
        popup: Option<PopupCss>,
        width: i32,
        height: i32,
        scale: f64,
    ) -> Option<OverlayRect> {
        let p = popup?;
        if ![p.x, p.y, p.w, p.h, p.r].iter().all(|v| v.is_finite()) {
            return None;
        }
        let px = |css: f32| ((css as f64) * scale).round() as i32;
        let x0 = px(p.x).clamp(0, width);
        let y0 = px(p.y).clamp(0, height);
        let x1 = (px(p.x) + px(p.w.clamp(0.0, POPUP_MAX_W_CSS))).clamp(0, width);
        let y1 = (px(p.y) + px(p.h.clamp(0.0, POPUP_MAX_H_CSS))).clamp(0, height);
        let rect = PxRect::new(x0, y0, x1 - x0, y1 - y0);
        if rect.is_empty() {
            return None;
        }
        Some(OverlayRect {
            kind: MythOverlay::Popup,
            rect,
            radius: px(p.r.max(0.0)).min(rect.w / 2).min(rect.h / 2),
        })
    }

    /// Top right of the page area: the Tasks panel, `tasks` = open (with the measured content
    /// height, if any). `None` when closed or the area is too small to hold it.
    fn place_panel(
        tasks: Option<Option<f32>>,
        content: PxRect,
        scale: f64,
        card: bool,
    ) -> Option<OverlayRect> {
        let want_h = tasks?;
        let px = |css: f32| ((css as f64) * scale).round() as i32;
        let (top, right) = if card {
            (px(TASKS_PANEL_TOP_CSS), px(TASKS_PANEL_RIGHT_CSS))
        } else {
            (
                px(TASKS_PANEL_TOP_INTERNAL_CSS),
                px(TASKS_PANEL_RIGHT_INTERNAL_CSS),
            )
        };
        let w = px(TASKS_PANEL_W_CSS).min(content.w - 2 * right);
        let max_h = content.h - 2 * top - px(2.0);
        let h_css = want_h
            .filter(|h| h.is_finite())
            .unwrap_or(TASKS_PANEL_H_CSS)
            .max(TASKS_PANEL_MIN_H_CSS);
        let h = px(h_css).min(max_h);
        if w < px(240.0) || h < px(TASKS_PANEL_MIN_H_CSS) {
            return None;
        }
        Some(OverlayRect {
            kind: MythOverlay::Tasks,
            rect: PxRect::new(content.x + content.w - right - w, content.y + top, w, h),
            radius: px(TASKS_PANEL_RADIUS_CSS).min(h / 2).min(w / 2),
        })
    }

    /// Bottom-centre of the card: the pill, or the taller approval card. `None` when the card
    /// is too small to hold it.
    fn place_overlay(
        kind: MythOverlay,
        content: PxRect,
        scale: f64,
        size: Option<(f32, f32)>,
    ) -> Option<OverlayRect> {
        let px = |css: f32| ((css as f64) * scale).round() as i32;
        if matches!(kind, MythOverlay::Compat | MythOverlay::Passwords) {
            let inset = px(COMPAT_PANEL_INSET_CSS);
            let w = px(COMPAT_PANEL_W_CSS).min(content.w - 2 * inset);
            let h = px(COMPAT_PANEL_MAX_H_CSS).min(content.h - 2 * inset);
            if w < px(240.0) || h < px(160.0) {
                return None;
            }
            return Some(OverlayRect {
                kind,
                rect: PxRect::new(content.x + content.w - inset - w, content.y + inset, w, h),
                radius: px(COMPAT_PANEL_RADIUS_CSS).min(h / 2).min(w / 2),
            });
        }
        let size = size.filter(|(w, h)| w.is_finite() && h.is_finite());
        let (max_w, h, radius) = match kind {
            MythOverlay::None
            | MythOverlay::Compat
            | MythOverlay::Passwords
            | MythOverlay::Tasks
            | MythOverlay::Popup
            | MythOverlay::Assist
            | MythOverlay::AssistDocked
            | MythOverlay::SelPill => return None,
            MythOverlay::Pill => (
                size.map_or(MYTH_PILL_MAX_W_CSS, |(w, _)| {
                    w.clamp(MYTH_PILL_MIN_W_CSS, MYTH_PILL_MAX_W_CSS)
                }),
                MYTH_PILL_H_CSS,
                MYTH_PILL_H_CSS / 2.0,
            ),
            MythOverlay::Card => (
                MYTH_CARD_MAX_W_CSS,
                size.map_or(MYTH_CARD_H_CSS, |(_, h)| {
                    h.clamp(MYTH_CARD_MIN_H_CSS, MYTH_CARD_MAX_H_CSS)
                }),
                MYTH_CARD_RADIUS_CSS,
            ),
        };
        let (bottom, side) = (px(MYTH_BOTTOM_CSS), px(MYTH_SIDE_CSS));
        let w = px(max_w).min(content.w - 2 * side);
        let h = px(h);
        if w < px(160.0) || h + 2 * bottom > content.h {
            return None;
        }
        let x = content.x + (content.w - w) / 2;
        let y = content.y + content.h - bottom - h;
        Some(OverlayRect {
            kind,
            rect: PxRect::new(x, y, w, h),
            radius: px(radius).min(h / 2).min(w / 2),
        })
    }

    /// Size of the page's webview.
    pub fn content_size(&self) -> (u32, u32) {
        (self.content.w.max(1) as u32, self.content.h.max(1) as u32)
    }

    /// The region under a window point (pointer coordinates, physical px).
    pub fn region_at(&self, x: f32, y: f32) -> Region {
        if self.overlay.is_some_and(|o| o.contains(x, y))
            || self.panel.is_some_and(|o| o.contains(x, y))
            || self.popup.is_some_and(|o| o.contains(x, y))
            || self.selpill.is_some_and(|o| o.contains(x, y))
            || self.assist.is_some_and(|o| o.contains(x, y))
        {
            return Region::Chrome;
        }
        if self.content.contains(x, y) {
            if self.in_cut_corner(x, y) {
                Region::Frame
            } else {
                Region::Content
            }
        } else if y < self.chrome_h as f32 {
            Region::Chrome
        } else {
            Region::Frame
        }
    }

    /// A window point in the page's coordinates (what the page's webview gets for pointer,
    /// wheel and IME positions). Not clamped: drags may leave the page.
    pub fn to_content(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.content.x as f32, y - self.content.y as f32)
    }

    /// A rectangle in the page's coordinates, in window coordinates (e.g. where the IME
    /// candidate window goes).
    pub fn content_to_window(&self, r: PxRect) -> PxRect {
        PxRect::new(r.x + self.content.x, r.y + self.content.y, r.w, r.h)
    }

    /// The `radius` x `radius` squares at the card's corners (empty when not rounded).
    pub fn corners(&self) -> Vec<(Corner, PxRect)> {
        let r = self.radius;
        if r <= 0 || !self.card {
            return Vec::new();
        }
        let c = self.content;
        vec![
            (Corner::TopLeft, PxRect::new(c.x, c.y, r, r)),
            (Corner::TopRight, PxRect::new(c.x + c.w - r, c.y, r, r)),
            (Corner::BottomLeft, PxRect::new(c.x, c.y + c.h - r, r, r)),
            (
                Corner::BottomRight,
                PxRect::new(c.x + c.w - r, c.y + c.h - r, r, r),
            ),
        ]
    }

    /// The parts of the window the page does not cover (the chrome strip and the sky ring
    /// around the card), so only those are copied from the chrome's framebuffer.
    pub fn chrome_bands(&self) -> Vec<PxRect> {
        let c = self.content;
        let bands = [
            PxRect::new(0, 0, self.width, c.y),
            PxRect::new(0, c.y + c.h, self.width, self.height - (c.y + c.h)),
            PxRect::new(0, c.y, c.x, c.h),
            PxRect::new(c.x + c.w, c.y, self.width - (c.x + c.w), c.h),
        ];
        bands.into_iter().filter(|b| !b.is_empty()).collect()
    }

    /// Whether a point inside the content rectangle lies outside the rounded corner.
    fn in_cut_corner(&self, x: f32, y: f32) -> bool {
        let r = self.radius as f32;
        if r <= 0.0 || !self.card {
            return false;
        }
        let c = self.content;
        let (left, top) = (c.x as f32, c.y as f32);
        let (right, bottom) = ((c.x + c.w) as f32, (c.y + c.h) as f32);
        // Centre of the corner's circle, if the point is in a corner square.
        let cx = if x < left + r {
            left + r
        } else if x >= right - r {
            right - r
        } else {
            return false;
        };
        let cy = if y < top + r {
            top + r
        } else if y >= bottom - r {
            bottom - r
        } else {
            return false;
        };
        // Test the pixel centre, as the corner drawing does.
        let (dx, dy) = (x.floor() + 0.5 - cx, y.floor() + 0.5 - cy);
        dx * dx + dy * dy > r * r
    }
}

/// For row `row` of a corner of radius `radius` (row 0 is the outermost edge row), how many
/// pixels from the side lie outside the rounded corner. Used for the plain-blit fallback
/// when the corner shader is not available.
pub fn corner_cut(radius: i32, row: i32) -> i32 {
    if radius <= 0 || row < 0 || row >= radius {
        return 0;
    }
    let r = radius as f64;
    let dy = r - (row as f64 + 0.5);
    (r - (r * r - dy * dy).max(0.0).sqrt()).round() as i32
}

// ------------------------------------------------------------------------------- page tint

/// Script run in a loaded page: its `<meta name="theme-color">` (honouring `media`), else
/// its background colour (body, then root), normalised through a 2D canvas so named colours
/// come back as `#rrggbb` / `rgba(...)`. Returns a string ("" if nothing usable). Read-only:
/// it never changes the page.
pub const PAGE_TINT_SCRIPT: &str = r##"(function () {
  try {
    var pick = "";
    var metas = document.querySelectorAll('meta[name="theme-color"]');
    for (var i = 0; i < metas.length; i++) {
      var m = metas[i], media = m.getAttribute("media");
      if (media && window.matchMedia && !window.matchMedia(media).matches) continue;
      if (m.content) { pick = "theme:" + m.content; break; }
    }
    if (!pick) {
      var els = [document.body, document.documentElement];
      for (var j = 0; j < els.length; j++) {
        if (!els[j]) continue;
        var bg = getComputedStyle(els[j]).backgroundColor;
        if (bg && bg !== "transparent" && !/^rgba\(.*,\s*0\)$/.test(bg)) { pick = "bg:" + bg; break; }
      }
      if (!pick) pick = "bg:#ffffff";
    }
    var kind = pick.slice(0, pick.indexOf(":")), value = pick.slice(pick.indexOf(":") + 1);
    try {
      var x = document.createElement("canvas").getContext("2d");
      x.fillStyle = "#000000"; x.fillStyle = value;
      if (x.fillStyle) value = String(x.fillStyle);
    } catch (e) {}
    return kind + ":" + value;
  } catch (e) { return ""; }
})()"##;

/// A tint colour for the chrome, from [`PAGE_TINT_SCRIPT`]'s result (`theme:<color>` or
/// `bg:<color>`). `None` when there is nothing usable (unparsable, or mostly transparent).
pub fn page_tint(result: &str) -> Option<[u8; 3]> {
    let (_, color) = result.split_once(':')?;
    let [r, g, b, a] = parse_css_color(color)?;
    (a >= 128).then_some([r, g, b])
}

/// Parse the CSS colour forms pages and canvases produce: `#rgb`, `#rgba`, `#rrggbb`,
/// `#rrggbbaa`, `rgb()` / `rgba()` with commas or spaces (alpha as a number or a
/// percentage), and `transparent`. Returns RGBA.
pub fn parse_css_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().to_ascii_lowercase();
    if s == "transparent" {
        return Some([0, 0, 0, 0]);
    }
    if let Some(hex) = s.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let nib = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return match hex.len() {
            3 => Some([nib(0)?, nib(1)?, nib(2)?, 255]),
            4 => Some([nib(0)?, nib(1)?, nib(2)?, nib(3)?]),
            6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
            8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
            _ => None,
        };
    }
    let inner = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let (rgb_part, alpha_part) = match inner.split_once('/') {
        Some((c, a)) => (c, Some(a.trim())),
        None => (inner, None),
    };
    let parts: Vec<&str> = rgb_part
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    let (channels, alpha) = match (parts.as_slice(), alpha_part) {
        ([r, g, b], a) => ([*r, *g, *b], a),
        ([r, g, b, a], None) => ([*r, *g, *b], Some(*a)),
        _ => return None,
    };
    let channel = |p: &str| -> Option<u8> {
        let v = match p.strip_suffix('%') {
            Some(pct) => pct.parse::<f64>().ok()? * 255.0 / 100.0,
            None => p.parse::<f64>().ok()?,
        };
        v.is_finite().then(|| v.round().clamp(0.0, 255.0) as u8)
    };
    let alpha = match alpha {
        None => 255,
        Some(a) => {
            let v = match a.strip_suffix('%') {
                Some(pct) => pct.parse::<f64>().ok()? / 100.0,
                None => a.parse::<f64>().ok()?,
            };
            if !v.is_finite() {
                return None;
            }
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        }
    };
    Some([
        channel(channels[0])?,
        channel(channels[1])?,
        channel(channels[2])?,
        alpha,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: f32 = 88.0;

    #[test]
    fn internal_pages_fill_under_the_chrome() {
        let l = WindowLayout::new(1280, 820, 1.0, CHROME, false, false);
        assert_eq!(l.chrome_h, 88);
        assert_eq!(l.content, PxRect::new(0, 88, 1280, 732));
        assert_eq!(l.radius, 0);
        assert!(l.corners().is_empty());
        assert_eq!(l.region_at(5.0, 10.0), Region::Chrome);
        assert_eq!(l.region_at(0.0, 88.0), Region::Content);
        assert_eq!(l.region_at(1279.0, 819.0), Region::Content);
        assert_eq!(l.to_content(10.0, 100.0), (10.0, 12.0));
    }

    #[test]
    fn web_pages_float_as_a_card() {
        let l = WindowLayout::new(1280, 820, 1.0, CHROME, true, false);
        assert_eq!(l.content, PxRect::new(8, 96, 1264, 716));
        assert_eq!(l.content_size(), (1264, 716));
        assert_eq!(l.radius, 14);
        // Gutters belong to the frame, the strip to the chrome.
        assert_eq!(l.region_at(4.0, 400.0), Region::Frame);
        assert_eq!(l.region_at(1275.0, 400.0), Region::Frame);
        assert_eq!(l.region_at(600.0, 815.0), Region::Frame);
        assert_eq!(l.region_at(600.0, 92.0), Region::Frame);
        assert_eq!(l.region_at(600.0, 87.5), Region::Chrome);
        assert_eq!(l.region_at(600.0, 96.0), Region::Content);
        // Page coordinates are offset by the card's origin.
        assert_eq!(l.to_content(8.0, 96.0), (0.0, 0.0));
        assert_eq!(l.to_content(34.0, 122.0), (26.0, 26.0));
        assert_eq!(
            l.content_to_window(PxRect::new(10, 20, 100, 18)),
            PxRect::new(18, 116, 100, 18)
        );
    }

    #[test]
    fn maximized_card_has_a_thinner_ring() {
        let l = WindowLayout::new(1920, 1040, 1.0, CHROME, true, true);
        assert_eq!(l.content, PxRect::new(4, 92, 1912, 944));
    }

    #[test]
    fn layout_scales_with_dpi() {
        let l = WindowLayout::new(2560, 1640, 2.0, CHROME, true, false);
        assert_eq!(l.chrome_h, 176);
        assert_eq!(l.content, PxRect::new(16, 192, 2528, 1432));
        assert_eq!(l.radius, 28);
        // 150 %: 88 * 1.5 = 132, 8 * 1.5 = 12, 14 * 1.5 = 21.
        let l = WindowLayout::new(1920, 1230, 1.5, CHROME, true, false);
        assert_eq!(
            (l.chrome_h, l.content.x, l.content.y, l.radius),
            (132, 12, 144, 21)
        );
        assert_eq!(l.region_at(12.0, 144.0 + 100.0), Region::Content);
        assert_eq!(l.region_at(11.0, 144.0 + 100.0), Region::Frame);
    }

    #[test]
    fn rounded_corners_are_not_page() {
        let l = WindowLayout::new(1280, 820, 1.0, CHROME, true, false);
        let c = l.content;
        // The very corner pixel of each corner is cut off; the middle of an edge is not.
        assert_eq!(l.region_at(c.x as f32, c.y as f32), Region::Frame);
        assert_eq!(
            l.region_at((c.x + c.w - 1) as f32, c.y as f32),
            Region::Frame
        );
        assert_eq!(
            l.region_at(c.x as f32, (c.y + c.h - 1) as f32),
            Region::Frame
        );
        assert_eq!(
            l.region_at((c.x + c.w - 1) as f32, (c.y + c.h - 1) as f32),
            Region::Frame
        );
        assert_eq!(l.region_at((c.x + 40) as f32, c.y as f32), Region::Content);
        assert_eq!(l.region_at(c.x as f32, (c.y + 40) as f32), Region::Content);
        // Inside the arc near the corner.
        assert_eq!(
            l.region_at((c.x + 6) as f32, (c.y + 6) as f32),
            Region::Content
        );
    }

    #[test]
    fn corners_and_bands_tile_the_window() {
        let l = WindowLayout::new(1280, 820, 1.0, CHROME, true, false);
        let corners = l.corners();
        assert_eq!(corners.len(), 4);
        assert_eq!(corners[0], (Corner::TopLeft, PxRect::new(8, 96, 14, 14)));
        assert_eq!(
            corners[3],
            (Corner::BottomRight, PxRect::new(1258, 798, 14, 14))
        );
        // Bands + content cover the window exactly once.
        let area: i64 = l
            .chrome_bands()
            .iter()
            .chain(std::iter::once(&l.content))
            .map(|r| r.w as i64 * r.h as i64)
            .sum();
        assert_eq!(area, 1280 * 820);
        for b in l.chrome_bands() {
            assert!(!b.is_empty());
        }
        // Edge-to-edge layout: only the strip is chrome.
        let full = WindowLayout::new(1280, 820, 1.0, CHROME, false, false);
        assert_eq!(full.chrome_bands(), vec![PxRect::new(0, 0, 1280, 88)]);
    }

    #[test]
    fn gl_rects_flip_vertically() {
        let l = WindowLayout::new(1280, 820, 1.0, CHROME, true, false);
        assert_eq!(l.content.to_gl(820), PxRect::new(8, 8, 1264, 716));
        assert_eq!(
            PxRect::new(0, 0, 1280, 88).to_gl(820),
            PxRect::new(0, 732, 1280, 88)
        );
    }

    #[test]
    fn tiny_windows_stay_valid() {
        for (w, h) in [(1, 1), (3, 3), (20, 90), (560, 360)] {
            for card in [false, true] {
                let l = WindowLayout::new(w, h, 2.0, CHROME, card, false);
                assert!(l.content.w >= 1 && l.content.h >= 1, "{w}x{h} {card}");
                assert!(l.content.x >= 0 && l.content.y >= 1);
                assert!(l.radius * 2 <= l.content.w.max(l.content.h));
            }
        }
    }

    fn ext(ai: f32, overlay: MythOverlay) -> LayoutExtras {
        LayoutExtras {
            ai,
            overlay,
            ..LayoutExtras::default()
        }
    }

    #[test]
    fn pill_and_card_follow_the_measured_content() {
        let size = |w: f32, h: f32, kind| {
            WindowLayout::with_extras(
                1440,
                900,
                1.0,
                CHROME,
                true,
                false,
                LayoutExtras {
                    overlay: kind,
                    overlay_size: Some((w, h)),
                    ..LayoutExtras::default()
                },
            )
            .overlay
            .unwrap()
        };
        // The pill takes the measured width (kept in 160..420), centred; height stays 50.
        let p = size(343.0, 50.0, MythOverlay::Pill);
        assert_eq!((p.rect.w, p.rect.h, p.radius), (343, 50, 25));
        let l = WindowLayout::new(1440, 900, 1.0, CHROME, true, false);
        assert_eq!(p.rect.x, l.content.x + (l.content.w - 343) / 2);
        assert_eq!(size(40.0, 50.0, MythOverlay::Pill).rect.w, 160);
        assert_eq!(size(900.0, 50.0, MythOverlay::Pill).rect.w, 420);
        // The card keeps its width and takes the measured height (150..420).
        let c = size(1.0, 238.0, MythOverlay::Card);
        assert_eq!((c.rect.w, c.rect.h), (440, 238));
        assert_eq!(c.rect.y + c.rect.h, l.content.y + l.content.h - 28);
        assert_eq!(size(1.0, 2000.0, MythOverlay::Card).rect.h, 420);
        assert_eq!(size(1.0, f32::NAN, MythOverlay::Card).rect.h, 256);
    }

    #[test]
    fn tasks_panel_floats_top_right_on_cards_and_internal_pages() {
        let open = |card: bool, h: Option<f32>| {
            WindowLayout::with_extras(
                1440,
                900,
                1.0,
                CHROME,
                card,
                false,
                LayoutExtras {
                    overlay: if card {
                        MythOverlay::Pill
                    } else {
                        MythOverlay::None
                    },
                    tasks: Some(h),
                    ..LayoutExtras::default()
                },
            )
        };
        let l = open(true, Some(380.0));
        let p = l.panel.unwrap();
        assert_eq!(p.kind, MythOverlay::Tasks);
        assert_eq!(
            p.rect,
            PxRect::new(
                l.content.x + l.content.w - 14 - 372,
                l.content.y + 10,
                372,
                380
            )
        );
        // Shown together with the pill; both route pointer events to the chrome.
        assert!(l.overlay.is_some());
        assert_eq!(
            l.region_at((p.rect.x + 40) as f32, (p.rect.y + 40) as f32),
            Region::Chrome
        );
        // Internal page: the design's 12 / 22 px from the page area; default height.
        let i = open(false, None);
        let q = i.panel.unwrap();
        assert_eq!(q.rect, PxRect::new(1440 - 22 - 372, 88 + 12, 372, 420));
        assert_eq!(i.region_at(1300.0, 200.0), Region::Chrome);
        // Taller than the area: clipped to it.
        let t = open(true, Some(5000.0)).panel.unwrap();
        assert!(t.rect.y + t.rect.h < l.content.y + l.content.h);
        // Closed: none.
        assert_eq!(
            WindowLayout::new(1440, 900, 1.0, CHROME, true, false).panel,
            None
        );
    }

    #[test]
    fn ai_widens_the_gap_and_eases() {
        // Maximized 4 px -> 12 px, normal 8 px -> 12 px; halfway is in between.
        let l = WindowLayout::with_extras(
            1920,
            1040,
            1.0,
            CHROME,
            true,
            true,
            ext(1.0, MythOverlay::None),
        );
        assert_eq!((l.gap, l.content), (12, PxRect::new(12, 100, 1896, 928)));
        let l = WindowLayout::with_extras(
            1280,
            820,
            1.0,
            CHROME,
            true,
            false,
            ext(1.0, MythOverlay::None),
        );
        assert_eq!(l.content, PxRect::new(12, 100, 1256, 708));
        let half = WindowLayout::with_extras(
            1920,
            1040,
            1.0,
            CHROME,
            true,
            true,
            ext(0.5, MythOverlay::None),
        );
        assert_eq!(half.gap, 8);
        let none = WindowLayout::with_extras(
            1920,
            1040,
            1.0,
            CHROME,
            true,
            true,
            ext(0.0, MythOverlay::None),
        );
        assert_eq!(none, WindowLayout::new(1920, 1040, 1.0, CHROME, true, true));
        // Out-of-range / NaN easing is clamped; internal pages never get a gap.
        let nan = WindowLayout::with_extras(
            1920,
            1040,
            1.0,
            CHROME,
            true,
            true,
            ext(f32::NAN, MythOverlay::None),
        );
        assert_eq!(nan.gap, 4);
        let internal = WindowLayout::with_extras(
            1920,
            1040,
            1.0,
            CHROME,
            false,
            true,
            ext(1.0, MythOverlay::Pill),
        );
        assert_eq!((internal.gap, internal.overlay), (0, None));
        // Page coordinates follow the wider gap.
        assert_eq!(l.to_content(12.0, 100.0), (0.0, 0.0));
        assert_eq!(l.region_at(11.0, 400.0), Region::Frame);
        assert_eq!(l.region_at(12.0, 400.0), Region::Content);
    }

    #[test]
    fn compat_panel_sits_top_right_and_belongs_to_the_chrome() {
        let l = WindowLayout::with_extras(
            1280,
            820,
            1.0,
            CHROME,
            true,
            false,
            ext(0.0, MythOverlay::Compat),
        );
        assert_eq!(l.content, PxRect::new(8, 96, 1264, 716));
        let o = l.overlay.unwrap();
        assert_eq!(o.kind, MythOverlay::Compat);
        assert_eq!((o.rect, o.radius), (PxRect::new(882, 106, 380, 520), 26));
        assert_eq!(l.region_at(900.0, 300.0), Region::Chrome);
        assert_eq!(l.region_at(800.0, 300.0), Region::Content);
        // Shorter windows shrink it; tiny ones drop it.
        let short = WindowLayout::with_extras(
            1280,
            500,
            1.0,
            CHROME,
            true,
            false,
            ext(0.0, MythOverlay::Compat),
        );
        assert_eq!(short.overlay.unwrap().rect.h, short.content.h - 20);
        let tiny = WindowLayout::with_extras(
            260,
            300,
            1.0,
            CHROME,
            true,
            false,
            ext(0.0, MythOverlay::Compat),
        );
        assert_eq!(tiny.overlay, None);
    }

    #[test]
    fn pill_floats_over_the_bottom_centre_and_belongs_to_the_chrome() {
        let l = WindowLayout::with_extras(
            1024,
            728,
            1.0,
            CHROME,
            true,
            true,
            ext(1.0, MythOverlay::Pill),
        );
        let c = l.content;
        assert_eq!(c, PxRect::new(12, 100, 1000, 616));
        let o = l.overlay.expect("pill");
        assert_eq!(o.kind, MythOverlay::Pill);
        assert_eq!(o.rect, PxRect::new(12 + 290, 100 + 616 - 28 - 50, 420, 50));
        assert_eq!(o.radius, 25);
        // Centred in the card.
        assert_eq!(o.rect.x - c.x, c.x + c.w - (o.rect.x + o.rect.w));
        // The page keeps its full size: the pill floats over it.
        assert_eq!(l.content_size(), (1000, 616));
        let (mx, my) = ((o.rect.x + o.rect.w / 2) as f32, (o.rect.y + 25) as f32);
        assert_eq!(l.region_at(mx, my), Region::Chrome);
        // Rounded ends: the corner of the bounding box is still page.
        assert_eq!(
            l.region_at(o.rect.x as f32, o.rect.y as f32),
            Region::Content
        );
        assert_eq!(
            l.region_at((o.rect.x + 25) as f32, (o.rect.y + 1) as f32),
            Region::Chrome
        );
        // Just above / below it is page again, with page coordinates offset by the card.
        assert_eq!(l.region_at(mx, (o.rect.y - 1) as f32), Region::Content);
        assert_eq!(
            l.region_at(mx, (o.rect.y + o.rect.h) as f32),
            Region::Content
        );
        assert_eq!(l.to_content(mx, my), (mx - 12.0, my - 100.0));
        // The chrome bands are unchanged by the pill (it is composed separately).
        let plain = WindowLayout::with_extras(
            1024,
            728,
            1.0,
            CHROME,
            true,
            true,
            ext(1.0, MythOverlay::None),
        );
        assert_eq!(l.chrome_bands(), plain.chrome_bands());
    }

    #[test]
    fn approval_card_is_taller_and_scales() {
        let l = WindowLayout::with_extras(
            1280,
            820,
            1.0,
            CHROME,
            true,
            false,
            ext(1.0, MythOverlay::Card),
        );
        let o = l.overlay.unwrap();
        assert_eq!((o.rect.w, o.rect.h, o.radius), (440, 256, 28));
        assert_eq!(o.rect.y + o.rect.h, l.content.y + l.content.h - 28);
        let hi = WindowLayout::with_extras(
            2560,
            1640,
            2.0,
            CHROME,
            true,
            false,
            ext(1.0, MythOverlay::Card),
        );
        let o2 = hi.overlay.unwrap();
        assert_eq!((o2.rect.w, o2.rect.h, o2.radius), (880, 512, 56));
        assert_eq!(hi.gap, 24);
        // Narrow windows shrink the width, keeping side margins; tiny cards drop it.
        let narrow = WindowLayout::with_extras(
            400,
            600,
            1.0,
            CHROME,
            true,
            false,
            ext(1.0, MythOverlay::Card),
        );
        let o3 = narrow.overlay.unwrap();
        assert_eq!(o3.rect.w, narrow.content.w - 48);
        let tiny = WindowLayout::with_extras(
            400,
            200,
            1.0,
            CHROME,
            true,
            false,
            ext(1.0, MythOverlay::Card),
        );
        assert_eq!(tiny.overlay, None);
    }

    #[test]
    fn corner_cut_follows_the_arc() {
        let cuts: Vec<i32> = (0..12).map(|row| corner_cut(12, row)).collect();
        // Widest at the edge row, nothing by the last row; never increasing.
        assert!(cuts[0] >= 7);
        assert_eq!(cuts[11], 0);
        assert!(cuts.windows(2).all(|w| w[0] >= w[1]));
        assert_eq!(corner_cut(0, 0), 0);
        assert_eq!(corner_cut(12, 12), 0);
    }

    #[test]
    fn parses_css_colors() {
        assert_eq!(parse_css_color("#fff"), Some([255, 255, 255, 255]));
        assert_eq!(parse_css_color("#1E90FF"), Some([30, 144, 255, 255]));
        assert_eq!(parse_css_color("#11223380"), Some([17, 34, 51, 128]));
        assert_eq!(parse_css_color("#abcd"), Some([170, 187, 204, 221]));
        assert_eq!(parse_css_color("rgb(1, 2, 3)"), Some([1, 2, 3, 255]));
        assert_eq!(parse_css_color("rgba(1, 2, 3, 0.5)"), Some([1, 2, 3, 128]));
        assert_eq!(parse_css_color("rgb(1 2 3 / 50%)"), Some([1, 2, 3, 128]));
        assert_eq!(
            parse_css_color("rgb(100%, 0%, 50%)"),
            Some([255, 0, 128, 255])
        );
        assert_eq!(parse_css_color("transparent"), Some([0, 0, 0, 0]));
        assert_eq!(parse_css_color("rgba(0, 0, 0, 0)"), Some([0, 0, 0, 0]));
        assert_eq!(parse_css_color("red"), None);
        assert_eq!(parse_css_color("#12"), None);
        assert_eq!(parse_css_color("#gggggg"), None);
        assert_eq!(parse_css_color("rgb(1,2)"), None);
    }

    #[test]
    fn page_tint_from_script_result() {
        assert_eq!(page_tint("theme:#24292f"), Some([36, 41, 47]));
        assert_eq!(page_tint("bg:rgb(238, 238, 238)"), Some([238, 238, 238]));
        assert_eq!(page_tint("bg:rgba(0, 0, 0, 0)"), None);
        assert_eq!(page_tint("theme:not-a-colour"), None);
        assert_eq!(page_tint(""), None);
    }

    #[test]
    fn docked_assist_shrinks_the_card_and_takes_the_clicks() {
        let base = WindowLayout::new(1440, 900, 1.0, CHROME, true, false);
        let lay = |w: f32, width: u32| {
            WindowLayout::with_extras(
                width,
                900,
                1.0,
                CHROME,
                true,
                false,
                LayoutExtras {
                    assist: Some(AssistCss::Docked { w }),
                    ..LayoutExtras::default()
                },
            )
        };
        let l = lay(380.0, 1440);
        let a = l.assist.unwrap();
        assert_eq!(a.kind, MythOverlay::AssistDocked);
        assert_eq!(l.content.w, base.content.w - 380 - base.gap);
        assert_eq!(a.rect.x, l.content.x + l.content.w + l.gap);
        assert_eq!(
            (a.rect.y, a.rect.h, a.rect.w),
            (l.content.y, l.content.h, 380)
        );
        assert_eq!(
            a.rect.x + a.rect.w,
            1440 - l.gap,
            "same sky gap on the right"
        );
        let (cx, cy) = (a.rect.x as f32 + 100.0, a.rect.y as f32 + 200.0);
        assert_eq!(l.region_at(cx, cy), Region::Chrome);
        assert_eq!(base.region_at(cx, cy), Region::Content);
        // Width kept in range.
        assert_eq!(lay(10.0, 1440).assist.unwrap().rect.w, 320);
        assert_eq!(lay(9000.0, 1440).assist.unwrap().rect.w, 560);
        // Too narrow: floats over the card instead (the card keeps its width).
        let n = lay(380.0, 800);
        assert_eq!(n.assist.unwrap().kind, MythOverlay::Assist);
        assert_eq!(
            n.content.w,
            WindowLayout::new(800, 900, 1.0, CHROME, true, false)
                .content
                .w
        );
        // Internal pages: no panel.
        let flat = WindowLayout::with_extras(
            1440,
            900,
            1.0,
            CHROME,
            false,
            false,
            LayoutExtras {
                assist: Some(AssistCss::Docked { w: 380.0 }),
                ..LayoutExtras::default()
            },
        );
        assert!(flat.assist.is_none());
    }

    #[test]
    fn floating_assist_is_clamped_inside_the_card() {
        let lay = |r: Option<PopupCss>| {
            WindowLayout::with_extras(
                1440,
                900,
                2.0,
                CHROME,
                true,
                false,
                LayoutExtras {
                    assist: Some(AssistCss::Floating(r)),
                    ..LayoutExtras::default()
                },
            )
        };
        let d = lay(None);
        let a = d.assist.unwrap();
        assert_eq!(a.kind, MythOverlay::Assist);
        let inset = 20; // 10 CSS px at 2x
        assert_eq!(a.rect.x + a.rect.w, d.content.x + d.content.w - inset);
        assert_eq!(a.rect.y, d.content.y + inset);
        assert_eq!(
            d.content,
            WindowLayout::new(1440, 900, 2.0, CHROME, true, false).content
        );
        // Dragged off the card: pushed back inside; size kept in range.
        let a = lay(Some(PopupCss {
            x: -500.0,
            y: 5000.0,
            w: 100.0,
            h: 100.0,
            r: 0.0,
        }))
        .assist
        .unwrap();
        assert_eq!((a.rect.w, a.rect.h), (600, 560));
        assert_eq!(a.rect.x, d.content.x + inset);
        assert_eq!(a.rect.y + a.rect.h, d.content.y + d.content.h - inset);
        // NaN: the default spot.
        let n = lay(Some(PopupCss {
            x: f32::NAN,
            ..PopupCss::default()
        }))
        .assist
        .unwrap();
        assert_eq!(n.rect, d.assist.unwrap().rect);
    }

    #[test]
    fn selection_pill_stays_inside_the_card() {
        let lay = |p| {
            WindowLayout::with_extras(
                1000,
                800,
                1.0,
                CHROME,
                true,
                false,
                LayoutExtras {
                    selpill: Some(p),
                    ..LayoutExtras::default()
                },
            )
        };
        let l = lay(PopupCss {
            x: 900.0,
            y: 20.0,
            w: SELPILL_W_CSS,
            h: SELPILL_H_CSS,
            r: 0.0,
        });
        let p = l.selpill.unwrap();
        assert_eq!(p.kind, MythOverlay::SelPill);
        assert_eq!(p.rect.x + p.rect.w, l.content.x + l.content.w);
        assert_eq!(p.rect.y, l.content.y);
        assert_eq!(p.radius, p.rect.h / 2);
        assert_eq!(
            l.region_at(p.rect.x as f32 + 10.0, p.rect.y as f32 + 19.0),
            Region::Chrome
        );
        assert!(
            lay(PopupCss {
                x: f32::INFINITY,
                ..PopupCss::default()
            })
            .selpill
            .is_none()
        );
    }

    #[test]
    fn popup_is_clipped_scaled_and_routes_to_the_chrome() {
        let lay = |popup| {
            WindowLayout::with_extras(
                1000,
                800,
                2.0,
                CHROME,
                true,
                false,
                LayoutExtras {
                    popup,
                    ..LayoutExtras::default()
                },
            )
        };
        let p = lay(Some(PopupCss {
            x: 100.0,
            y: 60.0,
            w: 300.0,
            h: 200.0,
            r: 12.0,
        }))
        .popup
        .unwrap();
        assert_eq!(p.kind, MythOverlay::Popup);
        assert_eq!(p.rect, PxRect::new(200, 120, 600, 400));
        assert_eq!(p.radius, 24);
        let l = lay(Some(PopupCss {
            x: 100.0,
            y: 60.0,
            w: 300.0,
            h: 200.0,
            r: 12.0,
        }));
        assert_eq!(l.region_at(500.0, 300.0), Region::Chrome);
        assert_eq!(l.region_at(900.0, 700.0), Region::Content);
        // Clipped to the window; caps on bogus sizes; NaN / empty -> none.
        let big = lay(Some(PopupCss {
            x: 400.0,
            y: 300.0,
            w: 5000.0,
            h: 5000.0,
            r: 0.0,
        }))
        .popup
        .unwrap();
        assert_eq!(big.rect, PxRect::new(800, 600, 200, 200));
        assert!(
            lay(Some(PopupCss {
                x: f32::NAN,
                ..PopupCss::default()
            }))
            .popup
            .is_none()
        );
        assert!(lay(Some(PopupCss::default())).popup.is_none());
        assert!(lay(None).popup.is_none());
        // Internal pages (no card) get the popup too.
        let flat = WindowLayout::with_extras(
            1000,
            800,
            1.0,
            CHROME,
            false,
            false,
            LayoutExtras {
                popup: Some(PopupCss {
                    x: 10.0,
                    y: 10.0,
                    w: 50.0,
                    h: 50.0,
                    r: 4.0,
                }),
                ..LayoutExtras::default()
            },
        );
        assert!(flat.popup.is_some());
    }
}
