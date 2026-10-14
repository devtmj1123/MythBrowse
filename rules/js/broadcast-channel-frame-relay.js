// MythBrowse compat: BroadcastChannel relay for redirected frames
// (rule site.broadcast-channel-frame-relay).
// Servo bug (servo-constellation 0.1.4 get_or_create_event_loop_for_new_pipeline): an iframe's
// event loop is picked from the URL it *starts* at. When that URL redirects back to the parent's
// origin (the OAuth silent sign-in: login.microsoftonline.com -> login.live.com ->
// outlook.live.com/mail/oauthRedirect.html), the same-origin document still lives in the other
// site's event loop: the parent gets a cross-origin WindowProxy (SecurityError on
// iframe.contentWindow.location) and BroadcastChannel messages from the frame never reach it.
// MSAL.js v5 hands the sign-in response back exactly that way, so Outlook's silent sign-in timed
// out after 10 s every time.
// Fix: in a frame whose parent it cannot reach (cross-origin, or this engine bug), every
// BroadcastChannel message is also sent with postMessage to parent and top, restricted to this
// document's own origin, so a truly cross-origin parent never receives it. A top or parent
// document of the same origin re-broadcasts it on a channel of the same name, which delivers
// it to the page's own BroadcastChannel listeners. Frames whose parent is reachable are left
// alone (BroadcastChannel already works there, so nothing is delivered twice).
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (typeof window.BroadcastChannel === "function" && !window.__mbBcRelay) {
  Object.defineProperty(window, "__mbBcRelay", { value: true });
  var BC = window.BroadcastChannel;
  var TAG = "__mbBroadcastRelay";
  var origin = location.origin;
  var seen = Object.create(null);

  var parentUnreachable = function () {
    if (window.parent === window) return false;
    try { void window.parent.location.href; return false; } catch (e) { return true; }
  };

  if (parentUnreachable() && origin && origin !== "null") {
    var post = BC.prototype.postMessage;
    BC.prototype.postMessage = function (message) {
      var result = post.apply(this, arguments);
      try {
        var env = {};
        env[TAG] = 1;
        env.id = String(Math.random()).slice(2) + String(Date.now());
        env.name = String(this.name);
        env.data = message;
        var targets = [window.parent];
        if (window.top !== window.parent) targets.push(window.top);
        for (var i = 0; i < targets.length; i++) targets[i].postMessage(env, origin);
      } catch (e) { /* unclonable message: the native send above is all we can do */ }
      return result;
    };
  }

  window.addEventListener("message", function (e) {
    var d = e.data;
    if (!d || typeof d !== "object" || d[TAG] !== 1 || e.origin !== origin) return;
    if (typeof d.id !== "string" || typeof d.name !== "string" || seen[d.id]) return;
    seen[d.id] = 1;
    var ch = new BC(d.name);
    ch.postMessage(d.data);
    ch.close();
  });
}
