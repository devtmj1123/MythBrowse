// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// UI smoke for v0.4.2 round 3 browsing features (headless Chromium, NOT Servo):
//  A. chrome.html driven through window.mb.render() like the shell: the address-bar zoom
//     indicator (click = reset), the find bar (typing, Enter / Shift+Enter, Esc, counts,
//     "No results"), the one-shot note (toast) and the fullscreen class.
//  B. The page script's find (crates/engine/src/js/page_input.js, window.__mbFind /
//     __mbFindClear) on a fixture page: counts, next / previous wrap, hidden and <script> text
//     not counted, the overlay painted and removed, and external-scheme link clicks reported
//     on the console channel (mailto:, zoommtg:) while http links are left alone.
//   PW=<playwright-core dir> CHROME=<chrome-headless-shell> OUT=<dir> node browse_smoke.mjs
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
const { chromium } = await import(path.join(process.env.PW, "index.mjs"));
const exe = process.env.CHROME;
const HERE = path.dirname(fileURLToPath(import.meta.url));
const UI = process.env.MB_UI || path.resolve(HERE, "../../ui");
const PAGE_INPUT = path.resolve(HERE, "../../crates/engine/src/js/page_input.js");
const OUT = process.env.OUT || "/tmp/browse-smoke";
fs.mkdirSync(OUT, { recursive: true });
const b = await chromium.launch({ executablePath: exe });
const calls = [], errors = [], checks = [];
const ok = (name, cond, got) => checks.push({ name, pass: !!cond, got });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------- A. chrome
async function chromePage(hour) {
  const ctx = await b.newContext({ viewport: { width: 1280, height: 800 } });
  await ctx.route(/^(?!https:\/\/mythbrowse\.internal)/, (r) => r.abort());
  await ctx.route("https://mythbrowse.internal/**", (route) => {
    const u = new URL(route.request().url());
    if (u.pathname.startsWith("/api/")) {
      const op = u.pathname.slice(5), q = Object.fromEntries(u.searchParams); delete q.t;
      if (op !== "glass") calls.push({ op, q });
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: true, mode: "off" }) });
    }
    const f = UI + u.pathname;
    if (!fs.existsSync(f)) return route.fulfill({ status: 404, body: "" });
    let body = fs.readFileSync(f).toString();
    const html = f.endsWith(".html"), js = f.endsWith(".js");
    if (html || js) body = body.replace(/^.*cloud_sync:begin[\s\S]*?cloud_sync:end.*$/gm, "");
    if (html) body = body.replaceAll("{{MB_TOKEN}}", "tok").replaceAll("{{MB_THEME_HOUR}}", hour).replaceAll("{{MB_GLASS}}", "off")
      .replaceAll("{{MB_TRANSPARENCY}}", "50").replaceAll("{{MB_REDUCE_TRANSPARENCY}}", "0");
    return route.fulfill({ contentType: html ? "text/html" : js ? "application/javascript" : f.endsWith(".css") ? "text/css" : "application/octet-stream", body });
  });
  const p = await ctx.newPage();
  p.on("pageerror", (e) => errors.push("chrome: " + e.message));
  await p.goto("https://mythbrowse.internal/chrome.html", { waitUntil: "domcontentloaded" });
  await p.waitForTimeout(800);
  await p.addStyleTag({ content: `body::after{content:"";position:fixed;left:8px;right:8px;top:96px;bottom:8px;border-radius:14px;background:#fff linear-gradient(#f4f6fa 0 64px,#fff 64px);z-index:0;pointer-events:none}` });
  return { ctx, p };
}
const base = {
  tabs: [
    { id: 1, title: "Servo — The embeddable web engine", url: "https://servo.org/", active: true, myth: null, sleeping: false, loading: false },
    { id: 7, title: "servo/servo", url: "https://github.com/servo/servo", active: false, myth: null, sleeping: false, loading: false },
  ],
  url: "https://servo.org/", can_go_back: true, can_go_forward: false, bookmarked: false, loading: false, internal: false,
  maximized: false, card: true, tint: null, myth: null, frame: { gap: 8, overlay: null, panel: null },
  motion: "off", glass: "off", transparency: 50, reduce_transparency: false, compat: null,
  dialog: null, menu: null, popups_blocked: null, suggest_web: true,
  zoom: null, find: null, fullscreen: false, toast: null,
};
const last = (op) => [...calls].reverse().find((c) => c.op === op);

