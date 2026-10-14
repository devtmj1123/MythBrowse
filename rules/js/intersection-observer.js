// MythBrowse compat: IntersectionObserver (rule polyfill.intersection-observer).
// Real geometry, not "everything is visible": the previous stub reported every observed element
// as intersecting at once, so infinite-scroll sentinels loaded page after page forever and the
// page never went idle (compat-script hangs in the top-sites runs). Now each target's
// intersection with the root (viewport or root element, grown by rootMargin) is computed on
// observe and re-checked after scroll / resize / DOM changes (batched to one check per frame);
// callbacks fire only when a target crosses a threshold, like the native API.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (typeof window.IntersectionObserver !== "function") {
  var observers = [];
  var scheduled = false;
  var raf = window.requestAnimationFrame ? function (f) { return window.requestAnimationFrame(f); } : function (f) { return setTimeout(f, 16); };
  var parseMargin = function (m) {
    var parts = String(m || "0px").trim().split(/\s+/);
    var vals = parts.map(function (p) {
      var n = parseFloat(p) || 0;
      return { v: n, pct: /%$/.test(p) };
    });
    while (vals.length < 4) vals.push(vals[vals.length === 3 ? 1 : vals.length === 2 ? 0 : 0]);
    if (parts.length === 2) vals = [vals[0], vals[1], vals[0], vals[1]];
    if (parts.length === 3) vals = [vals[0], vals[1], vals[2], vals[1]];
    return vals; // top right bottom left
  };
  var rootRect = function (obs) {
    var r;
    if (obs.root && obs.root.nodeType === 1) {
      var b = obs.root.getBoundingClientRect();
      r = { top: b.top, left: b.left, right: b.right, bottom: b.bottom };
    } else {
      var de = document.documentElement;
      var w = (de && de.clientWidth) || window.innerWidth, h = (de && de.clientHeight) || window.innerHeight;
      r = { top: 0, left: 0, right: w, bottom: h };
    }
    var m = obs._margin, W = r.right - r.left, H = r.bottom - r.top;
    var px = function (x, base) { return x.pct ? x.v * base / 100 : x.v; };
    r.top -= px(m[0], H); r.right += px(m[1], W); r.bottom += px(m[2], H); r.left -= px(m[3], W);
    r.width = Math.max(0, r.right - r.left); r.height = Math.max(0, r.bottom - r.top);
    r.x = r.left; r.y = r.top;
    return r;
  };
  var entryFor = function (obs, el, root) {
    var b = el.getBoundingClientRect();
    var connected = el.isConnected !== false && (!obs.root || obs.root.contains(el));
    var top = Math.max(b.top, root.top), left = Math.max(b.left, root.left);
    var bottom = Math.min(b.bottom, root.bottom), right = Math.min(b.right, root.right);
    var w = right - left, h = bottom - top;
    var area = b.width * b.height;
    // edge-adjacent (zero-area) intersections count, as in the spec
    var hit = connected && w >= 0 && h >= 0 && !(b.width === 0 && b.height === 0 && (b.top === 0 && b.left === 0 && b.bottom === 0));
    var ir = hit ? { top: top, left: left, bottom: bottom, right: right, width: Math.max(0, w), height: Math.max(0, h), x: left, y: top }
                 : { top: 0, left: 0, bottom: 0, right: 0, width: 0, height: 0, x: 0, y: 0 };
    var ratio = !hit ? 0 : area > 0 ? Math.min(1, (ir.width * ir.height) / area) : 1;
    return {
      target: el, time: (window.performance && performance.now) ? performance.now() : Date.now(),
      boundingClientRect: b, intersectionRect: ir, rootBounds: obs.root ? root : root,
      isIntersecting: hit, intersectionRatio: ratio, isVisible: false
    };
  };
  var thresholdIndex = function (obs, e) {
    if (!e.isIntersecting) return -1;
    var t = obs.thresholds, i = 0;
    while (i < t.length && e.intersectionRatio >= t[i]) i++;
    return i;
  };
  var check = function (obs) {
    if (!obs._targets.length) return;
    var root = rootRect(obs), out = [];
    for (var i = 0; i < obs._targets.length; i++) {
      var rec = obs._targets[i];
      var e = entryFor(obs, rec.el, root);
      var idx = thresholdIndex(obs, e);
      if (idx !== rec.last) { rec.last = idx; out.push(e); }
    }
    if (out.length) {
      obs._queue = obs._queue.concat(out);
      var q = obs._queue; obs._queue = [];
      try { obs._cb.call(obs, q, obs); } catch (err) { setTimeout(function () { throw err; }, 0); }
    }
  };
  var checkAll = function () {
    scheduled = false;
    var list = observers.slice();
    for (var i = 0; i < list.length; i++) check(list[i]);
  };
  var schedule = function () {
    if (scheduled) return;
    scheduled = true;
    raf(checkAll);
  };
  var listening = false;
  var listen = function () {
    if (listening) return;
    listening = true;
    window.addEventListener("scroll", schedule, { capture: true, passive: true });
    window.addEventListener("resize", schedule, { passive: true });
    window.addEventListener("load", schedule);
    if (typeof MutationObserver === "function") {
      var mo = new MutationObserver(schedule);
      var start = function () {
        if (document.documentElement) mo.observe(document.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: ["class", "style", "hidden"] });
      };
      if (document.documentElement) start(); else document.addEventListener("DOMContentLoaded", start, { once: true });
    }
  };
  var IO = function IntersectionObserver(callback, options) {
    if (typeof callback !== "function") throw new TypeError("IntersectionObserver: callback must be a function");
    var o = options || {};
    var th = o.threshold == null ? [0] : [].concat(o.threshold);
    for (var i = 0; i < th.length; i++) {
      th[i] = Number(th[i]);
      if (!(th[i] >= 0 && th[i] <= 1)) throw new RangeError("IntersectionObserver: threshold must be in [0, 1]");
    }
    th.sort(function (a, b) { return a - b; });
    this._cb = callback;
    this._targets = [];
    this._queue = [];
    this.root = o.root || null;
    this.rootMargin = o.rootMargin || "0px";
    this._margin = parseMargin(this.rootMargin);
    this.thresholds = th;
    this.scrollMargin = "0px";
    this.delay = 0;
    this.trackVisibility = false;
  };
  IO.prototype.observe = function (el) {
    if (!el || el.nodeType !== 1) throw new TypeError("IntersectionObserver.observe: target must be an Element");
    for (var i = 0; i < this._targets.length; i++) if (this._targets[i].el === el) return;
    this._targets.push({ el: el, last: -2 });
    if (observers.indexOf(this) < 0) observers.push(this);
    listen();
    schedule();
  };
  IO.prototype.unobserve = function (el) {
    for (var i = 0; i < this._targets.length; i++) if (this._targets[i].el === el) { this._targets.splice(i, 1); break; }
    if (!this._targets.length) { var at = observers.indexOf(this); if (at >= 0) observers.splice(at, 1); }
  };
  IO.prototype.disconnect = function () {
    this._targets = [];
    var at = observers.indexOf(this);
    if (at >= 0) observers.splice(at, 1);
  };
  IO.prototype.takeRecords = function () { var q = this._queue; this._queue = []; return q; };
  window.IntersectionObserver = IO;
  if (typeof window.IntersectionObserverEntry !== "function") {
    var Entry = function IntersectionObserverEntry() {};
    Entry.prototype.isIntersecting = false;
    Entry.prototype.intersectionRatio = 0;
    window.IntersectionObserverEntry = Entry;
  }
}
