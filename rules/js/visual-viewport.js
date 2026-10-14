// MythBrowse compat: window.visualViewport (rule polyfill.visual-viewport).
// Servo has no pinch zoom, so the visual viewport is the layout viewport: scale 1, offsets 0,
// size = the viewport without scrollbars, page offsets = scroll position. resize / scroll are
// forwarded from the window.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (typeof window.visualViewport === "undefined") {
  var listeners = { resize: [], scroll: [], scrollend: [] };
  var vv = {
    offsetLeft: 0, offsetTop: 0, scale: 1, onresize: null, onscroll: null, onscrollend: null,
    addEventListener: function (type, fn) { if (listeners[type] && typeof fn === "function" && listeners[type].indexOf(fn) < 0) listeners[type].push(fn); },
    removeEventListener: function (type, fn) { var l = listeners[type]; if (l) { var i = l.indexOf(fn); if (i >= 0) l.splice(i, 1); } },
    dispatchEvent: function (ev) {
      var h = vv["on" + ev.type];
      if (typeof h === "function") h.call(vv, ev);
      var l = (listeners[ev.type] || []).slice();
      for (var i = 0; i < l.length; i++) l[i].call(vv, ev);
      return true;
    }
  };
  var de = function () { return document.documentElement; };
  Object.defineProperties(vv, {
    width: { get: function () { var d = de(); return d && d.clientWidth ? d.clientWidth : window.innerWidth; }, enumerable: true },
    height: { get: function () { var d = de(); return d && d.clientHeight ? d.clientHeight : window.innerHeight; }, enumerable: true },
    pageLeft: { get: function () { return window.scrollX || window.pageXOffset || 0; }, enumerable: true },
    pageTop: { get: function () { return window.scrollY || window.pageYOffset || 0; }, enumerable: true }
  });
  var forward = function (type) {
    window.addEventListener(type, function () {
      var ev;
      try { ev = new Event(type); } catch (e) { ev = { type: type }; }
      try { Object.defineProperty(ev, "target", { value: vv }); } catch (e) { /* plain object */ }
      vv.dispatchEvent(ev);
    }, { passive: true });
  };
  forward("resize"); forward("scroll");
  Object.defineProperty(window, "visualViewport", { value: vv, configurable: true, enumerable: true, writable: false });
  if (typeof window.VisualViewport === "undefined") {
    var VisualViewport = function VisualViewport() { throw new TypeError("Illegal constructor"); };
    Object.setPrototypeOf(vv, VisualViewport.prototype);
    Object.defineProperty(window, "VisualViewport", { value: VisualViewport, configurable: true, writable: true });
  }
}
