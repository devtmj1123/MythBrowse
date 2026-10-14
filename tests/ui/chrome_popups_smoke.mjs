// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// UI smoke for the chrome popups (headless Chromium, NOT Servo): serves ui/ with the shell's
// placeholders filled, mocks /api/*, drives chrome.html through window.mb.render() like the
// shell does: suggestions dropdown (keyboard nav, inline autocomplete, Shift+Delete), the
// shell's context menu, the tab strip menu, alert/confirm/prompt/beforeunload sheets, a
// permission sheet (Enter never allows), the pop-up indicator, Snap rect reporting and
// Settings > Privacy & site settings.
//   PW=<playwright-core dir> CHROME=<chrome-headless-shell> OUT=<dir> [HOUR=21] node chrome_popups_smoke.mjs
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
const { chromium } = await import(path.join(process.env.PW, "index.mjs"));
const exe = process.env.CHROME;
const UI = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../ui");
const OUT = process.env.OUT || "/tmp/chrome-popups-smoke";
const hour = process.env.HOUR || "11";
const SH = OUT + (hour === "11" ? "/shots" : "/shots-h" + hour);
fs.mkdirSync(SH, { recursive: true });
const b = await chromium.launch({ executablePath: exe });
const calls = [], errors = [], checks = [];
const ok = (name, cond, got) => checks.push({ name, pass: !!cond, got });
const ctx = await b.newContext({ viewport: { width: 1280, height: 800 } });
const NOW = Date.now();
const SUG = [
  { kind: "bookmark", title: "GitHub", url: "https://github.com/", input: "https://github.com/", deletable: false },
  { kind: "tab", title: "servo/servo: Servo, the embeddable web engine", url: "https://github.com/servo/servo", input: "https://github.com/servo/servo", tab: 7, deletable: false },
  { kind: "history", title: "GitLab", url: "https://gitlab.com/", input: "https://gitlab.com/", deletable: true },
  { kind: "search", title: "git", url: "https://duckduckgo.com/?q=git", input: "? git", deletable: false },
  { kind: "search", title: "github copilot", url: "https://duckduckgo.com/?q=github+copilot", input: "? github copilot", deletable: false },
  { kind: "ask_myth", title: "git", url: "", input: "git", deletable: false },
];
await ctx.route(/^(?!https:\/\/mythbrowse\.internal)/, (r) => r.abort());
await ctx.route("https://mythbrowse.internal/**", (route) => {
  const u = new URL(route.request().url());
  if (u.pathname.startsWith("/api/")) {
    const op = u.pathname.slice(5), q = Object.fromEntries(u.searchParams); delete q.t;
    if (op !== "glass") calls.push({ op, q });
    let body = { ok: true };
    if (op === "suggest") body = { ok: true, seq: Number(q.seq), q: q.q, items: q.q ? SUG : [], inline: q.q === "git" ? "github.com/" : null, web: true, pending: false };
    if (op === "site_settings") body = { ok: true, suggest_web: true, features: [], sites: [
      { origin: "https://maps.example.com", feature: "geolocation", decision: "allow", updated_ms: NOW },
      { origin: "https://maps.example.com", feature: "notifications", decision: "block", updated_ms: NOW },
      { origin: "https://news.example.org", feature: "popups", decision: "allow", updated_ms: NOW },
    ] };
    if (op === "history") body = { ok: true, history: [] };
    if (op === "bookmarks") body = { ok: true, bookmarks: [] };
    if (op === "prefs") body = { ok: true, motion: "system", effective: "full", system_reduced: false, transparency: 50, reduce_transparency: "system", reduce_transparency_effective: false, system_reduced_transparency: false };
    if (op === "chat_list") body = { ok: true, groups: [] };
    if (op === "chat_settings") body = { ok: true, keep: "forever" };
    if (op === "ai_settings") body = { ok: true, kind: "ollama", base_url: "http://localhost:11434", model: "qwen3:8b", has_key: false, configured: true };
    if (op === "glass") body = { ok: true, mode: "blur" };
    return route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  }
  const f = UI + u.pathname;
  if (!fs.existsSync(f)) return route.fulfill({ status: 404, body: "" });
  let body = fs.readFileSync(f).toString();
  const html = f.endsWith(".html"), js = f.endsWith(".js");
  // The default (Store) build cuts the cloud_sync blocks out, as chrome::asset does.
  if (html || js) body = body.replace(/^.*cloud_sync:begin[\s\S]*?cloud_sync:end.*$/gm, "");
  if (html) body = body.replaceAll("{{MB_TOKEN}}", "tok").replaceAll("{{MB_THEME_HOUR}}", hour).replaceAll("{{MB_GLASS}}", "off")
    .replaceAll("{{MB_TRANSPARENCY}}", "50").replaceAll("{{MB_REDUCE_TRANSPARENCY}}", "0");
  return route.fulfill({ contentType: html ? "text/html" : js ? "application/javascript" : f.endsWith(".css") ? "text/css" : "application/octet-stream", body });
});
const p = await ctx.newPage();
p.on("pageerror", (e) => errors.push(e.message));
const base = {
  tabs: [
    { id: 1, title: "Servo — The embeddable web engine", url: "https://servo.org/", active: true, myth: null, sleeping: false, loading: false },
    { id: 7, title: "servo/servo", url: "https://github.com/servo/servo", active: false, myth: null, sleeping: false, loading: false },
    { id: 9, title: "Maps", url: "https://maps.example.com/", active: false, myth: null, sleeping: false, loading: false },
  ],
  url: "https://servo.org/", can_go_back: true, can_go_forward: false, bookmarked: false, loading: false, internal: false,
  maximized: false, card: true, tint: null, myth: null, frame: { gap: 8, overlay: null, panel: null },
  motion: "off", glass: "off", transparency: 50, reduce_transparency: false, compat: null,
  dialog: null, menu: null, popups_blocked: null, suggest_web: true,
};
await p.goto("https://mythbrowse.internal/chrome.html", { waitUntil: "domcontentloaded" });
await p.waitForTimeout(800);
// A stand-in page under the card (the shell composes the real one; Chromium shows the sky).
await p.addStyleTag({ content: `body::after{content:"";position:fixed;left:8px;right:8px;top:96px;bottom:8px;border-radius:14px;background:#fff linear-gradient(#f4f6fa 0 64px,#fff 64px);z-index:0;pointer-events:none}` });
const render = (s) => p.evaluate((s) => window.mb.render(s), s);
const wait = (ms) => p.waitForTimeout(ms);
await render(base); await wait(300);
const st = () => p.evaluate(() => window.mbPopups._state());
const shot = (name) => p.screenshot({ path: `${SH}/${name}.png` });
const last = (op) => calls.filter((c) => c.op === op).slice(-1)[0];

