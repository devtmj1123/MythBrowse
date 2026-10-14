// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! `mythbrowse`: desktop shell.
//!
//! One winit window, one `WindowRenderingContext`, and two `OffscreenRenderingContext`s:
//! a window-sized one for the **browser chrome** (tab strip + toolbar on top of the sky,
//! HTML/CSS/JS from `ui/`, rendered by Servo in its own webview) and one for the **active
//! tab**. Web pages float on the sky as a rounded card with a thin ring of sky around them;
//! internal pages fill everything under the toolbar (see `mythbrowse_engine::frame`). Each
//! frame both are painted into their offscreen framebuffers and composed into the window
//! (`compose`: the same blit servoshell uses to place webviews, plus the rounded corners).
//! Pointer, wheel and IME positions are mapped through the same layout.
//!
//! The chrome is lightly tinted with the active web page's colour (its `theme-color`, else
//! its background), read once per page load (`frame::PAGE_TINT_SCRIPT`).
//!
//! The chrome and the internal pages (AI chat home, history & bookmarks) are served from
//! `https://mythbrowse.internal/`, intercepted in `load_web_resource`; their JavaScript calls
//! the shell through `fetch('/api/…')`, and the shell pushes state into the chrome with
//! `evaluate_javascript`. See `mythbrowse_engine::chrome` for the protocol and access rules.
//!
//! Background tabs go to sleep after `MYTHBROWSE_TAB_SLEEP_SECS` (default 600) of
//! inactivity: their webview is dropped, URL/title/scroll position are kept, and the page
//! is reloaded and scrolled back when the tab is selected again.
//!
//! Shortcuts: Ctrl+L / F6 / Alt+D address bar · Ctrl+T new tab · Ctrl+W close tab ·
//! Ctrl+Tab / Ctrl+Shift+Tab switch · Alt+Left / Alt+Right back / forward · F5 / Ctrl+R
//! reload · Ctrl+D bookmark · Ctrl+H history & bookmarks · Ctrl+Shift+Y downloads.
//!
//! Downloads, the file chooser for `<input type=file>` and session restore live in
//! `files_host` (Servo 0.1.4 has no download API; see `mythbrowse_files`).
//!
//! The window is frameless (`with_decorations(false)`): the chrome page draws the title bar
//! (tabs, window controls) and asks the shell to move / minimize / maximize / close the window
//! over RPC; the shell itself handles resizing from an invisible border around the window. On
//! Windows the native drop shadow and rounded corners are kept (winit's undecorated shadow +
//! DWM corner preference), and moving goes through the system move loop, so Aero Snap works.
//!
//! Usage: `mythbrowse [--console] [URL | text]` (text is sent to the AI home as a prompt).
//!
//! Release builds on Windows have no console window; `--console` (or `MYTHBROWSE_CONSOLE=1`)
//! attaches to / opens one for debug output. Otherwise logs go to
//! `<profile>\logs\mythbrowse.log` (see `diag`; `%LOCALAPPDATA%\MythBrowse Open Build` in the open build). The UI files are embedded in
//! the binary; `MYTHBROWSE_UI_DIR=<dir>` serves them from disk instead (UI development).
//! `MYTHBROWSE_THEME_HOUR=<0..24>` pins the time-of-day theme (screenshots, UI work).

// No console window for release builds on Windows (see `diag` for --console and logging).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// The AI host: the chat-only host (`ai_host.rs`) in the open-source build. Official JustMyth
// builds apply a private overlay that enables the `agent` feature and compiles the proprietary
// Myth agent host here instead, against the same interface (see ARCHITECTURE.md).
#[cfg(feature = "agent")]
compile_error!("the `agent` feature needs JustMyth's private overlay (official builds only)");
#[path = "ai_host.rs"]
mod agent_host;
mod assist_host;
mod browse_host;
mod compat_host;
mod compose;
mod cookies_host;
mod diag;
mod files_host;
mod glass;
mod keys;
mod password_host;
mod perf_host;
mod settings_host;
mod snap;
mod sync_host;
mod ui_host;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use http::{HeaderMap, StatusCode};
use log::{info, warn};
use mythbrowse_ai::Secret;
use mythbrowse_blocker::Blocklist;
use mythbrowse_compat::ua::{Os as UaOs, UaMode};
use mythbrowse_engine::autoscroll::{Autoscroll, Release};
use mythbrowse_engine::chrome::{self, APP_NAME, ApiCall, ChromeState, Edge, Route};
use mythbrowse_engine::euclid::Scale;
use mythbrowse_engine::euclid::default::{Point2D, Rect, Size2D};
use mythbrowse_engine::frame::{self, LayoutExtras, MythOverlay, PxRect, Region, WindowLayout};
use mythbrowse_engine::glass::{self as glass_api, Appearance, GlassLook, GlassMode, GlassRectCss};
use mythbrowse_engine::glow::{self, Glow, GlowState, Motion};
use mythbrowse_engine::load_watch::{self, LoadWatch, Look};
use mythbrowse_engine::omnibox::{self, OmniboxTarget};
use mythbrowse_engine::page_input::{self, OpenGate, OpenRequest, WheelAnswer, WheelReport};
use mythbrowse_engine::page_ui;
use mythbrowse_engine::perf::{FrameSample, ScrollPath};
use mythbrowse_engine::servo::{
    AllowOrDenyRequest, Code, ConsoleLogLevel, CreateNewWebViewRequest, Cursor, DevicePoint,
    DeviceVector2D, EditingActionEvent, EmbedderControl, EmbedderControlId, InputEvent, JSValue,
    KeyState, KeyboardEvent, LoadStatus, Location, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, NavigationRequest, OffscreenRenderingContext,
    PermissionRequest, RenderingContext, Scroll, UserScript, WebResourceLoad, WebResourceResponse,
    WebView, WebViewDelegate, WebViewId, WebViewPoint, WebViewVector, WheelDelta, WheelEvent,
    WheelMode, WindowRenderingContext,
};
use mythbrowse_engine::smooth_scroll::{self, SmoothScroller, WheelInput};
use mythbrowse_engine::tabs::{ScrollPos, SleepPolicy, TabId, TabManager};
use mythbrowse_engine::{CallbackWaker, Engine};
use mythbrowse_storage::Store;
use serde_json::json;
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey as WNamedKey};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

const DEFAULT_BLOCKLIST: &str = include_str!("../resources/default-blocklist.txt");
/// How often to look for tabs to put to sleep (at most).
const SLEEP_CHECK_EVERY: Duration = Duration::from_secs(30);
/// Profile pref: "1" once the first-run welcome (ui/onboarding.js) was finished or skipped.
const PREF_ONBOARDED: &str = "onboarded";
/// Window and taskbar icons (packaging/icons/make_icons.py). The .exe's own icon is a
/// resource (build.rs); these cover the title bar / Alt+Tab / taskbar of the running window
/// and Linux.
const ICON_32: &[u8] = include_bytes!("../resources/icon-32.png");
#[cfg_attr(not(windows), allow(dead_code))] // the taskbar icon (Windows only)
const ICON_256: &[u8] = include_bytes!("../resources/icon-256.png");
/// Idle sky frame spacing on a software rasterizer (WARP, llvmpipe): every sky frame is a
/// full-window shader pass on the CPU there. 5 fps instead of 15: the slow clouds move so
/// little that consecutive frames 3 steps apart differ by at most 3/255 per channel
/// (measured), and the CPU is free for the page.
const SKY_FRAME_MS_SOFTWARE: u64 = 200;

/// Private session (`--private` or `MYTHBROWSE_PRIVATE=1`): no page or chat history is saved.
static PRIVATE: AtomicBool = AtomicBool::new(false);

/// Raises the open-file soft limit to the hard limit (capped), as Firefox and Chromium do.
/// Servo uses a file descriptor per IPC channel and an ipc channel per request, so a page with
/// a few hundred subresources and iframes hit the common 1024 default: "Too many open files"
/// panics in the script and network threads (calendly.com in the top-1000 run).
#[cfg(unix)]
fn raise_fd_limit() {
    // macOS rejects values above OPEN_MAX even when the hard limit is "unlimited".
    const CAP: libc::rlim_t = if cfg!(target_os = "macos") {
        10240
    } else {
        65536
    };
    // SAFETY: getrlimit/setrlimit only read/write the struct we pass.
    unsafe {
        let mut lim = std::mem::zeroed::<libc::rlimit>();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        let want = lim.rlim_max.min(CAP);
        if lim.rlim_cur < want {
            lim.rlim_cur = want;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    raise_fd_limit();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let flags = diag::take_flags(&mut args);
    diag::init(&flags);
    // Unpackaged Windows builds: our AppUserModelID, so toasts show under MythBrowse.
    mythbrowse_notify::init();
    let private_flag = match args.iter().position(|a| a == "--private") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    if private_flag || std::env::var("MYTHBROWSE_PRIVATE").is_ok_and(|v| v == "1") {
        PRIVATE.store(true, AtomicOrdering::Relaxed);
    }
    if let Some(i) = args.iter().position(|a| a == "--keystore-selftest") {
        args.remove(i);
        // CI / support: check the platform key store (DPAPI on Windows) and exit.
        let result = agent_host::keystore_selftest();
        let line = match &result {
            Ok(backend) => format!("keystore self-test: OK ({backend})"),
            Err(e) => format!("keystore self-test: FAILED: {e}"),
        };
        println!("{line}");
        diag::write_line(&line);
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    diag::check_graphics_runtime();

    // Servo's networking uses rustls; the embedder must pick the crypto provider
    // (same as servo's examples/winit_minimal.rs).
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let arg = args.join(" ");
    let start_url = omnibox::resolve(&arg)
        .map(|target| target_url(&target))
        .unwrap_or_else(|| chrome::home_url(None));

    let event_loop = EventLoop::<WakeUp>::with_user_event()
        .build()
        .unwrap_or_else(|e| diag::fatal(&format!("could not start the window system: {e}")));
    let mut app = App::Initial {
        proxy: event_loop.create_proxy(),
        start_url,
        explicit: !arg.trim().is_empty(),
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        diag::fatal(&format!("event loop failed: {e}"));
    }
    diag::write_line("exited normally");
    Ok(())
}

/// Where an address-bar entry goes: prompts open the AI home, everything else its URL.
fn target_url(target: &OmniboxTarget) -> Url {
    match target {
        OmniboxTarget::Prompt(p) => chrome::home_url(Some(p)),
        other => other.web_url().unwrap_or_else(|| chrome::home_url(None)),
    }
}

/// UI files on disk that override the copies embedded in the binary, so the UI can be edited
/// without rebuilding: `MYTHBROWSE_UI_DIR` if set, else (debug builds only) the source tree.
/// Release builds serve only the embedded copies, so a shipped build is a single exe.
fn find_ui_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("MYTHBROWSE_UI_DIR").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(dir));
    }
    if cfg!(debug_assertions) {
        candidates.push(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("ui"),
        );
    }
    candidates
        .into_iter()
        .find(|d| d.join("chrome.html").is_file())
        .and_then(|d| d.canonicalize().ok())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn sleep_policy_from_env() -> SleepPolicy {
    let mut policy = SleepPolicy::default();
    if let Some(secs) = env_u64("MYTHBROWSE_TAB_SLEEP_SECS") {
        policy.idle_ms = secs.saturating_mul(1000);
    }
    if let Some(max) = env_u64("MYTHBROWSE_MAX_AWAKE_TABS") {
        policy.max_awake = Some(max.max(1) as usize);
    }
    policy
}

/// User event sent by Servo's waker to make winit call `user_event`.
#[derive(Debug, Clone, Copy)]
struct WakeUp;

enum App {
    Initial {
        proxy: winit::event_loop::EventLoopProxy<WakeUp>,
        start_url: Url,
        /// A URL / prompt was given on the command line (opened after any restored tabs).
        explicit: bool,
    },
    Running(Rc<AppState>),
    Exiting,
}

/// How a wheel gesture reaches the page (v0.4.2, see `State::scroll_path`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum GesturePath {
    Fast,
    Dom,
    Asking { since_ms: f64, id: u64 },
}

/// A pause longer than this between wheel inputs starts a new gesture (path chosen again).
const GESTURE_GAP_MS: f64 = 250.0;
/// How long a gesture waits for the page's answer about blocking wheel listeners before it
/// takes the compositor path anyway (a busy script thread must not stall scrolling).
const WHEEL_ASK_MS: f64 = 50.0;
/// A page's autoscroll request counts only this soon after a real middle press there.
const AUTOSCROLL_GATE_MS: f64 = 1000.0;

/// Which part of the window has keyboard focus / is under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    Chrome,
    Content,
    /// The sky ring around the page card (and its rounded-off corners): no webview, inert.
    Frame,
}

struct AppState {
    window: Window,
    engine: Engine,
    window_ctx: Rc<WindowRenderingContext>,
    chrome_ctx: Rc<OffscreenRenderingContext>,
    content_ctx: Rc<OffscreenRenderingContext>,
    /// Shared by every webview; holds only a weak link back to us (no Rc cycle).
    delegate: Rc<dyn WebViewDelegate>,
    weak_self: Weak<AppState>,
    chrome: RefCell<Option<WebView>>,
    tabs: RefCell<TabManager<WebView>>,
    focus: Cell<Area>,
    hover: Cell<Area>,
    /// Area that received the last mouse-down, so drags and the release stay there.
    pressed: Cell<Option<Area>>,
    cursor: Cell<DevicePoint>,
    modifiers: Cell<ModifiersState>,
    /// Per-session secret required on every UI RPC.
    token: String,
    ui_dir: Option<PathBuf>,
    store: Store,
    /// AI conversations, the agent's background tabs and model settings (`agent_host`).
    agent: agent_host::AgentHost,
    blocker: Blocklist,
    blocked_requests: Cell<u64>,
    /// Site compat: shared user scripts, detector, per-site rules, AI repair (`compat_host`).
    compat: compat_host::CompatHost,
    /// Account + E2E sync (Appwrite), on its own thread (`sync_host`).
    sync: sync_host::SyncHost,
    /// Downloads, file chooser, session restore (`files_host`).
    files: files_host::FilesHost,
    /// Password manager + browser import (`password_host`).
    passwords: password_host::PasswordHost,
    clock: Instant,
    sleep_policy: SleepPolicy,
    next_sleep_check: Cell<Instant>,
    chrome_dirty: Cell<bool>,
    /// Resize edge under the pointer (frameless window), if any.
    edge: Cell<Option<Edge>>,
    /// The OS took over the pointer for a window move: ignore the next button release.
    swallow_release: Cell<bool>,
    /// The chrome's close button was pressed; exit at the next event-loop pass.
    exit_requested: Cell<bool>,
    maximized: Cell<bool>,
    /// Where the chrome strip and the page are in the window (physical px).
    layout: Cell<WindowLayout>,
    /// Rounds the page card's corners (GL; built on first use).
    corners: RefCell<compose::Corners>,
    /// `MYTHBROWSE_THEME_HOUR`: pinned time-of-day theme for the UI pages.
    theme_hour: Option<f64>,
    /// Light ring around the card (state machine; see `mythbrowse_engine::glow`).
    glow: RefCell<Glow>,
    /// Its shader (GL; built on first use).
    glow_gl: RefCell<compose::Glow>,
    /// When the last ring frame was drawn (frames are spaced by the 30 fps budget).
    glow_last: Cell<Instant>,
    /// The next redraw must let Servo paint (a webview has a new frame, layout changed...).
    /// Redraws only for the ring re-compose the existing frames and skip Servo.
    servo_paint: Cell<bool>,
    /// The same for the active page alone (a page frame does not repaint the chrome).
    content_paint: Cell<bool>,
    /// Counts the web page's frames: the glass re-blurs on a page frame only where its
    /// surfaces read the page (`glass::Frame::page_rect`), never for one beside it.
    page_gen: Cell<u64>,
    /// Wheel / touchpad input, coalesced per frame and smoothed (`smooth_scroll`); sent to
    /// the view and point it started on.
    scroller: RefCell<SmoothScroller>,
    scroll_target: Cell<Option<(Area, DevicePoint)>>,
    /// v0.4.2: how the current wheel gesture reaches the page. `Fast`: Servo's compositor
    /// scroll (`WebView::notify_scroll_event`, no script round trip), one step per presented
    /// frame; `Dom`: `WheelEvent`s through the page's script thread (pages with a blocking
    /// wheel listener under the pointer); `Asking`: waiting (briefly) for the page to say.
    scroll_path: Cell<GesturePath>,
    /// When the last wheel input arrived (ms on `clock`): a pause starts a new gesture.
    wheel_last_ms: Cell<f64>,
    /// Pages that reported a blocking wheel listener: (top document, a frame), by webview.
    wheel_listeners: RefCell<std::collections::HashMap<WebViewId, (bool, bool)>>,
    /// The page's answer to the gesture's query (query id, answer), set by the JS callback.
    wheel_answer: Rc<Cell<Option<(u64, WheelAnswer)>>>,
    wheel_query: Cell<u64>,
    /// Middle-click autoscroll (`engine::autoscroll`), driven once per presented frame.
    autoscroll: RefCell<Option<Autoscroll>>,
    /// The last middle press on a page (ms on `clock`, window point): autoscroll requests
    /// from the page script are honoured only right after one.
    middle_press: Cell<Option<(f64, DevicePoint)>>,
    middle_down: Cell<bool>,
    /// The click that ended autoscroll: its release is not passed on either.
    autoscroll_swallow: Cell<bool>,
    /// Perf HUD / trace (`MB_PERF=1`, Ctrl+Shift+F12).
    perf: perf_host::Perf,
    /// Tabs whose load has not completed: settle them when the document is parsed and quiet
    /// but never fires `load` (`engine::load_watch`). Looks arrive in `load_looks`.
    load_watch: RefCell<std::collections::HashMap<TabId, LoadWatch>>,
    load_looks: Rc<RefCell<Vec<(TabId, String)>>>,
    load_watch_next: Cell<Instant>,
    /// Ctrl+click / middle click on a link: only right after a real press (`page_input`).
    open_gate: Cell<OpenGate>,
    /// Motion setting (Settings: full / reduced / off) and the OS "reduce animations".
    /// `None`: follow the OS animation setting (the default).
    motion_setting: Cell<Option<Motion>>,
    os_reduced_motion: bool,
    prefs_path: Option<PathBuf>,
    /// Settings `ua_mode` and the user script that keeps `navigator` in step with it.
    ua_mode: Cell<UaMode>,
    ua_script: RefCell<Rc<UserScript>>,
    /// Settings > Appearance as saved (profile `prefs` table), the OS "Transparency effects"
    /// (off = reduce), and the live preview while the settings sheet's slider moves
    /// (transparency, reduce; `None` = show the saved setting).
    appearance: Cell<Appearance>,
    os_reduced_transparency: bool,
    appearance_preview: Cell<Option<(u8, bool)>>,
    /// The GL renderer (logged at start-up; Settings > About). On a software rasterizer
    /// the idle sky steps less often (see `sky_due`).
    gpu: GpuInfo,
    /// Zoom, find in page, external links, self-closing pop-ups, fullscreen (`browse_host`).
    browse: browse_host::BrowseHost,
    /// Myth pill last pushed to the chrome (pushed again only when it changes).
    last_bar: RefCell<Option<serde_json::Value>>,
    /// Per-tab Myth states last pushed to the chrome (tab status dots).
    last_dots: RefCell<Vec<(u64, &'static str)>>,
    /// Myth's surface over the card (pill / approval card), from `last_bar`.
    overlay: Cell<MythOverlay>,
    /// Content sizes the chrome measured (CSS px): the pill's width, the approval card's
    /// height. The overlay is sized to them (`LayoutExtras::overlay_size`).
    pill_w: Cell<Option<f32>>,
    card_h: Cell<Option<f32>>,
    /// The floating Tasks panel is open (toolbar Tasks button, Ctrl+J), and its measured
    /// content height.
    tasks_panel: Cell<bool>,
    tasks_panel_h: Cell<Option<f32>>,
    /// The sky gap easing to / from the wider AI gap: (from, to, since), see `ai_ease`.
    ai_gap: Cell<(f32, f32, Instant)>,
    /// Myth's overlay shader (GL; built on first use).
    overlay_gl: RefCell<compose::Overlay>,
    /// Liquid glass (`glass`): the GL compositor, built before Servo starts; `None` when
    /// glass is off.
    glass_gl: RefCell<Option<glass::Compositor>>,
    glass_mode: Cell<GlassMode>,
    /// Glass surfaces reported by the chrome and by the active internal page (CSS px of
    /// each webview).
    glass_chrome: RefCell<Vec<GlassRectCss>>,
    glass_page: RefCell<Vec<GlassRectCss>>,
    /// Backdrop generation (sky, page, layout): the blur is reused while it stays the same.
    glass_gen: Cell<u64>,
    /// Checking that the chrome really came out transparent (once, after its first report).
    glass_probe: Cell<GlassProbe>,
    /// Hour of day the chrome uses for its theme (the sky palette follows it).
    sky_hour: Cell<f32>,
    /// Sky clock (seconds; advances only while the sky animates) and its last frame.
    sky_t: Cell<f32>,
    sky_last: Cell<Instant>,
    /// When the UI last said a surface was moving (the sky waits for it).
    ui_moving_at: Cell<Option<Instant>>,
    /// The window is fully covered or minimized (no sky animation).
    occluded: Cell<bool>,
    /// Omnibox suggestions, context menus, page dialogs / permissions, pop-up blocking, IME
    /// pre-edit, chrome popups (`ui_host`).
    ui: ui_host::UiHost,
    /// The Myth panel over web pages and the selection pill (`assist_host`).
    assist: assist_host::AssistHost,
}

/// Transparency check of the chrome webview in glass mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlassProbe {
    /// Waiting for the chrome's first glass report (it has switched its own sky off).
    Waiting,
    /// Read the chrome's pixels on the next frame.
    Due,
    Done,
}