{
  const { ctx, p } = await chromePage("11");
  const render = (s) => p.evaluate((s) => window.mb.render(s), s);
  await render(base); await wait(200);
  ok("zoom chip hidden at 100 %", await p.evaluate(() => document.getElementById("zoom").hidden));
  ok("find bar hidden", await p.evaluate(() => document.getElementById("findbar").hidden));

  await render({ ...base, zoom: 110 }); await wait(150);
  const zt = await p.evaluate(() => { const z = document.getElementById("zoom"); return { hidden: z.hidden, text: z.textContent, w: z.getBoundingClientRect().width }; });
  ok("zoom chip shows 110%", !zt.hidden && zt.text === "110%" && zt.w > 20, zt);
  await p.screenshot({ path: OUT + "/zoom-chip.png", clip: { x: 0, y: 0, width: 1280, height: 110 } });
  await p.click("#zoom"); await wait(150);
  ok("zoom chip click -> zoom reset", last("zoom")?.q.dir === "reset", last("zoom"));

  // Find bar: opened by Ctrl+F (shell bumps focus).
  await render({ ...base, zoom: 110, find: { query: "", current: null, total: null, focus: 1 } }); await wait(200);
  const fb = await p.evaluate(() => {
    const f = document.getElementById("findbar"), r = f.getBoundingClientRect(), o = document.getElementById("omnibox-wrap").getBoundingClientRect();
    return { hidden: f.hidden, focused: document.activeElement && document.activeElement.id, w: r.width, top: r.top, bottom: r.bottom, ob: o.bottom, overlap: r.left < o.right - 1 };
  });
  ok("find bar shown, field focused", !fb.hidden && fb.focused === "find-input", fb);
  ok("find bar sits in the toolbar beside the address bar", fb.w >= 280 && fb.bottom <= 100 && !fb.overlap, fb);
  await p.keyboard.type("serv"); await wait(350);
  ok("typing searches (dir=new, debounced)", last("find")?.q.q === "serv" && last("find")?.q.dir === "new", last("find"));
  await render({ ...base, zoom: 110, find: { query: "serv", current: 2, total: 5, focus: 1 } }); await wait(120);
  ok("count shows 2 of 5", (await p.textContent("#find-count")) === "2 of 5");
  ok("typed text kept while rendering", (await p.inputValue("#find-input")) === "serv");
  await p.screenshot({ path: OUT + "/find-bar-light.png", clip: { x: 0, y: 0, width: 1280, height: 110 } });
  await p.keyboard.press("Enter"); await wait(100);
  ok("Enter -> next", last("find")?.q.dir === "next", last("find"));
  await p.keyboard.press("Shift+Enter"); await wait(100);
  ok("Shift+Enter -> prev", last("find")?.q.dir === "prev", last("find"));
  await p.click("#find-next"); await wait(100);
  ok("next button", last("find")?.q.dir === "next");
  await render({ ...base, find: { query: "zzz", current: 0, total: 0, focus: 1 } }); await wait(120);
  await p.fill("#find-input", "zzz"); await wait(300);
  await render({ ...base, find: { query: "zzz", current: 0, total: 0, focus: 1 } }); await wait(120);
  const none = await p.evaluate(() => { const c = document.getElementById("find-count"); return { t: c.textContent, bad: c.classList.contains("none"), prev: document.getElementById("find-prev").disabled }; });
  ok("no results shown, arrows off", none.t === "No results" && none.bad && none.prev, none);
  await p.focus("#find-input");
  await p.keyboard.press("Escape"); await wait(100);
  ok("Esc -> find_close", !!last("find_close"));
  // Ctrl+F again (focus bumped): field focused + selected.
  await p.focus("#omnibox");
  await render({ ...base, find: { query: "zzz", current: 0, total: 0, focus: 2 } }); await wait(150);
  ok("Ctrl+F again refocuses the field", await p.evaluate(() => document.activeElement.id === "find-input" && document.activeElement.selectionEnd - document.activeElement.selectionStart === 3));
  await render(base); await wait(100);
  ok("find bar hides when closed", await p.evaluate(() => document.getElementById("findbar").hidden));

  // Note (toast): shown once per id, gone after a few seconds.
  await render({ ...base, toast: { id: 1, text: "MythBrowse can't open zoommtg: links yet" } }); await wait(150);
  const tt = await p.evaluate(() => { const t = document.getElementById("toast"); const r = t.getBoundingClientRect(); return { hidden: t.hidden, text: t.textContent, bottom: r.bottom, w: r.width }; });
  ok("toast shows the note in the toolbar", !tt.hidden && tt.text.includes("zoommtg:") && tt.bottom <= 100 && tt.w > 100, tt);
  await p.screenshot({ path: OUT + "/toast.png", clip: { x: 0, y: 0, width: 1280, height: 110 } });
  await wait(4800);
  ok("toast hides by itself", await p.evaluate(() => document.getElementById("toast").hidden));
  await render({ ...base, toast: { id: 1, text: "MythBrowse can't open zoommtg: links yet" } }); await wait(150);
  ok("same toast id is not shown again", await p.evaluate(() => document.getElementById("toast").hidden));

  // Fullscreen: the chrome hides behind the page.
  await render({ ...base, fullscreen: true }); await wait(100);
  const fs1 = await p.evaluate(() => ({ cls: document.body.classList.contains("fullscreen"), vis: getComputedStyle(document.getElementById("toolbar")).visibility }));
  ok("fullscreen hides toolbar", fs1.cls && fs1.vis === "hidden", fs1);
  await render(base); await wait(100);
  ok("leaving fullscreen shows it again", await p.evaluate(() => getComputedStyle(document.getElementById("toolbar")).visibility === "visible"));
  await ctx.close();
}
{
  // Dark sky shot of the find bar + zoom chip.
  const { ctx, p } = await chromePage("21");
  const render = (s) => p.evaluate((s) => window.mb.render(s), s);
  await render({ ...base, zoom: 133, find: { query: "", current: null, total: null, focus: 1 } }); await wait(200);
  await p.keyboard.type("engine"); await wait(300);
  await render({ ...base, zoom: 133, find: { query: "engine", current: 1, total: 12, focus: 1 } }); await wait(200);
  await p.screenshot({ path: OUT + "/find-bar-dark.png", clip: { x: 0, y: 0, width: 1280, height: 110 } });
  ok("dark: count 1 of 12", (await p.textContent("#find-count")) === "1 of 12");
  await ctx.close();
}

