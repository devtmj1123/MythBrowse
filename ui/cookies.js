// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Settings > Cookies and site data (v0.4.2): every site that keeps cookies or storage, a
// search box, per site Allow / Block / Clear on exit, Clear, and the site's cookies (name,
// domain, path, expiry, flags, size; never a value) with delete-one. Global "clear when
// MythBrowse closes" and "clear all cookies". Deep link: library.html#cookies=<site>.
// Ops: cookies, cookies_list, cookie_delete, cookie_policy, cookies_prefs_save,
// site_data_clear (engine cookies_api.rs / settings_api.rs, shell cookies_host.rs).
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const list = $("ck-sites");
  if (!list) return;
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;
  function api(op, params) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    return fetch("/api/" + op + "?" + q.toString()).then((r) => r.json()).catch((e) => ({ ok: false, error: String(e) }));
  }
  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function button(cls, text, title, onClick) {
    const b = el("button", cls, text);
    b.type = "button";
    if (title) { b.title = title; b.setAttribute("aria-label", title); }
    b.addEventListener("click", (e) => { e.stopPropagation(); onClick(e); });
    return b;
  }
  const M = window.JMMotion;
  const I = window.JMIcons;
  function icon(name, size) { return I ? I.icon(name, { size: size || 14, role: "soft" }) : el("span", "", ""); }
  function bytes(n) { return n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(n < 10240 ? 1 : 0) + " KB" : (n / 1048576).toFixed(1) + " MB"; }
  function expires(t) {
    if (t == null) return "End of session";
    const d = new Date(t * 1000);
    try { return d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }); } catch (e) { return d.toDateString(); }
  }
  const POLICIES = [
    ["allow", "Allow", "Keep this site's cookies, even when clearing on exit"],
    ["block", "Block", "This site may not keep cookies (signs you out of it)"],
    ["clear_exit", "Clear on exit", "Keep them until MythBrowse closes"],
  ];
  const msgEl = $("ck-msg");
  function msg(t, bad) { msgEl.textContent = t || ""; msgEl.classList.toggle("bad", !!bad); }

  let data = null;           // last `cookies` answer
  const open = new Set();    // expanded sites
  const rows = {};           // site -> last cookies_list answer
  let want = null;           // site from the deep link

  function setExit(on) {
    const sw = $("ck-exit");
    const s = sw && M ? M.sw(sw) : null;
    if (s) s.set(!!on, true); else sw.setAttribute("aria-checked", String(!!on));
  }

  function setKeep(on) {
    const sw = $("ck-keep");
    if (!sw) return;
    const s = M ? M.sw(sw) : null;
    if (s) s.set(!!on, true); else sw.setAttribute("aria-checked", String(!!on));
  }

  function render() {
    if (!data) return;
    setExit(data.clear_on_exit);
    setKeep(data.persist_pref);
    const kv = $("ck-keep-val");
    if (kv) {
      kv.textContent = data.private
        ? "A private window never keeps cookies: they end when it closes."
        : data.persist_pref !== data.persistent
        ? (data.persist_pref ? "On after MythBrowse restarts: cookies will then be kept, encrypted for this user." : "Off after MythBrowse restarts: saved cookies are then deleted.")
        : "Stay signed in to sites after restart. Cookies are saved encrypted for this user; a change applies the next time MythBrowse starts.";
    }
    $("ck-3p").textContent = (data.third_party && data.third_party.note) || "";
    $("ck-persist").hidden = data.persistent !== false || !!data.private;
    const q = $("ck-filter").value.trim().toLowerCase();
    const sites = (data.sites || []).filter((s) => !q || s.site.indexOf(q) >= 0);
    const withCookies = (data.sites || []).filter((s) => s.count > 0).length;
    $("ck-count").textContent = withCookies ? String(withCookies) : "";
    list.textContent = "";
    $("ck-empty").hidden = sites.length > 0;
    $("ck-empty").textContent = q ? "No site matches \u201c" + q + "\u201d." : "No site keeps cookies or storage.";
    sites.forEach((s) => list.appendChild(siteEl(s)));
  }

  function siteEl(s) {
    const li = el("li", "ck-site" + (open.has(s.site) ? " open" : ""));
    li.dataset.site = s.site;
    const head = el("div", "ck-head");
    const tog = button("ck-tog", "", (open.has(s.site) ? "Hide" : "Show") + " cookies of " + s.site, () => toggle(s.site));
    tog.setAttribute("aria-expanded", String(open.has(s.site)));
    const chev = el("span", "ck-chev");
    chev.appendChild(icon("chevron", 14));
    tog.appendChild(chev);
    const name = el("span", "ck-name");
    name.appendChild(el("b", "", s.site));
    const bits = [];
    bits.push(s.count === 1 ? "1 cookie" : s.count + " cookies");
    if (s.count) bits.push(bytes(s.size));
    if (s.storage) bits.push("site storage");
    name.appendChild(el("span", "soft small", bits.join(" \u00b7 ")));
    tog.appendChild(name);
    head.appendChild(tog);

    const seg = el("div", "seg ck-seg");
    seg.setAttribute("role", "group");
    seg.setAttribute("aria-label", "Cookies for " + s.site);
    POLICIES.forEach(([id, label, title]) => {
      const on = s.policy === id;
      const b = button("seg-btn" + (on ? " on" : ""), label, title + (on ? " (click again for the default)" : ""), () => setPolicy(s.site, on ? "default" : id));
      b.setAttribute("aria-pressed", String(on));
      seg.appendChild(b);
    });
    head.appendChild(seg);
    const clr = button("pill-btn sm", "Clear", "Delete " + s.site + "'s cookies and site storage", () => clearSite(s.site));
    clr.disabled = !s.count && !s.storage;
    head.appendChild(clr);
    li.appendChild(head);
    if (open.has(s.site)) li.appendChild(cookiesEl(s.site));
    return li;
  }

  function cookiesEl(site) {
    const box = el("div", "ck-body");
    const r = rows[site];
    if (!r) { box.appendChild(el("div", "soft small ck-none", "Loading\u2026")); return box; }
    if (r.ok === false) { box.appendChild(el("div", "set-msg bad", r.error || "Could not read the cookies.")); return box; }
    const cs = r.cookies || [];
    if (!cs.length) {
      box.appendChild(el("div", "soft small ck-none", r.policy === "block" ? "Blocked: this site keeps no cookies." : "No cookies (only site storage, if any)."));
      return box;
    }
    const tbl = el("div", "ck-table");
    tbl.setAttribute("role", "table");
    tbl.setAttribute("aria-label", "Cookies of " + site);
    const hd = el("div", "ck-row ck-th");
    hd.setAttribute("role", "row");
    ["Name", "Domain", "Expires", "", "Size", ""].forEach((t) => { const c = el("span", "", t); c.setAttribute("role", "columnheader"); hd.appendChild(c); });
    tbl.appendChild(hd);
    cs.forEach((c) => {
      const row = el("div", "ck-row");
      row.setAttribute("role", "row");
      const nm = el("span", "ck-cname", c.name || "(no name)");
      nm.title = c.name + (c.path && c.path !== "/" ? "  (path " + c.path + ")" : "");
      row.appendChild(nm);
      const dom = el("span", "ck-dom", (c.host_only ? "" : ".") + c.domain + (c.path && c.path !== "/" ? c.path : ""));
      dom.title = c.host_only ? "Only " + c.domain : c.domain + " and its sub-domains";
      row.appendChild(dom);
      row.appendChild(el("span", "ck-exp", expires(c.expires)));
      const flags = el("span", "ck-flags");
      if (c.secure) flags.appendChild(el("i", "ck-flag", "Secure"));
      if (c.http_only) flags.appendChild(el("i", "ck-flag", "HttpOnly"));
      row.appendChild(flags);
      row.appendChild(el("span", "ck-size", bytes(c.size)));
      const x = button("ck-x", "", "Delete cookie " + c.name, () => {
        api("cookie_delete", { site: site, name: c.name, domain: c.domain, path: c.path || "/" }).then((r2) => {
          if (r2 && r2.ok !== false) rows[site] = r2; else msg((r2 && r2.error) || "Could not delete the cookie.", true);
          load();
        });
      });
      x.appendChild(icon("close", 13));
      row.appendChild(x);
      tbl.appendChild(row);
    });
    box.appendChild(tbl);
    box.appendChild(el("p", "soft small ck-fine", "Cookie values are not shown, and Myth never sees them."));
    return box;
  }

  function toggle(site) {
    if (open.has(site)) { open.delete(site); render(); return; }
    open.add(site);
    render();
    api("cookies_list", { site: site }).then((r) => { rows[site] = r; render(); });
  }
  function refreshOpen() {
    return Promise.all(Array.from(open).map((s) => api("cookies_list", { site: s }).then((r) => { rows[s] = r; })));
  }
  function setPolicy(site, mode) {
    msg("");
    api("cookie_policy", { site: site, mode: mode }).then((r) => {
      if (!r || r.ok === false) return msg((r && r.error) || "Could not save.", true);
      rows[site] = r;
      load();
    });
  }
  function clearSite(site) {
    msg("");
    api("site_data_clear", { what: "cookies,storage", site: site }).then((r) => {
      if (!r || r.ok === false) return msg((r && r.error) || "Could not clear.", true);
      msg("Cleared " + site + ".");
      refreshOpen().then(load);
    });
  }
  function load() {
    return api("cookies").then((r) => {
      if (!r || r.ok === false) { msg((r && r.error) || "Cookies are not available.", true); return; }
      data = r;
      render();
      if (want) {
        const s = want;
        want = null;
        $("ck-filter").value = s;
        if (!open.has(s)) toggle(s); else render();
        const p = $("cookies-panel");
        if (p) p.scrollIntoView({ block: "start" });
      }
    });
  }

  $("ck-filter").addEventListener("input", render);
  $("ck-exit").addEventListener("click", () => {
    const on = $("ck-exit").getAttribute("aria-checked") !== "true";
    api("cookies_prefs_save", { clear_on_exit: on ? "1" : "0" }).then((r) => {
      if (!r || r.ok === false) { msg((r && r.error) || "Could not save.", true); setExit(!on); return; }
      data = r;
      render();
    });
  });
  if ($("ck-keep")) $("ck-keep").addEventListener("click", () => {
    const on = $("ck-keep").getAttribute("aria-checked") !== "true";
    api("cookies_prefs_save", { persist: on ? "1" : "0" }).then((r) => {
      if (!r || r.ok === false) { msg((r && r.error) || "Could not save.", true); setKeep(!on); return; }
      data = r;
      render();
    });
  });
  let armed = 0;
  $("ck-clear-all").addEventListener("click", () => {
    const b = $("ck-clear-all");
    if (!armed) {
      armed = setTimeout(() => { armed = 0; b.textContent = "Clear all cookies"; b.classList.remove("navy"); }, 4000);
      b.textContent = "Sign out everywhere?";
      b.classList.add("navy");
      return;
    }
    clearTimeout(armed);
    armed = 0;
    b.textContent = "Clear all cookies";
    b.classList.remove("navy");
    api("site_data_clear", { what: "cookies" }).then((r) => {
      if (!r || r.ok === false) return msg((r && r.error) || "Could not clear.", true);
      msg("All cookies cleared.");
      refreshOpen().then(load);
    });
  });
  // Settings > Privacy "Clear browsing data" also clears cookies.
  window.addEventListener("mb-cookies-changed", () => refreshOpen().then(load));

  const m = /^#cookies=([a-z0-9.-]+)$/.exec(location.hash || "");
  if (m) want = m[1];
  if (I) I.paint($("cookies-panel"));
  load();
})();