/// How long the sky gap takes to widen / narrow when Myth starts / stops.
const AI_GAP_EASE: Duration = Duration::from_millis(220);

impl ApplicationHandler<WakeUp> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let App::Initial {
            proxy,
            start_url,
            explicit,
        } = self
        else {
            return;
        };

        let display_handle = event_loop
            .display_handle()
            .unwrap_or_else(|e| diag::fatal(&format!("no display handle: {e}")));
        let window = event_loop
            .create_window(window_attributes())
            .unwrap_or_else(|e| diag::fatal(&format!("could not create the window: {e}")));
        window.set_ime_allowed(true);
        let window_handle = window
            .window_handle()
            .unwrap_or_else(|e| diag::fatal(&format!("no window handle: {e}")));
        let size = window.inner_size();
        let window_ctx = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, size).unwrap_or_else(|e| {
                diag::fatal(&format!(
                    "could not create an OpenGL ES rendering context for the window \
                         ({e:?}). Updating the graphics driver usually fixes this."
                ))
            }),
        );
        diag::write_line(&format!(
            "window {}x{} @ scale {}; rendering context ready",
            size.width,
            size.height,
            window.scale_factor()
        ));
        let _ = window_ctx.make_current();
        // Which GPU (or software rasterizer) renders everything: one line in the log, so a
        // "slow" report can be told apart from a WARP / llvmpipe fallback at a glance.
        let renderer = gpu_info(&window_ctx.glow_gl_api());
        diag::write_line(&format!("renderer: {}", renderer.summary()));
        if renderer.software {
            warn!(
                "renderer: SOFTWARE rasterizer ({}); every frame is drawn by the CPU. \
                 Update or enable the graphics driver.",
                renderer.renderer
            );
        }
        // No tab yet: start with the edge-to-edge layout; `show_active` switches to the card.
        let layout = layout_for(size, window.scale_factor(), false, window.is_maximized());
        let (content_w, content_h) = layout.content_size();
        let chrome_ctx = Rc::new(window_ctx.offscreen_context(size_or_one(size)));
        let content_ctx =
            Rc::new(window_ctx.offscreen_context(PhysicalSize::new(content_w, content_h)));

        // Liquid glass needs every GL piece and the offscreen colour textures; decided before
        // Servo starts, because it sets how Servo clears the webviews (transparent or white).
        let glass_gl = build_glass(&window_ctx, &chrome_ctx, &content_ctx);
        let glass_mode = if glass_gl.is_some() {
            GlassMode::Blur
        } else {
            GlassMode::Off
        };
        diag::write_line(&format!("liquid glass: {}", glass_mode.as_str()));

        let agent_ctx = Rc::new(window_ctx.offscreen_context(PhysicalSize::new(
            agent_host::AGENT_VIEWPORT.0,
            agent_host::AGENT_VIEWPORT.1,
        )));
        let agent_proxy = proxy.clone();
        let agent_wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = agent_proxy.send_event(WakeUp);
        });
        let compat_proxy = proxy.clone();
        let compat_wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = compat_proxy.send_event(WakeUp);
        });
        let ui_proxy = proxy.clone();
        let ui_wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = ui_proxy.send_event(WakeUp);
        });
        let sync_proxy = proxy.clone();
        let sync_wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = sync_proxy.send_event(WakeUp);
        });
        // The user agent (Settings `ua_mode`, `<profile>/ui.json`; default a plain Firefox
        // desktop UA, see `mythbrowse_compat::ua`). Servo takes it at start-up.
        let ui_prefs_path = mythbrowse_storage::default_profile_dir().map(|d| d.join("ui.json"));
        let ua_mode = ui_prefs_path
            .as_deref()
            .and_then(|p| load_ui_pref(p, "ua_mode"))
            .and_then(|m| UaMode::parse(&m))
            .unwrap_or_default();
        let user_agent = ua_mode.user_agent(UaOs::current());
        info!("user agent ({}): {user_agent}", ua_mode.as_str());
        // Settings > Cookies "Stay signed in" (`persist_cookies`, default off; never in a
        // private session): Servo keeps its cookie jar (encrypted) and site storage in the
        // profile. Read here because Servo takes it at start-up.
        let engine_data = cookies_host::persistent_engine_dir(
            mythbrowse_storage::default_profile_dir().as_deref(),
            cookies_host::persist_pref(ui_prefs_path.as_deref()),
            PRIVATE.load(AtomicOrdering::Relaxed),
        );
        let waker_proxy = proxy.clone();
        let engine = Engine::new(
            Box::new(CallbackWaker::new(move || {
                if let Err(error) = waker_proxy.send_event(WakeUp) {
                    warn!("failed to wake event loop: {error:?}");
                }
            })),
            glass_mode.shell_sky(),
            &user_agent,
            engine_data,
        );

        let compat = compat_host::CompatHost::new(
            engine.new_user_content(),
            mythbrowse_storage::default_profile_dir().as_deref(),
            compat_wake,
        );
        let pw_proxy = proxy.clone();
        let pw_wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = pw_proxy.send_event(WakeUp);
        });
        // Same shared user-content manager as compat: every web page, never the chrome.
        let passwords = password_host::PasswordHost::new(
            &compat.user_content,
            mythbrowse_storage::default_profile_dir().as_deref(),
            pw_wake,
        );
        let (blocker, stats) = Blocklist::parse(DEFAULT_BLOCKLIST);
        info!(
            "content blocker: {} rules ({} skipped)",
            stats.blocked, stats.skipped
        );

        let store = mythbrowse_storage::default_profile_dir()
            .ok_or_else(|| "no profile directory".to_owned())
            .and_then(|dir| Store::open_in_dir(&dir).map_err(|e| format!("{}: {e}", dir.display())))
            .unwrap_or_else(|error| {
                warn!(
                    "profile storage unavailable ({error}); history and bookmarks will not persist"
                );
                Store::open_in_memory().expect("in-memory SQLite")
            });
        if let Err(e) = store.prune_history(mythbrowse_storage::DEFAULT_HISTORY_LIMIT) {
            warn!("history prune failed: {e}");
        }
        // Chat retention (forever / 30 / 90 days; pinned chats are kept).
        match store.prune_chats(mythbrowse_storage::unix_now_ms()) {
            Ok(n) if n > 0 => info!("chat history: {n} chats past the retention period removed"),
            Ok(_) => {}
            Err(e) => warn!("chat prune failed: {e}"),
        }
        let private = PRIVATE.load(AtomicOrdering::Relaxed);
        if private {
            info!("private session: no page or chat history is saved");
        }

        let files_proxy = proxy.clone();
        let files_wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = files_proxy.send_event(WakeUp);
        });
        let files = files_host::FilesHost::new(
            &store,
            mythbrowse_storage::default_profile_dir().as_deref(),
            private,
            files_wake,
        );
        info!("downloads go to {}", files.manager.dir().display());
        // `<a download>` clicks reach the shell on the console channel (see files_host).
        compat
            .user_content
            .add_script(files_host::FilesHost::link_script());
        // `navigator.appVersion` / `platform` agreeing with the UA (every web page).
        let ua_script = Rc::new(UserScript::new(
            mythbrowse_compat::ua::navigator_script(&user_agent),
            None,
        ));
        compat.user_content.add_script(ua_script.clone());
        // Page text selection, copy, and Ctrl / middle-click on links (see `page_input`).
        compat.user_content.add_script(Rc::new(UserScript::new(
            page_input::PAGE_INPUT_SCRIPT.to_owned(),
            None,
        )));

        let ui_dir = find_ui_dir();
        info!(
            "UI files: {}",
            ui_dir
                .as_ref()
                .map_or("embedded".to_owned(), |d| d.display().to_string())
        );
        let sleep_policy = sleep_policy_from_env();
        info!("tab sleeping: {sleep_policy:?}");
        let theme_hour = std::env::var("MYTHBROWSE_THEME_HOUR")
            .ok()
            .and_then(|v| chrome::parse_theme_hour(&v));
        if let Some(hour) = theme_hour {
            info!("theme pinned to hour {hour}");
        }

        // Appearance: read before the first page is served, so every page starts with the
        // right glass (the shell fills it into the HTML; no flash of the default look).
        let appearance = Appearance {
            transparency: store
                .transparency()
                .ok()
                .flatten()
                .unwrap_or(glass_api::TRANSPARENCY_DEFAULT),
            reduce: store.reduce_transparency().ok().flatten(),
        };
        let os_reduced_transparency = os_reduced_transparency();
        info!(
            "appearance: transparency {}, reduce transparency {} (system: {os_reduced_transparency})",
            appearance.transparency,
            appearance.reduce_str()
        );

        let suggest_web = store.search_suggestions().unwrap_or(true);
        info!(
            "address bar web suggestions: {}",
            if suggest_web { "on" } else { "off" }
        );
        let prefs_path = ui_prefs_path;
        let assist_mode = prefs_path
            .as_deref()
            .and_then(|p| load_ui_pref(p, "assist_mode"));
        let assist_dock_w = prefs_path
            .as_deref()
            .and_then(|p| load_ui_pref(p, "assist_dock_w"))
            .and_then(|w| w.parse::<f32>().ok());
        let motion_setting = prefs_path.as_deref().and_then(load_motion);
        let os_reduced_motion = os_reduced_motion();
        info!(
            "motion: {} (system reduce-animations: {os_reduced_motion})",
            motion_setting.map_or("system", Motion::as_str)
        );

        let state = Rc::new_cyclic(|weak: &Weak<AppState>| AppState {
            window,
            engine,
            window_ctx,
            chrome_ctx,
            content_ctx,
            delegate: Rc::new(Delegate(weak.clone())),
            weak_self: weak.clone(),
            chrome: RefCell::new(None),
            tabs: RefCell::new(TabManager::new()),
            focus: Cell::new(Area::Content),
            hover: Cell::new(Area::Content),
            pressed: Cell::new(None),
            cursor: Cell::new(DevicePoint::new(0.0, 0.0)),
            modifiers: Cell::new(ModifiersState::empty()),
            token: chrome::new_token(),
            ui_dir,
            store,
            agent: agent_host::AgentHost::new(
                agent_wake,
                agent_ctx,
                mythbrowse_storage::default_profile_dir(),
                private,
            ),
            blocker,
            blocked_requests: Cell::new(0),
            compat,
            sync: sync_host::SyncHost::new(mythbrowse_storage::default_profile_dir(), sync_wake),
            files,
            passwords,
            clock: Instant::now(),
            sleep_policy,
            next_sleep_check: Cell::new(Instant::now() + SLEEP_CHECK_EVERY),
            chrome_dirty: Cell::new(true),
            edge: Cell::new(None),
            swallow_release: Cell::new(false),
            exit_requested: Cell::new(false),
            maximized: Cell::new(false),
            layout: Cell::new(layout),
            corners: RefCell::new(compose::Corners::Untried),
            theme_hour,
            glow: RefCell::new(Glow::new(Instant::now())),
            glow_gl: RefCell::new(compose::Glow::Untried),
            glow_last: Cell::new(Instant::now()),
            servo_paint: Cell::new(true),
            content_paint: Cell::new(true),
            page_gen: Cell::new(0),
            scroller: RefCell::new(SmoothScroller::default()),
            scroll_target: Cell::new(None),
            scroll_path: Cell::new(GesturePath::Fast),
            wheel_last_ms: Cell::new(f64::NEG_INFINITY),
            wheel_listeners: RefCell::new(std::collections::HashMap::new()),
            wheel_answer: Rc::new(Cell::new(None)),
            wheel_query: Cell::new(0),
            autoscroll: RefCell::new(None),
            middle_press: Cell::new(None),
            middle_down: Cell::new(false),
            autoscroll_swallow: Cell::new(false),
            perf: perf_host::Perf::new(Instant::now()),
            load_watch: RefCell::new(std::collections::HashMap::new()),
            load_looks: Rc::new(RefCell::new(Vec::new())),
            load_watch_next: Cell::new(Instant::now()),
            open_gate: Cell::new(OpenGate::default()),
            motion_setting: Cell::new(motion_setting),
            os_reduced_motion,
            prefs_path,
            ua_mode: Cell::new(ua_mode),
            ua_script: RefCell::new(ua_script),
            appearance: Cell::new(appearance),
            os_reduced_transparency,
            appearance_preview: Cell::new(None),
            gpu: renderer.clone(),
            browse: browse_host::BrowseHost::default(),
            last_bar: RefCell::new(None),
            last_dots: RefCell::new(Vec::new()),
            overlay: Cell::new(MythOverlay::None),
            pill_w: Cell::new(None),
            card_h: Cell::new(None),
            tasks_panel: Cell::new(false),
            tasks_panel_h: Cell::new(None),
            ai_gap: Cell::new((0.0, 0.0, Instant::now())),
            overlay_gl: RefCell::new(compose::Overlay::Untried),
            glass_gl: RefCell::new(glass_gl),
            glass_mode: Cell::new(glass_mode),
            glass_chrome: RefCell::new(Vec::new()),
            glass_page: RefCell::new(Vec::new()),
            glass_gen: Cell::new(0),
            glass_probe: Cell::new(GlassProbe::Waiting),
            sky_hour: Cell::new(theme_hour.unwrap_or(12.0) as f32),
            // sky.js's clock is the time of day (UTC ms): start there, so the clouds sit where
            // the prototype's would.
            sky_t: Cell::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| (d.as_secs() % 86_400) as f32)
                    .unwrap_or(0.0),
            ),
            sky_last: Cell::new(Instant::now()),
            ui_moving_at: Cell::new(None),
            occluded: Cell::new(false),
            ui: ui_host::UiHost::new(ui_wake.clone(), suggest_web),
            assist: assist_host::AssistHost::new(
                mythbrowse_storage::default_profile_dir().as_deref(),
                PRIVATE.load(AtomicOrdering::Relaxed),
                assist_mode.as_deref(),
                assist_dock_w,
            ),
        });
        if snap::install(&state.window, ui_wake) {
            info!("snap layouts: maximize button hit test installed");
        }
        state.agent.motion.set(state.motion());
        state.maximized.set(state.window.is_maximized());

        let chrome_view = state.engine.new_webview(
            state.chrome_ctx.clone(),
            chrome::chrome_url(),
            state.window.scale_factor() as f32,
            state.delegate.clone(),
        );
        chrome_view.show();
        *state.chrome.borrow_mut() = Some(chrome_view);

        // Session restore (never in a private window; always after a crash; otherwise the
        // "On startup" setting). A URL from the command line opens after the restored tabs.
        let restored = state.files_restore();
        if !restored || *explicit {
            state.open_tab(start_url.clone());
        }
        // Settings (settings_host.rs): search engine, downloads folder / Save as, daily update check.
        state.settings_init();
        // CI: the manual top-sites compat run (no-op unless MYTHBROWSE_COMPAT_RUN is set).
        state.compat_run_start();
        if state.perf.on() {
            state.perf.start(&state.gpu.summary());
        }
        *self = App::Running(state);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: WakeUp) {
        if let App::Running(state) = self {
            state.engine.spin();
            state.agent_pump();
            state.compat_pump();
            state.files_pump();
            state.pw_pump();
            state.ui_pump();
            state.snap_pump();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let App::Running(state) = self else {
            return;
        };
        if state.exit_requested.get() {
            state.shutdown();
            *self = App::Exiting;
            event_loop.exit();
            return;
        }
        state.agent_pump();
        state.compat_pump();
        state.files_pump();
        state.pw_pump();
        state.ui_pump();
        state.update_glow();
        state.flush_chrome();
        let mut next = state.maybe_sleep_tabs();
        // The saved session (about 2 s after the last tab change).
        if let Some(due) = state.files_session_tick() {
            next = next.min(due);
        }
        // The sky gap easing in / out (a few layout steps at 30 fps, then nothing).
        if let Some(due) = state.ai_gap_tick() {
            next = next.min(due);
        }
        // The top-sites compat run (CI only) wakes up for its own deadlines.
        if let Some(due) = state.compat_run_tick() {
            next = next.min(due);
        }
        // The shell's sky (glass mode): about 15 fps with full motion, else once a minute.
        if let Some(due) = state.sky_due() {
            if due <= Instant::now() {
                state.sky_step();
            } else {
                next = next.min(due);
            }
        }
        // Wheel / touchpad scrolling: one step per presented frame on the compositor path
        // (`scroll_frame` in `paint`), one event per 8 ms on the DOM path.
        if let Some(due) = state.scroll_pump() {
            next = next.min(due);
        }
        // Tabs that parsed but never finish loading (spinner).
        if let Some(due) = state.load_watch_tick() {
            next = next.min(due);
        }
        // Middle-click autoscroll steps once per presented frame.
        if state.autoscroll.borrow().is_some() {
            state.window.request_redraw();
        }
        // The ring asks for frames only while it animates (30 fps), never when idle.
        if let Some(due) = state.glow_due() {
            if due <= Instant::now() {
                state.window.request_redraw();
            } else {
                next = next.min(due);
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let App::Running(state) = self else {
            return;
        };
        let state = state.clone();
        state.engine.spin();

        match event {
            WindowEvent::CloseRequested => {
                state.shutdown();
                *self = App::Exiting;
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => state.paint(),
            WindowEvent::Resized(_) => {
                let maximized = state.window.is_maximized();
                if state.maximized.replace(maximized) != maximized {
                    state.mark_chrome_dirty();
                }
                state.layout(true);
            }
            WindowEvent::Occluded(occluded) => {
                state.occluded.set(occluded);
                if !occluded {
                    state.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let scale = Scale::new(scale_factor as f32);
                for view in state.chrome_view().into_iter().chain(state.awake_views()) {
                    view.set_hidpi_scale_factor(scale);
                }
                state.layout(true);
            }
            WindowEvent::ModifiersChanged(mods) => state.modifiers.set(mods.state()),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    state.pw_note_input(); // a real gesture (passwords: focus suggestions)
                }
                state.on_key(event)
            }
            // IME: the composition shows inline (pre-edit, selected) and the candidate window
            // sits at the caret (see `ui_host`).
            WindowEvent::Ime(Ime::Preedit(text, _cursor)) => state.ime_preedit(text),
            WindowEvent::Ime(Ime::Commit(text)) => state.ime_commit(text),
            WindowEvent::Ime(Ime::Disabled) => state.ime_disabled(),
            WindowEvent::CursorMoved { position, .. } => {
                state.on_cursor_moved(DevicePoint::new(position.x as f32, position.y as f32))
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(view) = state.view_for(state.hover.get()) {
                    view.notify_input_event(InputEvent::MouseLeftViewport(
                        MouseLeftViewportEvent::default(),
                    ));
                }
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                if button_state == ElementState::Pressed {
                    state.pw_note_input();
                }
                state.on_mouse_button(button_state, button)
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let input = match delta {
                    MouseScrollDelta::LineDelta(dx, dy) => WheelInput::Lines(dx as f64, dy as f64),
                    MouseScrollDelta::PixelDelta(d) => WheelInput::Pixels(d.x, d.y),
                };
                // Ctrl+wheel over a web page zooms it.
                let ctrl = state.modifiers.get().control_key();
                if !(ctrl && state.browse_wheel_zoom(input)) {
                    state.on_wheel(input);
                }
            }
            _ => {}
        }
    }
}

/// The frameless main window. On Windows keep the DWM drop shadow and ask for rounded
/// corners (Windows 11; ignored on Windows 10), as a normal app window has.
/// Autoscroll cursor for a direction (`engine::autoscroll::direction`).
fn autoscroll_cursor(dir: (i8, i8)) -> CursorIcon {
    match dir {
        (0, 0) => CursorIcon::AllScroll,
        (0, -1) => CursorIcon::NResize,
        (0, 1) => CursorIcon::SResize,
        (-1, 0) => CursorIcon::WResize,
        (1, 0) => CursorIcon::EResize,
        (-1, -1) => CursorIcon::NwResize,
        (1, -1) => CursorIcon::NeResize,
        (-1, 1) => CursorIcon::SwResize,
        _ => CursorIcon::SeResize,
    }
}

/// A PNG from `resources/` as a winit icon (`None` if it cannot be decoded).
fn png_icon(bytes: &[u8]) -> Option<winit::window::Icon> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    winit::window::Icon::from_rgba(buf, info.width, info.height).ok()
}

fn window_attributes() -> winit::window::WindowAttributes {
    let attributes = Window::default_attributes()
        .with_window_icon(png_icon(ICON_32))
        .with_title(APP_NAME)
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(560.0, 360.0));
    #[cfg(windows)]
    let attributes = {
        use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
        attributes
            .with_undecorated_shadow(true)
            .with_corner_preference(CornerPreference::Round)
            // ICON_BIG (taskbar, Alt+Tab); the title-bar ICON_SMALL is the 32 px one.
            .with_taskbar_icon(png_icon(ICON_256))
    };
    attributes
}

