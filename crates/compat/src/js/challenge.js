(function (doc) {
  // SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
  // MythBrowse human-verification detector: is this page a bot / security check (Cloudflare
  // "Just a moment", Turnstile / reCAPTCHA / hCaptcha / DataDome / Arkose challenges, IP
  // blocks)? Returns the phrase or challenge host that matched, or null.
  //
  // DETECTION ONLY. MythBrowse never solves, clicks, bypasses or automates these checks: the
  // browser hands them to the person (crates/ai/src/handoff.rs). Shared by the compat probe
  // (js/probe.js fills it in) and the agent's hand-off check (mythbrowse-ai includes this
  // file), so both see the same thing. Read-only: no timers, no observers, nothing changed.
  "use strict";
  function safe(f, d) { try { var v = f(); return v === undefined ? d : v; } catch (e) { return d; } }
  var body = doc.body;
  var text = safe(function () { return body ? (body.innerText || body.textContent || "") : ""; }, "");
  var textLen = text.replace(/\s+/g, " ").trim().length;
  var title = safe(function () { return String(doc.title || "").toLowerCase(); }, "");
  // Phrases that only appear on challenge pages (title or short page text), and generic
  // words that count only in the title (a footer's "protected by reCAPTCHA" must not).
  var BOT = [
    "just a moment", "checking your browser", "verify you are human", "verifying you are human",
    "confirm you are human", "confirm that you are human", "are you a robot",
    "unusual traffic", "blocked by network security", "your request has been blocked",
    "pardon our interruption", "安全验证", "人机验证", "访问验证", "滑动验证", "visitor system"
  ];
  var BOT_TITLE = ["captcha", "access denied", "attention required", "security check", "request blocked"];
  // Challenge pages are short. A long page is content (an article about captchas, a news
  // story titled "Access denied"), never a check: a wrong match would pause the agent.
  if (textLen >= 3000) return null;
  var low = text.toLowerCase();
  for (var b = 0; b < BOT.length; b++) {
    if (title.indexOf(BOT[b]) >= 0 || low.indexOf(BOT[b]) >= 0) return BOT[b];
  }
  for (var t = 0; t < BOT_TITLE.length; t++) {
    if (title.indexOf(BOT_TITLE[t]) >= 0) return BOT_TITLE[t];
  }
  // Cloudflare's interstitial markup (the text may still be empty while its script runs).
  var CF = ["#challenge-running", "#challenge-stage", "#cf-challenge-running", "#challenge-form", "#cf-please-wait"];
  for (var k = 0; k < CF.length; k++) {
    if (safe(function () { return doc.querySelector(CF[k]); }, null)) return "challenge page (cloudflare)";
  }
  // A challenge served in a frame (DataDome on nytimes.com, Turnstile interstitials, a
  // reCAPTCHA / hCaptcha checkbox) on an otherwise near-empty page: the frame's text is not
  // ours to read. Only frames a person can see count: reCAPTCHA v3 / invisible widgets keep an
  // iframe on many normal pages and are not a check the user has to pass.
  var CH = ["captcha-delivery.com", "challenges.cloudflare.com", "hcaptcha.com", "arkoselabs.com",
    "google.com/recaptcha", "recaptcha.net/recaptcha"];
  var frames = safe(function () { return doc.querySelectorAll("iframe[src]"); }, []);
  for (var fi = 0; fi < frames.length; fi++) {
    var f = frames[fi];
    var src = String(safe(function () { return f.getAttribute("src"); }, "") || "");
    for (var ci = 0; ci < CH.length; ci++) {
      if (src.indexOf(CH[ci]) < 0) continue;
      if (/[?&#]size=invisible\b/.test(src)) continue;
      var r = safe(function () { return f.getBoundingClientRect(); }, null);
      var shown = !r || (r.width >= 20 && r.height >= 20);
      var vis = safe(function () {
        var cs = getComputedStyle(f);
        return cs.visibility !== "hidden" && cs.display !== "none" && cs.opacity !== "0";
      }, true);
      if (shown && vis) return "challenge frame (" + CH[ci] + ")";
    }
  }
  return null;
})
