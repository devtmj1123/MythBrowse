// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Chrome popups that float over the page: the address-bar suggestions dropdown, context
// menus (page menus from the shell, the tab strip's own menu), a page's JS dialog / permission
// sheet, and the pop-up blocked indicator. Same glass as the compat / Tasks panels
// (data-glass="f", --myth-* tokens). Only one popup shows at a time; its rectangle goes to
// the shell (/api/popup) so it is composed over the page and gets the pointer there.
// The Windows 11 Snap Layouts hit rectangle of the maximize button goes to /api/max_button.
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;
  const I = window.JMIcons;
  const omnibox = $("omnibox");
  const wrap = $("omnibox-wrap");

  function api(op, params) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    return fetch("/api/" + op + "?" + q.toString())
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }
  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function icon(name, size, role) {
    return I && I.icon ? I.icon(name, { size: size || 16, role: role || "soft" }) : el("span");
  }

  let state = null;

  // ------------------------------------------------------------------ popup rectangle
  const sugBox = el("div", "pop sug");
  sugBox.id = "suggest";
  sugBox.setAttribute("role", "listbox");
  sugBox.setAttribute("data-glass", "f");
  sugBox.hidden = true;
  const menuBox = el("div", "pop menu");
  menuBox.id = "ctx-menu";
  menuBox.setAttribute("role", "menu");
  menuBox.setAttribute("data-glass", "f");
  menuBox.tabIndex = -1;
  menuBox.hidden = true;
  const sheet = el("div", "pop sheet");
  sheet.id = "page-sheet";
  sheet.setAttribute("role", "alertdialog");
  sheet.setAttribute("data-glass", "f");
  sheet.hidden = true;
  // Site popover (v0.4.2): the address bar's site icon -> connection, cookie count, the
  // site's cookie policy, Clear, Manage (library.html#cookies=<site>).
  const siteBox = el("div", "pop site-pop");
  siteBox.id = "site-pop";
  siteBox.setAttribute("role", "dialog");
  siteBox.setAttribute("aria-label", "Site information");
  siteBox.setAttribute("data-glass", "f");
  siteBox.tabIndex = -1;
  siteBox.hidden = true;
  document.body.appendChild(sugBox);
  document.body.appendChild(menuBox);
  document.body.appendChild(sheet);
  document.body.appendChild(siteBox);

  let lastRect = "";
  function reportPopup() {
    const open = [sheet, menuBox, siteBox, sugBox].find((e) => !e.hidden);
    let key = "clear";
    let params = { clear: "1" };
    if (open) {
      const r = open.getBoundingClientRect();
      const rad = parseFloat(getComputedStyle(open).borderTopLeftRadius) || 0;
      params = { x: Math.floor(r.left), y: Math.floor(r.top), w: Math.ceil(r.width) + 1, h: Math.ceil(r.height) + 1, r: Math.round(rad) };
      key = [params.x, params.y, params.w, params.h, params.r].join(",");
    }
    if (key === lastRect) return;
    lastRect = key;
    api("popup", params);
  }
  function soon(fn) { requestAnimationFrame(() => requestAnimationFrame(fn)); }

  // ------------------------------------------------------------------ suggestions
  let seq = 0;
  let items = [];
  let sel = -1;
  let typed = "";       // what the user typed (without the inline completion)
  let deleting = false; // the last edit removed text: no inline completion
  let timer = 0;

  const KIND_ICON = { go: "globe", history: "history", bookmark: "star", tab: "tabs", search: "search", ask_myth: null };
  const KIND_HINT = { go: "", history: "", bookmark: "Bookmark", tab: "Switch to tab", search: "Search", ask_myth: "Ask Myth" };

  function hostOf(url) {
    try { return new URL(url).host.replace(/^www\./, ""); } catch (_) { return url || ""; }
  }
  function shortUrl(url) {
    const rest = (url || "").replace(/^[a-z]+:\/\/[^/]+/i, "");
    return hostOf(url) + (rest.length > 1 ? rest : "");
  }

  function rowEl(it, i) {
    // "k-" prefix: the kind names (tab, ...) must not pick up the tab strip's styles.
    const r = el("div", "sug-row k-" + it.kind);
    r.setAttribute("role", "option");
    r.dataset.i = String(i);
    const ic = el("span", "sug-ic");
    if (it.kind === "ask_myth") {
      const m = el("span", "mark");
      m.innerHTML = "<i></i><i></i><i></i>";
      ic.appendChild(m);
    } else {
      ic.appendChild(icon(KIND_ICON[it.kind] || "globe", 15, "soft"));
    }
    r.appendChild(ic);
    const main = el("span", "sug-main");
    main.appendChild(el("span", "sug-title", it.title));
    if (it.url && it.kind !== "search" && it.kind !== "go") main.appendChild(el("span", "sug-url", shortUrl(it.url)));
    r.appendChild(main);
    const hint = KIND_HINT[it.kind];
    if (hint) r.appendChild(el("span", "sug-hint", hint));
    if (it.deletable) {
      const x = el("button", "sug-del");
      x.type = "button";
      x.title = "Remove from history (Shift+Delete)";
      x.setAttribute("aria-label", "Remove from history");
      x.appendChild(icon("close", 12, "soft"));
      x.addEventListener("mousedown", (e) => { e.preventDefault(); e.stopPropagation(); removeRow(i); });
      r.appendChild(x);
    }
    r.addEventListener("mousedown", (e) => { e.preventDefault(); choose(i); });
    r.addEventListener("mousemove", () => { if (sel !== i) { sel = i; paintSel(); } });
    return r;
  }

  function placeSug() {
    const r = wrap.getBoundingClientRect();
    sugBox.style.left = Math.round(r.left) + "px";
    sugBox.style.top = Math.round(r.bottom + 6) + "px";
    sugBox.style.width = Math.round(r.width) + "px";
  }

  function renderSug() {
    sugBox.textContent = "";
    const show = document.activeElement === omnibox && items.length > 0 && !(state && state.dialog);
    if (!show) { hideSug(); return; }
    items.forEach((it, i) => sugBox.appendChild(rowEl(it, i)));
    if (state && state.suggest_web === false) sugBox.appendChild(el("div", "sug-foot", "Web suggestions are off"));
    placeSug();
    sugBox.hidden = false;
    menuBox.hidden = true;
    paintSel();
    soon(reportPopup);
  }
  function hideSug() {
    if (sugBox.hidden) return;
    sugBox.hidden = true;
    sel = -1;
    soon(reportPopup);
  }
  function paintSel() {
    sugBox.querySelectorAll(".sug-row").forEach((r) => r.classList.toggle("on", Number(r.dataset.i) === sel));
  }

  // The shell's reply (also pushed again when web suggestions arrive: mb.suggest).
  function onSuggest(r) {
    if (!r || r.seq !== seq || r.q !== typed) return;
    items = r.items || [];
    if (sel >= items.length) sel = -1;
    renderSug();
    applyInline(r.inline);
  }

  // Inline autocomplete: the completion follows the caret, selected (typing replaces it).
  function applyInline(full) {
    if (deleting || !full || document.activeElement !== omnibox) return;
    if (omnibox.value !== typed) return; // the user moved on
    if (full.length <= typed.length || full.slice(0, typed.length).toLowerCase() !== typed.toLowerCase()) return;
    omnibox.value = typed + full.slice(typed.length);
    omnibox.setSelectionRange(typed.length, omnibox.value.length);
  }

  function query() {
    clearTimeout(timer);
    const q = typed;
    if (!q.trim()) { items = []; hideSug(); return; }
    seq += 1;
    const my = seq;
    // Debounced: the shell answers local rows at once and fetches web rows (if on) itself.
    timer = setTimeout(() => {
      api("suggest", { q: q, seq: String(my) }).then((r) => { if (r && r.ok !== false) onSuggest(r); });
    }, 40);
  }

  omnibox.addEventListener("beforeinput", (e) => { deleting = /^delete/.test(e.inputType || ""); });
  omnibox.addEventListener("input", () => { typed = omnibox.value; sel = -1; query(); });
  omnibox.addEventListener("focus", () => { typed = omnibox.value; items = []; });
  omnibox.addEventListener("blur", () => { clearTimeout(timer); items = []; hideSug(); });

  function choose(i) {
    const it = items[i];
    if (!it) return;
    hideSug();
    omnibox.blur();
    if (it.kind === "tab" && it.tab != null) api("activate_tab", { id: String(it.tab) });
    else api("navigate", { input: it.input });
  }

  function removeRow(i) {
    const it = items[i];
    if (!it || !it.deletable) return;
    api("suggest_delete", { url: it.url }).then(() => {
      items.splice(i, 1);
      if (sel >= items.length) sel = items.length - 1;
      renderSug();
      query();
    });
  }

  function previewSel() {
    const it = items[sel];
    if (!it) { omnibox.value = typed; return; }
    omnibox.value = it.kind === "ask_myth" || it.kind === "search" ? it.title : it.url || it.input;
    omnibox.setSelectionRange(omnibox.value.length, omnibox.value.length);
  }

  // ------------------------------------------------------------------ menus
  // One renderer for the shell's page menu (`state.menu`) and local menus (tab strip, the
  // pop-up indicator). `onPick(id|null)`.
  let menu = null; // {key, items, x, y, onPick}
  let menuSel = -1;

  function openMenu(m) {
    menu = m;
    menuSel = -1;
    menuBox.textContent = "";
    m.items.forEach((it, i) => {
      if (it.id === "-") { menuBox.appendChild(el("div", "menu-sep")); return; }
      const b = el("button", "menu-item");
      b.type = "button";
      b.setAttribute("role", "menuitem");
      b.dataset.i = String(i);
      b.disabled = !it.enabled;
      b.appendChild(el("span", "menu-label", it.label));
      if (it.key) b.appendChild(el("span", "menu-key", it.key));
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", (e) => { e.stopPropagation(); if (it.enabled) closeMenu(it.id); });
      b.addEventListener("mousemove", () => { menuSel = i; paintMenu(); });
      menuBox.appendChild(b);
    });
    hideSug();
    menuBox.hidden = false;
    // Keep it inside the window; open up / left when there is no room.
    const W = window.innerWidth, H = window.innerHeight;
    const w = menuBox.offsetWidth, h = menuBox.offsetHeight;
    let x = m.x, y = m.y;
    if (x + w > W - 6) x = Math.max(6, x - w);
    if (y + h > H - 6) y = Math.max(6, H - 6 - h);
    menuBox.style.left = Math.round(x) + "px";
    menuBox.style.top = Math.round(y) + "px";
    menuBox.classList.remove("pop-in");
    void menuBox.offsetWidth;
    menuBox.classList.add("pop-in");
    menuBox.focus();
    soon(reportPopup);
  }
  function closeMenu(pick) {
    if (!menu) return;
    const m = menu;
    menu = null;
    menuBox.hidden = true;
    soon(reportPopup);
    m.onPick(pick == null ? null : pick);
  }
  function paintMenu() {
    menuBox.querySelectorAll(".menu-item").forEach((b) => b.classList.toggle("on", Number(b.dataset.i) === menuSel));
  }
  function moveMenu(step) {
    if (!menu) return;
    const n = menu.items.length;
    for (let k = 0; k < n; k++) {
      menuSel = (menuSel + step + n) % n;
      const it = menu.items[menuSel];
      if (it.id !== "-" && it.enabled) break;
    }
    paintMenu();
  }

  // The shell's context menu (page / link / image / selection / text field).
  let shellMenuId = null;
  function renderShellMenu(s) {
    const m = s.menu;
    if (!m) {
      if (shellMenuId != null && menu && menu.key === "shell:" + shellMenuId) { menu = null; menuBox.hidden = true; soon(reportPopup); }
      shellMenuId = null;
      return;
    }
    if (shellMenuId === m.id) return;
    shellMenuId = m.id;
    openMenu({
      key: "shell:" + m.id, items: m.items, x: m.x, y: m.y,
      onPick: (id) => { api("menu_pick", id ? { id: String(m.id), action: id } : { id: String(m.id) }); },
    });
  }

  // Tab strip menu (built here; the shell checks the command).
  document.addEventListener("contextmenu", (e) => {
    const tab = e.target.closest && e.target.closest(".tab");
    if (tab && tab._id != null) {
      e.preventDefault();
      const tabs = (state && state.tabs) || [];
      const idx = tabs.findIndex((t) => t.id === tab._id);
      openMenu({
        key: "tab:" + tab._id, x: e.clientX, y: e.clientY,
        items: [
          { id: "new_tab", label: "New tab", enabled: true, key: "Ctrl+T" },
          { id: "reload", label: "Reload", enabled: true },
          { id: "duplicate", label: "Duplicate", enabled: true },
          { id: "pin", label: tab._pinned ? "Unpin tab" : "Pin tab", enabled: true },
          { id: "-" },
          { id: "close", label: "Close tab", enabled: true, key: "Ctrl+W" },
          { id: "close_others", label: "Close other tabs", enabled: tabs.length > 1 },
          { id: "close_right", label: "Close tabs to the right", enabled: idx >= 0 && idx < tabs.length - 1 },
        ],
        onPick: (id) => {
          if (id === "pin") api("pin_tab", { id: String(tab._id), on: tab._pinned ? "0" : "1" });
          else if (id) api("tab_menu", { id: String(tab._id), action: id });
        },
      });
      return;
    }
    // The chrome's own text fields get the shell's editing menu; nothing else in the chrome.
    if (!(e.target.closest && e.target.closest("input, textarea"))) e.preventDefault();
  });

  document.addEventListener("mousedown", (e) => {
    if (menu && !menuBox.contains(e.target)) closeMenu(null);
  }, true);

  // ------------------------------------------------------------------ pop-up indicator
  const popBtn = el("button", "nav popups-btn");
  popBtn.type = "button";
  popBtn.id = "popups";
  popBtn.title = "Pop-up blocked";
  popBtn.setAttribute("aria-label", "Pop-up blocked");
  popBtn.hidden = true;
  popBtn.appendChild(icon("popup", 16, "soft"));
  const popCount = el("i", "popups-dot");
  popBtn.appendChild(popCount);
  const compatBtn = $("compat");
  if (compatBtn && compatBtn.parentNode) compatBtn.parentNode.insertBefore(popBtn, compatBtn);
  popBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    const pb = state && state.popups_blocked;
    if (!pb) return;
    const r = popBtn.getBoundingClientRect();
    openMenu({
      key: "popups", x: r.left, y: r.bottom + 6,
      items: [
        { id: "info", label: (pb.count > 1 ? pb.count + " pop-ups" : "A pop-up") + " blocked on " + pb.origin.replace(/^https?:\/\//, ""), enabled: false },
        { id: "-" },
        { id: "always", label: "Always allow pop-ups from this site", enabled: true },
        { id: "dismiss", label: "Keep blocking", enabled: true },
      ],
      onPick: (id) => { if (id === "always") api("popups", { always: "1" }); else if (id === "dismiss") api("popups", {}); },
    });
  });

  // ------------------------------------------------------------------ site popover
  const lead = $("lead");
  let siteFor = "";   // the page URL the popover was opened for
  let siteSeq = 0;
  function pageSite(s) {
    return s && s.url && !s.internal && /^https?:/i.test(s.url) ? s.url : "";
  }
  function closeSite() {
    if (siteBox.hidden) return;
    siteBox.hidden = true;
    siteFor = "";
    lead.setAttribute("aria-expanded", "false");
    soon(reportPopup);
  }
  function siteAct(label, cls, title, fn) {
    const b = el("button", "sh-btn" + (cls ? " " + cls : ""), label);
    b.type = "button";
    if (title) b.title = title;
    b.addEventListener("mousedown", (e) => e.preventDefault());
    b.addEventListener("click", (e) => { e.stopPropagation(); fn(); });
    return b;
  }
  function renderSite(r, url) {
    siteBox.textContent = "";
    const secure = /^https:/i.test(url);
    let host = "";
    try { host = new URL(url).host; } catch (e) { host = url; }
    const head = el("div", "sp-head");
    const ic = el("span", "sp-ic" + (secure ? "" : " bad"));
    ic.appendChild(icon(secure ? "lock" : "alert", 16, secure ? "soft" : "bad"));
    head.appendChild(ic);
    const t = el("div", "sp-title");
    t.appendChild(el("b", "", host));
    t.appendChild(el("span", "sp-sub", secure ? "Connection is secure" : "Not secure: what you send here can be read on the way"));
    head.appendChild(t);
    siteBox.appendChild(head);
    if (!r || r.ok === false || !r.site) {
      siteBox.appendChild(el("div", "sp-row sp-soft", (r && r.error) || "Cookie details are not available for this page."));
    } else {
      const row = el("div", "sp-row");
      const n = r.count || 0;
      row.appendChild(el("span", "sp-k", "Cookies"));
      row.appendChild(el("span", "sp-v", r.policy === "block" ? "Blocked for " + r.site : n === 1 ? "1 cookie from " + r.site : n + " cookies from " + r.site));
      siteBox.appendChild(row);
      const seg = el("div", "sp-seg");
      seg.setAttribute("role", "group");
      seg.setAttribute("aria-label", "Cookies for " + r.site);
      [["allow", "Allow"], ["block", "Block"], ["clear_exit", "Clear on exit"]].forEach(([id, label]) => {
        const on = r.policy === id;
        const b = el("button", "sp-opt" + (on ? " on" : ""), label);
        b.type = "button";
        b.setAttribute("aria-pressed", String(on));
        b.title = on ? "Back to the default" : id === "block" ? "This site may not keep cookies (signs you out)" : id === "allow" ? "Keep its cookies, even when clearing on exit" : "Keep them until MythBrowse closes";
        b.addEventListener("mousedown", (e) => e.preventDefault());
        b.addEventListener("click", (e) => {
          e.stopPropagation();
          api("cookie_policy", { site: r.site, mode: on ? "default" : id }).then(() => openSite(true));
        });
        seg.appendChild(b);
      });
      siteBox.appendChild(seg);
      const acts = el("div", "sh-acts sp-acts");
      const clr = siteAct("Clear cookies", "", "Delete " + r.site + "'s cookies and site storage (signs you out)", () => {
        api("site_data_clear", { what: "cookies,storage", site: r.site }).then(() => openSite(true));
      });
      clr.disabled = !n;
      acts.appendChild(clr);
      acts.appendChild(siteAct("Manage", "primary", "Settings > Cookies", () => {
        const site = r.site;
        closeSite();
        api("navigate", { input: "https://mythbrowse.internal/library.html#cookies=" + site });
      }));
      siteBox.appendChild(acts);
    }
    const W = window.innerWidth;
    const lr = lead.getBoundingClientRect();
    const wr = wrap.getBoundingClientRect();
    siteBox.style.left = Math.round(Math.max(6, Math.min(lr.left - 6, W - siteBox.offsetWidth - 6))) + "px";
    siteBox.style.top = Math.round(wr.bottom + 6) + "px";
    soon(reportPopup);
  }
  function openSite(refresh) {
    const url = pageSite(state);
    if (!url) return closeSite();
    const seq = ++siteSeq;
    if (!refresh) {
      hideSug();
      if (menu) closeMenu(null);
      siteFor = url;
      siteBox.hidden = false;
      lead.setAttribute("aria-expanded", "true");
      renderSite({ ok: true, site: null, error: "Looking\u2026" }, url);
      siteBox.classList.remove("pop-in");
      void siteBox.offsetWidth;
      siteBox.classList.add("pop-in");
      siteBox.focus();
    }
    api("cookie_site").then((r) => { if (seq === siteSeq && !siteBox.hidden) renderSite(r, url); });
  }
  lead.addEventListener("mousedown", (e) => { if (pageSite(state) && document.activeElement !== omnibox) e.preventDefault(); });
  lead.addEventListener("click", (e) => {
    if (!pageSite(state) || document.activeElement === omnibox) return;
    e.stopPropagation();
    if (siteBox.hidden) openSite(false); else closeSite();
  });
  lead.addEventListener("keydown", (e) => {
    if ((e.key === "Enter" || e.key === " ") && pageSite(state)) { e.preventDefault(); if (siteBox.hidden) openSite(false); else closeSite(); }
  });
  siteBox.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeSite(); } });
  document.addEventListener("mousedown", (e) => {
    if (!siteBox.hidden && !siteBox.contains(e.target) && !lead.contains(e.target)) closeSite();
  }, true);
  omnibox.addEventListener("focus", closeSite);

  // ------------------------------------------------------------------ dialog / permission sheet
  let sheetId = null;
  function cardRect(s) {
    const gap = (s.frame && s.frame.gap) || 0;
    const top = (parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--chrome-height")) || 88) + gap;
    return { left: gap, top: top, width: window.innerWidth - 2 * gap };
  }
  function permIcon(f) {
    return { geolocation: "pin", notifications: "bell", camera: "camera", microphone: "mic" }[f] || "lock";
  }

  function renderSheet(s) {
    const d = s.dialog;
    if (!d) {
      if (!sheet.hidden) { sheet.hidden = true; sheetId = null; soon(reportPopup); }
      return;
    }
    if (sheetId === d.id) return;
    sheetId = d.id;
    hideSug();
    if (menu) closeMenu(null);
    sheet.textContent = "";
    const k = d.kind || {};
    const isPerm = k.type === "permission";
    sheet.className = "pop sheet " + (isPerm ? "perm" : "dlg");
    const host = (d.origin || "").replace(/^https?:\/\//, "");
    // Who is asking: the site, in bold, so page text never reads as the browser's own.
    const head = el("div", "sh-head");
    head.appendChild(icon(isPerm ? permIcon(k.feature) : "globe", 16, "soft"));
    const who = el("span", "sh-who");
    who.appendChild(el("b", "", host || "This page"));
    who.appendChild(document.createTextNode(isPerm ? " wants to " + (d.label || "use a device feature") : k.type === "before_unload" ? "" : " says"));
    head.appendChild(who);
    sheet.appendChild(head);
    let input = null;
    if (k.type === "before_unload") {
      sheet.appendChild(el("div", "sh-title", "Leave site?"));
      sheet.appendChild(el("div", "sh-msg soft", "Changes you made may not be saved."));
    } else if (!isPerm && d.message) {
      sheet.appendChild(el("div", "sh-msg", d.message));
    }
    if (k.type === "prompt") {
      input = el("input", "field sh-input");
      input.type = "text";
      input.value = k.default || "";
      input.spellcheck = false;
      sheet.appendChild(input);
    }
    let check = null;
    if (d.suppress || isPerm) {
      const lab = el("label", "sh-check");
      check = el("input");
      check.type = "checkbox";
      check.checked = isPerm; // permissions: remember by default
      lab.appendChild(check);
      lab.appendChild(document.createTextNode(isPerm ? " Remember for this site" : " Prevent this page from creating more dialogs"));
      sheet.appendChild(lab);
    }
    const acts = el("div", "sh-acts");
    const answer = (ok) => {
      const p = { id: String(d.id), ok: ok ? "1" : "0" };
      if (input) p.value = input.value;
      if (check && check.checked) p[isPerm ? "remember" : "suppress"] = "1";
      sheet.hidden = true;
      soon(reportPopup);
      api("dialog_answer", p);
    };
    const labels = isPerm ? ["Block", "Allow"] : k.type === "before_unload" ? ["Stay", "Leave"] : ["Cancel", "OK"];
    if (k.type !== "alert") {
      const no = el("button", "sh-btn", labels[0]);
      no.type = "button";
      no.addEventListener("click", () => answer(false));
      acts.appendChild(no);
    }
    const yes = el("button", "sh-btn primary", labels[1]);
    yes.type = "button";
    yes.addEventListener("click", () => answer(true));
    acts.appendChild(yes);
    sheet.appendChild(acts);
    sheet._answer = answer;
    sheet._perm = isPerm;
    // Anchor: dialogs hang from the top centre of the page card; permission prompts sit under
    // the address bar's site icon (where the site is named).
    const c = cardRect(s);
    sheet.hidden = false;
    const w = Math.min(isPerm ? 380 : 440, c.width - 24);
    sheet.style.width = w + "px";
    if (isPerm) {
      const r = wrap.getBoundingClientRect();
      sheet.style.left = Math.round(Math.max(c.left + 12, r.left)) + "px";
    } else {
      sheet.style.left = Math.round(c.left + (c.width - w) / 2) + "px";
    }
    sheet.style.top = Math.round(c.top + 10) + "px";
    sheet.classList.remove("pop-in");
    void sheet.offsetWidth;
    sheet.classList.add("pop-in");
    // Focus: the text field of a prompt, else the safe button (Enter never allows a
    // permission by default).
    if (input) { input.focus(); input.select(); } else (isPerm ? acts.firstChild : yes).focus();
    soon(reportPopup);
  }

  // ------------------------------------------------------------------ keyboard
  document.addEventListener("keydown", (e) => {
    if (!sheet.hidden && sheet._answer) {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); sheet._answer(false); return; }
      if (e.key === "Enter" && !sheet._perm && !(e.target && e.target.tagName === "BUTTON")) {
        e.preventDefault(); e.stopPropagation(); sheet._answer(true); return;
      }
      return;
    }
    if (menu) {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeMenu(null); }
      else if (e.key === "ArrowDown") { e.preventDefault(); moveMenu(1); }
      else if (e.key === "ArrowUp") { e.preventDefault(); moveMenu(-1); }
      else if (e.key === "Enter") {
        e.preventDefault(); e.stopPropagation();
        const it = menu.items[menuSel];
        if (it && it.enabled && it.id !== "-") closeMenu(it.id);
      }
      return;
    }
    if (e.target !== omnibox || sugBox.hidden) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const n = items.length;
      if (!n) return;
      sel = e.key === "ArrowDown" ? (sel + 1 >= n ? -1 : sel + 1) : (sel - 1 < -1 ? n - 1 : sel - 1);
      paintSel();
      previewSel();
    } else if (e.key === "Enter" && sel >= 0) {
      e.preventDefault(); e.stopPropagation();
      choose(sel);
    } else if (e.key === "Escape") {
      // First Esc closes the list (and drops the completion); the next one leaves the field.
      e.preventDefault(); e.stopPropagation();
      omnibox.value = typed;
      items = [];
      hideSug();
    } else if (e.key === "Delete" && e.shiftKey && sel >= 0 && items[sel] && items[sel].deletable) {
      e.preventDefault();
      removeRow(sel);
    } else if (e.key === "Tab" && sel < 0 && omnibox.selectionEnd > omnibox.selectionStart) {
      // Accept the inline completion.
      e.preventDefault();
      omnibox.setSelectionRange(omnibox.value.length, omnibox.value.length);
      typed = omnibox.value;
      query();
    }
  }, true);

  // ------------------------------------------------------------------ snap layouts
  const maxBtn = $("win-max");
  let lastMax = "";
  function reportMax() {
    if (!maxBtn) return;
    const r = maxBtn.getBoundingClientRect();
    const key = r.width ? [r.left, r.top, r.width, r.height].map(Math.round).join(",") : "clear";
    if (key === lastMax) return;
    lastMax = key;
    api("max_button", r.width ? { x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) } : { clear: "1" });
  }
  window.addEventListener("resize", () => { reportMax(); if (!sugBox.hidden) { placeSug(); soon(reportPopup); } });

  // ------------------------------------------------------------------ state
  function render(s) {
    state = s;
    renderShellMenu(s);
    renderSheet(s);
    const pb = s.popups_blocked;
    popBtn.hidden = !pb || !!s.internal;
    popCount.hidden = !pb;
    const site = pageSite(s);
    wrap.classList.toggle("has-site", !!site);
    if (site) { lead.setAttribute("role", "button"); lead.tabIndex = 0; lead.title = "Site information and cookies"; lead.setAttribute("aria-haspopup", "dialog"); }
    else { lead.removeAttribute("role"); lead.removeAttribute("tabindex"); lead.removeAttribute("aria-haspopup"); lead.title = "Enter an address, ask Myth, or start with ? to search the web"; }
    if (!siteBox.hidden && site !== siteFor) closeSite();
    reportMax();
  }

  window.mbPopups = {
    render: render,
    suggest: onSuggest,
    snapHover: (on) => { if (maxBtn) maxBtn.classList.toggle("snap-hover", !!on); },
    // For tests.
    _state: () => ({ items: items, sel: sel, typed: typed, menu: !!menu, sheet: !sheet.hidden, site: !siteBox.hidden, rect: lastRect }),
  };
})();
