// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Saved Myth conversations as a grouped list (Pinned / Today / This week / Earlier), shared by
// the new-tab sidebar and the History page's Chats tab.
//
// Shell API (internal origin, per-session token):
//   chat_list?q=&tzo=         -> {private, groups: [{label, items: [{id, title, updated,
//                                 pinned, saved, conv, running}]}]}
//   chat_rename?id=&title=  chat_pin?id=&on=1|0  chat_delete?id=
// Titles are cut to fit with JMMotion.ellipsis (Servo 0.1.4 has no text-overflow). Delete is
// two-step (the button turns red, a second click deletes), so no modal dialog is needed.
"use strict";

(function () {
  const I = window.JMIcons;
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;

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

  const pad = (n) => (n < 10 ? "0" : "") + n;
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  function when(ms, label) {
    const d = new Date(ms);
    if (label === "Today" || label === "Pinned" && Date.now() - ms < 86400000) return pad(d.getHours()) + ":" + pad(d.getMinutes());
    if (label === "This week") return DAYS[d.getDay()];
    return d.getDate() + " " + MONTHS[d.getMonth()];
  }

  // opts: root (element), onOpen(item), active() -> id of the open chat, compact (sidebar),
  // onLoaded(result), empty (text when there are no chats).
  function List(opts) {
    this.root = opts.root;
    this.onOpen = opts.onOpen || function () {};
    this.active = opts.active || function () { return null; };
    this.compact = !!opts.compact;
    this.onLoaded = opts.onLoaded || function () {};
    this.emptyText = opts.empty || "No chats yet. Conversations with Myth appear here.";
    this.query = "";
    this.seq = 0;
    this.armed = null;
  }

  List.prototype.load = function (query) {
    if (query !== undefined) this.query = query;
    const n = ++this.seq;
    const params = { tzo: String(new Date().getTimezoneOffset()) };
    if (this.query.trim()) params.q = this.query.trim();
    return api("chat_list", params).then((r) => {
      if (n !== this.seq) return r;  // a newer search is on its way
      this.render(r);
      this.onLoaded(r);
      return r;
    });
  };

  List.prototype.render = function (r) {
    const root = this.root;
    root.textContent = "";
    if (!r.ok) {
      root.appendChild(el("p", "cl-empty", "Could not load chats: " + (r.error || "unknown error")));
      return;
    }
    if (!r.groups.length) {
      root.appendChild(el("p", "cl-empty", this.query.trim() ? "No chats match." : this.emptyText));
      return;
    }
    const fits = [];
    for (const g of r.groups) {
      const group = el("section", "cl-group");
      group.appendChild(el("div", "cl-label", g.label));
      const ul = el("ul", "cl-items");
      for (const it of g.items) {
        const li = this.item(it, g.label);
        ul.appendChild(li);
        fits.push(li.querySelector(".cl-title"));
      }
      group.appendChild(ul);
      root.appendChild(group);
    }
    // Cut titles once they are laid out.
    requestAnimationFrame(() => fits.forEach((t) => window.JMMotion && JMMotion.ellipsis(t, t.getAttribute("data-full"))));
  };

  List.prototype.item = function (it, label) {
    const self = this;
    const li = el("li", "cl-item");
    if (it.id === this.active()) li.classList.add("on");
    if (it.pinned) li.classList.add("pinned");
    const open = el("button", "cl-open");
    open.type = "button";
    open.title = it.title;
    if (it.running) open.appendChild(el("span", "cl-live"));
    const t = el("span", "cl-title", it.title || "New chat");
    t.setAttribute("data-full", it.title || "New chat");
    open.appendChild(t);
    open.appendChild(el("span", "cl-meta", when(it.updated, label)));
    open.addEventListener("click", () => self.onOpen(it));
    open.addEventListener("dblclick", (e) => { e.preventDefault(); self.rename(li, it); });
    li.appendChild(open);

    const acts = el("span", "cl-acts");
    if (it.saved) {
      const pin = el("button", "cl-act" + (it.pinned ? " on" : ""));
      pin.type = "button";
      pin.title = it.pinned ? "Unpin" : "Pin";
      pin.setAttribute("aria-label", pin.title);
      pin.appendChild(I.icon("pin", { size: 14, role: it.pinned ? "icon" : "soft" }));
      pin.addEventListener("click", (e) => {
        e.stopPropagation();
        api("chat_pin", { id: it.id, on: it.pinned ? "0" : "1" }).then(() => self.load());
      });
      acts.appendChild(pin);
    }
    if (!this.compact) {
      const ren = el("button", "cl-act");
      ren.type = "button";
      ren.title = "Rename";
      ren.setAttribute("aria-label", "Rename");
      ren.appendChild(I.icon("wand", { size: 14, role: "soft" }));
      ren.addEventListener("click", (e) => { e.stopPropagation(); self.rename(li, it); });
      acts.appendChild(ren);
    }
    const del = el("button", "cl-act del");
    del.type = "button";
    del.title = "Delete";
    del.setAttribute("aria-label", "Delete chat");
    del.appendChild(I.icon("trash", { size: 14, role: "soft" }));
    del.addEventListener("click", (e) => {
      e.stopPropagation();
      if (self.armed !== del) {
        if (self.armed) self.disarm();
        self.armed = del;
        del.classList.add("armed");
        del.title = "Click again to delete";
        del.textContent = "";
        del.appendChild(I.icon("trash", { size: 14, role: "white" }));
        clearTimeout(self.disarmTimer);
        self.disarmTimer = setTimeout(() => self.disarm(), 3500);
        return;
      }
      self.disarm();
      li.classList.add("cl-gone");
      api("chat_delete", { id: it.id }).then((r) => {
        if (self.onDeleted) self.onDeleted(it, r);
        self.load();
      });
    });
    acts.appendChild(del);
    li.appendChild(acts);
    return li;
  };

  List.prototype.disarm = function () {
    const b = this.armed;
    this.armed = null;
    if (!b) return;
    b.classList.remove("armed");
    b.title = "Delete";
    b.textContent = "";
    b.appendChild(I.icon("trash", { size: 14, role: "soft" }));
  };

  // Inline rename: the title becomes a field; Enter saves, Esc or leaving cancels.
  List.prototype.rename = function (li, it) {
    const open = li.querySelector(".cl-open");
    if (!open || li.querySelector(".cl-rename")) return;
    const field = el("input", "cl-rename field");
    field.type = "text";
    field.value = it.title || "";
    field.maxLength = 120;
    field.setAttribute("aria-label", "Chat name");
    open.hidden = true;
    li.insertBefore(field, open);
    field.focus();
    field.select();
    let done = false;
    const finish = (save) => {
      if (done) return;
      done = true;
      const title = field.value.trim();
      if (save && title && title !== it.title) {
        api("chat_rename", { id: it.id, title: title }).then(() => this.load());
      } else {
        field.remove();
        open.hidden = false;
      }
    };
    field.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); finish(true); }
      else if (e.key === "Escape") { e.preventDefault(); finish(false); }
    });
    field.addEventListener("blur", () => finish(true));
  };

  window.MythChatList = { List: List, api: api };
})();