// ---- 1. suggestions
await p.click("#omnibox");
await p.keyboard.press("Control+A"); await p.keyboard.press("Backspace");
await p.keyboard.type("git", { delay: 30 }); await wait(400);
ok("suggest RPC sent with the typed text", calls.some((c) => c.op === "suggest" && c.q.q === "git"));
ok("dropdown shows 6 rows", (await p.$$eval("#suggest .sug-row", (r) => r.length)) === 6);
const v = await p.evaluate(() => { const o = document.getElementById("omnibox"); return [o.value, o.selectionStart, o.selectionEnd]; });
ok("inline autocomplete: git|hub.com/ selected", v[0] === "github.com/" && v[1] === 3 && v[2] === 11, v);
ok("tab row says Switch to tab", (await p.textContent("#suggest .sug-row.k-tab .sug-hint")) === "Switch to tab");
const tabTitleW = await p.$eval("#suggest .sug-row.k-tab .sug-title", (e) => e.getBoundingClientRect().width);
ok("tab row title is not squeezed by tab-strip styles", tabTitleW > 200, tabTitleW);
ok("popup rect reported for the dropdown", calls.some((c) => c.op === "popup" && Number(c.q.h) > 100));
for (let i = 0; i < 3; i++) await p.keyboard.press("ArrowDown");
ok("ArrowDown x3 selects the history row and previews its URL", (await st()).sel === 2 && (await p.inputValue("#omnibox")) === "https://gitlab.com/");
await shot("01-suggest-dropdown");
await p.keyboard.press("Shift+Delete"); await wait(200);
ok("Shift+Delete removes the history row", calls.some((c) => c.op === "suggest_delete" && c.q.url === "https://gitlab.com/"));
await p.keyboard.press("ArrowUp"); await p.keyboard.press("Escape"); await wait(150);
ok("Esc closes the dropdown and drops the completion", (await p.isHidden("#suggest")) && (await p.inputValue("#omnibox")) === "git");
await p.keyboard.type("h", { delay: 20 }); await wait(250);
await p.keyboard.press("Backspace"); await wait(300);
ok("Backspace does not re-complete", (await p.inputValue("#omnibox")) === "git", await p.inputValue("#omnibox"));
await p.keyboard.press("ArrowDown"); await p.keyboard.press("ArrowDown"); await p.keyboard.press("Enter"); await wait(250);
ok("Enter on the tab row switches to tab 7", calls.some((c) => c.op === "activate_tab" && c.q.id === "7"));
ok("popup cleared after choosing", last("popup")?.q.clear === "1");
await p.click("#omnibox"); await p.keyboard.press("Control+A"); await p.keyboard.type("git", { delay: 20 }); await wait(300);
await p.click("#suggest .sug-row.k-ask_myth"); await wait(100);
ok("clicking Ask Myth navigates with the plain text", calls.some((c) => c.op === "navigate" && c.q.input === "git"));

