// MythBrowse compat: SVG geometry DOM (rule polyfill.svg-geometry).
// Servo draws SVG but its SVG DOM has none of the geometry methods: getBBox, getCTM,
// getScreenCTM, getTotalLength, getPointAtLength, createSVGPoint/Matrix/Rect,
// getComputedTextLength, pauseAnimations. Chart and animation code calls them unguarded
// (avast.com: "getBBox is not a function", "getScreenCTM is not a function", 65 errors per
// load; pages.dev / workers.dev: "getTotalLength is not a function").
// Boxes and lengths are computed from the element's own geometry attributes (rect, circle,
// ellipse, line, polyline, polygon, path, image, use, text by measured glyph width, groups as
// the union of their children), not from layout, so they also work for elements Servo did not
// lay out. Curves are measured by sampling. Missing members only; native ones are kept.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
(function () {
  if (typeof SVGElement === "undefined" || typeof DOMMatrix === "undefined") return;
  var G = (window.SVGGraphicsElement || SVGElement).prototype;
  var GEO = (window.SVGGeometryElement || window.SVGPathElement || SVGElement).prototype;
  var SVG = window.SVGSVGElement ? SVGSVGElement.prototype : null;
  function def(proto, name, fn) {
    if (!proto || name in proto) return;
    Object.defineProperty(proto, name, { value: fn, configurable: true, writable: true });
  }
  function num(el, a, d) {
    var v = parseFloat(el.getAttribute(a));
    return isFinite(v) ? v : (d || 0);
  }
  function rect(x, y, w, h) {
    var R = window.DOMRect || function (x, y, w, h) { this.x = x; this.y = y; this.width = w; this.height = h; };
    return new R(x, y, w, h);
  }
  // ---- path data -> polyline (sampled) ----
  var CMD = /([MmLlHhVvCcSsQqTtAaZz])([^MmLlHhVvCcSsQqTtAaZz]*)/g;
  function nums(s) {
    var m = s.match(/-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g);
    return m ? m.map(Number) : [];
  }
  function pathPoints(d) {
    var subs = [], cur = [], x = 0, y = 0, sx = 0, sy = 0, cx = 0, cy = 0, prev = "", m, N = 16;
    function to(px, py) { cur.push([px, py]); x = px; y = py; }
    function cubic(x1, y1, x2, y2, x3, y3) {
      var x0 = x, y0 = y;
      for (var i = 1; i <= N; i++) {
        var t = i / N, u = 1 - t;
        to(u * u * u * x0 + 3 * u * u * t * x1 + 3 * u * t * t * x2 + t * t * t * x3,
           u * u * u * y0 + 3 * u * u * t * y1 + 3 * u * t * t * y2 + t * t * t * y3);
      }
      cx = x2; cy = y2;
    }
    function quad(x1, y1, x2, y2) {
      var x0 = x, y0 = y;
      for (var i = 1; i <= N; i++) {
        var t = i / N, u = 1 - t;
        to(u * u * x0 + 2 * u * t * x1 + t * t * x2, u * u * y0 + 2 * u * t * y1 + t * t * y2);
      }
      cx = x1; cy = y1;
    }
    function arc(rx, ry, rot, large, sweep, x2, y2) {
      // Endpoint -> center parameterisation (SVG implementation notes F.6.5).
      var x1 = x, y1 = y;
      rx = Math.abs(rx); ry = Math.abs(ry);
      if (!rx || !ry) { to(x2, y2); return; }
      var phi = rot * Math.PI / 180, cs = Math.cos(phi), sn = Math.sin(phi);
      var dx = (x1 - x2) / 2, dy = (y1 - y2) / 2;
      var x1p = cs * dx + sn * dy, y1p = -sn * dx + cs * dy;
      var lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
      if (lam > 1) { rx *= Math.sqrt(lam); ry *= Math.sqrt(lam); }
      var num_ = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
      var co = Math.sqrt(Math.max(0, num_ / (rx * rx * y1p * y1p + ry * ry * x1p * x1p))) * (large === sweep ? -1 : 1);
      var cxp = co * rx * y1p / ry, cyp = -co * ry * x1p / rx;
      var ccx = cs * cxp - sn * cyp + (x1 + x2) / 2, ccy = sn * cxp + cs * cyp + (y1 + y2) / 2;
      function ang(ux, uy, vx, vy) {
        var a = Math.atan2(ux * vy - uy * vx, ux * vx + uy * vy);
        return a;
      }
      var t1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry);
      var dt = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry);
      if (!sweep && dt > 0) dt -= 2 * Math.PI; else if (sweep && dt < 0) dt += 2 * Math.PI;
      var n = Math.max(4, Math.ceil(Math.abs(dt) / (Math.PI / 8)));
      for (var i = 1; i <= n; i++) {
        var t = t1 + dt * i / n;
        to(ccx + rx * Math.cos(t) * cs - ry * Math.sin(t) * sn, ccy + rx * Math.cos(t) * sn + ry * Math.sin(t) * cs);
      }
      x = x2; y = y2;
    }
    String(d || "").replace(CMD, function (_, c, args) {
      var a = nums(args), rel = c === c.toLowerCase(), C = c.toUpperCase(), i = 0;
      var ox = function () { return rel ? x : 0; }, oy = function () { return rel ? y : 0; };
      if (C === "Z") {
        to(sx, sy); subs.push(cur); cur = [[sx, sy]]; prev = C; return "";
      }
      do {
        switch (C) {
          case "M":
            if (cur.length > 1) subs.push(cur);
            x = a[i] + ox(); y = a[i + 1] + oy(); i += 2; sx = x; sy = y; cur = [[x, y]];
            C = "L"; break;
          case "L": to(a[i] + ox(), a[i + 1] + oy()); i += 2; break;
          case "H": to(a[i] + ox(), y); i += 1; break;
          case "V": to(x, a[i] + oy()); i += 1; break;
          case "C": { var b = ox(), e = oy(); cubic(a[i] + b, a[i + 1] + e, a[i + 2] + b, a[i + 3] + e, a[i + 4] + b, a[i + 5] + e); i += 6; break; }
          case "S": {
            var b2 = ox(), e2 = oy(), r1x = x, r1y = y;
            if (prev === "C" || prev === "S") { r1x = 2 * x - cx; r1y = 2 * y - cy; }
            cubic(r1x, r1y, a[i] + b2, a[i + 1] + e2, a[i + 2] + b2, a[i + 3] + e2); i += 4; break;
          }
          case "Q": { var b3 = ox(), e3 = oy(); quad(a[i] + b3, a[i + 1] + e3, a[i + 2] + b3, a[i + 3] + e3); i += 4; break; }
          case "T": {
            var q1x = x, q1y = y;
            if (prev === "Q" || prev === "T") { q1x = 2 * x - cx; q1y = 2 * y - cy; }
            quad(q1x, q1y, a[i] + ox(), a[i + 1] + oy()); i += 2; break;
          }
          case "A": arc(a[i], a[i + 1], a[i + 2], a[i + 3] ? 1 : 0, a[i + 4] ? 1 : 0, a[i + 5] + ox(), a[i + 6] + oy()); i += 7; break;
          default: i = a.length;
        }
        prev = C;
      } while (i < a.length && !isNaN(a[i]));
      return "";
    });
    if (cur.length) subs.push(cur);
    return subs;
  }
  function shapePoints(el) {
    var t = (el.localName || "").toLowerCase();
    if (t === "path") return pathPoints(el.getAttribute("d"));
    if (t === "line") return [[[num(el, "x1"), num(el, "y1")], [num(el, "x2"), num(el, "y2")]]];
    if (t === "polyline" || t === "polygon") {
      var p = nums(el.getAttribute("points") || ""), pts = [];
      for (var i = 0; i + 1 < p.length; i += 2) pts.push([p[i], p[i + 1]]);
      if (t === "polygon" && pts.length) pts.push(pts[0]);
      return [pts];
    }
    if (t === "rect") {
      var x = num(el, "x"), y = num(el, "y"), w = num(el, "width"), h = num(el, "height");
      return [[[x, y], [x + w, y], [x + w, y + h], [x, y + h], [x, y]]];
    }
    if (t === "circle" || t === "ellipse") {
      var c = num(el, "cx"), d = num(el, "cy"), rx = t === "circle" ? num(el, "r") : num(el, "rx"), ry = t === "circle" ? rx : num(el, "ry"), out = [];
      for (var k = 0; k <= 64; k++) out.push([c + rx * Math.cos(k * Math.PI / 32), d + ry * Math.sin(k * Math.PI / 32)]);
      return [out];
    }
    return null;
  }
  function polyLength(subs) {
    var L = 0;
    subs.forEach(function (s) { for (var i = 1; i < s.length; i++) L += Math.hypot(s[i][0] - s[i - 1][0], s[i][1] - s[i - 1][1]); });
    return L;
  }
  // ---- transforms ----
  function parseTransform(s) {
    var m = new DOMMatrix();
    String(s || "").replace(/(matrix|translate|scale|rotate|skewX|skewY)\s*\(([^)]*)\)/g, function (_, f, args) {
      var a = nums(args);
      if (f === "matrix" && a.length >= 6) m = m.multiply(new DOMMatrix([a[0], a[1], a[2], a[3], a[4], a[5]]));
      else if (f === "translate") m = m.translate(a[0] || 0, a[1] || 0);
      else if (f === "scale") m = m.scale(a[0], a.length > 1 ? a[1] : a[0]);
      else if (f === "rotate") m = a.length >= 3 ? m.translate(a[1], a[2]).rotate(a[0]).translate(-a[1], -a[2]) : m.rotate(a[0] || 0);
      else if (f === "skewX") m = m.skewX(a[0] || 0);
      else if (f === "skewY") m = m.skewY(a[0] || 0);
      return "";
    });
    return m;
  }
  function own(el) { return el.getAttribute && el.getAttribute("transform") ? parseTransform(el.getAttribute("transform")) : new DOMMatrix(); }
  function isSvgRoot(el) { return el && (el.localName || "").toLowerCase() === "svg"; }
  function viewportMatrix(svg) {
    var vb = nums(svg.getAttribute("viewBox") || "");
    var r = svg.getBoundingClientRect ? svg.getBoundingClientRect() : { width: 0, height: 0 };
    var w = r.width || num(svg, "width"), h = r.height || num(svg, "height");
    var m = new DOMMatrix();
    if (vb.length === 4 && vb[2] > 0 && vb[3] > 0 && w && h) {
      var par = String(svg.getAttribute("preserveAspectRatio") || "xMidYMid meet");
      var sx = w / vb[2], sy = h / vb[3];
      if (!/^none/.test(par)) {
        var s = /slice/.test(par) ? Math.max(sx, sy) : Math.min(sx, sy);
        var tx = 0, ty = 0;
        if (/xMid/.test(par) || par === "") tx = (w - vb[2] * s) / 2; else if (/xMax/.test(par)) tx = w - vb[2] * s;
        if (/YMid/.test(par) || par === "") ty = (h - vb[3] * s) / 2; else if (/YMax/.test(par)) ty = h - vb[3] * s;
        sx = sy = s;
        m = m.translate(tx, ty);
      }
      m = m.scale(sx, sy).translate(-vb[0], -vb[1]);
    }
    return m;
  }
  // Element user space -> nearest viewport (the closest <svg>) user space.
  function ctm(el) {
    var m = own(el), p = el.parentNode;
    while (p && p.nodeType === 1 && !isSvgRoot(p)) { m = own(p).multiply(m); p = p.parentNode; }
    if (isSvgRoot(el)) return viewportMatrix(el);
    if (p && isSvgRoot(p)) {
      // Nested <svg>: its own x/y offset and viewBox mapping.
      var outer = p.ownerSVGElement || (p.parentNode && p.parentNode.closest ? p.parentNode.closest("svg") : null);
      if (outer) m = new DOMMatrix().translate(num(p, "x"), num(p, "y")).multiply(viewportMatrix(p)).multiply(m);
    }
    return m;
  }
  function screenCtm(el) {
    var svg = isSvgRoot(el) ? el : (el.closest ? el.closest("svg") : null), m = new DOMMatrix();
    if (!svg) return m;
    var outer = svg;
    while (outer.parentNode && outer.parentNode.closest && outer.parentNode.closest("svg")) outer = outer.parentNode.closest("svg");
    var r = outer.getBoundingClientRect();
    var toRoot = isSvgRoot(el) ? viewportMatrix(el) : ctm(el);
    var s = svg;
    while (s !== outer && s) {
      var up = s.parentNode && s.parentNode.closest ? s.parentNode.closest("svg") : null;
      if (!up) break;
      var mm = own(s.parentNode), q = s.parentNode.parentNode;
      while (q && q !== up) { mm = own(q).multiply(mm); q = q.parentNode; }
      toRoot = mm.multiply(new DOMMatrix().translate(num(s, "x"), num(s, "y"))).multiply(toRoot);
      s = up;
    }
    if (s === outer && !isSvgRoot(el)) toRoot = viewportMatrix(outer).multiply(toRoot);
    return new DOMMatrix().translate(r.left, r.top).multiply(toRoot);
  }
  // ---- bounding boxes ----
  var measureCtx = null;
  function textWidth(el) {
    var str = el.textContent || "";
    if (!str) return 0;
    try {
      if (!measureCtx) measureCtx = document.createElement("canvas").getContext("2d");
      var cs = getComputedStyle(el);
      measureCtx.font = (cs.fontStyle || "") + " " + (cs.fontWeight || "") + " " + (cs.fontSize || "16px") + " " + (cs.fontFamily || "sans-serif");
      return measureCtx.measureText(str).width;
    } catch (e) {
      return str.length * 8;
    }
  }
  function fontSize(el) {
    try { return parseFloat(getComputedStyle(el).fontSize) || 16; } catch (e) { return 16; }
  }
  function box(el) {
    var t = (el.localName || "").toLowerCase(), pts = shapePoints(el);
    if (pts) {
      var x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
      pts.forEach(function (s) { s.forEach(function (p) { x0 = Math.min(x0, p[0]); y0 = Math.min(y0, p[1]); x1 = Math.max(x1, p[0]); y1 = Math.max(y1, p[1]); }); });
      return isFinite(x0) ? [x0, y0, x1, y1] : null;
    }
    if (t === "image" || t === "foreignobject" || t === "use" && el.hasAttribute("width")) {
      var x = num(el, "x"), y = num(el, "y");
      return [x, y, x + num(el, "width"), y + num(el, "height")];
    }
    if (t === "text" || t === "tspan" || t === "textpath") {
      var fs = fontSize(el), tx = num(el, "x"), ty = num(el, "y"), w = textWidth(el);
      var anchor = el.getAttribute("text-anchor") || "";
      try { anchor = getComputedStyle(el).textAnchor || anchor; } catch (e) { /* ignore */ }
      if (anchor === "middle") tx -= w / 2; else if (anchor === "end") tx -= w;
      return w ? [tx, ty - fs * 0.8, tx + w, ty + fs * 0.2] : null;
    }
    var kids = el.children || [], u = null;
    if (t === "use") {
      var href = el.getAttribute("href") || el.getAttribute("xlink:href") || "";
      var ref = href.charAt(0) === "#" ? document.getElementById(href.slice(1)) : null;
      kids = ref ? [ref] : [];
    }
    for (var i = 0; i < kids.length; i++) {
      var k = kids[i], kt = (k.localName || "").toLowerCase();
      if (/^(defs|clippath|mask|pattern|lineargradient|radialgradient|symbol|style|script|title|desc|metadata|marker|filter)$/.test(kt)) continue;
      var b = box(k);
      if (!b) continue;
      var m = own(k);
      if (t === "use") m = new DOMMatrix().translate(num(el, "x"), num(el, "y")).multiply(m);
      var c = [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]].map(function (p) { var q = new DOMPoint(p[0], p[1]).matrixTransform(m); return [q.x, q.y]; });
      for (var j = 0; j < 4; j++) {
        if (!u) u = [c[j][0], c[j][1], c[j][0], c[j][1]];
        u[0] = Math.min(u[0], c[j][0]); u[1] = Math.min(u[1], c[j][1]); u[2] = Math.max(u[2], c[j][0]); u[3] = Math.max(u[3], c[j][1]);
      }
    }
    return u;
  }
  // Servo models some graphics elements (text, g, use, ...) as plain SVGElement, so the
  // graphics methods also go on SVGElement when SVGGraphicsElement does not cover them.
  [G, SVGElement.prototype].forEach(function (P) {
    def(P, "getBBox", function () { var b = box(this); return b ? rect(b[0], b[1], b[2] - b[0], b[3] - b[1]) : rect(0, 0, 0, 0); });
    def(P, "getCTM", function () { return ctm(this); });
    def(P, "getScreenCTM", function () { return screenCtm(this); });
  });
  def(GEO, "getTotalLength", function () { var p = shapePoints(this); return p ? polyLength(p) : 0; });
  def(GEO, "getPointAtLength", function (len) {
    var subs = shapePoints(this) || [], left = Math.max(0, Number(len) || 0), last = null;
    for (var s = 0; s < subs.length; s++) {
      var pts = subs[s];
      for (var i = 1; i < pts.length; i++) {
        var d = Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]);
        if (left <= d && d > 0) {
          var f = left / d;
          return new DOMPoint(pts[i - 1][0] + (pts[i][0] - pts[i - 1][0]) * f, pts[i - 1][1] + (pts[i][1] - pts[i - 1][1]) * f);
        }
        left -= d; last = pts[i];
      }
      if (!last && pts.length) last = pts[0];
    }
    return last ? new DOMPoint(last[0], last[1]) : new DOMPoint(0, 0);
  });
  def(GEO, "isPointInFill", function (pt) {
    var b = box(this);
    return !!b && pt && pt.x >= b[0] && pt.x <= b[2] && pt.y >= b[1] && pt.y <= b[3];
  });
  if (window.SVGTextContentElement) {
    def(SVGTextContentElement.prototype, "getComputedTextLength", function () { return textWidth(this); });
    def(SVGTextContentElement.prototype, "getNumberOfChars", function () { return (this.textContent || "").length; });
  } else {
    def(SVGElement.prototype, "getComputedTextLength", function () { return textWidth(this); });
  }
  if (SVG) {
    def(SVG, "createSVGPoint", function () { return new DOMPoint(0, 0); });
    def(SVG, "createSVGMatrix", function () { return new DOMMatrix(); });
    def(SVG, "createSVGRect", function () { return rect(0, 0, 0, 0); });
    def(SVG, "createSVGNumber", function () { return { value: 0 }; });
    def(SVG, "createSVGTransformFromMatrix", function (m) { return { type: 1, matrix: new DOMMatrix(m && [m.a, m.b, m.c, m.d, m.e, m.f]), angle: 0 }; });
    var t0 = Date.now(), paused = null;
    def(SVG, "pauseAnimations", function () { if (paused === null) paused = Date.now(); });
    def(SVG, "unpauseAnimations", function () { if (paused !== null) { t0 += Date.now() - paused; paused = null; } });
    def(SVG, "animationsPaused", function () { return paused !== null; });
    def(SVG, "getCurrentTime", function () { return ((paused !== null ? paused : Date.now()) - t0) / 1000; });
    def(SVG, "setCurrentTime", function (s) { t0 = (paused !== null ? paused : Date.now()) - (Number(s) || 0) * 1000; });
    def(SVG, "forceRedraw", function () {});
    def(SVG, "suspendRedraw", function () { return 0; });
    def(SVG, "unsuspendRedraw", function () {});
    def(SVG, "unsuspendRedrawAll", function () {});
  }
})();
