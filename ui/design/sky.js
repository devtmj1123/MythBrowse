// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// The MythDesk living sky (MythDesk sky.js), drawn on <canvas id="sky">:
// a gradient blended through the day, slow washes of colour, a sun or moon glow riding its
// arc (with turning rays by day), stars after dark and drifting clouds. Same palette, seeds
// and maths as MythDesk, so the two products show the same sky. Nothing here is a photo.
//
// MythBrowse draws one window-sized sky across two webviews: the chrome (window-sized; its
// sky shows around the floating web-page card) and, for internal pages, the page under the
// toolbar. Attributes on the canvas say where it sits in that window sky:
//   data-band          the chrome: no clouds, drawn once a minute (no animation loop)
//   data-above="88"    CSS px of window above this page (the chrome); the sky is drawn as if
//                      the page started that far down, so it continues the chrome's sky
//   data-bleed="12"    the canvas overhangs the viewport by this much (for the CSS blur)
// The page sky animates at about 15 fps and stops while the page is hidden.
"use strict";

(function () {
  const JM = window.JM;
  const SUNRISE = 7.1, SUNSET = 19.2;  // Kuala Lumpur is near the equator: about 7:10 to 19:15 all year
  const mix = (a, b, t) => a.map((v, i) => v + (b[i] - v) * t);
  const rgb = (c, a) => "rgba(" + (c[0] | 0) + "," + (c[1] | 0) + "," + (c[2] | 0) + "," + (a === undefined ? 1 : a) + ")";

  // Seeded random so stars and clouds are stable between frames (MythDesk's seed and order).
  let seed = 7;
  const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
  const stars = Array.from({ length: 140 }, () => ({ x: rnd(), y: rnd() * 0.7, r: 0.4 + rnd() * 1.1, p: rnd() * 6.28 }));
  const clouds = Array.from({ length: 10 }, (_, i) => {
    const y = 0.15 + rnd() * 0.65;
    return {
      x: rnd(), y, w: 0.22 + rnd() * 0.3, h: 0.06 + rnd() * 0.07,
      v: 0.006 + y * 0.012 + rnd() * 0.004, a: 0.5 + rnd() * 0.5, warm: i % 3 === 0, depth: 0.4 + y,
      bob: rnd() * 6.28,
      puffs: Array.from({ length: 3 + Math.floor(rnd() * 3) }, () => ({ dx: (rnd() - 0.5) * 1.2, dy: (rnd() - 0.5) * 0.5, s: 0.55 + rnd() * 0.5 })),
    };
  });
  const washes = Array.from({ length: 3 }, (_, i) => ({ x: rnd(), y: 0.2 + rnd() * 0.5, r: 0.5 + rnd() * 0.4, v: 0.00004 + rnd() * 0.00005, p: i * 2.1 }));
  // The cloud deck the title bar and toolbar sit in (liquid glass prototype, calm revision):
  // long flat banks lit from above, shaded undersides at low contrast, drifting slowly. Drawn
  // in window-sky coordinates, so the chrome and the page under it continue each other.
  const deck = Array.from({ length: 11 }, (_, i) => ({
    x: (i + rnd() * 0.6) / 11, y: 0.18 + rnd() * 0.62, w: 0.07 + rnd() * 0.07, v: 0.0016 + rnd() * 0.0022, a: 0.6 + rnd() * 0.4,
    puffs: Array.from({ length: 4 + Math.floor(rnd() * 3) }, (_, k) => ({ dx: (k - 2.5) * 0.42 + (rnd() - 0.5) * 0.3, dy: (rnd() - 0.5) * 0.35, s: 0.55 + rnd() * 0.55 })),
  }));
  // With liquid glass on (html[data-glass] blur / flat) the shell draws the sky itself and
  // this page is transparent: nothing to draw here.
  const shellSky = () => {
    const g = document.documentElement.getAttribute("data-glass");
    return g === "blur" || g === "flat";
  };

  const SCALE = 0.4;  // render small and let CSS scale it up: soft and cheap
  let canvas, ctx, band = false, w = 0, h = 0, W = 0, H = 0, oy = 0, ox = 0, last = 0;

  // Wall-clock time (ms into the day), so the chrome strip and the page agree on where the
  // washes and the sun's breathing are.
  const clock = () => Date.now() % 86400000;

  function resize() {
    band = canvas.hasAttribute("data-band");
    const above = Number(canvas.getAttribute("data-above")) || 0;
    const bleed = Number(canvas.getAttribute("data-bleed")) || 0;
    const vw = innerWidth, vh = innerHeight;
    // Height of the whole window sky: the chrome covers the whole window, a page under the
    // chrome starts `above` px down it.
    const full = vh + above;
    w = Math.max(1, Math.round((vw + 2 * bleed) * SCALE));
    h = Math.max(1, Math.round((vh + 2 * bleed) * SCALE));
    W = Math.max(1, vw * SCALE);
    H = Math.max(1, full * SCALE);
    ox = bleed * SCALE;
    oy = (above - bleed) * SCALE;
    canvas.width = w;
    canvas.height = h;
  }

  function draw(t) {
    const hour = JM.hour();
    const p = JM.palette(hour);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.translate(ox, -oy);  // draw in window-sky coordinates (W x H)

    const g = ctx.createLinearGradient(0, 0, 0, H);
    g.addColorStop(0, rgb(p.top));
    g.addColorStop(0.55, rgb(p.mid));
    g.addColorStop(1, rgb(p.low));
    ctx.fillStyle = g;
    ctx.fillRect(-ox, 0, W + 2 * ox, H + h);

    for (const c of washes) {
      const x = (0.5 + 0.45 * Math.sin(t * c.v + c.p)) * W;
      const y = (c.y + 0.06 * Math.cos(t * c.v * 1.3 + c.p)) * H;
      const R = c.r * Math.max(W, H);
      const col = mix(p.glow, p.top, 0.5);
      const wg = ctx.createRadialGradient(x, y, 0, x, y, R);
      wg.addColorStop(0, rgb(col, 0.16));
      wg.addColorStop(1, rgb(col, 0));
      ctx.fillStyle = wg;
      ctx.fillRect(-ox, 0, W + 2 * ox, H + h);
    }

    if (p.stars > 0.01) {
      for (const s of stars) {
        const tw = band ? 0.7 : 0.4 + 0.6 * Math.sin(t / 700 + s.p * 3);
        ctx.fillStyle = "rgba(255,255,255," + (p.stars * Math.max(0.1, tw) * 0.95).toFixed(3) + ")";
        ctx.beginPath();
        ctx.arc(s.x * W, s.y * H, s.r * SCALE * 1.6, 0, 6.283);
        ctx.fill();
      }
    }

    // Sun by day, moon by night, each riding an arc from left to right.
    const day = hour >= SUNRISE - 0.6 && hour <= SUNSET + 0.6;
    let k;
    if (day) k = (hour - (SUNRISE - 0.6)) / (SUNSET - SUNRISE + 1.2);
    else k = ((hour - SUNSET - 0.6 + 24) % 24) / (24 - (SUNSET - SUNRISE + 1.2));
    const cx = W * (0.08 + 0.84 * k);
    const cy = H * (0.95 - 0.75 * Math.sin(Math.PI * k));
    const R = Math.max(W, H) * (day ? 0.42 : 0.32) * (1 + 0.04 * Math.sin(t / 1800));
    const glow = ctx.createRadialGradient(cx, cy, 0, cx, cy, R);
    glow.addColorStop(0, rgb(p.glow, day ? 0.24 : 0.22));
    glow.addColorStop(0.25, rgb(p.glow, day ? 0.07 : 0.07));
    glow.addColorStop(1, rgb(p.glow, 0));
    ctx.fillStyle = glow;
    ctx.fillRect(-ox, 0, W + 2 * ox, H + h);
    if (day) {  // slowly turning rays
      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate(t / 40000);
      for (let i = 0; i < 12; i++) {
        ctx.rotate(Math.PI / 6);
        const len = R * (0.6 + 0.25 * Math.sin(t / 2500 + i * 1.7));
        const rg = ctx.createLinearGradient(0, 0, len, 0);
        rg.addColorStop(0, rgb(p.glow, 0.035));
        rg.addColorStop(1, rgb(p.glow, 0));
        ctx.fillStyle = rg;
        ctx.beginPath();
        ctx.moveTo(0, 0);
        ctx.lineTo(len, -len * 0.06);
        ctx.lineTo(len, len * 0.06);
        ctx.fill();
      }
      ctx.restore();
    }
    ctx.fillStyle = day ? rgb(mix(p.glow, [255, 255, 255], 0.35), 0.7) : "rgba(235,238,255,0.85)";
    ctx.beginPath();
    ctx.arc(cx, cy, Math.max(2, Math.min(W, H) * 0.013), 0, 6.283);
    ctx.fill();

    if (!band) {
      // Clouds: soft elliptical puffs lit by the current glow colour.
      const lit = mix(p.glow, [255, 255, 255], 0.5);
      for (const c of clouds) {
        const x = (((c.x + (t / 1000) * c.v) % 1.6) - 0.3) * W;
        const y = (c.y + 0.006 * Math.sin(t / 3000 + c.bob)) * H;
        const col = c.warm ? mix(lit, p.low, 0.35) : mix(lit, p.mid, 0.25);
        for (const pf of c.puffs) {
          ctx.save();
          ctx.translate(x + pf.dx * c.w * W * 0.5, y + pf.dy * c.h * H);
          ctx.scale(1, c.h / c.w);
          const r = c.w * W * 0.5 * pf.s;
          const cg = ctx.createRadialGradient(0, 0, 0, 0, 0, r);
          cg.addColorStop(0, rgb(col, p.cloud * c.a * 0.45));
          cg.addColorStop(0.6, rgb(col, p.cloud * c.a * 0.14));
          cg.addColorStop(1, rgb(col, 0));
          ctx.fillStyle = cg;
          ctx.beginPath();
          ctx.arc(0, 0, r, 0, 6.283);
          ctx.fill();
          ctx.restore();
        }
      }
      // Warm haze along the horizon.
      const hz = ctx.createLinearGradient(0, H * 0.65, 0, H);
      hz.addColorStop(0, rgb(p.low, 0));
      hz.addColorStop(1, rgb(mix(p.low, p.glow, 0.4), 0.4));
      ctx.fillStyle = hz;
      ctx.fillRect(-ox, H * 0.65, W + 2 * ox, H * 0.35 + h);
    }
    drawDeck(t, p);
  }

  function drawDeck(t, p) {
    const DH = 150 * SCALE, night = p.stars > 0.3, dayish = p.cloud > 0.6;
    if (oy > DH * 1.3) return;  // this page starts below the deck
    const lit = mix(p.glow, [255, 255, 255], 0.5);
    const key = night ? mix(p.mid, [138, 160, 204], 0.55) : dayish ? mix(lit, [255, 255, 255], 0.55) : mix(p.glow, [255, 236, 214], 0.35);
    const under = night ? mix(p.top, [26, 36, 74], 0.5) : dayish ? mix(p.top, p.mid, 0.35) : mix(p.top, p.low, 0.25);
    const ka = night ? 0.26 : dayish ? 0.42 : 0.28;  // calm: low contrast between tops and undersides
    const zg = ctx.createLinearGradient(0, 0, 0, DH * 1.25);
    zg.addColorStop(0, rgb(mix(p.top, [0, 0, 0], night ? 0.25 : 0.08), night ? 0.30 : 0.16));
    zg.addColorStop(0.55, rgb(p.top, 0));
    zg.addColorStop(0.82, rgb(key, night ? 0.03 : 0.06));
    zg.addColorStop(1, rgb(key, 0));
    ctx.fillStyle = zg;
    ctx.fillRect(-ox, 0, W + 2 * ox, DH * 1.25);
    const layers = [[0.30, mix(under, key, 0.45), ka * 0.30], [-0.10, key, ka]];
    for (const b of deck) {
      const x = (((b.x + (t / 1000) * b.v * 0.25) % 1.2) - 0.1) * W, y = b.y * DH, R = b.w * W;
      for (const [dy, col, a] of layers) {
        for (const pf of b.puffs) {
          const r = R * pf.s, cx = x + pf.dx * R, cy = y + (pf.dy + dy) * R * 0.55;
          ctx.save();
          ctx.translate(cx, cy);
          ctx.scale(1, 0.52);
          const g2 = ctx.createRadialGradient(0, 0, 0, 0, 0, r);
          g2.addColorStop(0, rgb(col, a * b.a));
          g2.addColorStop(0.5, rgb(col, a * b.a * 0.45));
          g2.addColorStop(1, rgb(col, 0));
          ctx.fillStyle = g2;
          ctx.beginPath();
          ctx.arc(0, 0, r, 0, 6.283);
          ctx.fill();
          ctx.restore();
        }
      }
    }
  }

  function safeDraw() {
    if (shellSky()) {
      if (canvas.width > 1) { canvas.width = 1; canvas.height = 1; }  // free the bitmap
      return;
    }
    if (canvas.width <= 1) resize();
    try { draw(clock()); } catch (e) { /* the CSS sky colour underneath still shows */ }
  }

  // ~15 fps; holds still while a control moves (window.__ctlBusy, set by motion.js springs)
  // and while motion isn't "full".
  function loop(ts) {
    const busy = ts - (window.__ctlBusy || -1e9) < 250;
    const still = (document.documentElement.getAttribute("data-motion") || "full") !== "full";
    if (!document.hidden && !busy && !still && !shellSky() && ts - last > 66) {  // ~15 fps is plenty for clouds this slow
      last = ts;
      safeDraw();
    }
    requestAnimationFrame(loop);
  }

  function start() {
    canvas = document.getElementById("sky");
    if (!canvas || !canvas.getContext) return;
    ctx = canvas.getContext("2d");
    if (!ctx) return;
    resize();
    window.addEventListener("resize", () => { resize(); safeDraw(); });
    safeDraw();
    JM.onChange(safeDraw);
    window.JMSky = { redraw: safeDraw };
    const reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (band || reduce) setInterval(safeDraw, 60 * 1000);
    else requestAnimationFrame(loop);
  }

  if (document.readyState === "loading") window.addEventListener("DOMContentLoaded", start);
  else start();
})();
