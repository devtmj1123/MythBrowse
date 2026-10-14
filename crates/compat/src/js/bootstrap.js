// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// MythBrowse compat bootstrap. The first Servo user script in every web page (see
// crates/compat/src/bundle.rs). It
//   * records what the detector needs, cheaply and passively: uncaught errors, unhandled
//     rejections, failed subresources, and which web APIs are missing natively (checked
//     before any polyfill rule runs);
//   * gives rule scripts their helpers (host matching, CSS injection, UA override, modes).
// Everything lives on one frozen, non-enumerable window property whose name is random per
// browser session (__MB_KEY__ is replaced when the bundle is built). No timers, no polling.
(function () {
  "use strict";
  var KEY = "__MB_KEY__";
  if (location.protocol !== "http:" && location.protocol !== "https:") return;
  if (location.hostname === "mythbrowse.internal") return;
  if (Object.prototype.hasOwnProperty.call(window, KEY)) return;

  var MAX = 20;
  var S = {
    errors: [], errorCount: 0, rejections: 0,
    resources: [], resourceCount: 0,
    missing: [], ran: [], failed: [], ua: null, mode: null,
  };
  function clip(s, n) {
    s = String(s == null ? "" : s);
    return s.length > n ? s.slice(0, n - 1) + "\u2026" : s;
  }
  function push(list, item) { if (list.length < MAX) list.push(item); }

  // Web APIs whose absence commonly breaks sites. Checked once, before polyfills.
  var EP = typeof Element !== "undefined" ? Element.prototype : {};
  var checks = [
    ["IntersectionObserver", function () { return "IntersectionObserver" in window; }],
    ["ResizeObserver", function () { return "ResizeObserver" in window; }],
    ["MutationObserver", function () { return "MutationObserver" in window; }],
    ["PerformanceObserver", function () { return "PerformanceObserver" in window; }],
    ["requestIdleCallback", function () { return "requestIdleCallback" in window; }],
    ["structuredClone", function () { return "structuredClone" in window; }],
    ["queueMicrotask", function () { return "queueMicrotask" in window; }],
    ["BroadcastChannel", function () { return "BroadcastChannel" in window; }],
    ["AbortController", function () { return "AbortController" in window; }],
    ["TextEncoder", function () { return "TextEncoder" in window; }],
    ["crypto.randomUUID", function () { return !!(window.crypto && crypto.randomUUID); }],
    ["Element.animate", function () { return "animate" in EP; }],
    ["Animation", function () { return typeof window.Animation === "function"; }],
    ["Element.checkVisibility", function () { return "checkVisibility" in EP; }],
    ["Element.getAnimations", function () { return "getAnimations" in EP; }],
    ["document.adoptedStyleSheets", function () { return "adoptedStyleSheets" in document; }],
    ["Element.replaceChildren", function () { return "replaceChildren" in EP; }],
    ["Element.attachShadow", function () { return "attachShadow" in EP; }],
    ["document.fonts", function () { return !!document.fonts; }],
    ["document.fonts.load", function () { return !!(document.fonts && typeof document.fonts.load === "function"); }],
    ["FontFace", function () { return typeof window.FontFace === "function"; }],
    ["visualViewport", function () { return typeof window.visualViewport === "object" && !!window.visualViewport; }],
    ["popover", function () { return typeof HTMLElement !== "undefined" && "popover" in HTMLElement.prototype; }],
    ["OffscreenCanvas", function () { return typeof window.OffscreenCanvas === "function"; }],
    ["navigator.clipboard", function () { return !!navigator.clipboard; }],
    ["navigator.serviceWorker", function () { return "serviceWorker" in navigator; }],
    ["indexedDB", function () { return "indexedDB" in window; }],
    ["customElements", function () { return "customElements" in window; }],
    ["matchMedia", function () { return "matchMedia" in window; }],
    ["CSS.escape", function () { return !!(window.CSS && CSS.escape); }],
    ["Intl.Segmenter", function () { return !!(window.Intl && Intl.Segmenter); }],
    ["Intl.RelativeTimeFormat", function () { return !!(window.Intl && Intl.RelativeTimeFormat); }],
    ["WebAssembly", function () { return "WebAssembly" in window; }],
    ["Worker", function () { return "Worker" in window; }],
    ["CompressionStream", function () { return "CompressionStream" in window; }],
    ["Notification", function () { return "Notification" in window; }],
    ["AudioContext.createDynamicsCompressor", function () {
      var A = window.BaseAudioContext || window.AudioContext || window.OfflineAudioContext;
      return !A || typeof A.prototype.createDynamicsCompressor === "function";
    }],
    ["SVGGraphicsElement.getBBox", function () {
      var P = window.SVGGraphicsElement || window.SVGElement;
      return !P || typeof P.prototype.getBBox === "function";
    }],
    ["console.table", function () { return !!window.console && typeof console.table === "function"; }],
    ["performance.measure(navigation timing name)", function () {
      // Measures from "fetchStart" etc. (spec-valid); the check entry is removed at once.
      if (!window.performance || typeof performance.measure !== "function") return true;
      try { performance.measure("__mb_nav_check", "fetchStart"); } catch (e) { return false; }
      try { performance.clearMeasures("__mb_nav_check"); } catch (e) {}
      return true;
    }],
    ["Promise.withResolvers", function () { return typeof Promise.withResolvers === "function"; }],
  ];
  for (var i = 0; i < checks.length; i++) {
    var ok = false;
    try { ok = !!checks[i][1](); } catch (e) { ok = false; }
    if (!ok) S.missing.push(checks[i][0]);
  }

  // Uncaught errors and failed subresources (resource "error" events do not bubble, but the
  // capture phase on window sees them).
  window.addEventListener("error", function (ev) {
    var t = ev.target;
    if (t && t !== window && t.nodeType === 1) {
      S.resourceCount++;
      push(S.resources, clip(String(t.tagName || "").toLowerCase() + " " +
        (t.currentSrc || t.src || t.href || ""), 200));
      return;
    }
    S.errorCount++;
    push(S.errors, { m: clip(ev.message, 300), s: clip(ev.filename, 200), l: ev.lineno | 0 });
  }, true);
  window.addEventListener("unhandledrejection", function (ev) {
    S.rejections++;
    var r = ev.reason;
    push(S.errors, { m: clip("Unhandled rejection: " + (r && r.message ? r.message : r), 300), s: "", l: 0 });
  });

  // Host / path matching: same semantics as crates/compat/src/matcher.rs.
  function hostMatch(p, host) {
    if (!host || host === "mythbrowse.internal") return false;
    if (p === "*") return true;
    function subOf(d) {
      return host.length > d.length && host.slice(-d.length) === d &&
        host.charAt(host.length - d.length - 1) === ".";
    }
    if (p.charAt(0) === "=") return host === p.slice(1);
    if (p.slice(0, 2) === "*.") return subOf(p.slice(2));
    return host === p || subOf(p);
  }
  function glob(p, t) {
    var pi = 0, ti = 0, sp = -1, st = 0;
    while (ti < t.length) {
      if (pi < p.length && p.charAt(pi) === "*") { sp = pi; st = ti; pi++; }
      else if (pi < p.length && p.charAt(pi) === t.charAt(ti)) { pi++; ti++; }
      else if (sp >= 0) { pi = sp + 1; st++; ti = st; }
      else return false;
    }
    while (pi < p.length && p.charAt(pi) === "*") pi++;
    return pi === p.length;
  }
  function any(list, f) { for (var k = 0; k < list.length; k++) if (f(list[k])) return true; return false; }

  var applyMode = __MB_READER__;

  var C = {
    match: function (hosts, excl, paths) {
      var h = location.hostname;
      if (!any(hosts, function (p) { return hostMatch(p, h); })) return false;
      if (any(excl, function (p) { return hostMatch(p, h); })) return false;
      return !paths.length || any(paths, function (p) { return glob(p, location.pathname); });
    },
    ran: function (id) { push(S.ran, id); },
    fail: function (id, e) {
      push(S.failed, id);
      push(S.errors, { m: clip("[compat rule " + id + "] " + (e && e.message ? e.message : e), 300), s: "", l: 0 });
    },
    css: function (id, text) {
      var st = document.createElement("style");
      st.setAttribute("data-mb-compat", id);
      st.textContent = text;
      (document.head || document.documentElement).appendChild(st);
    },
    // Per-site UA (compat rule `ua`): what the page's scripts see. The HTTP header keeps the
    // global choice (Servo 0.1.4 has one UA pref per process). Only these three values.
    ua: function (ua, appVersion, platform) {
      S.ua = ua;
      var NP = Navigator.prototype;
      function def(name, value) {
        try { Object.defineProperty(NP, name, { get: function () { return value; }, configurable: true }); } catch (e) { /* keep native */ }
      }
      def("userAgent", ua);
      def("appVersion", appVersion || ua.replace(/^Mozilla\//, ""));
      if (platform) def("platform", platform);
    },
    mode: function (m) {
      S.mode = m;
      if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", function () { applyMode(m); }, { once: true });
      } else {
        applyMode(m);
      }
    },
    applyMode: function (m) { return applyMode(m); },
    state: function () { return S; },
  };
  Object.defineProperty(window, KEY, { value: Object.freeze(C), enumerable: false, configurable: false, writable: false });
})();
