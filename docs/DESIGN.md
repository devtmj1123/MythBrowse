# JustMyth design system: MythDesk is the source

MythBrowse uses the **MythDesk** desktop design system (JustMyth's MythDesk desktop,
`desktop/ui/style.css`, `sky.js`, `icons.js`, merged in PR #8 and #9). Seen side by side, the
two must read as one product family. When the two disagree, MythDesk wins and this repo follows.

Files (`ui/design/`):

| File | What | From MythDesk |
|---|---|---|
| `tokens.css` | Variables with MythDesk's names and values (`--glass`, `--glass-hi`, `--glass-you`, `--chip`, `--chip-hi`, `--line`, `--text`, `--soft`, `--dim`, `--ink`, `--ink-text`, `--accent`, `--ok/--warn/--bad`, `--r-lg 22`, `--r-md 16`, `--shadow`), light glass for dawn/day and dark glass otherwise; blocks `.glass`, `.primary`, `.ghost`, `.chip`, `.w-title`, `kbd`, `.mark` (launcher logo), `.tb-dots` (title-bar dots), `.site`. | `style.css` `:root` + `body[data-phase="day"/"dawn"]` |
| `theme.js` | Time of day: MythDesk's sky palette (KEYS), phases (night < 6.2, dawn < 7.5, day < 17.3, dusk < 19.5, evening < 20.5) and tone; sets `data-phase` / `data-tone` on `<html>` and `--sky-*`. `window.JM`. | `sky.js` palette/phase |
| `sky.js` | The living sky on `<canvas id="sky">`: gradient, colour washes, sun/moon glow with turning rays, stars, drifting clouds, horizon haze. Same seed and maths. | `sky.js` (port) |
| `icons.js` | MythDesk icon set, paths unchanged (24 grid, 1.8 stroke, round caps), plus browser icons drawn the same way (back, forward, reload, plus, star, history, search, globe, lock, trash, moon; week 3: compat puzzle piece, reader, undo). `window.JMIcons`. | `icons.js` |

## The MythDesk visual language (what to keep identical)

- **Sky, not a background colour.** A procedural sky that follows the real time of day
  (Kuala Lumpur sun times): clear blue by day (`#89b4de` to `#f1efe8` at noon-ish), warm
  sunrise/sunset, deep navy with stars at night. Rendered small (0.4x) and blurred 5 px.
- **Frosted glass on the sky.** Every surface is translucent white by day (navy by night) with a
  1 px inner white highlight and ring (`--shadow`), no hard borders, soft drop shadow for
  floating cards (`0 18px 40px rgba(10,16,40,.18)`).
- **Navy ink.** Text `#16203a` (day) / `#f3f5ff` (night); secondary text is the same colour at
  62 % / 40 %. Solid actions (send, active launcher tab) are filled navy with white icons.
- **Accent used sparingly.** Purple `#5b4bd6` (day) / `#b9a8ff` (night) only for focus and the
  composer's spark icon. No gradients on controls; the brand colours live in the three dots.
- **Shapes.** Panels 22 px, chat bubbles 18 px (6 px tail corner), composer 18 px, buttons,
  inputs and status pills 12 px, tabs/chips 10 to 11 px, tooltips 8 px.
- **Type.** Segoe UI; 15 px body, 12 px bold uppercase section labels (letter-spacing .8 px),
  28 px/600 headings, the home clock 96 to 104 px/600 with a dimmed colon, 13 px uppercase date
  with 2 px tracking.
- **Title bar.** Sits on the sky; three dots at the left; 46 px window buttons with soft icons;
  close turns `#e81123`.
- **Brand.** "Myth" is the assistant's name. Its mark is three dots, lilac `#a99bff`, sky
  `#7fd3ff`, pink `#ff9cc6`.

## How MythBrowse applies it

- **Chrome** (`ui/chrome.*`, bars 88 px): the chrome webview covers the whole window and draws
  the same sky behind everything (canvas `data-band`: redrawn once a minute, no clouds, no
  animation loop, so browsing costs nothing). Tabs are glass chips (the active one
  `--glass-hi` + `--shadow`), window buttons are MythDesk's, the address bar is a 12 px glass
  field. Its left icon is plain browser state (lock, "Not secure", globe, search); the AI only
  shows as a quiet "Enter · Ask Myth" hint while you type something that is not an address.
- **Site compat.** A quiet `compat` line icon by the bookmark star, web pages only, with a 7 px
  status dot only when needed (amber degraded, red broken, blue fix active, grey paused). Its
  panel is the same glass surface as the Myth pill, floating over the top right of the page card
  (see `docs/COMPAT.md`); navy for the one primary action, glass for the rest, no emoji.
- **Web pages float on the sky.** A web page is a card: 8 px of sky to its left, right and
  bottom and between it and the toolbar (4 px when maximized), 12 px corners, a soft MythDesk
  floating-card shadow (`0 6px 22px`, stronger at night) and a 1 px hairline ring. Constants:
  `CARD_GAP_CSS`, `CARD_GAP_MAXIMIZED_CSS`, `CARD_RADIUS_CSS` in `crates/engine/src/frame.rs`
  = `--card-gap` / `--card-radius` in `ui/chrome.css`. Internal pages (new tab, history) are
  not cards: they draw the sky themselves and fill everything under the toolbar, so the sky is
  seamless there. The sky ring is inert (clicks do nothing and do not take focus); the outer
  6 px still resize the window.
- **Page tint.** On web pages the bars and the sky ring take a light wash of the page's colour:
  its `<meta name="theme-color">` (respecting `media`), else its background colour, read once
  when the page finishes loading. Alpha is about 20 % (22 % at night), halved for greys and
  whites, and lowered further until the bar text keeps 4.5:1 and secondary text keeps 3:1 (or
  loses at most 10 % where the bare sky is already below 3:1). By day this usually lands at
  6 to 10 % for dark or saturated colours. Switching tabs cross-fades the wash in 0.45 s (a CSS
  colour transition, not a loop). Internal pages are never tinted. Clipped tab titles fade into
  the tinted colour (`--strip`).
- **New tab** (`ui/home.*`): the MythDesk home on the sky. Date + clock on the left, glass cards
  on the right with real data (Recent pages, Bookmarks when there are any, link to all history),
  Myth's status pill top right, the MythDesk composer ("Message Myth") at the bottom. A new tab
  is a new conversation; replies use MythDesk's chat bubbles. The page sky is drawn as if it
  started 88 px down the window, so it continues the chrome's sky.
- **History & bookmarks** (`ui/library.*`): MythDesk's wide views (28 px heading, glass panels,
  12 px rows that highlight with `--chip`).
