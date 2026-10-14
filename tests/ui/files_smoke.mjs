// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// feat-files UI smoke (headless Chromium, NOT Servo): serves feat-files/ui with the shell's
// placeholders filled, mocks /api/*, and drives the Downloads page, the toolbar Downloads
// button + tab menu (pin) in chrome.html, and Settings > On startup in library.html.
// Usage: node ui_smoke.mjs  (writes shots/*.png, ui-smoke.json)
const { chromium } = await import(process.env.PW + "/index.mjs");
import fs from "node:fs";
const exe = process.env.CHROME;
const W = process.env.MYTHBROWSE_WORK || process.cwd();
const UI = process.env.MB_UI_DIR || new URL("../../ui", import.meta.url).pathname;
const OUT = process.env.MB_SMOKE_OUT || W + "/ff-smoke";
fs.mkdirSync(OUT + "/shots", { recursive: true });
const b = await chromium.launch({ executablePath: exe });
const calls = [], errors = [], checks = [];
const ok = (name, cond, got) => checks.push({ name, pass: !!cond, got });

const items = [
  { id: 4, url: "https://ex.com/setup.exe", final_url: "https://ex.com/setup.exe", name: "setup.exe", path: "/dl/setup.exe", mime: "application/x-msdownload", total: 5000000, received: 0, state: "confirm", error: "", danger: true, confirmed: false, resumable: true, started_ms: Date.now(), finished_ms: 0, speed: 0, private: false, by_agent: true, warning: "setup.exe is a program. Keep it only if you trust ex.com." },
  { id: 3, url: "https://ex.com/big.zip", name: "big.zip", path: "/dl/big.zip", total: 10485760, received: 4194304, state: "running", error: "", danger: false, speed: 1048576, started_ms: Date.now(), by_agent: false, warning: "" },
  { id: 2, url: "https://ex.com/paper.pdf", final_url: "https://ex.com/paper.pdf", name: "paper.pdf", path: "/dl/paper.pdf", total: 204800, received: 204800, state: "done", danger: false, speed: 0, started_ms: Date.now() - 60000, warning: "" },
  { id: 1, url: "https://ex.com/gone.iso", name: "gone.iso", path: "", total: null, received: 0, state: "failed", error: "The server says the file was not found (404).", danger: false, warning: "" },
];
let startup = "continue";
function api(op, q) {
  if (op === "downloads") return { ok: true, downloads: items, folder: "C:\\Users\\me\\Downloads" };
  if (op === "download_action") return { ok: true, downloads: items };
  if (op === "downloads_clear") return { ok: true, downloads: items.filter((i) => !["done", "failed", "cancelled"].includes(i.state)) };
  if (op === "startup_pref") return { ok: true, mode: startup };
  if (op === "startup_pref_save") { startup = q.mode; return { ok: true, mode: startup }; }
  if (op === "prefs") return { ok: true, motion: "system", effective: "full", system_reduced: false };
  if (op === "ai_settings") return { ok: true, kind: "ollama", model: "qwen3:8b", configured: true };
  if (op === "history") return { ok: true, history: [] };
  if (op === "bookmarks") return { ok: true, bookmarks: [] };
  if (op === "chat_list") return { ok: true, groups: [] };
  if (op === "chat_settings") return { ok: true, keep: "forever" };
  if (op === "glass") return { ok: true, mode: "blur" };
  return { ok: true };
}
const ctx = await b.newContext({ viewport: { width: 1280, height: 860 } });
await ctx.route("https://mythbrowse.internal/**", (route) => {
  const u = new URL(route.request().url());
  if (u.pathname.startsWith("/api/")) {
    const op = u.pathname.slice(5), q = Object.fromEntries(u.searchParams); delete q.t;
    if (op !== "glass") calls.push({ op, q });
    return route.fulfill({ contentType: "application/json", body: JSON.stringify(api(op, q)) });
  }
  const f = UI + u.pathname;
  if (!fs.existsSync(f)) return route.fulfill({ status: 404, body: "" });
  let body = fs.readFileSync(f).toString();
  const html = f.endsWith(".html"), js = f.endsWith(".js");
  // The Store build serves no cloud_sync sections (as the shell cuts them).
  body = body.replace(/<!-- cloud_sync:begin[\s\S]*?cloud_sync:end -->/g, "").replace(/\/\/ cloud_sync:begin[\s\S]*?\/\/ cloud_sync:end/g, "");
  if (html) body = body.replaceAll("{{MB_TOKEN}}", "tok").replaceAll("{{MB_THEME_HOUR}}", "11").replaceAll("{{MB_GLASS}}", "off")
    .replaceAll("{{MB_TRANSPARENCY}}", "50").replaceAll("{{MB_REDUCE_TRANSPARENCY}}", "0");
  return route.fulfill({ contentType: html ? "text/html" : js ? "application/javascript" : f.endsWith(".css") ? "text/css" : "application/octet-stream", body });
});
const page = async () => {
  const p = await ctx.newPage();
  p.on("pageerror", (e) => errors.push(p.url() + ": " + e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/404/.test(m.text())) errors.push(p.url() + ": " + m.text()); });
  return p;
};