fn resize_direction(edge: Edge) -> ResizeDirection {
    match edge {
        Edge::North => ResizeDirection::North,
        Edge::South => ResizeDirection::South,
        Edge::East => ResizeDirection::East,
        Edge::West => ResizeDirection::West,
        Edge::NorthEast => ResizeDirection::NorthEast,
        Edge::NorthWest => ResizeDirection::NorthWest,
        Edge::SouthEast => ResizeDirection::SouthEast,
        Edge::SouthWest => ResizeDirection::SouthWest,
    }
}

fn resize_cursor(edge: Edge) -> CursorIcon {
    match edge {
        Edge::North | Edge::South => CursorIcon::NsResize,
        Edge::East | Edge::West => CursorIcon::EwResize,
        Edge::NorthEast | Edge::SouthWest => CursorIcon::NeswResize,
        Edge::NorthWest | Edge::SouthEast => CursorIcon::NwseResize,
    }
}

/// Lay out the chrome strip and the page in a window (physical pixels).
fn layout_for(
    window: PhysicalSize<u32>,
    scale_factor: f64,
    card: bool,
    maximized: bool,
) -> WindowLayout {
    WindowLayout::new(
        window.width,
        window.height,
        scale_factor,
        chrome::CHROME_CSS_HEIGHT,
        card,
        maximized,
    )
}

fn size_or_one(size: PhysicalSize<u32>) -> PhysicalSize<u32> {
    PhysicalSize::new(size.width.max(1), size.height.max(1))
}

impl AppState {
    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    fn chrome_view(&self) -> Option<WebView> {
        self.chrome.borrow().clone()
    }

    fn is_chrome(&self, webview: &WebView) -> bool {
        self.chrome.borrow().as_ref() == Some(webview)
    }

    /// Clone of the active tab's handle; never hold the `tabs` borrow while calling into Servo.
    fn active_view(&self) -> Option<WebView> {
        self.tabs.borrow().active().and_then(|t| t.view.clone())
    }

    fn awake_views(&self) -> Vec<WebView> {
        self.tabs
            .borrow()
            .iter()
            .filter_map(|t| t.view.clone())
            .collect()
    }

    fn tab_id_of(&self, webview: &WebView) -> Option<TabId> {
        self.tabs
            .borrow_mut()
            .find_mut(|v| v == webview)
            .map(|t| t.id)
    }

    fn view_for(&self, area: Area) -> Option<WebView> {
        match area {
            Area::Chrome => self.chrome_view(),
            Area::Content => self.active_view(),
            Area::Frame => None,
        }
    }

    fn focused_view(&self) -> Option<WebView> {
        self.view_for(self.focus.get())
    }

    /// Size of the active page's webview.
    fn content_size(&self) -> PhysicalSize<u32> {
        let (w, h) = self.layout.get().content_size();
        PhysicalSize::new(w, h)
    }

    /// Whether the active tab shows a web page (a floating card) rather than an internal one.
    fn card_mode(&self) -> bool {
        self.tabs
            .borrow()
            .active()
            .is_some_and(|t| chrome::shows_as_card(&t.url))
    }

    fn compute_layout(&self) -> WindowLayout {
        let (assist, selpill) = self.assist_layout();
        let maximized = self.maximized.get() || self.window.fullscreen().is_some();
        let size = self.window.inner_size();
        // A fullscreen page (Fullscreen API) fills the window: no chrome bar, no card.
        let page_fullscreen = self.browse_fullscreen();
        WindowLayout::with_extras(
            size.width,
            size.height,
            self.window.scale_factor(),
            if page_fullscreen {
                0.0
            } else {
                chrome::CHROME_CSS_HEIGHT
            },
            self.card_mode() && !page_fullscreen,
            maximized,
            LayoutExtras {
                ai: self.ai_ease(Instant::now()),
                overlay: self.overlay.get(),
                overlay_size: match self.overlay.get() {
                    MythOverlay::Pill => self.pill_w.get().map(|w| (w, frame::MYTH_PILL_H_CSS)),
                    MythOverlay::Card => self.card_h.get().map(|h| (frame::MYTH_CARD_MAX_W_CSS, h)),
                    _ => None,
                },
                tasks: self.tasks_panel.get().then(|| self.tasks_panel_h.get()),
                popup: self.ui.popup.get(),
                assist,
                selpill,
            },
        )
    }

