// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Browser chrome logic. Rust -> JS: window.mb.render(state) / window.mb.focusOmnibox().
// JS -> Rust: fetch('/api/<op>?...') on the internal origin (see crates/engine/src/chrome.rs).
// The window is frameless: this page draws the title bar, and asks the shell to move
// (window_drag), minimize, maximize/restore and close the window.
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const tabsEl = $("tabs");
  const omnibox = $("omnibox");
  const wrap = $("omnibox-wrap");
  const hint = $("hint");
  const I = window.JMIcons;
  let state = null;
  let editing = false;

  const TOKEN = document.querySelector('meta[name="mb-token"]').content;

  function api(op, params) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    return fetch("/api/" + op + "?" + q.toString())
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }

  // What will Enter do with this text? Mirrors omnibox::resolve roughly (display only).
  function guessMode(text) {
    const t = text.trim();
    if (!t) return "ai";
    if (t.startsWith("?")) return "search";
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(t) || /^(about|data|file):/i.test(t)) return "url";
    if (!/\s/.test(t) && (/^localhost(:\d+)?(\/|$)/i.test(t) || /^[^\s/]+\.[a-z]{2,}(:\d+)?(\/.*)?$/i.test(t))) return "url";
    return "ai";
  }

  // What the address bar shows on its left (icon + optional label), and the hint on its right
  // while editing. The AI is one of three things Enter can do, so it gets a quiet hint only.
  const MODES = {
    ai: { icon: "search", role: "dim", label: "", hint: "Ask Myth" },
    search: { icon: "search", role: "soft", label: "", hint: "Search the web" },
    url: { icon: "globe", role: "soft", label: "", hint: "Go" },
    secure: { icon: "lock", role: "soft", label: "" },
    insecure: { cls: "insecure", icon: "alert", role: "bad", label: "Not secure" },
    internal: { mark: true, label: "" },
    page: { icon: "globe", role: "soft", label: "" },
  };

  function setMode(name) {
    const m = MODES[name] || MODES.ai;
    $("lead").className = "lead " + (m.cls || "");
    const ic = $("lead-icon");
    ic.textContent = "";
    if (m.mark) {
      const mark = document.createElement("span");
      mark.className = "mark";
      mark.innerHTML = "<i></i><i></i><i></i>";
      ic.appendChild(mark);
    } else {
      ic.appendChild(I.icon(m.icon, { size: 15, role: m.role }));
    }
    $("lead-label").textContent = m.label;
    if (editing && m.hint && omnibox.value.trim()) {
      $("hint-text").textContent = m.hint;
      hint.hidden = false;
    } else {
      hint.hidden = true;
    }
  }

  // Mode chip for the page that is showing (not being edited).
  function pageMode(s) {
    if (!s.url) return "ai";
    if (s.internal) return "internal";
    if (/^https:/i.test(s.url)) return "secure";
    if (/^http:/i.test(s.url)) return "insecure";
    return "page";
  }

  function hostOf(url) {
    const m = /^[a-z][a-z0-9+.-]*:\/\/([^/?#:]+)/i.exec(url || "");
    return m ? m[1].replace(/^www\./i, "") : "";
  }

  function tabIcon(t) {
    const box = document.createElement("span");
    if (t.loading && !t.sleeping) {
      box.className = "loading-dot";
      return box;
    }
    box.className = "tab-icon";
    if (/^https:\/\/mythbrowse\.internal\/library/.test(t.url)) {
      box.appendChild(I.icon("history", { size: 15, role: "soft" }));
    } else if (/^https:\/\/mythbrowse\.internal\//.test(t.url) || !t.url) {
      const mark = document.createElement("span");
      mark.className = "mark";
      mark.innerHTML = "<i></i><i></i><i></i>";
      box.appendChild(mark);
    } else if (t.sleeping) {
      box.appendChild(I.icon("moon", { size: 14, role: "soft" }));
    } else {
      const host = hostOf(t.url);
      const site = document.createElement("span");
      site.className = "site fav";
      // An IP address (a local server, 127.0.0.1) says nothing: use the page title's letter.
      const ip = /^[\d.]+$|^\[|:/.test(host);
      const letter = (ip ? (t.title || "").replace(/^[^a-z0-9]+/i, "") : host)[0] || host[0] || "?";
      site.textContent = letter.toUpperCase();
      site.style.backgroundColor = favColor(host);
      box.appendChild(site);
    }
    return box;
  }

  // No favicons yet: a letter tile in a colour of its own per site (stable for the host), like
  // the approved design's favicons.
  const FAV = ["#2f6b46", "#1f2430", "#3d5fb4", "#a3452f", "#6b4fb8", "#1f7a8c", "#9a6a12", "#b0365f"];
  function favColor(host) {
    let h = 0;
    for (let i = 0; i < host.length; i++) h = (h * 31 + host.charCodeAt(i)) >>> 0;
    return FAV[h % FAV.length];
  }

  // Tabs are keyed by id so opening / closing / switching animate (width + fade, CSS) instead
  // of the whole strip being rebuilt.
  const tabEls = new Map();
  let quietTabs = true; // first render / theme rebuild: no entrance animation

  function tabEl(t) {
    const el = document.createElement("div");
    el.setAttribute("role", "tab");
    el.appendChild(document.createElement("span"));
    const title = document.createElement("span");
    title.className = "title";
    el.appendChild(title);
    const close = document.createElement("button");
    close.className = "close";
    close.title = "Close tab (Ctrl+W)";
    close.setAttribute("aria-label", "Close tab");
    close.appendChild(I.icon("close", { size: 14, role: "soft" }));
    close.addEventListener("click", (e) => {
      e.stopPropagation();
      api("close_tab", { id: el._id });
    });
    el.appendChild(close);
    el.addEventListener("mousedown", (e) => {
      if (e.button === 0 && !el.classList.contains("active") && !e.target.closest(".close")) api("activate_tab", { id: el._id });
    });
    // Middle click closes the tab. Servo 0.1.4 fires no auxclick, only mouseup.
    el.addEventListener("mouseup", (e) => { if (e.button === 1) api("close_tab", { id: el._id }); });
    el._id = t.id;
    return el;
  }

  // Tab motion as in the approved design: the active tab is marked by one glass indicator that
  // springs to it (PillMover 340/34, a small stretch along its velocity); a new tab grows in
  // (opacity + scale .6 / translateY 8, bouncy) while its neighbours slide over (FLIP, snappy);
  // a closed tab fades and shrinks (160 ms), then its neighbours close the gap (FLIP, snappy)
  // and the indicator moves on. Reduced: opacity only (the indicator jumps and fades in).
  // Off: nothing moves. CSS keyframes / transitions only (Servo has no Web Animations).
  const tabInd = document.createElement("span");
  tabInd.className = "tab-ind";
  tabInd.setAttribute("data-glass", "c");
  tabsEl.appendChild(tabInd);
  const indPill = window.JMMotion ? JMMotion.PillMover(tabInd, 340, 34, 0.07, 5000) : null;
  let indTimer = 0;
  function placeInd(jump) {
    clearTimeout(indTimer);
    const t = tabsEl.querySelector(".tab.active:not(.leaving)");
    if (!t || !indPill) { tabInd.style.opacity = "0"; return; }
    tabInd.style.opacity = "";
    const mode = motionMode();
    indPill.to(t.offsetLeft, t.offsetWidth, jump || mode === "off");
    if (!jump && mode === "reduced") replay(tabInd, "ind-fade");
    if (!jump && window.JMGlass) JMGlass.track(700);
  }
  function tabEdges() {
    const m = new Map();
    tabsEl.querySelectorAll(".tab").forEach((t) => m.set(t, t.getBoundingClientRect().left));
    m.set($("new-tab"), $("new-tab").getBoundingClientRect().left);
    return m;
  }
  // FLIP: everything that moved slides from where it was with the snappy spring.
  function flip(before) {
    if (motionMode() !== "full") return;
    before.forEach((x0, t) => {
      if (!t.isConnected || t.classList.contains("leaving")) return;
      const dx = x0 - t.getBoundingClientRect().left;
      if (Math.abs(dx) < 0.5) return;
      t.style.transition = "none";
      t.style.transform = "translateX(" + dx.toFixed(1) + "px)";
      void t.offsetWidth;
      t.style.transition = "transform var(--d-snappy) var(--spr-snappy)";
      t.style.transform = "";
      clearTimeout(t._flipT);
      t._flipT = setTimeout(() => { t.style.transition = ""; }, 900);
    });
    if (window.JMGlass) JMGlass.track(700);
  }

  function renderTabs(s) {
    let prev = tabInd;
    const mode = motionMode();
    const animate = mode !== "off" && !quietTabs;
    const jumpInd = quietTabs;
    quietTabs = false;
    const before = tabEdges();
    let leaving = false;
    // Closed tabs first: they stay where they are while they fade out (FLIP after removal).
    const ids = new Set(s.tabs.map((t) => t.id));
    for (const [id, el] of tabEls) {
      if (ids.has(id)) continue;
      tabEls.delete(id);
      if (animate) {
        leaving = true;
        el.classList.add("leaving");
        el.classList.remove("active");
        setTimeout(() => {
          const b = tabEdges();
          el.remove();
          flip(b);
          placeInd(false);
          fitTitles();
        }, mode === "full" ? 160 : 180);
      } else {
        el.remove();
      }
    }
    for (const t of s.tabs) {
      let el = tabEls.get(t.id);
      const fresh = !el;
      if (fresh) {
        el = tabEl(t);
        tabEls.set(t.id, el);
      }
      el.className = "tab" + (t.active ? " active" : "") + (t.sleeping ? " sleeping" : "") +
        (t.loading ? " loading" : "") + (t.pinned ? " pinned" : "") + (el._entering ? " tab-in" : "");
      el._pinned = !!t.pinned;
      el.title = t.title + (t.sleeping ? " (sleeping)" : "");
      const key = (t.loading && !t.sleeping ? "L" : "") + (t.sleeping ? "S" : "") + t.url;
      if (el._iconKey !== key) {
        el.replaceChild(tabIcon(t), el.firstChild);
        el._iconKey = key;
      }
      const titleEl = el.querySelector(".title");
      if (titleEl.getAttribute("data-full") !== t.title) {
        titleEl.textContent = t.title;
        titleEl.setAttribute("data-full", t.title);
      }
      let dot = el.querySelector(".tab-myth");
      if (t.myth) {
        if (!dot) {
          dot = document.createElement("span");
          el.insertBefore(dot, el.querySelector(".close"));
        }
        dot.className = "tab-myth " + t.myth;
        dot.title = t.myth === "waiting" ? "Myth needs your OK" : "Myth is working in this tab";
        dot.setAttribute("aria-label", dot.title);
        dot.setAttribute("role", "img");
      } else if (dot) {
        dot.remove();
      }
      // A background tab whose site wants a human check: a quiet badge, no pop-up (the tab
      // in front never gets one; the shell sends verify only for background tabs).
      let vb = el.querySelector(".tab-verify");
      if (t.verify && !t.myth) {
        if (!vb) {
          vb = document.createElement("span");
          vb.className = "tab-verify";
          vb.title = "This site wants you to confirm you\u2019re human";
          vb.setAttribute("aria-label", vb.title);
          vb.setAttribute("role", "img");
          el.insertBefore(vb, el.querySelector(".close"));
        }
      } else if (vb) {
        vb.remove();
      }
      let want = prev.nextSibling;
      while (want && want !== el && want.classList.contains("leaving")) want = want.nextSibling;
      if (want !== el) tabsEl.insertBefore(el, want);
      prev = el;
      if (fresh && animate) {
        el._entering = true;
        el.classList.add("tab-in");
        setTimeout(() => { el._entering = false; el.classList.remove("tab-in"); }, 700);
      }
    }
    flip(before);
    fitTitles();
    // A closing tab first fades out; the indicator moves on once it is gone (as in the design).
    if (!leaving) placeInd(jumpInd || !animate);
  }

  // Servo has no text-overflow: titles that don't fit end in an ellipsis (motion.js), fitted
  // again once a tab's width transition ends.
  function fitTitles() {
    if (!window.JMMotion) return;
    tabsEl.querySelectorAll(".tab:not(.leaving):not(.entering)").forEach((el) => {
      const title = el.querySelector(".title");
      if (title) window.JMMotion.ellipsis(title, title.getAttribute("data-full") || title.textContent);
    });
  }
  window.addEventListener("resize", () => { fitTitles(); placeInd(true); });

  // ---- Myth pill / approval card --------------------------------------------------------
  // Floats over the bottom centre of the page card at the rectangle the shell gives
  // (state.frame.overlay, CSS px): the shell composes exactly that rounded rectangle over the
  // page and sends pointer events inside it here. Running: dot, "Myth", the step, Stop.
  // Waiting for an OK: a card with what / where / why and Allow / Don't allow (Esc = Don't
  // allow, handled by the shell; Enter never allows unless Allow itself has focus).
  // Finished: "Done" + Open answer + close; it closes itself after a few seconds.
  const MYTH_STATE = {
    thinking: "Reading the page",
    controlling: "Working on this page",
    waiting: "Needs your OK",
    done: "Done",
    error: "Something went wrong",
    stopped: "Stopped",
  };
  const DONE_CLOSE_MS = 8000;
  let mythKey = "";
  let doneTimer = 0;

  // Servo has no text-overflow: cut `text` until it fits `el` (one line, or `lines` lines of
  // `lineH` px) and end it with an ellipsis.
  function fitText(el, text, lines, lineH) {
    el.textContent = text;
    const over = () => (lines ? el.scrollHeight > lines * lineH + 1 : el.scrollWidth > el.clientWidth + 1);
    if (!over()) return;
    let lo = 0, hi = text.length;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      el.textContent = text.slice(0, mid).replace(/\s+$/, "") + "\u2026";
      if (over()) hi = mid - 1; else lo = mid;
    }
    el.textContent = text.slice(0, lo).replace(/\s+$/, "") + "\u2026";
    el.title = text;
  }

  function hostLabel(url) {
    const h = hostOf(url);
    return h || "this page";
  }

  function mythButton(parent, label, cls, fn, title) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = cls || "";
    b.textContent = label;
    if (title) b.title = title;
    b.addEventListener("click", (e) => { e.stopPropagation(); fn(); });
    parent.appendChild(b);
    return b;
  }

  // Myth's surface is content-sized like the approved design: the pill as wide as its
  // content (the shell centres a rectangle of the width the chrome measured, myth_size?w=),
  // the approval card as tall as its content (myth_size?h=). The chrome draws at its measured
  // size right away (centred / bottom-anchored in the shell's rectangle), so a resize never
  // shows a gap while the shell catches up.
  let sentPillW = 0, sentCardH = 0, mythKind = "", mythTimer = 0, mythPill = null;
  function sizeRequest(kind, v) {
    v = Math.round(v * 10) / 10;
    if (kind === "pill") {
      if (Math.abs(v - sentPillW) < 0.6) return;
      sentPillW = v;
      api("myth_size", { w: v });
    } else {
      if (Math.abs(v - sentCardH) < 0.6) return;
      sentCardH = v;
      api("myth_size", { h: v });
    }
  }
  function motionMode() { return document.documentElement.getAttribute("data-motion") || "full"; }
  function geom(el, g) {
    el.style.left = g.x + "px";
    el.style.top = g.y + "px";
    el.style.width = g.w + "px";
    el.style.height = g.h + "px";
    el.style.borderRadius = g.r + "px";
  }
  // Restart a CSS animation class on `el` (Servo has no Web Animations).
  function replay(el, cls, delay) {
    el.classList.remove(cls);
    void el.offsetWidth;
    if (delay) el.style.animationDelay = delay + "ms";
    el.classList.add(cls);
  }
  const MORPH_CLS = ["morphing", "mm-pop", "mm-squash", "mm-fade"];
  function endMorph(el) {
    clearTimeout(mythTimer);
    el.style.transition = "";
    el.style.transform = "";
    el.style.transformOrigin = "";
    MORPH_CLS.forEach((c) => el.classList.remove(c));
  }
  // Pill <-> approval card (prototype morph()): the surface's rectangle springs from the old
  // shape to the new one, anchored bottom-centre (the prototype's clip-path inset, done with
  // the box itself since the shell's glass follows the element's box), the content laid out
  // at its final size inside it. Full: morph spring + a small scale; reduced: the new content
  // fades in; off (or no shell glass, where only the final rectangle is composed): instant.
  // The pill's width bounce when its text changes (design setPill(): scaleX(old / new) with
  // the bouncy spring, the new text rising in). On card -> pill this is what the design shows
  // too (it replaces the card's own squash), measured from the pill width before the card.
  // v0.4: the morph is a FLIP: the box takes its final geometry at once and a transform
  // (translate + scale from the old rectangle) springs back to none, so no frame re-lays-out
  // the surface (left/top/width/height transitions did, every frame).
  const GEOM_T = "transform var(--d-morph) var(--spr-morph)";
  function flipBox(el, from, to) {
    if (!to.w || !to.h) return;
    const sx = from.w / to.w, sy = from.h / to.h, dx = from.x - to.x, dy = from.y - to.y;
    el.style.transformOrigin = "0 0";
    el.style.transition = "none";
    el.style.transform = "translate3d(" + dx.toFixed(1) + "px," + dy.toFixed(1) + "px,0) scale(" + sx.toFixed(4) + "," + sy.toFixed(4) + ")";
    void el.offsetWidth;
    el.style.transition = GEOM_T;
    el.style.transform = "";
  }
  function bounceX(el, ratio, extra) {
    el.style.transition = extra || "none";
    el.style.transform = "scaleX(" + ratio.toFixed(3) + ")";
    void el.offsetWidth;
    el.style.transition = (extra ? extra + ", " : "") + "transform var(--d-bouncy) var(--spr-bouncy)";
    el.style.transform = "";
  }
  function pillChange(el, inner, prevW, w) {
    if (motionMode() !== "full" || !state || state.glass === "off") return;
    const step = inner.querySelector(".my-step");
    if (step) replay(step, "mp-step-in");
    if (prevW && Math.abs(prevW - w) > 0.5) {
      el.classList.add("morphing");
      bounceX(el, prevW / w);
      clearTimeout(mythTimer);
      mythTimer = setTimeout(() => endMorph(el), 700);
    }
    if (window.JMGlass) JMGlass.track(700);
  }
  function morph(el, from, to, open, inner, prevPillW) {
    const m = motionMode();
    if (m === "off" || !from || !state || state.glass === "off") return;
    if (m === "reduced") { replay(inner, "mm-fade"); return; }
    el.classList.add("morphing");
    geom(el, to);
    flipBox(el, from, to);
    // (No mm-pop / mm-squash here: an animation on the box would override the FLIP transform.)
    if (open) {
      replay(inner, "mc-in-anim");
      inner.querySelectorAll(".mc-head, .mc-what, .mc-detail, .mc-actions").forEach((k, i) => {
        k.classList.add("mc-kid");
        k.style.animationDelay = 50 + i * 40 + "ms";
      });
    } else {
      replay(inner, "mp-in-anim");
      const step = inner.querySelector(".my-step");
      if (step) replay(step, "mp-step-in");
    }
    if (window.JMGlass) JMGlass.track(800);
    mythTimer = setTimeout(() => endMorph(el), 760);
  }

  function renderMyth(s) {
    const el = $("myth");
    const m = s.myth;
    const o = s.card && s.frame && s.frame.overlay;
    if (!m || !o || o.kind === "compat" || o.kind === "passwords") {
      el.hidden = true;
      mythKey = "";
      mythKind = "";
      mythPill = null;
      clearTimeout(doneTimer);
      endMorph(el);
      return;
    }
    const pending = m.approval && m.approval.action ? m.approval : null;
    // Paused on a human-verification check: the person completes it, Myth never touches it.
    const handoff = !pending && m.handoff && m.handoff.host ? m.handoff : null;
    const isCard = !!((pending || handoff) && o.kind === "card");
    const kind = isCard ? "card" : "pill";
    // Rebuild only when what it says or where it is anchored changes: the shell resizing the
    // rectangle to the size measured here keeps the anchor (pill: centre; card: bottom), so a
    // running morph is not cut short by it.
    const anchor = isCard ? [o.x, o.w, o.y + o.h] : [o.x + o.w / 2, o.y, o.h];
    const key = JSON.stringify([m, kind, anchor.map((v) => Math.round(v)), window.innerWidth]);
    if (key === mythKey && !el.hidden) return;
    mythKey = key;
    clearTimeout(doneTimer);
    // Where the surface is now (for the morph), before it is rebuilt.
    const prevKind = el.hidden ? "" : mythKind;
    const r0 = el.getBoundingClientRect();
    const prevPill = mythPill;
    const from = prevKind && prevKind !== kind ? { x: r0.left, y: r0.top, w: r0.width, h: r0.height, r: prevKind === "card" ? 28 : 25 } : null;
    endMorph(el);
    mythKind = kind;
    el.textContent = "";
    el.className = "myth " + m.state + (isCard ? " card" : "");
    // The approval card floats over the page (soft shadow); the pill at rest is chrome glass.
    el.setAttribute("data-glass", isCard ? "f" : "c");
    el.hidden = false;
    const dot = document.createElement("span");
    dot.className = "my-dot";
    const mark = () => {
      const k = document.createElement("span");
      k.className = "mark";
      k.innerHTML = "<i></i><i></i><i></i>";
      return k;
    };
    if (isCard && handoff) {
      // Verification card (approval-card look): who / where, what to do, then Cancel task /
      // Show tab. Myth continues by itself once the check is gone (the shell re-checks).
      el.setAttribute("role", "alertdialog");
      el.setAttribute("aria-label", "Verification needed on " + handoff.host);
      el.removeAttribute("aria-live");
      el.classList.add("handoff");
      const inner = document.createElement("div");
      inner.className = "mc-in";
      el.appendChild(inner);
      const head = document.createElement("div");
      head.className = "mc-head";
      const tile = document.createElement("span");
      tile.className = "mc-mark";
      tile.appendChild(mark());
      head.appendChild(tile);
      const who = document.createElement("div");
      who.className = "mc-who";
      const b = document.createElement("b");
      b.textContent = "Verification needed";
      who.appendChild(b);
      const where = document.createElement("span");
      where.className = "mc-where";
      who.appendChild(where);
      head.appendChild(who);
      inner.appendChild(head);
      const what = document.createElement("div");
      what.className = "mc-what";
      inner.appendChild(what);
      const detail = document.createElement("div");
      detail.className = "mc-detail";
      const note = document.createElement("div");
      note.className = "mc-row mc-note";
      detail.appendChild(note);
      inner.appendChild(detail);
      const actions = document.createElement("div");
      actions.className = "mc-actions";
      inner.appendChild(actions);
      mythButton(actions, "Cancel task", "my-deny", () => api("handoff_cancel", { conv: m.conv }), "Stop this task");
      if (!handoff.here) {
        mythButton(actions, "Show tab", "my-allow", () => api("handoff_show", { conv: m.conv }), "Bring the page with the check to the front");
      }
      const w = o.w;
      el.style.left = o.x + "px";
      el.style.width = w + "px";
      el.style.height = "auto";
      inner.style.width = w + "px";
      inner.style.marginLeft = -w / 2 + "px";
      fitText(where, handoff.host);
      fitText(what, "Verification needed on " + handoff.host + " \u2014 complete it yourself, Myth will continue.", 3, 20);
      fitText(note, handoff.here
        ? "The check is on this page. Myth is paused and does not touch it."
        : "Myth is paused and does not touch the check. Show tab opens it here.", 2, 18);
      const maxH = Math.max(150, Math.min(420, o.y + o.h - 96));
      const h = Math.min(maxH, Math.ceil(inner.offsetHeight));
      inner.style.height = h + "px";
      sizeRequest("card", h);
      const to = { x: o.x, y: o.y + o.h - h, w: w, h: h, r: 28 };
      geom(el, to);
      morph(el, from, to, true, inner);
      return;
    }
    if (isCard) {
      // Approval card (the approved design): the mark on its tile, who is asking and where,
      // the question, what it involves, then Don't allow / Allow as two equal pills. Esc =
      // Don't allow (the shell); Stop stays on the pill and in Tasks.
      el.setAttribute("role", "alertdialog");
      el.setAttribute("aria-label", "Myth wants to: " + pending.action);
      el.removeAttribute("aria-live");
      const inner = document.createElement("div");
      inner.className = "mc-in";
      el.appendChild(inner);
      const head = document.createElement("div");
      head.className = "mc-head";
      const tile = document.createElement("span");
      tile.className = "mc-mark";
      tile.appendChild(mark());
      head.appendChild(tile);
      const who = document.createElement("div");
      who.className = "mc-who";
      const b = document.createElement("b");
      b.textContent = "Myth needs your OK";
      who.appendChild(b);
      const where = document.createElement("span");
      where.className = "mc-where";
      who.appendChild(where);
      head.appendChild(who);
      inner.appendChild(head);
      const what = document.createElement("div");
      what.className = "mc-what";
      inner.appendChild(what);
      const detail = document.createElement("div");
      detail.className = "mc-detail";
      const row = (label) => {
        const r = document.createElement("div");
        r.className = "mc-row";
        const l = document.createElement("span");
        l.className = "mc-label";
        l.textContent = label;
        const v = document.createElement("span");
        v.className = "mc-value";
        r.appendChild(l);
        r.appendChild(v);
        detail.appendChild(r);
        return v;
      };
      const onEl = row("Where");
      // A navigation shows its full destination URL, never shortened (navguard).
      const destEl = pending.dest ? row("Opens") : null;
      if (destEl) destEl.parentNode.classList.add("mc-dest");
      const whyEl = pending.reason ? row("Why") : null;
      inner.appendChild(detail);
      const actions = document.createElement("div");
      actions.className = "mc-actions";
      inner.appendChild(actions);
      const answer = (allow) => api("chat_approve", { conv: m.conv, request: pending.id, allow: allow ? "1" : "0" });
      mythButton(actions, "Don\u2019t allow", "my-deny", () => answer(false), "Don\u2019t allow (Esc)");
      mythButton(actions, "Allow", "my-allow", () => answer(true), "Let Myth do this once");
      // Lay out at the card's width, measure the content's height.
      const w = o.w;
      el.style.left = o.x + "px";
      el.style.width = w + "px";
      el.style.height = "auto";
      inner.style.width = w + "px";
      inner.style.marginLeft = -w / 2 + "px";
      fitText(where, hostLabel(pending.url));
      fitText(what, pending.action + "?", 2, 20);
      fitText(onEl, pending.url || hostLabel(pending.url));
      if (destEl) destEl.textContent = pending.dest;
      if (whyEl) fitText(whyEl, "Myth asks first because " + pending.reason + ".", 2, 18);
      const maxH = Math.max(150, Math.min(420, o.y + o.h - 96));
      const h = Math.min(maxH, Math.ceil(inner.offsetHeight));
      inner.style.height = h + "px";
      sizeRequest("card", h);
      const to = { x: o.x, y: o.y + o.h - h, w: w, h: h, r: 28 };
      geom(el, to);
      morph(el, from, to, true, inner);
      return;
    }
    // Pill: the mark and "Myth", a hairline, the status dot and step, then Stop (round).
    el.setAttribute("role", "status");
    el.setAttribute("aria-live", "polite");
    el.removeAttribute("aria-label");
    const inner = document.createElement("div");
    inner.className = "mp-in";
    el.appendChild(inner);
    inner.appendChild(mark());
    const name = document.createElement("span");
    name.className = "my-name";
    name.textContent = "Myth";
    inner.appendChild(name);
    const sep = document.createElement("span");
    sep.className = "my-sep";
    inner.appendChild(sep);
    inner.appendChild(dot);
    const step = document.createElement("span");
    step.className = "my-step";
    inner.appendChild(step);
    const actions = document.createElement("span");
    actions.className = "my-actions";
    inner.appendChild(actions);
    let text;
    if (pending) text = "Needs your OK: " + pending.action;
    else if (handoff) text = "Waiting for you: verification on " + handoff.host;
    else if (m.running) text = m.step || MYTH_STATE[m.state] || "Working";
    else text = (MYTH_STATE[m.state] || m.state) + (m.title ? " \u00b7 " + m.title : "");
    if (m.running) {
      const stop = mythButton(actions, "", "my-icon my-round", () => api("chat_stop", { conv: m.conv }), "Stop Myth");
      stop.setAttribute("aria-label", "Stop");
      const ic = document.createElement("span");
      ic.appendChild(I.icon("stop", { size: 15, role: "icon" }));
      stop.appendChild(ic);
    } else {
      mythButton(actions, m.state === "done" ? "Open answer" : "Chat", "my-open", () => api("myth_open", { conv: m.conv }), "Open the conversation");
      const x = mythButton(actions, "", "my-icon my-round", () => api("myth_dismiss"), "Close");
      x.setAttribute("aria-label", "Close");
      const ic = document.createElement("span");
      ic.appendChild(I.icon("close", { size: 16, role: "soft" }));
      x.appendChild(ic);
      if (m.state === "done") {
        // Close by itself after a few seconds, unless the user is on it.
        doneTimer = setTimeout(function tick() {
          if (el.contains(document.activeElement) || el.matches(":hover")) {
            doneTimer = setTimeout(tick, 2000);
            return;
          }
          api("myth_dismiss");
        }, DONE_CLOSE_MS);
      }
    }
    // Content width (the step at most 300 px, as in the design), within the card.
    const gap = (s.frame && s.frame.gap) || 8;
    const maxW = Math.max(160, Math.min(420, window.innerWidth - 2 * gap - 48));
    step.textContent = text;
    el.style.width = maxW + "px";
    el.style.height = o.h + "px";
    el.style.left = "0px";
    inner.style.left = "0px";
    const natural = Math.ceil(inner.offsetWidth) + 1;
    const w = Math.max(160, Math.min(maxW, natural));
    sizeRequest("pill", w);
    const cx = o.x + o.w / 2;
    const to = { x: Math.round((cx - w / 2) * 10) / 10, y: o.y, w: w, h: o.h, r: o.h / 2 };
    // Content at its final size, anchored bottom-centre (so a morph only moves the edges).
    inner.style.left = "";
    inner.style.width = w + "px";
    inner.style.marginLeft = -w / 2 + "px";
    geom(el, to);
    fitText(step, text);
    mythPill = { w: w, text: text };
    if (from) morph(el, from, to, false, inner, prevPill && prevPill.w);
    else if (prevKind === "pill" && prevPill && prevPill.text !== text) pillChange(el, inner, prevPill.w, w);
  }

  // ---- Tasks panel ---------------------------------------------------------------------
  // The approved design's floating #tasks: opened from the toolbar Tasks button (Ctrl+J in
  // the shell), it floats over the top right of the page area at the rectangle the shell gives
  // (state.frame.panel). It is as tall as its content (tasks_panel?h=, within the area).
  // The newest task (a running one first) is a card: live thumbnail, title, status, progress,
  // the run's steps (finished ones ticked, the current one marked), Peek / Take over / Stop.
  // Older tasks are one-line rows. Data from /api/tasks (polled while open), the thumbnail from
  // /api/task_shot. "All tasks" opens the full Tasks page.
  const tasksPanel = $("tasks-panel");
  let tasksData = null, tasksJson = "", tasksTimer = 0, tasksOpen = false, sentTasksH = 0;
  let peek = null, peekTimer = 0;
  const taskShots = {};
  const TASK_STATUS = { running: "Running", waiting: "Needs your OK", done: "Done", stopped: "Stopped by you", error: "Something went wrong", idle: "Idle" };

  function ago(ms) {
    const m = Math.round((Date.now() - ms) / 60000);
    return m < 1 ? "just now" : m < 60 ? m + " min ago" : Math.round(m / 60) + " h ago";
  }
  function tpButton(parent, iconName, label, cls, fn, title) {
    const b = el("button", "tp-btn" + (cls ? " " + cls : ""));
    b.type = "button";
    if (iconName) b.appendChild(I.icon(iconName, { size: iconName === "stop" ? 13 : 15, role: cls === "danger" ? "bad" : "icon" }));
    if (label) b.appendChild(document.createTextNode(label));
    if (title) b.title = title;
    b.addEventListener("click", (e) => { e.stopPropagation(); fn(); });
    parent.appendChild(b);
    return b;
  }
  function shotInto(img, tab, fresh) {
    const have = taskShots[tab.id];
    if (have && img.src !== have.image) img.src = have.image;
    if (!fresh && (!tab.shot || (have && have.version >= tab.shot))) return Promise.resolve();
    return api("task_shot", { tab: tab.id, fresh: fresh ? "1" : "0" }).then((r) => {
      if (r && r.ok && r.version) { taskShots[tab.id] = { version: r.version, image: r.image }; img.src = r.image; }
    });
  }
  function taskCardEl(t) {
    const card = el("div", "tp-task");
    const top = el("div", "tp-top");
    const tab = (t.tabs || []).find((x) => x.open) || (t.tabs || [])[0];
    const thumb = el("div", "tp-thumb");
    if (tab) {
      const img = el("img");
      img.alt = "";
      thumb.appendChild(img);
      shotInto(img, tab, false);
    }
    top.appendChild(thumb);
    const tt = el("div", "tp-tt");
    const title = el("b", "tp-title");
    tt.appendChild(title);
    const steps = t.steps || [];
    const live = t.status === "running" || t.status === "waiting";
    const st = el("span", "tp-status " + t.status);
    st.appendChild(el("i", "tp-dot"));
    st.appendChild(document.createTextNode((TASK_STATUS[t.status] || t.status) + (steps.length || live ? " \u00b7 step " + (steps.length + (live ? 1 : 0)) : "")));
    tt.appendChild(st);
    top.appendChild(tt);
    card.appendChild(top);
    // Progress: finished steps of the steps seen so far plus the one running (the run has no
    // fixed plan, so it fills as steps finish and completes when the task is done).
    const prog = el("div", "tp-prog");
    const bar = el("i");
    const p = t.status === "done" ? 1 : live ? steps.length / (steps.length + 2) : steps.length ? steps.length / (steps.length + 1) : 0;
    bar.style.transform = "scaleX(" + (Math.round(Math.max(0.04, p) * 1000) / 1000) + ")";
    prog.appendChild(bar);
    card.appendChild(prog);
    const list = el("ul", "tp-steps");
    steps.forEach((x) => {
      const li = el("li", x.state === "done" ? "ok" : "bad");
      const s = el("span", "s");
      s.appendChild(I.icon(x.state === "done" ? "check" : "close", { size: 11, stroke: 2.6, role: x.state === "done" ? "inkText" : "soft" }));
      li.appendChild(s);
      const tx = el("span", "tx");
      li.appendChild(tx);
      list.appendChild(li);
      tx._text = x.text;
    });
    if (live && t.step) {
      const li = el("li", "now");
      li.appendChild(el("span", "s"));
      const tx = el("span", "tx");
      tx._text = t.step;
      li.appendChild(tx);
      list.appendChild(li);
    }
    if (list.children.length) card.appendChild(list);
    const btns = el("div", "tp-btns");
    if (tab && tab.open) {
      tpButton(btns, "eye", "Peek", "", () => openPeekTab(tab), "See the page Myth is working on");
      tpButton(btns, "takeover", "Take over", "", () => { api("task_takeover", { tab: tab.id }); api("tasks_panel", { open: "0" }); }, "Open the page as your tab and stop Myth");
    }
    if (t.running) tpButton(btns, "stop", "Stop", "danger", () => api("task_stop", { conv: t.conv }).then(refreshTasks), "Stop this task");
    if (btns.children.length) card.appendChild(btns);
    card._title = title;
    card._text = t.title;
    return card;
  }
  function taskMiniEl(t) {
    const row = el("div", "tp-mini");
    row.appendChild(I.icon(t.status === "done" ? "check" : t.status === "error" ? "alert" : "stop", { size: 16, role: "soft" }));
    const b = el("b");
    row.appendChild(b);
    row.appendChild(el("span", "tp-when", (TASK_STATUS[t.status] || t.status).replace(" by you", "") + (t.started ? " \u00b7 " + ago(t.started) : "")));
    row._title = b;
    row._text = t.title;
    return row;
  }
  function openPeekTab(tab) {
    peek = tab;
    renderTasksBody(true);
    clearTimeout(peekTimer);
    const tick = () => {
      if (!peek || !tasksOpen) return;
      const img = tasksPanel.querySelector(".tp-peek img");
      if (img) shotInto(img, peek, true).then(() => { peekTimer = setTimeout(tick, 1500); });
    };
    tick();
  }
  function closePeek() { peek = null; clearTimeout(peekTimer); renderTasksBody(true); }

  // Build the panel's content (header + body) at the shell's width; then measure its height.
  function renderTasksBody(force, entering) {
    if (!tasksOpen) return;
    const json = JSON.stringify([tasksData, peek && peek.id]);
    if (!force && json === tasksJson) return;
    tasksJson = json;
    const body0 = tasksPanel.querySelector(".tp-body");
    const keep = body0 ? body0.scrollTop : 0;
    tasksPanel.textContent = "";
    const head = el("div", "tp-head");
    const h3 = el("h3", "", "Tasks");
    const kbd = el("kbd", "", "Ctrl J");
    h3.appendChild(kbd);
    head.appendChild(h3);
    const x = tpButton(head, "close", "", "tp-icon", () => api("tasks_panel", { open: "0" }), "Close (Esc)");
    x.setAttribute("aria-label", "Close");
    tasksPanel.appendChild(head);
    const body = el("div", "tp-body");
    tasksPanel.appendChild(body);
    const tasks = (tasksData || []).slice();
    const kids = [];
    if (!tasksData) body.appendChild(el("div", "tp-empty", "Loading\u2026"));
    else if (!tasks.length) {
      body.appendChild(el("div", "tp-empty", "No tasks yet. Ask Myth to do something on a page, and you can follow it here."));
    } else {
      // A running task first, else the newest.
      tasks.sort((a, b) => (b.running ? 1 : 0) - (a.running ? 1 : 0));
      tasks.forEach((t, i) => {
        const k = i === 0 ? taskCardEl(t) : taskMiniEl(t);
        body.appendChild(k);
        kids.push(k);
      });
    }
    const foot = el("button", "tp-all", "All tasks");
    foot.type = "button";
    foot.addEventListener("click", (e) => { e.stopPropagation(); api("open_tasks"); });
    body.appendChild(foot);
    if (peek) {
      const pk = el("div", "tp-peek");
      const ph = el("div", "tp-head");
      const pt = el("h3");
      pt.appendChild(I.icon("eye", { size: 17, role: "icon" }));
      pt.appendChild(document.createTextNode("Peek \u00b7 live"));
      ph.appendChild(pt);
      const pc = tpButton(ph, "close", "", "tp-icon", closePeek, "Close peek");
      pc.setAttribute("aria-label", "Close peek");
      pk.appendChild(ph);
      const big = el("div", "tp-big");
      const img = el("img");
      img.alt = "";
      const have = taskShots[peek.id];
      if (have) img.src = have.image;
      big.appendChild(img);
      pk.appendChild(big);
      tasksPanel.appendChild(pk);
    }
    layoutTasks();
    // Titles / steps: cut to two lines / one line (Servo has no text-overflow).
    tasksPanel.querySelectorAll(".tp-task, .tp-mini").forEach((k) => fitText(k._title, k._text, k.classList.contains("tp-task") ? 3 : 0, 18));
    tasksPanel.querySelectorAll(".tp-steps .tx").forEach((tx) => fitText(tx, tx._text));
    layoutTasks();
    body.scrollTop = keep;
    if (entering && motionMode() === "full") {
      // The content settles in just behind the panel (design: 40 ms + 30 ms per item).
      kids.concat([foot]).forEach((k, i) => { k.style.animationDelay = 40 + i * 30 + "ms"; k.classList.add("tp-kid"); });
    }
    if (window.JMGlass) JMGlass.kick();
  }
  // Size: the shell's rectangle (x, y, w; max height), content height measured here.
  function layoutTasks() {
    const p = state && state.frame && state.frame.panel;
    if (!p) return;
    tasksPanel.style.left = p.x + "px";
    tasksPanel.style.top = p.y + "px";
    tasksPanel.style.width = p.w + "px";
    tasksPanel.style.borderRadius = p.r + "px";
    tasksPanel.style.height = "auto";
    const bottom = state.card ? (state.frame.gap || 8) + 12 : 12;
    const maxH = Math.max(120, window.innerHeight - p.y - bottom);
    tasksPanel.style.maxHeight = maxH + "px";
    const natural = Math.ceil(tasksPanel.scrollHeight);
    const h = Math.min(maxH, Math.max(120, natural));
    tasksPanel.style.height = h + "px";
    const req = Math.round(h * 10) / 10;
    if (Math.abs(req - sentTasksH) > 0.6) { sentTasksH = req; api("tasks_panel", { h: req }); }
  }
  function refreshTasks() {
    clearTimeout(tasksTimer);
    if (!tasksOpen) return Promise.resolve();
    return api("tasks").then((r) => {
      if (!tasksOpen) return;
      if (r && r.ok) { tasksData = r.tasks || []; renderTasksBody(false); }
      const busy = (tasksData || []).some((t) => t.running);
      tasksTimer = setTimeout(refreshTasks, busy ? 1000 : 3000);
    });
  }
  function renderTasks(s) {
    const p = s.frame && s.frame.panel;
    $("tasks").classList.toggle("on", !!p);
    $("tasks").setAttribute("aria-expanded", p ? "true" : "false");
    if (!p) {
      if (!tasksOpen) return;
      tasksOpen = false;
      peek = null;
      clearTimeout(tasksTimer);
      clearTimeout(peekTimer);
      sentTasksH = 0;
      if (window.JMMotion) JMMotion.unpop(tasksPanel); else tasksPanel.hidden = true;
      return;
    }
    tasksPanel.classList.toggle("on-internal", !s.card);
    if (!tasksOpen) {
      tasksOpen = true;
      tasksJson = "";
      tasksPanel.hidden = false;
      renderTasksBody(true, true);
      if (window.JMMotion) JMMotion.pop(tasksPanel, $("tasks"), { from: 0.5 });
      refreshTasks();
      return;
    }
    layoutTasks();
  }

  // ---- Site compat: toolbar indicator + panel -------------------------------------------
  const COMPAT_STATUS = {
    ok: "No problems found",
    degraded: "Some parts may not work",
    broken: "This page looks broken",
  };
  const SOURCE_LABEL = { builtin: "Built-in", remote: "Signed", local: "Yours" };
  const DOES_LABEL = {
    ua: "browser name", css: "styles", js: "script", block: "blocks scripts",
    reader: "reader view", simplified: "simplified view",
  };
  let compatKey = "";

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function cpButton(parent, label, cls, fn, title, disabled) {
    const b = el("button", "cp-btn" + (cls ? " " + cls : ""), label);
    b.type = "button";
    if (title) b.title = title;
    if (disabled) b.disabled = true;
    b.addEventListener("click", (e) => { e.stopPropagation(); fn(); });
    parent.appendChild(b);
    return b;
  }
  function compatAct(op, params) {
    return api(op, params).then((r) => {
      if (r && r.ok === false && r.error) compatError = r.error;
      else compatError = "";
      if (state) renderCompat(state, true);
      return r;
    });
  }
  let compatError = "";

  function renderCompatButton(c) {
    const b = $("compat");
    b.hidden = !c;
    if (!c) return;
    // Site / own rules active here (every-site shims are not listed) or a patch on preview.
    const fixed = c.preview || (c.rules || []).some((r) => !r.off);
    const level = c.paused ? "paused" : (c.level || "");
    b.className = "nav compat-btn" + (level && level !== "ok" ? " lv-" + level : "") + (fixed ? " fixed" : "") + (c.open ? " open" : "");
    b.title = "Site compatibility" + (c.paused ? ": paused on this site" : c.level ? ": " + COMPAT_STATUS[c.level].toLowerCase() : c.checking ? ": checking" : "");
    b.setAttribute("aria-expanded", c.open ? "true" : "false");
  }

  // ---- Passwords: toolbar key + panel ----------------------------------------------------
  // state.passwords: {available, locked, site, forms, count, accounts:[{id, username, site,
  // kind}], open, prompt:{site, username, update}|null, myth, notice}. Usernames only.
  let pwKey = "";
  let pwError = "";

  function pwAct(op, params) {
    return api(op, params).then((r) => {
      pwError = r && r.ok === false && r.error ? r.error : "";
      if (state) renderPasswords(state, true);
      return r;
    });
  }

  function renderPwButton(p) {
    const b = $("pw");
    const show = !!(p && (p.forms > 0 || p.count > 0 || p.prompt));
    b.hidden = !show;
    if (!show) return;
    b.className = "nav pw-btn" + (p.prompt ? " ask" : p.count > 0 ? " saved" : "") + (p.open || p.prompt ? " open" : "");
    b.title = p.prompt ? "Save password?" : p.count > 0 ? "Passwords (" + p.count + " saved for this site)" : "Passwords";
    b.setAttribute("aria-expanded", p.open || p.prompt ? "true" : "false");
  }

  function renderPasswords(s, force) {
    const p = s.passwords;
    renderPwButton(p);
    const panel = $("pw-panel");
    const o = s.card && s.frame && s.frame.overlay;
    if (!p || !o || o.kind !== "passwords" || !(p.open || p.prompt)) {
      panel.hidden = true;
      pwKey = "";
      return;
    }
    const key = JSON.stringify([p, o, pwError]);
    if (!force && key === pwKey && !panel.hidden) return;
    pwKey = key;
    panel.style.left = o.x + "px";
    panel.style.top = o.y + "px";
    panel.style.width = o.w + "px";
    panel.style.height = o.h + "px";
    panel.style.borderRadius = o.r + "px";
    panel.textContent = "";
    const opening = panel.hidden;
    panel.hidden = false;
    if (opening && window.JMMotion) JMMotion.pop(panel, $("pw"), { from: 0.5 });

    const head = el("div", "cp-head");
    const ic = el("span", "cp-ic");
    ic.appendChild(I.icon("key", { size: 18, role: "icon" }));
    head.appendChild(ic);
    const ttl = el("div", "cp-title");
    const pr = p.prompt;
    ttl.appendChild(el("div", "cp-name", pr ? (pr.update ? "Update password?" : "Save password?") : "Passwords"));
    ttl.appendChild(el("div", "cp-host", pr ? pr.site : p.site));
    head.appendChild(ttl);
    const close = () => api(pr ? "pw_prompt" : "pw_panel", pr ? { choice: "not_now" } : { open: "0" });
    const x = cpButton(head, "", "cp-icon", close, "Close (Esc)");
    x.appendChild(I.icon("close", { size: 16, role: "soft" }));
    x.setAttribute("aria-label", "Close");
    panel.appendChild(head);

    const body = el("div", "cp-body");
    panel.appendChild(body);
    if (pr) {
      const card = el("div", "cp-card");
      const row = (k, v, cls) => {
        const r = el("div", "pw-field");
        r.appendChild(el("span", "pw-k", k));
        r.appendChild(el("span", "pw-v" + (cls ? " " + cls : ""), v));
        card.appendChild(r);
      };
      row("Username", pr.username || "(none)");
      row("Password", "\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022", "pw-dots");
      body.appendChild(card);
      const acts = el("div", "cp-row");
      const save = cpButton(acts, pr.update ? "Update" : "Save", "primary", () => pwAct("pw_prompt", { choice: "save" }));
      if (!pr.update) cpButton(acts, "Never for this site", "", () => pwAct("pw_prompt", { choice: "never" }));
      cpButton(acts, "Not now", "", () => pwAct("pw_prompt", { choice: "not_now" }));
      body.appendChild(acts);
      body.appendChild(el("div", "cp-fine", "Saved encrypted on this computer, for your account only. Myth never sees saved passwords."));
      if (opening) setTimeout(() => { try { save.focus(); } catch (e) { /* ignore */ } }, 30);
    } else {
      if (p.myth) body.appendChild(el("div", "cp-warn", "Myth is working in this tab. Stop it to fill a password."));
      const accts = p.accounts || [];
      if (!accts.length) body.appendChild(el("div", "cp-soft", "No saved passwords for " + p.site + "."));
      accts.forEach((a) => {
        const b = el("button", "pw-acct");
        b.type = "button";
        b.disabled = !!p.myth;
        b.appendChild(el("span", "pw-av", ((a.username || "?")[0] || "?").toUpperCase()));
        const who = el("span", "pw-who");
        who.appendChild(el("div", "pw-user", a.username || "(no username)"));
        if (a.kind !== "exact") who.appendChild(el("div", "pw-from", "Saved for " + a.site));
        b.appendChild(who);
        b.appendChild(el("span", "pw-fill", "Fill"));
        b.title = "Fill this login into the page";
        b.addEventListener("click", (e) => { e.stopPropagation(); pwAct("pw_fill", { id: String(a.id) }); });
        body.appendChild(b);
      });
      if (pwError || p.notice) body.appendChild(el("div", "cp-error", pwError || p.notice));
      const tools = el("div", "cp-row");
      cpButton(tools, "Manage passwords", "small", () => api("navigate", { input: "https://mythbrowse.internal/passwords.html" }));
      body.appendChild(tools);
    }
  }

  function renderCompat(s, force) {
    const c = s.compat;
    renderCompatButton(c);
    const panel = $("compat-panel");
    const o = s.card && s.frame && s.frame.overlay;
    if (!c || !c.open || !o || o.kind !== "compat") {
      panel.hidden = true;
      compatKey = "";
      return;
    }
    const key = JSON.stringify([c, o, compatError]);
    if (!force && key === compatKey && !panel.hidden) return;
    compatKey = key;
    const scroll = panel.querySelector(".cp-body");
    const keepScroll = scroll ? scroll.scrollTop : 0;
    panel.style.left = o.x + "px";
    panel.style.top = o.y + "px";
    panel.style.width = o.w + "px";
    panel.style.height = o.h + "px";
    panel.style.borderRadius = o.r + "px";
    panel.textContent = "";
    const opening = panel.hidden;
    panel.hidden = false;
    if (opening && window.JMMotion) JMMotion.pop(panel, $("compat"), { from: 0.5 });

    // Header: icon, title, host, close.
    const head = el("div", "cp-head");
    const ic = el("span", "cp-ic");
    ic.appendChild(I.icon("compat", { size: 18, role: "icon" }));
    head.appendChild(ic);
    const ttl = el("div", "cp-title");
    ttl.appendChild(el("div", "cp-name", "Site compatibility"));
    ttl.appendChild(el("div", "cp-host", c.host));
    head.appendChild(ttl);
    const x = cpButton(head, "", "cp-icon", () => api("compat_panel", { open: "0" }), "Close (Esc)");
    x.setAttribute("aria-label", "Close");
    x.appendChild(I.icon("close", { size: 16, role: "soft" }));
    panel.appendChild(head);

    const body = el("div", "cp-body");
    panel.appendChild(body);

    // Status.
    const st = el("div", "cp-status " + (c.paused ? "paused" : c.level || "checking"));
    st.appendChild(el("i", "cp-dot"));
    st.appendChild(el("span", "cp-status-text",
      c.paused ? "Compat fixes are paused on this site"
        : c.blocked ? "The site is showing a security check"
        : c.level ? COMPAT_STATUS[c.level]
        : c.checking ? "Checking this page…" : "Not checked yet"));
    if (c.score != null && !c.paused) st.appendChild(el("span", "cp-score", c.score + "/100"));
    body.appendChild(st);
    if (c.notice) body.appendChild(el("div", "cp-notice", c.notice));
    if (compatError) body.appendChild(el("div", "cp-error", compatError));

    // Findings (problems first, then details).
    const findings = c.findings || [];
    if (findings.length) {
      const list = el("ul", "cp-findings");
      findings.slice().sort((a, b) => sevRank(b.severity) - sevRank(a.severity)).forEach((f) => {
        const li = el("li", "sev-" + f.severity);
        li.appendChild(el("i", "cp-sev"));
        li.appendChild(el("span", "", f.text));
        list.appendChild(li);
      });
      body.appendChild(list);
    }

    // A tried patch: keep or undo.
    const fix = c.fix || { state: "idle" };
    if (c.preview) {
      const pv = el("div", "cp-card preview");
      pv.appendChild(el("div", "cp-card-title", "Patch applied to this page only"));
      const after = c.after;
      pv.appendChild(el("div", "cp-soft", after
        ? "After the patch: " + COMPAT_STATUS[after.level].toLowerCase() + " (" + after.score + "/100, was " + c.score + ")."
        : "Re-checking…"));
      const row = el("div", "cp-row");
      cpButton(row, "Keep for this site", "primary", () => compatAct("compat_keep"), "Save as a rule for " + c.host + " only");
      const u = cpButton(row, "Undo", "", () => compatAct("compat_undo"), "Reload the page without the patch");
      u.insertBefore(I.icon("undo", { size: 14, role: "soft" }), u.firstChild);
      pv.appendChild(row);
      body.appendChild(pv);
    } else if (fix.state === "ready") {
      const cand = fix.candidate || {};
      const rv = fix.review || { blocked: [], warnings: [] };
      const card = el("div", "cp-card");
      card.appendChild(el("div", "cp-card-title", "Myth suggests"));
      card.appendChild(el("div", "cp-text", cand.explanation || "A patch for this page."));
      (rv.blocked || []).forEach((b) => card.appendChild(el("div", "cp-blocked", b)));
      (rv.warnings || []).forEach((w) => card.appendChild(el("div", "cp-warn", w)));
      const extras = [];
      if (cand.ua) extras.push("browser name: " + cand.ua);
      if (cand.mode) extras.push(cand.mode + " view");
      if (extras.length) card.appendChild(el("div", "cp-soft", "Also: " + extras.join(", ")));
      if (cand.css) { card.appendChild(el("div", "cp-code-label", "CSS")); card.appendChild(el("pre", "cp-code", cand.css)); }
      if (cand.js) { card.appendChild(el("div", "cp-code-label", "Script")); card.appendChild(el("pre", "cp-code", cand.js)); }
      const row = el("div", "cp-row");
      const blocked = (rv.blocked || []).length > 0;
      cpButton(row, "Try on this page", "primary", () => compatAct("compat_try"),
        blocked ? "Blocked by the safety check" : "Apply to this page only; Undo reloads without it", blocked);
      cpButton(row, "Discard", "", () => compatAct("compat_discard"));
      card.appendChild(row);
      body.appendChild(card);
    } else if (fix.state === "running") {
      const card = el("div", "cp-card");
      card.appendChild(el("div", "cp-text", "Myth is looking at the page…"));
      const row = el("div", "cp-row");
      cpButton(row, "Cancel", "", () => compatAct("compat_fix_cancel"));
      card.appendChild(row);
      body.appendChild(card);
    } else if (c.checked && !c.paused) {
      const card = el("div", "cp-card plain");
      if (fix.state === "failed") card.appendChild(el("div", "cp-error", fix.error));
      const ai = c.ai || {};
      const row = el("div", "cp-row");
      const wants = c.level && c.level !== "ok";
      cpButton(row, fix.state === "failed" ? "Ask Myth again" : "Ask Myth to fix", wants ? "primary" : "",
        () => compatAct("compat_fix"), ai.ready ? "" : "Set up a model in Myth’s settings first", !ai.ready);
      card.appendChild(row);
      card.appendChild(el("div", "cp-fine", ai.ready
        ? "Sends the findings and the page’s structure (no page text or form values) to " + ai.model +
          (ai.local ? " on this PC." : " at your model provider.") + " Nothing changes until you try it."
        : "No model is set up yet. Add one in Myth’s settings (Ollama or your own API key)."));
      body.appendChild(card);
    }

    // Rules applied here.
    const rules = c.rules || [];
    const polyfills = c.polyfills || [];
    if (rules.length || polyfills.length) {
      body.appendChild(el("div", "cp-section", "Applied here"));
      rules.forEach((r) => {
        const row = el("div", "cp-rule" + (r.off ? " off" : ""));
        const txt = el("div", "cp-rule-text");
        txt.appendChild(el("div", "cp-rule-title", r.title));
        const meta = (SOURCE_LABEL[r.source] || r.source) + " · " + (r.does || []).map((d) => DOES_LABEL[d] || d).join(", ") + (r.off ? " · off" : "");
        txt.appendChild(el("div", "cp-rule-meta", meta));
        row.appendChild(txt);
        cpButton(row, r.off ? "Turn on" : "Turn off", "small", () => compatAct("compat_rule", { id: r.id, on: r.off ? "1" : "0" }));
        if (r.deletable) {
          const d = cpButton(row, "", "cp-icon small", () => compatAct("compat_rule_delete", { id: r.id }), "Delete this rule");
          d.setAttribute("aria-label", "Delete rule");
          d.appendChild(I.icon("trash", { size: 15, role: "soft" }));
        }
        body.appendChild(row);
      });
      if (polyfills.length) {
        body.appendChild(el("div", "cp-fine", polyfills.length + " built-in shim" + (polyfills.length === 1 ? "" : "s") +
          " filled in missing web features: " + polyfills.map((p) => p.replace(/^polyfill\./, "")).join(", ") + "."));
      }
    }

    // Tools.
    const tools = el("div", "cp-tools");
    const rd = cpButton(tools, c.mode === "reader" ? "Close reader" : "Reader view", "small", () => compatAct("compat_mode", { mode: c.mode === "reader" ? "off" : "reader" }), "Show the article without the page around it");
    rd.insertBefore(I.icon("reader", { size: 14, role: "soft" }), rd.firstChild);
    cpButton(tools, c.mode === "simplified" ? "Restore page" : "Simplify", "small", () => compatAct("compat_mode", { mode: c.mode === "simplified" ? "off" : "simplified" }), "Remove overlays and scroll locks");
    cpButton(tools, "Re-check", "small", () => compatAct("compat_recheck"));
    cpButton(tools, c.paused ? "Resume fixes" : "Pause on this site", "small", () => compatAct("compat_pause", { on: c.paused ? "0" : "1" }),
      c.paused ? "Apply compat rules on " + c.host + " again" : "Load " + c.host + " without any compat rules");
    body.appendChild(tools);

    I.paint(panel);
    body.scrollTop = keepScroll;
  }
  function sevRank(s) { return s === "bad" ? 2 : s === "warn" ? 1 : 0; }

  let lastTone = null;
  function reportTone() {
    const dark = document.documentElement.getAttribute("data-tone") === "dark";
    if (dark === lastTone) return;
    lastTone = dark;
    api("chrome_tone", { dark: dark ? "1" : "0" });
  }

  function renderBookmark(on) {
    const box = $("bookmark-icon");
    box.textContent = "";
    box.appendChild(on
      ? I.icon("star", { size: 18, role: "star", fill: true })
      : I.icon("star", { size: 18, role: "soft" }));
    $("bookmark").title = on ? "Remove bookmark (Ctrl+D)" : "Bookmark this page (Ctrl+D)";
  }

  function renderMaximized(max) {
    const box = $("win-max-icon");
    box.textContent = "";
    box.appendChild(I.icon(max ? "restore" : "max", { size: 14, role: "soft" }));
    $("win-max").title = max ? "Restore" : "Maximize";
  }

  // ---- page tint ------------------------------------------------------------------------
  // The bars (and the sky ring around the page card) take on a light wash of the page's own
  // colour: its theme-color, else its background (read by the shell). Low alpha, less for
  // greys, and lowered further until the bar text keeps its contrast. Internal pages: none.
  const lum = (c) => {
    const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
    return 0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2]);
  };
  const contrast = (a, b) => { const x = lum(a), y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05); };
  const blend = (base, top, a) => base.map((v, i) => v + (top[i] - v) * a);
  const hexRgb = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const css = (c, a) => "rgba(" + Math.round(c[0]) + ", " + Math.round(c[1]) + ", " + Math.round(c[2]) + ", " + a + ")";

  // Alpha for washing `tint` over `sky` so `text` keeps >= 4.5:1, and secondary (`soft`)
  // text stays at >= 3:1 or, where the plain sky is already below that, loses at most 10 %.
  function tintAlpha(sky, tint, text, soft, dark) {
    const chroma = (Math.max.apply(null, tint) - Math.min.apply(null, tint)) / 255;
    const softMin = Math.min(3, contrast(soft, sky) * 0.9);
    let a = (dark ? 0.22 : 0.2) * (chroma < 0.1 ? 0.5 : 1);
    for (; a > 0.005; a -= 0.02) {
      const bg = blend(sky, tint, a);
      if (contrast(text, bg) >= 4.5 && contrast(soft, bg) >= softMin) return Math.round(a * 1000) / 1000;
    }
    return 0;
  }

  let lastTint = null;
  function renderTint(s) {
    const el = $("tint");
    const root = document.documentElement.style;
    if (!s.card || !s.tint) {
      // Fade out (keeping the hue, so it does not pass through grey).
      el.style.backgroundColor = css(lastTint || [0, 0, 0], 0);
      root.removeProperty("--strip");
      root.removeProperty("--strip-0");
      return;
    }
    const p = window.JM.palette(window.JM.hour());
    const c = window.JM.colors();
    const dark = document.documentElement.getAttribute("data-tone") === "dark";
    const a = tintAlpha(p.top, s.tint, hexRgb(c.icon), hexRgb(c.soft), dark);
    lastTint = s.tint;
    el.style.backgroundColor = css(s.tint, a);
    const strip = blend(p.top, s.tint, a);
    root.setProperty("--strip", css(strip, 1));
    root.setProperty("--strip-0", css(strip, 0));
  }

  // Right-click a tab: the tab strip menu in popups.js (incl. Pin / Unpin).

  // ---- Downloads button ---------------------------------------------------------------------
  function renderDownloads(s) {
    const d = s.downloads;
    const btn = $("downloads");
    btn.hidden = !d;
    if (!d) return;
    const bar = $("dl-bar");
    bar.hidden = !d.active;
    bar.classList.toggle("indet", d.active > 0 && d.frac == null);
    bar.firstChild.style.transform = d.frac == null ? "" : "scaleX(" + Math.round(d.frac * 100) / 100 + ")";
    $("dl-dot").hidden = !d.attention;
    btn.title = d.attention ? "Downloads: a file is waiting for you (Ctrl+Shift+Y)"
      : d.active ? "Downloads: " + d.active + " in progress (Ctrl+Shift+Y)" : "Downloads (Ctrl+Shift+Y)";
  }
  $("downloads").addEventListener("click", () => api("open_downloads"));

  function render(s) {
    state = s;
    if (s.motion && window.JM.setMotion) window.JM.setMotion(s.motion);
    if (s.glass && window.JMGlass) window.JMGlass.setMode(s.glass);
    if (typeof s.transparency === "number" && window.JMGlass && JMGlass.setLook) JMGlass.setLook(s.transparency, !!s.reduce_transparency);
    renderTabs(s);
    renderMyth(s);
    renderCompat(s);
    renderPasswords(s);
    renderTasks(s);
    renderDownloads(s);
    if (window.mbPopups) window.mbPopups.render(s);
    const busy = s.myth && s.myth.running ? 1 : 0;
    $("tasks-badge").hidden = !busy;
    $("tasks-badge").textContent = busy ? String(busy) : "";
    $("back").disabled = !s.can_go_back;
    $("forward").disabled = !s.can_go_forward;
    $("bookmark").disabled = s.internal || !s.url;
    $("bookmark").hidden = !!s.internal || !s.url;
    if (window.MythAskShotVisible) window.MythAskShotVisible(!s.internal && !!s.url);
    renderBookmark(s.bookmarked);
    renderMaximized(s.maximized);
    document.body.classList.toggle("on-page", !!s.url && !s.internal);
    document.body.classList.toggle("card", !!s.card);
    document.body.classList.toggle("maximized", !!s.maximized);
    // The shell's gap (eases wider while Myth works): inline, so it beats body.maximized.
    if (s.card && s.frame) document.body.style.setProperty("--card-gap", s.frame.gap + "px");
    else document.body.style.removeProperty("--card-gap");
    renderTint(s);
    $("progress").hidden = !s.loading;
    if (!editing) {
      // The new tab is where you talk to Myth: its address bar is empty (placeholder).
      omnibox.value = isHome(s) ? "" : s.url;
      setMode(isHome(s) ? "ai" : pageMode(s));
    }
    renderUrlView(s);
    renderZoom(s);
    renderFind(s);
    renderToast(s);
    document.body.classList.toggle("fullscreen", !!s.fullscreen);
  }

  // ---- v0.4.2: zoom indicator, find bar, note ------------------------------------------
  function renderZoom(s) {
    const z = $("zoom");
    z.hidden = !s.zoom;
    if (s.zoom) {
      z.textContent = s.zoom + "%";
      z.title = "Zoom " + s.zoom + "%: click to reset (Ctrl+0)";
    }
  }
  $("zoom").addEventListener("click", () => api("zoom", { dir: "reset" }));

  const findbar = $("findbar");
  const findInput = $("find-input");
  let findOpen = false, findFocusSeen = 0, findTimer = 0;
  function findSend(dir) {
    clearTimeout(findTimer);
    findTimer = 0;
    api("find", { q: findInput.value, dir: dir });
  }
  function renderFind(s) {
    const f = s.find;
    findbar.hidden = !f;
    document.body.classList.toggle("finding", !!f);
    if (!f) { findOpen = false; return; }
    if (!findOpen) { findOpen = true; findInput.value = f.query || ""; }
    if (f.focus && f.focus !== findFocusSeen) {
      findFocusSeen = f.focus;
      findInput.focus();
      findInput.select();
      if (findInput.value && f.total == null) findSend("new");
    }
    const c = $("find-count");
    const typed = !!findInput.value;
    c.textContent = !typed ? "" : f.total == null ? "\u2026" : f.total ? f.current + " of " + f.total : "No results";
    c.classList.toggle("none", typed && f.total === 0);
    $("find-prev").disabled = !f.total;
    $("find-next").disabled = !f.total;
  }
  findInput.addEventListener("input", () => {
    clearTimeout(findTimer);
    findTimer = setTimeout(() => findSend("new"), 120);
  });
  findInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); findSend(e.shiftKey ? "prev" : "next"); }
    else if (e.key === "Escape") { e.preventDefault(); api("find_close"); }
  });
  $("find-prev").addEventListener("click", () => findSend("prev"));
  $("find-next").addEventListener("click", () => findSend("next"));
  $("find-close").addEventListener("click", () => api("find_close"));

  let toastSeen = 0, toastTimer = 0;
  function renderToast(s) {
    const t = s.toast;
    if (!t || t.id === toastSeen) return;
    toastSeen = t.id;
    const el = $("toast");
    el.textContent = t.text;
    el.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { el.hidden = true; }, 4500);
  }

  function isHome(s) {
    return !!s.internal && /^https:\/\/mythbrowse\.internal\/home\.html/.test(s.url || "");
  }

  // The address at rest: the host in bold and the rest dimmed, over the input (which keeps
  // the real text for editing; its own text is hidden meanwhile).
  function renderUrlView(s) {
    const v = $("url-view");
    const m = !editing && s && !isHome(s) && /^([a-z][a-z0-9+.-]*:\/\/)([^/?#]*)(.*)$/i.exec(s.url || "");
    wrap.classList.toggle("has-view", !!m);
    v.hidden = !m;
    if (!m) return;
    v.textContent = "";
    const host = document.createElement("b");
    host.textContent = m[2].replace(/^www\./i, "");
    v.appendChild(host);
    const rest = m[3] === "/" ? "" : m[3];
    if (rest) {
      const r = document.createElement("span");
      r.className = "url-rest";
      r.textContent = rest;
      v.appendChild(r);
    }
    // Too long: end it with an ellipsis (ellipsis() flattens to text, so rebuild host + rest).
    if (v.scrollWidth > v.clientWidth + 1 && window.JMMotion) {
      window.JMMotion.ellipsis(v, v.textContent);
      const cut = v.textContent;
      const hl = host.textContent.length;
      v.textContent = "";
      const b2 = document.createElement("b");
      b2.textContent = cut.slice(0, hl);
      v.appendChild(b2);
      if (cut.length > hl) {
        const r2 = document.createElement("span");
        r2.className = "url-rest";
        r2.textContent = cut.slice(hl);
        v.appendChild(r2);
      }
    }
  }

  function repaintAll() {
    I.paint(document);
    reportTone();
    // Theme changed: rebuild tab icons and the Myth bar in the new colours (no animation).
    tabEls.clear();
    tabsEl.querySelectorAll(".tab").forEach((t) => t.remove());
    quietTabs = true;
    mythKey = "";
    if (state) render(state);
    else setMode("ai");
  }

  function focusOmnibox() {
    omnibox.focus();
    omnibox.select();
  }

  omnibox.addEventListener("focus", () => {
    editing = true;
    wrap.classList.add("focused");
    wrap.classList.remove("has-view");
    $("url-view").hidden = true;
    setMode(guessMode(omnibox.value));
  });
  omnibox.addEventListener("blur", () => {
    editing = false;
    wrap.classList.remove("focused");
    if (state) render(state);
    else setMode("ai");
  });
  omnibox.addEventListener("input", () => { setMode(guessMode(omnibox.value)); });
  omnibox.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      omnibox.blur();
      api("focus_content");
    } else if (e.key === "Enter" && !e.isComposing && !e.defaultPrevented) {
      // The toolbar form also holds the find-in-page field (two text fields, no submit button),
      // so HTML implicit submission does not fire on Enter; submit explicitly.
      e.preventDefault();
      submitOmnibox();
    }
  });

  // "Ask about this page": the next prompt typed here goes with a screenshot of the tab.
  let askShot = false;
  const askBtn = $("ask-shot");
  function setAskShot(on) {
    askShot = !!on;
    if (!askBtn) return;
    askBtn.setAttribute("aria-pressed", askShot ? "true" : "false");
    askBtn.classList.toggle("on", askShot);
    omnibox.placeholder = askShot ? "Ask Myth about this page" : "Message Myth or type an address";
  }
  if (askBtn) askBtn.addEventListener("click", () => {
    setAskShot(!askShot);
    if (askShot) { omnibox.value = ""; omnibox.focus(); }
  });
  window.MythAskShotVisible = function (webPage) {
    if (!askBtn) return;
    askBtn.hidden = !webPage;
    if (!webPage) setAskShot(false);
  };

  $("toolbar").addEventListener("submit", (e) => {
    e.preventDefault();
    submitOmnibox();
  });
  function submitOmnibox() {
    const input = omnibox.value.trim();
    if (!input) return;
    editing = false;
    omnibox.blur();
    const params = { input: input };
    if (askShot) params.shot = "1";
    setAskShot(false);
    api("navigate", params);
  }

  $("back").addEventListener("click", () => api("back"));
  $("forward").addEventListener("click", () => api("forward"));
  $("reload").addEventListener("click", () => api("reload"));
  $("new-tab").addEventListener("click", () => api("new_tab"));
  $("bookmark").addEventListener("click", () => api("toggle_bookmark"));
  $("library").addEventListener("click", () => api("open_library"));
  $("tasks").addEventListener("click", () => api("tasks_panel", { open: "toggle" }));
  $("compat").addEventListener("click", () => {
    const open = !!(state && state.compat && state.compat.open);
    compatError = "";
    api("compat_panel", { open: open ? "0" : "1" });
  });
  $("pw").addEventListener("click", () => {
    const p = state && state.passwords;
    pwError = "";
    if (p && p.prompt) api("pw_prompt", { choice: "not_now" });
    else api("pw_panel", { open: p && p.open ? "0" : "1" });
  });
  document.addEventListener("keydown", (e) => {
    const p = state && state.passwords;
    if (e.key === "Escape" && p && (p.open || p.prompt)) {
      e.preventDefault();
      if (p.prompt) api("pw_prompt", { choice: "not_now" });
      else api("pw_panel", { open: "0" });
      return;
    }
    if (e.key === "Escape" && state && state.compat && state.compat.open) {
      e.preventDefault();
      api("compat_panel", { open: "0" });
    }
  });

  // ---- window controls & title-bar dragging ---------------------------------------------
  $("win-min").addEventListener("click", () => api("window_minimize"));
  $("win-max").addEventListener("click", () => api("window_maximize"));
  $("win-close").addEventListener("click", () => api("window_close"));

  // Empty title-bar space moves the window; a second press within the double-click time
  // maximizes/restores it instead. (Double-click is detected here on mousedown: once the OS
  // move loop owns the pointer, a real dblclick event may never reach the page.)
  let lastDown = 0, lastX = 0, lastY = 0;
  $("titlebar").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.target.closest("button, .tab, input")) return;
    const now = Date.now();
    if (now - lastDown < 450 && Math.abs(e.clientX - lastX) < 6 && Math.abs(e.clientY - lastY) < 6) {
      lastDown = 0;
      api("window_maximize");
      return;
    }
    lastDown = now;
    lastX = e.clientX;
    lastY = e.clientY;
    e.preventDefault();
    api("window_drag");
  });

  window.JM.onChange(repaintAll);
  window.mb = {
    render: render, focusOmnibox: focusOmnibox, tintAlpha: tintAlpha,
    // Pushed by the shell: web suggestions arrived; Snap Layouts hover on the max button.
    suggest: (r) => window.mbPopups && window.mbPopups.suggest(r),
    snapHover: (on) => window.mbPopups && window.mbPopups.snapHover(on),
  };
  // Perf HUD (MB_PERF=1 / Ctrl+Shift+F12): one summary line a second from the shell.
  window.mbPerfHud = function (line) {
    let el = document.getElementById("perf-hud");
    if (line == null) { if (el) el.remove(); return; }
    if (!el) {
      el = document.createElement("div");
      el.id = "perf-hud";
      el.className = "perf-hud";
      el.setAttribute("aria-hidden", "true");
      document.body.appendChild(el);
    }
    el.textContent = String(line).split(" | ").join("\n");
  };
  // Middle-click autoscroll anchor at (x, y) window CSS px; null hides it.
  window.mbAutoscroll = function (x, y) {
    let el = document.getElementById("autoscroll-anchor");
    if (x == null) { if (el) el.remove(); return; }
    if (!el) {
      el = document.createElement("div");
      el.id = "autoscroll-anchor";
      el.className = "autoscroll-anchor";
      el.setAttribute("aria-hidden", "true");
      el.innerHTML = '<svg width="30" height="30" viewBox="0 0 30 30"><circle cx="15" cy="15" r="14" class="as-ring"/>' +
        '<path d="M15 5 l4 5 h-8z M15 25 l4 -5 h-8z M5 15 l5 -4 v8z M25 15 l-5 -4 v8z" class="as-arrows"/>' +
        '<circle cx="15" cy="15" r="2" class="as-arrows"/></svg>';
      document.body.appendChild(el);
    }
    el.style.left = (x - 15) + "px";
    el.style.top = (y - 15) + "px";
  };
  repaintAll();
  api("state").then((r) => { if (r.ok && r.state) render(r.state); });
})();