- English UI only (MythDesk has a language setting; MythBrowse will follow it later), no
  duplicated bilingual strings.

Deliberately **not** used (generic "AI product" tropes): purple/lilac glow gradients, sparkle
icons on every control, "Good morning" greetings, "Ask anything" copy, suggestion chips, empty
placeholder cards, an AI pill inside the address bar.

## Time of day

Pin the hour with `<meta name="mb-theme-hour" content="21">` or `?hour=21` (MythBrowse:
`MYTHBROWSE_THEME_HOUR=21`; MythDesk: `?hour=21`). CI screenshots use 10 (day), 21.5 (night)
and 18.4 (dusk).

## Motion

Motion is quiet and state-driven: nothing animates while nothing changes (zero extra frames
when idle), and **Settings > Motion** (Model sheet on the AI home) chooses **Full / Reduced /
Off**. Windows' "Show animations in Windows" off (`SPI_GETCLIENTAREAANIMATION`, read at start)
turns Full into Reduced; elsewhere `MYTHBROWSE_REDUCED_MOTION=1`; pages also honour
`prefers-reduced-motion` where Servo reports it. Pages get `<html data-motion>` (theme.js);
tokens.css then keeps only colour / opacity fades (Reduced) or nothing (Off).

**Card glow** on the page card's own edge, drawn by the shell with a small GL shader after
the page, its rounded corners and Myth's pill (`crates/shell/src/compose.rs`; state machine
and frame budget in `crates/engine/src/glow.rs`, unit-tested). Shape: a **2 px solid
gradient line just inside the card edge**, a faint inner glow over the page's outer 6 px, and
a **soft outer glow (~16 px) fading out across the sky**. While Myth works on the tab the
sky gap eases from 8 px (4 px maximized) to **12 px** in 0.22 s so the light has room
(`CARD_GAP_AI_CSS`; the page card shrinks by those few px, and the chrome follows the gap the
shell pushes in `state.frame`). CI 37759078770 showed the old ring (4 px of sky blue on the
sky, maximized) was invisible; the colours are now chosen to contrast with the sky and with
a white page, with alpha peaking at ~0.85 (never neon):

| state | look |
|---|---|
| Myth reading / thinking on this tab | blue `#3d7fd9` breathing, 3.2 s, no movement |
| Myth controlling this tab (has clicked / typed / navigated) | blue `#3d7fd9` -> warm orange `#f2a65a` -> a little magenta `#d05ca8` -> blue, flowing round the edge, 5 s a lap; a bright pulse on the edge next to each click / keystroke target |
| waiting for your OK | amber `#f0a020`, clear pulse (half to full strength, 1.6 s), with the approval card |
| page loading (no Myth) | a thin light sweeping along the top edge, outside the card only |
| done / error | one 0.9 s flash: green `#34c47c` / soft red `#e05f5f` |

At night (dark tone, reported by the chrome with `chrome_tone`) the same hues are brighter
(`#6caaff`, `#ffbe7a`, `#f080c8`, `#5ce09c`). Frames: only while the glow moves, at most
30 fps, and those frames re-compose the existing page / chrome framebuffers without asking
Servo to paint (`servo_paint` in `main.rs`); the shader runs only in four bands along the
edge (scissor). Reduced / Off: drawn once, static (the gap jumps instead of easing). Internal
pages (new tab, history) never glow. (The whole window is re-composed per frame because the
swap chain does not keep the previous frame; the cost is blits, not Servo.)

