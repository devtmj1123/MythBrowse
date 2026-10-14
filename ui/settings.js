// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Settings (v0.4), the sections library.js does not already run: search engine, theme,
// transparency, downloads, clear browsing data, About + updates (cookies: ui/cookies.js).
// Ops: settings, settings_save, downloads_folder_pick / _reset, downloads_folder,
// downloads_clear, history_clear, site_data_clear, prefs, prefs_save, glass_params,
// update_status / update_check / update_install (engine settings_api.rs, shell settings_host.rs).
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  if (!$("general-panel")) return;
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;
  function api(op, params) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    return fetch("/api/" + op + "?" + q.toString())
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }
  const M = window.JMMotion;
  const swOf = (el) => (el && M ? M.sw(el) : null);
  function setSw(el, on) {
    const s = swOf(el);
    if (s) s.set(!!on, true); else el.setAttribute("aria-checked", String(!!on));
  }
  const isOn = (el) => el.getAttribute("aria-checked") === "true";
  function msg(el, text, bad) { el.textContent = text || ""; el.classList.toggle("bad", !!bad); }

  // ---- settings (search engine, theme, downloads, update_auto) ---------------------------------
  let S = null;
  function show(r) {
    if (!r || r.ok === false) return;
    S = r;
    const sel = $("search-engine");
    if (r.engines && sel.options.length !== r.engines.length) {
      sel.textContent = "";
      r.engines.forEach((e) => { const o = document.createElement("option"); o.value = e.id; o.textContent = e.name; sel.appendChild(o); });
    }
    sel.value = r.search_engine;
    document.querySelectorAll("#theme-seg .seg-btn").forEach((b) => b.classList.toggle("on", b.getAttribute("data-theme") === r.theme));
    $("dl-folder").textContent = r.downloads_folder || "";
    $("dl-folder").title = r.downloads_folder || "";
    $("dl-reset").hidden = !r.downloads_folder_custom;
    $("dl-change").disabled = !!r.picking;
    $("dl-change").textContent = r.picking ? "Choosing\u2026" : "Change\u2026";
    setSw($("dl-ask"), r.ask_save);
    setSw($("upd-auto"), r.update_auto);
    $("about-version").textContent = r.version ? "MythBrowse " + r.version + (r.private ? " \u00b7 private window" : "") : "";
    $("about-commit").textContent = r.commit ? r.commit : "Local build (no commit recorded)";
    if (r.update) showUpdate(r.update);
  }
  function save(params) {
    return api("settings_save", params).then((r) => { if (r.ok === false) msg($("dl-msg"), r.error, true); show(r); return r; });
  }
  $("search-engine").addEventListener("change", (e) => save({ search_engine: e.target.value }));
  document.querySelectorAll("#theme-seg .seg-btn").forEach((b) => b.addEventListener("click", () => {
    const t = b.getAttribute("data-theme");
    save({ theme: t }).then(() => {
      if (window.JM && JM.setTheme) JM.setTheme(t);
      try { localStorage.setItem("mb-theme", t + ":" + Date.now()); } catch (e) { /* other pages refetch on focus */ }
    });
  }));

  // ---- downloads ---------------------------------------------------------------------------------
  let pickPoll = 0;
  function pollPick() {
    clearTimeout(pickPoll);
    api("settings").then((r) => { show(r); if (r && r.picking) pickPoll = setTimeout(pollPick, 700); });
  }
  $("dl-change").addEventListener("click", () => api("downloads_folder_pick").then((r) => {
    if (r.ok === false) return msg($("dl-msg"), r.error, true);
    msg($("dl-msg"), "");
    show(r);
    pickPoll = setTimeout(pollPick, 700);
  }));
  $("dl-reset").addEventListener("click", () => api("downloads_folder_reset").then(show));
  $("dl-open").addEventListener("click", () => api("downloads_folder").then((r) => { if (r.ok === false) msg($("dl-msg"), r.error, true); }));
  $("dl-ask").addEventListener("change", (e) => save({ ask_save: isOn(e.currentTarget) ? "1" : "0" }));
  $("upd-auto").addEventListener("change", (e) => save({ update_auto: isOn(e.currentTarget) ? "1" : "0" }));

  // ---- appearance: transparency + reduce transparency (applies at once) --------------------------
  let look = { v: window.JMGlass ? JMGlass.look.v : 50, reduce: window.JMGlass ? JMGlass.look.reduce : false };
  const trLabel = (v) => (v === 50 ? "Default" : v + "%");
  let sending = false, queued = null;
  function sendLook(p) {
    if (sending) { queued = p; return; }
    sending = true;
    api("glass_params", p).then(() => { sending = false; if (queued) { const q = queued; queued = null; sendLook(q); } });
  }
  const slider = M ? M.slider($("tr-slider"), {
    value: look.v, def: 50,
    onInput: (v) => {
      look.v = v; $("tr-val").textContent = trLabel(v); $("tr-slider").setAttribute("aria-valuetext", trLabel(v));
      if (window.JMGlass) JMGlass.setLook(v, look.reduce);
      sendLook({ v: v, reduce: look.reduce ? 1 : 0 });
    },
    onChange: (v) => { api("prefs_save", { transparency: v }); },
  }) : null;
  function showLook(p) {
    if (!p || p.ok === false || typeof p.transparency !== "number") return;
    look = { v: p.transparency, reduce: !!p.reduce_transparency_effective };
    if (slider) slider.set(look.v, true);
    $("tr-val").textContent = trLabel(look.v);
    $("tr-slider").classList.toggle("off", look.reduce);
    setSw($("set-reduce"), look.reduce);
    $("reduce-note").textContent = p.reduce_transparency === "system"
      ? "Solid surfaces, no blur. Follows Windows\u2019 \u201cTransparency effects\u201d until you change it here."
      : "Solid surfaces, no blur.";
  }
  $("set-reduce").addEventListener("change", (e) => {
    look.reduce = isOn(e.currentTarget);
    $("tr-slider").classList.toggle("off", look.reduce);
    if (window.JMGlass) JMGlass.setLook(look.v, look.reduce);
    api("prefs_save", { reduce_transparency: look.reduce ? "on" : "off" }).then(() => api("prefs")).then(showLook);
  });

  // ---- clear browsing data -----------------------------------------------------------------------
  const cbd = $("cbd");
  function cbdOpen(on) {
    if (on) {
      cbd.hidden = false;
      msg($("cbd-msg"), "");
      // The switches were measured while hidden: put their thumbs where they belong.
      cbd.querySelectorAll(".sw").forEach((x) => { const s = swOf(x); if (s) s.set(isOn(x), true); });
      if (M) M.pop(cbd, $("cbd-open"), { from: 0.6 });
    }
    else if (M) M.unpop(cbd); else cbd.hidden = true;
    $("cbd-open").setAttribute("aria-expanded", String(on));
  }
  $("cbd-open").addEventListener("click", () => cbdOpen(cbd.hidden));
  $("cbd-cancel").addEventListener("click", () => cbdOpen(false));
  $("cbd-go").addEventListener("click", () => {
    const pick = {};
    cbd.querySelectorAll("[data-cbd]").forEach((s) => { pick[s.getAttribute("data-cbd")] = isOn(s); });
    const jobs = [];
    if (pick.history && pick.chats) jobs.push(api("history_clear", { scope: "both" }));
    else if (pick.history) jobs.push(api("history_clear", { scope: "pages" }));
    else if (pick.chats) jobs.push(api("history_clear", { scope: "chats" }));
    if (pick.downloads) jobs.push(api("downloads_clear"));
    const what = ["cookies", "cache"].filter((k) => pick[k]).concat(pick.cookies ? ["storage"] : []);
    if (what.length) jobs.push(api("site_data_clear", { what: what.join(",") }));
    if (!jobs.length) return msg($("cbd-msg"), "Choose something to clear.", true);
    $("cbd-go").disabled = true;
    Promise.all(jobs).then((rs) => {
      $("cbd-go").disabled = false;
      const bad = rs.find((r) => r && r.ok === false);
      if (bad) return msg($("cbd-msg"), bad.error || "Something could not be cleared.", true);
      msg($("cbd-msg"), "Cleared.");
      setTimeout(() => cbdOpen(false), 900);
      try { window.dispatchEvent(new Event("mb-cookies-changed")); } catch (e) { /* ignore */ }
      try { window.dispatchEvent(new Event("mb-history-cleared")); } catch (e) { /* ignore */ }
    });
  });

  // ---- About + updates ---------------------------------------------------------------------------
  const size = (b) => (b >= 1048576 ? (b / 1048576).toFixed(1) + " MB" : Math.max(1, Math.round(b / 1024)) + " KB");
  function ago(ms) {
    if (!ms) return "";
    const m = Math.round((Date.now() - ms) / 60000);
    if (m < 1) return "just now";
    if (m < 60) return m + " min ago";
    const h = Math.round(m / 60);
    return h < 24 ? h + " h ago" : Math.round(h / 24) + " days ago";
  }
  let updPoll = 0;
  function showUpdate(u) {
    if (!u) return;
    const dot = $("upd-dot"), text = $("upd-msg"), bar = $("upd-bar");
    let t = "", cls = "", busy = false, install = false;
    switch (u.phase) {
      case "checking": t = "Checking for updates\u2026"; busy = true; break;
      case "up_to_date": t = "MythBrowse is up to date" + (u.checked_ms ? " \u00b7 checked " + ago(u.checked_ms) : "") + "."; cls = "ok"; break;
      case "available": t = "Update available: v" + u.latest + (u.installer_size ? " (" + size(u.installer_size) + ")" : "") + "."; cls = "new"; install = true; break;
      case "no_installer": t = "v" + u.latest + " is out, but has no Windows installer for this PC yet."; cls = "new"; break;
      case "downloading": t = "Downloading v" + u.latest + "\u2026 " + (u.total ? Math.floor(100 * u.received / u.total) + "%" : size(u.received)); busy = true; break;
      case "verifying": t = "Verifying the download" + (u.checksum ? " (size and SHA-256)" : " (size)") + "\u2026"; busy = true; break;
      case "installing": t = "Windows Installer is open. Follow it to finish the update (it may ask you to close MythBrowse)."; cls = "ok"; break;
      case "error": t = u.error || "The update check failed."; cls = "bad"; break;
      default: t = u.checked_ms ? "Last checked " + ago(u.checked_ms) + "." : "Not checked yet.";
    }
    text.textContent = t;
    dot.className = "upd-dot " + cls;
    $("upd-install").hidden = !install;
    $("upd-check").disabled = busy;
    $("upd-check").hidden = install;
    bar.hidden = u.phase !== "downloading";
    if (u.phase === "downloading" && u.total) bar.firstChild.style.transform = "scaleX(" + Math.min(1, u.received / u.total).toFixed(3) + ")";
    clearTimeout(updPoll);
    if (busy) updPoll = setTimeout(() => api("update_status").then((r) => r && r.update && showUpdate(r.update)), 500);
  }
  $("upd-check").addEventListener("click", () => api("update_check").then((r) => { if (r.ok === false) return showUpdate({ phase: "error", error: r.error }); showUpdate(r.update); }));
  $("upd-install").addEventListener("click", () => {
    $("upd-install").disabled = true;
    api("update_install").then((r) => {
      $("upd-install").disabled = false;
      if (r.ok === false) return showUpdate({ phase: "error", error: r.error });
      showUpdate(r.update);
    });
  });

  // ---- browser identity (ua_mode: prefs / prefs_save, compat ua.rs) ------------------------------
  function showUa(p) {
    if (!p || p.ok === false || !p.ua_mode) return;
    document.querySelectorAll("#ua-seg .seg-btn").forEach((b) => b.classList.toggle("on", b.getAttribute("data-ua") === p.ua_mode));
    if (p.user_agent) $("ua-note").title = p.user_agent;
  }
  document.querySelectorAll("#ua-seg .seg-btn").forEach((b) => b.addEventListener("click", () => {
    api("prefs_save", { ua_mode: b.getAttribute("data-ua") }).then(() => api("prefs")).then(showUa);
  }));

  // ---- site-fix rules (rules_auto_update: prefs / prefs_save, crates/compat) ---------------------
  function showRules(p) {
    if (!p || p.ok === false || p.rules_auto_update === undefined) return;
    setSw($("rules-auto"), p.rules_auto_update);
    if (p.rules_cloud === false) {
      $("rules-note").textContent = "Automatically update site-fix rules. This build has no update server configured, so only the fixes built into it are used.";
    }
  }
  $("rules-auto").addEventListener("change", (e) => {
    api("prefs_save", { rules_auto_update: isOn(e.currentTarget) ? "1" : "0" }).then(() => api("prefs")).then(showRules);
  });
  api("prefs").then(showRules);

  // ---- section links: jump without changing the history tab hash ------------------------------
  const navLinks = Array.from(document.querySelectorAll("#set-nav a"));
  function markNav(a) {
    navLinks.forEach((l) => { l.classList.toggle("on", l === a); if (l === a) l.setAttribute("aria-current", "true"); else l.removeAttribute("aria-current"); });
  }
  navLinks.forEach((a) => a.addEventListener("click", (e) => {
    const t = document.querySelector(a.getAttribute("href"));
    if (!t) return;
    e.preventDefault();
    t.scrollIntoView({ block: "start" });
    markNav(a);
    // Bring the link into the rail's own sideways scroll only (scrollIntoView would also
    // scroll the page back up to the rail).
    const nav = a.parentElement;
    const nr = nav.getBoundingClientRect(), ar = a.getBoundingClientRect();
    if (ar.left < nr.left) nav.scrollLeft -= nr.left - ar.left + 8;
    else if (ar.right > nr.right) nav.scrollLeft += ar.right - nr.right + 8;
  }));
  document.querySelectorAll("a[data-jump]").forEach((a) => a.addEventListener("click", (e) => {
    const t = document.querySelector(a.getAttribute("href"));
    if (!t) return;
    e.preventDefault();
    t.scrollIntoView({ block: "start" });
    markNav(navLinks.find((l) => l.getAttribute("href") === a.getAttribute("href")));
  }));
  // Keyboard focus rings only after a key press (mouse clicks show none).
  document.addEventListener("keydown", (e) => { if (e.key === "Tab" || e.key.startsWith("Arrow")) document.documentElement.classList.add("kbd"); }, true);
  document.addEventListener("mousedown", () => document.documentElement.classList.remove("kbd"), true);

  if (window.JMIcons) JMIcons.paint(document.getElementById("about-panel"));
  api("settings").then(show);
  api("prefs").then((p) => {
    showLook(p);
    showUa(p);
    if (p && p.ok) {
      $("about-renderer").textContent = (p.renderer || "").replace(/ \| (software|hardware)$/, "") +
        (p.software_renderer ? " (software rendering: pages may scroll slower)" : "");
    }
  });
})();