    /// 0..1: how far the sky gap is towards the wider AI gap now (eased; instant without
    /// motion).
    fn ai_ease(&self, now: Instant) -> f32 {
        let (from, to, since) = self.ai_gap.get();
        if !self.motion().animates() {
            return to;
        }
        let t = (now.saturating_duration_since(since).as_secs_f32() / AI_GAP_EASE.as_secs_f32())
            .clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - t).powi(3);
        from + (to - from) * e
    }

    /// Start easing the gap towards `to` (1: Myth works in the card, 0: it does not).
    fn set_ai_gap(&self, to: f32, now: Instant) -> bool {
        let (_, cur_to, _) = self.ai_gap.get();
        if cur_to == to {
            return false;
        }
        self.ai_gap.set((self.ai_ease(now), to, now));
        true
    }

    /// While the gap eases: re-lay out (one step per call) and say when the next step is due.
    fn ai_gap_tick(&self) -> Option<Instant> {
        let now = Instant::now();
        let (_, to, since) = self.ai_gap.get();
        let want = self.compute_layout();
        if want != self.layout.get() {
            self.layout(false);
        }
        let easing =
            self.motion().animates() && now < since + AI_GAP_EASE && self.ai_ease(now) != to;
        easing.then(|| now + glow::FRAME_INTERVAL)
    }

    /// Map a window point to the area under it and the point inside that webview. While a
    /// button is held, events stay with the webview that got the press.
    fn locate(&self, point: DevicePoint) -> (Area, DevicePoint) {
        let layout = self.layout.get();
        let in_content = || {
            let (x, y) = layout.to_content(point.x, point.y);
            DevicePoint::new(x, y)
        };
        match self.pressed.get() {
            Some(Area::Chrome) => (Area::Chrome, point),
            Some(Area::Content) => (Area::Content, in_content()),
            _ => match layout.region_at(point.x, point.y) {
                Region::Chrome => (Area::Chrome, point),
                Region::Content => (Area::Content, in_content()),
                Region::Frame => (Area::Frame, point),
            },
        }
    }

    // ------------------------------------------------------------------ layout & paint

    /// Recompute the layout and resize what changed. `window_changed`: the window itself was
    /// resized (or its scale changed), so the window surface and the chrome follow it.
    fn layout(&self, window_changed: bool) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return; // minimized
        }
        let new = self.compute_layout();
        let old = self.layout.replace(new);
        if window_changed {
            self.window_ctx.resize(size);
            if let Some(chrome) = self.chrome_view() {
                chrome.resize(size);
            }
        }
        // All tabs share `content_ctx`, so resizing one resizes it for all of them.
        if (window_changed || old.content != new.content)
            && let Some(view) = self.active_view()
        {
            view.resize(self.content_size());
        }
        if old != new {
            // The chrome draws the sky gap and Myth's surface from this layout.
            self.mark_chrome_dirty();
        }
        if old != new || window_changed {
            self.bump_backdrop();
        }
        self.redraw();
    }

    /// Paint chrome and active tab into their offscreen framebuffers, then compose them into
    /// the window (see `compose`).
    fn paint(&self) {
        let t_start = self.perf.mark();
        // This frame's scroll step (compositor path, autoscroll): handed to Servo now, it is
        // in the page frame shown at the next vsync, so steps stay one per display frame.
        self.scroll_frame();
        let chrome = self.chrome_view();
        let content = self.active_view();
        // Ring-only frames re-compose what Servo already painted, and a webview that has no
        // new frame keeps its offscreen image (a page scrolling does not re-render the
        // chrome, a chrome animation does not re-render the page).
        let paint_chrome = self.servo_paint.replace(false);
        let paint_content = self.content_paint.replace(false);
        let t = self.perf.mark();
        if paint_chrome && let Some(view) = &chrome {
            view.paint();
        }
        let servo_chrome = perf_host::ms_since(t);
        let t = self.perf.mark();
        let page_frame = (paint_chrome || paint_content) && content.is_some();
        if (paint_chrome || paint_content)
            && let Some(view) = &content
        {
            view.paint();
        }
        let servo_page = perf_host::ms_since(t);
        let t_compose = self.perf.mark();

        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let layout = self.layout.get();
        if let Err(e) = self.window_ctx.make_current() {
            warn!("make_current failed: {e:?}");
            return;
        }
        let gl = self.window_ctx.glow_gl_api();
        let window_fb = match self.paint_glass(&gl, &layout, content.as_ref()) {
            Some(fb) => fb,
            None => self.paint_classic(&gl, &layout, size, content.as_ref()),
        };
        if layout.card {
            let now = Instant::now();
            let frame = self.glow.borrow().frame(now, self.motion());
            if let Some(f) = frame {
                self.glow_gl.borrow_mut().paint(
                    &gl,
                    &layout,
                    window_fb,
                    self.window.scale_factor() as f32,
                    &f,
                );
            }
        }
        self.glow.borrow_mut().drawn();
        self.glow_last.set(Instant::now());
        let compose = perf_host::ms_since(t_compose);
        let t_present = self.perf.mark();
        self.window_ctx.present();
        if self.perf.on() {
            let sample = FrameSample {
                total: perf_host::ms_since(t_start),
                servo_page,
                servo_chrome,
                compose,
                present: perf_host::ms_since(t_present),
                page_frame,
            };
            if let Some(line) = self.perf.on_frame(sample) {
                self.perf_hud(Some(&line));
            }
        }
        // Keep the frames coming while a scroll animation or autoscroll runs (paced by
        // present, which waits for vsync).
        let scrolling =
            self.scroll_path.get() == GesturePath::Fast && !self.scroller.borrow().is_idle();
        if scrolling || self.autoscroll.borrow().is_some() {
            self.window.request_redraw();
        }
    }

    /// Show / hide the perf HUD line (drawn by the chrome).
    fn perf_hud(&self, line: Option<&str>) {
        if let Some(chrome) = self.chrome_view() {
            let arg = line.map_or("null".to_owned(), |l| json!(l).to_string());
            chrome.evaluate_javascript(format!("window.mbPerfHud && mbPerfHud({arg})"), |_| {});
        }
    }

    /// The classic composition (glass off): the chrome draws the sky; it is copied around the
    /// page card, the page is blitted, its corners rounded and Myth's surface copied over it.
    /// Returns the window framebuffer.
    fn paint_classic(
        &self,
        gl: &::glow::Context,
        layout: &WindowLayout,
        size: PhysicalSize<u32>,
        content: Option<&WebView>,
    ) -> u32 {
        // Find the chrome's framebuffer (and colour texture) by binding it. It is copied 1:1,
        // so only use it directly while it is exactly window-sized (otherwise, e.g. in the
        // middle of a resize, fall back to Servo's scaling blit of the whole chrome).
        let direct = self.chrome_ctx.size() == size
            && layout.width == size.width as i32
            && layout.height == size.height as i32;
        let (chrome_fb, chrome_tex) = if direct {
            self.chrome_ctx.prepare_for_rendering();
            let fb = compose::bound_framebuffer(gl);
            let tex = if layout.radius > 0 && fb != 0 {
                compose::bound_color_texture(gl)
            } else {
                0
            };
            (fb, tex)
        } else {
            (0, 0)
        };
        // Bind the window's framebuffer: Servo's blit first clears its target rect in whatever
        // framebuffer is bound, which would otherwise still be an offscreen one.
        self.window_ctx.prepare_for_rendering();
        let src = compose::Sources {
            chrome_fb,
            chrome_tex,
            window_fb: compose::bound_framebuffer(gl),
        };
        let window_h = layout.height;
        if chrome_fb != 0 {
            let mut bands = layout.chrome_bands();
            if content.is_none() {
                bands.push(layout.content);
            }
            compose::copy_from_chrome(gl, &src, window_h, &bands);
        } else if let Some(blit) = self.chrome_ctx.render_to_parent_callback() {
            blit(
                gl,
                Rect::new(
                    Point2D::new(0, 0),
                    Size2D::new(size.width as i32, size.height as i32),
                ),
            );
        }
        if content.is_some()
            && let Some(blit) = self.content_ctx.render_to_parent_callback()
        {
            // GL's origin is bottom-left.
            let g = layout.content.to_gl(window_h);
            blit(gl, Rect::new(Point2D::new(g.x, g.y), Size2D::new(g.w, g.h)));
            if layout.card {
                self.corners.borrow_mut().paint(gl, layout, &src);
            }
            // Myth's pill / approval card and the Tasks panel float over the page (drawn by
            // the chrome).
            self.overlay_gl
                .borrow_mut()
                .paint(gl, layout, &src, self.window.scale_factor() as f32);
        }
        src.window_fb
    }

    // ------------------------------------------------------------------ liquid glass

    /// Glass mode: sky, page, blurred backdrop under the reported surfaces, then the
    /// transparent chrome on top (see `glass`). Returns the window framebuffer, or `None` when
    /// glass is off (or just failed and switched itself off): compose the classic way.
    fn paint_glass(
        &self,
        gl: &::glow::Context,
        layout: &WindowLayout,
        content: Option<&WebView>,
    ) -> Option<u32> {
        let mode = self.glass_mode.get();
        if !mode.shell_sky() {
            return None;
        }
        self.chrome_ctx.prepare_for_rendering();
        let chrome_fb = compose::bound_framebuffer(gl);
        let chrome_tex = compose::bound_color_texture(gl);
        let page_tex = if content.is_some() {
            self.content_ctx.prepare_for_rendering();
            compose::bound_color_texture(gl)
        } else {
            0
        };
        self.window_ctx.prepare_for_rendering();
        let window_fb = compose::bound_framebuffer(gl);
        if chrome_tex == 0 || (content.is_some() && page_tex == 0) {
            self.glass_off("an offscreen framebuffer has no colour texture");
            return None;
        }
        if self.glass_probe.get() == GlassProbe::Due {
            self.glass_probe.set(GlassProbe::Done);
            match glass::chrome_is_transparent(gl, chrome_fb, layout.width, layout.height) {
                Some(false) => {
                    self.glass_off("the chrome webview is not transparent");
                    return None;
                }
                Some(true) => info!("liquid glass: the chrome webview is transparent"),
                None => warn!("liquid glass: could not check the chrome's transparency"),
            }
        }
        let scale = self.window.scale_factor() as f32;
        let rects = self.glass_rects(layout, scale);
        let c = layout.content;
        let page = content.map(|_| glass::PageLayer {
            tex: page_tex,
            rect: [
                c.x as f32,
                c.y as f32,
                (c.x + c.w) as f32,
                (c.y + c.h) as f32,
            ],
            card_radius: layout.card.then_some(layout.radius as f32),
        });
        let inputs = glass::FrameInputs {
            window_fb,
            width: layout.width,
            height: layout.height,
            scale,
            palette: glass_api::palette(self.sky_hour.get()),
            sky_t: self.sky_t.get(),
            page,
            chrome_tex,
            glass: &rects,
            generation: self.glass_gen.get(),
            page_generation: self.page_gen.get(),
            // Reduce transparency: solid surfaces, no blur at all.
            blur: mode == GlassMode::Blur && !self.look_now().1,
        };
        let painted = self
            .glass_gl
            .borrow_mut()
            .as_mut()
            .map(|g| g.paint(gl, &inputs));
        match painted {
            Some(Ok(glass::Painted::Glass)) => Some(window_fb),
            Some(Ok(glass::Painted::NoBlur)) => {
                self.set_glass_mode(GlassMode::Flat);
                Some(window_fb)
            }
            Some(Err(e)) => {
                self.glass_off(&e);
                None
            }
            None => {
                self.glass_off("no compositor");
                None
            }
        }
    }

    /// Every reported surface in window device px: the active internal page's, then the
    /// chrome's (drawn last, on top).
    fn glass_rects(&self, layout: &WindowLayout, scale: f32) -> Vec<glass::GlassRect> {
        let s = scale as f64;
        let mut out = Vec::new();
        if !layout.card {
            out.extend(glass_api::to_device(
                &self.glass_page.borrow(),
                s,
                layout.content,
            ));
        }
        out.extend(glass_api::to_device(
            &self.glass_chrome.borrow(),
            s,
            PxRect::new(0, 0, layout.width, layout.height),
        ));
        let look = self.glass_look();
        out.iter()
            .take(glass::MAX_RECTS)
            .map(|g| glass::GlassRect::from_report(g, scale, &look))
            .collect()
    }

    /// The Appearance in effect: the live preview while the slider moves, else the saved
    /// setting (Reduce transparency resolved against the OS): (transparency, reduced).
    fn look_now(&self) -> (u8, bool) {
        self.appearance_preview.get().unwrap_or_else(|| {
            let a = self.appearance.get();
            (a.transparency, a.reduced(self.os_reduced_transparency))
        })
    }

    /// What the blur does now (radius factor, saturation, brightness for the sky's tone).
    fn glass_look(&self) -> GlassLook {
        let (t, reduced) = self.look_now();
        let dark = glass_api::palette(self.sky_hour.get()).dark();
        GlassLook::at(t, dark, reduced)
    }

    /// The Appearance in effect changed: blur again with the new parameters, and tell the
    /// chrome (its state) and the active internal page.
    fn appearance_changed(&self) {
        self.bump_backdrop();
        self.mark_chrome_dirty();
        self.push_appearance();
        self.redraw();
    }

    /// Tell the active internal page the Appearance in effect (other pages get it when served).
    fn push_appearance(&self) {
        let internal = self
            .tabs
            .borrow()
            .active()
            .is_some_and(|t| chrome::is_internal_str(&t.url));
        if !internal {
            return;
        }
        if let Some(view) = self.active_view() {
            let (t, reduced) = self.look_now();
            let script =
                format!("window.JMGlass && JMGlass.setLook && JMGlass.setLook({t}, {reduced});");
            view.evaluate_javascript(script, |_| {});
        }
    }

    /// The backdrop (sky, page, layout) changed: the next glass frame blurs again.
    fn bump_backdrop(&self) {
        self.glass_gen.set(self.glass_gen.get().wrapping_add(1));
    }

    /// A webview reported its glass surfaces (`/api/glass`).
    fn glass_report(&self, webview: &WebView, report: &str, hour: Option<&str>, moving: bool) {
        let rects = glass_api::parse_report(report).unwrap_or_default();
        if self.is_chrome(webview) {
            *self.glass_chrome.borrow_mut() = rects;
            if let Some(h) = hour.and_then(chrome::parse_theme_hour) {
                let h = h as f32;
                if (self.sky_hour.get() - h).abs() > 1e-3 {
                    self.sky_hour.set(h);
                    self.bump_backdrop();
                }
            }
            if self.glass_mode.get().shell_sky() && self.glass_probe.get() == GlassProbe::Waiting {
                self.glass_probe.set(GlassProbe::Due);
            }
        } else if self.active_view().as_ref() == Some(webview) {
            *self.glass_page.borrow_mut() = rects;
        } else {
            return;
        }
        if moving {
            self.ui_moving_at.set(Some(Instant::now()));
        }
        if self.glass_mode.get().shell_sky() {
            // Re-compose with the new rectangles (the blur is reused).
            self.window.request_redraw();
        }
    }

    fn set_glass_mode(&self, mode: GlassMode) {
        if self.glass_mode.replace(mode) == mode {
            return;
        }
        info!("liquid glass: now {}", mode.as_str());
        self.mark_chrome_dirty();
        self.push_glass_mode();
        self.redraw();
    }

    /// Glass cannot work here: back to the classic look for good.
    fn glass_off(&self, why: &str) {
        if self.glass_mode.get() != GlassMode::Off {
            warn!("liquid glass: off ({why}); the UI draws its own sky");
        }
        self.set_glass_mode(GlassMode::Off);
    }

    /// Tell the active internal page the glass mode (the chrome gets it in its state).
    fn push_glass_mode(&self) {
        let internal = self
            .tabs
            .borrow()
            .active()
            .is_some_and(|t| chrome::is_internal_str(&t.url));
        if !internal {
            return;
        }
        if let Some(view) = self.active_view() {
            let script = format!(
                "window.JMGlass && window.JMGlass.setMode({:?});",
                self.glass_mode.get().as_str()
            );
            view.evaluate_javascript(script, |_| {});
        }
    }

    /// When the sky needs its next frame (glass mode only).
    fn sky_due(&self) -> Option<Instant> {
        if !self.glass_mode.get().shell_sky() {
            return None;
        }
        let size = self.window.inner_size();
        let visible = !self.occluded.get() && size.width > 0 && size.height > 0;
        let now = Instant::now();
        let moving_ago = self
            .ui_moving_at
            .get()
            .map(|t| now.saturating_duration_since(t).as_millis() as u64);
        let mut ms = glass_api::sky_next_ms(self.motion().animates(), visible, moving_ago)?;
        if self.gpu.software {
            ms = ms.max(SKY_FRAME_MS_SOFTWARE);
        }
        Some(self.sky_last.get() + Duration::from_millis(ms))
    }

    /// Advance the sky one frame (its clock only moves with full motion) and re-compose.
    fn sky_step(&self) {
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.sky_last.get())
            .as_secs_f32()
            .min(0.25);
        self.sky_last.set(now);
        if self.motion().animates() {
            self.sky_t.set(self.sky_t.get() + dt);
        }
        self.bump_backdrop();
        self.window.request_redraw();
    }

    /// Ask for a redraw in which Servo paints too (see `servo_paint`).
    fn redraw(&self) {
        self.servo_paint.set(true);
        self.window.request_redraw();
    }

    /// A webview has a new frame: repaint just that one at the next redraw (one redraw per
    /// display frame: winit coalesces `request_redraw`).
    fn redraw_view(&self, chrome: bool) {
        if chrome {
            self.servo_paint.set(true);
        } else {
            self.content_paint.set(true);
            self.page_gen.set(self.page_gen.get().wrapping_add(1));
        }
        self.window.request_redraw();
    }

    fn motion(&self) -> Motion {
        Motion::resolve(self.motion_setting.get(), self.os_reduced_motion)
    }

    /// When the ring needs its next frame (`None`: not until something changes).
    fn glow_due(&self) -> Option<Instant> {
        self.glow
            .borrow()
            .next_frame(self.glow_last.get(), Instant::now(), self.motion())
    }

    /// Recompute the ring and the Myth pill for the tab in the card (once per loop pass).
    fn update_glow(&self) {
        let now = Instant::now();
        let (active, loading) = {
            let tabs = self.tabs.borrow();
            match tabs.active() {
                Some(t) => (Some(t.id), t.loading),
                None => (None, false),
            }
        };
        let card = self.layout.get().card;
        let ai = active.and_then(|id| self.agent_activity(id));
        let mut glow = self.glow.borrow_mut();
        // While Myth waits for an OK the approval card is the signal (as in the approved
        // design): no amber ring around the whole card.
        let ring_ai = ai.filter(|a| *a != glow::AiActivity::Approval);
        glow.set(GlowState::pick(card, ring_ai, loading), now);
        if let Some((tab, kind)) = self.agent.flash.take()
            && card
            && Some(tab) == active
        {
            glow.flash(kind, now);
        }
        if let Some((tab, x, y)) = self.agent.pulse.take()
            && card
            && Some(tab) == active
        {
            glow.pulse(x, y, now);
        }
        if !card {
            glow.clear(now);
        }
        drop(glow);
        let bar = if card {
            // The tab's own Myth bar; else a run waiting on a human-verification check
            // somewhere shows its card over whatever web page is in front.
            let own = active.and_then(|id| self.agent_bar(id));
            // The Myth panel shows this conversation (approvals inline): no pill / card over
            // the page for it, except a human-verification hand-off.
            let own = own.filter(|b| {
                let conv = b.get("conv").and_then(serde_json::Value::as_u64);
                let handoff = b.get("handoff").is_some_and(|h| !h.is_null());
                handoff
                    || !matches!((active, conv), (Some(t), Some(c)) if self.assist_shows_conv(t, c))
            });
            let card_of = |b: &serde_json::Value, k: &str| b.get(k).is_some_and(|v| !v.is_null());
            match own {
                Some(b) if card_of(&b, "approval") || card_of(&b, "handoff") => Some(b),
                own => self.agent_handoff_bar(active).or(own),
            }
        } else {
            None
        };
        // Myth's surface over the card and the wider sky gap while it works. An approval
        // card wins over the compat panel, which wins over the status pill.
        let overlay = match &bar {
            Some(b)
                if b.get("approval").is_some_and(|a| !a.is_null())
                    || b.get("handoff").is_some_and(|h| !h.is_null()) =>
            {
                MythOverlay::Card
            }
            _ if card && self.pw_panel_open() => MythOverlay::Passwords,
            _ if card && self.compat_panel_open() => MythOverlay::Compat,
            None => MythOverlay::None,
            Some(_) => MythOverlay::Pill,
        };
        let working = card && ai.is_some();
        let mut relayout = self.overlay.replace(overlay) != overlay;
        relayout |= self.set_ai_gap(if working { 1.0 } else { 0.0 }, now);
        if *self.last_bar.borrow() != bar {
            *self.last_bar.borrow_mut() = bar;
            self.mark_chrome_dirty();
        }
        // Tab status dots follow every run, not only the one in the card.
        let dots = self.tab_myth_states();
        if *self.last_dots.borrow() != dots {
            *self.last_dots.borrow_mut() = dots;
            self.mark_chrome_dirty();
        }
        if relayout {
            self.layout(false);
        }
    }

    /// `None`: follow the OS animation setting again.
    fn save_motion(&self, motion: Option<Motion>) {
        self.motion_setting.set(motion);
        self.agent.motion.set(self.motion());
        self.save_ui_pref("motion", motion.map_or("system", Motion::as_str));
        self.mark_chrome_dirty();
        self.window.request_redraw();
    }

    /// Write one key of `<profile>/ui.json`, keeping the others.
    fn save_ui_pref(&self, key: &str, value: &str) {
        let Some(path) = &self.prefs_path else { return };
        let mut obj = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .filter(|v| v.is_object())
            .unwrap_or_else(|| json!({}));
        obj[key] = json!(value);
        if let Err(e) = std::fs::write(path, obj.to_string()) {
            warn!("could not save UI preferences: {e}");
        }
    }

    /// Settings: which UA to send. Applies to requests from now on and to pages loaded
    /// from now on (`navigator`); open pages see it after a reload.
    fn set_ua_mode(&self, mode: UaMode) {
        if self.ua_mode.replace(mode) == mode {
            return;
        }
        let ua = mode.user_agent(UaOs::current());
        info!("user agent ({}): {ua}", mode.as_str());
        self.engine.set_user_agent(&ua);
        let script = Rc::new(UserScript::new(
            mythbrowse_compat::ua::navigator_script(&ua),
            None,
        ));
        let old = self.ua_script.replace(script.clone());
        self.compat.user_content.remove_script(old);
        self.compat.user_content.add_script(script);
        self.save_ui_pref("ua_mode", mode.as_str());
    }

    // ------------------------------------------------------------------------- tabs

    fn new_content_view(&self, url: Url) -> WebView {
        self.engine.new_page_webview(
            self.content_ctx.clone(),
            url,
            self.window.scale_factor() as f32,
            self.delegate.clone(),
            self.compat.user_content.clone(),
        )
    }

    /// Hide the active tab and remember where it was scrolled to (for tab sleeping).
    fn deactivate_current(&self) {
        let current = self
            .tabs
            .borrow()
            .active()
            .and_then(|t| t.view.clone().map(|v| (t.id, v)));
        if let Some((id, view)) = current {
            self.capture_scroll(id, &view);
            view.hide();
        }
    }

    fn capture_scroll(&self, id: TabId, view: &WebView) {
        let state = self.weak_self.clone();
        view.evaluate_javascript("[window.scrollX, window.scrollY]", move |result| {
            let Some(state) = state.upgrade() else { return };
            if let Ok(JSValue::Array(values)) = result
                && let [JSValue::Number(x), JSValue::Number(y)] = values.as_slice()
                && let Some(tab) = state.tabs.borrow_mut().get_mut(id)
            {
                tab.scroll = ScrollPos { x: *x, y: *y };
            }
        });
    }

    fn open_tab(&self, url: Url) {
        self.deactivate_current();
        let view = self.new_content_view(url.clone());
        self.tabs
            .borrow_mut()
            .open(view, url.as_str(), self.now_ms());
        self.set_focus(Area::Content);
        self.show_active();
    }

    /// Ctrl / middle click on a link in a user tab (`page_input`): a new tab after the
    /// current one, in the background unless Shift was held. Ignored unless the user just
    /// pressed a button in that page (a page cannot open tabs by logging the marker).
    fn open_link_request(&self, webview: &WebView, req: OpenRequest) {
        if self.is_chrome(webview) || self.agent.owns(webview) {
            return;
        }
        if self.active_view().as_ref() != Some(webview) {
            return;
        }
        let mut gate = self.open_gate.get();
        let allowed = gate.take(self.now_ms());
        self.open_gate.set(gate);
        if !allowed {
            warn!("page asked to open a link without a click; ignored");
            return;
        }
        let Ok(url) = Url::parse(&req.href) else {
            return;
        };
        if req.foreground {
            self.open_tab(url);
        } else {
            self.open_background_tab(url);
        }
    }

    /// A new awake tab right after the active one, not shown.
    fn open_background_tab(&self, url: Url) {
        let view = self.new_content_view(url.clone());
        view.hide();
        view.resize(self.content_size());
        self.tabs
            .borrow_mut()
            .open_background(view, url.as_str(), self.now_ms());
        self.mark_chrome_dirty();
    }

    // ------------------------------------------------------------------ wheel

    fn scroll_now_ms(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1000.0
    }

    /// A wheel / touchpad event: queued for the view under the pointer. On the compositor
    /// path `scroll_frame` sends one step per presented frame; on the DOM path `scroll_pump`
    /// sends one `WheelEvent` per 8 ms (notches animate unless motion is reduced).
    fn on_wheel(&self, input: WheelInput) {
        // A wheel turn ends autoscroll (as in Firefox) and is not applied.
        if self.autoscroll.borrow().is_some() {
            self.autoscroll_stop();
            return;
        }
        let (area, point) = self.locate(self.cursor.get());
        if area == Area::Frame {
            return;
        }
        let now = self.scroll_now_ms();
        self.perf.on_input();
        let mut scroller = self.scroller.borrow_mut();
        // Another view (or the page moved under a still pointer): start afresh there.
        let same_view = self.scroll_target.get().map(|(a, _)| a) == Some(area);
        if !same_view {
            scroller.cancel();
        }
        let new_gesture = !same_view || now - self.wheel_last_ms.get() > GESTURE_GAP_MS;
        self.wheel_last_ms.set(now);
        self.scroll_target.set(Some((area, point)));
        let px_per_notch = smooth_scroll::NOTCH_CSS_PX * self.window.scale_factor();
        scroller.on_wheel(input, px_per_notch, self.motion().animates(), now);
        drop(scroller);
        if new_gesture {
            self.choose_scroll_path(area, point, now);
        }
        self.scroll_pump();
    }

    /// Pick the gesture's path: the compositor unless the page reported a wheel listener
    /// that may call `preventDefault`; then the page says whether one is under the pointer
    /// (answer awaited for at most [`WHEEL_ASK_MS`]).
    fn choose_scroll_path(&self, area: Area, point: DevicePoint, now: f64) {
        let view = match area {
            Area::Content => self.active_view(),
            _ => None,
        };
        let listeners = view
            .as_ref()
            .and_then(|v| self.wheel_listeners.borrow().get(&v.id()).copied());
        let (Some(view), Some(_)) = (view, listeners) else {
            self.scroll_path.set(GesturePath::Fast);
            return;
        };
        let id = self.wheel_query.get() + 1;
        self.wheel_query.set(id);
        self.wheel_answer.set(None);
        self.scroll_path
            .set(GesturePath::Asking { since_ms: now, id });
        let scale = self.window.scale_factor();
        let script =
            page_input::wheel_blocking_script(point.x as f64 / scale, point.y as f64 / scale);
        let slot = self.wheel_answer.clone();
        view.evaluate_javascript(script, move |r| {
            let answer = match r {
                Ok(JSValue::String(s)) => page_input::parse_wheel_answer(&s),
                _ => WheelAnswer::No,
            };
            slot.set(Some((id, answer)));
        });
    }

    /// The gesture's path, once the page answered (or the wait is over).
    fn resolve_scroll_path(&self, now: f64) -> GesturePath {
        let GesturePath::Asking { since_ms, id } = self.scroll_path.get() else {
            return self.scroll_path.get();
        };
        let answer = self
            .wheel_answer
            .get()
            .filter(|(i, _)| *i == id)
            .map(|(_, a)| a);
        let frames = self
            .active_view()
            .and_then(|v| self.wheel_listeners.borrow().get(&v.id()).copied())
            .is_some_and(|(_, frame)| frame);
        let path = match answer {
            Some(WheelAnswer::Yes) => GesturePath::Dom,
            Some(WheelAnswer::Frame) if frames => GesturePath::Dom,
            Some(_) => GesturePath::Fast,
            None if now - since_ms >= WHEEL_ASK_MS => GesturePath::Fast,
            None => return self.scroll_path.get(),
        };
        self.scroll_path.set(path);
        path
    }

    /// DOM path: send what the scroller has due (one `WheelEvent` per 8 ms). Compositor
    /// path: make sure frames come (`scroll_frame` steps in `paint`). Returns when to look
    /// again.
    fn scroll_pump(&self) -> Option<Instant> {
        let now = self.scroll_now_ms();
        match self.resolve_scroll_path(now) {
            GesturePath::Asking { .. } => {
                if self.scroller.borrow().is_idle() {
                    return None;
                }
                Some(Instant::now() + Duration::from_millis(4))
            }
            GesturePath::Fast => {
                if !self.scroller.borrow().is_idle() {
                    self.window.request_redraw();
                }
                None
            }
            GesturePath::Dom => {
                let delta = self.scroller.borrow_mut().tick(now);
                if let Some((x, y)) = delta
                    && let Some((area, point)) = self.scroll_target.get()
                    && let Some(view) = self.view_for(area)
                {
                    view.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta {
                            x,
                            y,
                            z: 0.0,
                            mode: WheelMode::DeltaPixel,
                        },
                        point.into(),
                    )));
                    self.perf.on_scroll_sent(ScrollPath::Dom);
                }
                let due = self.scroller.borrow().next_due(self.scroll_now_ms())?;
                let wait = (due - self.scroll_now_ms()).max(0.0);
                Some(Instant::now() + Duration::from_micros((wait * 1000.0) as u64))
            }
        }
    }

    /// Once per presented frame (start of `paint`): the compositor-path wheel step and the
    /// autoscroll step, straight to Servo's compositor (`notify_scroll_event`: no DOM event,
    /// no script round trip; layout and `scroll` events follow asynchronously).
    fn scroll_frame(&self) {
        let now = self.scroll_now_ms();
        let mut sent = false;
        if self.resolve_scroll_path(now) == GesturePath::Fast {
            let delta = self
                .scroller
                .borrow_mut()
                .tick_paced(now, smooth_scroll::PACED_MIN_GAP_MS);
            if let Some((x, y)) = delta
                && let Some((area, point)) = self.scroll_target.get()
                && let Some(view) = self.view_for(area)
            {
                // Wheel deltas are positive towards the top; a scroll delta reveals the
                // bottom when positive (Servo inverts wheel deltas the same way).
                view.notify_scroll_event(
                    Scroll::Delta(WebViewVector::Device(DeviceVector2D::new(
                        -x as f32, -y as f32,
                    ))),
                    WebViewPoint::Device(point),
                );
                self.perf.on_scroll_sent(ScrollPath::Fast);
                sent = true;
            }
        }
        let step = self.autoscroll.borrow_mut().as_mut().and_then(|a| {
            let anchor = a.anchor();
            a.tick(now).map(|d| (d, anchor))
        });
        if let Some(((dx, dy), anchor)) = step
            && let Some(view) = self.active_view()
        {
            let (x, y) = self
                .layout
                .get()
                .to_content(anchor.0 as f32, anchor.1 as f32);
            self.perf.on_input();
            view.notify_scroll_event(
                Scroll::Delta(WebViewVector::Device(DeviceVector2D::new(
                    dx as f32, dy as f32,
                ))),
                WebViewPoint::Device(DevicePoint::new(x, y)),
            );
            self.perf.on_scroll_sent(ScrollPath::Fast);
            sent = true;
        }
        if sent {
            // Servo applies pending scrolls in `perform_updates` (inside `spin_event_loop`):
            // do it now so WebRender builds the frame while this one is composed.
            self.engine.spin();
        }
    }

    // ------------------------------------------------------------------ load watch

    /// Every couple of seconds: look at tabs that have loaded for a while without `load`;
    /// a parsed, quiet one stops loading (spinner off, `loading` false), with a log line
    /// naming what still holds its load event. Returns when to look again.
    fn load_watch_tick(&self) -> Option<Instant> {
        // Looks that came back since the last tick.
        let looks: Vec<(TabId, String)> = std::mem::take(&mut *self.load_looks.borrow_mut());
        let now = self.now_ms();
        for (id, json) in looks {
            let Some(look) = Look::parse(&json) else {
                continue;
            };
            let settled = self
                .load_watch
                .borrow_mut()
                .get_mut(&id)
                .is_some_and(|w| w.on_look(now, &look));
            if settled {
                self.load_settled(id);
            }
        }
        if self.load_watch.borrow().values().all(LoadWatch::settled) {
            return None;
        }
        let tick = self.load_watch_next.get();
        if Instant::now() < tick {
            return Some(tick);
        }
        let next = Instant::now() + Duration::from_millis(load_watch::PROBE_EVERY_MS);
        self.load_watch_next.set(next);
        let due: Vec<TabId> = self
            .load_watch
            .borrow()
            .iter()
            .filter(|(_, w)| w.due(now))
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let Some(view) = self.tabs.borrow().get(id).and_then(|t| t.view.clone()) else {
                self.load_watch.borrow_mut().remove(&id);
                continue;
            };
            let slot = self.load_looks.clone();
            view.evaluate_javascript(load_watch::PROBE_SCRIPT, move |r| {
                if let Ok(JSValue::String(s)) = r {
                    slot.borrow_mut().push((id, s));
                }
            });
        }
        Some(next)
    }

    /// A tab settled without `load`: stop its spinner and say why in the log.
    fn load_settled(&self, id: TabId) {
        let view = {
            let mut tabs = self.tabs.borrow_mut();
            let Some(tab) = tabs.get_mut(id) else { return };
            tab.loading = false;
            tab.view.clone()
        };
        if let Some(view) = view {
            let url = view.url().map(|u| u.to_string()).unwrap_or_default();
            view.evaluate_javascript(load_watch::BLOCKERS_SCRIPT, move |r| {
                let held = match r {
                    Ok(JSValue::String(s)) if !s.is_empty() => s,
                    _ => "nothing visible (a script or style sheet?)".to_owned(),
                };
                warn!("page parsed but never fired load; spinner stopped: {url} (pending: {held})");
            });
        }
        self.mark_chrome_dirty();
    }

    // ------------------------------------------------------------------ autoscroll

    /// A page or frame reported a wheel listener that may call `preventDefault`.
    fn wheel_listener_report(&self, webview: &WebView, r: WheelReport) {
        if self.is_chrome(webview) {
            return;
        }
        let mut map = self.wheel_listeners.borrow_mut();
        let e = map.entry(webview.id()).or_insert((false, false));
        if r.top {
            e.0 = true;
        } else {
            e.1 = true;
        }
    }

    /// The page script asks for autoscroll (middle press on a scrollable spot that is not a
    /// link): honoured only right after a real middle press in the active page.
    fn autoscroll_request(&self, webview: &WebView) {
        if self.is_chrome(webview)
            || self.agent.owns(webview)
            || self.active_view().as_ref() != Some(webview)
            || self.autoscroll.borrow().is_some()
        {
            return;
        }
        let now = self.scroll_now_ms();
        let Some((at, point)) = self.middle_press.take() else {
            return;
        };
        if now - at > AUTOSCROLL_GATE_MS {
            return;
        }
        self.scroller.borrow_mut().cancel();
        let scale = self.window.scale_factor();
        let mut a = Autoscroll::new((point.x as f64, point.y as f64), scale, now);
        if !self.middle_down.get() {
            // A quick click: the release came before the page's report.
            a.on_release();
        }
        a.on_move((self.cursor.get().x as f64, self.cursor.get().y as f64));
        let dir = a.direction();
        *self.autoscroll.borrow_mut() = Some(a);
        self.window.set_cursor(autoscroll_cursor(dir));
        if let Some(chrome) = self.chrome_view() {
            chrome.evaluate_javascript(
                format!(
                    "window.mbAutoscroll && mbAutoscroll({:.1}, {:.1})",
                    point.x as f64 / scale,
                    point.y as f64 / scale
                ),
                |_| {},
            );
        }
        self.window.request_redraw();
    }

    /// The pointer moved during autoscroll: steer (the page gets no moves meanwhile).
    /// Returns whether autoscroll is on.
    fn autoscroll_move(&self, point: DevicePoint) -> bool {
        let dir = {
            let mut a = self.autoscroll.borrow_mut();
            let Some(a) = a.as_mut() else { return false };
            a.on_move((point.x as f64, point.y as f64));
            a.direction()
        };
        self.window.set_cursor(autoscroll_cursor(dir));
        true
    }

    fn autoscroll_stop(&self) {
        if self.autoscroll.borrow_mut().take().is_none() {
            return;
        }
        if let Some(chrome) = self.chrome_view() {
            chrome.evaluate_javascript("window.mbAutoscroll && mbAutoscroll(null)", |_| {});
        }
        let icon = self
            .view_for(self.hover.get())
            .map_or(CursorIcon::Default, |v| keys::to_cursor_icon(v.cursor()));
        self.window.set_cursor(icon);
    }

    fn activate_tab(&self, id: TabId) {
        if self.tabs.borrow().active_id() == Some(id) {
            return;
        }
        self.autoscroll_stop();
        if self.tabs.borrow().get(id).is_none() {
            return;
        }
        self.deactivate_current();
        self.tabs.borrow_mut().activate(id, self.now_ms());
        self.wake_if_sleeping(id);
        self.set_focus(Area::Content);
        self.show_active();
    }

    fn cycle_tab(&self, step: isize) {
        let target = {
            let tabs = self.tabs.borrow();
            let ids: Vec<TabId> = tabs.iter().map(|t| t.id).collect();
            let Some(cur) = tabs
                .active_id()
                .and_then(|a| ids.iter().position(|i| *i == a))
            else {
                return;
            };
            if ids.len() < 2 {
                return;
            }
            ids[(cur as isize + step).rem_euclid(ids.len() as isize) as usize]
        };
        self.activate_tab(target);
    }

    fn close_tab(&self, id: TabId) {
        self.ui_tab_closed(id);
        self.assist_tab_closed(id);
        self.agent_unbind_user_tab(id);
        self.compat_tab_closed(id);
        self.pw_tab_closed(id);
        self.browse_tab_closed(id);
        let was_active = self.tabs.borrow().active_id() == Some(id);
        // Dropping the last handle closes the webview in Servo.
        let closed = self.tabs.borrow_mut().close(id);
        drop(closed);
        if self.tabs.borrow().is_empty() {
            self.open_tab(chrome::home_url(None));
            return;
        }
        if was_active {
            let new_active = self.tabs.borrow().active_id();
            if let Some(new_id) = new_active {
                self.wake_if_sleeping(new_id);
            }
            self.show_active();
        }
        self.mark_chrome_dirty();
    }

    /// Recreate the webview of a sleeping tab; its scroll position is restored on load.
    fn wake_if_sleeping(&self, id: TabId) {
        let url = {
            let tabs = self.tabs.borrow();
            match tabs.get(id) {
                Some(tab) if tab.is_sleeping() => Url::parse(&tab.url).ok(),
                _ => return,
            }
        };
        let view = self.new_content_view(url.unwrap_or_else(|| chrome::home_url(None)));
        let rejected = self.tabs.borrow_mut().wake(id, view).err();
        drop(rejected); // only if the tab woke up meanwhile
        info!("woke tab {id:?}");
    }

    fn show_active(&self) {
        // A fullscreen page that is no longer in front leaves fullscreen.
        self.browse_tab_shown();
        // Web pages float as a card, internal pages fill the area: the size may change.
        self.layout(false);
        if let Some(view) = self.active_view() {
            view.resize(self.content_size());
            view.show();
            if self.focus.get() == Area::Content {
                view.focus();
            }
        }
        self.refresh_window_title();
        self.mark_chrome_dirty();
        // The new active page reports its own glass (internal pages) or none (web pages).
        self.glass_page.borrow_mut().clear();
        self.push_glass_mode();
        self.push_appearance();
        self.redraw();
    }

    /// Put idle background tabs to sleep; returns when to check next.
    fn maybe_sleep_tabs(&self) -> Instant {
        let now = Instant::now();
        if now < self.next_sleep_check.get() {
            return self.next_sleep_check.get();
        }
        let candidates = self
            .tabs
            .borrow()
            .sleep_candidates(&self.sleep_policy, self.now_ms());
        for id in candidates {
            if self.agent_uses_user_tab(id) {
                continue; // Myth is working in it
            }
            if self.browse_is_opener(id) {
                continue; // its pop-up (a sign-in) may still post back to it
            }
            let view = self.tabs.borrow_mut().sleep(id, None);
            if view.is_some() {
                info!("tab {id:?} is now sleeping");
                self.mark_chrome_dirty();
            }
            drop(view); // releases the webview (and its pipeline) in Servo
        }
        let every = if self.sleep_policy.idle_ms > 0 {
            Duration::from_millis(self.sleep_policy.idle_ms / 4)
                .clamp(Duration::from_secs(1), SLEEP_CHECK_EVERY)
        } else {
            SLEEP_CHECK_EVERY
        };
        self.next_sleep_check.set(now + every);
        now + every
    }

    // ----------------------------------------------------------------------- chrome

    fn mark_chrome_dirty(&self) {
        self.chrome_dirty.set(true);
        // Anything the tab strip shows may be part of the saved session (debounced).
        self.files_session_dirty();
    }

    fn chrome_state(&self) -> ChromeState {
        let (back, forward) = self.can_go_back_forward();
        let active_url = self
            .tabs
            .borrow()
            .active()
            .map(|t| t.url.clone())
            .unwrap_or_default();
        let bookmarked =
            !active_url.is_empty() && self.store.is_bookmarked(&active_url).unwrap_or(false);
        let mut state = ChromeState::from_tabs(&self.tabs.borrow(), back, forward, bookmarked);
        state.maximized = self.maximized.get();
        state.card = self.layout.get().card;
        if !state.card {
            state.tint = None;
        }
        state.myth = self.last_bar.borrow().clone();
        let dots = self.last_dots.borrow();
        let checks = self.compat_check_tabs();
        for t in &mut state.tabs {
            t.myth = dots.iter().find(|(id, _)| *id == t.id).map(|(_, s)| *s);
            // Quiet badge on a background tab whose site wants a human check (never on the
            // tab in front).
            t.verify = !t.active && checks.contains(&t.id);
        }
        let layout = self.layout.get();
        state.assist = self.assist_state();
        if layout.card || layout.panel.is_some() {
            let scale = self.window.scale_factor().max(0.1);
            let css = |v: i32| (v as f64 / scale * 100.0).round() / 100.0;
            let rect = |o: frame::OverlayRect| {
                json!({
                    "kind": match o.kind {
                        MythOverlay::Card => "card",
                        MythOverlay::Compat => "compat",
                        MythOverlay::Passwords => "passwords",
                        MythOverlay::Tasks => "tasks",
                        MythOverlay::Assist => "assist",
                        MythOverlay::AssistDocked => "assist_docked",
                        MythOverlay::SelPill => "selpill",
                        _ => "pill",
                    },
                    "x": css(o.rect.x), "y": css(o.rect.y),
                    "w": css(o.rect.w), "h": css(o.rect.h), "r": css(o.radius),
                })
            };
            state.frame = Some(json!({
                "gap": css(layout.gap),
                "overlay": layout.overlay.map(rect),
                "panel": layout.panel.map(rect),
                "assist": layout.assist.map(rect),
                "selpill": layout.selpill.map(rect),
            }));
        }
        state.motion = self.motion().as_str();
        state.glass = self.glass_mode.get().as_str();
        let (transparency, reduced) = self.look_now();
        state.transparency = transparency;
        state.reduce_transparency = reduced;
        state.compat = self.compat_state();
        state.downloads = self.files_toolbar();
        state.passwords = self.pw_state();
        self.ui_state(&mut state);
        self.browse_state(&mut state);
        state
    }

    /// Back / Forward availability for the active tab: its live view, plus history it keeps
    /// from before a sleep or a restart (`Tab::extra_back` / `extra_forward`).
    fn can_go_back_forward(&self) -> (bool, bool) {
        let tabs = self.tabs.borrow();
        let Some(t) = tabs.active() else {
            return (false, false);
        };
        let (vb, vf) = t
            .view
            .as_ref()
            .map_or((false, false), |v| (v.can_go_back(), v.can_go_forward()));
        (
            vb || !t.extra_back.is_empty(),
            vf || !t.extra_forward.is_empty(),
        )
    }

    /// Back in the active tab: the live view's history first, then the kept history (a fresh
    /// view for that entry; Servo 0.1.4 cannot be handed a history list).
    fn go_back(&self) {
        let Some(id) = self.tabs.borrow().active_id() else {
            return;
        };
        if let Some(v) = self.active_view().filter(|v| v.can_go_back()) {
            v.go_back(1);
            return;
        }
        let Some(url) = self.tabs.borrow_mut().step_extra_back(id) else {
            return;
        };
        self.replace_active_view(id, &url);
    }

    fn go_forward(&self) {
        let Some(id) = self.tabs.borrow().active_id() else {
            return;
        };
        if let Some(v) = self.active_view().filter(|v| v.can_go_forward()) {
            v.go_forward(1);
            return;
        }
        let Some(url) = self.tabs.borrow_mut().step_extra_forward(id) else {
            return;
        };
        self.replace_active_view(id, &url);
    }

    fn replace_active_view(&self, id: TabId, url: &str) {
        let target = Url::parse(url).unwrap_or_else(|_| chrome::home_url(None));
        let view = self.new_content_view(target.clone());
        let old = self
            .tabs
            .borrow_mut()
            .replace_view(id, view, target.as_str());
        if let Some(old) = &old {
            old.hide();
        }
        drop(old);
        self.show_active();
    }

    /// Myth's state in every user tab it works in (for the tabs' status dots).
    fn tab_myth_states(&self) -> Vec<(u64, &'static str)> {
        let ids: Vec<TabId> = self.tabs.borrow().iter().map(|t| t.id).collect();
        ids.into_iter()
            .filter_map(|id| {
                self.agent_activity(id).map(|a| {
                    let s = match a {
                        glow::AiActivity::Approval => "waiting",
                        glow::AiActivity::Controlling => "controlling",
                        glow::AiActivity::Thinking => "thinking",
                    };
                    (id.0, s)
                })
            })
            .collect()
    }

    /// Push the current state into the chrome page (coalesced: once per event-loop pass).
    fn flush_chrome(&self) {
        if !self.chrome_dirty.replace(false) {
            return;
        }
        if let Some(chrome) = self.chrome_view() {
            chrome.evaluate_javascript(self.chrome_state().to_script(), |result| {
                if let Err(e) = result {
                    log::debug!("chrome render script failed: {e:?}");
                }
            });
        }
    }

    fn set_focus(&self, area: Area) {
        self.focus.set(area);
        if let Some(view) = self.view_for(area) {
            view.focus();
        }
    }

    fn focus_omnibox(&self) -> bool {
        self.set_focus(Area::Chrome);
        if let Some(chrome) = self.chrome_view() {
            chrome.evaluate_javascript(chrome::FOCUS_OMNIBOX_SCRIPT, |_| {});
        }
        true
    }

    fn refresh_window_title(&self) {
        let title = match self.tabs.borrow().active() {
            Some(tab) => chrome::window_title(&tab.title, &tab.url),
            None => APP_NAME.to_owned(),
        };
        self.window.set_title(&title);
    }

    /// Address-bar submit: URL, `? search`, or AI prompt (→ AI home). `shot`: "Ask about this
    /// page", the prompt goes with a screenshot of the tab (vision models only).
    fn navigate(&self, input: &str, shot: bool) {
        let Some(target) = omnibox::resolve(input) else {
            return;
        };
        // A prompt typed on a web page: Myth works right here, in this tab.
        if let OmniboxTarget::Prompt(text) = &target {
            let here = {
                let tabs = self.tabs.borrow();
                tabs.active()
                    .filter(|t| chrome::shows_as_card(&t.url))
                    .and_then(|t| t.view.clone().map(|v| (t.id, v)))
            };
            if let Some((id, view)) = here {
                self.agent_send_here(id, view, text.clone(), shot);
                self.set_focus(Area::Content);
                return;
            }
        }
        let url = target_url(&target);
        if self.files_typed(&url) {
            self.set_focus(Area::Content);
            return;
        }
        match self.active_view() {
            Some(view) => view.load(url),
            None => self.open_tab(url),
        }
        self.set_focus(Area::Content);
    }

    fn toggle_maximize(&self) {
        self.chrome_pointer_left();
        self.window.set_maximized(!self.window.is_maximized());
    }

    /// Snap Layouts: the pointer on the maximize button as Windows reports it (see `snap`).
    fn snap_pump(&self) {
        for e in snap::take_events() {
            match e {
                snap::SnapEvent::Hover(on) => {
                    if let Some(chrome) = self.chrome_view() {
                        chrome.evaluate_javascript(
                            format!(
                                "window.mb && window.mb.snapHover && window.mb.snapHover({on});"
                            ),
                            |_| {},
                        );
                    }
                }
                snap::SnapEvent::Click => self.toggle_maximize(),
            }
        }
    }

    fn toggle_bookmark(&self) -> Option<bool> {
        let (url, title) = {
            let tabs = self.tabs.borrow();
            let tab = tabs.active()?;
            (tab.url.clone(), tab.title.clone())
        };
        if chrome::is_internal_str(&url) {
            return None;
        }
        let now = mythbrowse_storage::unix_now_ms();
        match self.store.toggle_bookmark(&url, &title, now) {
            Ok(on) => {
                self.mark_chrome_dirty();
                Some(on)
            }
            Err(e) => {
                warn!("bookmark failed: {e}");
                None
            }
        }
    }

    fn open_library(&self) {
        let existing = self
            .tabs
            .borrow()
            .iter()
            .find(|t| {
                Url::parse(&t.url).is_ok_and(|u| {
                    chrome::is_internal(&u) && u.path() == chrome::library_url().path()
                })
            })
            .map(|t| t.id);
        match existing {
            Some(id) => {
                self.activate_tab(id);
                if let Some(view) = self.active_view() {
                    view.reload();
                }
            }
            None => self.open_tab(chrome::library_url()),
        }
    }

    /// Show / hide the floating Tasks panel (`None`: unchanged, just lay out again).
    fn set_tasks_panel(&self, open: Option<bool>) {
        if let Some(open) = open
            && self.tasks_panel.replace(open) != open
        {
            self.mark_chrome_dirty();
        }
        self.layout(false);
    }

    /// Open the Tasks page (what the AI is doing in the background), or switch to it.
    fn open_tasks(&self) {
        let tasks = chrome::tasks_url();
        let existing = self
            .tabs
            .borrow()
            .iter()
            .find(|t| {
                Url::parse(&t.url)
                    .is_ok_and(|u| chrome::is_internal(&u) && u.path() == tasks.path())
            })
            .map(|t| t.id);
        match existing {
            Some(id) => self.activate_tab(id),
            None => self.open_tab(tasks),
        }
    }

    /// Record a visit to `url` in history (unless it is not a web page or this is a private
    /// session). Returns true if it was recorded.
    fn record_page(&self, url: &Url, title: &str) -> bool {
        if self.agent.private
            || !mythbrowse_storage::should_record(url.as_str(), chrome::INTERNAL_HOST)
        {
            return false;
        }
        match self
            .store
            .record_visit(url.as_str(), title, mythbrowse_storage::unix_now_ms())
        {
            Ok(()) => true,
            Err(e) => {
                warn!("history write failed: {e}");
                false
            }
        }
    }

    /// Run a UI RPC; returns the HTTP status and JSON body. `secret` is the API key header
    /// sent by the model settings form (agent RPCs only).
    fn handle_api(&self, call: ApiCall, secret: Option<Secret>) -> (StatusCode, String) {
        let db = |r: Result<serde_json::Value, mythbrowse_storage::StorageError>| match r {
            Ok(v) => (StatusCode::OK, chrome::json_ok(v)),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                chrome::json_error(&e.to_string()),
            ),
        };
        let ok = || (StatusCode::OK, chrome::json_ok(serde_json::Value::Null));
        match call {
            ApiCall::State => (
                StatusCode::OK,
                chrome::json_ok(json!({ "state": self.chrome_state() })),
            ),
            ApiCall::Navigate { input, shot } => {
                self.navigate(&input, shot);
                ok()
            }
            ApiCall::Back => {
                self.go_back();
                ok()
            }
            ApiCall::Forward => {
                self.go_forward();
                ok()
            }
            ApiCall::Reload => {
                if let Some(v) = self.active_view() {
                    v.reload();
                }
                ok()
            }
            ApiCall::NewTab => {
                self.open_tab(chrome::home_url(None));
                ok()
            }
            ApiCall::CloseTab(id) => {
                self.close_tab(id);
                ok()
            }
            ApiCall::ActivateTab(id) => {
                self.activate_tab(id);
                ok()
            }
            ApiCall::ToggleBookmark => (
                StatusCode::OK,
                chrome::json_ok(json!({ "bookmarked": self.toggle_bookmark() })),
            ),
            ApiCall::FocusContent => {
                self.set_focus(Area::Content);
                ok()
            }
            ApiCall::OpenLibrary => {
                self.open_library();
                ok()
            }
            ApiCall::OpenTasks => {
                self.set_tasks_panel(Some(false));
                self.open_tasks();
                ok()
            }
            ApiCall::TasksPanel { open, h } => {
                if let Some(h) = h {
                    self.tasks_panel_h.set(Some(h as f32));
                }
                self.set_tasks_panel(open.map(|o| match o {
                    chrome::PanelOpen::Show => true,
                    chrome::PanelOpen::Hide => false,
                    chrome::PanelOpen::Toggle => !self.tasks_panel.get(),
                }));
                (
                    StatusCode::OK,
                    chrome::json_ok(json!({ "open": self.tasks_panel.get() })),
                )
            }
            ApiCall::MythSize { w, h } => {
                if let Some(w) = w {
                    self.pill_w.set(Some(w as f32));
                }
                if let Some(h) = h {
                    self.card_h.set(Some(h as f32));
                }
                self.layout(false);
                ok()
            }
            ApiCall::WindowDrag => {
                self.start_window_drag();
                ok()
            }
            ApiCall::WindowMinimize => {
                self.chrome_pointer_left();
                self.window.set_minimized(true);
                ok()
            }
            ApiCall::WindowToggleMaximize => {
                self.toggle_maximize();
                ok()
            }
            ApiCall::WindowClose => {
                self.exit_requested.set(true);
                // Make sure the event loop wakes up to see the request.
                self.window.request_redraw();
                ok()
            }
            ApiCall::Agent(call) => self.agent_api(call, secret),
            ApiCall::Assist(call) => self.assist_api(call),
            ApiCall::Compat(op) => self.compat_api(op),
            ApiCall::Files(op) => self.files_api(op),
            ApiCall::Settings(op) => self.settings_api(op),
            ApiCall::Passwords(op) => self.pw_api(op, secret),
            ApiCall::Suggest { q, seq } => {
                (StatusCode::OK, chrome::json_ok(self.ui_suggest(&q, seq)))
            }
            ApiCall::SuggestDelete { url } => {
                let deleted = self.ui_suggest_delete(&url);
                (
                    StatusCode::OK,
                    chrome::json_ok(json!({ "deleted": deleted })),
                )
            }
            ApiCall::Popup { rect } => {
                self.ui_set_popup(rect);
                ok()
            }
            ApiCall::MenuPick { id, action } => {
                let action = action.as_deref().and_then(page_ui::MenuAction::parse);
                let done = self.ui_menu_pick(id, action);
                (StatusCode::OK, chrome::json_ok(json!({ "done": done })))
            }
            ApiCall::TabMenu { id, action } => {
                let done = self.ui_tab_menu(id, action);
                (StatusCode::OK, chrome::json_ok(json!({ "done": done })))
            }
            ApiCall::DialogAnswer {
                id,
                ok: yes,
                value,
                suppress,
                remember,
            } => {
                let done = self.ui_dialog_answer(id, yes, value.as_deref(), suppress, remember);
                (StatusCode::OK, chrome::json_ok(json!({ "done": done })))
            }
            ApiCall::Popups { always } => {
                self.ui_popups(always);
                ok()
            }
            ApiCall::MaxButton { rect } => {
                let scale = self.window.scale_factor();
                let px = |v: f32| (v as f64 * scale).round() as i32;
                snap::set_max_button(rect.map(|(x, y, w, h)| (px(x), px(y), px(w), px(h))));
                ok()
            }
            ApiCall::SiteSettings => match self.store.site_permissions() {
                Ok(v) => {
                    let rows: Vec<_> = v
                        .into_iter()
                        .map(|p| {
                            json!({ "origin": p.origin, "feature": p.feature,
                                "decision": p.decision.as_str(), "updated_ms": p.updated_ms,
                                "label": page_ui::feature_label(&p.feature) })
                        })
                        .collect();
                    (
                        StatusCode::OK,
                        chrome::json_ok(json!({
                            "sites": rows,
                            "features": mythbrowse_storage::site_settings::FEATURES,
                            "suggest_web": self.ui.suggest_web.get(),
                        })),
                    )
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    chrome::json_error(&e.to_string()),
                ),
            },
            ApiCall::SiteSettingSet {
                origin,
                feature,
                decision,
            } => {
                use mythbrowse_storage::site_settings::Decision;
                let d = decision.map(|a| if a { Decision::Allow } else { Decision::Block });
                match self.store.set_site_permission(
                    &origin,
                    &feature,
                    d,
                    mythbrowse_storage::unix_now_ms(),
                ) {
                    Ok(true) => ok(),
                    Ok(false) => (
                        StatusCode::BAD_REQUEST,
                        chrome::json_error("Unknown site or permission."),
                    ),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        chrome::json_error(&e.to_string()),
                    ),
                }
            }
            ApiCall::SiteClear { origin } => match self.store.clear_site(&origin) {
                Ok(n) => (StatusCode::OK, chrome::json_ok(json!({ "removed": n }))),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    chrome::json_error(&e.to_string()),
                ),
            },
            ApiCall::SuggestPref { on } => match self.ui_set_suggest_web(on) {
                Ok(()) => ok(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, chrome::json_error(&e)),
            },
            ApiCall::Source { id } => match self.ui_source(id) {
                Some(v) => (StatusCode::OK, chrome::json_ok(v)),
                None => (
                    StatusCode::NOT_FOUND,
                    chrome::json_error("That page source is no longer available."),
                ),
            },
            ApiCall::Sync(op) => self.sync.api(op, secret),
            ApiCall::MythOpen { conv } => {
                if let Some(id) = self.tabs.borrow().active_id() {
                    self.agent_dismiss_bar(id);
                }
                self.open_tab(chrome::conversation_url(conv));
                ok()
            }
            ApiCall::Zoom { dir } => {
                self.browse_zoom(dir);
                ok()
            }
            ApiCall::Find { query, dir } => {
                self.browse_find(query, dir);
                ok()
            }
            ApiCall::FindClose => {
                self.browse_find_close();
                ok()
            }
            ApiCall::MythDismiss => {
                if let Some(id) = self.tabs.borrow().active_id() {
                    self.agent_dismiss_bar(id);
                }
                ok()
            }
            ApiCall::ChromeTone { dark } => {
                if self.glow.borrow_mut().set_night(dark) {
                    self.window.request_redraw();
                }
                ok()
            }
            // Handled in `load_web_resource` (it needs the reporting webview).
            ApiCall::Glass { .. } => ok(),
            ApiCall::Prefs => {
                let a = self.appearance.get();
                (
                    StatusCode::OK,
                    chrome::json_ok(json!({
                        "motion": self.motion_setting.get().map_or("system", Motion::as_str),
                        "effective": self.motion().as_str(),
                        "system_reduced": self.os_reduced_motion,
                        "transparency": a.transparency,
                        "reduce_transparency": a.reduce_str(),
                        "reduce_transparency_effective": a.reduced(self.os_reduced_transparency),
                        "system_reduced_transparency": self.os_reduced_transparency,
                        // First-run welcome (ui/onboarding.js) and Settings > About.
                        "onboarded": self.store.pref(PREF_ONBOARDED).ok().flatten().as_deref() == Some("1"),
                        "version": env!("CARGO_PKG_VERSION"),
                        "renderer": self.gpu.summary(),
                        "software_renderer": self.gpu.software,
                        // Screenshots (pinned theme hour) and the CI agent build never get the
                        // welcome on their own (home.html#welcome still opens it).
                        "welcome_suppressed": self.theme_hour.is_some(),
                        // User agent: firefox (default) / chrome / servo, and the string sent.
                        "ua_mode": self.ua_mode.get().as_str(),
                        "user_agent": self.ua_mode.get().user_agent(UaOs::current()),
                        // Settings > Site compatibility: site-fix rule updates (and whether this build
                        // can download them at all).
                        "rules_auto_update": self.compat_auto_update(),
                        "rules_cloud": self.compat_cloud_rules_available(),
                    })),
                )
            }
            ApiCall::PrefsSave {
                motion,
                transparency,
                reduce_transparency,
                onboarded,
                ua_mode,
                rules_auto_update,
            } => {
                if let Some(on) = rules_auto_update {
                    self.compat_set_auto_update(on);
                }
                if let Some(mode) = ua_mode.as_deref().and_then(UaMode::parse) {
                    self.set_ua_mode(mode);
                }
                if let Some(done) = onboarded
                    && let Err(e) = self
                        .store
                        .set_pref(PREF_ONBOARDED, if done { "1" } else { "0" })
                {
                    warn!("could not save the welcome state: {e}");
                }
                if let Some(m) = motion {
                    self.save_motion(Motion::parse(&m));
                }
                if transparency.is_some() || reduce_transparency.is_some() {
                    let mut a = self.appearance.get();
                    if let Some(t) = transparency {
                        a.transparency = t;
                    }
                    if let Some(r) = reduce_transparency {
                        a.reduce = r;
                    }
                    let saved = transparency
                        .map_or(Ok(()), |t| self.store.set_transparency(t))
                        .and_then(|()| {
                            reduce_transparency
                                .map_or(Ok(()), |r| self.store.set_reduce_transparency(r))
                        });
                    if let Err(e) = saved {
                        warn!("could not save the appearance: {e}");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            chrome::json_error("Could not save the appearance."),
                        );
                    }
                    self.appearance.set(a);
                    self.appearance_preview.set(None);
                    self.appearance_changed();
                }
                let (t, reduced) = self.look_now();
                (
                    StatusCode::OK,
                    chrome::json_ok(json!({
                        "effective": self.motion().as_str(),
                        "transparency": t,
                        "reduce_transparency_effective": reduced,
                    })),
                )
            }
            ApiCall::GlassParams {
                transparency,
                reduce,
                reset,
            } => {
                let next = if reset {
                    None
                } else {
                    let (t, r) = self.look_now();
                    Some((transparency.unwrap_or(t), reduce.unwrap_or(r)))
                };
                if self.appearance_preview.replace(next) != next {
                    self.appearance_changed();
                }
                let (t, reduced) = self.look_now();
                (
                    StatusCode::OK,
                    chrome::json_ok(json!({
                        "transparency": t,
                        "reduce_transparency_effective": reduced,
                    })),
                )
            }
            ApiCall::History { query, limit } => db(self
                .store
                .search_history(query.as_deref().unwrap_or(""), limit)
                .map(|h| json!({ "history": h }))),
            ApiCall::HistoryDelete { url } => db(self
                .store
                .delete_history_url(&url)
                .map(|d| json!({ "deleted": d }))),
            ApiCall::HistoryClear { scope } => {
                let pages = if scope.pages() {
                    match self.store.clear_history() {
                        Ok(n) => n,
                        Err(e) => return db(Err(e)),
                    }
                } else {
                    0
                };
                let chats = if scope.chats() {
                    match self.agent_chat_clear() {
                        Ok(n) => n,
                        Err(e) => {
                            return (StatusCode::INTERNAL_SERVER_ERROR, chrome::json_error(&e));
                        }
                    }
                } else {
                    0
                };
                (
                    StatusCode::OK,
                    chrome::json_ok(json!({ "deleted": pages, "chats": chats })),
                )
            }
            ApiCall::Bookmarks => db(self.store.bookmarks().map(|b| json!({ "bookmarks": b }))),
            ApiCall::BookmarkRemove { url } => {
                let r = self
                    .store
                    .remove_bookmark(&url)
                    .map(|d| json!({ "removed": d }));
                self.mark_chrome_dirty();
                db(r)
            }
        }
    }

    // ------------------------------------------------------------------------ window

    /// Resize edge under a window point: only when the window is neither maximized nor
    /// fullscreen and no button is held.
    fn edge_at(&self, point: DevicePoint) -> Option<Edge> {
        if self.pressed.get().is_some()
            || self.maximized.get()
            || self.window.fullscreen().is_some()
        {
            return None;
        }
        let size = self.window.inner_size();
        let border = (chrome::RESIZE_BORDER_CSS as f64 * self.window.scale_factor()).round() as f32;
        chrome::resize_edge(
            point.x,
            point.y,
            size.width as f32,
            size.height as f32,
            border.max(2.0),
        )
    }

    /// Title-bar drag (RPC from the chrome on mouse-down): hand the pointer to the OS move
    /// loop (Aero Snap on Windows, the window manager on Linux). Only while the left button
    /// is actually held in the chrome, so a late RPC never starts a button-less drag.
    fn start_window_drag(&self) {
        if self.pressed.get() != Some(Area::Chrome) {
            return;
        }
        if let Err(e) = self.window.drag_window() {
            warn!("window drag not available: {e}");
            return;
        }
        // The OS now owns the pointer until the button is released, and the release may never
        // reach us (X11), or arrive after the move (Windows). Finish the click in the chrome now
        // and ignore that release.
        self.pressed.set(None);
        self.swallow_release.set(true);
        if let Some(chrome) = self.chrome_view() {
            chrome.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Up,
                mythbrowse_engine::servo::MouseButton::Left,
                self.cursor.get().into(),
            )));
        }
        self.chrome_pointer_left();
    }

    /// The window moves or changes size under the pointer (title-bar buttons, drags): clear
    /// the chrome's hover state; the next real pointer move sets it again.
    fn chrome_pointer_left(&self) {
        if let Some(chrome) = self.chrome_view() {
            chrome.notify_input_event(InputEvent::MouseLeftViewport(
                MouseLeftViewportEvent::default(),
            ));
        }
    }

    // ------------------------------------------------------------------------ input

    fn on_cursor_moved(&self, point: DevicePoint) {
        self.cursor.set(point);
        if self.autoscroll_move(point) {
            return;
        }
        let edge = self.edge_at(point);
        let previous_edge = self.edge.replace(edge);
        if let Some(edge) = edge {
            if previous_edge != Some(edge) {
                self.window.set_cursor(resize_cursor(edge));
            }
            return;
        }
        let (area, local) = self.locate(point);
        let previous = self.hover.replace(area);
        if previous_edge.is_some()
            && previous == area
            && let Some(view) = self.view_for(area)
        {
            self.window.set_cursor(keys::to_cursor_icon(view.cursor()));
        }
        if previous != area
            && self.pressed.get().is_none()
            && let Some(view) = self.view_for(previous)
        {
            view.notify_input_event(InputEvent::MouseLeftViewport(
                MouseLeftViewportEvent::default(),
            ));
        }
        if let Some(view) = self.view_for(area) {
            if previous != area {
                self.window.set_cursor(keys::to_cursor_icon(view.cursor()));
            }
            view.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(local.into())));
        } else if previous != area || previous_edge.is_some() {
            // The sky around the page card.
            self.window.set_cursor(CursorIcon::Default);
        }
    }

    fn on_mouse_button(&self, button_state: ElementState, button: winit::event::MouseButton) {
        let action = match button_state {
            ElementState::Pressed => MouseButtonAction::Down,
            ElementState::Released => MouseButtonAction::Up,
        };
        if action == MouseButtonAction::Down {
            self.swallow_release.set(false);
            if let Some(edge) = self.edge.get() {
                if button == winit::event::MouseButton::Left {
                    match self.window.drag_resize_window(resize_direction(edge)) {
                        // As for a move: the release belongs to the OS resize loop.
                        Ok(()) => self.swallow_release.set(true),
                        Err(e) => warn!("window resize not available: {e}"),
                    }
                }
                return;
            }
        } else if button == winit::event::MouseButton::Left && self.swallow_release.replace(false) {
            self.pressed.set(None);
            return;
        }
        let middle = button == winit::event::MouseButton::Middle;
        if middle {
            self.middle_down.set(action == MouseButtonAction::Down);
        }
        // Autoscroll: any press ends it (and neither the press nor its release reaches the
        // page); the middle release ends it only after a drag.
        if action == MouseButtonAction::Up && self.autoscroll_swallow.replace(false) {
            self.pressed.set(None);
            return;
        }
        if self.autoscroll.borrow().is_some() {
            if action == MouseButtonAction::Down {
                self.autoscroll_stop();
                self.autoscroll_swallow.set(true);
                return;
            }
            if middle {
                let release = self
                    .autoscroll
                    .borrow_mut()
                    .as_mut()
                    .map(|a| a.on_release());
                if release == Some(Release::End) {
                    self.autoscroll_stop();
                }
                // The page still gets the middle release (its button state stays right).
            }
        }
        let ctrl = {
            let m = self.modifiers.get();
            m.control_key() || m.super_key()
        };
        let link_press = button == winit::event::MouseButton::Middle
            || (button == winit::event::MouseButton::Left && ctrl);
        let button = keys::to_servo_button(button);
        let (area, local) = self.locate(self.cursor.get());
        if action == MouseButtonAction::Down {
            // A click stops a wheel animation (as in other browsers).
            self.scroller.borrow_mut().cancel();
            if area == Area::Content && link_press {
                let mut gate = self.open_gate.get();
                gate.arm(self.now_ms());
                self.open_gate.set(gate);
            }
            self.middle_press.set(
                (area == Area::Content && middle)
                    .then(|| (self.scroll_now_ms(), self.cursor.get())),
            );
        }
        if action == MouseButtonAction::Down && area == Area::Content {
            // A click on the page closes the compat panel (like any popover).
            self.compat_close_panel();
            // ...and an open context menu, which swallows the click (as native menus do).
            if self.ui_menu_open() {
                self.ui_close_menu();
                self.swallow_release.set(true);
                return;
            }
        }
        // A page behind its dialog gets no pointer input (the sheet is modal to the tab).
        if area == Area::Content && self.ui_page_blocked() && self.pressed.get().is_none() {
            return;
        }
        if action == MouseButtonAction::Down && area == Area::Content {
            // User activation: the page may open one pop-up right after this.
            self.ui_activation(self.tabs.borrow().active_id());
        }
        if area == Area::Frame {
            // Clicks on the sky around the page do nothing (and keep the keyboard focus).
            return;
        }
        if action == MouseButtonAction::Down {
            self.pressed.set(Some(area));
            if self.focus.get() != area {
                self.set_focus(area);
            }
        } else {
            self.pressed.set(None);
        }
        if let Some(view) = self.view_for(area) {
            view.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                action,
                button,
                local.into(),
            )));
        }
    }

    fn with_active(&self, f: impl FnOnce(&WebView)) -> bool {
        match self.active_view() {
            Some(v) => {
                f(&v);
                true
            }
            None => false,
        }
    }

    fn on_key(&self, event: KeyEvent) {
        let mods = self.modifiers.get();
        let pressed = event.state == ElementState::Pressed;

        if pressed && self.autoscroll.borrow().is_some() {
            // Any key ends autoscroll; Esc does nothing else.
            self.autoscroll_stop();
            if event.logical_key == WKey::Named(WNamedKey::Escape) {
                return;
            }
        }
        if pressed {
            let handled = match &event.logical_key {
                // Esc leaves a fullscreen page first.
                WKey::Named(WNamedKey::Escape) if self.browse_fullscreen() => {
                    self.browse_escape_fullscreen()
                }
                // F3 / Shift+F3: next / previous match while the find bar is open.
                WKey::Named(WNamedKey::F3) => self.browse_find_step(if mods.shift_key() {
                    mythbrowse_engine::browse::FindDir::Prev
                } else {
                    mythbrowse_engine::browse::FindDir::Next
                }),
                // Perf HUD + trace (perf.log in the profile folder).
                WKey::Named(WNamedKey::F12) if mods.control_key() && mods.shift_key() => {
                    if !self.perf.toggle(&self.gpu.summary()) {
                        self.perf_hud(None);
                    } else {
                        self.perf_hud(Some("perf: measuring…"));
                    }
                    true
                }
                // Esc answers Myth's pending request on this tab with "Don't allow" (Enter
                // never allows by default: allowing takes a click or a focused Allow button).
                // Esc closes the Tasks panel first (the less final of the two).
                WKey::Named(WNamedKey::Escape) if self.tasks_panel.get() => {
                    self.set_tasks_panel(Some(false));
                    true
                }
                WKey::Named(WNamedKey::Escape) if self.deny_pending_here() => true,
                WKey::Named(WNamedKey::F6) => self.focus_omnibox(),
                WKey::Named(WNamedKey::F5) => self.with_active(|v| v.reload()),
                WKey::Named(WNamedKey::Tab) if mods.control_key() => {
                    self.cycle_tab(if mods.shift_key() { -1 } else { 1 });
                    true
                }
                WKey::Named(WNamedKey::ArrowLeft) if mods.alt_key() => {
                    self.go_back();
                    true
                }
                WKey::Named(WNamedKey::ArrowRight) if mods.alt_key() => {
                    self.go_forward();
                    true
                }
                WKey::Character(c) if mods.alt_key() && c.eq_ignore_ascii_case("d") => {
                    self.focus_omnibox()
                }
                WKey::Character(c) if mods.control_key() => match c.to_lowercase().as_str() {
                    "l" | "k" => self.focus_omnibox(),
                    "t" => {
                        self.open_tab(chrome::home_url(None));
                        true
                    }
                    "w" => {
                        let active = self.tabs.borrow().active_id();
                        if let Some(id) = active {
                            self.close_tab(id);
                        }
                        true
                    }
                    "r" => self.with_active(|v| v.reload()),
                    // Page zoom (web pages); "=" is the unshifted "+" key.
                    "=" | "+" => self.browse_zoom(1),
                    "-" | "_" => self.browse_zoom(-1),
                    "0" => self.browse_zoom(0),
                    "f" => self.browse_find_open(),
                    "d" => {
                        self.toggle_bookmark();
                        true
                    }
                    "h" => {
                        self.open_library();
                        true
                    }
                    "y" if mods.shift_key() => {
                        self.files_open_downloads();
                        true
                    }
                    // Ctrl+J: the Myth panel on web pages (Tasks elsewhere); Ctrl+Shift+J: Tasks.
                    // (Not Ctrl+Space: that is the Windows IME switch.)
                    "j" if mods.shift_key() || !self.card_mode() => {
                        self.set_tasks_panel(Some(!self.tasks_panel.get()));
                        true
                    }
                    "j" => {
                        self.assist_toggle();
                        true
                    }
                    action @ ("c" | "x" | "v") => {
                        if let Some(view) = self.focused_view() {
                            let action = match action {
                                "c" => EditingActionEvent::Copy,
                                "x" => EditingActionEvent::Cut,
                                _ => EditingActionEvent::Paste,
                            };
                            view.notify_input_event(InputEvent::EditingAction(action));
                        }
                        true
                    }
                    _ => false,
                },
                _ => false,
            };
            if handled {
                return;
            }
        } else if matches!(&event.logical_key, WKey::Character(c)
            if mods.control_key() && matches!(c.to_lowercase().as_str(), "c" | "x" | "v"))
        {
            return; // the press was turned into an editing action
        }

        if self.focus.get() == Area::Content {
            if self.ui_page_blocked() {
                return; // the page's dialog has the keyboard (it lives in the chrome)
            }
            if pressed {
                self.ui_activation(self.tabs.borrow().active_id());
            }
        }
        // Everything else goes to the focused webview (chrome or page).
        if let (Some(view), Some(key)) =
            (self.focused_view(), keys::to_servo_key(&event.logical_key))
        {
            let state = if pressed {
                KeyState::Down
            } else {
                KeyState::Up
            };
            view.notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
                state,
                key,
                Code::Unidentified,
                Location::Standard,
                keys::to_servo_modifiers(mods),
                event.repeat,
                false,
            )));
        }
    }

    /// Read a loaded web page's colour (theme-color, else background) to tint the chrome.
    fn read_page_tint(&self, id: TabId, view: &WebView) {
        let is_web = self
            .tabs
            .borrow()
            .get(id)
            .is_some_and(|t| chrome::shows_as_card(&t.url));
        if !is_web {
            return;
        }
        let state = self.weak_self.clone();
        view.evaluate_javascript(frame::PAGE_TINT_SCRIPT, move |result| {
            let Some(state) = state.upgrade() else { return };
            let tint = match result {
                Ok(JSValue::String(value)) => frame::page_tint(&value),
                _ => None,
            };
            let (changed, active) = {
                let mut tabs = state.tabs.borrow_mut();
                let active = tabs.active_id() == Some(id);
                match tabs.get_mut(id) {
                    Some(tab) => (std::mem::replace(&mut tab.tint, tint) != tint, active),
                    None => return,
                }
            };
            if changed && active {
                state.mark_chrome_dirty();
                // Make sure the event loop runs to push the new state into the chrome.
                state.redraw();
            }
        });
    }

    /// Release every webview before Servo goes away.
    fn shutdown(&self) {
        // Saved first, while every tab is still there (marked clean: a normal exit).
        self.files_shutdown();
        self.agent.shutdown();
        // Cookie policy: "Clear on exit" sites, while Servo still runs.
        self.cookies_on_exit();
        let views: Vec<WebView> = {
            let mut tabs = self.tabs.borrow_mut();
            let ids: Vec<TabId> = tabs.iter().map(|t| t.id).collect();
            ids.into_iter()
                .filter_map(|id| tabs.close(id).flatten())
                .collect()
        };
        drop(views);
        drop(self.chrome.borrow_mut().take());
        info!(
            "exiting; content blocker cancelled {} requests",
            self.blocked_requests.get()
        );
    }

    // ------------------------------------------------------------- resource loading

    fn serve(&self, load: WebResourceLoad, status: StatusCode, mime: &'static str, body: Vec<u8>) {
        let url = load.request().url.clone();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(mime));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let reason = status.canonical_reason().unwrap_or("").as_bytes().to_vec();
        let mut intercepted = load.intercept(
            WebResourceResponse::new(url)
                .headers(headers)
                .status_code(status)
                .status_message(reason),
        );
        intercepted.send_body_data(body);
        intercepted.finish();
    }

    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        // A form the page posts to another site while Myth works in the tab: held at the
        // approval card before it is sent (see `agent_hold_post`).
        let Some(load) = self.agent_hold_post(&webview, load) else {
            return;
        };
        let request = load.request();
        let url = request.url.clone();
        // The agent's background tabs never get internal pages (or their RPC token).
        if chrome::is_internal(&url) && self.agent.owns(&webview) {
            warn!("refused internal request from an agent tab: {url}");
            load.intercept(WebResourceResponse::new(url).status_code(StatusCode::FORBIDDEN))
                .cancel();
            return;
        }
        let secret = request
            .headers
            .get(mythbrowse_engine::agent_api::SECRET_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(Secret::new);
        let route = chrome::route(
            &url,
            request.is_for_main_frame,
            self.is_chrome(&webview),
            &self.token,
        );
        match route {
            Route::PassThrough
                if request.is_for_main_frame && self.files_takeover(&webview, &url) =>
            {
                // A file: the download manager fetches it (once); the navigation ends here
                // and the tab keeps its page (or closes, if it was opened for the file).
                self.serve(load, StatusCode::NO_CONTENT, "text/plain", Vec::new());
            }
            Route::PassThrough => {
                // Built-in content blocker: cancel subresource loads to blocked hosts before
                // they hit the network. Top-level navigations are never blocked.
                let blocked = !request.is_for_main_frame
                    && url
                        .host_str()
                        .is_some_and(|host| self.blocker.is_blocked(host));
                if blocked {
                    self.blocked_requests.set(self.blocked_requests.get() + 1);
                    log::debug!("blocked {url}");
                    load.intercept(WebResourceResponse::new(url)).cancel();
                } else if !request.is_for_main_frame
                    && !self.is_chrome(&webview)
                    && let Some(rule) = self.compat_blocks(&webview, &url)
                {
                    log::debug!("compat rule {rule} blocked {url}");
                    load.intercept(WebResourceResponse::new(url)).cancel();
                }
                // Not intercepted: dropping `load` lets Servo continue normally.
            }
            Route::Asset(path) => match chrome::asset(&path, self.ui_dir.as_deref()) {
                Some((mime, body)) => {
                    let body = chrome::render_asset(mime, body, &self.token, self.theme_hour);
                    let body = chrome::fill_glass_mode(mime, body, self.glass_mode.get());
                    let (transparency, reduced) = self.look_now();
                    let body = chrome::fill_appearance(mime, body, transparency, reduced);
                    self.serve(load, StatusCode::OK, mime, body);
                }
                None => self.serve(
                    load,
                    StatusCode::NOT_FOUND,
                    "text/plain",
                    b"not found".to_vec(),
                ),
            },
            Route::Api(ApiCall::Glass {
                report,
                hour,
                moving,
            }) => {
                self.glass_report(&webview, &report, hour.as_deref(), moving);
                let body = chrome::json_ok(json!({ "mode": self.glass_mode.get().as_str() }));
                self.serve(load, StatusCode::OK, "application/json", body.into_bytes());
            }
            Route::Api(call) => {
                let (status, body) = self.handle_api(call, secret);
                self.serve(load, status, "application/json", body.into_bytes());
            }
            Route::BadRequest(message) => self.serve(
                load,
                StatusCode::BAD_REQUEST,
                "application/json",
                chrome::json_error(&message).into_bytes(),
            ),
            Route::Deny => {
                warn!("refused internal request {url}");
                load.intercept(WebResourceResponse::new(url).status_code(StatusCode::FORBIDDEN))
                    .cancel();
            }
            Route::NotFound => self.serve(
                load,
                StatusCode::NOT_FOUND,
                "text/plain",
                b"not found".to_vec(),
            ),
        }
    }
}

