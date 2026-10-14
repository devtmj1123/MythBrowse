// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Tasks panel: polls /api/tasks, shows each task's step, background tabs with thumbnails,
// approvals; Peek refreshes a live screenshot; Take over opens the page as a normal tab.
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const I = window.JMIcons;
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;
  const shots = {};   // tab id -> { version, image }
  let peekTab = null;
  let peekTimer = null;

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
    if (text !== undefined && text !== null) e.textContent = text;
    return e;
  }
  const clip = (s, n) => (s.length > n ? s.slice(0, n - 1).trimEnd() + "\u2026" : s);
  function hostOf(url) {
    const m = /^[a-z][a-z0-9+.-]*:\/\/([^/?#:]+)/i.exec(url || "");
    return m ? m[1].replace(/^www\./i, "") : "";
  }
  function button(cls, iconName, label, onClick) {
    const b = el("button", cls);
    b.type = "button";
    if (iconName) b.appendChild(I.icon(iconName, { size: 14, role: cls === "primary" ? "inkText" : "soft" }));
    b.appendChild(document.createTextNode(label));
    b.addEventListener("click", onClick);
    return b;
  }

  const STATUS = { running: "Working", waiting: "Needs your OK", done: "Done", stopped: "Stopped", error: "Failed", idle: "Idle" };

  function loadShot(tab, version, img, fresh) {
    const have = shots[tab.id];
    if (!fresh && have && have.version >= version) {
      if (img.src !== have.image) img.src = have.image;
      return Promise.resolve(have);
    }
    return api("task_shot", { tab: tab.id, fresh: fresh ? "1" : "0" }).then((r) => {
      if (r.ok && r.version) {
        shots[tab.id] = { version: r.version, image: r.image };
        img.src = r.image;
      }
      return shots[tab.id];
    });
  }

  function tabRow(tab, running) {
    const row = el("div", "task-tab");
    const thumb = el("div", "thumb");
    const img = el("img");
    img.alt = "";
    thumb.appendChild(img);
    if (!tab.shot) thumb.appendChild(el("span", "thumb-empty", tab.open ? "Loading\u2026" : "Closed"));
    row.appendChild(thumb);
    if (tab.shot) loadShot(tab, tab.shot, img, false);
    const info = el("div", "tab-info");
    const t = el("div", "tab-title");
    const site = el("span", "site", (hostOf(tab.url)[0] || "?").toUpperCase());
    t.appendChild(site);
    t.appendChild(el("span", "t", clip(tab.title || hostOf(tab.url) || tab.url, 60)));
    info.appendChild(t);
    info.appendChild(el("div", "tab-url", clip(tab.url, 90)));
    const acts = el("div", "tab-acts");
    if (tab.open) {
      acts.appendChild(button("ghost", "eye", "Peek", () => openPeek(tab)));
      acts.appendChild(button("ghost", "takeover", "Take over", () => takeover(tab.id)));
    }
    info.appendChild(acts);
    row.appendChild(info);
    return row;
  }

  // Entrances only for what is new (the list is rebuilt when anything changes).
  const seenTasks = new Set();
  const seenApprovals = new Set();
  let firstPaint = true;

  function taskCard(task, index) {
    const card = el("section", "glass panel task");
    if (!seenTasks.has(task.conv)) {
      seenTasks.add(task.conv);
      card.classList.add("mv-slide");
      if (firstPaint) card.style.animationDelay = Math.min(index, 6) * 40 + "ms";
    }
    const head = el("div", "task-head");
    head.appendChild(el("div", "task-title", task.title));
    const chip = el("span", "chip status " + task.status, STATUS[task.status] || task.status);
    head.appendChild(chip);
    if (task.running) {
      head.appendChild(button("ghost", "stop", "Stop", () => api("task_stop", { conv: task.conv }).then(refresh)));
    }
    card.appendChild(head);
    if (task.step) {
      const step = el("div", "task-step");
      step.appendChild(I.icon(task.running ? "globe" : "check", { size: 14, role: "soft" }));
      step.appendChild(document.createTextNode(task.step));
      card.appendChild(step);
    }
    const ap = task.approval;
    if (ap && ap.state === "pending") {
      const box = el("div", "approval");
      if (!seenApprovals.has(ap.id)) {
        seenApprovals.add(ap.id);
        box.classList.add("mv-pop");
      }
      const w = el("div", "w-title");
      w.appendChild(I.icon("shield", { size: 15, role: "soft" }));
      w.appendChild(document.createTextNode("Needs your OK"));
      box.appendChild(w);
      box.appendChild(el("p", "approval-action", ap.action));
      box.appendChild(el("p", "approval-why", "Myth asks first because " + ap.reason + "."));
      const btns = el("div", "approval-btns");
      btns.appendChild(button("primary", null, "Allow", () => api("chat_approve", { conv: task.conv, request: ap.id, allow: "1" }).then(refresh)));
      btns.appendChild(button("ghost", null, "Don\u2019t allow", () => api("chat_approve", { conv: task.conv, request: ap.id, allow: "0" }).then(refresh)));
      box.appendChild(btns);
      card.appendChild(box);
    }
    const tabs = el("div", "task-tabs");
    task.tabs.forEach((t) => tabs.appendChild(tabRow(t, task.running)));
    if (task.tabs.length) card.appendChild(tabs);
    return card;
  }

  let lastJson = "";
  function refresh() {
    return api("tasks").then((r) => {
      if (!r.ok) return;
      const json = JSON.stringify(r.tasks);
      if (json !== lastJson) {
        lastJson = json;
        const list = $("tasks");
        list.textContent = "";
        r.tasks.forEach((t, i) => list.appendChild(taskCard(t, i)));
        firstPaint = false;
        $("empty").hidden = r.tasks.length > 0;
        I.paint(list);
      }
      const busy = r.tasks.some((t) => t.running);
      setTimeout(refresh, busy ? 1000 : 3000);
    });
  }

  function takeover(tab) {
    closePeek();
    api("task_takeover", { tab: tab });
  }

  function peekTick() {
    if (peekTab === null) return;
    loadShot(peekTab, 0, $("peek-img"), true).then(() => {
      peekTimer = setTimeout(peekTick, 1500);
    });
  }

  function openPeek(tab) {
    peekTab = tab;
    $("peek-title").textContent = clip(tab.title || hostOf(tab.url), 60);
    $("peek-url").textContent = tab.url;
    const have = shots[tab.id];
    $("peek-img").src = have ? have.image : "";
    $("peek").hidden = false;
    if (window.JMMotion) JMMotion.pop($("peek").querySelector(".sheet"), null, { from: 0.3 });
    clearTimeout(peekTimer);
    peekTick();
  }

  function closePeek() {
    peekTab = null;
    clearTimeout(peekTimer);
    $("peek").hidden = true;
  }

  $("peek-close").addEventListener("click", closePeek);
  $("peek-takeover").addEventListener("click", () => { if (peekTab) takeover(peekTab.id); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") closePeek(); });

  I.paint(document);
  refresh();
})();
