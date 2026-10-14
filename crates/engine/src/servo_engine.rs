// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Thin wrapper over the `servo` crate (0.1.x LTS embedding API:
//! `ServoBuilder` -> `Servo`, `WebViewBuilder` -> `WebView`, `WebViewDelegate`).
//! Modelled on the crate's own `examples/winit_minimal.rs`.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub use euclid;
pub use servo;
use servo::{
    EventLoopWaker, Preferences, RenderingContext, Servo, ServoBuilder, UserContentManager,
    WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

/// An [`EventLoopWaker`] that calls a closure. Servo calls `wake` from any thread when
/// it needs the embedder to run [`Engine::spin`] on the main thread.
#[derive(Clone)]
pub struct CallbackWaker(Arc<dyn Fn() + Send + Sync>);

impl CallbackWaker {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }
}

impl EventLoopWaker for CallbackWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        (self.0)()
    }
}

/// One Servo instance. Must live on the thread that created it.
/// Servo preferences MythBrowse changes from Servo's defaults.
///
/// - `layout_grid_enabled`: CSS grid is behind a pref in Servo 0.1.x (off by default). Sites
///   that use grid fall back to block layout without it, and scripts reading
///   `getComputedStyle().gridTemplateColumns` throw. Top-sites run with grid on
///   (2026-10-09, 53 sites, docs/COMPAT.md): no site got worse; archive.org went from
///   degraded to OK, github.com lost its script errors.
///
/// - `shell_background_color_rgba`: with liquid glass on (`transparent`), every webview is
///   cleared to transparent so the shell can draw the sky and the blurred backdrop under the
///   UI pages and put web pages on their own white underlay (`crates/shell/src/glass.rs`).
///   Otherwise Servo's default opaque white.
///
/// - `user_agent`: the shell's choice (`mythbrowse_compat::ua`, default a plain Firefox desktop
///   UA without Servo's `Servo/x.y.z` token, which UA-sniffing sites refuse). Empty keeps
///   Servo's own. Only the UA string is chosen; nothing else is disguised.
fn engine_preferences(transparent: bool, user_agent: &str) -> Preferences {
    let mut prefs = Preferences {
        layout_grid_enabled: true,
        ..Preferences::default()
    };
    if !user_agent.is_empty() {
        prefs.user_agent = user_agent.to_owned();
    }
    if transparent {
        prefs.shell_background_color_rgba = [0.0, 0.0, 0.0, 0.0];
    }
    prefs
}

pub struct Engine {
    servo: Servo,
}

impl Engine {
    /// `transparent`: clear every webview to transparent instead of white (liquid glass).
    /// `user_agent`: the UA string for HTTP and `navigator.userAgent` (empty: Servo's own).
    /// `config_dir`: where Servo keeps the cookie jar, HSTS list and local / IndexedDB
    /// storage between runs (`None`, the default: nothing is written, everything ends with
    /// the session). Only set with the `persist_cookies` pref (docs/COOKIES.md); the shell
    /// installs the jar encryption first.
    pub fn new(
        waker: Box<dyn EventLoopWaker>,
        transparent: bool,
        user_agent: &str,
        config_dir: Option<PathBuf>,
    ) -> Self {
        let mut builder = ServoBuilder::default()
            .preferences(engine_preferences(transparent, user_agent))
            .event_loop_waker(waker);
        if let Some(dir) = config_dir {
            builder = builder.opts(servo::Opts {
                config_dir: Some(dir),
                ..servo::Opts::default()
            });
        }
        let servo = builder.build();
        servo.setup_logging();
        Self { servo }
    }

    pub fn servo(&self) -> &Servo {
        &self.servo
    }

    /// Change the UA for requests and `navigator.userAgent` from now on (pages loaded
    /// earlier keep what they saw until they reload).
    pub fn set_user_agent(&self, user_agent: &str) {
        self.servo
            .set_preference("user_agent", servo::PrefValue::Str(user_agent.to_owned()));
    }

    /// Let Servo process pending work. Call on every wake-up and window event.
    pub fn spin(&self) {
        self.servo.spin_event_loop();
    }

    /// Create a webview painting into `rendering_context`.
    pub fn new_webview(
        &self,
        rendering_context: Rc<dyn RenderingContext>,
        url: Url,
        hidpi_scale_factor: f32,
        delegate: Rc<dyn WebViewDelegate>,
    ) -> WebView {
        WebViewBuilder::new(&self.servo, rendering_context)
            .url(url)
            .hidpi_scale_factor(euclid::Scale::new(hidpi_scale_factor))
            .delegate(delegate)
            .build()
    }

    /// A user-content manager (user scripts / stylesheets) to share between web-page
    /// webviews. Changes take effect on each page's next load.
    pub fn new_user_content(&self) -> Rc<UserContentManager> {
        Rc::new(UserContentManager::new(&self.servo))
    }

    /// Like [`Engine::new_webview`], for web pages: with the shared user content (site
    /// compat scripts). The chrome and other internal pages use [`Engine::new_webview`].
    pub fn new_page_webview(
        &self,
        rendering_context: Rc<dyn RenderingContext>,
        url: Url,
        hidpi_scale_factor: f32,
        delegate: Rc<dyn WebViewDelegate>,
        user_content: Rc<UserContentManager>,
    ) -> WebView {
        WebViewBuilder::new(&self.servo, rendering_context)
            .url(url)
            .hidpi_scale_factor(euclid::Scale::new(hidpi_scale_factor))
            .delegate(delegate)
            .user_content_manager(user_content)
            .build()
    }
}