/// The `WebViewDelegate` for every webview. Holds a weak link so webviews (owned by
/// `AppState`) do not keep `AppState` alive.
struct Delegate(Weak<AppState>);

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        if let Some(s) = self.0.upgrade() {
            if s.agent_frame_ready(&webview) {
                return; // agent tabs are never on screen
            }
            // The page is part of the glass backdrop: its frames count `page_gen`, which
            // re-blurs only glass that reads the page.
            s.redraw_view(s.is_chrome(&webview));
        }
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        let Some(s) = self.0.upgrade() else { return };
        if s.is_chrome(&webview) {
            return;
        }
        // A page Myth opened or works in: recorded with its conversation, so the history page
        // links back to the chat.
        let myth_conv = s.agent.conv_key_of_view(&webview);
        if s.agent.owns(&webview) {
            if let Some(key) = myth_conv.flatten()
                && s.record_page(&url, "")
                && let Err(e) = s.store.link_history_conv(url.as_str(), &key)
            {
                warn!("history write failed: {e}");
            }
            return;
        }
        let (title, is_active, id) = {
            let mut tabs = s.tabs.borrow_mut();
            let active = tabs.active_id();
            match tabs.find_mut(|v| *v == webview) {
                Some(tab) => {
                    tab.url = url.to_string();
                    (tab.title.clone(), Some(tab.id) == active, tab.id)
                }
                None => return,
            }
        };
        if chrome::is_internal(&url) {
            // Myth never works in browser pages.
            s.agent_unbind_user_tab(id);
        }
        s.ui_tab_navigated(id);
        s.assist_tab_navigated(id);
        s.browse_url_changed(id, &webview, &url);
        if s.record_page(&url, &title)
            && let Some(key) = myth_conv.flatten()
            && let Err(e) = s.store.link_history_conv(url.as_str(), &key)
        {
            warn!("history write failed: {e}");
        }
        if is_active {
            s.refresh_window_title();
            // Internal page <-> web page: the page becomes / stops being a floating card.
            s.layout(false);
        }
        s.mark_chrome_dirty();
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        let Some(s) = self.0.upgrade() else { return };
        let title = title.unwrap_or_default();
        if s.agent.owns(&webview) {
            // Myth's background tab: only its history row learns the title.
            if !s.agent.private
                && let Some(url) = webview.url()
                && let Err(e) = s.store.set_title(url.as_str(), &title)
            {
                warn!("history title update failed: {e}");
            }
            return;
        }
        let url = {
            let mut tabs = s.tabs.borrow_mut();
            match tabs.find_mut(|v| *v == webview) {
                Some(tab) => {
                    tab.title = title.clone();
                    tab.url.clone()
                }
                None => return,
            }
        };
        if !s.agent.private
            && let Err(e) = s.store.set_title(&url, &title)
        {
            warn!("history title update failed: {e}");
        }
        s.refresh_window_title();
        s.mark_chrome_dirty();
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        let Some(s) = self.0.upgrade() else { return };
        if s.is_chrome(&webview) {
            if status == LoadStatus::Complete {
                s.mark_chrome_dirty();
            }
            return;
        }
        s.compat_load_status(&webview, status);
        if status == LoadStatus::Started {
            // A new document: its wheel listeners report again.
            s.wheel_listeners.borrow_mut().remove(&webview.id());
            if s.active_view().as_ref() == Some(&webview) {
                s.autoscroll_stop();
            }
        }
        if status == LoadStatus::Complete {
            // A run paused on a human-verification check in this page looks again now.
            s.agent_load_kick(&webview);
        }
        let Some(id) = s.tab_id_of(&webview) else {
            return;
        };
        match status {
            LoadStatus::Started => {
                let now = s.now_ms();
                s.load_watch.borrow_mut().insert(id, LoadWatch::new(now));
            }
            LoadStatus::Complete => {
                s.load_watch.borrow_mut().remove(&id);
            }
            _ => {}
        }
        let restore = {
            let mut tabs = s.tabs.borrow_mut();
            let Some(tab) = tabs.get_mut(id) else { return };
            tab.loading = status != LoadStatus::Complete;
            if status == LoadStatus::Complete {
                tabs.take_pending_restore(id)
            } else {
                None
            }
        };
        if let Some(pos) = restore {
            webview.evaluate_javascript(format!("window.scrollTo({}, {});", pos.x, pos.y), |_| {});
        }
        if status == LoadStatus::Complete {
            s.read_page_tint(id, &webview);
            s.files_load_complete(&webview);
        }
        s.mark_chrome_dirty();
    }

    fn notify_history_changed(&self, webview: WebView, entries: Vec<Url>, current: usize) {
        if let Some(s) = self.0.upgrade() {
            // Kept per tab so Back / Forward survive sleeping and restarts.
            if let Some(tab) = s.tabs.borrow_mut().find_mut(|v| *v == webview) {
                tab.set_history(entries.iter().map(Url::to_string).collect(), current);
            }
            s.mark_chrome_dirty();
        }
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        let Some(s) = self.0.upgrade() else { return };
        if s.autoscroll.borrow().is_some() {
            return; // the autoscroll cursor stays until it ends
        }
        if s.view_for(s.hover.get()).as_ref() == Some(&webview) {
            s.window.set_cursor(keys::to_cursor_icon(cursor));
        }
    }

    /// Put the IME candidate window next to the focused field: its position is in the
    /// webview's coordinates, so offset it by where that webview sits in the window.
    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        let Some(s) = self.0.upgrade() else { return };
        let control = match control {
            EmbedderControl::FilePicker(picker) => {
                if s.is_chrome(&webview) {
                    picker.dismiss();
                } else {
                    s.files_picker(&webview, picker);
                }
                return;
            }
            EmbedderControl::ContextMenu(menu) => {
                s.ui_context_menu(webview, menu);
                return;
            }
            EmbedderControl::SimpleDialog(dialog) => {
                s.ui_simple_dialog(webview, dialog);
                return;
            }
            other => other,
        };
        if let EmbedderControl::InputMethod(ime) = control {
            let p = ime.position();
            let local = PxRect::new(p.min.x, p.min.y, p.width(), p.height());
            let at = if s.is_chrome(&webview) {
                local
            } else if s.active_view().as_ref() == Some(&webview) {
                s.layout.get().content_to_window(local)
            } else {
                return;
            };
            s.window.set_ime_cursor_area(
                PhysicalPosition::new(at.x, at.y),
                PhysicalSize::new(at.w.max(1) as u32, at.h.max(1) as u32),
            );
            // Then refine it to the caret itself.
            s.ime_place_at_caret(&webview);
        }
    }

    fn hide_embedder_control(&self, _webview: WebView, control_id: EmbedderControlId) {
        if let Some(s) = self.0.upgrade() {
            s.files_hide_control(control_id);
            s.ui_hide_control(control_id);
        }
    }

    fn request_permission(&self, webview: WebView, request: PermissionRequest) {
        match self.0.upgrade() {
            Some(s) => s.ui_permission(webview, request),
            None => request.deny(),
        }
    }

    fn request_unload(&self, webview: WebView, request: AllowOrDenyRequest) {
        match self.0.upgrade() {
            Some(s) => s.ui_unload(webview, request),
            None => request.allow(),
        }
    }

    fn notify_crashed(&self, webview: WebView, reason: String, backtrace: Option<String>) {
        match self.0.upgrade() {
            Some(s) => s.compat_crashed(&webview, &reason, backtrace.as_deref()),
            None => warn!("web content crashed: {reason}"),
        }
    }

    /// A page navigates by itself (script, form, meta refresh). In tabs Myth works in during
    /// a run this goes through the navigation guard first (`agent_request_navigation`); all
    /// other navigations are allowed as before.
    /// A page navigation: a file-like URL becomes a download (`files_host`; in a page Myth
    /// controls that asks on the run's card), anything else in a tab Myth works in goes
    /// through the page navigation guard (`agent_host`).
    fn request_navigation(&self, webview: WebView, request: NavigationRequest) {
        match self.0.upgrade() {
            // mailto: / tel: / app links: never loaded; the OS or a note (`browse_host`).
            Some(s) if !s.is_chrome(&webview) && s.browse_scheme(&webview, &request.url) => {
                request.deny();
            }
            Some(s) if !s.is_chrome(&webview) => {
                if let Some(request) = s.files_navigation(&webview, request) {
                    s.agent_request_navigation(&webview, request);
                }
            }
            _ => request.allow(),
        }
    }

    /// `window.open` / `target=_blank`: open as a new background-less (active) tab.
    fn request_create_new(&self, parent: WebView, request: CreateNewWebViewRequest) {
        let Some(s) = self.0.upgrade() else { return };
        if s.is_chrome(&parent) {
            return;
        }
        let Err(request) = s.agent_popup(&parent, request) else {
            return; // opened as another hidden agent tab
        };
        // Pop-up blocker: only right after the user's click / key press, or when the site
        // is allowed. Dropping the request makes `window.open` return null.
        if !s.ui_popup_allowed(&parent) {
            drop(request);
            return;
        }
        s.deactivate_current();
        let view = request
            .builder(s.content_ctx.clone())
            .hidpi_scale_factor(Scale::new(s.window.scale_factor() as f32))
            .delegate(s.delegate.clone())
            .user_content_manager(s.compat.user_content.clone())
            .build();
        let url = view.url().map(|u| u.to_string()).unwrap_or_default();
        let opener = s.tab_id_of(&parent);
        let popup = s.tabs.borrow_mut().open(view, url, s.now_ms());
        s.browse_popup_opened(popup, opener);
        s.set_focus(Area::Content);
        s.show_active();
    }

    /// The page closed its own window (`window.close()`, script-closable per Servo): an
    /// OAuth / sign-in pop-up that is done. The tab closes and its opener comes back.
    fn notify_closed(&self, webview: WebView) {
        if let Some(s) = self.0.upgrade() {
            s.browse_page_closed(&webview);
        }
    }

    /// Fullscreen API: the window goes borderless fullscreen with the page filling it.
    fn notify_fullscreen_state_changed(&self, webview: WebView, fullscreen: bool) {
        if let Some(s) = self.0.upgrade() {
            s.browse_fullscreen_changed(&webview, fullscreen);
        }
    }

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        // Password page-script messages can carry a submitted password: handled here first,
        // never logged or passed to the compat detector.
        if mythbrowse_passwords::page::is_pw_message(&message) {
            if let Some(s) = self.0.upgrade() {
                s.pw_console(&webview, &message);
            }
            return;
        }
        // Selection pill: box + length only (never the text), see `assist_host`.
        if let Some(r) = page_input::parse_sel_report(&message) {
            if let Some(s) = self.0.upgrade() {
                s.assist_sel_report(&webview, r);
            }
            return;
        }
        if let Some(req) = page_input::parse_open_request(&message) {
            if let Some(s) = self.0.upgrade() {
                s.open_link_request(&webview, req);
            }
            return;
        }
        if let Some(r) = page_input::parse_wheel_report(&message) {
            if let Some(s) = self.0.upgrade() {
                s.wheel_listener_report(&webview, r);
            }
            return;
        }
        if let Some(url) = page_input::parse_external_request(&message) {
            if let Some(s) = self.0.upgrade() {
                s.browse_scheme(&webview, &url);
            }
            return;
        }
        if page_input::is_autoscroll_request(&message) {
            if let Some(s) = self.0.upgrade() {
                s.autoscroll_request(&webview);
            }
            return;
        }
        if let Some(s) = self.0.upgrade()
            && !s.is_chrome(&webview)
            && !s.files_console(&webview, &message)
        {
            s.compat_console(&webview, &level, &message);
        }
    }

    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        if let Some(s) = self.0.upgrade() {
            s.load_web_resource(webview, load);
        }
        // If the app is gone, dropping `load` lets Servo continue (it is shutting down).
    }
}