// ---- 2. page context menu from the shell
await render({ ...base, menu: { id: 4, x: 520, y: 300, items: [
  { id: "open_link_new_tab", label: "Open link in new tab", enabled: true },
  { id: "copy_link", label: "Copy link address", enabled: true },
  { id: "ask_myth_link", label: "Ask Myth about this link", enabled: true },
  { id: "-", label: "", enabled: false },
  { id: "copy", label: "Copy", enabled: true, key: "Ctrl+C" },
  { id: "-", label: "", enabled: false },
  { id: "search_selection", label: "Search the web for \u201cembeddable web engine\u201d", enabled: true },
  { id: "ask_myth_selection", label: "Ask Myth about this", enabled: true },
] } });
await wait(250);
ok("context menu visible", await p.isVisible("#ctx-menu"));
const mr = await p.$eval("#ctx-menu", (e) => { const r = e.getBoundingClientRect(); return [Math.round(r.left), Math.round(r.top)]; });
ok("menu placed at x=520,y=300", mr[0] === 520 && mr[1] === 300, mr);
await p.keyboard.press("ArrowDown"); await p.keyboard.press("ArrowDown");
await shot("02-context-menu");
await p.keyboard.press("Enter"); await wait(150);
ok("Enter picks copy_link", calls.some((c) => c.op === "menu_pick" && c.q.id === "4" && c.q.action === "copy_link"));
await render(base);
await render({ ...base, menu: { id: 5, x: 1200, y: 760, items: [{ id: "back", label: "Back", enabled: true, key: "Alt+Left" }, { id: "forward", label: "Forward", enabled: false, key: "Alt+Right" }, { id: "reload", label: "Reload", enabled: true, key: "F5" }, { id: "-" }, { id: "view_source", label: "View page source", enabled: true }] } });
await wait(200);
const mr2 = await p.$eval("#ctx-menu", (e) => { const r = e.getBoundingClientRect(); return [r.right, r.bottom]; });
ok("menu near the corner flips inside the window", mr2[0] <= 1280 && mr2[1] <= 800, mr2);
await shot("03-context-menu-page");
await p.keyboard.press("Escape"); await wait(150);
ok("Esc dismisses (menu_pick id=5, no action)", calls.some((c) => c.op === "menu_pick" && c.q.id === "5" && !c.q.action));
await render(base);

// ---- 3. tab strip menu
const tabBox = await p.$eval(".tab:nth-child(2)", (e) => { const r = e.getBoundingClientRect(); return [r.left + 30, r.top + 12]; }).catch(() => null);
if (tabBox) {
  await p.mouse.click(tabBox[0], tabBox[1], { button: "right" }); await wait(200);
  ok("tab strip menu opens", await p.isVisible("#ctx-menu"));
  await shot("04-tab-menu");
  await p.click("#ctx-menu .menu-item:has-text('Close tabs to the right')"); await wait(100);
  ok("Close tabs to the right sent", calls.some((c) => c.op === "tab_menu" && c.q.action === "close_right"));
} else ok("tab strip menu opens", false, "no .tab");

