// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// New tab: date and time (MythDesk home), recent pages and bookmarks in glass cards, the
// model pill, and the Myth composer. The conversation itself is chat.js; a prompt typed in
// the address bar arrives as ?q=, a reloaded conversation as ?c= (this session) or ?id= (a
// saved chat, also after a restart). The glass sidebar lists saved chats (chatlist.js).
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const conv = $("conversation");
  const form = $("composer");
  const input = $("prompt");
  const I = window.JMIcons;
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;

  function api(op, params) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    return fetch("/api/" + op + "?" + q.toString())
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }

  // ---- date and time (MythDesk: "WEDNESDAY · 7 OCTOBER", 15:58 with a dimmed colon) ------
  const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
  const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August",
    "September", "October", "November", "December"];
  const pad = (n) => (n < 10 ? "0" : "") + n;

  function tick() {
    const now = new Date();
    const h = window.JM.hour();
    // A pinned theme hour (screenshots) also pins the clock, so sky and time agree.
    const hh = window.JM.pinned ? Math.floor(h) : now.getHours();
    const mm = window.JM.pinned ? Math.round((h % 1) * 60) : now.getMinutes();
    $("home-date").textContent = DAYS[now.getDay()] + " \u00b7 " + now.getDate() + " " + MONTHS[now.getMonth()];
    const t = $("home-time");
    t.textContent = "";
    t.appendChild(document.createTextNode(pad(hh)));
    const colon = document.createElement("span");
    colon.className = "colon";
    colon.textContent = ":";
    t.appendChild(colon);
    t.appendChild(document.createTextNode(pad(mm)));
  }

  // ---- cards ------------------------------------------------------------------------------
  function hostOf(url) {
    const m = /^[a-z][a-z0-9+.-]*:\/\/([^/?#:]+)/i.exec(url || "");
    return m ? m[1].replace(/^www\./i, "") : "";
  }
  // Servo has no text-overflow, so long titles are shortened here.
  const clip = (s, n) => (s.length > n ? s.slice(0, n - 1).trimEnd() + "\u2026" : s);

  function linkItem(url, title) {
    const li = document.createElement("li");
    const a = document.createElement("a");
    a.href = url;
    a.title = url;
    const host = hostOf(url);
    const site = document.createElement("span");
    site.className = "site";
    site.textContent = (host[0] || "?").toUpperCase();
    a.appendChild(site);
    const t = document.createElement("span");
    t.className = "t";
    t.textContent = clip(title || host || url, 34);
    a.appendChild(t);
    if (title && host) {
      const h = document.createElement("span");
      h.className = "h";
      h.textContent = clip(host, 22);
      a.appendChild(h);
    }
    li.appendChild(a);
    return li;
  }

  function loadCards() {
    api("history", { limit: 6 }).then((r) => {
      const list = $("recent");
      list.textContent = "";
      const items = r.ok ? r.history.slice(0, 5) : [];
      if (!items.length) {
        const li = document.createElement("li");
        li.className = "empty";
        li.textContent = r.ok ? "Nothing yet. Pages you open will show up here." : "History is unavailable.";
        list.appendChild(li);
      }
      items.forEach((it) => list.appendChild(linkItem(it.url, it.title)));
    });
    api("bookmarks").then((r) => {
      const items = r.ok ? r.bookmarks.slice(0, 5) : [];
      const list = $("bookmarks");
      list.textContent = "";
      items.forEach((it) => list.appendChild(linkItem(it.url, it.title)));
      $("bookmarks-card").hidden = !items.length;
    });
  }

  // ---- conversation (chat.js) ---------------------------------------------------------------
  const send = $("send");
  const stateText = $("state-text");
  const pill = $("state-pill");
  function setSend(running) {
    send.textContent = "";
    send.appendChild(I.icon(running ? "stop" : "send", { size: 18, role: "white" }));
    send.title = running ? "Stop" : "Send (Enter)";
    send.setAttribute("aria-label", running ? "Stop" : "Send");
    form.classList.toggle("running", running);
  }
  function showModel(model, local) {
    // Status pill as in the approved design: "Ollama · qwen3:8b" / "API · gpt-4.1-mini".
    stateText.textContent = model ? (local ? "Ollama \u00b7 " : "API \u00b7 ") + model : "Model";
    $("model-label").textContent = model || "Model";
    pill.classList.toggle("ready", !!model);
    pill.title = (local ? "Runs on this computer" : "Remote API") + " \u00b7 Model settings";
  }
  const settings = new window.MythChat.Settings({
    sheet: $("settings"),
    onSaved: (s) => { showModel(s.model, s.local); input.focus(); },
  });
  const chat = new window.MythChat.Chat({
    root: conv,
    onActivity: () => document.body.classList.add("chatting"),
    onRunning: (on) => { setSend(on); if (!on) refreshSide(); },
    onModel: showModel,
    openSettings: () => settings.open(),
    onConv: () => refreshSide(),
  });
  chat.onBackground = (tabs, step) => {
    const bar = $("background");
    bar.hidden = !tabs.length;
    if (tabs.length) {
      $("bg-text").textContent = (step ? step + " \u00b7 " : "") +
        (tabs.length === 1 ? "1 background tab" : tabs.length + " background tabs");
    }
  };
  // For the first-run welcome (onboarding.js).
  window.MythHome = { settings: settings, showModel: showModel, focusPrompt: () => input.focus() };
  pill.addEventListener("click", () => settings.open(pill));
  $("model-pill").addEventListener("click", () => settings.open($("model-pill")));
  // home.html#settings opens the sheet directly (screenshots, links from other pages).
  if (location.hash === "#settings") settings.open();
  window.MythChat.api("ai_settings").then((s) => {
    if (!s.ok) return;
    if (s.configured) showModel(s.model, s.local);
    else stateText.textContent = $("model-label").textContent = "Choose a model";
  });

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (chat.running) { chat.stop(); return; }
    const text = input.value;
    if (!text.trim()) return;
    input.value = "";
    chat.send(text);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && chat.running) chat.stop();
  });
  input.addEventListener("focus", () => form.classList.add("focused"));
  input.addEventListener("blur", () => form.classList.remove("focused"));
  setSend(false);

  I.paint(document);
  tick();
  setInterval(tick, 15 * 1000);
  window.JM.onChange(tick);
  loadCards();

  // ---- sidebar: saved chats (glass panel, springs per motion.js) ----------------------------
  const side = $("sidebar");
  const sideBtn = $("side-btn");
  const sideList = new window.MythChatList.List({
    root: $("side-list"),
    compact: true,
    active: () => chat.id || (chat.conv !== null ? "s" + chat.conv : null),
    onOpen: (it) => openChat(it.id),
    onLoaded: (r) => { $("side-private").hidden = !(r.ok && r.private); },
    empty: "No chats yet. Ask Myth something and it shows up here.",
  });
  sideList.onDeleted = (it) => {
    // The open chat was deleted: start a new one.
    if (it.id === chat.id || it.id === "s" + chat.conv) newChat();
  };
  let sideOpen = false;
  function refreshSide() { if (sideOpen) sideList.load(); }
  function setSide(open, quiet) {
    sideOpen = open;
    sideBtn.setAttribute("aria-expanded", String(open));
    sideBtn.classList.toggle("on", open);
    document.body.classList.toggle("side-open", open);
    try { localStorage.setItem("mb-side", open ? "1" : "0"); } catch (e) { /* ignore */ }
    if (open) {
      sideList.load();
      if (quiet) { side.hidden = false; window.JMGlass && JMGlass.kick(); }
      else window.JMMotion.pop(side, sideBtn);
    } else if (quiet) {
      side.hidden = true;
    } else {
      window.JMMotion.unpop(side);
    }
  }
  function openChat(id) {
    if (id === chat.id || id === "s" + chat.conv) return;
    chat.open(id).then((r) => {
      if (r && !r.ok) { refreshSide(); return; }
      document.body.classList.add("chatting");
      refreshSide();
      input.focus();
    });
  }
  function newChat() {
    if (chat.running) chat.stop();
    chat.reset();
    document.body.classList.remove("chatting");
    try { history.replaceState(null, "", "home.html"); } catch (e) { /* ignore */ }
    loadCards();
    refreshSide();
    input.focus();
  }
  sideBtn.addEventListener("click", () => setSide(!sideOpen));
  $("side-close").addEventListener("click", () => setSide(false));
  $("side-new").addEventListener("click", newChat);
  $("new-btn").addEventListener("click", newChat);
  const sideFilter = $("side-filter");
  let sideTimer = 0;
  sideFilter.addEventListener("input", () => {
    clearTimeout(sideTimer);
    sideTimer = setTimeout(() => sideList.load(sideFilter.value), 150);
  });
  sideFilter.addEventListener("focus", () => $("side-search").classList.add("focused"));
  sideFilter.addEventListener("blur", () => $("side-search").classList.remove("focused"));
  sideFilter.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { sideFilter.value = ""; sideList.load(""); input.focus(); }
  });
  let stored = null;
  try { stored = localStorage.getItem("mb-side"); } catch (e) { /* ignore */ }
  // Open by default on a wide window; remembered after the first toggle.
  setSide(stored === null ? window.innerWidth >= 1200 : stored === "1", true);

  const params = new URLSearchParams(location.search);
  const q = params.get("q");
  const c = params.get("c");
  const id = params.get("id");
  if (id && /^[0-9a-f]{8,64}$/i.test(id)) {
    chat.open(id).then((r) => {
      if (r && r.ok) document.body.classList.add("chatting");
      else try { history.replaceState(null, "", "home.html"); } catch (e) { /* ignore */ }
    });
  } else if (c && /^\d+$/.test(c)) chat.resume(Number(c));
  else if (q) chat.send(q);
  input.focus();

  // ---- update badge (v0.4): the daily check (shell settings_host.rs) found a newer release ----
  // A small pill at the bottom right that opens Settings > About; dismissed per version.
  function updBadge(r) {
    const u = r && r.ok !== false && r.update;
    if (!u || u.phase !== "available" || !u.latest || document.getElementById("upd-toast")) return !!u && u.phase === "checking";
    try { if (localStorage.getItem("mb-upd-dismissed") === u.latest) return false; } catch (e) { /* no storage */ }
    const t = document.createElement("div");
    t.id = "upd-toast";
    t.className = "upd-toast glass";
    t.setAttribute("role", "status");
    const a = document.createElement("a");
    a.href = "library.html#about-panel";
    a.textContent = "MythBrowse " + u.latest + " is available";
    const x = document.createElement("button");
    x.type = "button";
    x.className = "upd-x";
    x.setAttribute("aria-label", "Dismiss");
    x.textContent = "\u00d7";
    x.addEventListener("click", () => {
      try { localStorage.setItem("mb-upd-dismissed", u.latest); } catch (e) { /* no storage */ }
      t.remove();
    });
    t.append(a, x);
    document.body.appendChild(t);
    return false;
  }
  let updTries = 0;
  (function updPoll() {
    api("update_status").then((r) => { if (updBadge(r) && ++updTries < 10) setTimeout(updPoll, 3000); });
  })();
})();