/// The saved motion setting (`<profile>/ui.json`); `None` (follow the OS) if missing,
/// unreadable or `system`.
fn load_motion(path: &std::path::Path) -> Option<Motion> {
    load_ui_pref(path, "motion").and_then(|m| Motion::parse(&m))
}

/// One string key of `<profile>/ui.json` (`None` if missing or unreadable).
fn load_ui_pref(path: &std::path::Path, key: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get(key).and_then(|m| m.as_str()).map(str::to_owned))
}

/// Build the liquid-glass compositor if everything it needs is there: the GL programs and
/// the colour textures of the chrome's and the page's offscreen framebuffers.
/// `MYTHBROWSE_GLASS=0` turns it off (comparison screenshots, troubleshooting).
fn build_glass(
    window_ctx: &WindowRenderingContext,
    chrome_ctx: &OffscreenRenderingContext,
    content_ctx: &OffscreenRenderingContext,
) -> Option<glass::Compositor> {
    if std::env::var("MYTHBROWSE_GLASS").is_ok_and(|v| v == "0") {
        info!("liquid glass: off (MYTHBROWSE_GLASS=0)");
        return None;
    }
    let gl = window_ctx.glow_gl_api();
    chrome_ctx.prepare_for_rendering();
    let chrome_tex = compose::bound_color_texture(&gl);
    content_ctx.prepare_for_rendering();
    let page_tex = compose::bound_color_texture(&gl);
    window_ctx.prepare_for_rendering();
    if chrome_tex == 0 || page_tex == 0 {
        warn!("liquid glass: off (offscreen framebuffers have no colour texture)");
        return None;
    }
    match glass::Compositor::new(&gl) {
        Ok(c) => {
            info!("liquid glass: shaders ready");
            Some(c)
        }
        Err(e) => {
            warn!("liquid glass: off ({e})");
            None
        }
    }
}

