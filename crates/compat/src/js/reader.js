(function (mode) {
  // SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
  // Reader / simplified view, one shot (no observers, no timers).
  //   "simplified": the page itself, minus fixed / sticky overlays that cover much of the
  //                 viewport and the scroll locks (overflow: hidden) that come with them.
  //   "reader":     an article view over the page: title, headings, paragraphs, lists,
  //                 quotes, code and images copied (as plain elements, no scripts or
  //                 handlers) from the main content. "Close" removes it again.
  //   "off":        remove what either mode added.
  "use strict";
  var doc = document, de = doc.documentElement, body = doc.body;
  if (!de || !body) return "no-document";
  var OVERLAY_ID = "mb-compat-reader";
  var STYLE_ID = "mb-compat-simplified";
  function off() {
    var o = doc.getElementById(OVERLAY_ID);
    if (o) o.remove();
    var s = doc.getElementById(STYLE_ID);
    if (s) s.remove();
    var hidden = doc.querySelectorAll("[data-mb-compat-hidden]");
    for (var i = 0; i < hidden.length; i++) {
      hidden[i].style.removeProperty("display");
      hidden[i].removeAttribute("data-mb-compat-hidden");
    }
    return "off";
  }
  if (mode === "off") return off();

  if (mode === "simplified") {
    if (!doc.getElementById(STYLE_ID)) {
      var st = doc.createElement("style");
      st.id = STYLE_ID;
      st.textContent = "html,body{overflow:auto!important;position:static!important;height:auto!important}" +
        "body>*{filter:none!important}";
      (doc.head || de).appendChild(st);
    }
    var vw = innerWidth, vh = innerHeight, all = body.getElementsByTagName("*");
    var n = Math.min(all.length, 5000), removed = 0;
    for (var i = 0; i < n; i++) {
      var el = all[i], cs;
      try { cs = getComputedStyle(el); } catch (e) { continue; }
      if (cs.position !== "fixed" && cs.position !== "sticky") continue;
      var r = el.getBoundingClientRect();
      var cover = Math.max(0, Math.min(r.right, vw) - Math.max(r.left, 0)) *
        Math.max(0, Math.min(r.bottom, vh) - Math.max(r.top, 0));
      if (cover > vw * vh * 0.3 || (cs.position === "fixed" && r.height > vh * 0.25 && r.top > vh * 0.5)) {
        el.setAttribute("data-mb-compat-hidden", "1");
        el.style.setProperty("display", "none", "important");
        removed++;
      }
    }
    return "simplified:" + removed;
  }

  if (mode !== "reader") return "unknown-mode";
  off();
  // Main content: <article> / [role=main] / <main>, else the element whose direct <p>
  // children hold the most text.
  var root = doc.querySelector("article") || doc.querySelector("[role=main]") || doc.querySelector("main");
  if (!root) {
    var best = null, bestLen = 0, ps = body.getElementsByTagName("p");
    var score = new Map();
    for (var k = 0; k < Math.min(ps.length, 3000); k++) {
      var p = ps[k], parent = p.parentElement;
      if (!parent) continue;
      var len = (p.textContent || "").trim().length;
      var s = (score.get(parent) || 0) + len;
      score.set(parent, s);
      if (s > bestLen) { bestLen = s; best = parent; }
    }
    root = best || body;
  }
  var KEEP = { P: 1, H1: 1, H2: 1, H3: 1, H4: 1, H5: 1, H6: 1, UL: 1, OL: 1, BLOCKQUOTE: 1, PRE: 1, FIGURE: 1, IMG: 1, TABLE: 1 };
  var ATTRS = { src: 1, alt: 1, href: 1, colspan: 1, rowspan: 1 };
  function clean(node) {
    if (node.nodeType === 3) return doc.createTextNode(node.nodeValue);
    if (node.nodeType !== 1) return null;
    var tag = node.tagName;
    if (/^(SCRIPT|STYLE|NOSCRIPT|IFRAME|OBJECT|EMBED|FORM|INPUT|BUTTON|SELECT|TEXTAREA|SVG|CANVAS|VIDEO|AUDIO)$/i.test(tag)) return null;
    var out = doc.createElement(/^[A-Z][A-Z0-9]*$/.test(tag) ? tag : "SPAN");
    for (var a = 0; a < node.attributes.length; a++) {
      var at = node.attributes[a];
      if (!ATTRS[at.name]) continue;
      if ((at.name === "href" || at.name === "src") && /^\s*javascript:/i.test(at.value)) continue;
      out.setAttribute(at.name, at.value);
    }
    if (tag === "IMG" && !out.getAttribute("src") && node.currentSrc) out.setAttribute("src", node.currentSrc);
    for (var c = node.firstChild; c; c = c.nextSibling) {
      var cc = clean(c);
      if (cc) out.appendChild(cc);
    }
    return out;
  }
  var picked = [];
  (function collect(el, depth) {
    for (var c = el.firstElementChild; c && picked.length < 2000; c = c.nextElementSibling) {
      if (KEEP[c.tagName]) picked.push(c);
      else if (depth < 12) collect(c, depth + 1);
    }
  })(root, 0);
  var overlay = doc.createElement("div");
  overlay.id = OVERLAY_ID;
  overlay.setAttribute("style", "all:initial;position:fixed;inset:0;z-index:2147483647;overflow:auto;background:#f7f6f2;");
  var host = overlay.attachShadow ? overlay.attachShadow({ mode: "closed" }) : overlay;
  var style = doc.createElement("style");
  style.textContent =
    ".mb-r{box-sizing:border-box;max-width:680px;margin:0 auto;padding:48px 24px 96px;color:#1c2433;" +
    "font:18px/1.65 Georgia,'Times New Roman',serif}" +
    ".mb-r h1{font:700 32px/1.25 system-ui,sans-serif;margin:0 0 24px}" +
    ".mb-r h2,.mb-r h3,.mb-r h4{font-family:system-ui,sans-serif;line-height:1.3;margin:32px 0 12px}" +
    ".mb-r img{max-width:100%;height:auto}.mb-r pre{overflow:auto;background:#eceae4;padding:12px;font-size:14px}" +
    ".mb-r a{color:#2a5db0}.mb-r table{border-collapse:collapse}.mb-r td,.mb-r th{border:1px solid #ccc;padding:4px 8px}" +
    ".mb-x{position:fixed;top:16px;right:20px;font:600 13px system-ui,sans-serif;color:#1c2433;" +
    "background:#fff;border:1px solid #d8d6cf;border-radius:999px;padding:7px 14px;cursor:pointer}";
  host.appendChild(style);
  var wrap = doc.createElement("div");
  wrap.className = "mb-r";
  var h1 = doc.createElement("h1");
  h1.textContent = doc.title || location.hostname;
  wrap.appendChild(h1);
  for (var q = 0; q < picked.length; q++) {
    var cl = clean(picked[q]);
    if (cl) wrap.appendChild(cl);
  }
  host.appendChild(wrap);
  var x = doc.createElement("button");
  x.className = "mb-x";
  x.textContent = "Close reader";
  x.addEventListener("click", function () { overlay.remove(); });
  host.appendChild(x);
  body.appendChild(overlay);
  return "reader:" + picked.length;
})
