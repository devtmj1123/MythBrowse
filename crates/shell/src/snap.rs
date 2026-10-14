// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Windows 11 Snap Layouts on the title bar's own maximize button.
//!
//! The window is frameless and the chrome page draws the window controls, so Windows does not
//! know where "the maximize button" is. Windows 11 shows the Snap Layouts flyout when a
//! window answers `WM_NCHITTEST` with `HTMAXBUTTON` under the pointer. The shell subclasses
//! winit's window (`SetWindowSubclass`, so winit's own window procedure still runs for
//! everything else) and answers `HTMAXBUTTON` inside the rectangle the chrome reports for its
//! button (`/api/max_button`). Over that rectangle Windows then sends non-client mouse
//! messages instead of client ones, so the subclass also keeps our button's look: hover in /
//! out goes to the chrome (`mb.snapHover`), and a click (`WM_NCLBUTTONUP` on `HTMAXBUTTON`)
//! maximizes / restores through the shell, as the button's own click does.
//!
//! The hit test and the `LPARAM` decoding are plain functions, tested on every platform;
//! the subclass itself only builds on Windows.

use std::sync::Mutex;

/// The maximize button in client (window) physical px: x, y, w, h.
pub type ButtonRect = (i32, i32, i32, i32);

/// What the pointer did on the button (taken by the shell on its next pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapEvent {
    Hover(bool),
    #[cfg_attr(not(windows), allow(dead_code))] // sent by the Windows subclass
    Click,
}

struct State {
    rect: Option<ButtonRect>,
    hover: bool,
    events: Vec<SnapEvent>,
}

static STATE: Mutex<State> = Mutex::new(State {
    rect: None,
    hover: false,
    events: Vec::new(),
});

/// Where the chrome draws the maximize button now (`None`: hidden, e.g. fullscreen).
pub fn set_max_button(rect: Option<ButtonRect>) {
    if let Ok(mut s) = STATE.lock() {
        s.rect = rect.filter(|r| r.2 > 0 && r.3 > 0);
        if s.rect.is_none() && s.hover {
            s.hover = false;
            s.events.push(SnapEvent::Hover(false));
        }
    }
}

/// Events since the last call.
pub fn take_events() -> Vec<SnapEvent> {
    STATE
        .lock()
        .map(|mut s| std::mem::take(&mut s.events))
        .unwrap_or_default()
}

/// Whether client point (x, y) is on the button.
pub fn hit(rect: Option<ButtonRect>, x: i32, y: i32) -> bool {
    rect.is_some_and(|(rx, ry, w, h)| x >= rx && x < rx + w && y >= ry && y < ry + h)
}

/// `GET_X_LPARAM` / `GET_Y_LPARAM`: the signed 16-bit screen coordinates packed in an
/// `LPARAM` (negative on monitors left of / above the primary one).
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows subclass (and tests)
pub fn lparam_point(lparam: isize) -> (i32, i32) {
    let x = (lparam & 0xffff) as u16 as i16 as i32;
    let y = ((lparam >> 16) & 0xffff) as u16 as i16 as i32;
    (x, y)
}

