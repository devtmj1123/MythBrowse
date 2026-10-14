(function () {
  // SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
  // Structure-only DOM summary for AI repair (sent to the model only when the user presses
  // "Ask Myth to fix"). Tags, ids, classes, sizes and layout flags; no text content, no form
  // values, no attribute values other than id / class / script and stylesheet URLs.
  "use strict";
  var doc = document, body = doc.body, de = doc.documentElement;
  var lines = [], budget = 7000, used = 0, nodes = 0;
  var SKIP = { SCRIPT: 1, STYLE: 1, NOSCRIPT: 1, TEMPLATE: 1, LINK: 1, META: 1 };
  function cls(el) {
    var c = typeof el.className === "string" ? el.className.trim().split(/\s+/).slice(0, 3).join(".") : "";
    return (el.id ? "#" + el.id.slice(0, 40) : "") + (c ? "." + c.slice(0, 60) : "");
  }
  function line(el, depth) {
    var r = el.getBoundingClientRect(), cs = getComputedStyle(el), f = [];
    if (cs.display === "none") f.push("display:none");
    else if (cs.display !== "block" && cs.display !== "inline") f.push("display:" + cs.display);
    if (cs.visibility === "hidden") f.push("hidden");
    if (cs.position === "fixed" || cs.position === "sticky" || cs.position === "absolute") f.push(cs.position);
    if (cs.overflow !== "visible") f.push("overflow:" + cs.overflow);
    if (cs.opacity === "0") f.push("opacity:0");
    var own = 0;
    for (var t = el.firstChild; t; t = t.nextSibling) if (t.nodeType === 3) own += t.nodeValue.trim().length;
    return new Array(depth + 1).join("  ") + el.tagName.toLowerCase() + cls(el) +
      " [" + Math.round(r.width) + "x" + Math.round(r.height) + "]" +
      (f.length ? " {" + f.join(",") + "}" : "") + (own ? " t:" + own : "") +
      (el.children.length ? " c:" + el.children.length : "");
  }
  function walk(el, depth) {
    for (var c = el.firstElementChild; c; c = c.nextElementSibling) {
      if (SKIP[c.tagName]) continue;
      if (used > budget || nodes > 250) return;
      var s;
      try { s = line(c, depth); } catch (e) { continue; }
      lines.push(s);
      used += s.length + 1;
      nodes++;
      if (depth < 6) walk(c, depth + 1);
    }
  }
  if (body) walk(body, 0);
  function url(u) {
    try { var x = new URL(u, location.href); return x.host + x.pathname; } catch (e) { return ""; }
  }
  var scripts = [], styles = [];
  var ss = doc.querySelectorAll("script[src]");
  for (var i = 0; i < ss.length && scripts.length < 20; i++) scripts.push(url(ss[i].getAttribute("src")));
  var ls = doc.querySelectorAll("link[rel~=stylesheet][href]");
  for (var k = 0; k < ls.length && styles.length < 12; k++) styles.push(url(ls[k].getAttribute("href")));
  var hcs = de ? getComputedStyle(de) : null, bcs = body ? getComputedStyle(body) : null;
  return JSON.stringify({
    url: location.origin + location.pathname,
    title: (doc.title || "").slice(0, 120),
    html: de ? cls(de) + (hcs ? " overflow:" + hcs.overflow : "") : "",
    body: body ? cls(body) + (bcs ? " overflow:" + bcs.overflow + " display:" + bcs.display : "") : "",
    viewport: innerWidth + "x" + innerHeight,
    scripts: scripts,
    stylesheets: styles,
    outline: lines.join("\n")
  });
})()