/// The OS asks for solid surfaces: Windows Settings > Personalization > Colors >
/// "Transparency effects" off (`HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\
/// Personalize\EnableTransparency` = 0; one registry read at start), or
/// `MYTHBROWSE_REDUCED_TRANSPARENCY=1`. Used when the user has not set Reduce transparency.
/// Not followed live: a change in Windows applies at the next start.
fn os_reduced_transparency() -> bool {
    if std::env::var("MYTHBROWSE_REDUCED_TRANSPARENCY").is_ok_and(|v| v == "1") {
        return true;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW,
        };
        let wide = |s: &str| {
            s.encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let name = wide("EnableTransparency");
        let mut value: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call; RRF_RT_REG_DWORD
        // writes at most `size` (4) bytes into `value`; no type out-parameter is requested.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            )
        };
        if status == 0 {
            return value == 0;
        }
    }
    false
}

/// The OS asks for fewer animations: Windows "Animation effects" off
/// (`SPI_GETCLIENTAREAANIMATION`), GNOME `enable-animations` false (`gsettings`), or
/// `MYTHBROWSE_REDUCED_MOTION=1`. Used when the user has not picked a Motion setting.
fn os_reduced_motion() -> bool {
    if std::env::var("MYTHBROWSE_REDUCED_MOTION").is_ok_and(|v| v == "1") {
        return true;
    }
    // GNOME (and most GTK desktops): Settings > Accessibility > Reduce animation.
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "enable-animations"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(out) = out
            && out.status.success()
            && String::from_utf8_lossy(&out.stdout).trim() == "false"
        {
            return true;
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut on: i32 = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the pointer we pass.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut on as *mut i32).cast(),
                0,
            )
        };
        if ok != 0 {
            return on == 0;
        }
    }
    false
}

