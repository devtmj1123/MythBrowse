#!/usr/bin/env python3
"""Copy the polyfill sources in rules/js/*.js into rules/core.toml (the compiled-in ruleset).

rules/js/<file>.js is the source of truth for the rules listed in RULES; core.toml carries a
copy in a TOML literal string because the ruleset is one self-contained file (signed remote
rulesets use the same format). `cargo test -p mythbrowse-compat` fails when the two differ
(core_rules_match_js_sources), so run this after editing a .js file:

    python3 rules/js/sync_core.py && node --check rules/js/*.js && node rules/js/test/indexeddb.test.js

A rule missing from core.toml is appended with the metadata below.
Copyright (c) 2026 JustMyth / Tan Ming Jing. SPDX-License-Identifier: MPL-2.0."""
import os, re, sys
HERE = os.path.dirname(os.path.abspath(__file__))
CORE = os.path.join(HERE, "..", "core.toml")
RULES = [
    ("polyfill.intersection-observer", "intersection-observer.js", "IntersectionObserver fallback (real viewport geometry, re-checked on scroll/resize/DOM changes)",
     ["IntersectionObserver"],
     "Reports a target only when it crosses a threshold of its real intersection with the root (+rootMargin). The old stub said every target was visible at once, which made infinite-scroll pages load forever (compat-script hangs)."),
    ("polyfill.document-fonts", "document-fonts.js", "document.fonts members + FontFace (fonts become @font-face rules)",
     ["document.fonts", "document.fonts.load", "FontFace"],
     "Fills what the engine's FontFaceSet lacks (Servo 0.5 has only `ready`: 'document.fonts.load is not a function' in the top-1000 run) and adds FontFace; faces added to the set are written as @font-face rules so they load."),
    ("polyfill.check-visibility", "check-visibility.js", "Element.checkVisibility()",
     ["Element.checkVisibility"],
     "Computed-style walk: display:none on the element or an ancestor, display:contents, content-visibility:hidden ancestors, plus the opacity / visibility options."),
    ("polyfill.visual-viewport", "visual-viewport.js", "window.visualViewport (= the layout viewport)",
     ["visualViewport"],
     "No pinch zoom in Servo, so scale 1 and offsets 0; width/height/pageLeft/pageTop from the viewport; resize/scroll forwarded from window."),
    ("polyfill.popover", "popover.js", "Popover API (popover attribute, showPopover/hidePopover/togglePopover, popovertarget)",
     ["popover"],
     "Hidden until shown, light dismiss for popover=auto, beforetoggle/toggle events, :popover-open via [data-mb-popover-open] in matches/querySelector. No top layer: open popovers are position:fixed with a high z-index."),
    ("polyfill.offscreen-canvas", "offscreen-canvas.js", "OffscreenCanvas (main thread, backed by a detached canvas)",
     ["OffscreenCanvas"],
     "getContext / convertToBlob / transferToImageBitmap on a detached <canvas>. No transferControlToOffscreen and nothing in workers, so worker-rendering sites keep their fallback path."),
    ("polyfill.indexeddb", "indexeddb.js", "IndexedDB (in memory, for the life of the page)",
     ["indexedDB"],
     "Spec-shaped IndexedDB (open/upgrade, object stores, indexes incl. unique/multiEntry, key ranges, cursors, transactions with rollback on abort). Data lives in memory per document: nothing persists across reloads. Fixes 'indexedDB is not defined' / 'IndexedDB API missing' start-up failures."),
    ("polyfill.dynamics-compressor", "dynamics-compressor.js", "createDynamicsCompressor() fallback (unity-gain pass-through with the compressor's params)",
     ["AudioContext.createDynamicsCompressor"],
     "Servo's Web Audio lacks DynamicsCompressorNode; audio-fingerprint start-up code threw and tinyurl.com never left its loading screen. Audio passes through uncompressed."),
    ("polyfill.svg-geometry", "svg-geometry.js", "SVG geometry DOM (getBBox, getCTM/getScreenCTM, getTotalLength/getPointAtLength, createSVGPoint/Matrix, ...)",
     ["SVGGraphicsElement.getBBox"],
     "Servo's SVG DOM has none of the geometry methods; chart/animation code threw (avast.com 'getBBox is not a function' / 'getScreenCTM is not a function', pages.dev 'getTotalLength is not a function'). Computed from the elements' own geometry attributes (curves sampled), not from layout."),
    ("polyfill.console-methods", "console-methods.js", "Missing console methods (console.table, group, count, time, ...) forward to console.log",
     ["console.table"],
     "Servo's console lacks console.table; squarespace.com's homepage bundle called it unguarded and stopped before rendering ('console.table is not a function')."),
    ("polyfill.performance-measure", "performance-measure.js", "performance.measure() accepts navigation-timing names (fetchStart, domComplete, ...)",
     ["performance.measure(navigation timing name)"],
     "Servo throws OperationError for measure(name, 'fetchStart') ('fetchStart hasn't been implemented', ~90 uncaught errors per aliexpress.com load). Retries with the names turned into times since navigationStart; other errors are thrown as before."),
    ("polyfill.notification", "notification.js", "Notification stub (permission is always \"denied\")",
     ["Notification"],
     "Scripts that read Notification.permission unguarded no longer throw; nothing is ever shown or prompted."),
]
# Built-in *site* rules (engine-bug workarounds reproduced on named sites): no `fills`, a host
# list instead of "*", optional CSS. (id, file, title, hosts, css, note)
MS_HOSTS = ["live.com", "microsoftonline.com", "office.com", "office365.com", "microsoft365.com",
            "cloud.microsoft", "microsoft.com", "azure.com", "outlook.com", "sharepoint.com", "onedrive.com",
            "bing.com", "msn.com", "xbox.com", "skype.com"]