#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows subclass (and tests)
fn on_button(x: i32, y: i32) -> bool {
    STATE.lock().map(|s| hit(s.rect, x, y)).unwrap_or(false)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn set_hover(on: bool) -> bool {
    let Ok(mut s) = STATE.lock() else {
        return false;
    };
    if s.hover == on {
        return false;
    }
    s.hover = on;
    s.events.push(SnapEvent::Hover(on));
    true
}

#[cfg_attr(not(windows), allow(dead_code))]
fn push(e: SnapEvent) {
    if let Ok(mut s) = STATE.lock() {
        s.events.push(e);
    }
}

#[cfg(windows)]
mod win {
    use std::sync::{Arc, OnceLock};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
    };
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HTMAXBUTTON, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
        WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    };

    use super::{SnapEvent, lparam_point, on_button, push, set_hover};

    const SUBCLASS_ID: usize = 0x4d42_534e; // "MBSN"

    static WAKE: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();

    fn wake() {
        if let Some(w) = WAKE.get() {
            w();
        }
    }

    unsafe extern "system" fn proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        match msg {
            WM_NCHITTEST => {
                let (x, y) = lparam_point(lparam);
                let mut p = POINT { x, y };
                // SAFETY: `hwnd` is the window this subclass is installed on; `p` is a valid
                // POINT on the stack.
                if unsafe { ScreenToClient(hwnd, &mut p) } != 0 && on_button(p.x, p.y) {
                    return HTMAXBUTTON as LRESULT;
                }
            }
            WM_NCMOUSEMOVE if wparam == HTMAXBUTTON as WPARAM => {
                if set_hover(true) {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: `t` is a fully initialised TRACKMOUSEEVENT for our window.
                    unsafe { TrackMouseEvent(&mut t) };
                    wake();
                }
                return 0;
            }
            WM_NCMOUSEMOVE | WM_NCMOUSELEAVE => {
                if set_hover(false) {
                    wake();
                }
            }
            // Pressed on our button: keep the default handler from running its own (classic)
            // caption-button tracking loop; the click happens on release.
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam == HTMAXBUTTON as WPARAM => {
                return 0;
            }
            WM_NCLBUTTONUP if wparam == HTMAXBUTTON as WPARAM => {
                push(SnapEvent::Click);
                wake();
                return 0;
            }
            _ => {}
        }
        // SAFETY: forwards the message unchanged to the next window procedure in the chain
        // (winit's), as subclass procedures must.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// Subclass the window `hwnd` (winit's HWND). Returns false if Windows refused.
    pub fn install(hwnd: isize, wake: Arc<dyn Fn() + Send + Sync>) -> bool {
        let _ = WAKE.set(wake);
        // SAFETY: `hwnd` is a live top-level window owned by this thread (winit created it
        // on the event-loop thread, where this runs); `proc` matches SUBCLASSPROC.
        unsafe { SetWindowSubclass(hwnd as HWND, Some(proc), SUBCLASS_ID, 0) != 0 }
    }
}

/// Install the Snap Layouts subclass on `window` (Windows only; elsewhere a no-op).
pub fn install(
    window: &winit::window::Window,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> bool {
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => win::install(h.hwnd.get(), wake),
            _ => false,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, wake);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_is_half_open() {
        let r = Some((100, 0, 46, 40));
        assert!(hit(r, 100, 0));
        assert!(hit(r, 145, 39));
        assert!(!hit(r, 146, 10));
        assert!(!hit(r, 120, 40));
        assert!(!hit(r, 99, 10));
        assert!(!hit(None, 120, 10));
    }

    #[test]
    fn lparam_is_signed_16_bit() {
        assert_eq!(lparam_point(((20 << 16) | 10) as isize), (10, 20));
        // x = -5 (0xfffb), y = -1 (0xffff): monitors left of / above the primary one.
        assert_eq!(lparam_point(0xffff_fffb_u32 as isize), (-5, -1));
        assert_eq!(lparam_point(((300 << 16) | 0x8000) as isize), (-32768, 300));
    }

    #[test]
    fn state_events_and_rect() {
        set_max_button(Some((10, 0, 40, 30)));
        assert!(on_button(20, 10));
        assert!(set_hover(true));
        assert!(!set_hover(true), "no duplicate hover events");
        push(SnapEvent::Click);
        // Hiding the button ends the hover.
        set_max_button(None);
        assert!(!on_button(20, 10));
        assert_eq!(
            take_events(),
            vec![
                SnapEvent::Hover(true),
                SnapEvent::Click,
                SnapEvent::Hover(false)
            ]
        );
        assert!(take_events().is_empty());
        set_max_button(Some((0, 0, 0, 10)));
        assert!(!on_button(0, 5), "empty rects never hit");
    }
}
