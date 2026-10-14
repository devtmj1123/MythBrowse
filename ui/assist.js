// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Myth panel over web pages + selection pill (docs/AI-OVERLAY.md). Rendered from
// state.assist / state.frame.assist / state.frame.selpill (the shell lays both out and
// composes them over the page); the conversation is the AI home's renderer (ui/chat.js).
//
// Shell API (chrome only):
//   assist?open=1|0|toggle&mode=docked|floating&x=&y=&w=&h=&dock_w=
//   assist_send?action=ask|summarize|key_points|compare_tabs|find|explain|translate|
//               summarize_selection|rewrite&text=&page=0|1&selection=0|1&shot=0|1&lang=
//   assist_new  assist_stop  assist_share?share=0|1  assist_reveal?block=N
//   assist_sel?action=&lang=  assist_sel_dismiss
//   chat_poll / chat_approve (as on the AI home), myth_open?conv= (open as a full chat)
"use strict";

(function () {
  const I = window.JMIcons;
  const MC = window.MythChat;
  if (!MC) return;
  const api = MC.api;
  const $ = (id) => document.getElementById(id);
  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = text;
    return e;
  };
  const icon = (n, s, r) => I.icon(n, { size: s || 16, role: r || "soft" });
  const mark = () => { const m = el("span", "mark"); for (let k = 0; k < 3; k++) m.appendChild(el("i")); return m; };
  const motionOff = () => (document.documentElement.getAttribute("data-motion") || "full") !== "full";
  // Servo 0.1.4 has no text-overflow: cut long labels here.
  const clip = (s, n) => (s && s.length > n ? s.slice(0, n - 1) + "\u2026" : s || "");

  const root = $("assist");
  const pill = $("selpill");
  const btn = $("assist-btn");
  if (!root || !pill) return;

  let A = null;          // state.assist
  let frame = null;      // state.frame.assist
  let shown = false;
  let tab = null;
  let conv = null;
  let runs = -1;
  let includePage = true;
  let includeShot = false;
  let findMode = false;
  let lang = (function () { try { return localStorage.getItem("mb.assist.lang") || navigator.language || "en"; } catch (e) { return navigator.language || "en"; } })();

  // ---- DOM ----------------------------------------------------------------------------------
  const head = el("div", "as-head");
  head.appendChild(mark());
  head.appendChild(el("span", "as-name", "Myth"));
  const model = el("span", "as-model");
  head.appendChild(model);
  function ibtn(name, title, fn) {
    const b = el("button", "as-ibtn");
    b.type = "button";
    b.title = title;
    b.setAttribute("aria-label", title);
    b.appendChild(icon(name, 17));
    b.addEventListener("click", (e) => { e.stopPropagation(); fn(); });
    head.appendChild(b);
    return b;
  }
  const modeBtn = ibtn("sidebar", "Float the panel", () => {
    api("assist", { mode: A && A.mode === "docked" ? "floating" : "docked" });
  });
  ibtn("chat", "Open as a full chat", () => { if (conv !== null) api("myth_open", { conv: conv }); });
  ibtn("plus", "New conversation", () => { api("assist_new").then(() => resetChat()); });
  ibtn("close", "Close (Ctrl+J)", () => api("assist", { open: "0" }));

  const ctx = el("div", "as-ctx");
  const body = el("div", "as-body");
  const quick = el("div", "as-quick");
  const form = el("form", "as-composer");
  form.setAttribute("autocomplete", "off");
  const input = el("textarea");
  input.rows = 1;
  input.placeholder = "Ask about this page\u2026";
  input.setAttribute("aria-label", "Message Myth");
  const send = el("button", "as-send");
  send.type = "submit";
  send.title = "Send (Enter)";
  form.appendChild(input);
  form.appendChild(send);
  const grip = el("div", "as-grip");
  root.appendChild(grip);
  root.appendChild(head);
  root.appendChild(ctx);
  root.appendChild(body);
  root.appendChild(quick);
  root.appendChild(form);

  const CMDS = [
    ["summarize", "Summarize page", "reader"],
    ["key_points", "Key points", "tasks"],
    ["compare_tabs", "Compare open tabs", "tabs"],
    ["find", "Find on page\u2026", "search"],
  ];

  // ---- conversation -------------------------------------------------------------------------
  const chat = new MC.Chat({
    root: body,
    onRunning: (on) => setRunning(on),
  });
  // Find results: the [n] block numbers become buttons that scroll the page there.
  const baseHandle = chat.handle.bind(chat);
  chat.handle = function (e) {
    const reply = this.reply;
    const prompt = this.lastPrompt || "";
    baseHandle(e);
    if (e.k === "end" && reply && /^Find on this page/.test(prompt)) linkRefs(reply.body);
  };
  function linkRefs(node) {
    const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
    const hits = [];
    for (let n = walker.nextNode(); n; n = walker.nextNode()) if (/\[\d{1,5}\]/.test(n.data)) hits.push(n);
    hits.forEach((n) => {
      const frag = document.createDocumentFragment();
      let last = 0;
      n.data.replace(/\[(\d{1,5})\]/g, (m, num, at) => {
        if (at > last) frag.appendChild(document.createTextNode(n.data.slice(last, at)));
        const b = el("button", "as-ref", num);
        b.type = "button";
        b.title = "Show on the page";
        b.addEventListener("click", () => api("assist_reveal", { block: num }));
        frag.appendChild(b);
        last = at + m.length;
        return m;
      });
      if (last < n.data.length) frag.appendChild(document.createTextNode(n.data.slice(last)));
      n.parentNode.replaceChild(frag, n);
    });
  }

  function resetChat() {
    chat.reset();
    conv = null;
    renderEmpty();
  }

  function renderEmpty() {
    if (conv !== null) return;
    body.textContent = "";
    const box = el("div", "as-empty");
    box.appendChild(el("h4", null, A && A.shared === false ? "Myth can\u2019t see this site" : "Ask about this page"));
    box.appendChild(el("p", null, A && A.shared === false
      ? "You set this site to \u201cDon\u2019t share with Myth\u201d. Myth can still answer general questions."
      : "Myth reads the page when you send a message. It can also click, type and scroll here \u2014 it asks before anything that matters."));
    const grid = el("div", "as-cmds");
    CMDS.forEach(([a, label, ic]) => {
      const b = el("button", "as-cmd");
      b.type = "button";
      b.appendChild(icon(ic, 16));
      b.appendChild(document.createTextNode(label));
      b.disabled = A && A.shared === false && a !== "compare_tabs";
      b.addEventListener("click", () => command(a));
      grid.appendChild(b);
    });
    box.appendChild(grid);
    body.appendChild(box);
  }

  function renderQuick() {
    quick.textContent = "";
    quick.hidden = conv === null;
    // Short labels so all four fit one row in the narrowest panel (the empty state has room
    // for the long ones).
    const SHORT = { summarize: "Summarize", key_points: "Key points", compare_tabs: "Compare", find: "Find" };
    CMDS.forEach(([a, label]) => {
      const b = el("button", "as-q" + (a === "find" && findMode ? " on" : ""), SHORT[a] || label.replace("\u2026", ""));
      b.title = label.replace("\u2026", "");
      b.disabled = A && A.shared === false && a !== "compare_tabs";
      b.type = "button";
      b.addEventListener("click", () => command(a));
      quick.appendChild(b);
    });
  }

  function command(a) {
    if (a === "find") {
      findMode = !findMode;
      input.placeholder = findMode ? "Find on this page by meaning\u2026" : "Ask about this page\u2026";
      renderQuick();
      input.focus();
      return;
    }
    submit(a, "");
  }

  let running = false;
  function setRunning(on) {
    running = on;
    send.textContent = "";
    send.appendChild(I.icon(on ? "stop" : "send", { size: 16, color: "#ffffff" }));
    send.title = on ? "Stop (Esc)" : "Send (Enter)";
  }
  setRunning(false);

  function showError(text) {
    const old = body.querySelector(".as-err");
    if (old) old.remove();
    if (!text) return;
    body.appendChild(el("p", "as-err", text));
    body.scrollTop = body.scrollHeight;
  }

  function submit(action, text) {
    if (running) { api("assist_stop"); return; }
    const params = {
      action: action,
      text: text,
      page: includePage ? "1" : "0",
      selection: A && A.selection ? "1" : "0",
      shot: includeShot ? "1" : "0",
      lang: lang,
      // The user's local date (the shell only knows UTC).
      today: new Date().toDateString(),
    };
    if (conv === null) body.textContent = "";
    showReading(action);
    api("assist_send", params).then((r) => {
      if (!r.ok) { hideReading(); showError(r.error); }
    });
  }

  let readingEl = null;
  function showReading(action) {
    hideReading();
    readingEl = el("div", "as-reading");
    const t = el("span", "typing");
    for (let k = 0; k < 3; k++) t.appendChild(el("i"));
    readingEl.appendChild(t);
    readingEl.appendChild(document.createTextNode(action === "compare_tabs" ? "Reading your tabs\u2026" : includePage || action !== "ask" ? "Reading the page\u2026" : "Sending\u2026"));
    body.appendChild(readingEl);
    body.scrollTop = body.scrollHeight;
  }
  function hideReading() { if (readingEl) { readingEl.remove(); readingEl = null; } }

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (running) { api("assist_stop"); return; }
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    autosize();
    if (findMode) { findMode = false; input.placeholder = "Ask about this page\u2026"; renderQuick(); submit("find", text); }
    else submit("ask", text);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); form.requestSubmit ? form.requestSubmit() : form.dispatchEvent(new Event("submit", { cancelable: true })); }
    else if (e.key === "Escape") {
      e.preventDefault();
      if (running) api("assist_stop");
      else api("assist", { open: "0" }).then(() => api("focus_content"));
    }
  });
  function autosize() {
    input.style.height = "22px";
    input.style.height = Math.min(120, Math.max(22, input.scrollHeight)) + "px";
  }
  input.addEventListener("input", autosize);

  // ---- context chips --------------------------------------------------------------------------
  function chip(cls, iconName, label, onX, xTitle) {
    const c = el("span", "as-chip " + (cls || ""));
    if (iconName) c.appendChild(icon(iconName, 14));
    c.appendChild(el("span", "as-chip-t", label));
    if (onX) {
      const x = el("button");
      x.type = "button";
      x.title = xTitle || "Remove";
      x.setAttribute("aria-label", x.title);
      x.appendChild(icon("close", 12));
      x.addEventListener("click", (e) => { e.stopPropagation(); onX(); });
      c.appendChild(x);
    }
    ctx.appendChild(c);
    return c;
  }
  function renderCtx() {
    ctx.textContent = "";
    if (!A) return;
    if (A.shared === false) {
      const c = chip("lock", "lock", "Not sharing this site");
      const b = el("button", "as-share", "Share");
      b.type = "button";
      b.title = "Let Myth read this site again";
      b.addEventListener("click", () => api("assist_share", { share: "1" }));
      c.appendChild(b);
      return;
    }
    if (includePage) chip("", "globe", clip(A.title || A.site || "This page", 34), () => { includePage = false; renderCtx(); }, "Don\u2019t send this page");
    else {
      const c = chip("off", "plus", "Add this page");
      c.addEventListener("click", () => { includePage = true; renderCtx(); });
    }
    if (A.selection) chip("", "reader", "Selection \u00b7 " + A.selection.words + (A.selection.words === 1 ? " word" : " words"), () => api("assist_sel_dismiss"));
    if (A.vision) {
      const c = chip("toggle" + (includeShot ? " on" : ""), "eye", "Screenshot");
      c.title = "Send a screenshot of the page with the next message (passwords are blacked out)";
      c.addEventListener("click", () => { includeShot = !includeShot; renderCtx(); });
    }
    const lock = el("button", "as-ibtn");
    lock.type = "button";
    lock.title = "Don\u2019t share " + (A.site || "this site") + " with Myth";
    lock.setAttribute("aria-label", lock.title);
    lock.style.width = lock.style.height = "26px";
    lock.appendChild(icon("lock", 14));
    lock.addEventListener("click", () => api("assist_share", { share: "0" }));
    ctx.appendChild(lock);
  }

  // ---- placement, drag, resize --------------------------------------------------------------
  function place(f) {
    root.style.left = f.x + "px";
    root.style.top = f.y + "px";
    root.style.width = f.w + "px";
    root.style.height = f.h + "px";
    root.style.borderRadius = f.r + "px";
  }
  let drag = null;
  head.addEventListener("pointerdown", (e) => {
    if (!A || A.mode !== "floating" || e.button !== 0 || e.target.closest("button")) return;
    drag = { dx: e.clientX - frame.x, dy: e.clientY - frame.y, w: frame.w, h: frame.h, raf: 0, x: frame.x, y: frame.y };
    root.classList.add("dragging");
    try { head.setPointerCapture(e.pointerId); } catch (err) { /* ignore */ }
    e.preventDefault();
  });
  head.addEventListener("pointermove", (e) => {
    if (!drag) return;
    drag.x = e.clientX - drag.dx;
    drag.y = e.clientY - drag.dy;
    root.style.transform = "translate(" + (drag.x - frame.x) + "px," + (drag.y - frame.y) + "px)";
    if (!drag.raf) drag.raf = requestAnimationFrame(() => {
      if (!drag) return;
      drag.raf = 0;
      api("assist", { x: drag.x.toFixed(1), y: drag.y.toFixed(1), w: drag.w, h: drag.h });
    });
  });
  function endDrag() {
    if (!drag) return;
    api("assist", { x: drag.x.toFixed(1), y: drag.y.toFixed(1), w: drag.w, h: drag.h });
    drag = null;
    root.classList.remove("dragging");
  }
  head.addEventListener("pointerup", endDrag);
  head.addEventListener("pointercancel", endDrag);
  let resize = null;
  grip.addEventListener("pointerdown", (e) => {
    if (!A || A.mode !== "docked" || e.button !== 0) return;
    resize = { x0: e.clientX, w0: frame.w, raf: 0, w: frame.w };
    try { grip.setPointerCapture(e.pointerId); } catch (err) { /* ignore */ }
    e.preventDefault();
  });
  grip.addEventListener("pointermove", (e) => {
    if (!resize) return;
    resize.w = Math.max(320, Math.min(560, resize.w0 + (resize.x0 - e.clientX)));
    if (!resize.raf) resize.raf = requestAnimationFrame(() => {
      if (!resize) return;
      resize.raf = 0;
      api("assist", { dock_w: Math.round(resize.w) });
    });
  });
  grip.addEventListener("pointerup", () => { if (resize) api("assist", { dock_w: Math.round(resize.w) }); resize = null; });

  // ---- selection pill -----------------------------------------------------------------------
  const PILL = [
    ["ask", "Ask Myth"], ["explain", "Explain"], ["translate", "Translate"],
    ["summarize_selection", "Summarize"], ["rewrite", "Rewrite"],
  ];
  PILL.forEach(([a, label], i) => {
    if (i === 1) pill.appendChild(el("span", "sp-sep"));
    const b = el("button", a === "ask" ? "sp-ask" : "");
    b.type = "button";
    if (a === "ask") b.appendChild(mark());
    b.appendChild(document.createTextNode(label));
    if (a === "translate") b.title = "Translate into " + lang;
    b.addEventListener("click", () => {
      api("assist_sel", { action: a, lang: lang, today: new Date().toDateString() }).then(() => { if (a === "ask") setTimeout(() => input.focus(), 50); });
    });
    pill.appendChild(b);
  });
  let pillShown = false;
  function renderPill(s) {
    const p = s.frame && s.frame.selpill;
    const on = !!(p && s.assist && s.assist.pill);
    if (!on) { pill.hidden = true; pillShown = false; return; }
    pill.style.left = p.x + "px";
    pill.style.top = p.y + "px";
    pill.style.width = p.w + "px";
    pill.style.height = p.h + "px";
    if (!pillShown) {
      pill.hidden = false;
      pill.classList.remove("sp-in");
      if (!motionOff()) { void pill.offsetWidth; pill.classList.add("sp-in"); }
      pillShown = true;
    }
  }

  // ---- render from the shell's state -------------------------------------------------------
  function render(s) {
    A = s.assist || null;
    frame = s.frame && s.frame.assist;
    if (btn) {
      btn.hidden = !A;
      btn.classList.toggle("on", !!(A && A.open));
      btn.setAttribute("aria-pressed", A && A.open ? "true" : "false");
    }
    renderPill(s);
    if (!A || !A.open || !frame) {
      if (shown) { root.hidden = true; shown = false; }
      return;
    }
    root.classList.toggle("docked", A.mode === "docked");
    root.classList.toggle("floating", A.mode !== "docked");
    if (!drag) { root.style.transform = ""; place(frame); }
    modeBtn.title = A.mode === "docked" ? "Float the panel" : "Dock the panel on the right";
    modeBtn.setAttribute("aria-label", modeBtn.title);
    modeBtn.textContent = "";
    modeBtn.appendChild(icon(A.mode === "docked" ? "popup" : "sidebar", 17));
    model.textContent = clip(A.model || "", 28);
    // The conversation follows the active tab.
    if (A.tab !== tab || A.conv !== conv) {
      const sameTab = A.tab === tab;
      tab = A.tab;
      if (A.conv === null || A.conv === undefined) {
        if (!sameTab || conv !== null) { chat.reset(); conv = null; renderEmpty(); }
      } else {
        hideReading();
        chat.reset();
        conv = A.conv;
        body.textContent = "";
        chat.resume(conv);
      }
      runs = A.runs;
    } else if (A.runs !== runs) {
      runs = A.runs;
      hideReading();
      chat.poll();
    }
    if (A.reading) { if (!readingEl) showReading(A.reading); } else if (readingEl && A.runs === runs && !A.reading && conv !== null) hideReading();
    if (A.error) { hideReading(); showError(A.error); }
    renderCtx();
    renderQuick();
    if (!shown) {
      root.hidden = false;
      shown = true;
      root.classList.remove("as-in");
      if (!motionOff()) { void root.offsetWidth; root.classList.add("as-in"); }
      if (conv === null) renderEmpty();
      setTimeout(() => input.focus(), 30);
    }
  }

  if (btn) btn.addEventListener("click", () => api("assist", { open: "toggle" }));

  // Hook into the chrome's render (chrome.js owns window.mb).
  function hook() {
    if (!window.mb || window.mb.__assist) return;
    const orig = window.mb.render;
    window.mb.render = function (s) { orig(s); try { render(s); } catch (e) { console.error("assist", e); } };
    window.mb.__assist = true;
  }
  hook();
  api("state").then((r) => { if (r.ok && r.state) render(r.state); });
  window.MythAssist = { render: render, setLang: (l) => { lang = l; try { localStorage.setItem("mb.assist.lang", l); } catch (e) { /* ignore */ } } };
})();
