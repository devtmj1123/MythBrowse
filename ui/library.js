// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// History & bookmarks page: bookmarks, then history grouped by day, with search and removal.
// The history panel has two tabs sharing one search box: Pages (visited pages; a page Myth
// opened links back to its chat) and Chats (saved Myth conversations, chatlist.js), with the
// chat retention setting and clear options.
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const I = window.JMIcons;

  const TOKEN = document.querySelector('meta[name="mb-token"]').content;

  function api(op, params) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    return fetch("/api/" + op + "?" + q.toString())
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }

  const pad = (n) => (n < 10 ? "0" : "") + n;
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

  function timeOf(ms) {
    const d = new Date(ms);
    return pad(d.getHours()) + ":" + pad(d.getMinutes());
  }

  function dayLabel(ms) {
    const d = new Date(ms);
    const start = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const diff = Math.round((start(new Date()) - start(d)) / 86400000);
    if (diff === 0) return "Today";
    if (diff === 1) return "Yesterday";
    return d.getDate() + " " + MONTHS[d.getMonth()] + " " + d.getFullYear();
  }

  function hostOf(url) {
    const m = /^[a-z][a-z0-9+.-]*:\/\/([^/?#:]+)/i.exec(url || "");
    return m ? m[1].replace(/^www\./i, "") : "";
  }

  function entry(url, title, meta, onRemove, conv) {
    const li = document.createElement("li");
    li.className = "entry";
    const host = hostOf(url);
    const site = document.createElement("span");
    site.className = "site";
    site.textContent = (host[0] || "?").toUpperCase();
    li.appendChild(site);
    const a = document.createElement("a");
    a.href = url;
    a.title = url;
    const t = document.createElement("span");
    t.className = "t";
    t.textContent = title || url;
    a.appendChild(t);
    if (title && host) {
      const h = document.createElement("span");
      h.className = "h";
      h.textContent = host;
      a.appendChild(h);
    }
    li.appendChild(a);
    if (conv) {
      // Opened by Myth: back to that chat.
      const c = document.createElement("a");
      c.className = "myth-link";
      c.href = "home.html?id=" + encodeURIComponent(conv);
      c.title = "Open the chat that opened this page";
      const mk = document.createElement("span");
      mk.className = "mark";
      for (let k = 0; k < 3; k++) mk.appendChild(document.createElement("i"));
      c.appendChild(mk);
      c.appendChild(document.createTextNode("Myth"));
      li.appendChild(c);
    }
    const m = document.createElement("span");
    m.className = "meta";
    m.textContent = meta;
    li.appendChild(m);
    const x = document.createElement("button");
    x.className = "remove";
    x.title = "Remove";
    x.setAttribute("aria-label", "Remove");
    x.appendChild(I.icon("close", { size: 14, role: "soft" }));
    x.addEventListener("click", onRemove);
    li.appendChild(x);
    return li;
  }

  function empty(list, text) {
    const li = document.createElement("li");
    li.className = "list-empty";
    li.textContent = text;
    list.appendChild(li);
  }

  function loadBookmarks() {
    api("bookmarks").then((r) => {
      const list = $("bookmarks");
      list.textContent = "";
      $("bookmark-count").textContent = r.ok && r.bookmarks.length ? String(r.bookmarks.length) : "";
      if (!r.ok) return empty(list, "Could not load bookmarks: " + r.error);
      if (!r.bookmarks.length) return empty(list, "No bookmarks yet. Use the star in the toolbar or Ctrl+D on a page.");
      for (const b of r.bookmarks) {
        list.appendChild(entry(b.url, b.title, dayLabel(b.created_ms), () =>
          api("bookmark_remove", { url: b.url }).then(loadBookmarks)));
      }
    });
  }

  function loadHistory() {
    const q = $("filter").value;
    api("history", q ? { q: q, limit: 300 } : { limit: 300 }).then((r) => {
      const list = $("history");
      list.textContent = "";
      if (!r.ok) return empty(list, "Could not load history: " + r.error);
      if (!r.history.length) return empty(list, q ? "No matches." : "No history yet. Pages you visit appear here.");
      let day = "";
      for (const h of r.history) {
        const label = dayLabel(h.last_visit_ms);
        if (label !== day) {
          day = label;
          const d = document.createElement("li");
          d.className = "day";
          d.textContent = label;
          list.appendChild(d);
        }
        const visits = h.visit_count > 1 ? h.visit_count + " visits · " : "";
        list.appendChild(entry(h.url, h.title, visits + timeOf(h.last_visit_ms), () =>
          api("history_delete", { url: h.url }).then(loadHistory), h.conv));
      }
    });
  }

  // ---- tabs: Pages / Chats, one search box --------------------------------------------------
  let tab = location.hash === "#chats" ? "chats" : "pages";
  const chats = new window.MythChatList.List({
    root: $("chats"),
    onOpen: (it) => { location.href = "home.html?id=" + encodeURIComponent(it.id); },
    onLoaded: (r) => { if (r.ok && r.private) $("keep-note").textContent = "This is a private window: chats are not saved."; },
  });
  chats.onDeleted = () => loadHistory();
  function loadTab() {
    if (tab === "chats") chats.load($("filter").value);
    else loadHistory();
  }
  // v0.4: switching tabs slides the segmented control's pill (motion.js, transform only) and
  // crossfades the lists: the old one fades out (90 ms), the new one fades + rises in. Only
  // opacity / transform animate. Reduced: fades only; off: instant.
  const motionMode = () => document.documentElement.getAttribute("data-motion") || "full";
  let swapT = 0;
  function swapLists(t, first) {
    const show = t === "pages" ? [$("history")] : [$("chat-settings"), $("chats")];
    const hide = t === "pages" ? [$("chat-settings"), $("chats")] : [$("history")];
    clearTimeout(swapT);
    const all = [$("history"), $("chats"), $("chat-settings")];
    all.forEach((el) => el.classList.remove("lib-out", "lib-in"));
    const done = () => {
      hide.forEach((el) => { el.hidden = true; el.classList.remove("lib-out"); });
      show.forEach((el) => {
        el.hidden = false;
        if (!first && motionMode() !== "off") { void el.offsetWidth; el.classList.add("lib-in"); }
      });
      window.JMGlass && JMGlass.kick();
    };
    const visible = hide.filter((el) => !el.hidden);
    if (first || motionMode() === "off" || !visible.length) { done(); return; }
    visible.forEach((el) => el.classList.add("lib-out"));
    swapT = setTimeout(done, 100);
  }
  function setTab(t, first) {
    tab = t;
    document.querySelectorAll("#lib-tabs .seg-btn").forEach((b) => {
      const on = b.getAttribute("data-tab") === t;
      b.classList.toggle("on", on);
      b.setAttribute("aria-selected", String(on));
    });
    swapLists(t, first);
    $("filter").placeholder = t === "chats" ? "Search chats" : "Search pages";
    setClear(false);
    // On the first call keep any other deep link (#cookies=<site>, #privacy-panel, ...).
    if (!first || t === "chats" || location.hash === "#chats") {
      try { history.replaceState(null, "", t === "chats" ? "#chats" : location.pathname + location.search); } catch (e) { /* ignore */ }
    }
    loadTab();
  }
  document.querySelectorAll("#lib-tabs .seg-btn").forEach((b) => {
    b.addEventListener("click", () => { if (b.getAttribute("data-tab") !== tab) setTab(b.getAttribute("data-tab")); });
  });

  // Two-step clear (no modal dialogs needed). The button clears the open tab's kind; "Clear
  // pages and chats" (Chats tab) clears both.
  function twoStep(button, textEl, label, onConfirm) {
    let armed = false, timer = 0;
    const icon = button.querySelector("[data-icon]") || null;
    const set = (on) => {
      armed = on;
      button.classList.toggle("armed", on);
      textEl.textContent = on ? "Click again to clear" : label();
      if (icon) { icon.textContent = ""; icon.appendChild(I.icon("trash", { size: 14, role: on ? "white" : "soft" })); }
    };
    button.addEventListener("click", () => {
      clearTimeout(timer);
      if (!armed) { set(true); timer = setTimeout(() => set(false), 4000); return; }
      set(false);
      onConfirm();
    });
    return set;
  }
  const setClear = twoStep($("clear"), $("clear-text"), () => (tab === "chats" ? "Clear chats" : "Clear pages"),
    () => api("history_clear", { scope: tab }).then(loadTab));
  twoStep($("clear-both"), $("clear-both-text"), () => "Clear pages and chats",
    () => api("history_clear", { scope: "both" }).then(() => { loadHistory(); chats.load(); }));

  const filter = $("filter");
  filter.addEventListener("focus", () => $("search").classList.add("focused"));
  filter.addEventListener("blur", () => $("search").classList.remove("focused"));
  let timer = 0;
  filter.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(loadTab, 150);
  });

  // ---- chat settings: retention, chat sync -------------------------------------------------
  function showChatSettings(r) {
    if (!r || !r.ok) return;
    document.querySelectorAll("#keep-seg .seg-btn").forEach((b) => {
      b.classList.toggle("on", b.getAttribute("data-keep") === r.retention);
    });
    // cloud_sync:begin
    const sw = $("sync-chats");
    if (sw._sw) sw._sw.set(!!r.sync, true);
    else sw.setAttribute("aria-checked", String(!!r.sync));
    // cloud_sync:end
  }
  document.querySelectorAll("#keep-seg .seg-btn").forEach((b) => {
    b.addEventListener("click", () => {
      api("chat_settings_save", { retention: b.getAttribute("data-keep") }).then((r) => { showChatSettings(r); if (tab === "chats") chats.load(); });
    });
  });
  // cloud_sync:begin
  $("sync-chats").addEventListener("change", (e) => {
    const on = e.currentTarget.getAttribute("aria-checked") === "true";
    api("chat_settings_save", { sync: on ? "1" : "0" }).then(showChatSettings);
  });
  // cloud_sync:end
  api("chat_settings").then(showChatSettings);

  // ---- account + sync (only in builds with the cloud_sync feature) -------------------------
  // cloud_sync:begin
  // Secrets travel in the x-mb-secret header as a form, never in the URL.
  function syncApi(op, params, secrets) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    const init = secrets ? { headers: { "x-mb-secret": new URLSearchParams(secrets).toString() } } : undefined;
    return fetch("/api/" + op + "?" + q.toString(), init)
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }

  const FORMS = ["sync-account", "sync-setup", "sync-unlock", "sync-recover", "sync-code-box"];
  let recovering = false;
  let lastSync = null;
  let poll = 0;

  function show(id) {
    for (const f of FORMS) $(f).hidden = f !== id;
  }

  function syncMsg(text, bad) {
    $("sync-msg").textContent = text || "";
    $("sync-msg").classList.toggle("bad", !!bad);
  }

  function ago(ms) {
    const s = Math.round((Date.now() - ms) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return Math.round(s / 60) + " min ago";
    return timeOf(ms);
  }

  function render(st) {
    const ready = st.phase === "ready";
    $("sync-state").textContent = st.busy ? "working…" : ready ? "on" : st.phase === "signed_out" ? "off" : "";
    $("sync-now").hidden = !ready;
    $("sync-out").hidden = st.phase === "signed_out";
    $("sync-chats-row").hidden = !ready;
    $("where").textContent = ready ? "Synced, end-to-end encrypted." : "Kept only on this computer.";
    if (st.recovery_code) {
      show("sync-code-box");
      $("sync-code-text").textContent = st.recovery_code;
    } else if (st.phase === "signed_out") show("sync-account");
    else if (st.phase === "needs_setup") show("sync-setup");
    else if (st.phase === "needs_unlock") show(recovering ? "sync-recover" : "sync-unlock");
    else show(null);
    const info = $("sync-info");
    info.hidden = !st.email;
    if (st.email) {
      let t = "Signed in as " + st.email + ".";
      if (ready && st.last_sync_ms) {
        t += " Last sync " + ago(st.last_sync_ms);
        if (st.last) t += ": " + st.last.pushed + " sent, " + st.last.applied + " received";
        t += ".";
        if (st.last && st.last.unreadable) t += " " + st.last.unreadable + " items couldn't be read with this key.";
      }
      info.textContent = t;
    }
    syncMsg(st.error, true);
    // New data arrived from another computer.
    if (st.last_sync_ms && st.last_sync_ms !== lastSync) {
      if (lastSync !== null && st.last && st.last.applied) { loadBookmarks(); loadTab(); }
      lastSync = st.last_sync_ms;
    }
    clearTimeout(poll);
    if (st.busy) poll = setTimeout(refreshSync, 600);
  }

  function handle(r) {
    if (!r.ok) return syncMsg(r.error || "Something went wrong.", true);
    render(r.sync);
  }
  function refreshSync() { syncApi("sync_status").then(handle); }
  function run(op, params, secrets, clear) {
    syncMsg("");
    syncApi(op, params, secrets).then((r) => {
      for (const id of clear || []) $(id).value = "";
      handle(r);
    });
  }

  function account(op) {
    const email = $("sync-email").value.trim();
    const password = $("sync-password").value;
    if (!email || !password) return syncMsg("Enter your email and password.", true);
    if (op === "sync_signup" && password.length < 8) return syncMsg("Use at least 8 characters for the password.", true);
    run(op, { email: email }, { password: password }, ["sync-password"]);
  }
  $("sync-account").addEventListener("submit", (e) => { e.preventDefault(); account("sync_signin"); });
  $("sync-signup").addEventListener("click", () => account("sync_signup"));
  $("sync-setup").addEventListener("submit", (e) => {
    e.preventDefault();
    const a = $("sync-pass1").value, b = $("sync-pass2").value;
    if (a.length < 8) return syncMsg("Use at least 8 characters.", true);
    if (a !== b) return syncMsg("The two passphrases don't match.", true);
    run("sync_setup", null, { passphrase: a }, ["sync-pass1", "sync-pass2"]);
  });
  $("sync-unlock").addEventListener("submit", (e) => {
    e.preventDefault();
    run("sync_unlock", null, { passphrase: $("sync-pass").value }, ["sync-pass"]);
  });
  $("sync-forgot").addEventListener("click", () => { recovering = true; syncMsg(""); refreshSync(); });
  $("sync-back").addEventListener("click", () => { recovering = false; syncMsg(""); refreshSync(); });
  $("sync-recover").addEventListener("submit", (e) => {
    e.preventDefault();
    const p = $("sync-newpass").value;
    if (p.length < 8) return syncMsg("Use at least 8 characters for the new passphrase.", true);
    recovering = false;
    run("sync_recover", null, { code: $("sync-code").value, passphrase: p }, ["sync-code", "sync-newpass"]);
  });
  $("sync-copy").addEventListener("click", () => {
    const t = $("sync-code-text").textContent;
    if (navigator.clipboard) navigator.clipboard.writeText(t).then(() => syncMsg("Copied."), () => syncMsg("Select the code and copy it.", true));
    else syncMsg("Select the code and copy it.", true);
  });
  $("sync-saved").addEventListener("click", () => run("sync_ack"));
  $("sync-now").addEventListener("click", () => run("sync_now"));
  $("sync-out").addEventListener("click", () => run("sync_signout"));
  // cloud_sync:end

  I.paint(document);
  window.JM.onChange(() => { loadBookmarks(); loadTab(); });
  loadBookmarks();
  setTab(tab, true);
  // Settings > Clear browsing data (settings.js) cleared history and / or chats.
  window.addEventListener("mb-history-cleared", () => { loadTab(); loadBookmarks(); });
  // cloud_sync:begin
  refreshSync();
  // cloud_sync:end
  // ---- General: model + Motion (moved here from the model sheet) ---------------------------
  function showMotion(r) {
    if (!r || !r.ok) return;
    document.querySelectorAll("#motion-seg .seg-btn").forEach((b) => {
      b.classList.toggle("on", b.getAttribute("data-motion") === r.motion);
    });
    window.JM.setMotion(r.effective);
    $("motion-note").textContent = r.system_reduced && r.motion === "full"
      ? "Windows asks for fewer animations, so motion is reduced." : "";
  }
  document.querySelectorAll("#motion-seg .seg-btn").forEach((b) => {
    b.addEventListener("click", () => {
      api("prefs_save", { motion: b.getAttribute("data-motion") })
        .then(() => api("prefs"))
        .then(showMotion);
    });
  });
  api("prefs").then(showMotion);
  // ---- General: On startup (session restore) ----------------------------------------------
  function showStartup(r) {
    if (!r || !r.ok) return;
    document.querySelectorAll("#startup-seg .seg-btn").forEach((b) => {
      b.classList.toggle("on", b.getAttribute("data-startup") === r.mode);
    });
  }
  document.querySelectorAll("#startup-seg .seg-btn").forEach((b) => {
    b.addEventListener("click", () => {
      api("startup_pref_save", { mode: b.getAttribute("data-startup") }).then(showStartup);
    });
  });
  api("startup_pref").then(showStartup);
  api("ai_settings").then((s) => {
    if (!s || !s.ok) return;
    $("gen-model").textContent = s.configured
      ? (s.kind === "ollama" ? "Ollama \u00b7 " : "API \u00b7 ") + s.model
      : "Not set up yet";
  });
})();
