// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Settings > Privacy & site settings: the address bar's web-suggestions switch, and every
// site's remembered permissions (Allow / Block / Ask) with Reset. /api/site_settings,
// /api/site_setting_set, /api/site_clear, /api/suggest_pref.
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const list = $("sites");
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
  const NAMES = {
    geolocation: "Location", notifications: "Notifications", camera: "Camera", microphone: "Microphone",
    popups: "Pop-ups", midi: "MIDI devices", bluetooth: "Bluetooth", "persistent-storage": "Persistent storage",
    "background-sync": "Background sync", speaker: "Speakers", "device-info": "Media devices", push: "Push messages",
  };
  const sw = $("suggest-web");

  function render(r) {
    if (!r || r.ok === false) return;
    sw.setAttribute("aria-checked", r.suggest_web ? "true" : "false");
    sw.classList.toggle("on", !!r.suggest_web);
    const by = {};
    (r.sites || []).forEach((s) => { (by[s.origin] = by[s.origin] || []).push(s); });
    list.textContent = "";
    const origins = Object.keys(by).sort();
    $("sites-empty").hidden = origins.length > 0;
    origins.forEach((o) => {
      const card = el("div", "ss-site");
      const head = el("div", "ss-head");
      head.appendChild(el("b", "", o.replace(/^https?:\/\//, "")));
      const reset = el("button", "ghost sm", "Reset");
      reset.type = "button";
      reset.addEventListener("click", () => api("site_clear", { origin: o }).then(load));
      head.appendChild(reset);
      card.appendChild(head);
      by[o].forEach((p) => {
        const row = el("div", "ss-row");
        row.appendChild(el("span", "ss-feat", NAMES[p.feature] || p.feature));
        const seg = el("div", "seg ss-seg");
        ["allow", "block", "ask"].forEach((v) => {
          const b = el("button", "seg-btn" + (p.decision === v ? " on" : ""), v === "allow" ? "Allow" : v === "block" ? "Block" : "Ask");
          b.type = "button";
          b.addEventListener("click", () => api("site_setting_set", { origin: o, feature: p.feature, value: v }).then(load));
          seg.appendChild(b);
        });
        row.appendChild(seg);
        card.appendChild(row);
      });
      list.appendChild(card);
    });
  }
  function load() { return api("site_settings").then(render); }
  sw.addEventListener("click", () => {
    const on = sw.getAttribute("aria-checked") !== "true";
    api("suggest_pref", { on: on ? "1" : "0" }).then(load);
  });
  load();
})();
