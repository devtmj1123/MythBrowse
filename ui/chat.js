// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Myth conversation for the new-tab page: MythDesk chat bubbles, streamed replies, the
// agent's browser steps, approval cards, stop, and the model settings sheet.
//
// Shell API (internal origin, per-session token):
//   chat_send?conv=&text=&today=  -> {conv, id}      start a reply (worker thread); id = saved chat
//   chat_open?id=                 -> {conv, id}      load a saved chat to continue it
//   chat_poll?conv=&since=        -> {events, next, running, status, step, tabs}
//   chat_stop?conv=  chat_approve?conv=&request=&allow=1
//   ai_settings  ai_settings_save?kind=&base_url=&model=[&vision=auto|on|off&screenshots=0|1]
//                (API key in the x-mb-secret header)
//   ai_probe?kind=&base_url= -> {id};  ai_probe_result?id= -> {done, result}
// Events: history | user | start | text | tool | approval | notice | error | end. A reopened
// chat starts with `history` (its dates) and replays its messages as user/start/text/tool/end.
"use strict";

(function () {
  const I = window.JMIcons;
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;
  const SEARCH = "https://duckduckgo.com/?q=";

  function api(op, params, secret) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    const init = secret ? { headers: { "x-mb-secret": secret } } : undefined;
    return fetch("/api/" + op + "?" + q.toString(), init)
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = text;
    return e;
  }
  const icon = (name, size, role) => I.icon(name, { size: size || 16, role: role || "soft" });

  // ---- minimal Markdown (safe: DOM nodes only, never innerHTML) ---------------------------
  function inline(parent, text) {
    const re = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(\[[^\]\n]+\]\((https?:\/\/[^)\s]+)\))|(https?:\/\/[^\s)<>]+)/g;
    let last = 0;
    let m;
    while ((m = re.exec(text))) {
      if (m.index > last) parent.appendChild(document.createTextNode(text.slice(last, m.index)));
      if (m[1]) parent.appendChild(el("code", null, m[1].slice(1, -1)));
      else if (m[2]) parent.appendChild(el("strong", null, m[2].slice(2, -2)));
      else if (m[3]) {
        const a = el("a", "link", m[3].slice(1, m[3].indexOf("](")));
        a.href = m[4];
        parent.appendChild(a);
      } else if (m[5]) {
        const a = el("a", "link", m[5]);
        a.href = m[5];
        parent.appendChild(a);
      }
      last = re.lastIndex;
    }
    if (last < text.length) parent.appendChild(document.createTextNode(text.slice(last)));
  }

  function markdown(target, src) {
    target.textContent = "";
    const lines = src.replace(/\r/g, "").split("\n");
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (/^```/.test(line)) {
        const code = [];
        i++;
        while (i < lines.length && !/^```/.test(lines[i])) code.push(lines[i++]);
        i++;
        target.appendChild(el("pre", null, code.join("\n")));
        continue;
      }
      if (/^\s*([-*]|\d+[.)])\s+/.test(line)) {
        const ordered = /^\s*\d/.test(line);
        const list = el(ordered ? "ol" : "ul");
        while (i < lines.length && /^\s*([-*]|\d+[.)])\s+/.test(lines[i])) {
          const li = el("li");
          inline(li, lines[i].replace(/^\s*([-*]|\d+[.)])\s+/, ""));
          list.appendChild(li);
          i++;
        }
        target.appendChild(list);
        continue;
      }
      const h = /^(#{1,4})\s+(.*)$/.exec(line);
      if (h) {
        const p = el("p", "h");
        inline(p, h[2]);
        target.appendChild(p);
        i++;
        continue;
      }
      if (!line.trim()) { i++; continue; }
      const para = [];
      while (i < lines.length && lines[i].trim() && !/^```/.test(lines[i]) &&
             !/^\s*([-*]|\d+[.)])\s+/.test(lines[i]) && !/^#{1,4}\s/.test(lines[i])) {
        para.push(lines[i++]);
      }
      const p = el("p");
      inline(p, para.join("\n"));
      target.appendChild(p);
    }
  }

  // ---- conversation view --------------------------------------------------------------------
  function Chat(opts) {
    this.root = opts.root;          // scroll container
    this.onActivity = opts.onActivity || function () {};
    this.onRunning = opts.onRunning || function () {};
    this.onModel = opts.onModel || function () {};
    this.openSettings = opts.openSettings || function () {};
    this.onConv = opts.onConv || function () {};
    this.conv = null;
    this.id = null;     // saved chat id (null: new, or a private session)
    this.next = 0;
    this.running = false;
    this.inner = null;
    this.reply = null;   // current Myth bubble {row, body, text, steps}
    this.timer = null;
    this.approvals = {};
    this.lastPrompt = "";
  }

  Chat.prototype.ensureInner = function () {
    if (!this.inner) {
      this.inner = el("div", "inner");
      // A new tab is one conversation: it starts today.
      this.daySep = el("div", "day-sep", "Today");
      this.inner.appendChild(this.daySep);
      this.root.appendChild(this.inner);
    }
    this.onActivity();
    return this.inner;
  };

  Chat.prototype.scroll = function () {
    this.root.scrollTop = this.root.scrollHeight;
  };

  Chat.prototype.addUser = function (text) {
    const row = el("div", "msg-row you mv-msg");
    // Your messages are solid navy (no glass behind them), Myth's are glass.
    const b = el("div", "msg you");
    b.appendChild(el("p", null, text));
    row.appendChild(b);
    this.ensureInner().appendChild(row);
    this.lastPrompt = text;
    this.reply = null;
  };

  Chat.prototype.mythBubble = function () {
    if (this.reply) return this.reply;
    const row = el("div", "msg-row myth mv-msg");
    const b = el("div", "msg myth");
    b.setAttribute("data-glass", "p");
    // Who is talking: the Myth mark and name, small, at the top of the bubble.
    const who = el("div", "who");
    const mk = el("span", "mark");
    for (let k = 0; k < 3; k++) mk.appendChild(el("i"));
    who.appendChild(mk);
    who.appendChild(document.createTextNode("Myth"));
    b.appendChild(who);
    const steps = el("div", "steps");
    steps.hidden = true;
    const body = el("div", "body");
    const typing = el("span", "typing");
    for (let k = 0; k < 3; k++) typing.appendChild(el("i"));
    b.appendChild(steps);
    b.appendChild(body);
    b.appendChild(typing);
    row.appendChild(b);
    this.ensureInner().appendChild(row);
    this.reply = { row: row, bubble: b, body: body, steps: steps, typing: typing, text: "", tools: {} };
    return this.reply;
  };

  Chat.prototype.setTyping = function (on) {
    if (this.reply) this.reply.typing.hidden = !on;
  };

  Chat.prototype.appendText = function (t) {
    const r = this.mythBubble();
    r.text += t;
    markdown(r.body, r.text);
  };

  Chat.prototype.toolEvent = function (e) {
    const r = this.mythBubble();
    r.steps.hidden = false;
    let row = r.tools[e.id];
    if (!row) {
      row = el("div", "step mv-fade");
      row.appendChild(el("span", "step-ico"));
      row.appendChild(el("span", "step-text"));
      r.steps.appendChild(row);
      r.tools[e.id] = row;
    }
    const ico = row.firstChild;
    ico.textContent = "";
    const state = e.state || "running";
    row.className = "step mv-fade " + state;
    const name = state === "running" ? "globe" : state === "done" ? "check" : state === "denied" ? "shield" : "alert";
    const role = state === "error" ? "bad" : state === "denied" ? "warn" : state === "done" ? "soft" : "accent";
    ico.appendChild(icon(name, 14, role));
    row.lastChild.textContent = e.summary || e.name;
  };

  Chat.prototype.approvalEvent = function (e) {
    const self = this;
    if (e.state === "pending") {
      const card = el("div", "approval glass mv-pop");
      card.setAttribute("data-glass", "f");
      const head = el("div", "w-title");
      head.appendChild(icon("shield", 15, "soft"));
      head.appendChild(document.createTextNode("Needs your OK"));
      card.appendChild(head);
      card.appendChild(el("p", "approval-action", e.action));
      const why = "Myth asks first because " + e.reason + ".";
      card.appendChild(el("p", "approval-why", why));
      // A navigation shows its full destination (navguard: the URL may carry data).
      if (e.dest) card.appendChild(el("p", "approval-url approval-dest", "Opens " + e.dest));
      else if (e.url) card.appendChild(el("p", "approval-url", e.url));
      const btns = el("div", "approval-btns");
      const allow = el("button", "primary", "Allow");
      const deny = el("button", "ghost", "Don\u2019t allow");
      allow.type = deny.type = "button";
      allow.addEventListener("click", () => self.answer(e.id, true));
      deny.addEventListener("click", () => self.answer(e.id, false));
      btns.appendChild(allow);
      btns.appendChild(deny);
      card.appendChild(btns);
      const r = this.mythBubble();
      r.bubble.appendChild(card);
      this.approvals[e.id] = card;
      this.setTyping(false);
    } else {
      const card = this.approvals[e.id];
      if (card) {
        const done = el("div", "approval-done");
        done.appendChild(icon(e.state === "allowed" ? "check" : "close", 14, "soft"));
        done.appendChild(document.createTextNode(e.state === "allowed" ? "Allowed" : "Not allowed"));
        const btns = card.querySelector(".approval-btns");
        if (btns) card.replaceChild(done, btns);
        card.classList.add("answered");
      }
    }
  };

  // Human-verification hand-off: Myth is paused on a check the person completes themselves
  // (Myth never clicks or solves it). Show tab / Cancel task while waiting; then one line.
  const HANDOFF_DONE = {
    cleared: ["check", "Verified \u2014 Myth continued"],
    timeout: ["close", "Not completed in time \u2014 the task stopped"],
    cancelled: ["close", "Task cancelled"],
  };
  Chat.prototype.handoffEvent = function (e) {
    const self = this;
    const finish = (card, state) => {
      const d = HANDOFF_DONE[state] || HANDOFF_DONE.cancelled;
      const done = el("div", "approval-done");
      done.appendChild(icon(d[0], 14, "soft"));
      done.appendChild(document.createTextNode(d[1]));
      const btns = card.querySelector(".approval-btns");
      if (btns) card.replaceChild(done, btns);
      card.classList.add("answered");
    };
    if (this.handoffCard && !this.handoffCard.classList.contains("answered")) {
      if (e.state !== "waiting") finish(this.handoffCard, e.state);
      return;
    }
    if (!e.host || (e.state && e.state !== "waiting" && this.handoffCard)) return;
    const card = el("div", "approval handoff glass mv-pop");
    card.setAttribute("data-glass", "f");
    card.setAttribute("role", "alert");
    const head = el("div", "w-title");
    head.appendChild(icon("shield", 15, "soft"));
    head.appendChild(document.createTextNode("Verification needed on " + e.host));
    card.appendChild(head);
    card.appendChild(el("p", "approval-action", "Complete it yourself \u2014 Myth will continue."));
    card.appendChild(el("p", "approval-why", "The site wants a person to confirm they are human. Myth is paused and does not touch the check."));
    if (e.url) card.appendChild(el("p", "approval-url", e.url));
    const btns = el("div", "approval-btns");
    const show = el("button", "primary", "Show tab");
    const cancel = el("button", "ghost", "Cancel task");
    show.type = cancel.type = "button";
    show.addEventListener("click", () => api("handoff_show", { conv: self.conv }));
    cancel.addEventListener("click", () => {
      card.querySelectorAll("button").forEach((b) => (b.disabled = true));
      api("handoff_cancel", { conv: self.conv }).then(() => self.poll());
    });
    btns.appendChild(show);
    btns.appendChild(cancel);
    card.appendChild(btns);
    const r = this.mythBubble();
    r.bubble.appendChild(card);
    this.handoffCard = card;
    this.setTyping(false);
    // Replayed log: the first event already carries the final state.
    if (e.state && e.state !== "waiting") finish(card, e.state);
  };

  Chat.prototype.answer = function (id, allow) {
    const card = this.approvals[id];
    if (card) card.querySelectorAll("button").forEach((b) => (b.disabled = true));
    api("chat_approve", { conv: this.conv, request: id, allow: allow ? "1" : "0" }).then(() => this.poll());
  };

  Chat.prototype.notice = function (text) {
    const r = this.mythBubble();
    r.bubble.insertBefore(el("p", "notice", text), r.body);
  };

  Chat.prototype.errorEvent = function (e) {
    const r = this.mythBubble();
    r.bubble.classList.add("error");
    const box = el("div", "err");
    box.appendChild(el("p", null, e.text));
    if (e.hint) box.appendChild(el("p", "err-hint", e.hint));
    const acts = el("div", "acts");
    if (e.settings) {
      const b = el("button", "action");
      b.type = "button";
      b.appendChild(icon("settings", 15));
      b.appendChild(document.createTextNode("Model settings"));
      b.addEventListener("click", () => this.openSettings());
      acts.appendChild(b);
    }
    if (this.lastPrompt) {
      const a = el("a", "action");
      a.href = SEARCH + encodeURIComponent(this.lastPrompt);
      a.appendChild(icon("search", 15));
      a.appendChild(document.createTextNode("Search the web instead"));
      acts.appendChild(a);
    }
    box.appendChild(acts);
    r.bubble.appendChild(box);
  };

  const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August",
    "September", "October", "November", "December"];
  function dayLabel(ms) {
    const d = new Date(ms);
    const start = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const diff = Math.round((start(new Date()) - start(d)) / 86400000);
    if (diff === 0) return "Today";
    if (diff === 1) return "Yesterday";
    const y = d.getFullYear() === new Date().getFullYear() ? "" : " " + d.getFullYear();
    return d.getDate() + " " + MONTHS[d.getMonth()] + y;
  }

  Chat.prototype.handle = function (e) {
    switch (e.k) {
      case "history":
        // A reopened chat: its own start date instead of "Today".
        this.ensureInner();
        if (this.daySep && e.created) this.daySep.textContent = dayLabel(e.created);
        break;
      case "user": this.addUser(e.text); break;
      case "start":
        this.mythBubble();
        this.setTyping(!e.replay);
        if (e.model) this.onModel(e.model, e.local);
        break;
      case "text": this.appendText(e.text); break;
      case "tool": this.toolEvent(e); break;
      case "approval": this.approvalEvent(e); break;
      case "handoff": this.handoffEvent(e); break;
      case "notice": this.notice(e.text); break;
      case "error": this.errorEvent(e); break;
      case "end":
        if (this.reply) {
          this.setTyping(false);
          if (e.state === "stopped") this.reply.bubble.appendChild(el("p", "notice", "Stopped."));
          else if (e.state === "done" && !this.reply.text.trim() && !this.reply.bubble.classList.contains("error")) {
            this.reply.body.appendChild(el("p", "notice", "(no reply)"));
          }
        }
        this.reply = null;
        break;
    }
  };

  Chat.prototype.setRunning = function (on, step, tabs) {
    if (this.running !== on) {
      this.running = on;
      this.onRunning(on);
    }
    if (this.reply && on) {
      const waiting = Object.keys(this.approvals).some((k) => !this.approvals[k].classList.contains("answered")) ||
        !!(this.handoffCard && !this.handoffCard.classList.contains("answered"));
      this.setTyping(!waiting && !this.reply.text);
    }
    this.onBackground && this.onBackground(on ? tabs || [] : [], step);
  };

  Chat.prototype.poll = function () {
    if (this.conv === null) return Promise.resolve();
    clearTimeout(this.timer);
    return api("chat_poll", { conv: this.conv, since: this.next }).then((r) => {
      if (!r.ok) {
        this.setRunning(false);
        return;
      }
      const stick = this.root.scrollHeight - this.root.scrollTop - this.root.clientHeight < 80;
      r.events.forEach((e) => this.handle(e));
      this.next = r.next;
      if (stick || r.events.length) this.scroll();
      this.setRunning(r.running, r.step, (r.tabs || []).filter((t) => t.open));
      if (r.running) this.timer = setTimeout(() => this.poll(), r.status === "waiting" ? 500 : 120);
    });
  };

  Chat.prototype.send = function (text) {
    text = text.trim();
    if (!text || this.running) return;
    const params = { text: text, today: new Date().toDateString() };
    if (this.conv !== null) params.conv = this.conv;
    this.ensureInner();
    this.setRunning(true);
    return api("chat_send", params).then((r) => {
      if (!r.ok) {
        this.addUser(text);
        this.errorEvent({ text: r.error });
        this.reply = null;
        this.setRunning(false);
        return;
      }
      if (this.conv !== r.conv) {
        this.conv = r.conv;
        this.id = r.id || null;
        this.next = 0;
        this.remember();
        this.onConv(this);
      }
      return this.poll();
    });
  };

  // Reloading the tab (or reopening it after a restart) shows this conversation again.
  Chat.prototype.remember = function () {
    const q = this.id ? "home.html?id=" + this.id : "home.html?c=" + this.conv;
    try { history.replaceState(null, "", q); } catch (e) { /* ignore */ }
  };

  Chat.prototype.resume = function (conv, id) {
    this.conv = conv;
    if (id !== undefined) this.id = id || null;
    this.next = 0;
    return this.poll();
  };

  // Empty the view for a new chat (the conversation itself stays in the shell).
  Chat.prototype.reset = function () {
    clearTimeout(this.timer);
    this.root.textContent = "";
    this.inner = null;
    this.daySep = null;
    this.reply = null;
    this.approvals = {};
    this.conv = null;
    this.id = null;
    this.next = 0;
    this.lastPrompt = "";
    if (this.running) { this.running = false; this.onRunning(false); }
    this.onBackground && this.onBackground([], "");
  };

  // Continue a saved chat (`id` from chat_list) in this tab.
  Chat.prototype.open = function (id) {
    return api("chat_open", { id: id }).then((r) => {
      if (!r.ok) return r;
      this.reset();
      this.id = r.id || null;
      this.conv = r.conv;
      this.remember();
      this.onConv(this);
      return this.resume(r.conv).then(() => r);
    });
  };

  Chat.prototype.stop = function () {
    if (this.conv === null) return;
    api("chat_stop", { conv: this.conv }).then(() => this.poll());
  };

  // ---- model settings sheet -----------------------------------------------------------------
  // The approved sheet: API key | Ollama, Base URL, Check connection (result beside it), the
  // model as a radio list from the server (Ollama /api/tags with sizes, or /v1/models), a text
  // field when the server lists none, Cancel | Save. The hidden-or-shown #set-model field is
  // the one value Save reads; a radio click writes it.
  function fmtSize(bytes) {
    if (!(bytes > 0)) return "";
    return bytes >= 1e9 ? (bytes / 1e9).toFixed(1) + " GB" : Math.max(1, Math.round(bytes / 1e6)) + " MB";
  }

  function Settings(opts) {
    this.sheet = opts.sheet;
    this.onSaved = opts.onSaved || function () {};
    this.origin = opts.origin || null;
    this.kind = "ollama";
    this.state = null;
    this.lists = {};      // kind -> {url, models: [{name, size}]} from the last good probe
    this.probeSeq = 0;
    const $ = (id) => document.getElementById(id);
    this.f = {
      ollama: $("kind-ollama"), openai: $("kind-openai"), url: $("set-url"), key: $("set-key"),
      keyRow: $("key-row"), keyNote: $("key-note"), model: $("set-model"), modelInp: $("model-inp"),
      models: $("models"), test: $("set-test"), save: $("set-save"), cancel: $("set-cancel"),
      close: $("set-close"), msg: $("set-msg"), trVal: $("tr-val"), reduce: $("set-reduce"),
      shots: $("set-shots"), vision: $("set-vision"),
    };
    const f = this.f;
    this.appearance($("tr-slider"));
    f.ollama.addEventListener("click", () => this.setKind("ollama", true));
    f.openai.addEventListener("click", () => this.setKind("openai", true));
    f.test.addEventListener("click", () => this.test(false));
    f.save.addEventListener("click", () => this.save());
    f.cancel.addEventListener("click", () => this.close());
    f.close.addEventListener("click", () => this.close());
    this.sheet.addEventListener("keydown", (e) => { if (e.key === "Escape") this.close(); });
    // A click on the dimmed backdrop (not the sheet) closes without saving, as in the prototype.
    this.sheet.addEventListener("mousedown", (e) => { if (e.target === this.sheet) this.close(); });
    // Subtle focus: the field's hairline darkens (no :focus-within in Servo 0.1.4, so a class).
    this.sheet.querySelectorAll(".inp input").forEach((inp) => {
      inp.addEventListener("focus", () => inp.parentNode.classList.add("focused"));
      inp.addEventListener("blur", () => inp.parentNode.classList.remove("focused"));
    });
    f.url.addEventListener("change", () => { delete this.lists[this.kind]; });
    // Privacy: screenshots to the model on / off (saved with the model settings).
    if (f.shots) f.shots.addEventListener("click", () => {
      const on = f.shots.getAttribute("aria-checked") !== "true";
      f.shots.setAttribute("aria-checked", on ? "true" : "false");
      f.shots.classList.toggle("on", on);
      if (f.vision) f.vision.disabled = !on;
    });
  }

  Settings.prototype.setKind = function (kind, user) {
    this.kind = kind;
    const f = this.f;
    f.ollama.classList.toggle("on", kind === "ollama");
    f.openai.classList.toggle("on", kind === "openai");
    f.keyRow.hidden = kind !== "openai";
    if (user) {
      const s = this.state || {};
      if (kind === "ollama") f.url.value = s.kind === "ollama" ? s.base_url : "http://localhost:11434";
      else f.url.value = s.kind === "openai" ? s.base_url : "https://api.openai.com/v1";
      f.model.value = s.kind === kind ? s.model : "";
      this.message("");
    }
    f.url.placeholder = kind === "ollama" ? "http://localhost:11434" : "https://api.openai.com/v1";
    f.model.placeholder = kind === "ollama" ? "qwen2.5:7b" : "gpt-4o-mini";
    this.renderModels();
    if (user && !this.lists[kind]) this.test(true);
  };

  Settings.prototype.message = function (text, bad, ok) {
    const m = this.f.msg;
    m.textContent = "";
    m.className = "set-msg" + (bad ? " bad" : "") + (ok ? " ok" : "");
    if (!text) return;
    if (ok) m.appendChild(el("i", "dot"));
    m.appendChild(el("span", null, text));
  };

  // The radio list for the current kind (or the text field when there is none).
  Settings.prototype.renderModels = function () {
    const f = this.f;
    const list = this.lists[this.kind];
    const box = f.models;
    box.textContent = "";
    const models = list ? list.models.slice() : [];
    const cur = f.model.value.trim();
    if (models.length && cur && !models.some((m) => m.name === cur)) models.unshift({ name: cur, size: null });
    if (models.length && !cur) f.model.value = models[0].name;
    box.hidden = !models.length;
    f.modelInp.hidden = !!models.length;
    const local = this.kind === "ollama";
    models.forEach((m) => {
      const r = el("button", "mrow");
      r.type = "button";
      r.setAttribute("role", "radio");
      r.setAttribute("aria-checked", String(m.name === f.model.value));
      const radio = el("span", "radio");
      const dot = el("i");
      dot.appendChild(I.icon("check", { size: 11, role: "white", stroke: 3 }));
      radio.appendChild(dot);
      r.appendChild(radio);
      r.appendChild(el("span", "mn", m.name));
      const info = [fmtSize(m.size), local ? "local" : ""].filter(Boolean).join(" \u00b7 ");
      if (info) r.appendChild(el("span", "mi", info));
      r.addEventListener("click", () => {
        f.model.value = m.name;
        box.querySelectorAll(".mrow").forEach((x) => x.setAttribute("aria-checked", String(x === r)));
      });
      box.appendChild(r);
    });
  };

  // `kind` ("openai" / "ollama"): preselect it once the saved settings are loaded (the
  // first-run welcome's choice).
  Settings.prototype.open = function (trigger, kind) {
    if (trigger) this.origin = trigger;
    const panel = this.sheet.querySelector(".sheet");
    if (!this.sheet.hidden && !this.closing) return;
    this.closing = false;
    this.sheet.hidden = false;
    // Backdrop fades in (220 ms ease) while the sheet pops from its trigger (prototype).
    this.sheet.classList.remove("scrim-out");
    this.sheet.classList.add("scrim-in");
    if (window.JMMotion && panel) JMMotion.pop(panel, this.origin || document.getElementById("state-pill"), { from: 0.6 });
    document.body.classList.add("sheet-open");
    this.message("");
    if (panel) panel.focus();
    this.loadAppearance();
    api("ai_settings").then((s) => {
      if (!s.ok) return this.message(s.error, true);
      this.state = s;
      this.f.url.value = s.base_url;
      this.f.model.value = s.model;
      this.setKind(kind || s.kind, !!kind && kind !== s.kind);
      if (this.f.shots) {
        const on = s.screenshots !== false;
        this.f.shots.setAttribute("aria-checked", on ? "true" : "false");
        this.f.shots.classList.toggle("on", on);
        if (this.f.vision) { this.f.vision.value = s.vision || "auto"; this.f.vision.disabled = !on; }
      }
      this.f.key.value = "";
      this.f.key.placeholder = s.has_key ? "Saved \u2022\u2022\u2022\u2022 (type to replace)" : "sk-\u2026";
      this.f.keyNote.textContent = s.key_from_env
        ? "Using the key from an environment variable."
        : "Stored with " + s.key_store + ". Never written to the settings file or logs.";
      if (s.error) this.message("Your settings file has a problem: " + s.error, true);
      else if (!s.configured) this.message("Not set up yet. Myth uses Ollama on this computer until you choose.");
      // Fill the model list quietly (Ollama is local, so this is quick).
      if (!this.lists[s.kind]) this.test(true);
    });
  };

  // ---- Appearance: Transparency slider + Reduce transparency -------------------------------
  // Changes apply at once while dragging (this page now, the shell's blur and the chrome through
  // /api/glass_params, newest request wins); Save writes them with /api/prefs_save; closing
  // without saving puts the saved look back (glass_params?reset=1).
  Settings.prototype.appearance = function (box) {
    const f = this.f;
    if (!box || !f.reduce || !window.JMMotion) return;
    this.app = { saved: null, cur: null, reduceTouched: false, sending: false, queued: null };
    const label = (v) => (v === 50 ? "Default" : v + "%");
    this.trSlider = JMMotion.slider(box, {
      value: window.JMGlass ? JMGlass.look.v : 50, def: 50,
      onInput: (v) => { f.trVal.textContent = label(v); box.setAttribute("aria-valuetext", label(v)); this.preview({ v: v }); },
    });
    this.reduceSw = JMMotion.sw(f.reduce);
    f.reduce.addEventListener("change", () => {
      this.app.reduceTouched = true;
      this.preview({ reduce: f.reduce.getAttribute("aria-checked") === "true" });
    });
    this.labelFor = label;
  };

  Settings.prototype.showAppearance = function (look, jump) {
    if (!this.app) return;
    this.trSlider.set(look.v, jump);
    this.f.trVal.textContent = this.labelFor(look.v);
    this.reduceSw.set(look.reduce, jump);
    this.f.reduce.setAttribute("aria-checked", String(look.reduce));
    document.getElementById("tr-slider").classList.toggle("off", look.reduce);
  };

  Settings.prototype.loadAppearance = function () {
    if (!this.app) return;
    const a = this.app;
    a.reduceTouched = false;
    // What this page shows now, until the shell answers.
    const now = window.JMGlass ? JMGlass.look : { v: 50, reduce: false };
    a.saved = { v: now.v, reduce: now.reduce, reduceSetting: "system" };
    a.cur = { v: now.v, reduce: now.reduce };
    this.showAppearance(a.cur, true);
    api("prefs").then((p) => {
      if (!p || p.ok === false || typeof p.transparency !== "number") return;
      a.saved = { v: p.transparency, reduce: !!p.reduce_transparency_effective, reduceSetting: p.reduce_transparency || "system" };
      a.cur = { v: a.saved.v, reduce: a.saved.reduce };
      this.showAppearance(a.cur, true);
    });
  };

  Settings.prototype.preview = function (change) {
    const a = this.app;
    if (!a) return;
    a.cur = { v: change.v !== undefined ? change.v : a.cur.v, reduce: change.reduce !== undefined ? change.reduce : a.cur.reduce };
    document.getElementById("tr-slider").classList.toggle("off", a.cur.reduce);
    if (window.JMGlass) JMGlass.setLook(a.cur.v, a.cur.reduce);
    this.sendLook({ v: a.cur.v, reduce: a.cur.reduce ? 1 : 0 });
  };

  // One request in flight; while it runs only the newest change is kept.
  Settings.prototype.sendLook = function (params) {
    const a = this.app;
    if (a.sending) { a.queued = params; return; }
    a.sending = true;
    api("glass_params", params).then(() => {
      a.sending = false;
      if (a.queued) { const q = a.queued; a.queued = null; this.sendLook(q); }
    });
  };

  Settings.prototype.appearanceDirty = function () {
    const a = this.app;
    return !!(a && a.saved && a.cur && (a.cur.v !== a.saved.v || a.cur.reduce !== a.saved.reduce || a.reduceTouched));
  };

  // Save: write the appearance (when it changed), then it is the new saved look.
  Settings.prototype.saveAppearance = function () {
    const a = this.app;
    if (!this.appearanceDirty()) return Promise.resolve(true);
    const params = { transparency: a.cur.v };
    if (a.reduceTouched) params.reduce_transparency = a.cur.reduce ? "on" : "off";
    return api("prefs_save", params).then((r) => {
      if (r && r.ok === false) { this.message(r.error || "Could not save the appearance.", true); return false; }
      a.saved = { v: a.cur.v, reduce: a.cur.reduce, reduceSetting: params.reduce_transparency || a.saved.reduceSetting };
      a.reduceTouched = false;
      return true;
    });
  };

  // Closed without saving: back to the saved look.
  Settings.prototype.revertAppearance = function () {
    const a = this.app;
    if (!this.appearanceDirty()) return;
    a.cur = { v: a.saved.v, reduce: a.saved.reduce };
    a.reduceTouched = false;
    if (window.JMGlass) JMGlass.setLook(a.saved.v, a.saved.reduce);
    a.queued = null;
    this.sendLook({ reset: 1 });
  };

  Settings.prototype.close = function () {
    if (this.sheet.hidden || this.closing) return;
    this.revertAppearance();
    const panel = this.sheet.querySelector(".sheet");
    this.closing = true;
    const hide = () => {
      if (!this.closing) return;   // reopened meanwhile
      this.closing = false;
      this.sheet.hidden = true;
      this.sheet.classList.remove("scrim-out");
    };
    this.sheet.classList.remove("scrim-in");
    this.sheet.classList.add("scrim-out");
    if (window.JMMotion && panel) JMMotion.unpop(panel, { to: 0.9 }).then(() => { panel.hidden = false; hide(); });
    else hide();
    document.body.classList.remove("sheet-open");
    this.f.key.value = "";
  };

  function waitProbe(id, tries) {
    return new Promise((resolve) => {
      const step = (n) => api("ai_probe_result", { id: id }).then((r) => {
        if (r.ok && r.done) resolve(r.result);
        else if (n <= 0) resolve({ ok: false, error: "No answer from the server." });
        else setTimeout(() => step(n - 1), 250);
      });
      step(tries);
    });
  }

  // Check connection; `quiet` (the automatic fill on open) only reports success.
  Settings.prototype.test = function (quiet) {
    const f = this.f;
    const kind = this.kind;
    const seq = ++this.probeSeq;
    if (!quiet) this.message("Checking\u2026");
    f.test.disabled = true;
    api("ai_probe", { kind: kind, base_url: f.url.value.trim() }, f.key.value.trim() || null).then((r) => {
      if (!r.ok) { f.test.disabled = false; if (!quiet) this.message(r.error, true); return; }
      return waitProbe(r.id, 120).then((res) => {
        if (seq !== this.probeSeq) return;
        f.test.disabled = false;
        if (!res.ok) { if (!quiet) this.message(res.error + (res.hint ? " " + res.hint : ""), true); return; }
        const names = (res.models || []).slice(0, 300);
        const sizes = res.sizes || [];
        this.lists[kind] = { models: names.map((n, i) => ({ name: n, size: sizes[i] || null })) };
        if (kind === this.kind) this.renderModels();
        const n = names.length;
        const what = kind === "ollama" ? (n === 1 ? "1 model installed" : n + " models installed")
          : (n === 1 ? "1 model" : n + " models");
        this.message(n ? "Connected \u00b7 " + what : "Connected. The server lists no models: type the name.", false, true);
      });
    });
  };

  Settings.prototype.save = function () {
    const f = this.f;
    const key = f.key.value.trim();
    this.message("Saving\u2026");
    this.saveAppearance().then((ok) => {
      if (!ok) return;
      this.saveModel(f, key);
    });
  };

  Settings.prototype.saveModel = function (f, key) {
    const params = { kind: this.kind, base_url: f.url.value.trim(), model: f.model.value.trim() };
    if (f.shots) params.screenshots = f.shots.getAttribute("aria-checked") === "true" ? "1" : "0";
    if (f.vision) params.vision = f.vision.value;
    api("ai_settings_save", params, key || null)
      .then((r) => {
        f.key.value = "";
        if (!r.ok) return this.message(r.error, true);
        this.state = r;
        this.close();
        this.onSaved(r);
      });
  };

  window.MythChat = { Chat: Chat, Settings: Settings, api: api, markdown: markdown };
})();