// ---- 4. dialogs
await render({ ...base, dialog: { id: 11, kind: { type: "confirm" }, message: "Discard the draft you were writing?\nThis cannot be undone.", origin: "https://mail.example.com", suppress: true, label: "" } });
await wait(300);
ok("confirm sheet visible", await p.isVisible("#page-sheet"));
ok("sheet names the site in bold", (await p.textContent("#page-sheet .sh-head b")) === "mail.example.com");
ok("suppress checkbox offered", await p.isVisible("#page-sheet .sh-check"));
await shot("05-dialog-confirm");
await p.check("#page-sheet .sh-check input");
await p.click("#page-sheet .sh-btn.primary"); await wait(150);
ok("OK + suppress -> dialog_answer ok=1 suppress=1", calls.some((c) => c.op === "dialog_answer" && c.q.id === "11" && c.q.ok === "1" && c.q.suppress === "1"));
await render(base);
await render({ ...base, dialog: { id: 12, kind: { type: "prompt", default: "Untitled" }, message: "Name this board:", origin: "https://board.example.com", suppress: false, label: "" } });
await wait(250);
ok("prompt input focused with default", (await p.evaluate(() => document.activeElement && document.activeElement.value)) === "Untitled");
await p.keyboard.type("Trip 2026");
await shot("06-dialog-prompt");
await p.keyboard.press("Enter"); await wait(150);
ok("Enter submits the prompt value", calls.some((c) => c.op === "dialog_answer" && c.q.id === "12" && c.q.ok === "1" && c.q.value === "Trip 2026"));
await render(base);
await render({ ...base, dialog: { id: 13, kind: { type: "before_unload" }, message: "", origin: "https://docs.example.com", suppress: false, label: "" } });
await wait(200);
ok("Leave site? sheet shows Stay / Leave", (await p.$$eval("#page-sheet .sh-btn", (b) => b.map((x) => x.textContent))).join("|") === "Stay|Leave");
await p.keyboard.press("Escape"); await wait(150);
ok("Esc = Stay (ok=0)", calls.some((c) => c.op === "dialog_answer" && c.q.id === "13" && c.q.ok === "0"));
await render(base);

// ---- 5. permission sheet
await render({ ...base, dialog: { id: 14, kind: { type: "permission", feature: "geolocation" }, message: "", origin: "https://maps.example.com", suppress: false, label: "know your location" } });
await wait(300);
ok("permission sheet text", (await p.textContent("#page-sheet .sh-who")).includes("maps.example.com wants to know your location"));
ok("Remember for this site on by default", await p.isChecked("#page-sheet .sh-check input"));
ok("focus is on Block (the safe button)", (await p.evaluate(() => document.activeElement.textContent)) === "Block");
await shot("07-permission-sheet");
await p.keyboard.press("Enter"); await wait(150);
ok("Enter never allows: ok=0 remember=1", calls.some((c) => c.op === "dialog_answer" && c.q.id === "14" && c.q.ok === "0" && c.q.remember === "1"));
await render(base);

// ---- 6. pop-up indicator
await render({ ...base, popups_blocked: { count: 2, origin: "https://news.example.org" } });
await wait(150);
ok("pop-up indicator visible", await p.isVisible("#popups"));
await p.click("#popups"); await wait(200);
await shot("08-popups-blocked");
await p.click("#ctx-menu .menu-item:has-text('Always allow')"); await wait(100);
ok("Always allow -> popups?always=1", calls.some((c) => c.op === "popups" && c.q.always === "1"));
await render(base);

// ---- 7. snap layouts rect
ok("max button rect reported", calls.some((c) => c.op === "max_button" && Number(c.q.w) > 30));
await p.evaluate(() => window.mb.snapHover(true));
ok("snap hover class from the shell", await p.$eval("#win-max", (e) => e.classList.contains("snap-hover")));
ok("no page errors", errors.length === 0, errors);

// ---- 8. Settings > Privacy & site settings
const p2 = await ctx.newPage();
p2.on("pageerror", (e) => errors.push("library: " + e.message));
await p2.goto("https://mythbrowse.internal/library.html", { waitUntil: "domcontentloaded" });
await p2.waitForTimeout(900);
ok("site settings lists 2 sites", (await p2.$$eval("#sites .ss-site", (r) => r.length)) === 2);
await p2.$eval("#sites", (e) => e.scrollIntoView());
await p2.waitForTimeout(200);
await p2.screenshot({ path: `${SH}/09-site-settings.png` });
await p2.locator("#sites .ss-site").first().locator(".ss-row").first().locator(".seg-btn", { hasText: "Ask" }).click();
await p2.click("#suggest-web"); await p2.waitForTimeout(200);
ok("Ask -> site_setting_set value=ask", calls.some((c) => c.op === "site_setting_set" && c.q.value === "ask" && c.q.feature === "geolocation"));
ok("suggestions switch -> suggest_pref on=0", calls.some((c) => c.op === "suggest_pref" && c.q.on === "0"));
ok("library: no page errors", !errors.some((e) => e.startsWith("library")), errors);

await b.close();
const pass = checks.filter((c) => c.pass).length;
fs.writeFileSync(`${OUT}/ui-smoke${hour === "11" ? "" : "-h" + hour}.json`, JSON.stringify({ pass, total: checks.length, checks, errors }, null, 1));
console.log(`${pass}/${checks.length}`);
for (const c of checks) if (!c.pass) console.log("FAIL", c.name, JSON.stringify(c.got));
process.exit(pass === checks.length ? 0 : 1);
