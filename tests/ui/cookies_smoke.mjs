// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// UI smoke for cookie management (v0.4.2; headless Chromium, NOT Servo): Settings > Cookies
// (ui/cookies.js) and the address bar's site popover (ui/popups.js), against a stateful mock of
// the shell's cookie ops (crates/shell/src/cookies_host.rs). Also takes the v0.4.2 review shots:
// address bar focused / unfocused, the Settings rail and the Cookies section, light and dark.
//   PW=<playwright-core dir> CHROME=<chrome-headless-shell> OUT=<dir> node cookies_smoke.mjs
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
const { chromium } = await import(path.join(process.env.PW, "index.mjs"));
const exe = process.env.CHROME;
const UI = process.env.MB_UI || path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../ui");
const OUT = process.env.OUT || "/tmp/cookies-smoke";
fs.mkdirSync(OUT, { recursive: true });
const b = await chromium.launch({ executablePath: exe });
const calls = [], errors = [], checks = [];
const ok = (name, cond, got) => checks.push({ name, pass: !!cond, got });

// ---- stateful mock ----------------------------------------------------------------------
const day = 86400;
const now = Math.floor(Date.now() / 1000);
let C, POL, EXIT, KEEP = false;
function reset() {
  C = {
    "example.com": [
      { name: "sid", domain: "www.example.com", host_only: true, path: "/", expires: null, secure: true, http_only: true, size: 40 },
      { name: "prefs", domain: "example.com", host_only: false, path: "/", expires: now + 30 * day, secure: false, http_only: false, size: 18 },
      { name: "cart_token_with_a_long_name", domain: "shop.example.com", host_only: true, path: "/checkout", expires: now + 2 * day, secure: true, http_only: false, size: 2200 },
    ],
    "github.com": [
      { name: "_gh_sess", domain: "github.com", host_only: true, path: "/", expires: null, secure: true, http_only: true, size: 512 },
      { name: "logged_in", domain: "github.com", host_only: false, path: "/", expires: now + 365 * day, secure: true, http_only: true, size: 12 },
    ],
    "tracker.test": [{ name: "uid", domain: "tracker.test", host_only: false, path: "/", expires: now + 300 * day, secure: false, http_only: false, size: 24 }],
  };
  POL = { "github.com": "allow" };
  EXIT = false;
  KEEP = false;
}
reset();
const STORAGE = new Set(["example.com", "news.example.org"]);
function sites() {
  const names = new Set([...Object.keys(C), ...STORAGE, ...Object.keys(POL)]);
  return [...names].sort().map((s) => {
    const rows = C[s] || [];
    return { site: s, count: rows.length, size: rows.reduce((a, r) => a + r.size, 0), storage: STORAGE.has(s), policy: POL[s] || null };
  });
}
const sitesJson = () => ({ ok: true, sites: sites(), clear_on_exit: EXIT, persistent: false, persist_pref: KEEP,
  third_party: { supported: false, note: "Blocking third-party cookies is not supported yet: the Servo engine MythBrowse uses does not tell its cookie store which site you are on. Block a tracker's site here instead." } });
