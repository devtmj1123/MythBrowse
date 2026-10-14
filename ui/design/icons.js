// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// The MythDesk icon set (MythDesk icons.js): one stroke style everywhere,
// 24 px grid, 1.8 stroke, round caps and joins. The MythDesk paths are copied unchanged; the
// browser-only icons below them (back, forward, reload, star, lock, ...) are drawn in the same
// style. Original JustMyth drawings, no third-party icon set.
//
// Servo renders inline <svg> as an image (the subtree is serialised to an SVG document), so an
// icon cannot take `currentColor`; the colour is written into the SVG. Use roles ("icon",
// "soft", "dim", "accent", "inkText", ...) from JM.colors(); JMIcons.paint() re-colours icons
// when the tone changes.
//
//   <span data-icon="search" data-size="18" data-role="soft"></span>   -> JMIcons.paint(root)
//   el.appendChild(JMIcons.icon("plus", { size: 16, role: "icon" }))
"use strict";

(function () {
  const P = {
    // ---- MythDesk (unchanged) ----
    home: ["M3.5 10.2 12 3.5l8.5 6.7", "M5.5 8.8V20h4.8v-5.6h3.4V20h4.8V8.8"],
    chat: ["M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H10l-4.5 4v-4h0A2.5 2.5 0 0 1 4 13.5z", "M8 8.5h8", "M8 12h5"],
    tasks: ["M9 6h11", "M9 12h11", "M9 18h11", "M3.5 6l1.5 1.5L7.5 5", "M3.5 12l1.5 1.5 2.5-2.5", "M3.5 18l1.5 1.5 2.5-2.5"],
    settings: ["M12 15.2a3.2 3.2 0 1 0 0-6.4 3.2 3.2 0 0 0 0 6.4z",
      "M19.4 13.5a7.6 7.6 0 0 0 0-3l2-1.5-2-3.4-2.3.9a7.6 7.6 0 0 0-2.6-1.5L14.1 2.5h-4.2l-.4 2.5a7.6 7.6 0 0 0-2.6 1.5l-2.3-.9-2 3.4 2 1.5a7.6 7.6 0 0 0 0 3l-2 1.5 2 3.4 2.3-.9a7.6 7.6 0 0 0 2.6 1.5l.4 2.5h4.2l.4-2.5a7.6 7.6 0 0 0 2.6-1.5l2.3.9 2-3.4z"],
    send: ["M12 19V5", "M5.5 11.5 12 5l6.5 6.5"],
    stop: ["M7 7h10v10H7z"],
    alert: ["M12 3.5 2.8 19.5h18.4z", "M12 10v4", "M12 16.8v.2"],
    folder: ["M3.5 7.5A2 2 0 0 1 5.5 5.5h4l2 2h7a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"],
    wand: ["M4 20 15 9", "M14 4v3", "M19 9h-3", "M17.5 5.5l-2 2", "M12.5 6.5l5 5"],
    min: ["M6 12h12"],
    max: ["M6.5 6.5h11v11h-11z"],
    restore: ["M8.5 8.5h9v9h-9z", "M6.5 15.5v-9h9"],
    close: ["M6.5 6.5l11 11", "M17.5 6.5l-11 11"],
    chevron: ["M6.5 9.5 12 15l5.5-5.5"],
    clock: ["M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z", "M12 7.5V12l3 2"],
    // ---- browser icons, same grid and stroke ----
    back: ["M19 12H5.5", "M11 5.5 4.5 12l6.5 6.5"],
    forward: ["M5 12h13.5", "M13 5.5l6.5 6.5-6.5 6.5"],
    reload: ["M19.5 12a7.5 7.5 0 1 1-2.2-5.3", "M19.5 4.5v4.2h-4.2"],
    plus: ["M12 5v14", "M5 12h14"],
    star: ["M12 3.8l2.5 5.1 5.6.8-4.05 3.95.95 5.6L12 16.6l-5 2.65.95-5.6L3.9 9.7l5.6-.8z"],
    history: ["M4 12a8 8 0 1 0 2.4-5.7", "M4 4.5V8.3h3.8", "M12 8v4.2l2.8 1.8"],
    search: ["M10.5 17.5a7 7 0 1 0 0-14 7 7 0 0 0 0 14z", "M15.6 15.6 20.5 20.5"],
    globe: ["M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17z", "M3.5 12h17",
      "M12 3.5c2.3 2.4 3.4 5.2 3.4 8.5s-1.1 6.1-3.4 8.5c-2.3-2.4-3.4-5.2-3.4-8.5s1.1-6.1 3.4-8.5z"],
    lock: ["M7 10.5h10a1.5 1.5 0 0 1 1.5 1.5v6.5A1.5 1.5 0 0 1 17 20H7a1.5 1.5 0 0 1-1.5-1.5V12A1.5 1.5 0 0 1 7 10.5z",
      "M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"],
    pin: ["M9 3.5h6", "M10 3.5v5.8L6.8 13h10.4L14 9.3V3.5", "M12 13v7.5"],
    sidebar: ["M4.5 4.5h15a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1v-13a1 1 0 0 1 1-1z", "M9.5 4.5v15"],
    trash: ["M4.5 7h15", "M9.5 7V4.8h5V7", "M6.6 7l.9 12.5h9l.9-12.5"],
    moon: ["M19.5 14.6A7.8 7.8 0 0 1 9.4 4.5a7.8 7.8 0 1 0 10.1 10.1z"],
    // ---- AI / Tasks (week 2), same grid and stroke ----
    check: ["M5 12.5l4.2 4.2L19 7"],
    eye: ["M2.8 12S6.2 5.5 12 5.5 21.2 12 21.2 12 17.8 18.5 12 18.5 2.8 12 2.8 12z", "M12 14.8a2.8 2.8 0 1 0 0-5.6 2.8 2.8 0 0 0 0 5.6z"],
    takeover: ["M13.5 4.5h6v6", "M19.5 4.5 11 13", "M17.5 13.5v4.5a1.5 1.5 0 0 1-1.5 1.5H6a1.5 1.5 0 0 1-1.5-1.5V8A1.5 1.5 0 0 1 6 6.5h4.5"],
    key: ["M8.5 15.5a4 4 0 1 0 0-8 4 4 0 0 0 0 8z", "M12 11.5h8.5", "M17.5 11.5v3", "M20.5 11.5v2.5"],
    shield: ["M12 3.5 5 6v5.5c0 4.3 3 7.9 7 9 4-1.1 7-4.7 7-9V6z"],
    // ---- site compat (week 3), same grid and stroke ----
    // A puzzle piece: the page with a patch fitted in.
    compat: ["M3.8 7.7h3.6a2.4 2.4 0 1 1 4.8 0h3.6v3.6a2.4 2.4 0 1 1 0 4.8v2.6h-12z"],
    reader: ["M5.5 5.5h13", "M5.5 9.5h13", "M5.5 13.5h13", "M5.5 17.5h8"],
    undo: ["M8.5 5.5 4 10l4.5 4.5", "M4 10h10.5a5 5 0 0 1 0 10H11"],
    // ---- files (downloads, session), same grid and stroke ----
    download: ["M12 4v11", "M7 10.5l5 5 5-5", "M5 19.5h14"],
    pause: ["M8.5 6v12", "M15.5 6v12"],
    file: ["M6.5 3.5h7l4 4v13h-11z", "M13.5 3.5v4h4"],
    // ---- chrome popups (suggestions, permission sheet, pop-up indicator) ----
    tabs: ["M3.5 8.5h17v10a1.5 1.5 0 0 1-1.5 1.5H5a1.5 1.5 0 0 1-1.5-1.5z", "M3.5 8.5V6A1.5 1.5 0 0 1 5 4.5h5.5l1.5 4"],
    popup: ["M4 6.5A1.5 1.5 0 0 1 5.5 5h13A1.5 1.5 0 0 1 20 6.5v11a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 17.5z", "M4 9h16", "M9.5 12.5l5 5", "M14.5 12.5l-5 5"],
    bell: ["M6.5 16.5V11a5.5 5.5 0 0 1 11 0v5.5l1.5 1.5H5z", "M10 20.5h4"],
    camera: ["M3.5 8A1.5 1.5 0 0 1 5 6.5h10A1.5 1.5 0 0 1 16.5 8v8a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 16z", "M16.5 10.5l4-2.5v8l-4-2.5"],
    mic: ["M9 6.5a3 3 0 0 1 6 0v5a3 3 0 0 1-6 0z", "M6 11.5a6 6 0 0 0 12 0", "M12 17.5V21"],
  };
  const NS = "http://www.w3.org/2000/svg";

  function colorFor(opts) {
    if (opts.color) return opts.color;
    const c = window.JM ? window.JM.colors() : null;
    return (c && c[opts.role || "icon"]) || "#16203a";
  }
  // Secondary icons are the text colour at the design's --soft / --dim alpha (as
  // `currentColor` would be in the prototype), not an opaque grey: on the coloured sky (dusk
  // especially) an opaque grey loses most of its contrast. [colour, opacity].
  const ALPHA = { soft: 0.7, dim: 0.46 };
  function paintFor(opts) {
    const role = opts.role || "icon";
    if (opts.color || !ALPHA[role]) return [colorFor(opts), 1];
    const dark = document.documentElement.getAttribute("data-tone") === "dark";
    return [dark ? "#e8ecff" : "#16203a", dark ? ALPHA[role] : role === "soft" ? 0.68 : 0.46];
  }

  /// icon(name, {size=20, role="icon" | color, fill=false, stroke=1.8}) -> <svg class="ico">
  function icon(name, opts) {
    opts = opts || {};
    const size = String(opts.size || 20);
    const [color, alpha] = paintFor(opts);
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("width", size);
    svg.setAttribute("height", size);
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", opts.fill ? color : "none");
    svg.setAttribute("stroke", color);
    svg.setAttribute("stroke-width", String(opts.stroke || 1.8));
    if (alpha < 1) {
      svg.setAttribute("stroke-opacity", String(alpha));
      if (opts.fill) svg.setAttribute("fill-opacity", String(alpha));
    }
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("class", "ico");
    for (const d of P[name] || P.spark) {
      const p = document.createElementNS(NS, "path");
      p.setAttribute("d", d);
      svg.appendChild(p);
    }
    return svg;
  }

  /// Fill every [data-icon] element under root (re-run after a tone change).
  function paint(root) {
    (root || document).querySelectorAll("[data-icon]").forEach((el) => {
      el.textContent = "";
      el.appendChild(icon(el.getAttribute("data-icon"), {
        size: Number(el.getAttribute("data-size")) || 20,
        role: el.getAttribute("data-role") || "icon",
        color: el.getAttribute("data-color") || undefined,
        fill: el.hasAttribute("data-fill"),
        stroke: Number(el.getAttribute("data-stroke")) || undefined,
      }));
    });
  }

  window.JMIcons = { icon: icon, paint: paint, names: Object.keys(P) };
  if (window.JM) window.JM.onChange(() => paint(document));
})();
