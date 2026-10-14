# Site compat (week 3)

MythBrowse renders with Servo, which still lacks some web APIs and CSS features and is
sometimes refused by sites that sniff the user agent. The compat layer finds pages that break,
fixes what it safely can, and lets Myth propose a fix for the rest, without ever running
unreviewed code on sites the user did not choose. It follows the layered plan in
`docs/PLAN.md` §3.

| layer | where | status |
|---|---|---|
| 1. Detect | `crates/compat/src/detect.rs`, `js/bootstrap.js`, `js/probe.js` | done |
| 2. Per-site patch ruleset (signed) | `rules/`, `crates/compat/src/{rules,matcher,bundle,sign}.rs` | done (built-in + local; remote verified but not downloaded yet) |
| 3. AI repair | `crates/compat/src/repair.rs`, `crates/shell/src/compat_host.rs` | done (needs a model: Ollama or BYOK) |
| 4. Reader / simplified fallback | `crates/compat/src/js/reader.js` | done (one-shot, from the panel or a rule) |
| 5. Cloud Gecko renderer | - | not planned for the open build |
| 6. Top-sites compat dashboard | `.github/workflows/compat.yml`, `ci/compat/` | written, manual only, not run yet |

## 1. Detector

Cost first: nothing runs while a page is being used.

* **Bootstrap** (first Servo user script in every web page): event listeners only. Records
  uncaught errors, unhandled rejections and failed subresources (`error` in the capture phase),
  and which of ~30 commonly needed web APIs are missing *natively* (checked before any polyfill
  runs). State lives on one frozen, non-enumerable `window` property whose name is random per
  browser session.
* **Console**: the shell counts Servo console errors / warnings per tab
  (`WebViewDelegate::show_console_message`) and keeps the last few lines.
* **Probe**: evaluated **once** when a user tab reaches `LoadStatus::Complete`, and again only
  when the user presses Re-check (or after a preview). It reads the bootstrap state plus a few
  cheap facts: body / document height, visible text length, element and media counts, a 4x4
  `elementFromPoint` grid (how much of the viewport shows nothing), scroll lock, missing CSS
  features (`CSS.supports`), "your browser is not supported" phrases on short pages (EN / 中文 /
  BM), and API names extracted from error messages ("X is not defined / not a function") that
  no reachable object has.
* **Scoring** (`assess`): blank layout +55, UA-block page +60, uncaught errors 6 each (max 5),
  unsupported APIs 10 each (max 4), failed resources 2 each (max 10), console errors 1 each
  (max 10), scroll lock +10; missing APIs / CSS are listed but score 0. Level: `ok` < 20 <=
  `degraded` < 50 <= `broken`. The weights are first guesses; the top-sites run exists to tune
  them on real sites.

Agent (background) tabs get the rules and the bootstrap but no probe and no panel.
`MYTHBROWSE_COMPAT=0` turns the whole layer off (no scripts, no probe).

## 2. Ruleset

Declarative TOML, schema `rules/schema.json` (`schema = 1`), validated again in Rust with the
trust rules below. Fields per `[[rule]]`: `id`, `title`, `kind` (`polyfill` / `site` / `ai`),
`hosts`, `exclude_hosts`, `paths`, and what it does: `ua`, `css`, `js`, `block`, `mode`
(`reader` / `simplified`), plus `enabled`, `fills`, `note`.

Host patterns: `*` every web site, `example.com` it and its subdomains, `=example.com` exactly,
`*.example.com` subdomains only. Paths and block patterns are `*` globs. The browser's own
origin never matches.

### Sources and trust

| source | file | may use `*` | JS goes into the all-sites bundle |
|---|---|---|---|
| built-in | `rules/core.toml`, compiled in | polyfills only (css / js) | yes |
| remote (signed) | `<profile>/compat/remote.toml` + `.sig` | polyfills only | yes |
| local (the user's, incl. kept AI patches) | `<profile>/compat/local.toml` | no | **never** |

The user's switches (pause a site, turn a rule off for a site) are in
`<profile>/compat/state.json`. A bad local file is ignored with a log warning; it never stops
the browser.

### How rules are applied (Servo 0.1.4 APIs)

* **User scripts**: one `UserContentManager` is shared by every web-page webview (user tabs,
  popups, agent tabs; never the chrome or internal pages), set with
  `WebViewBuilder::user_content_manager`. Servo has no per-URL matching for user content, so
  each rule is its **own** user script that gates itself on `location` (same matcher as Rust)
  before doing anything; a broken rule cannot take the others down. After a rule change the
  scripts are swapped (`remove_script` / `add_script`) and the page reloads, because Servo
  applies user content on the next load.
* **Local rules**: only *data* goes into the shared scripts (CSS text, a UA string, a mode
  name, each embedded as a JSON string literal, so it cannot escape the gate). Their JS is run
  with `WebView::evaluate_javascript` at `LoadStatus::HeadParsed`, only in pages whose URL Rust
  itself matched.
* **CSS**: a tagged `<style data-mb-compat=...>` added by the rule script.
* **UA override**: `navigator.userAgent` / `appVersion` / `platform` are redefined for the
  page's scripts (presets `firefox`, `chrome`, `servo`, `firefox-windows`, `firefox-linux`,
  `chrome-windows`, or a full string; `crates/compat/src/ua.rs`). Limitation: the HTTP
  `User-Agent` header cannot be changed per site with the 0.1.4 embedding API
  (`WebResourceLoad` can only replace the whole response; the UA is one process-wide
  preference), so it keeps the global `ua_mode` below. Use a per-site `chrome` only for a
  site that gates purely on the UA in script and works with it.
