// MythBrowse compat: Popover API (rule polyfill.popover).
// Gives elements with a popover attribute the hidden-until-shown behaviour, showPopover /
// hidePopover / togglePopover, popovertarget / popovertargetaction buttons, beforetoggle /
// toggle events, light dismiss (outside click, Esc) for popover=auto, and a :popover-open
// stand-in ([data-mb-popover-open]); el.matches(":popover-open") answers too. No top layer:
// open popovers get position:fixed with a very high z-index instead.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (typeof HTMLElement !== "undefined" && !("popover" in HTMLElement.prototype)) {
  var OPEN = "data-mb-popover-open";
  var HP = HTMLElement.prototype;
  var addCss = function () {
    var root = document.head || document.documentElement;
    if (!root || document.querySelector("style[data-mb-popover]")) return;
    var st = document.createElement("style");
    st.setAttribute("data-mb-popover", "");
    st.textContent = "[popover]:not([" + OPEN + "]){display:none!important}" +
      "[popover][" + OPEN + "]{position:fixed;inset:0;width:fit-content;height:fit-content;margin:auto;" +
      "border:solid;padding:.25em;overflow:auto;color:CanvasText;background-color:Canvas;z-index:2147483646}";
    root.insertBefore(st, root.firstChild);
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", addCss, { once: true });
  addCss();
  var stateOf = function (el) {
    var v = el.getAttribute("popover");
    if (v === null) return null;
    v = v.toLowerCase();
    return v === "manual" ? "manual" : v === "hint" ? "hint" : "auto";
  };
  var isOpen = function (el) { return el.hasAttribute(OPEN); };
  var fire = function (el, type, oldState, newState, cancelable) {
    var ev;
    try { ev = new Event(type, { cancelable: !!cancelable }); } catch (e) { ev = document.createEvent("Event"); ev.initEvent(type, false, !!cancelable); }
    try { ev.oldState = oldState; ev.newState = newState; } catch (e) { /* read-only */ }
    return el.dispatchEvent(ev);
  };
  var autoStack = [];
  var show = function (el) {
    if (stateOf(el) === null) throw new DOMException("Element does not have the popover attribute", "NotSupportedError");
    if (isOpen(el)) return;
    if (!el.isConnected) throw new DOMException("Popover is not connected", "InvalidStateError");
    if (!fire(el, "beforetoggle", "closed", "open", true)) return;
    if (stateOf(el) !== "manual") {
      // close auto popovers that are not ancestors of this one
      for (var i = autoStack.length - 1; i >= 0; i--) {
        var other = autoStack[i];
        if (other !== el && !other.contains(el)) hide(other);
      }
      autoStack.push(el);
    }
    addCss();
    el.setAttribute(OPEN, "");
    setTimeout(function () { fire(el, "toggle", "closed", "open", false); }, 0);
  };
  var hide = function (el) {
    if (stateOf(el) === null) throw new DOMException("Element does not have the popover attribute", "NotSupportedError");
    if (!isOpen(el)) return;
    fire(el, "beforetoggle", "open", "closed", false);
    el.removeAttribute(OPEN);
    var at = autoStack.indexOf(el);
    if (at >= 0) autoStack.splice(at, 1);
    setTimeout(function () { fire(el, "toggle", "open", "closed", false); }, 0);
  };
  Object.defineProperty(HP, "popover", {
    get: function () { return stateOf(this); },
    set: function (v) { if (v === null || v === undefined) this.removeAttribute("popover"); else this.setAttribute("popover", String(v)); },
    configurable: true, enumerable: true
  });
  HP.showPopover = function () { show(this); };
  HP.hidePopover = function () { hide(this); };
  HP.togglePopover = function (force) {
    var want = typeof force === "boolean" ? force : (force && typeof force === "object" && "force" in force ? !!force.force : !isOpen(this));
    if (want) show(this); else hide(this);
    return isOpen(this);
  };
  var targetOf = function (btn) {
    if (btn.popoverTargetElement instanceof Element) return btn.popoverTargetElement;
    var id = btn.getAttribute("popovertarget");
    return id ? document.getElementById(id) : null;
  };
  if (typeof HTMLButtonElement !== "undefined" && !("popoverTargetAction" in HTMLButtonElement.prototype)) {
    [HTMLButtonElement, typeof HTMLInputElement !== "undefined" ? HTMLInputElement : null].forEach(function (C) {
      if (!C) return;
      Object.defineProperty(C.prototype, "popoverTargetAction", {
        get: function () { var a = (this.getAttribute("popovertargetaction") || "").toLowerCase(); return a === "show" || a === "hide" ? a : "toggle"; },
        set: function (v) { this.setAttribute("popovertargetaction", v); }, configurable: true
      });
    });
  }
  var origMatches = Element.prototype.matches;
  Element.prototype.matches = function (sel) {
    if (typeof sel === "string" && sel.indexOf(":popover-open") >= 0) sel = sel.split(":popover-open").join("[" + OPEN + "]");
    return origMatches.call(this, sel);
  };
  ["querySelector", "querySelectorAll"].forEach(function (name) {
    [Document.prototype, Element.prototype, typeof DocumentFragment !== "undefined" ? DocumentFragment.prototype : null].forEach(function (P) {
      if (!P || typeof P[name] !== "function") return;
      var orig = P[name];
      P[name] = function (sel) {
        if (typeof sel === "string" && sel.indexOf(":popover-open") >= 0) sel = sel.split(":popover-open").join("[" + OPEN + "]");
        return orig.call(this, sel);
      };
    });
  });
  document.addEventListener("click", function (e) {
    var t = e.target;
    var btn = t && t.closest ? t.closest("button[popovertarget],input[popovertarget]") : null;
    if (btn && !btn.disabled) {
      var pop = targetOf(btn);
      if (pop && stateOf(pop) !== null) {
        var action = btn.popoverTargetAction || "toggle";
        if (action === "show") show(pop);
        else if (action === "hide") hide(pop);
        else if (isOpen(pop)) hide(pop); else show(pop);
        return;
      }
    }
    // light dismiss: a click outside the topmost auto popover closes it (and those above)
    for (var i = autoStack.length - 1; i >= 0; i--) {
      if (autoStack[i].contains(t)) break;
      hide(autoStack[i]);
    }
  }, true);
  document.addEventListener("keydown", function (e) {
    if ((e.key === "Escape" || e.key === "Esc") && autoStack.length) hide(autoStack[autoStack.length - 1]);
  }, true);
}
