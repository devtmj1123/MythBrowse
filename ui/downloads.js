// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Downloads page: one row per download with progress, Pause / Resume / Cancel / Retry, Open /
// Show in folder, and the Keep / Discard question for files that can run code. The shell
// pushes the list with window.mbDownloads(list) whenever something changes, so the page never
// polls.
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

  function size(n) {
    if (n == null) return "";
    if (n < 1024) return n + " B";
    const u = ["KB", "MB", "GB", "TB"];
    let v = n / 1024;
    let i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return (v >= 100 ? v.toFixed(0) : v.toFixed(1)) + " " + u[i];
  }

  function eta(d) {
    if (!d.speed || d.total == null || d.total <= d.received) return "";
    const s = Math.round((d.total - d.received) / d.speed);
    if (s < 60) return s + " s left";
    if (s < 3600) return Math.round(s / 60) + " min left";
    return Math.round(s / 3600) + " h left";
  }

  function hostOf(url) {
    const m = /^[a-z][a-z0-9+.-]*:\/\/([^/?#:]+)/i.exec(url || "");
    return m ? m[1].replace(/^www\./i, "") : "";
  }

  function status(d) {
    const of = d.total != null ? " of " + size(d.total) : "";
    switch (d.state) {
      case "pending": return "Starting…";
      case "confirm": return "Waiting for you";
      case "running": return [size(d.received) + of, d.speed ? size(d.speed) + "/s" : "", eta(d)].filter(Boolean).join(" · ");
      case "paused": return "Paused · " + size(d.received) + of;
      case "done": return [size(d.total != null ? d.total : d.received), hostOf(d.final_url || d.url)].filter(Boolean).join(" · ");
      case "failed": return d.error || "Failed";
      case "cancelled": return "Cancelled";
      default: return "";
    }
  }

  function button(label, op, d, cls, icon) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = cls || "ghost";
    if (icon) {
      const s = document.createElement("span");
      s.setAttribute("data-icon", icon);
      s.setAttribute("data-size", "14");
      s.setAttribute("data-role", cls === "primary" ? "inkText" : "soft");
      b.appendChild(s);
    }
    b.appendChild(document.createTextNode(label));
    b.setAttribute("data-op", op);
    b.addEventListener("click", () => act(d, op));
    return b;
  }

  function act(d, op) {
    const params = { id: String(d.id), op: op };
    if (op === "open" && d.danger) {
      // Opening a program is its own decision, even after Keep.
      if (!window.confirm(d.name + " can run code on this computer. Open it?")) return;
      params.confirm = "1";
    }
    api("download_action", params).then((r) => {
      if (!r.ok && r.error) flash(r.error);
      if (r.downloads) render(r.downloads);
    });
  }

  let flashTimer = 0;
  function flash(msg) {
    const w = $("where");
    w.textContent = msg;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => { w.textContent = folderText; }, 5000);
  }

  function row(d) {
    const li = document.createElement("li");
    li.className = "dl " + d.state;
    li.setAttribute("data-id", String(d.id));
    const ico = document.createElement("span");
    ico.className = "ico";
    ico.setAttribute("data-icon", d.danger ? "alert" : "file");
    ico.setAttribute("data-size", "17");
    ico.setAttribute("data-role", "soft");
    li.appendChild(ico);

    const body = document.createElement("div");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = d.name;
    name.title = d.path || d.url;
    if (d.state === "done") {
      name.setAttribute("data-open", "1");
      name.addEventListener("click", () => act(d, "open"));
    }
    body.appendChild(name);
    const sub = document.createElement("span");
    sub.className = "sub";
    sub.textContent = status(d);
    if (d.by_agent) {
      const a = document.createElement("span");
      a.className = "agent";
      a.textContent = "· by Myth";
      sub.appendChild(a);
    }
    body.appendChild(sub);
    if (d.state === "running" || d.state === "paused" || d.state === "pending") {
      const bar = document.createElement("div");
      const known = d.total != null && d.total > 0;
      bar.className = "bar" + (known || d.state === "paused" ? "" : " indet");
      const fill = document.createElement("i");
      if (known) fill.style.width = Math.min(100, (100 * d.received) / d.total).toFixed(1) + "%";
      bar.appendChild(fill);
      body.appendChild(bar);
    }
    li.appendChild(body);

    const acts = document.createElement("div");
    acts.className = "acts";
    switch (d.state) {
      case "pending":
      case "running":
        acts.appendChild(button("Pause", "pause", d));
        acts.appendChild(button("Cancel", "cancel", d));
        break;
      case "paused":
        acts.appendChild(button("Resume", "resume", d));
        acts.appendChild(button("Cancel", "cancel", d));
        break;
      case "failed":
      case "cancelled":
        acts.appendChild(button("Retry", "retry", d));
        acts.appendChild(button("Remove", "remove", d));
        break;
      case "done":
        acts.appendChild(button("Show in folder", "show", d));
        acts.appendChild(button("Remove", "remove", d));
        break;
      default:
        break;
    }
    li.appendChild(acts);

    if (d.state === "confirm") {
      const w = document.createElement("div");
      w.className = "warn";
      const t = document.createElement("span");
      const b = document.createElement("b");
      b.textContent = "Can run code. ";
      t.appendChild(b);
      t.appendChild(document.createTextNode(d.warning || "This type of file can harm your computer. Keep it only if you trust where it came from."));
      w.appendChild(t);
      // Discard is the safe default and comes first; Keep is never the Enter default.
      const discard = button("Discard", "discard", d, "primary");
      const keep = button("Keep", "keep", d, "ghost");
      w.appendChild(discard);
      w.appendChild(keep);
      li.appendChild(w);
    }
    return li;
  }

  let folderText = "";

  function render(list) {
    const ul = $("list");
    ul.textContent = "";
    (list || []).forEach((d) => ul.appendChild(row(d)));
    $("empty").hidden = (list || []).length > 0;
    const active = (list || []).filter((d) => d.state === "running" || d.state === "pending").length;
    $("count").textContent = active ? active + " in progress" : (list || []).length ? String(list.length) : "";
    $("clear").disabled = !(list || []).some((d) => d.state === "done" || d.state === "failed" || d.state === "cancelled");
    if (I) I.paint(ul);
  }

  // The shell pushes changes here.
  window.mbDownloads = function (list) { render(list); };

  $("folder").addEventListener("click", () => api("downloads_folder").then((r) => { if (!r.ok) flash(r.error || "Could not open the folder."); }));
  $("clear").addEventListener("click", () => api("downloads_clear").then((r) => { if (r.downloads) render(r.downloads); }));

  api("downloads").then((r) => {
    if (!r.ok) { flash(r.error || "Could not load downloads."); return; }
    folderText = r.folder ? "Saved to " + r.folder : "";
    $("where").textContent = folderText;
    render(r.downloads || []);
  });

  if (I) I.paint(document);
})();
