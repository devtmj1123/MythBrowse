// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Motion and liquid glass for every UI page (the approved prototype, ported
// to Servo 0.1.4). Load after theme.js:
//
//   <meta name="mb-glass" content="{{MB_GLASS}}">     blur / flat / off, filled by the shell
//   <meta name="mb-transparency" content="{{MB_TRANSPARENCY}}">                 0-100 (Appearance)
//   <meta name="mb-reduce-transparency" content="{{MB_REDUCE_TRANSPARENCY}}">   1 = solid surfaces
//   <script src="design/motion.js"></script>
//
// What is here, and the Servo 0.1.4 limits it works around:
//  - Springs as CSS `linear()` curves (Servo parses linear(), no pref): --spr-* / --d-* on
//    <html>, used by tokens.css for press, pop, rise and every transition.
//  - A time-based JS spring (rAF, integrated on real elapsed time in 1 ms sub-steps). set()
//    retargets from the current position *and velocity*, so rapid taps bend the motion and
//    never restart it; hold() follows a drag 1:1; release() keeps the drag's velocity.
//  - Segmented controls (.seg) and switches (.sw): only transform and opacity change while
//    they move. Mouse events only: Servo 0.1.4 has no pointer capture, so a drag listens on
//    the document until mouseup.
//  - pop(el, origin) / unpop(el): panels open from where they were opened (class + keyframes,
//    since Servo has no Web Animations); rise(el) for new messages.
//  - ellipsis(el, text): Servo has no text-overflow, so text is cut to fit with "…".
//  - The glass reporter: every [data-glass] element (and every .glass, as a panel) (c chrome / p panel / f floating) is
//    measured with its effective opacity and clip, and sent to /api/glass only when the list
//    changed, so the shell blurs the backdrop under exactly those shapes.
//  - The Appearance look: --glass-level on <html> (tokens.css interpolates every glass token from
//    it) and html[data-transparency="reduced"], set in <head> from the metas above (no flash)
//    and later by JMGlass.setLook(v, reduce) (the settings sheet's live preview, shell pushes).
//  - slider(el): the Transparency slider (drag / click / keys), transform-only.
// Motion setting (html[data-motion], from /api/prefs): full = springs; reduced = opacity and
// colour fades only (JS springs jump); off = nothing animates.
"use strict";

