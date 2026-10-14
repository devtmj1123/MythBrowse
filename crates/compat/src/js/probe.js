(function () {
  // SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
  // MythBrowse compat detector probe: evaluated ONCE by the shell when a page finishes
  // loading (and again only when the user asks to re-check). Reads what the bootstrap
  // recorded plus a few cheap layout facts, and returns one JSON string (parsed and scored in
  // crates/compat/src/detect.rs). No timers, no observers, nothing left behind.
  "use strict";
  var C = window["__MB_KEY__"];
  var S = C ? C.state() : null;
  function safe(f, d) { try { var v = f(); return v === undefined ? d : v; } catch (e) { return d; } }
  var doc = document, de = doc.documentElement, body = doc.body;
  var vw = innerWidth || 0, vh = innerHeight || 0;

  // innerText is the *rendered* text. An empty string is an answer, not a reason to fall back:
  // textContent also counts <noscript>, <script> and hidden text, so a JS app that never
  // rendered (bsky.app: an empty #root plus a <noscript> blurb) looked like a page with text.
  var text = safe(function () {
    if (!body) return "";
    return typeof body.innerText === "string" ? body.innerText : (body.textContent || "");
  }, "");
  var textLen = text.replace(/\s+/g, " ").trim().length;
  // Pages built from web components (msn.com: ~190 open shadow roots) keep their text inside
  // shadow trees, which body.innerText does not include, so they looked blank. When the light
  // DOM has little text, add the rendered text of open shadow roots (bounded walk).
  if (textLen < 500 && body) {
    safe(function () {
      var hosts = 0, extra = 0, stack = [body];
      while (stack.length && hosts < 400 && extra < 5000) {
        var all = stack.pop().querySelectorAll("*");
        for (var k = 0; k < all.length && hosts < 400; k++) {
          var sr = all[k].shadowRoot;
          if (!sr) continue;
          hosts++;
          for (var c = sr.firstElementChild; c; c = c.nextElementSibling) {
            if (c.localName === "style" || c.localName === "script" || c.localName === "link") continue;
            extra += (c.innerText || "").replace(/\s+/g, " ").trim().length;
          }
          stack.push(sr);
        }
      }
      textLen += extra;
    }, null);
  }

  // Sample a 4x4 grid: points that hit only <html>/<body> show nothing there.
  var samples = 0, blank = 0;
  for (var i = 1; i <= 4; i++) {
    for (var j = 1; j <= 4; j++) {
      samples++;
      var el = safe(function () { return doc.elementFromPoint(vw * i / 5, vh * j / 5); }, null);
      if (!el || el === de || el === body) blank++;
    }
  }
  var media = safe(function () { return doc.querySelectorAll("img,video,canvas,svg,iframe,picture").length; }, 0);
  var elements = safe(function () { return body ? body.getElementsByTagName("*").length : 0; }, 0);
  var bodyRect = safe(function () { return body.getBoundingClientRect(); }, null);
  var overflowHidden = safe(function () {
    return getComputedStyle(de).overflowY === "hidden" || getComputedStyle(body).overflowY === "hidden";
  }, false);

  // CSS features in common use that this engine may lack (informational).
  var CSS_CHECKS = [
    "display:grid", "display:flex", "position:sticky", "aspect-ratio:1",
    "container-type:inline-size", "selector(:has(a))", "selector(:is(a))", "selector(:where(a))",
    "inset:0", "backdrop-filter:blur(1px)", "color:oklch(0.5 0.1 120)",
    "color:color-mix(in srgb, red, blue)", "text-wrap:balance", "scroll-snap-type:x mandatory",
    "overflow:clip", "translate:1px", "content-visibility:auto"
  ];
  var cssMissing = [];
  for (var c = 0; c < CSS_CHECKS.length; c++) {
    var q = CSS_CHECKS[c];
    var ok = safe(function () {
      if (!window.CSS || !CSS.supports) return false;
      if (q.indexOf("selector(") === 0) return CSS.supports(q);
      var at = q.indexOf(":");
      return CSS.supports(q.slice(0, at), q.slice(at + 1));
    }, false);
    if (!ok) cssMissing.push(q);
  }

  // Unsupported API calls, from the error messages: "X is not defined",
  // "a.b is not a function / constructor", "a.b is undefined". A name counts when no
  // object a page would normally reach has it.
  var KNOWN = {
    animate: 1, fonts: 1, clipboard: 1, serviceWorker: 1, userAgentData: 1, scheduler: 1,
    share: 1, getAnimations: 1, showPicker: 1, showModal: 1, requestFullscreen: 1,
    checkVisibility: 1, replaceChildren: 1, structuredClone: 1, randomUUID: 1, hasOwn: 1,
    replaceAll: 1, findLast: 1, toSorted: 1, groupBy: 1, withResolvers: 1, adoptedStyleSheets: 1
  };
  var errors = S ? S.errors.slice() : [];
  var apiSeen = {}, api = [];
  for (var e = 0; e < errors.length; e++) {
    var m = String(errors[e].m || "");
    var r = /([A-Za-z_$][\w$.]*) is not (?:defined|a function|a constructor)/.exec(m) ||
      /([A-Za-z_$][\w$.]*) is (?:undefined|null)/.exec(m);
    if (!r) continue;
    var name = r[1].split(".").pop();
    if (!name || (name.length < 4 && !KNOWN[name]) || apiSeen[name]) continue;
    apiSeen[name] = 1;
    var known = safe(function () {
      return (name in window) || (name in doc) || (name in navigator) ||
        (typeof Element !== "undefined" && name in Element.prototype) ||
        (typeof HTMLElement !== "undefined" && name in HTMLElement.prototype);
    }, true);
    // camelCase / PascalCase identifiers only: minified names ("a", "t.n") are not APIs.
    if (!known && (KNOWN[name] || /^[a-z]+[A-Z][a-zA-Z]*$|^[A-Z][a-z]+[A-Za-z]*$/.test(name))) api.push(name);
  }

  // "Your browser is not supported" pages (UA sniffing). Only on short pages, so an
  // article that merely mentions browsers is not flagged.
  var uaBlock = null;
  if (textLen < 2500) {
    var low = text.toLowerCase();
    var PHRASES = [
      "browser is not supported", "browser isn't supported", "browser is no longer supported",
      "unsupported browser", "browser not supported", "upgrade your browser", "update your browser",
      "please use a supported browser", "switch to a supported browser", "use a different browser",
      "download chrome", "requires a modern browser", "your browser is out of date",
      "浏览器版本过低", "不支持您的浏览器", "不支持当前浏览器", "请升级浏览器", "请使用chrome", "浏览器不受支持",
      "pelayar anda tidak disokong"
    ];
    for (var p = 0; p < PHRASES.length; p++) {
      if (low.indexOf(PHRASES[p]) >= 0) { uaBlock = PHRASES[p]; break; }
    }
  }

  // Bot / security checks (Cloudflare "Just a moment", captchas, IP blocks). The site is
  // gating the visitor, not failing in this engine, so the detector reports it and stops.
  // Same detector as the agent's human-verification hand-off (js/challenge.js).
  var botCheck = safe(function () { return (__CHALLENGE__)(doc); }, null);

  return JSON.stringify({
    url: location.href,
    ready: doc.readyState,
    bootstrap: !!S,
    errors: errors,
    error_count: S ? S.errorCount : 0,
    rejection_count: S ? S.rejections : 0,
    resource_failures: S ? S.resources.slice() : [],
    resource_failure_count: S ? S.resourceCount : 0,
    missing: S ? S.missing.slice() : [],
    css_missing: cssMissing,
    api: api,
    ua_block: uaBlock,
    bot_check: botCheck,
    rules: S ? S.ran.slice() : [],
    failed_rules: S ? S.failed.slice() : [],
    mode: S ? S.mode : null,
    layout: {
      vw: vw, vh: vh,
      doc_h: safe(function () { return de.scrollHeight; }, 0),
      body_h: bodyRect ? Math.round(bodyRect.height) : 0,
      text_len: textLen,
      elements: elements,
      media: media,
      samples: samples,
      blank_samples: blank,
      overflow_hidden: overflowHidden
    }
  });
})()
