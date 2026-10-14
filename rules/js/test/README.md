# Polyfill tests

* `node rules/js/test/indexeddb.test.js`: behaviour of the in-memory IndexedDB shim (open/upgrade,
  stores, indexes, cursors, key ranges, rollback on abort, version errors).
* `node --check rules/js/*.js`: syntax of every polyfill source.
* `cargo test -p mythbrowse-compat`: core.toml parses, every rule fills a feature the bootstrap
  checks, and core.toml carries rules/js/*.js verbatim (`core_rules_match_js_sources`).
* In the engine: serve `polyfills.html` over http with the compat user scripts installed and run
  `polyfills_check.js` (WebDriver execute/sync or the console). It returns `{ok, fails, results}`
  for checkVisibility, visualViewport, popover, OffscreenCanvas, document.fonts/FontFace,
  IntersectionObserver (real geometry), indexedDB and Notification. The runner is the maintainers' top-sites harness (stock servoshell 0.5.0); last result: 31/31 pass.

## Site rules (engine-bug workarounds)

The built-in site rules only run on their hosts, so their in-engine checks install the rule's
JS (and CSS) directly as a user script; the runner is the maintainers' top-sites harness.

* `anim-end.html` + `anim_end_check.js`: `site.css-animation-implicit-end`. Animations with an
  implicit end keyframe end on the element's own value after a stylesheet change half-way;
  explicit keyframes keep their fill-forwards value. Without the rule Servo 0.5 gives
  d1 opacity ~0.5, d3 a ~50px transform. Last result (2026-10-12): 5/5 with the rule.
* `site.broadcast-channel-frame-relay`: `bc-relay-parent.html` frames a URL on another origin
  (127.0.0.2:8767/r, a 302 back to 127.0.0.1:8765/bc-relay-child.html) whose BroadcastChannel
  message must reach the parent once; without the rule it never arrives (Servo keeps the frame in
  the first site's event loop). Last result (2026-10-12): delivered once with the rule, and
  once (unchanged) in the same-origin case.
