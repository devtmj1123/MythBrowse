// MythBrowse compat: Element.prototype.checkVisibility (rule polyfill.check-visibility).
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (typeof Element !== "undefined" && typeof Element.prototype.checkVisibility !== "function") {
  Element.prototype.checkVisibility = function (options) {
    var o = options || {};
    if (!this.isConnected) return false;
    var cs;
    try { cs = getComputedStyle(this); } catch (e) { return false; }
    // No box (display:none on it or an ancestor, or display:contents) => not visible.
    if (!cs || cs.display === "none") return false;
    for (var el = this.parentElement; el; el = el.parentElement) {
      var s = getComputedStyle(el);
      if (s.display === "none") return false;
      if ((o.checkOpacity || o.opacityProperty) && s.opacity === "0") return false;
      if (s.contentVisibility === "hidden" && el !== this) return false;
    }
    if (cs.display === "contents") return false;
    if ((o.checkOpacity || o.opacityProperty) && cs.opacity === "0") return false;
    if ((o.checkVisibilityCSS || o.visibilityProperty) && (cs.visibility === "hidden" || cs.visibility === "collapse")) return false;
    return true;
  };
}
