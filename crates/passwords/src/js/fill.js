// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Fill one saved login into the top-level document. Run by the shell (evaluate_javascript)
// only after the user picked an account in the browser's passwords panel. Self-contained
// (does not trust anything the page defined). Refuses if the document is no longer on the
// origin the user chose for (the tab navigated meanwhile). Never touches iframes.
(function (origin, user, pass) {
  try {
    if (window.top !== window || location.origin !== origin) return "origin";
    var setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
    function vis(el) {
      if (!el || el.disabled || el.readOnly) return false;
      var r = el.getBoundingClientRect();
      return r.width >= 2 && r.height >= 2;
    }
    function put(el, v) {
      setter.call(el, v);
      el.setAttribute("data-mb-pw-filled", "1");
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    }
    var a = document.activeElement;
    var pws = Array.prototype.filter.call(
      document.querySelectorAll('input[type="password"], input[data-mb-pw="1"]'), vis);
    var pw = null;
    if (a && a.tagName === "INPUT" && pws.indexOf(a) >= 0) pw = a;
    var root = (a && a.form) || (pw && pw.form) || null;
    if (!pw) {
      for (var i = 0; i < pws.length; i++) if (!root || pws[i].form === root) { pw = pws[i]; break; }
    }
    if (!pw) return "nofield";
    pw.setAttribute("data-mb-pw", "1");
    root = pw.form || pw.closest("form") || document;
    var inputs = root.querySelectorAll("input"), u = null;
    for (var j = 0; j < inputs.length; j++) {
      var el = inputs[j];
      if (el === pw) break;
      if (/^(text|email|tel|)$/.test(el.type) && vis(el)) u = el;
    }
    if (u && user) put(u, user);
    put(pw, pass);
    return "ok";
  } catch (e) { return "error"; }
})(__ORIGIN__, __USER__, __PASS__)