* **Block**: `load_web_resource` cancels matching subresource loads on matching pages (never
  top-level navigations), next to the content blocker.
* **Mode**: reader (article view over the page) or simplified (removes fixed overlays that
  cover > 30 % of the viewport and scroll locks), applied after DOMContentLoaded.

### Built-in rules

Only generic shims that do nothing when the API exists: `requestIdleCallback`,
`IntersectionObserver` (reports elements as visible once, so lazy content loads; no scroll
tracking), `ResizeObserver` (initial size + window resizes), `CSS.escape` (spec algorithm),
`document.fonts` (fonts treated as loaded), `Element.animate` (finishes at once, keeps the
last keyframe with `fill: forwards`). Site-specific fixes are added only after they are
reproduced on the real site (top-sites run or a user report), never guessed.

### Signing (for the cloud ruleset service, week 4+)

`<name>.toml.sig` is one line, `mbsig1 <key-id> <base64 Ed25519 signature>`, over
`"mythbrowse-compat-ruleset-v1\n" || toml bytes`; `key-id` = first 8 bytes of SHA-256 of the
public key (hex). The browser accepts a remote ruleset only if it verifies with a key in
`sign::TRUSTED_KEYS`, passes the schema / trust checks, and its `version` is not older than the
last accepted one (anti-rollback). `TRUSTED_KEYS` is empty until the release key exists, so
today every remote ruleset is refused.

```text
mb-rules check   rules/core.toml builtin        # schema + trust rules
mb-rules scripts rules/core.toml out/           # generated user scripts (CI runs node --check)
mb-rules keygen  release.key                    # prints the TrustedKey line to paste
mb-rules sign    release.key remote.toml        # writes remote.toml.sig
mb-rules verify  remote.toml <public-key-hex>
```

The private key never goes in git: keep it offline, or in the publishing job's secret store.
The core rules live in this private repo and are compiled into the binary; they are not part
of the planned public docs / SDK repo.

## 3. AI repair: workflow and safety rules

1. **Only on request.** Nothing is sent to a model until the user presses "Ask Myth to fix" in
   the panel. It uses the chat model from Myth's settings (Ollama on this PC, or the user's own
   API key); the panel says which model and whether it is local.
2. **What is sent**: the detector findings, uncaught error messages (with script URLs and
   lines), recent console errors, failed resource URLs, missing features, layout numbers, and a
   **structure-only** DOM summary: tags, ids, classes, sizes, layout flags, script / stylesheet
   URLs (host + path). No page text, no form values, no other attribute values, no cookies.
3. **What comes back**: a candidate with an explanation, CSS, optional JS, optionally the
   `firefox` UA preset or a reader / simplified mode. The prompt forbids network, eval, storage,
   cookies, navigation, form submission and reading inputs, and asks for shims only of APIs the
   uncaught errors show the page calls (not the whole "missing natively" list). Code a model
   escaped twice (literal `\n` instead of line breaks) is unescaped before review.
4. **Static review** (`repair::review`) blocks candidates that mention network APIs, eval /
   `new Function` / dynamic import, cookies or storage, `postMessage`, `window.open`,
   navigation, form submission, password / credential APIs, script injection, string timers,
   `setInterval`, remote CSS loads (`@import`, remote `url()`), non-preset UAs, or exceed 8 KB
   JS / 16 KB CSS. Blocked candidates cannot be tried. This is defence in depth, not a sandbox.
5. **Preview in one tab**: "Try on this page" applies the candidate to that tab's current
   document only (`evaluate_javascript`), then re-runs the detector and shows before / after.
   **Undo** reloads the page without it.
