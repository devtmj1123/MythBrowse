// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// MythBrowse password page script (a Servo user script in every web page; see
// crates/passwords/src/page.rs). Top-level documents only: never in iframes, so a login is
// never captured from or filled into a cross-origin frame.
//   * finds login forms (a visible password field and the username field before it) and marks
//     password fields with data-mb-pw="1" (the agent's page reader keeps redacting them even
//     if a "show password" button turns them into text fields);
//   * tells the shell, through console messages carrying a per-session nonce, that the page
//     has a login form, that the user (trusted events only) focused a login field or clicked
//     the key icon, and what was submitted, so the shell can offer "Save password?".
// It never receives a saved password: filling is a separate script the shell runs after the
// user picked an account in the browser's own passwords panel.
(function () {
  "use strict";
  var NONCE = "__MB_PW_NONCE__";
  if (window.top !== window) return;
  if (location.protocol !== "http:" && location.protocol !== "https:") return;
  if (location.hostname === "mythbrowse.internal") return;
  var KEY = "__MB_PW_HOOK__";
  if (Object.prototype.hasOwnProperty.call(window, KEY)) return;

  // Captured before any page script runs.
  var send = console.debug.bind(console);
  var stringify = JSON.stringify;
  var MARK = "\u0000mbpw:" + NONCE + ":";
  function post(msg) { try { send(MARK + stringify(msg)); } catch (e) { /* ignore */ } }

  function visible(el) {
    if (!el || el.disabled || el.readOnly) return false;
    var r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    var cs = getComputedStyle(el);
    return cs.visibility !== "hidden" && cs.display !== "none";
  }
  function isPw(el) {
    return !!el && el.tagName === "INPUT" &&
      (el.type === "password" || el.getAttribute("data-mb-pw") === "1");
  }
  var TEXTY = { text: 1, email: 1, tel: 1, "": 1, url: 1 };
  function isUserField(el) {
    if (!el || el.tagName !== "INPUT" || !(el.type in TEXTY)) return false;
    var hay = ((el.getAttribute("autocomplete") || "") + " " + (el.name || "") + " " + (el.id || "") +
      " " + (el.getAttribute("placeholder") || "")).toLowerCase();
    return !/search|query|captcha|otp|one-time|coupon|promo/.test(hay);
  }
  function scope(pw) { return pw.form || pw.closest("form") || document; }
  function passwords(root) {
    var all = root.querySelectorAll('input[type="password"], input[data-mb-pw="1"]'), out = [];
    for (var i = 0; i < all.length; i++) {
      all[i].setAttribute("data-mb-pw", "1");
      if (visible(all[i]) || all[i].value) out.push(all[i]);
    }
    return out;
  }
  // The text field right before the first password field of `root` (document order).
  function usernameFor(pw) {
    var root = scope(pw);
    var inputs = root.querySelectorAll("input"), best = null;
    for (var i = 0; i < inputs.length; i++) {
      if (inputs[i] === pw) break;
      if (isUserField(inputs[i]) && (visible(inputs[i]) || inputs[i].value)) best = inputs[i];
    }
    if (!best) {
      var ac = root.querySelector('input[autocomplete~="username"], input[autocomplete~="email"]');
      if (ac && isUserField(ac)) best = ac;
    }
    return best;
  }

  var announced = -1;
  function scan() {
    var pws = passwords(document);
    var n = pws.length;
    if (n !== announced) {
      announced = n;
      var signup = false;
      for (var i = 0; i < pws.length; i++) {
        if ((pws[i].getAttribute("autocomplete") || "").indexOf("new-password") >= 0) signup = true;
      }
      post({ t: "forms", n: n, signup: signup || n > 1, o: location.href });
    }
    return pws;
  }

  // ---- capture on submit -------------------------------------------------------------
  var lastSent = "";
  function capture(root, how) {
    var pws = passwords(root === document ? document : root);
    if (!pws.length) return;
    var filled = [];
    for (var i = 0; i < pws.length; i++) if (pws[i].value) filled.push(pws[i]);
    if (!filled.length) return;
    // Sign-up / change forms: the new password (last one; the confirmation repeats it).
    var pw = filled[filled.length - 1];
    var user = usernameFor(filled[0]);
    var u = user ? String(user.value || "").trim() : "";
    var key = u + "\u0001" + pw.value;
    if (key === lastSent) return;
    lastSent = key;
    var form = pw.form;
    post({
      t: "submit", how: how, o: location.href, u: u, p: String(pw.value),
      a: form ? String(form.action || "") : "", n: filled.length,
    });
  }
  document.addEventListener("submit", function (e) {
    capture(e.target && e.target.querySelectorAll ? e.target : document, "submit");
  }, true);
  // Script-driven logins (no submit event): Enter in a login field, or a click on a button
  // near the password field. Trusted events only.
  document.addEventListener("keydown", function (e) {
    if (!e.isTrusted || e.key !== "Enter") return;
    var t = e.target;
    if (isPw(t) || isUserField(t)) {
      var pws = passwords(document);
      if (pws.length) capture(isPw(t) ? scope(t) : scope(pws[0]), "enter");
    }
  }, true);
  document.addEventListener("click", function (e) {
    if (!e.isTrusted) return;
    var b = e.target && e.target.closest ? e.target.closest('button, input[type="submit"], input[type="image"], [role="button"]') : null;
    if (!b) return;
    var pws = passwords(document);
    if (!pws.length) return;
    var root = scope(pws[0]);
    if (root === document || root.contains(b)) capture(root, "click");
  }, true);

  // ---- focus suggestion + key icon -----------------------------------------------------
  var has = 0, icon = null, iconFor = null;
  function hideIcon() { if (icon) icon.style.display = "none"; iconFor = null; }
  function placeIcon(field) {
    if (!has || !visible(field)) { hideIcon(); return; }
    if (!icon) {
      var host = document.createElement("mb-pw-key");
      var root = host.attachShadow({ mode: "closed" });
      var b = document.createElement("button");
      b.type = "button";
      b.title = "Saved passwords (MythBrowse)";
      b.setAttribute("aria-label", "Saved passwords");
      b.style.cssText = "all:initial;position:fixed;z-index:2147483647;width:22px;height:22px;border-radius:6px;" +
        "background:rgba(255,255,255,.92);box-shadow:0 1px 3px rgba(15,30,60,.25);cursor:pointer;display:flex;" +
        "align-items:center;justify-content:center";
      b.innerHTML = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#1d2b4a" stroke-width="1.8" ' +
        'stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="15" r="4"/><path d="M10.8 12.2 20 3m-3 3 3 3m-6 0 2 2"/></svg>';
      b.addEventListener("mousedown", function (e) { e.preventDefault(); }, true);
      b.addEventListener("click", function (e) {
        e.preventDefault(); e.stopPropagation();
        if (e.isTrusted) post({ t: "key", o: location.href });
      }, true);
      root.appendChild(b);
      (document.documentElement || document.body).appendChild(host);
      icon = b;
    }
    var r = field.getBoundingClientRect();
    icon.style.left = Math.round(r.right - 26) + "px";
    icon.style.top = Math.round(r.top + (r.height - 22) / 2) + "px";
    icon.style.display = "flex";
    iconFor = field;
  }
  document.addEventListener("focusin", function (e) {
    var t = e.target;
    if (!(isPw(t) || (isUserField(t) && passwords(scope(t)).length))) { hideIcon(); return; }
    scan();
    placeIcon(t);
    if (e.isTrusted) post({ t: "focus", o: location.href, field: isPw(t) ? "password" : "username" });
  }, true);
  document.addEventListener("focusout", function (e) {
    if (e.target === iconFor) setTimeout(function () {
      if (document.activeElement !== iconFor) hideIcon();
    }, 150);
  }, true);
  window.addEventListener("scroll", function () { if (iconFor) placeIcon(iconFor); }, true);

  Object.defineProperty(window, KEY, {
    value: Object.freeze({
      // The shell says how many saved logins this page has (0 hides the key icon).
      has: function (n) { has = +n || 0; if (iconFor) placeIcon(iconFor); },
    }),
    enumerable: false, configurable: false, writable: false,
  });

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", scan);
  else scan();
  window.addEventListener("load", scan);
})();