// ---------------------------------------------------------------- Downloads page
{
  const p = await page();
  await p.goto("https://mythbrowse.internal/downloads.html");
  await p.waitForTimeout(800);
  ok("downloads: four rows", (await p.locator(".dl").count()) === 4, await p.locator(".dl").count());
  ok("downloads: folder shown", (await p.textContent("#where")).includes("Downloads"), await p.textContent("#where"));
  ok("downloads: count says 1 in progress", (await p.textContent("#count")) === "1 in progress", await p.textContent("#count"));
  const confirm = p.locator('.dl[data-id="4"]');
  ok("confirm row: warning text", (await confirm.locator(".warn").textContent()).includes("setup.exe is a program"), null);
  ok("confirm row: Discard is the primary (safe) button, Keep is not", (await confirm.locator('[data-op="discard"]').getAttribute("class")) === "primary" && (await confirm.locator('[data-op="keep"]').getAttribute("class")) === "ghost", null);
  ok("confirm row: marked by Myth", (await confirm.textContent()).includes("by Myth"), null);
  const bar = await p.locator('.dl[data-id="3"] .bar > i').evaluate((e) => e.style.width);
  ok("running row: progress 40%", parseFloat(bar) === 40, bar);
  ok("running row: speed + time left", /1\.0 MB\/s/.test(await p.textContent('.dl[data-id="3"] .sub')) && /s left/.test(await p.textContent('.dl[data-id="3"] .sub')), await p.textContent('.dl[data-id="3"] .sub'));
  ok("failed row: plain-words error + Retry", (await p.textContent('.dl[data-id="1"] .sub')).includes("404") && (await p.locator('.dl[data-id="1"] [data-op="retry"]').count()) === 1, null);
  await p.screenshot({ path: OUT + "/shots/01-downloads.png" });
  await confirm.locator('[data-op="keep"]').click(); await p.waitForTimeout(200);
  ok("Keep -> download_action?op=keep&id=4", calls.some((c) => c.op === "download_action" && c.q.op === "keep" && c.q.id === "4"), null);
  await p.locator('.dl[data-id="3"] [data-op="pause"]').click(); await p.waitForTimeout(200);
  ok("Pause -> op=pause id=3", calls.some((c) => c.op === "download_action" && c.q.op === "pause" && c.q.id === "3"), null);
  await p.locator('.dl[data-id="2"] .name').click(); await p.waitForTimeout(200);
  ok("click finished name -> op=open (no confirm needed)", calls.some((c) => c.op === "download_action" && c.q.op === "open" && c.q.id === "2" && !c.q.confirm), null);
  // Shell push: the dangerous file is now done; opening it asks first.
  await p.evaluate(() => window.mbDownloads([{ id: 4, url: "https://ex.com/setup.exe", name: "setup.exe", path: "/dl/setup.exe", total: 5000000, received: 5000000, state: "done", danger: true, warning: "" }]));
  await p.waitForTimeout(150);
  ok("mbDownloads push re-renders (1 row, done)", (await p.locator(".dl").count()) === 1 && (await p.locator(".dl.done").count()) === 1, null);
  let asked = "";
  p.once("dialog", (d) => { asked = d.message(); d.dismiss(); });
  await p.locator(".dl .name").click(); await p.waitForTimeout(200);
  ok("opening a program asks first; dismiss sends nothing", asked.includes("can run code") && !calls.some((c) => c.op === "download_action" && c.q.id === "4" && c.q.op === "open"), asked);
  p.once("dialog", (d) => d.accept());
  await p.locator(".dl .name").click(); await p.waitForTimeout(200);
  ok("accept -> op=open&confirm=1", calls.some((c) => c.op === "download_action" && c.q.id === "4" && c.q.op === "open" && c.q.confirm === "1"), null);
  await p.click("#folder"); await p.waitForTimeout(150);
  ok("Open folder -> downloads_folder", calls.some((c) => c.op === "downloads_folder"), null);
  await p.evaluate(() => window.mbDownloads([]));
  ok("empty list shows the empty note", await p.isVisible("#empty"), null);
  await p.close();
}