6. **Keep = local rule for that exact host** (`hosts = ["=host"]`), and only after it was
   tried. It never applies to other sites, other hosts of the same site, or the all-sites
   bundle, and it is never uploaded. Saved rules show in the panel with Turn off / Delete.
7. **Never automatic**: no repair runs in agent tabs or without the panel; a kept patch is not
   shared with anyone (crowd-sourcing to the shared ruleset is a later, reviewed, opt-in step).

## 4. UI

A quiet line icon (MythDesk icon set, `compat`) next to the bookmark star, on web pages only. A
small dot appears only when the detector found problems (amber = degraded, red = broken), a
site fix is active (blue), or fixes are paused (grey). It opens a panel that floats over the top
right of the page card (the shell composes it like the Myth pill; `MythOverlay::Compat`; an
approval card takes precedence): status and score, findings, the AI fix flow (suggestion with
the exact code, Try / Discard, then Keep / Undo), rules applied here (Turn off / on, Delete for
your own), and tools: Reader view, Simplify, Re-check, Pause on this site. Esc or a click on the
page closes it.

## 5. Top-sites compat run (manual)

`.github/workflows/compat.yml` (workflow_dispatch only; inputs `top_n`, `ci_run_id` to reuse a
CI run's Linux build instead of building Servo again, `settle_ms`). `ci/compat/run-linux.sh`
runs the browser on Xvfb + llvmpipe with `MYTHBROWSE_COMPAT_RUN=<sites>` /
`MYTHBROWSE_COMPAT_OUT=<dir>`: the shell loads each site in turn, waits for load (timeout
`MYTHBROWSE_COMPAT_TIMEOUT_MS`, default 25 s) and settle (`MYTHBROWSE_COMPAT_SETTLE_MS`,
default 3 s), runs the detector, saves `NNN-host.png` and a JSON line in `report.jsonl`, then
exits. `ci/compat/report.py` writes `summary.md` (also to the job summary) and `summary.json`;
the **compat score** is the share of sites rated `ok`, with the mean breakage score next to it.
Sites list: `ci/compat/top-sites.txt`.

### First results (2026-10-09, sandbox, no CI minutes)

Run outside Actions: official `servoshell` v0.5.0 (aarch64, headless; 0.1.x ships no aarch64
build, so results may be slightly better than our 0.1.4) driven over WebDriver, with this crate's
bootstrap + core rules as user scripts and the same probe and `assess()`. Default Servo UA
(Firefox/140-compatible), datacenter IP.

- 53 sites: 41 ok, 2 degraded (airbnb.com: 130 script errors + `getAnimations`; archive.org:
  mostly empty + `adoptedStyleSheets`), 2 hung (amazon.com, zoom.us: the page never answered the
  probe), 8 behind a bot check (reddit, microsoft, openai, chatgpt, medium, nytimes, zhihu,
  weibo). **91.1 % of the 45 scored sites OK.**
- No "browser not supported" page anywhere, so the default UA stays.
- Detector changes from this run: `bot_check` (challenge phrases, title-only generic words,
  challenge frames) sets `blocked`, scores 0 and offers no repair; script-error and
  failed-resource points are halved on pages that visibly rendered and resources count 1 each
  (cap 20), so github.com-style noise stays ok; missing APIs 12 each; the run records `hung`
  when the probe never answers and `report.py` scores blocked sites out.
- The two "hangs" were a Servo crash, not slow pages: Servo runs user scripts in every document
  whose `<head>` gets attached, including `<iframe sandbox>` (no `allow-scripts`) documents, and
  `GlobalScope::evaluate_js_on_global` asserts `can_run_script()`. The panic kills the script
  thread, and the top-level page on it. Any user script triggers it (even `void 0;`); repro:
  `data:text/html,<iframe sandbox srcdoc="<p>x</p>"></iframe>` with any user script. Present in
  servo-script 0.1.4, servoshell 0.5.0 and Servo `main` (2026-10-09). Since MythBrowse always
  injects the compat bootstrap, every site with a sandboxed iframe was affected. Fixed by the
  vendored `third_party/servo-script` patch (skip user scripts where script cannot run); see
  `third_party/README.md`. Upstream report to follow.
- Next engine work: confirm the patch in a full build.

## Polyfills added 2026-10-09 (core ruleset 2026100901)

Checked in servoshell 0.5.0 (sandbox, aarch64 Linux) before writing them: Servo has
`new CSSStyleSheet()` with `replace`/`replaceSync`/`insertRule`, but no `adoptedStyleSheets`
and no `getAnimations`.

- `polyfill.get-animations`: `Element/Document/ShadowRoot.getAnimations()` return `[]`. True
  to what Servo does: the `Element.animate` stub finishes at once, so nothing is ever running.
  airbnb.com: 130 script errors -> 0, page content up (15.8k text chars, 6.7k elements).
- `polyfill.adopted-style-sheets`: `adoptedStyleSheets` on `document` and on every root from
  `attachShadow`, defined per instance and not on the prototypes, so library feature checks
  (Lit: `'adoptedStyleSheets' in Document.prototype`) keep their native `<style>` path. Each
  adopted sheet is mirrored as `<style data-mb-adopted>` at the end of its root; `replace`,
  `replaceSync`, `insertRule`, `deleteRule` and `push()` into the array re-render. Limits:
  declarative shadow roots (from HTML) do not get it; sheets changed via `insertRule` are
  re-serialised from `cssRules` (Servo drops rules it cannot parse).
  archive.org: blank page -> renders (header, Wayback search, news, search box).
- Still left on archive.org then: missing CSS grid (`getComputedStyle().gridTemplateColumns`
  undefined). Engine work, not a polyfill: see next section.

## CSS grid enabled (Servo pref `layout_grid_enabled`, 2026-10-09)

Servo 0.1.x ships grid layout behind a pref, off by default; without it `display:grid` is
parsed as invalid and the page falls back to block layout. MythBrowse now turns it on in
`crates/engine/src/servo_engine.rs` (`engine_preferences()`).

Top-sites run with `--pref=layout_grid_enabled` (servoshell 0.5.0, sandbox, all 53 sites):
no site got worse. archive.org degraded -> OK (script errors 2 -> 0, page height 361k px ->
6k px), github.com script errors 2 -> 0. The only other changes were run-to-run noise
(nytimes bot check in the old run, shopee's language dialog), checked on screenshots.

## What only real runs can confirm

* The full Servo build with the new APIs (`UserContentManager`, `show_console_message`,
  `user_content_manager` on builders): type-checked against a mock locally, compiled for real
  only by the full CI.
* When Servo runs user scripts relative to page scripts (expected: when `<head>` is attached,
  before body scripts), and that `evaluate_javascript` at `HeadParsed` is early enough for
  local rules.
* The panel's look in Servo (light / dark sky, HiDPI), scrolling inside it, Esc / click-away.
* Detector false positives / negatives and the score weights: needs the top-sites run.
* AI repair end to end in the real shell UI. The flow itself (probe + DOM summary in a headless
  Servo, real model, parse, review, rule validation, save / reload, preview, "fixed?" check) is
  covered by `crates/e2e` with a real model (`compat_*` scenarios); the injected-instruction case
  showed that a model can follow instructions hidden in error text, and that the static review
  is what stops it.

## User agent (decided 2026-10-13, v0.4)

Sites should treat MythBrowse like a mainstream browser instead of refusing it (Google
reCAPTCHA's "upgrade to a supported browser", Canva / Bank of America / Fiverr UA rejects).
Servo's own UA carries a `Servo/0.1.4` token that UA sniffers reject.

* **Default UA = a standard Firefox desktop UA** with the `rv:` Servo itself claims:
  `Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0`
  (`X11; Linux x86_64` on Linux, `Macintosh; Intel Mac OS X 10.15` on macOS). It is Servo's
  `user_agent` preference, so the HTTP header and `navigator.userAgent` agree; a user script
  in every page sets `navigator.appVersion` (`5.0 (Windows)`) and `navigator.platform`
  (`Win32` / `Linux x86_64` / `MacIntel`) to match (Servo reports `4.0` / `Linux`).
* **Why Firefox, not Chrome**: Servo shares Gecko's JS engine (SpiderMonkey) and much of its
  feature set; a Chrome UA makes sites ship Chrome-only code paths that Servo lacks.
* **Setting `ua_mode`** (`/api/prefs` reports `ua_mode` + `user_agent`; `/api/prefs_save?ua_mode=`
  `firefox` | `chrome` | `servo`; stored in `<profile>/ui.json`; Settings UI later). A change
  applies to new requests at once and to `navigator` on pages loaded afterwards.
* **Per site**: a compat rule's `ua` (see "How rules are applied").
* **Not done, on purpose**: no fingerprint forging, no `navigator.webdriver` / automation-flag
  changes, no TLS / HTTP-2 / client-hint imitation, nothing aimed at evading bot detection.
  Only the UA string is chosen (plus the two `navigator` values that must agree with it).
  Sites whose bot checks still refuse Servo stay refused.

## Decisions open

* Release signing key: who holds it, and where the publishing job keeps it.
* When to spend Actions minutes on the top-sites run (after the full CI is green on this
  branch; `ci_run_id` avoids a second Servo build).