const siteJson = (s) => ({ ok: true, site: s, cookies: C[s] || [], count: (C[s] || []).length, policy: POL[s] || null });
function mock(op, q) {
  switch (op) {
    case "cookies": return sitesJson();
    case "cookies_list": return siteJson(q.site);
    case "cookie_delete": C[q.site] = (C[q.site] || []).filter((r) => !(r.name === q.name && r.domain === q.domain && r.path === q.path)); if (!C[q.site].length) delete C[q.site]; return siteJson(q.site);
    case "cookie_policy": if (q.mode === "default") delete POL[q.site]; else POL[q.site] = q.mode; if (q.mode === "block") { delete C[q.site]; STORAGE.delete(q.site); } return siteJson(q.site);
    case "cookies_prefs_save":
      if (q.clear_on_exit != null) EXIT = q.clear_on_exit === "1";
      if (q.persist != null) KEEP = q.persist === "1";
      return sitesJson();
    case "site_data_clear": if (q.site) { delete C[q.site]; STORAGE.delete(q.site); } else C = {}; return { ok: true, sites: [], cache_entries: 0 };
    case "cookie_site": return { ok: true, site: "example.com", host: "www.example.com", count: (C["example.com"] || []).length, here: 2, policy: POL["example.com"] || null };
    case "prefs": return { ok: true, motion: "full", effective: "full", transparency: 50, ua_mode: "firefox", onboarded: true, welcome_suppressed: true };
    case "settings": return { ok: true, search_engine: "duckduckgo", engines: [{ id: "duckduckgo", name: "DuckDuckGo" }], ask_save: false, downloads_folder: "C:\\Users\\tmj\\Downloads", downloads_folder_custom: false, picking: false, update_auto: true, theme: "time", version: "0.4.2", commit: "abc", private: false, release_repo: "x/y", update: { phase: "idle" } };
    case "site_settings": return { ok: true, suggest_web: true, sites: [] };
    case "glass": return { ok: true, mode: "off" };
    case "history": return { ok: true, history: [] };
    case "bookmarks": return { ok: true, bookmarks: [] };
    default: return { ok: true };
  }
}
const ctx = await b.newContext({ viewport: { width: 1280, height: 860 } });
await ctx.route(/^(?!https:\/\/mythbrowse\.internal)/, (r) => r.abort());
await ctx.route("https://mythbrowse.internal/**", (route) => {
  const u = new URL(route.request().url());
  if (u.pathname.startsWith("/api/")) {
    const op = u.pathname.slice(5), q = Object.fromEntries(u.searchParams); delete q.t;
    if (op !== "glass") calls.push({ op, q });
    return route.fulfill({ contentType: "application/json", body: JSON.stringify(mock(op, q)) });
  }
  const f = UI + u.pathname;
  if (!fs.existsSync(f)) return route.fulfill({ status: 404, body: "" });
  let body = fs.readFileSync(f).toString();
  const html = f.endsWith(".html"), js = f.endsWith(".js");
  if (html || js) body = body.replace(/^.*cloud_sync:begin[\s\S]*?cloud_sync:end.*$/gm, "");
  if (html) body = body.replaceAll("{{MB_TOKEN}}", "tok").replaceAll("{{MB_THEME_HOUR}}", "").replaceAll("{{MB_GLASS}}", "off")
    .replaceAll("{{MB_TRANSPARENCY}}", "50").replaceAll("{{MB_REDUCE_TRANSPARENCY}}", "0");
  return route.fulfill({ contentType: html ? "text/html" : js ? "application/javascript" : f.endsWith(".css") ? "text/css" : f.endsWith(".svg") ? "image/svg+xml" : "application/octet-stream", body });
});
async function page() {
  const p = await ctx.newPage();
  p.on("pageerror", (e) => errors.push(p.url() + ": " + e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errors.push(p.url() + ": " + m.text()); });
  return p;
}
const last = (op) => [...calls].reverse().find((c) => c.op === op);

