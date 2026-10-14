// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// First-run welcome on the new tab (home.html #welcome): what MythBrowse is, Myth's model
// (opens the Model sheet with the choice made; "Later" skips), importing from another
// browser, the look (glass + motion, previewed live), and privacy. Shown once: the shell
// keeps the profile pref "onboarded" (/api/prefs, /api/prefs_save?onboarded=1); Skip, Esc
// and finishing all set it. home.html#welcome opens it again (Settings > About).
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const wrap = $("welcome");
  if (!wrap || !window.MythChat || !window.MythHome) return;
  const api = window.MythChat.api;
  const home = window.MythHome;
  const panel = wrap.querySelector(".sheet");
  const steps = Array.prototype.slice.call(wrap.querySelectorAll(".wl-step"));
  const dots = Array.prototype.slice.call(wrap.querySelectorAll(".wl-steps i"));
  const next = $("wl-next");
  const back = $("wl-back");
  const LAST = steps.length - 1;
  const forced = location.hash === "#welcome";
  const q = new URLSearchParams(location.search);
  // A tab opened for a prompt or a saved conversation is not the place for a welcome.
  if (!forced && (q.has("q") || q.has("c") || q.has("id"))) return;

  let step = 0;
  let configured = false;
  // The look as saved, and as chosen here (previewed until Next saves it).
  let saved = null;
  let look = null;

  function label() {
    if (step === 0) return "Get started";
    if (step === 1) return configured ? "Next" : "Later";
    if (step === LAST) return "Start browsing";
    return "Next";
  }

  function show(n) {
    step = Math.max(0, Math.min(LAST, n));
    steps.forEach((el) => { el.hidden = Number(el.getAttribute("data-step")) !== step; });
    dots.forEach((d, i) => { d.classList.toggle("on", i === step); d.classList.toggle("done", i < step); });
    back.hidden = step === 0;
    next.textContent = label();
    const cur = steps[step];
    if (window.JMMotion && JMMotion.rise && cur) JMMotion.rise(cur);
    if (step === 3) loadLook();
  }

  function modelNote(s) {
    configured = !!(s && s.ok && s.configured);
    const note = $("wl-model");
    note.hidden = !configured;
    if (configured) $("wl-model-text").textContent = "Myth uses " + s.model + (s.local ? " on this PC" : "");
    $("wl-openai").classList.toggle("on", configured && !s.local);
    $("wl-ollama").classList.toggle("on", configured && !!s.local);
    if (step === 1) next.textContent = label();
  }

  function open() {
    wrap.hidden = false;
    wrap.classList.remove("scrim-out");
    wrap.classList.add("scrim-in");
    document.body.classList.add("sheet-open");
    if (window.JMMotion && panel) JMMotion.pop(panel, $("state-pill"), { from: 0.6 });
    if (panel) panel.focus();
    show(step);
  }

  function hide() {
    return new Promise((resolve) => {
      wrap.classList.remove("scrim-in");
      wrap.classList.add("scrim-out");
      document.body.classList.remove("sheet-open");
      const done = () => { wrap.hidden = true; wrap.classList.remove("scrim-out"); resolve(); };
      if (window.JMMotion && panel) JMMotion.unpop(panel, { to: 0.9 }).then(() => { panel.hidden = false; done(); });
      else done();
    });
  }

  // Skip / Esc / Start browsing: never again (until Settings > About asks).
  function finish() {
    revertLook();
    api("prefs_save", { onboarded: 1 });
    if (location.hash === "#welcome") history.replaceState(null, "", location.pathname + location.search);
    hide().then(() => { if (home.focusPrompt) home.focusPrompt(); });
  }

  // ---- step 1: Myth's model -----------------------------------------------------------------
  function setUp(kind) {
    hide().then(() => {
      const sheet = $("settings");
      home.settings.open($("state-pill"), kind);
      // Back to the welcome when the Model sheet closes (saved or not).
      const mo = new MutationObserver(() => {
        if (!sheet.hidden) return;
        mo.disconnect();
        api("ai_settings").then((s) => { modelNote(s); open(); });
      });
      mo.observe(sheet, { attributes: true, attributeFilter: ["hidden"] });
    });
  }
  $("wl-openai").addEventListener("click", () => setUp("openai"));
  $("wl-ollama").addEventListener("click", () => setUp("ollama"));

  // ---- step 2: import ----------------------------------------------------------------------
  $("wl-import").addEventListener("click", () => {
    api("prefs_save", { onboarded: 1 }).then(() => { location.href = "passwords.html#import-panel"; });
  });

  // ---- step 3: the look --------------------------------------------------------------------
  const glassSeg = $("wl-glass");
  const motionSeg = $("wl-motion");
  function mark(seg, attr, value) {
    seg.querySelectorAll(".seg-btn").forEach((b) => b.classList.toggle("on", b.getAttribute(attr) === String(value)));
  }
  function loadLook() {
    if (saved) return;
    api("prefs").then((p) => {
      if (!p || p.ok === false) return;
      saved = { v: p.transparency, reduce: !!p.reduce_transparency_effective, motion: p.motion === "system" ? p.effective : p.motion };
      look = { v: saved.v, motion: saved.motion };
      mark(glassSeg, "data-v", [25, 50, 75].indexOf(saved.v) >= 0 ? saved.v : "");
      mark(motionSeg, "data-m", saved.motion);
    });
  }
  glassSeg.addEventListener("click", (e) => {
    const b = e.target.closest(".seg-btn");
    if (!b || !look) return;
    look.v = Number(b.getAttribute("data-v"));
    mark(glassSeg, "data-v", look.v);
    if (window.JMGlass) JMGlass.setLook(look.v, saved.reduce);
    api("glass_params", { v: look.v, reduce: saved.reduce ? 1 : 0 });
  });
  motionSeg.addEventListener("click", (e) => {
    const b = e.target.closest(".seg-btn");
    if (!b || !look) return;
    look.motion = b.getAttribute("data-m");
    mark(motionSeg, "data-m", look.motion);
  });
  function lookChanged() {
    return !!(saved && look && (look.v !== saved.v || look.motion !== saved.motion));
  }
  function saveLook() {
    if (!lookChanged()) return;
    const params = {};
    if (look.v !== saved.v) params.transparency = look.v;
    if (look.motion !== saved.motion) params.motion = look.motion;
    api("prefs_save", params).then((r) => {
      if (r && r.ok === false) return;
      if (params.motion) document.documentElement.setAttribute("data-motion", r.effective || look.motion);
      saved = { v: look.v, reduce: saved.reduce, motion: look.motion };
    });
  }
  function revertLook() {
    if (!saved || !look || look.v === saved.v) return;
    look.v = saved.v;
    if (window.JMGlass) JMGlass.setLook(saved.v, saved.reduce);
    api("glass_params", { reset: 1 });
  }

  // ---- navigation ----------------------------------------------------------------------------
  next.addEventListener("click", () => {
    if (step === 3) saveLook();
    if (step === LAST) finish();
    else show(step + 1);
  });
  back.addEventListener("click", () => show(step - 1));
  $("wl-skip").addEventListener("click", finish);
  wrap.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.preventDefault(); finish(); } });

  api("ai_settings").then(modelNote);
  api("prefs").then((p) => {
    if (!p || p.ok === false) return;
    if (forced || (!p.onboarded && !p.welcome_suppressed)) open();
  });
  window.MythWelcome = { open: () => { step = 0; open(); } };
})();
