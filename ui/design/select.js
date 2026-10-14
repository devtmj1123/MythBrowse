// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Glass dropdown (v0.4): replaces every native <select> on the UI pages. On Windows Servo drew a
// grey system control on the dark glass; this draws the button and its list in the design.
//
//   <script src="design/select.js"></script>   after icons.js (and motion.js); every <select>
//   on the page, now and added later, is upgraded. JMSelect.enhance(sel) does one by hand.
//
// The <select> stays in the DOM (display: none, class .dd-native) and remains the source of truth: page
// code keeps reading / writing select.value and listening for "change". Setting .value /
// .selectedIndex from script, or changing its <option>s, updates the button's label.
//
// Keyboard (WAI-ARIA select-only combobox): on the button Enter / Space / Alt+Down / Up / Down
// open the list; in the list Up / Down / Home / End / PageUp / PageDown move, typing jumps to
// the option that starts with it, Enter / Space choose, Esc / Tab close (Esc never reaches the
// sheet behind). The list is near-opaque (--menu-bg) and only fades + lifts (opacity /
// transform) when it opens.
"use strict";

(function () {
  const root = document.documentElement;
  const motion = () => root.getAttribute("data-motion") || "full";
  let open = null;   // the dropdown whose list is showing
  let seq = 0;

  function iconSpan(name, size) {
    const s = document.createElement("span");
    s.setAttribute("data-icon", name);
    s.setAttribute("data-size", String(size));
    s.setAttribute("data-role", "soft");
    if (window.JMIcons) s.appendChild(JMIcons.icon(name, { size: size, role: "soft" }));
    return s;
  }

  function enhance(sel) {
    if (!sel || sel._dd || sel.multiple || sel.closest(".dd-skip")) return sel && sel._dd;
    const id = "dd" + ++seq;
    const wrap = document.createElement("span");
    wrap.className = "dd";
    sel.parentNode.insertBefore(wrap, sel);
    wrap.appendChild(sel);
    sel.classList.add("dd-native");
    sel.tabIndex = -1;
    sel.setAttribute("aria-hidden", "true");

    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "dd-btn";
    btn.id = sel.id ? sel.id + "-dd" : id + "-btn";
    btn.setAttribute("role", "combobox");
    btn.setAttribute("aria-haspopup", "listbox");
    btn.setAttribute("aria-expanded", "false");
    btn.setAttribute("aria-controls", id + "-list");
    const label = sel.getAttribute("aria-label");
    if (label) btn.setAttribute("aria-label", label);
    if (sel.title) btn.title = sel.title;
    const text = document.createElement("span");
    text.className = "dd-val";
    btn.appendChild(text);
    const chev = iconSpan("chevron", 14);
    chev.className = "dd-chev";
    btn.appendChild(chev);
    wrap.appendChild(btn);

    const list = document.createElement("ul");
    list.className = "dd-list";
    list.id = id + "-list";
    list.setAttribute("role", "listbox");
    if (label) list.setAttribute("aria-label", label);
    list.hidden = true;
    let active = -1, typed = "", typedT = 0, closing = 0;

    const opts = () => Array.prototype.slice.call(sel.options);
    function sync() {
      const o = sel.options[sel.selectedIndex];
      text.textContent = o ? o.textContent : "";
      btn.disabled = !!sel.disabled;
      if (!list.hidden) build();
    }
    function build() {
      list.textContent = "";
      opts().forEach((o, i) => {
        const li = document.createElement("li");
        li.className = "dd-opt";
        li.id = id + "-o" + i;
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", String(i === sel.selectedIndex));
        if (o.disabled) li.setAttribute("aria-disabled", "true");
        const tick = iconSpan("check", 14);
        tick.className = "dd-tick";
        li.appendChild(tick);
        const t = document.createElement("span");
        t.textContent = o.textContent;
        li.appendChild(t);
        li.addEventListener("mousemove", () => highlight(i, false));
        li.addEventListener("mousedown", (e) => e.preventDefault());   // keep focus on the button
        li.addEventListener("click", (e) => { e.stopPropagation(); choose(i); });
        list.appendChild(li);
      });
      highlight(active, false);
    }
    function highlight(i, scroll) {
      const items = list.children;
      if (!items.length) return;
      i = Math.max(0, Math.min(items.length - 1, i));
      active = i;
      for (let k = 0; k < items.length; k++) items[k].classList.toggle("on", k === i);
      btn.setAttribute("aria-activedescendant", items[i].id);
      if (scroll) {
        const it = items[i];
        if (it.offsetTop < list.scrollTop) list.scrollTop = it.offsetTop - 5;
        else if (it.offsetTop + it.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = it.offsetTop + it.offsetHeight - list.clientHeight + 5;
      }
    }
    function step(from, dir) {
      const o = opts();
      for (let i = from + dir; i >= 0 && i < o.length; i += dir) if (!o[i].disabled) return i;
      return from;
    }
    function place() {
      const r = btn.getBoundingClientRect();
      list.style.minWidth = Math.round(r.width) + "px";
      list.style.left = "0px";
      list.style.top = "0px";
      // offsetHeight, not scrollHeight: the list is capped at 280px (max-height) anyway, and a
      // scroll-area query on a list just moved to <body> is what Servo 0.1.4 panicked on
      // (docs/REGRESSIONS.md).
      const lw = list.offsetWidth, lh = Math.min(list.offsetHeight, 280);
      const below = window.innerHeight - r.bottom - 8, above = r.top - 8;
      const up = below < lh && above > below;
      list.classList.toggle("up", up);
      let left = r.right - lw;
      if (left < 8) left = Math.min(r.left, window.innerWidth - lw - 8);
      list.style.left = Math.round(Math.max(8, left)) + "px";
      list.style.top = Math.round(up ? r.top - 6 - lh : r.bottom + 6) + "px";
    }
    function show() {
      if (btn.disabled || !list.hidden) return;
      if (open && open !== api) open.close();
      clearTimeout(closing);
      list.classList.remove("dd-out", "dd-in");
      document.body.appendChild(list);   // out of any clipping / transformed ancestor
      list.hidden = false;
      active = sel.selectedIndex;
      build();
      place();
      // Scroll the chosen option into view on the next frame, after the list has been laid out
      // once (no scroll-area query on the just-inserted list in the same task).
      const first = active < 0 ? step(-1, 1) : active;
      highlight(first, false);
      requestAnimationFrame(() => { if (!list.hidden) highlight(active, true); });
      wrap.classList.add("open");
      btn.setAttribute("aria-expanded", "true");
      if (motion() !== "off") { void list.offsetWidth; list.classList.add("dd-in"); }
      open = api;
    }
    function close(focus) {
      if (list.hidden) return;
      wrap.classList.remove("open");
      btn.setAttribute("aria-expanded", "false");
      btn.removeAttribute("aria-activedescendant");
      if (open === api) open = null;
      if (focus) btn.focus();
      list.classList.remove("dd-in");
      if (motion() === "off") { list.hidden = true; return; }
      list.classList.add("dd-out");
      closing = setTimeout(() => { list.hidden = true; list.classList.remove("dd-out"); }, 110);
    }
    function choose(i) {
      const o = sel.options[i];
      if (!o || o.disabled) return;
      const changed = sel.selectedIndex !== i;
      sel.selectedIndex = i;
      sync();
      close(true);
      if (changed) {
        sel.dispatchEvent(new Event("input", { bubbles: true }));
        sel.dispatchEvent(new Event("change", { bubbles: true }));
      }
    }
    function typeahead(ch) {
      clearTimeout(typedT);
      typed += ch.toLowerCase();
      typedT = setTimeout(() => { typed = ""; }, 700);
      const o = opts();
      const start = list.hidden ? sel.selectedIndex : active;
      for (let k = 1; k <= o.length; k++) {
        const i = (start + k - (typed.length > 1 ? 1 : 0) + o.length) % o.length;
        if (!o[i].disabled && o[i].textContent.trim().toLowerCase().startsWith(typed)) {
          if (list.hidden) choose(i); else highlight(i, true);
          return;
        }
      }
    }

    btn.addEventListener("click", () => (list.hidden ? show() : close(false)));
    btn.addEventListener("keydown", (e) => {
      const k = e.key;
      if (list.hidden) {
        if (k === "Enter" || k === " " || k === "ArrowDown" || k === "ArrowUp" || (k === "ArrowDown" && e.altKey)) {
          e.preventDefault(); show();
        } else if (k.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) typeahead(k);
        return;
      }
      const n = sel.options.length;
      if (k === "Escape") { e.preventDefault(); e.stopPropagation(); close(true); }
      else if (k === "Tab") close(false);
      else if (k === "ArrowDown") { e.preventDefault(); highlight(step(active, 1), true); }
      else if (k === "ArrowUp") { e.preventDefault(); if (e.altKey) choose(active); else highlight(step(active, -1), true); }
      else if (k === "Home") { e.preventDefault(); highlight(step(-1, 1), true); }
      else if (k === "End") { e.preventDefault(); highlight(step(n, -1), true); }
      else if (k === "PageDown") { e.preventDefault(); highlight(Math.min(n - 1, active + 8), true); }
      else if (k === "PageUp") { e.preventDefault(); highlight(Math.max(0, active - 8), true); }
      else if (k === "Enter" || k === " ") { e.preventDefault(); choose(active); }
      else if (k.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) typeahead(k);
    }, true);
    // Esc on the open list must not also close the dialog / sheet behind it.
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && !list.hidden) { e.stopPropagation(); e.preventDefault(); close(true); }
    }, true);
    btn.addEventListener("blur", () => setTimeout(() => { if (document.activeElement !== btn) close(false); }, 0));
    document.addEventListener("mousedown", (e) => {
      if (!list.hidden && !wrap.contains(e.target) && !list.contains(e.target)) close(false);
    }, true);
    window.addEventListener("resize", () => close(false));
    document.addEventListener("scroll", (e) => { if (!list.hidden && !list.contains(e.target)) close(false); }, true);
    // A <label for=sel> focuses the button.
    if (sel.id) document.querySelectorAll('label[for="' + sel.id + '"]').forEach((l) => l.setAttribute("for", btn.id));

    // Script writes to .value / .selectedIndex keep the label right.
    ["value", "selectedIndex"].forEach((prop) => {
      const d = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, prop);
      if (!d || !d.set) return;
      try {
        Object.defineProperty(sel, prop, {
          configurable: true,
          get() { return d.get.call(this); },
          set(v) { d.set.call(this, v); sync(); },
        });
      } catch (err) { /* engine without own accessors: the observers below still catch most */ }
    });
    new MutationObserver(sync).observe(sel, { childList: true, subtree: true, attributes: true, characterData: true });
    sel.addEventListener("change", sync);

    const api = { sel, button: btn, list, sync, open: show, close };
    sel._dd = api;
    sync();
    return api;
  }

  function enhanceAll(scope) {
    (scope || document).querySelectorAll("select").forEach(enhance);
  }
  function start() {
    enhanceAll();
    new MutationObserver((recs) => {
      for (const r of recs) {
        r.addedNodes.forEach((n) => {
          if (n.nodeType !== 1) return;
          if (n.tagName === "SELECT") enhance(n);
          else if (n.querySelector && n.querySelector("select")) enhanceAll(n);
        });
      }
    }).observe(document.body, { childList: true, subtree: true });
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();

  window.JMSelect = { enhance, enhanceAll };
})();