// ---- Settings > Cookies -------------------------------------------------------------------
for (const [h, tag] of [[21, "dark"], [12, "light"]]) {
  reset();
  const p = await page();
  await p.goto(`https://mythbrowse.internal/library.html?hour=${h}`); await p.waitForTimeout(1400);
  const nav = await p.evaluate(() => {
    const n = document.getElementById("set-nav");
    const links = [...n.querySelectorAll("a")];
    const tops = new Set(links.map((a) => Math.round(a.getBoundingClientRect().top)));
    return { rows: tops.size, fits: n.scrollWidth <= n.clientWidth + 1, labels: links.map((a) => a.textContent), cookies: links.some((a) => a.getAttribute("href") === "#cookies-panel"), target: !!document.getElementById("cookies-panel") };
  });
  ok(`${tag}: Settings rail is one line and links Cookies to a real section`, nav.rows === 1 && nav.fits && nav.cookies && nav.target, nav);
  await p.locator("#set-nav").screenshot({ path: `${OUT}/settings-rail-${tag}.png` });
  await p.click('#set-nav a[href="#cookies-panel"]'); await p.waitForTimeout(400);
  const top = await p.evaluate(() => document.getElementById("cookies-panel").getBoundingClientRect().top);
  ok(`${tag}: rail Cookies jumps to the section`, top >= 0 && top < 60, top);
  const names = await p.$$eval("#ck-sites .ck-name b", (e) => e.map((x) => x.textContent));
  ok(`${tag}: sites listed (cookies, storage-only, rules)`, JSON.stringify(names) === JSON.stringify(["example.com", "github.com", "news.example.org", "tracker.test"]), names);
  ok(`${tag}: count, third-party note and persistence note shown`, (await p.textContent("#ck-count")) === "3" && /not supported yet/i.test(await p.textContent("#ck-3p")) && await p.isVisible("#ck-persist"));
  ok(`${tag}: Allow shown for github.com`, await p.evaluate(() => document.querySelector('.ck-site[data-site="github.com"] .seg-btn.on')?.textContent) === "Allow");
  await p.locator("#cookies-panel").screenshot({ path: `${OUT}/cookies-section-${tag}.png` });

  if (tag === "dark") {
    await p.fill("#ck-filter", "git"); await p.waitForTimeout(150);
    ok("search filters sites", (await p.$$eval("#ck-sites .ck-site", (e) => e.length)) === 1);
    await p.fill("#ck-filter", ""); await p.waitForTimeout(150);
    await p.click('.ck-site[data-site="example.com"] .ck-tog'); await p.waitForTimeout(400);
    const rows = await p.$$eval('.ck-site[data-site="example.com"] .ck-row:not(.ck-th)', (rs) => rs.map((r) => r.textContent));
    ok("expand lists every cookie (name, domain, expiry, flags, size)", rows.length === 3 && /sid/.test(rows.join()) && /HttpOnly/.test(rows.join()) && /End of session/.test(rows.join()) && /2\.1 KB/.test(rows.join()), rows);
    ok("domain cookies are marked with a leading dot", rows.some((r) => r.includes(".example.com")));
    ok("no Value column", !(await p.textContent('.ck-site[data-site="example.com"] .ck-th')).includes("Value"));
    await p.locator('.ck-site[data-site="example.com"]').screenshot({ path: `${OUT}/cookies-site-expanded-dark.png` });
    await p.click('.ck-site[data-site="example.com"] .ck-row:not(.ck-th) .ck-x >> nth=0'); await p.waitForTimeout(400);
    const del = last("cookie_delete");
    ok("delete one -> cookie_delete with name/domain/path", del && del.q.site === "example.com" && del.q.name === "sid" && del.q.domain === "www.example.com" && del.q.path === "/", del);
    ok("deleted row disappears", (await p.$$eval('.ck-site[data-site="example.com"] .ck-row:not(.ck-th)', (rs) => rs.length)) === 2);
    await p.click('.ck-site[data-site="tracker.test"] .seg-btn:has-text("Block")'); await p.waitForTimeout(400);
    ok("Block -> cookie_policy mode=block, cookies gone", last("cookie_policy")?.q.mode === "block" && /0 cookies/.test(await p.textContent('.ck-site[data-site="tracker.test"] .ck-name')), last("cookie_policy"));
    await p.click('.ck-site[data-site="tracker.test"] .seg-btn:has-text("Block")'); await p.waitForTimeout(400);
    ok("clicking the active policy again -> default", last("cookie_policy")?.q.mode === "default");
    await p.click("#ck-exit"); await p.waitForTimeout(300);
    ok("Clear on exit switch -> cookies_prefs_save", last("cookies_prefs_save")?.q.clear_on_exit === "1" && await p.getAttribute("#ck-exit", "aria-checked") === "true");
    // Stay signed in (v0.4.2 prep): off by default, applies after a restart.
    ok("Stay signed in switch off by default", await p.getAttribute("#ck-keep", "aria-checked") === "false");
    await p.click("#ck-keep"); await p.waitForTimeout(300);
    const keepTxt = await p.textContent("#ck-keep-val");
    ok("Stay signed in -> cookies_prefs_save persist=1, says after restart", last("cookies_prefs_save")?.q.persist === "1" && last("cookies_prefs_save")?.q.clear_on_exit == null
      && await p.getAttribute("#ck-keep", "aria-checked") === "true" && /restart/i.test(keepTxt), { q: last("cookies_prefs_save")?.q, keepTxt });
    ok("persist note still shown (this run does not keep cookies)", await p.isVisible("#ck-persist"));
    await p.click("#ck-keep"); await p.waitForTimeout(300);
    ok("Stay signed in off again", last("cookies_prefs_save")?.q.persist === "0" && await p.getAttribute("#ck-keep", "aria-checked") === "false");
    const before = calls.length;
    await p.click("#ck-clear-all"); await p.waitForTimeout(150);
    ok("Clear all asks once before clearing", !calls.slice(before).some((c) => c.op === "site_data_clear") && /Sign out/.test(await p.textContent("#ck-clear-all")));
    await p.click("#ck-clear-all"); await p.waitForTimeout(400);
    ok("second click clears all cookies", calls.slice(before).some((c) => c.op === "site_data_clear" && c.q.what === "cookies" && !c.q.site));
    await p.locator("#privacy-panel").scrollIntoViewIfNeeded();
    ok("Privacy no longer has the old site-data list", !(await p.$("#sd-list")) && await p.isVisible('#privacy-panel a[href="#cookies-panel"]'));
  }
  await p.close();
}
// Deep link from the site popover.
{
  reset();
  const p = await page();
  await p.goto("https://mythbrowse.internal/library.html?hour=12#cookies=github.com"); await p.waitForTimeout(1600);
  ok("deep link #cookies=<site> filters + expands that site", (await p.inputValue("#ck-filter")) === "github.com" && await p.isVisible('.ck-site[data-site="github.com"] .ck-table'));
  await p.close();
}