(function () {
  const root = document.documentElement;
  const mode = () => root.getAttribute("data-motion") || "full";
  const full = () => mode() === "full";
  const now = () => (window.performance && performance.now ? performance.now() : Date.now());
  const raf = window.requestAnimationFrame
    ? (f) => window.requestAnimationFrame(f)
    : (f) => setTimeout(() => f(now()), 16);
  const caf = window.cancelAnimationFrame ? (id) => window.cancelAnimationFrame(id) : clearTimeout;

  // ---- springs as CSS linear() --------------------------------------------------------------
  // Unit mass from 0 to 1, sampled every 1/60 s. Calm damping (ratio ~0.65-0.8): at most a
  // small overshoot, no toy bounce (user, 2026-10-10).
  function curve(k, c) {
    let x = 0, v = 0, t = 0;
    const dt = 1 / 240, pts = [0];
    for (let i = 1; t < 2.5; i++) {
      const a = -k * (x - 1) - c * v;
      v += a * dt; x += v * dt; t += dt;
      if (i % 4 === 0) pts.push(+x.toFixed(4));
      if (Math.abs(x - 1) < 0.0015 && Math.abs(v) < 0.02) break;
    }
    pts.push(1);
    return { ease: "linear(" + pts.join(", ") + ")", dur: Math.round(t * 1000) };
  }
  const S = {
    snappy: curve(340, 28),
    bouncy: curve(380, 27),
    release: curve(520, 34),
    soft: curve(220, 30),
    morph: curve(260, 25),
  };
  for (const n in S) {
    root.style.setProperty("--spr-" + n, S[n].ease);
    root.style.setProperty("--d-" + n, S[n].dur + "ms");
  }

  // ---- one continuous JS spring -----------------------------------------------------------
  // onFrame(x, v) after every step. While anything moves, window.__ctlBusy is the time of the
  // last step: the sky holds still then (a sky redraw invalidates every backdrop blur).
  function Spring(onFrame, k, c) {
    k = k || 420; c = c || 30;
    let x = 0, v = 0, target = 0, id = 0, lastT = 0, held = false, hx = 0, ht = 0;
    const render = () => onFrame(x, v);
    function step(t) {
      id = 0;
      if (held) return;
      window.__ctlBusy = t;
      let ms = Math.min(64, Math.max(0, t - lastT));
      lastT = t;
      for (; ms > 0; ms -= 1) {
        const dt = Math.min(1, ms) * 0.001;
        const a = -k * (x - target) - c * v;
        v += a * dt; x += v * dt;
      }
      if (Math.abs(x - target) < 0.05 && Math.abs(v) < 2) {
        x = target; v = 0; render(); return;
      }
      id = raf(step);
      render();
      // v0.4: no per-frame glass re-measure here. Controls (seg pill, switch, slider) are not
      // glass; a caller that moves a glass surface with a spring calls JMGlass.track() itself.
    }
    function run() { if (!id) { lastT = now(); id = raf(step); } }
    const api = {
      get x() { return x; },
      get v() { return v; },
      get target() { return target; },
      get moving() { return !!id || held; },
      get near() { return !held && Math.abs(x - target) < 0.5 && Math.abs(v) < 15; },
      set(tgt, jump) {
        target = tgt; held = false;
        if (jump || !full()) { caf(id); id = 0; x = tgt; v = 0; render(); return; }
        run();
      },
      hold(nx) {
        const t = now();
        window.__ctlBusy = t;
        if (!held) { held = true; caf(id); id = 0; hx = nx; ht = t; }
        const dt = (t - ht) / 1000;
        if (dt > 0.004) { const iv = (nx - hx) / dt; v = v * 0.35 + iv * 0.65; hx = nx; ht = t; }
        x = nx; target = nx; render();
      },
      release(tgt) { held = false; if (now() - ht > 80) v = 0; api.set(tgt); },
    };
    return api;
  }

  // A pill moved by transform only: translate3d(left) scaleX(width / layout width), origin
  // left. Its layout width is re-synced only at rest (scaleX back to 1 in the same frame).
  function PillMover(el, k, c, stretchMax, stretchDiv) {
    el.style.transformOrigin = "0 50%";
    let base = el.offsetWidth || 1, X = 0, W = 0, xv = 0, rest = [], restT = 0;
    const flush = () => { clearTimeout(restT); const r = rest; rest = []; r.forEach((f) => f()); };
    let xs, ws;
    const paint = () => {
      if (!xs || !ws) return;
      const st = Math.min(stretchMax, Math.abs(xv) / stretchDiv), w = W * (1 + st), left = X - (w - W) / 2;
      el.style.transform = "translate3d(" + left.toFixed(2) + "px,0,0) scaleX(" + (w / base).toFixed(4) + ")";
      if (!xs.moving && !ws.moving && Math.abs(W - base) > 0.01 && W > 0) {
        base = W; el.style.width = W + "px"; el.style.transform = "translate3d(" + X.toFixed(2) + "px,0,0)";
      }
      if (rest.length && xs.near && ws.near) flush();
    };
    xs = Spring((x, v) => { X = x; xv = v; paint(); }, k, c);
    ws = Spring((w) => { W = w; paint(); }, k, c);
    return {
      to(x, w, jump) { if (jump) { base = w || base; el.style.width = base + "px"; } ws.set(w, jump); xs.set(x, jump); },
      hold(x, w) { ws.set(w); xs.hold(x); },
      release(x, w) { ws.set(w); xs.release(x); },
      get x() { return X; },
      get w() { return W; },
      get v() { return xv; },
      // Run f once the pill is visually at rest (at most `max` ms later): heavy work a choice
      // triggers (a theme switch, a re-layout) never competes with the pill for frames.
      afterRest(f, max) {
        rest.push(f); clearTimeout(restT); restT = setTimeout(flush, max || 700);
        if (xs.near && ws.near) flush();
      },
    };
  }

  // ---- mouse drags without pointer capture ------------------------------------------------
  function drag(e, onMove, onEnd) {
    const move = (ev) => onMove(ev);
    const up = (ev) => {
      document.removeEventListener("mousemove", move, true);
      document.removeEventListener("mouseup", up, true);
      window.removeEventListener("blur", up);
      onEnd(ev && ev.clientX !== undefined ? ev : null);
    };
    document.addEventListener("mousemove", move, true);
    document.addEventListener("mouseup", up, true);
    window.addEventListener("blur", up);
  }

  // ---- segmented control ------------------------------------------------------------------
  // Markup: <div class="seg"> <button class="seg-btn">…</button>… </div>; the selected button
  // carries .on (or aria-pressed="true"). The page's own click handlers keep working: this
  // only adds the sliding pill (.seg-pill) and the drag, and a drag ends by clicking the
  // option it lands on. The pill follows .on through a MutationObserver.
  const isOn = (b) => b.classList.contains("on") || b.getAttribute("aria-pressed") === "true";
  function seg(el) {
    if (el._seg) return el._seg;
    const pill = document.createElement("span");
    pill.className = "seg-pill";
    pill.setAttribute("aria-hidden", "true");
    el.insertBefore(pill, el.firstChild);
    el.classList.add("seg-live");
    const sp = PillMover(pill, 380, 36, 0.08, 3200);
    const buttons = () => Array.prototype.slice.call(el.querySelectorAll(".seg-btn, button"));
    const current = () => buttons().find(isOn);
    const nearest = (mid) => {
      let best = null, bd = 1e9;
      buttons().forEach((b) => { const d = Math.abs(b.offsetLeft + b.offsetWidth / 2 - mid); if (d < bd) { bd = d; best = b; } });
      return best;
    };
    let shown = false;
    function place(jump) {
      const b = current();
      if (!b || !b.offsetWidth) { pill.style.opacity = "0"; shown = false; return; }
      pill.style.opacity = "1";
      sp.to(b.offsetLeft, b.offsetWidth, jump || !shown);
      shown = true;
    }
    let swallow = false;
    el.addEventListener("mousedown", (e) => {
      const b = e.target.closest("button");
      if (!b || e.button > 0 || b !== current()) return;
      const x0 = e.clientX, start = sp.x;
      let moved = false, under = null;
      drag(e, (ev) => {
        const dx = ev.clientX - x0;
        if (!moved && Math.abs(dx) < 4) return;
        if (!moved) { moved = true; el.classList.add("lift"); }
        under = nearest(sp.x + sp.w / 2);
        const w = under.offsetWidth, max = el.clientWidth - 3 - w;
        sp.hold(Math.max(3, Math.min(max, start + dx - Math.sign(dx) * 4)), w);
        buttons().forEach((x) => x.classList.toggle("under", x === under));
        ev.preventDefault();
      }, () => {
        el.classList.remove("lift");
        buttons().forEach((x) => x.classList.remove("under"));
        if (!moved) return;
        swallow = true;
        setTimeout(() => { swallow = false; }, 0);
        const best = nearest(sp.x + sp.w / 2 + sp.v * 0.08);
        sp.release(best.offsetLeft, best.offsetWidth);
        if (best !== b) sp.afterRest(() => best.click());
      });
    });
    // A drag's own trailing click must not count as a click on its start button.
    el.addEventListener("click", (e) => { if (swallow) { swallow = false; e.stopImmediatePropagation(); e.preventDefault(); } }, true);
    new MutationObserver(() => place(false)).observe(el, { subtree: true, attributes: true, attributeFilter: ["class", "aria-pressed", "hidden"] });
    // v0.4: a resize re-targets the pill with the spring instead of jumping. Choosing an option
    // bolds its label, which resizes the control in the same frame; jumping here was why the
    // pill never visibly slid (Settings > History Pages / Chats, Motion, Keep chats…).
    if (window.ResizeObserver) new ResizeObserver(() => place(!shown)).observe(el);
    place(true);
    el._seg = { place, pill };
    return el._seg;
  }

  // ---- switch -----------------------------------------------------------------------------
  // Markup: <button class="sw" role="switch" aria-checked="false"><b></b><i></i></button>. A tap
  // toggles (retargeting mid-flight); a drag moves the thumb 1:1 and the side it heads for
  // wins. The thumb moves by transform, the green fill fades by opacity. Fires "change".
  function sw(el) {
    if (el._sw) return el._sw;
    if (!el.querySelector("b")) el.appendChild(document.createElement("b"));
    if (!el.querySelector("i")) el.appendChild(document.createElement("i"));
    const th = el.querySelector("i"), fill = el.querySelector("b");
    let on = el.getAttribute("aria-checked") === "true", swallow = false;
    const travel = () => Math.max(0, el.clientWidth - th.offsetWidth - 4);
    const sp = Spring((x, v) => {
      const t = travel() || 1, st = Math.min(0.12, Math.abs(v) / 1400);
      th.style.transform = "translate3d(" + x.toFixed(2) + "px,0,0) scale(" + (1 + st).toFixed(4) + "," + (1 - st * 0.25).toFixed(4) + ")";
      fill.style.opacity = Math.max(0, Math.min(1, x / t)).toFixed(3);
    }, 520, 42);
    const set = (v, jump) => { on = !!v; el.setAttribute("aria-checked", String(on)); sp.set(on ? travel() : 0, jump); };
    const changed = () => el.dispatchEvent(new Event("change", { bubbles: true }));
    el.addEventListener("mousedown", (e) => {
      if (e.button > 0) return;
      const x0 = e.clientX, start = sp.x;
      let moved = false;
      drag(e, (ev) => {
        const dx = ev.clientX - x0;
        if (!moved && Math.abs(dx) < 3) return;
        moved = true;
        sp.hold(Math.max(0, Math.min(travel(), start + dx)));
        ev.preventDefault();
      }, () => {
        if (!moved) return;
        swallow = true;
        const next = sp.x + sp.v * 0.08 > travel() / 2, was = on;
        on = next; el.setAttribute("aria-checked", String(next));
        sp.release(next ? travel() : 0);
        if (next !== was) changed();
      });
    });
    el.addEventListener("click", (e) => {
      if (swallow) { swallow = false; e.preventDefault(); return; }
      set(!on); changed();
    });
    el.addEventListener("keydown", (e) => {
      if (e.key === " " || e.key === "Enter") { e.preventDefault(); set(!on); changed(); }
    });
    if (window.ResizeObserver) new ResizeObserver(() => sp.set(on ? travel() : 0, true)).observe(el);
    set(on, true);
    el._sw = { set, get on() { return on; } };
    return el._sw;
  }

  // ---- slider -------------------------------------------------------------------------------
  // Markup: <div class="slider" role="slider" tabindex="0"> (the track, fill, tick and thumb are
  // added). opts: {value, def (tick), onInput(v) while it moves, onChange(v) when it settles}.
  // Drag follows 1:1 (mouse events on the document: Servo has no pointer capture); a click
  // jumps there with the spring; arrows +/-1 (Shift +/-10), PageUp/Down +/-10, Home/End.
  // Within 2 of the default it snaps to it.
  function slider(el, opts) {
    if (el._sl) return el._sl;
    opts = opts || {};
    const mk = (tag, cls, parent) => { const n = document.createElement(tag); n.className = cls; parent.appendChild(n); return n; };
    el.textContent = "";
    const track = mk("span", "sl-track", el), fill = mk("b", "sl-fill", track);
    const tick = opts.def === undefined ? null : mk("s", "sl-tick", el);
    const th = mk("i", "sl-thumb", el);
    let value = Math.max(0, Math.min(100, Math.round(opts.value || 0)));
    const travel = () => Math.max(1, el.clientWidth - th.offsetWidth);
    const paint = (x) => {
      const t = travel(), p = Math.max(0, Math.min(1, x / t));
      th.style.transform = "translate3d(" + (p * t).toFixed(2) + "px,0,0)";
      fill.style.transform = "scaleX(" + p.toFixed(4) + ")";
    };
    const sp = Spring((x) => paint(x), 520, 46);
    const placeTick = () => { if (tick) tick.style.left = (th.offsetWidth / 2 + travel() * opts.def / 100) + "px"; };
    const snap = (v) => (opts.def !== undefined && Math.abs(v - opts.def) <= 2 ? opts.def : v);
    const toV = (clientX) => snap(Math.round(100 * (clientX - el.getBoundingClientRect().left - th.offsetWidth / 2) / travel()));
    function set(v, how) {   // how: "jump" (no motion), "hold" (drag), else spring
      v = Math.max(0, Math.min(100, Math.round(v)));
      const changed = v !== value;
      value = v;
      el.setAttribute("aria-valuenow", String(v));
      const x = travel() * v / 100;
      if (how === "jump") sp.set(x, true); else if (how === "hold") sp.hold(x); else sp.set(x);
      return changed;
    }
    const input = () => { if (opts.onInput) opts.onInput(value); };
    const done = () => { if (opts.onChange) opts.onChange(value); };
    el.addEventListener("mousedown", (e) => {
      if (e.button > 0 || el.classList.contains("off")) return;
      e.preventDefault();
      if (el.focus) el.focus();
      if (set(toV(e.clientX))) input();
      drag(e, (ev) => { if (set(toV(ev.clientX), "hold")) input(); ev.preventDefault(); },
        () => { sp.release(travel() * value / 100); done(); });
    });
    el.addEventListener("keydown", (e) => {
      if (el.classList.contains("off")) return;
      const big = e.shiftKey ? 10 : 1;
      const k = { ArrowLeft: -big, ArrowDown: -big, ArrowRight: big, ArrowUp: big, PageDown: -10, PageUp: 10 }[e.key];
      let v = null;
      if (k !== undefined) v = value + k;
      else if (e.key === "Home") v = 0;
      else if (e.key === "End") v = 100;
      if (v === null) return;
      e.preventDefault();
      if (set(v)) { input(); done(); }
    });
    el.setAttribute("aria-valuemin", "0");
    el.setAttribute("aria-valuemax", "100");
    if (window.ResizeObserver) new ResizeObserver(() => { placeTick(); sp.set(travel() * value / 100, true); }).observe(el);
    placeTick();
    set(value, "jump");
    el._sl = { set: (v, jump) => set(v, jump ? "jump" : ""), get value() { return value; } };
    return el._sl;
  }

  // ---- panels and messages ----------------------------------------------------------------
  // pop: show `el`, growing from `origin` (a CSS transform-origin, or an element whose centre is
  // used). unpop: shrink back and hide. Keyframes in tokens.css (.mv-pop-in / .mv-pop-out),
  // so they also run in Servo (no Web Animations). Reduced: fades; off: instant.
  function originOf(el, origin) {
    if (!origin || typeof origin === "string") return origin || "";
    const a = origin.getBoundingClientRect(), b = el.getBoundingClientRect();
    return Math.round(a.left + a.width / 2 - b.left) + "px " + Math.round(a.top + a.height / 2 - b.top) + "px";
  }
  const POP_CLS = ["mv-pop-in", "mv-pop-out", "mv-msg", "mv-from-60", "mv-from-50", "mv-from-30", "mv-to-90", "mv-to-60"];
  function clearAnim(el) { POP_CLS.forEach((c) => el.classList.remove(c)); }
  // Per-surface scales (prototype): {from: .6|.5|.3} on pop, {to: .9|.6} on unpop.
  const fromCls = (f) => (f === 0.6 ? "mv-from-60" : f === 0.5 ? "mv-from-50" : f === 0.3 ? "mv-from-30" : "");
  const toCls = (t) => (t === 0.9 ? "mv-to-90" : t === 0.6 ? "mv-to-60" : "");
  // f once `el`'s own animation has ended. A pop runs two animations (a short opacity one and
  // the scale spring): with full motion wait for the scale, not the 150 ms fade.
  function onAnimEnd(el, f) {
    let done = false;
    const end = (e) => {
      if (done) return;
      if (e && e.target !== el) return;
      if (e && full() && /^mv-op-(in|out)$/.test(e.animationName || "")) return;
      done = true; el.removeEventListener("animationend", end); f();
    };
    el.addEventListener("animationend", end);
    setTimeout(() => end(null), 1200);  // in case the event never comes (hidden, or a style flush lost it)
  }
  function pop(el, origin, opts) {
    clearAnim(el);
    el.hidden = false;
    Glass.kick();
    if (mode() === "off") return;
    const o = originOf(el, origin);
    if (o) el.style.transformOrigin = o;
    void el.offsetWidth;  // restart the keyframes
    const fc = fromCls(opts && opts.from);
    if (fc) el.classList.add(fc);
    el.classList.add("mv-pop-in");
    onAnimEnd(el, () => { clearAnim(el); Glass.kick(); });
    Glass.track(600);
  }
  function unpop(el, opts) {
    return new Promise((res) => {
      clearAnim(el);
      if (mode() === "off" || el.hidden) { el.hidden = true; Glass.kick(); res(); return; }
      void el.offsetWidth;
      const tc = toCls(opts && opts.to);
      if (tc) el.classList.add(tc);
      el.classList.add("mv-pop-out");
      Glass.track(260);
      onAnimEnd(el, () => { clearAnim(el); el.hidden = true; Glass.kick(); res(); });
    });
  }
  function rise(el) {
    if (mode() === "off") return;
    el.classList.add("mv-msg");
    onAnimEnd(el, () => { el.classList.remove("mv-msg"); Glass.kick(); });
    Glass.track(500);
  }

  // ---- ellipsis (Servo has no text-overflow) ----------------------------------------------
  // Cut `text` so it fits `el` on one line, ending in "…". The full text goes into title.
  function ellipsis(el, text, maxWidth) {
    text = text === undefined ? el.getAttribute("data-full") || el.textContent : String(text);
    el.setAttribute("data-full", text);
    el.textContent = text;
    const limit = maxWidth || el.clientWidth;
    if (!limit || el.scrollWidth <= limit + 0.5) { el.removeAttribute("title"); return; }
    el.title = text;
    let lo = 0, hi = text.length;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      el.textContent = text.slice(0, mid).trimEnd() + "…";
      if (el.scrollWidth <= limit + 0.5) lo = mid; else hi = mid - 1;
    }
    el.textContent = text.slice(0, lo).trimEnd() + "…";
  }

  // ---- appearance: transparency level -------------------------------------------------------
  // Same mapping as mythbrowse_engine::glass::glass_level: slider 0-100 -> steps from the B look,
  // 0 -> -1.5, 50 (default) -> 0, 100 -> +1.
  const Look = (function () {
    const DEF = 50, MIN = -1.5, MAX = 1;
    const clampV = (v) => { v = Math.round(Number(v)); return isFinite(v) ? Math.max(0, Math.min(100, v)) : DEF; };
    function level(v) { v = clampV(v); return v <= DEF ? MIN * (DEF - v) / DEF : MAX * (v - DEF) / (100 - DEF); }
    let cur = { v: DEF, reduce: false };
    function apply(v, reduce) {
      cur = { v: clampV(v), reduce: !!reduce };
      root.style.setProperty("--glass-level", String(Math.round(level(cur.v) * 10000) / 10000));
      if (cur.reduce) root.setAttribute("data-transparency", "reduced");
      else root.removeAttribute("data-transparency");
    }
    const mv = document.querySelector('meta[name="mb-transparency"]');
    const mr = document.querySelector('meta[name="mb-reduce-transparency"]');
    apply(mv && /^\d{1,3}$/.test(mv.content) ? mv.content : DEF, !!(mr && mr.content === "1"));
    return { apply, level, get v() { return cur.v; }, get reduce() { return cur.reduce; }, DEF };
  })();

  // ---- liquid glass: mode + reporter ------------------------------------------------------
  const Glass = (function () {
    const meta = document.querySelector('meta[name="mb-glass"]');
    const tokenMeta = document.querySelector('meta[name="mb-token"]');
    const internal = location.origin === "https://mythbrowse.internal";
    let gmode = meta && /^(blur|flat|off)$/.test(meta.content) ? meta.content : "off";
    root.setAttribute("data-glass", gmode);
    let last = null, id = 0, trackUntil = 0, movingSent = false, sending = false, queued = false;
    const KIND = { c: "c", p: "p", f: "f", chrome: "c", panel: "p", float: "f" };
    const ORDER = { f: 0, p: 1, c: 2 };  // the shell keeps at most 24: floating panels first

    // Effective opacity: the element's own and every ancestor's (a fading panel fades its
    // blurred backdrop with it).
    function alphaOf(el) {
      let a = 1;
      for (let n = el; n && n.nodeType === 1 && a > 0.004; n = n.parentElement) {
        const cs = getComputedStyle(n);
        if (cs.display === "none" || cs.visibility === "hidden") return 0;
        // A blurred subtree (filter) shows only soft shapes: no sharp glass under it.
        if (n !== el && cs.filter && cs.filter !== "none") return 0;
        a *= parseFloat(cs.opacity);
        if (isNaN(a)) a = 1;
      }
      return a;
    }
    // The nearest scrolling / clipping ancestor (a scroll container cuts its glass off).
    function clipOf(el) {
      for (let n = el.parentElement; n && n !== document.body; n = n.parentElement) {
        const cs = getComputedStyle(n);
        if (/(auto|scroll|hidden)/.test(cs.overflowY + cs.overflowX)) return n.getBoundingClientRect();
      }
      return null;
    }
    const f1 = (v) => String(Math.round(v * 10) / 10);
    function measure() {
      const out = [];
      document.querySelectorAll("[data-glass], .glass").forEach((el) => {
        if (el.hidden || el === root) return;
        const r = el.getBoundingClientRect();
        if (r.width < 1 || r.height < 1) return;
        if (r.bottom < 0 || r.right < 0 || r.top > innerHeight || r.left > innerWidth) return;
        const a = alphaOf(el);
        if (a <= 0.004) return;
        const kind = KIND[el.getAttribute("data-glass")] || "p";
        const radius = parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0;
        let entry = [f1(r.left), f1(r.top), f1(r.width), f1(r.height), f1(radius), kind, (Math.round(a * 100) / 100).toString()];
        const c = clipOf(el);
        if (c) entry = entry.concat([f1(c.left), f1(c.top), f1(c.width), f1(c.height)]);
        out.push({ k: ORDER[kind], s: entry.join(",") });
      });
      out.sort((x, y) => x.k - y.k);
      return out.slice(0, 24).map((e) => e.s).join(";");
    }
    function send(report, moving) {
      if (!internal || !tokenMeta) return;
      if (sending) { queued = true; return; }  // one request in flight; the newest wins
      sending = true;
      let url = "/api/glass?t=" + encodeURIComponent(tokenMeta.content) + "&r=" + encodeURIComponent(report);
      if (window.JM && JM.hour) url += "&h=" + (Math.round(JM.hour() * 100) / 100);
      if (moving) url += "&mv=1";
      fetch(url)
        .then((r) => r.json())
        .then((r) => { if (r && r.mode && r.mode !== gmode) setMode(r.mode); })
        .catch(() => {})
        .then(() => { sending = false; if (queued) { queued = false; last = null; kick(); } });
    }
    function frame() {
      id = 0;
      const t = now(), moving = t < trackUntil;
      if (gmode !== "off") {
        const report = measure();
        if (report !== last || moving !== movingSent) {
          last = report; movingSent = moving;
          send(report, moving);
        }
      }
      if (moving) id = raf(frame);
    }
    // Re-measure on the next frame; `track(ms)` keeps measuring every frame for that long
    // (a CSS transition or keyframes moving a glass surface).
    function kick(soon) {
      if (gmode === "off" || id) return;
      id = raf(frame);
      if (soon) trackUntil = Math.max(trackUntil, now() + 120);
    }
    function track(ms) { trackUntil = Math.max(trackUntil, now() + (full() ? ms : 220)); kick(); }
    function setMode(m) {
      if (!/^(blur|flat|off)$/.test(m) || m === gmode) return;
      gmode = m;
      root.setAttribute("data-glass", m);
      last = null;
      if (window.JMSky) JMSky.redraw();
      kick();
    }
    // Can a change on `el` move, resize or fade a glass surface? (It is one, or holds one.)
    const SEL = "[data-glass], .glass";
    function shapes(el) {
      if (!el || el.nodeType !== 1 || el === document.documentElement) return false;
      return !!(el.matches && el.matches(SEL)) || !!(el.querySelector && el.querySelector(SEL));
    }
    function start() {
      if (!internal) return;
      kick();
      window.addEventListener("resize", () => kick());
      document.addEventListener("scroll", () => kick(true), true);
      // v0.4: only motion that can move or fade a glass shape is tracked (a glass element, or an
      // ancestor of one, changing transform / opacity / geometry). Before, any transition on the
      // page (a hover colour, the address bar's focus ring, a seg pill) re-measured every glass
      // surface each frame for 600 ms, forcing style + layout flushes and /api/glass requests.
      const GEO = /^(transform|opacity|left|top|right|bottom|width|height|margin|padding|translate|scale|rotate|all|visibility|display)/;
      const onStart = (e) => { if (shapes(e.target) && (!e.propertyName || GEO.test(e.propertyName))) track(600); };
      const onEnd = (e) => { if (shapes(e.target)) kick(); };
      ["transitionstart", "transitionrun", "animationstart"].forEach((ev) => document.addEventListener(ev, onStart, true));
      ["transitionend", "animationend"].forEach((ev) => document.addEventListener(ev, onEnd, true));
      new MutationObserver((list) => {
        for (let i = 0; i < list.length; i++) {
          const m = list[i];
          if (m.type === "childList" || m.attributeName === "hidden" || m.attributeName === "data-glass" || shapes(m.target)) { kick(); return; }
        }
      }).observe(document.body, {
        subtree: true, childList: true, attributes: true, attributeFilter: ["class", "style", "hidden", "data-glass"],
      });
      if (window.JM && JM.onChange) JM.onChange(() => { last = null; kick(); });
      document.addEventListener("visibilitychange", () => { if (!document.hidden) { last = null; kick(); } });
    }
    if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
    else start();
    // Appearance changed (slider preview, Save / Cancel, a push from the shell).
    function setLook(v, reduce) {
      if (Look.v === Math.round(v) && Look.reduce === !!reduce) return;
      Look.apply(v, reduce);
      last = null;
      kick();
    }
    return { kick, track, setMode, setLook, get look() { return { v: Look.v, reduce: Look.reduce }; }, level: Look.level, get mode() { return gmode; }, measure };
  })();

  // ---- wire up controls -------------------------------------------------------------------
  function enhance(scope) {
    (scope || document).querySelectorAll(".seg").forEach(seg);
    (scope || document).querySelectorAll(".sw").forEach(sw);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", () => enhance());
  else enhance();

  window.JMMotion = { S, Spring, PillMover, seg, sw, slider, pop, unpop, rise, ellipsis, enhance, mode };
  window.JMGlass = Glass;
})();