**Myth on the page: pill and approval card.** The title bar holds only tabs (a tab Myth
works in gets a small status dot: blue thinking, orange acting, amber waiting). Myth's state
is a **glass pill floating over the bottom centre of the page card**, where MythDesk puts its
composer: status dot, **Myth**, the current step (one line, cut with an ellipsis by
chrome.js since Servo has no `text-overflow`), **Stop**. When Myth needs an OK the pill
expands into a **card**: "Myth wants to: Click "Sign in"", the site and why it asks, and
**Allow** (solid navy) / **Don't allow** (glass, with an `Esc` hint) side by side, equal
height, labels centred (height from padding + line-height: Servo top-aligns a fixed-height
button's label). **Esc** = Don't allow (handled by the shell, whatever has focus); **Enter
never allows** unless the Allow button itself has keyboard focus. When the run ends the pill
shows "Done · <task>" with **Open answer** and a close button, and closes itself after 8 s
(not while hovered or focused). Pill 44 px tall, up to 560 px wide; card 150 px, up to
500 px; 16 px above the card's bottom edge (`MYTH_*` in `frame.rs`).

How it is drawn: Servo 0.1.4 cannot stack the chrome webview over a page webview, so the
**shell composes it**: the chrome page (window-sized) draws the pill at exactly the
rectangle `WindowLayout::overlay` computes (pushed as `state.frame.overlay` in CSS px), and
after the page is blitted the shell copies that rounded rectangle from the chrome's texture
over the page, anti-aliased, with a soft drop shadow (`OVERLAY_FRAGMENT` in `compose.rs`).
Pointer events inside the rounded rectangle go to the chrome (`region_at`), everything else
to the page; the page keeps its full size. The glass is nearly opaque (no backdrop-filter in
Servo), text keeps >= 4.5:1 (secondary text `rgba(22,32,58,.76)` on ~white by day,
`rgba(232,236,255,.78)` on dark glass at night).

**Done (week 2):** card ring; Myth cursor (click ripple, typing outline); tabs grow / shrink
on open / close (keyed DOM, no full rebuild); address bar widens slightly on focus; chat
bubbles rise in, browser steps fade in, approval cards pop in; Tasks cards slide in (new
ones only), their approval boxes pop in; Myth pill / approval card over the card; card glow on
the card edge with the sky gap easing wider while Myth works; tab status dots; window /
toolbar button hover fades; Motion setting.

**Later (TODO):** tab drag-to-reorder with follow animation (needs reordering in the tab
model first); omnibox suggestion dropdown fade (there is no dropdown yet); card fade on
navigation / back / forward (needs a compose-level crossfade of two page frames);
streamed-text fade per chunk (Markdown is re-rendered per chunk); step progress shimmer;
time-of-day sky colour easing on the new tab (sky.js / theme.js switch per minute today);
checking that sky.js does not draw when nothing changes.

## Renderer limits (Servo 0.1.4) and the substitutions they force

| MythDesk uses | Servo 0.1.4 | MythBrowse does instead |
|---|---|---|
| `backdrop-filter: blur(24px)` glass | not supported | slightly more opaque glass (day 0.56 / 0.72 instead of 0.46 / 0.62) |
| CSS grid (launcher logo, home layout, app tiles) | off by default | flexbox |
| `text-overflow: ellipsis` | not supported | titles shortened in JS or faded with an overlay |
| "Segoe UI Variable Text/Display" | variable fonts off | static "Segoe UI" (real Light/Semibold/Bold files) |
| `color-mix()` focus ring | not supported | `--accent-ring` rgba token |
| icons with `stroke: currentColor` | inline SVG is rendered as an image | colour written into the SVG from `JM.colors()` roles (`icon`, `soft`, `dim`, `accent`, `ink`, `inkText`, `bad`, `ok`, `warn`, `white`, `star`); re-painted on tone change |
| `transparent` in gradients | grey fringes | zero-alpha copy of the same colour |
| rounded / clipped page (`border-radius` + `overflow: hidden` around an iframe-like view) | a webview is an offscreen framebuffer blitted into the window as a plain rectangle; no clip or mask on the embedder side | the shell rounds the card itself (`crates/shell/src/compose.rs`): after the page blit it re-draws each 12 px corner square from the chrome's colour texture with a tiny shader, blended by the corner's coverage (anti-aliased). Fallback without GLSL 1.30 / ES 3.0: per-row blits (same shape, hard edge) |

## Using it

```html
<link rel="stylesheet" href="design/tokens.css">
<script src="design/theme.js"></script>
<script src="design/icons.js"></script>
<body class="sky-page">
  <canvas id="sky" data-above="88" data-bleed="12"></canvas>
  <div class="glass">…<span data-icon="search" data-role="soft"></span></div>
  <script src="design/sky.js"></script>
```

## Licensing

MythDesk and MythBrowse are both JustMyth code. All icon paths are original JustMyth drawings
(no third-party icon set), so no attribution is needed.