// ---------------------------------------------------------------- chrome: button + pin
{
  const p = await page();
  await p.goto("https://mythbrowse.internal/chrome.html");
  await p.waitForTimeout(800);
  const state = (dl, pinned) => ({
    tabs: [
      { id: 1, title: "Docs", url: "https://docs.rs/", active: false, myth: null, sleeping: false, loading: false, pinned: pinned },
      { id: 2, title: "Example", url: "https://example.com/", active: true, myth: null, sleeping: false, loading: false, pinned: false },
    ],
    url: "https://example.com/", can_go_back: true, can_go_forward: false, bookmarked: false, loading: false, internal: false,
    maximized: false, card: true, tint: null, myth: null, frame: null, motion: "off", glass: "off", transparency: 50,
    reduce_transparency: false, compat: null, downloads: dl,
  });
  await p.evaluate((s) => window.mb.render(s), state(null, false));
  await p.waitForTimeout(200);
  ok("chrome: no Downloads button before any download", await p.isHidden("#downloads"), null);
  await p.evaluate((s) => window.mb.render(s), state({ active: 1, frac: 0.25, attention: false }, false));
  await p.waitForTimeout(350);
  ok("chrome: button shows with a progress bar at 25%", (await p.isVisible("#downloads")) && (await p.locator("#dl-bar b").evaluate((e) => e.style.transform)) === "scaleX(0.25)", await p.locator("#dl-bar b").evaluate((e) => e.style.transform));
  await p.evaluate((s) => window.mb.render(s), state({ active: 1, frac: null, attention: true }, false));
  await p.waitForTimeout(200);
  ok("chrome: attention dot + unknown size = indeterminate", (await p.isVisible("#dl-dot")) && (await p.locator("#dl-bar").getAttribute("class")).includes("indet"), null);
  await p.screenshot({ path: OUT + "/shots/02-chrome-downloads-button.png", clip: { x: 0, y: 0, width: 1280, height: 110 } });
  await p.click("#downloads"); await p.waitForTimeout(150);
  ok("chrome: button -> open_downloads", calls.some((c) => c.op === "open_downloads"), null);
  const tab1 = p.locator(".tab").first();
  await tab1.click({ button: "right" }); await p.waitForTimeout(200);
  ok("chrome: right-click opens the tab menu with Pin tab", (await p.isVisible("#ctx-menu")) && (await p.locator("#ctx-menu .menu-label", { hasText: /^Pin tab$/ }).count()) === 1, null);
  await p.screenshot({ path: OUT + "/shots/03-chrome-tab-menu.png", clip: { x: 0, y: 0, width: 1280, height: 380 } });
  await p.locator("#ctx-menu .menu-item", { hasText: /^Pin tab$/ }).click(); await p.waitForTimeout(150);
  ok("chrome: Pin tab -> pin_tab?id=1&on=1, menu closes", calls.some((c) => c.op === "pin_tab" && c.q.id === "1" && c.q.on === "1") && (await p.isHidden("#ctx-menu")), null);
  await p.evaluate((s) => window.mb.render(s), state(null, true));
  await p.waitForTimeout(300);
  const w = await tab1.evaluate((e) => e.getBoundingClientRect().width);
  ok("chrome: pinned tab is icon-only (40px, no title)", Math.abs(w - 40) < 1.5 && (await tab1.locator(".title").isHidden()), w);
  await tab1.click({ button: "right" }); await p.waitForTimeout(150);
  ok("chrome: menu on a pinned tab says Unpin tab", (await p.locator("#ctx-menu .menu-label", { hasText: /^Unpin tab$/ }).count()) === 1, null);
  await p.keyboard.press("Escape"); await p.waitForTimeout(100);
  ok("chrome: Esc closes the tab menu", await p.isHidden("#ctx-menu"), null);
  await p.screenshot({ path: OUT + "/shots/04-chrome-pinned.png", clip: { x: 0, y: 0, width: 1280, height: 110 } });
  await p.close();
}

// ---------------------------------------------------------------- Settings: On startup
{
  const p = await page();
  await p.goto("https://mythbrowse.internal/library.html");
  await p.waitForTimeout(900);
  ok("settings: On startup shows Continue selected (default)", (await p.getAttribute('#startup-seg [data-startup="continue"]', "class")).includes("on"), null);
  await p.click('#startup-seg [data-startup="newtab"]'); await p.waitForTimeout(250);
  ok("settings: New tab -> startup_pref_save?mode=newtab and selected", calls.some((c) => c.op === "startup_pref_save" && c.q.mode === "newtab") && (await p.getAttribute('#startup-seg [data-startup="newtab"]', "class")).includes("on"), null);
  await p.locator("#general-panel").screenshot({ path: OUT + "/shots/05-settings-on-startup.png" });
  await p.close();
}

ok("no JS errors", errors.length === 0, errors);
await b.close();
const fail = checks.filter((c) => !c.pass);
fs.writeFileSync(`${OUT}/ui-smoke.json`, JSON.stringify({ checks, calls: calls.map((c) => c.op + "?" + new URLSearchParams(c.q)), errors }, null, 1));
for (const c of checks) console.log((c.pass ? "PASS " : "FAIL ") + c.name + (c.pass ? "" : "  got " + JSON.stringify(c.got)));
console.log(fail.length ? `UI SMOKE FAIL (${fail.length}/${checks.length})` : `UI SMOKE PASS (${checks.length})`);
process.exit(fail.length ? 1 : 0);
