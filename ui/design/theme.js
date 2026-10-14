// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Time of day for the JustMyth (MythDesk) design system. The sky palette, sun times and phase
// boundaries are MythDesk's (desktop/ui/sky.js), so both products show the same sky at the
// same hour. See docs/DESIGN.md.
//
// Sets on <html>: data-phase (night | dawn | day | dusk | evening) and data-tone (light for
// dawn and day, dark otherwise; MythDesk switches its glass the same way), plus the sky
// colours --sky-top/-mid/-low and --tab-solid (an opaque copy of the active-tab glass, for
// fade-outs). Refreshed every minute.
//
// Pin the hour for screenshots/tests: <meta name="mb-theme-hour" content="21.5"> (filled by
// the shell from MYTHBROWSE_THEME_HOUR) or ?hour=21.5 on the page URL (as in MythDesk).
//
// API (window.JM): hour(), pinned, phase(h), tone(h), palette(h), colors(), onChange(fn).
"use strict";

(function () {
  // MythDesk: hour -> [sky top, sky middle, horizon, glow, star alpha, cloud light]
  const KEYS = [
    [0.0, "#04060f", "#0a1128", "#1b1a3a", "#3a2f6a", 1.0, 0.10],
    [4.5, "#060a1c", "#161c45", "#3b2f5c", "#7a4f7a", 0.8, 0.12],
    [6.0, "#3a4677", "#b48aa0", "#f2b49a", "#ffc9a0", 0.2, 0.45],
    [7.2, "#7499cb", "#c0b9c8", "#e0cbba", "#eedac4", 0.0, 0.68],
    [10.0, "#6698d2", "#a2c1e2", "#d0d8de", "#ece5d6", 0.0, 0.72],
    [13.0, "#6094d2", "#9ebfe3", "#ccd6de", "#e9e6de", 0.0, 0.72],
    [16.5, "#6888bf", "#adaec3", "#dcc5b0", "#ecd2b3", 0.0, 0.66],
    [18.3, "#2b4386", "#6670ab", "#ec9f7c", "#ffad6e", 0.0, 0.45],
    [19.3, "#1f2d63", "#4b5a98", "#c48c86", "#f08d68", 0.15, 0.30],
    [20.5, "#0e1638", "#222b5a", "#4a3a62", "#8a5a78", 0.6, 0.15],
    [24.0, "#04060f", "#0a1128", "#1b1a3a", "#3a2f6a", 1.0, 0.10],
  ];

  const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const mix = (a, b, t) => a.map((v, i) => v + (b[i] - v) * t);
  const smooth = (t) => t * t * (3 - 2 * t);
  const rgba = (c, a) =>
    "rgba(" + Math.round(c[0]) + ", " + Math.round(c[1]) + ", " + Math.round(c[2]) + ", " +
    (a === undefined ? 1 : a) + ")";

  function palette(hour) {
    let i = 0;
    while (i < KEYS.length - 2 && hour >= KEYS[i + 1][0]) i++;
    const a = KEYS[i], b = KEYS[i + 1];
    const t = smooth(Math.min(1, Math.max(0, (hour - a[0]) / (b[0] - a[0]))));
    return {
      top: mix(hex(a[1]), hex(b[1]), t), mid: mix(hex(a[2]), hex(b[2]), t),
      low: mix(hex(a[3]), hex(b[3]), t), glow: mix(hex(a[4]), hex(b[4]), t),
      stars: a[5] + (b[5] - a[5]) * t, cloud: a[6] + (b[6] - a[6]) * t,
    };
  }

  // MythDesk phases.
  function phase(hour) {
    if (hour < 6.2 || hour >= 20.5) return "night";
    if (hour < 7.5) return "dawn";
    if (hour < 17.3) return "day";
    if (hour < 19.5) return "dusk";
    return "evening";
  }
  function tone(hour) {
    const p = phase(hour);
    return p === "day" || p === "dawn" ? "light" : "dark";
  }

  function pinnedHour() {
    const meta = document.querySelector('meta[name="mb-theme-hour"]');
    let v = meta ? meta.getAttribute("content") : "";
    if (!v || v.indexOf("{{") === 0) {
      try { v = new URLSearchParams(location.search).get("hour") || ""; } catch (e) { v = ""; }
    }
    const n = parseFloat(v);
    return v !== "" && n >= 0 && n < 24 ? n : null;
  }
  const PINNED = pinnedHour();

  // Settings > Appearance > Theme (v0.4): "light" / "dark" pin the look to a bright day or a
  // dark night; "time" (default) follows the clock. A pinned hour from the shell (screenshots)
  // always wins. The glass reporter sends JM.hour() to the shell, so its sky follows too.
  const THEME_HOUR = { light: 12, dark: 21.5 };
  let themeHour = null;
  function hour() {
    if (PINNED !== null) return PINNED;
    if (themeHour !== null) return themeHour;
    const d = new Date();
    return d.getHours() + d.getMinutes() / 60 + d.getSeconds() / 3600;
  }

  // Literal colours for inline SVG icons (Servo renders inline <svg> as an image, so icons
  // cannot inherit CSS colours). These are MythDesk's --text / --soft / --dim flattened over
  // its glass, plus its accent and status colours.
  const COLORS = {
    light: { icon: "#16203a", soft: "#5f677c", dim: "#949cab", accent: "#5b4bd6", ink: "#16203a",
      inkText: "#ffffff", bad: "#e2486a", ok: "#1fb877", warn: "#d9961a", white: "#ffffff", star: "#d9961a" },
    dark: { icon: "#f3f5ff", soft: "#a3a8be", dim: "#727890", accent: "#b9a8ff", ink: "#ffffff",
      inkText: "#12182e", bad: "#ff6b85", ok: "#4fe0a0", warn: "#ffc65c", white: "#ffffff", star: "#ffc65c" },
  };

  const listeners = [];
  let current = { tone: null, phase: null };

  function apply() {
    const h = hour();
    const p = palette(h);
    const t = tone(h);
    const root = document.documentElement;
    const st = root.style;
    st.setProperty("--sky-top", rgba(p.top));
    st.setProperty("--sky-top-0", rgba(p.top, 0));
    st.setProperty("--sky-mid", rgba(p.mid));
    st.setProperty("--sky-low", rgba(p.low));
    // The active tab's glass (--glass-hi) over the top of the sky, as an opaque colour.
    const tab = t === "light" ? mix(p.top, [255, 255, 255], 0.72) : mix(p.top, [28, 36, 64], 0.70);
    st.setProperty("--tab-solid", rgba(tab));
    st.setProperty("--tab-solid-0", rgba(tab, 0));
    const ph = phase(h);
    if (t !== current.tone || ph !== current.phase) {
      current = { tone: t, phase: ph };
      root.setAttribute("data-tone", t);
      root.setAttribute("data-phase", ph);
      listeners.forEach((fn) => { try { fn(t, ph); } catch (e) { /* keep going */ } });
    }
  }

  // Motion (Settings: full / reduced / off, combined with the OS setting by the shell):
  // data-motion on <html>; tokens.css turns movement down / off from it.
  function setMotion(m) {
    document.documentElement.setAttribute("data-motion", m === "reduced" || m === "off" ? m : "full");
  }
  (function loadMotion() {
    const meta = document.querySelector('meta[name="mb-token"]');
    if (!meta || location.origin !== "https://mythbrowse.internal") return;
    fetch("/api/prefs?t=" + encodeURIComponent(meta.content))
      .then((r) => r.json())
      .then((r) => { if (r && r.ok) setMotion(r.effective); })
      .catch(() => { /* keep full */ });
  })();

  function setTheme(t) {
    const h = Object.prototype.hasOwnProperty.call(THEME_HOUR, t) ? THEME_HOUR[t] : null;
    if (h === themeHour) return;
    themeHour = h;
    apply();
  }
  // The saved theme (one small internal request; never on web pages). Other pages follow a
  // change made in Settings when they are shown again (or at once via the storage event).
  function loadTheme() {
    const meta = document.querySelector('meta[name="mb-token"]');
    if (!meta || location.origin !== "https://mythbrowse.internal" || PINNED !== null) return;
    fetch("/api/settings?t=" + encodeURIComponent(meta.content))
      .then((r) => r.json())
      .then((r) => { if (r && r.ok !== false && r.theme) setTheme(r.theme); })
      .catch(() => { /* keep the time of day */ });
  }
  loadTheme();
  window.addEventListener("storage", (e) => { if (e.key === "mb-theme") loadTheme(); });
  document.addEventListener("visibilitychange", () => { if (!document.hidden) loadTheme(); });

  window.JM = {
    setMotion: setMotion,
    setTheme: setTheme,
    theme: () => (themeHour === null ? "time" : themeHour === THEME_HOUR.light ? "light" : "dark"),
    motion: () => document.documentElement.getAttribute("data-motion") || "full",
    hour: hour,
    pinned: PINNED !== null,
    phase: phase,
    tone: tone,
    palette: palette,
    colors: () => COLORS[current.tone || tone(hour())],
    onChange: (fn) => { listeners.push(fn); },
    refresh: apply,
  };

  apply();
  if (PINNED === null) setInterval(apply, 60 * 1000);
})();