SITE_RULES = [
    ("site.css-animation-implicit-end", "css-animation-end.js",
     "CSS animations with an implicit end keyframe end on the element's own style (Servo freezes them mid-way)",
     MS_HOSTS, "[data-mb-anim-end]{animation-name:none!important}",
     "Servo recomputes running animations' keyframes from the already-animated style when a stylesheet changes (CSS-in-JS), so `@keyframes{0%{opacity:0}}` + fill-mode both froze Microsoft's sign-in card invisible (login.live.com). Ends such animations on the element's own value. Engine fix: upstream issue drafts/stylo-0001."),
    ("site.broadcast-channel-frame-relay", "broadcast-channel-frame-relay.js",
     "BroadcastChannel messages from a frame redirected back to its parent's origin reach the parent",
     MS_HOSTS, None,
     "Servo keeps an iframe in the event loop of the site it started at, so after a cross-site redirect back to the parent's origin (OAuth silent sign-in) BroadcastChannel never reaches the parent: MSAL.js v5 silent sign-in on outlook.live.com timed out after 10 s. Relays the messages with same-origin postMessage."),
]
def q(t):
    return t.replace(chr(92), chr(92) * 2).replace(chr(34), chr(92) + chr(34))
def site_block(rid, fn, title, hosts, css, note):
    js = open(os.path.join(HERE, fn)).read()
    if "'" * 3 in js: sys.exit(f"{fn}: contains ''' which a TOML literal string cannot hold")
    hl = ", ".join('"%s"' % h for h in hosts)
    out = f'[[rule]]\nid = "{rid}"\ntitle = "{q(title)}"\nkind = "site"\nhosts = [{hl}]\nnote = "{q(note)}"\n'
    if css: out += f'css = "{q(css)}"\n'
    return out + "js = " + "'" * 3 + "\n" + js + "'" * 3 + "\n"
def block(rid, fn, title, fills, note):
    js = open(os.path.join(HERE, fn)).read()
    if "'''" in js: sys.exit(f"{fn}: contains ''' which a TOML literal string cannot hold")
    fl = ", ".join('"%s"' % f for f in fills)
    return (f'[[rule]]\nid = "{rid}"\ntitle = "{title.replace(chr(34), chr(92) + chr(34))}"\nkind = "polyfill"\nhosts = ["*"]\n'
            f'fills = [{fl}]\nnote = "{note.replace(chr(34), chr(92) + chr(34))}"\njs = \'\'\'\n{js}\'\'\'\n')
def main():
    core = open(CORE).read()
    q3 = "'" * 3
    items = [(r[0], r[1], block(*r)) for r in RULES] + [(r[0], r[1], site_block(*r)) for r in SITE_RULES]
    for rid, fn, new in items:
        m = re.search(r'\[\[rule\]\]\nid = "%s"\n' % re.escape(rid), core)
        if m:
            # Only the js literal is replaced: hand-written metadata (exclude_hosts and its
            # comment, a changed note) stays. The literal ends at the first line that is exactly
            # three quotes after the "js = " line.
            js_at = core.index("js = " + q3 + "\n", m.start()) + len("js = " + q3 + "\n")
            end = core.index("\n" + q3 + "\n", js_at - 1) + 1
            core = core[:js_at] + open(os.path.join(HERE, fn)).read() + core[end:]
        else:
            core = core.rstrip("\n") + "\n\n" + new
    open(CORE, "w").write(core)
    print("core.toml synced:", ", ".join(r[0] for r in RULES + SITE_RULES))
main()
