# Everyday browsing (v0.4.2)

Behaviours every browser has that Servo 0.1.4 leaves to the embedder. Pure logic and its unit
tests: `crates/engine/src/browse.rs`; shell side: `crates/shell/src/browse_host.rs`; page script
parts: `crates/engine/src/js/page_input.js`; chrome UI: `ui/chrome.{html,js,css}`; UI smoke:
`tests/ui/browse_smoke.mjs` (headless Chromium, not Servo).

## Pop-ups that close themselves (OAuth sign-in)

"Sign in with Google / GitHub / Microsoft" opens a pop-up (`window.open`, allowed right after
the click by the pop-up blocker), which posts the result back with `window.opener.postMessage`
and calls `window.close()`.

* Servo 0.1.4 checks "script-closable" itself (opened by script, or a one-entry history) and
  then calls `WebViewDelegate::notify_closed`. The shell closes that tab and, when it was in
  front, brings back the tab that opened it (remembered in `request_create_new`).
* A tab with an open pop-up is never put to sleep (`maybe_sleep_tabs`): sleeping drops its
  webview and the pop-up's `window.opener` with it.
* `window.opener` itself is Servo's: the pop-up is built from `CreateNewWebViewRequest`, so it
  shares the opener's browsing context group.
* Needs a real-device check: GitHub / Google sign-in on a site that uses a pop-up flow.

## Page zoom

* Ctrl + (also Ctrl =), Ctrl -, Ctrl 0, and Ctrl+wheel over the page (one step per mouse-wheel
  notch; touchpads every 120 px). Steps: 30 % … 300 % (Firefox's list, `ZOOM_LEVELS`).
* `WebView::set_page_zoom` (layout zoom: the page reflows, `devicePixelRatio` changes).
* Remembered per site (registrable domain, pref `zoom_sites` in the profile store, at most 500
  sites; 100 % forgets the site) and applied whenever a tab lands on that site.
* Address bar: a "110%" chip when the page is not at 100 %; click resets.
* Web pages only (internal pages stay at 100 %).

## mailto:, tel:, sms: and app links

* Clicks on links whose scheme the engine cannot load are caught twice: the page script reports
  a trusted, un-cancelled click (`\u0001mythbrowse:external {...}`), and `request_navigation`
  denies any such navigation.
* `mailto:`, `tel:`, `sms:`, `callto:` go to the OS (`mythbrowse_files::os::open_url`:
  `ShellExecuteW("open")` / `xdg-open`) only within 1 s of the user's own click / key press in
  that page, once per click; a short note says "Opening your email app…". The URL is checked
  again in `files` (scheme list, ≤ 2048 bytes, no control characters, quotes or backslashes).
* Every other scheme (`zoommtg:`, `msteams:`, `ms-settings:` …) is refused with a note
  ("MythBrowse can't open zoommtg: links yet"). No "open this app?" prompt exists yet.
* Never for Myth's agent tabs or the chrome. Without a user click: ignored and logged.

## Find in page (Ctrl+F)

* Servo has no find API, so the page script searches: `window.__mbFind(query, dir)` walks the
  rendered text nodes (case-insensitive, within one text node, up to 1000 matches; hidden
  elements, `<script>` and `<style>` text are skipped), paints every match in an overlay
  element (`<mb-find>`, pointer-events none; current match orange), scrolls the current one
  into view and answers `"current/total"`. `__mbFindClear()` removes the overlay. The page's
  own DOM is not changed.
* The find bar sits in the toolbar next to the address bar (never over the page): typing
  searches (debounced), Enter / Shift+Enter or F3 / Shift+F3 = next / previous, Esc or × closes
  and clears the highlights. Typing more letters keeps the place.
* Limits: a match that spans two elements (`<b>ser</b>vo`) is not found; text inside iframes is
  not searched; positions of matches inside inner scroll areas are refreshed on scroll.

## Fullscreen API

* `WebViewDelegate::notify_fullscreen_state_changed`: the window goes borderless fullscreen
  (`winit Fullscreen::Borderless`) and the page fills it (the layout drops the chrome bar and
  the card frame; the chrome page hides its bars).
* Esc leaves (shell first, then `WebView::exit_fullscreen` for the page); so do switching or
  closing the tab, and the page's own `document.exitFullscreen()`.
* Only the tab in front may go fullscreen; background and agent tabs are refused.
* Note: video playback itself is not supported in this build (no media backend), so fullscreen
  is mostly for games, slides and maps.
