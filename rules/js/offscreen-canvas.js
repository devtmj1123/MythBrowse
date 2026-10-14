// MythBrowse compat: basic OffscreenCanvas on the main thread (rule polyfill.offscreen-canvas).
// Backed by a detached <canvas>: getContext("2d" | "webgl" | "webgl2" | "bitmaprenderer"),
// width/height, convertToBlob(), transferToImageBitmap(). Not transferable to workers (Servo
// workers get no polyfill); code that feature-detects for a worker path falls back as usual.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (typeof window.OffscreenCanvas === "undefined" && typeof document !== "undefined" && document.createElement) {
  var OffscreenCanvas = function OffscreenCanvas(width, height) {
    if (!(this instanceof OffscreenCanvas)) throw new TypeError("Constructor OffscreenCanvas requires 'new'");
    var c = document.createElement("canvas");
    c.width = Math.max(0, width | 0); c.height = Math.max(0, height | 0);
    Object.defineProperty(this, "_c", { value: c });
    this.oncontextlost = null; this.oncontextrestored = null;
  };
  Object.defineProperty(OffscreenCanvas.prototype, "width", {
    get: function () { return this._c.width; }, set: function (v) { this._c.width = v; }, configurable: true, enumerable: true
  });
  Object.defineProperty(OffscreenCanvas.prototype, "height", {
    get: function () { return this._c.height; }, set: function (v) { this._c.height = v; }, configurable: true, enumerable: true
  });
  OffscreenCanvas.prototype.getContext = function (type, options) {
    var ctx = this._c.getContext(type, options);
    if (ctx) { try { Object.defineProperty(ctx, "canvas", { value: this, configurable: true }); } catch (e) { /* native getter */ } }
    return ctx;
  };
  OffscreenCanvas.prototype.convertToBlob = function (options) {
    var c = this._c, o = options || {};
    return new Promise(function (resolve, reject) {
      try {
        c.toBlob(function (b) { if (b) resolve(b); else reject(new DOMException("Could not encode the canvas", "EncodingError")); },
                 o.type || "image/png", o.quality);
      } catch (e) { reject(e); }
    });
  };
  OffscreenCanvas.prototype.transferToImageBitmap = function () {
    // Synchronous in the spec; the best a polyfill can do is hand back a canvas copy, which
    // drawImage() and most consumers accept wherever an ImageBitmap is.
    var copy = document.createElement("canvas");
    copy.width = this._c.width; copy.height = this._c.height;
    var ctx = copy.getContext("2d");
    if (ctx) ctx.drawImage(this._c, 0, 0);
    var g = this._c.getContext("2d");
    if (g) g.clearRect(0, 0, this._c.width, this._c.height);
    copy.close = function () {};
    return copy;
  };
  OffscreenCanvas.prototype.addEventListener = function (t, f, o) { this._c.addEventListener(t, f, o); };
  OffscreenCanvas.prototype.removeEventListener = function (t, f, o) { this._c.removeEventListener(t, f, o); };
  OffscreenCanvas.prototype.dispatchEvent = function (e) { return this._c.dispatchEvent(e); };
  Object.defineProperty(window, "OffscreenCanvas", { value: OffscreenCanvas, configurable: true, writable: true });
  // transferControlToOffscreen is deliberately not provided: a fake one cannot draw to the
  // visible canvas from a worker, and sites check for it before taking their worker path.
}