/// The GL implementation the window context runs on (`GL_VENDOR` / `GL_RENDERER` /
/// `GL_VERSION`). With ANGLE the renderer names the D3D11 adapter, e.g.
/// "ANGLE (Intel, Intel(R) UHD Graphics 620 Direct3D11 vs_5_0 ps_5_0, D3D11)".
#[derive(Clone, Debug, Default)]
pub(crate) struct GpuInfo {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    /// A CPU rasterizer (WARP / Microsoft Basic Render Driver, llvmpipe, softpipe, SwiftShader).
    pub software: bool,
}

impl GpuInfo {
    pub fn summary(&self) -> String {
        format!(
            "{} | {} | {}{}",
            self.vendor,
            self.renderer,
            self.version,
            if self.software {
                " | software"
            } else {
                " | hardware"
            }
        )
    }
}

fn gpu_info(gl: &::glow::Context) -> GpuInfo {
    use ::glow::HasContext;
    // SAFETY: plain string queries on the current context.
    let (vendor, renderer, version) = unsafe {
        (
            gl.get_parameter_string(::glow::VENDOR),
            gl.get_parameter_string(::glow::RENDERER),
            gl.get_parameter_string(::glow::VERSION),
        )
    };
    let software = is_software_renderer(&renderer);
    GpuInfo {
        vendor,
        renderer,
        version,
        software,
    }
}

/// Whether a `GL_RENDERER` string names a CPU rasterizer.
fn is_software_renderer(renderer: &str) -> bool {
    let r = renderer.to_ascii_lowercase();
    [
        "basic render driver",
        "warp",
        "llvmpipe",
        "softpipe",
        "swiftshader",
        "software rasterizer",
    ]
    .iter()
    .any(|k| r.contains(k))
}

#[cfg(test)]
mod gpu_info_tests {
    use super::is_software_renderer;

    #[test]
    fn software_renderers_are_recognised() {
        assert!(is_software_renderer(
            "ANGLE (Microsoft, Microsoft Basic Render Driver Direct3D11 vs_5_0 ps_5_0, D3D11)"
        ));
        assert!(is_software_renderer("llvmpipe (LLVM 19.1.7, 128 bits)"));
        assert!(is_software_renderer("Google SwiftShader"));
        assert!(!is_software_renderer(
            "ANGLE (Intel, Intel(R) UHD Graphics 620 Direct3D11 vs_5_0 ps_5_0, D3D11)"
        ));
        assert!(!is_software_renderer(
            "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)"
        ));
    }
}