// ---- chrome: address bar + site popover ------------------------------------------------------
const state = (h) => ({
  tabs: [{ id: 2, title: "Example", url: "https://www.example.com/a", active: true, myth: null, sleeping: false, loading: false, pinned: false }],
  url: "https://www.example.com/a", title: "Example", can_go_back: true, can_go_forward: false, bookmarked: false, loading: false, internal: false,
  maximized: false, card: true, tint: null, myth: null, frame: null, motion: "full", glass: "off", transparency: 50, reduce_transparency: false, compat: null, downloads: null,
});
for (const [h, tag] of [[21, "dark"], [12, "light"]]) {
  reset();
  const p = await page();
  await p.goto(`https://mythbrowse.internal/chrome.html?hour=${h}`); await p.waitForTimeout(800);
  await p.evaluate((s) => window.mb.render(s), state(h)); await p.waitForTimeout(400);
  const bar = { x: 0, y: 0, width: 1280, height: 100 };
  await p.screenshot({ path: `${OUT}/addressbar-unfocused-${tag}.png`, clip: bar });
  await p.click("#omnibox"); await p.waitForTimeout(500);
  await p.screenshot({ path: `${OUT}/addressbar-focused-${tag}.png`, clip: bar });
  const ring = await p.evaluate(() => { const w = getComputedStyle(document.getElementById("omnibox-wrap"), "::after"); return { bw: w.borderTopWidth, style: w.borderTopStyle, op: w.opacity, shadow: w.boxShadow }; });
  ok(`${tag}: focused address bar ring is a 2px border (no box-shadow)`, ring.bw === "2px" && ring.style === "solid" && ring.op === "1" && ring.shadow === "none", ring);
  await p.evaluate(() => document.activeElement.blur()); await p.keyboard.press("Escape"); await p.waitForTimeout(300);
  ok(`${tag}: site icon is a button on web pages`, (await p.getAttribute("#lead", "role")) === "button");
  await p.click("#lead"); await p.waitForTimeout(500);
  ok(`${tag}: site popover opens with the cookie count`, await p.isVisible("#site-pop") && /3 cookies from example\.com/.test(await p.textContent("#site-pop")), await p.textContent("#site-pop"));
  ok(`${tag}: popover reports its rectangle to the shell`, calls.some((c) => c.op === "popup" && c.q.w && !c.q.clear));
  await p.screenshot({ path: `${OUT}/site-popover-${tag}.png`, clip: { x: 0, y: 0, width: 700, height: 340 } });
  if (tag === "dark") {
    await p.click('#site-pop .sp-opt:has-text("Clear on exit")'); await p.waitForTimeout(400);
    ok("popover policy -> cookie_policy clear_exit", last("cookie_policy")?.q.mode === "clear_exit" && /on/.test(await p.getAttribute('#site-pop .sp-opt:has-text("Clear on exit")', "class")));
    await p.click('#site-pop .sh-btn:has-text("Manage")'); await p.waitForTimeout(300);
    ok("Manage -> Settings > Cookies for that site", last("navigate")?.q.input === "https://mythbrowse.internal/library.html#cookies=example.com" && !(await p.isVisible("#site-pop")), last("navigate"));
    await p.click("#lead"); await p.waitForTimeout(300);
    await p.keyboard.press("Escape"); await p.waitForTimeout(200);
    ok("Esc closes the popover", !(await p.isVisible("#site-pop")));
    await p.evaluate((s) => window.mb.render({ ...s, url: "https://mythbrowse.internal/home.html", internal: true }), state(h)); await p.waitForTimeout(200);
    ok("no site popover on internal pages", (await p.getAttribute("#lead", "role")) === null);
  }
  await p.close();
}

await b.close();
const res = { pass: checks.filter((c) => c.pass).length, total: checks.length, checks, errors };
fs.writeFileSync(`${OUT}/cookies_smoke.json`, JSON.stringify(res, null, 2));
console.log(`${res.pass}/${res.total}`);
checks.filter((c) => !c.pass).forEach((c) => console.log("FAIL", c.name, String(JSON.stringify(c.got)).slice(0, 600)));
errors.slice(0, 12).forEach((e) => console.log("ERR", e));
process.exit(res.pass === res.total && !errors.length ? 0 : 1);