// ---------------------------------------------------------------- B. page find + links
{
  const ctx = await b.newContext({ viewport: { width: 1000, height: 700 } });
  const consoleLines = [];
  const fixture = `<!doctype html><html><head><title>MythBrowse fixture</title>
<style>body{font:16px/1.5 sans-serif;margin:24px;max-width:760px} .tall{height:1600px} .hide{display:none}</style></head><body>
<h1>Find in page fixture</h1>
<p id="p1">Servo is a web engine. MythBrowse uses Servo; servo is written in Rust.</p>
<p>Links: <a id="mail" href="mailto:hello@example.com?subject=Hi">email us</a>, <a id="zoom" href="zoommtg://zoom.us/join?confno=1">join the call</a>,
<a id="web" href="https://example.com/">a web link</a>, <a id="handled" href="mailto:x@example.com">handled by the page</a>.</p>
<div class="hide">servo hidden text must not count</div>
<script>var notCounted = "servo in a script";</script>
<div class="tall"></div>
<p id="far">The last servo mention is far down the page.</p>
<script>document.getElementById("handled").addEventListener("click", function (e) { e.preventDefault(); });</script>
</body></html>`;
  await ctx.route("https://fixture.test/**", (r) => r.fulfill({ contentType: "text/html", body: fixture }));
  await ctx.route(/^(?!https:\/\/fixture\.test)/, (r) => r.abort());
  await ctx.addInitScript({ content: fs.readFileSync(PAGE_INPUT, "utf8") });
  const p = await ctx.newPage();
  p.on("pageerror", (e) => errors.push("page: " + e.message));
  p.on("console", (m) => consoleLines.push(m.text()));
  await p.goto("https://fixture.test/", { waitUntil: "load" });
  await wait(200);
  const r1 = await p.evaluate(() => window.__mbFind("servo", 0));
  ok("find counts rendered matches only (4: 3 visible + far, not hidden/script)", r1 === "1/4", r1);
  const layer = await p.evaluate(() => { const l = document.querySelector("mb-find"); return l ? l.children.length : -1; });
  ok("matches painted in the overlay", layer >= 4, layer);
  await p.screenshot({ path: OUT + "/page-find.png" });
  ok("next", (await p.evaluate(() => window.__mbFind("servo", 1))) === "2/4");
  ok("prev", (await p.evaluate(() => window.__mbFind("servo", -1))) === "1/4");
  ok("prev wraps to last", (await p.evaluate(() => window.__mbFind("servo", -1))) === "4/4");
  const sy = await p.evaluate(() => window.scrollY);
  ok("current match scrolled into view", sy > 500, sy);
  await p.screenshot({ path: OUT + "/page-find-far.png" });
  ok("next wraps to first", (await p.evaluate(() => window.__mbFind("servo", 1))) === "1/4");
  {
    // On match 2 ("uses Servo;"), typing a space: the next "servo " at or after it is #2 of 3.
    const grow = await p.evaluate(() => { window.__mbFind("servo", 1); return window.__mbFind("servo ", 0); });
    ok("growing query keeps the place", grow === "2/3", grow);
  }
  ok("case-insensitive", (await p.evaluate(() => window.__mbFind("MYTHBROWSE", 0))) === "1/1");
  ok("no match", (await p.evaluate(() => window.__mbFind("qqqzzz", 0))) === "0/0");
  ok("empty query clears", (await p.evaluate(() => window.__mbFind("", 0))) === "0/0" && (await p.evaluate(() => !document.querySelector("mb-find"))));
  await p.evaluate(() => window.__mbFind("servo", 0));
  await p.evaluate(() => window.__mbFindClear());
  ok("clear removes the overlay", await p.evaluate(() => !document.querySelector("mb-find")));
  const textAfter = await p.evaluate(() => document.getElementById("p1").innerHTML);
  ok("page DOM untouched by find", textAfter.startsWith("Servo is a web engine."), textAfter);

  // External links.
  await p.evaluate(() => window.scrollTo(0, 0));
  await p.click("#mail"); await wait(100);
  await p.click("#zoom"); await wait(100);
  await p.click("#handled"); await wait(100);
  const ext = consoleLines.filter((l) => l.startsWith("\u0001mythbrowse:external "));
  ok("mailto click reported", ext.some((l) => l.includes("mailto:hello@example.com")), ext);
  ok("zoommtg click reported", ext.some((l) => l.includes("zoommtg://zoom.us/join")), ext);
  ok("page-handled click not reported", !ext.some((l) => l.includes("x@example.com")), ext);
  ok("still on the page after external clicks", p.url() === "https://fixture.test/", p.url());
  const navs = [];
  p.on("request", (r) => navs.push(r.url()));
  await p.click("#web"); await wait(200);
  ok("http link not reported (engine loads it)", !consoleLines.some((l) => l.includes("example.com/\"")), consoleLines.slice(-3));
  await ctx.close();
}

await b.close();
const failed = checks.filter((c) => !c.pass);
const res = { checks, failed: failed.length, errors, calls: calls.slice(-30) };
fs.writeFileSync(OUT + "/browse_smoke.json", JSON.stringify(res, null, 2));
for (const c of checks) console.log((c.pass ? "PASS " : "FAIL ") + c.name + (c.pass ? "" : "  got: " + JSON.stringify(c.got)));
console.log(`${checks.length - failed.length}/${checks.length} passed; ${errors.length} page errors`);
if (errors.length) console.log(errors.join("\n"));
process.exit(failed.length || errors.length ? 1 : 0);
