// MythBrowse compat: CSS animation end state (rule site.css-animation-implicit-end).
// Two Servo bugs (stylo 0.15 servo/animation.rs + matching.rs):
//  1. When any stylesheet changes while a CSS animation runs (CSS-in-JS libraries such as
//     Griffel insert rules all the time), the keyframes are recomputed from the element's
//     *current* style, which already holds the animated value. A keyframe the @keyframes rule
//     leaves out (`@keyframes f{0%{opacity:0}}`: 100% means "the element's own value") takes
//     that mid-animation value, and with fill-mode forwards/both the element stays frozen there:
//     Microsoft's sign-in card stayed at opacity 0..0.6 (login.live.com, login.microsoftonline.com).
//  2. A finished animation is dropped, so the next stylesheet change or any change to the
//     element's animation-* properties starts it again from 0%.
//  3. A style-attribute change only swaps that one rule level, so an inline `animation-name: none`
//     leaves the last animated value in the element's style (it stayed at opacity 0 even with
//     an inline `opacity: 1`). Only a full selector re-match drops it.
// Fix: when an element's only animation ends and its end keyframe is implicit, its correct end
// state is its own style. The element gets a `data-mb-anim-end` attribute, which the rule's
// CSS (`[data-mb-anim-end]{animation-name:none!important}`, injected once at document start so
// it adds no later stylesheet change) matches: the attribute forces a full re-match (fixes 3),
// the animation is gone, and nothing can restart it (fixes 2). The attribute is removed again
// when the page changes the element's class or style attribute, so later animations the page
// starts still run. Copyright (c) 2026 JustMyth / Tan Ming Jing. SPDX-License-Identifier: MPL-2.0.
if (typeof document.addEventListener === "function" && !window.__mbAnimEndFix) {
  Object.defineProperty(window, "__mbAnimEndFix", { value: true });
  var cache = Object.create(null);
  var saved = typeof WeakMap === "function" ? new WeakMap() : null;
  var KEYFRAMES = 7;

  var findKeyframes = function (rules, name) {
    var found = null;
    for (var i = 0; rules && i < rules.length; i++) {
      var r = rules[i];
      if (r.type === KEYFRAMES && r.name === name) found = r;
      else if (r.cssRules && r.type !== KEYFRAMES) {
        var inner = findKeyframes(r.cssRules, name);
        if (inner) found = inner;
      }
    }
    return found;
  };

  // Which of the 0% / 100% keyframes an @keyframes rule spells out (null: rule not found).
  var keyframeEnds = function (name) {
    if (cache[name]) return cache[name];
    var rule = null, sheets = document.styleSheets;
    for (var i = 0; i < sheets.length; i++) {
      var rules = null;
      try { rules = sheets[i].cssRules; } catch (e) { continue; }
      var r = findKeyframes(rules, name);
      if (r) rule = r;
    }
    if (!rule) return null;
    var ends = { from: false, to: false };
    for (var k = 0; k < rule.cssRules.length; k++) {
      // Servo has no CSSKeyframeRule.keyText: read the selector off cssText ("0%, 50% { ... }").
      var kf = rule.cssRules[k];
      var keyText = typeof kf.keyText === "string" ? kf.keyText : String(kf.cssText).split("{")[0];
      var keys = keyText.split(",");
      for (var j = 0; j < keys.length; j++) {
        var key = keys[j].trim().toLowerCase();
        if (key === "from" || parseFloat(key) === 0) ends.from = true;
        if (key === "to" || parseFloat(key) === 100) ends.to = true;
      }
    }
    cache[name] = ends;
    return ends;
  };

  var list = function (value) {
    return String(value).split(",").map(function (s) { return s.trim(); });
  };

  // The keyframe an animation that ran `count` iterations in `dir` ends on.
  var endsOnTo = function (dir, count) {
    var n = Number(count);
    if (!isFinite(n) || n <= 0 || Math.floor(n) !== n) return null;
    var odd = n % 2 === 1;
    if (dir === "normal") return true;
    if (dir === "reverse") return false;
    if (dir === "alternate") return odd;
    if (dir === "alternate-reverse") return !odd;
    return null;
  };

  var ATTR = "data-mb-anim-end";
  var restore = function (el) {
    var mo = saved.get(el);
    if (!mo) return;
    saved.delete(el);
    mo.disconnect();
    el.removeAttribute(ATTR);
  };

  document.addEventListener("animationend", function (e) {
    var el = e.target;
    if (!saved || !el || el.nodeType !== 1 || e.pseudoElement || saved.has(el)) return;
    if (typeof MutationObserver !== "function") return;
    var cs = getComputedStyle(el);
    var names = list(cs.animationName);
    if (names.length !== 1 || names[0] !== e.animationName) return;
    var fill = list(cs.animationFillMode)[0];
    if (fill !== "forwards" && fill !== "both") return;
    var toEnd = endsOnTo(list(cs.animationDirection)[0], list(cs.animationIterationCount)[0]);
    if (toEnd === null) return;
    var ends = keyframeEnds(e.animationName);
    if (!ends || (toEnd ? ends.to : ends.from)) return;
    var mo = new MutationObserver(function () { restore(el); });
    saved.set(el, mo);
    el.setAttribute(ATTR, "");
    // Observe after our own change so it does not count as the page's.
    mo.observe(el, { attributes: true, attributeFilter: ["class", "style"] });
  }, true);
}
