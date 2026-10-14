// MythBrowse compat: document.fonts (FontFaceSet) + FontFace (rule polyfill.document-fonts).
// Servo 0.5 has a FontFaceSet with only `ready`; 0.1.x may have none. Sites call
// document.fonts.load()/check() (seen in the top-1000 run: "document.fonts.load is not a
// function") or construct FontFace objects and add them. Missing members are filled in;
// FontFace objects added to the set become @font-face rules, so the font really loads.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
(function () {
  var noop = function () {};
  var faces = [];
  var styleEl = null;
  var css = function (s) { return String(s).replace(/["\\\n]/g, " "); };
  var render = function () {
    var root = document.head || document.documentElement;
    if (!root) return;
    if (!styleEl) { styleEl = document.createElement("style"); styleEl.setAttribute("data-mb-fontfaces", ""); }
    var out = [];
    for (var i = 0; i < faces.length; i++) {
      var f = faces[i];
      if (!f._src) continue;
      out.push("@font-face{font-family:\"" + css(f.family) + "\";src:" + f._src + ";font-style:" + f.style +
               ";font-weight:" + f.weight + ";font-display:" + f.display +
               (f.unicodeRange && f.unicodeRange !== "U+0-10FFFF" ? ";unicode-range:" + f.unicodeRange : "") + "}");
    }
    styleEl.textContent = out.join("\n");
    if (!styleEl.parentNode) root.appendChild(styleEl);
  };
  var OurFontFace = null;
  if (typeof window.FontFace !== "function") {
    var FontFace = OurFontFace = function FontFace(family, source, descriptors) {
      if (!(this instanceof FontFace)) throw new TypeError("Constructor FontFace requires 'new'");
      var d = descriptors || {};
      this.family = String(family);
      this.style = d.style || "normal"; this.weight = String(d.weight || "normal");
      this.stretch = d.stretch || "normal"; this.unicodeRange = d.unicodeRange || "U+0-10FFFF";
      this.variant = d.variant || "normal"; this.featureSettings = d.featureSettings || "normal";
      this.display = d.display || "auto"; this.ascentOverride = "normal"; this.descentOverride = "normal";
      this.lineGapOverride = "normal";
      this._src = null;
      if (typeof source === "string") {
        this._src = source;
      } else if (source && (source instanceof ArrayBuffer || ArrayBuffer.isView(source))) {
        try {
          var bytes = source instanceof ArrayBuffer ? new Uint8Array(source) : new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
          this._src = "url(" + URL.createObjectURL(new Blob([bytes])) + ")";
        } catch (e) { this._src = null; }
      }
      this.status = "unloaded";
      var self = this;
      this.loaded = new Promise(function (res) { self._resolve = res; });
    };
    FontFace.prototype.load = function () {
      this.status = "loaded";
      this._resolve(this);
      return this.loaded;
    };
    window.FontFace = FontFace;
  }
  var proto = {
    status: "loaded", size: 0, onloading: null, onloadingdone: null, onloadingerror: null,
    check: function () { return true; },
    // Resolves with the faces for the font's first family. check() says every font is
    // available, so load() must agree: an empty list means "no such font yet", and loaders
    // that poll load() until it is non-empty never finished (smartadserver.com: Adobe
    // Typekit's loader kept the page busy until the WebDriver timeout). When no face object
    // of ours matches, a loaded stand-in for the family is returned.
    load: function (font) {
      var m = /(?:^|\s)[\d.]+(?:px|pt|pc|em|rem|ex|ch|%|vw|vh|q|mm|cm|in)(?:\s*\/\s*\S+)?\s+(.+)$/i.exec(String(font || ""));
      var fam = (m ? m[1] : String(font || "")).split(",")[0].trim().replace(/^["']|["']$/g, "");
      var hit = faces.filter(function (f) { return String(f.family).replace(/^["']|["']$/g, "") === fam; });
      if (!hit.length && fam) {
        var stub = { family: fam, style: "normal", weight: "normal", stretch: "normal", unicodeRange: "U+0-10FFFF",
                     variant: "normal", featureSettings: "normal", display: "auto", status: "loaded" };
        stub.loaded = Promise.resolve(stub);
        stub.load = function () { return stub.loaded; };
        hit = [stub];
      }
      return Promise.resolve(hit);
    },
    add: function (face) {
      if (face && faces.indexOf(face) < 0) {
        faces.push(face);
        try { render(); } catch (e) { /* no document yet */ }
        if (face.status === "unloaded" && typeof face.load === "function") { try { face.load(); } catch (e) { /* ignore */ } }
      }
      return this;
    },
    "delete": function (face) {
      var i = faces.indexOf(face);
      if (i < 0) return false;
      faces.splice(i, 1);
      try { render(); } catch (e) { /* ignore */ }
      return true;
    },
    clear: function () { faces = []; try { render(); } catch (e) { /* ignore */ } },
    has: function (face) { return faces.indexOf(face) >= 0; },
    forEach: function (cb, thisArg) { faces.slice().forEach(function (f) { cb.call(thisArg, f, f, this); }, this); },
    values: function () { return faces.slice()[Symbol.iterator](); },
    keys: function () { return faces.slice()[Symbol.iterator](); },
    entries: function () { return faces.map(function (f) { return [f, f]; })[Symbol.iterator](); },
    addEventListener: noop, removeEventListener: noop,
    dispatchEvent: function () { return true; }
  };
  var set = document.fonts;
  if (!set) {
    set = {};
    Object.defineProperty(Document.prototype, "fonts", { get: function () { return set; }, configurable: true });
  }
  var ownSize = !("size" in set);
  for (var k in proto) {
    if (!(k in set)) {
      try { set[k] = proto[k]; } catch (e) { /* read-only native member */ }
    }
  }
  if (OurFontFace) {
    // The engine's own FontFaceSet methods only take engine FontFace objects: route ours here.
    ["add", "delete", "has"].forEach(function (name) {
      var native = set[name] !== proto[name] ? set[name] : null;
      try {
        Object.defineProperty(set, name, {
          configurable: true, writable: true,
          value: function (face) {
            if (face instanceof OurFontFace || !native) return proto[name].apply(set, arguments);
            return native.apply(set, arguments);
          }
        });
      } catch (e) { /* non-configurable */ }
    });
  }
  if (!(Symbol.iterator in set)) { try { set[Symbol.iterator] = proto.values; } catch (e) { /* ignore */ } }
  if (!set.ready || typeof set.ready.then !== "function") {
    try { Object.defineProperty(set, "ready", { value: Promise.resolve(set), configurable: true }); } catch (e) { /* ignore */ }
  }
  if (ownSize) Object.defineProperty(set, "size", { get: function () { return faces.length; }, configurable: true });
})();
