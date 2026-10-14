// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// MythBrowse page input (a user script in every web page; crates/engine/src/page_input.rs).
//
// 1. Text selection. Servo 0.1.4 selects text only inside form fields: in page text a drag,
//    a double / triple click or Ctrl+A select nothing and nothing is painted, so Ctrl+C has
//    nothing to copy. It also has no caretPositionFromPoint and Range.getClientRects returns
//    whole text-node boxes. So this script does the hit-testing itself (text-node boxes +
//    canvas measureText within a line), keeps the real DOM Selection in step (so
//    getSelection() works for sites and the context menu) and paints the highlight in an
//    overlay (pointer-events: none). Copy: on the trusted `copy` event Servo fires for
//    Ctrl+C / the menu's Copy, the selected text goes to clipboardData (Servo then writes
//    the system clipboard). Form fields keep Servo's native selection.
// 2. Links: Ctrl+click and middle click open a new background tab, Ctrl+Shift+click (and
//    Shift+middle click) a foreground one. Reported on the console channel
//    (`\u0001mythbrowse:open {...}`); the shell only honours it right after a real click.
// 3. Selection pill (Myth panel, docs/AI-OVERLAY.md): after the user selects page text with
//    the mouse, the top frame reports the selection's bounding box (viewport CSS px) and its
//    length on `\u0001mythbrowse:sel {...}`, and `{"len":0}` once it is gone or the page
//    scrolls. Never the text itself: the shell reads it only when the user picks an action.
// 4. Scrolling (v0.4.2). The shell scrolls pages on Servo's compositor path (no script round
//    trip per frame), which fires no DOM `wheel` events. Pages that need them (maps, custom
//    scrollers that call preventDefault) register a non-passive wheel listener on an element:
//    this script notices (addEventListener / onwheel, as Chrome's passive-by-default rule:
//    listeners on window, document, <html>, <body> count as passive unless passive: false)
//    and reports `\u0001mythbrowse:wheel {"top":bool}` once per document; the shell then
//    asks `__mbWheelBlockingAt(x, y)` at the start of a wheel gesture and sends that gesture
//    through the DOM path when the pointer is over such an element.
// 5. Middle-click autoscroll: a trusted middle press that is not on a link, a form field or
//    media, in a page that can scroll, is reported as `\u0001mythbrowse:autoscroll {}` (and
//    its default prevented); the shell honours it only right after a real middle press and
//    runs the autoscroll itself (compositor path).
// 6. Media (v0.4.2): a <video> whose source failed as unsupported (this build has no media
//    backend) shows a quiet "can't play in this preview" background instead of an empty box.
// 7. Find in page (v0.4.2, Ctrl+F): Servo has no native find. `window.__mbFind(q, dir)`
//    (top frame only) finds the query in rendered page text (case-insensitive, within one
//    text node), paints every match in an overlay like the selection (pointer-events: none,
//    current match stronger), scrolls the current one into view and returns
//    "<current>/<total>"; `window.__mbFindClear()` removes it all. The page's DOM is not
//    changed apart from the overlay element.
// 8. Links with a scheme the engine cannot load (`mailto:`, `tel:`, app links): a trusted
//    left click that the page did not handle itself is cancelled and reported as
//    `\u0001mythbrowse:external {"href":...}`; the shell decides (hand `mailto:` / `tel:` /
//    `sms:` to the OS right after the real click, refuse the rest with a note).
(function () {
  "use strict";
  if (window.__mbPageInput) return;
  window.__mbPageInput = true;
  var MARK = "\u0001mythbrowse:open ";
  var doc = document;
  var HL = "rgba(0, 120, 215, 0.32)";
  var MAX_NODES = 4000;
  var SELMARK = "\u0001mythbrowse:sel ";
  var TOP = window.top === window;
  var WHEELMARK = "\u0001mythbrowse:wheel ";
  var ASMARK = "\u0001mythbrowse:autoscroll ";
  var EXTMARK = "\u0001mythbrowse:external ";
  var selBox = null;      // union of the painted boxes (viewport px)
  var selReported = false;

  // ------------------------------------------------------------------ wheel listeners
  // Elements (and frames) with a wheel listener that may call preventDefault.
  var blockingWheel = typeof WeakSet === "function" ? new WeakSet() : null;
  var blockingAny = false, wheelReported = false;
  function passiveByDefault(t) {
    return t === window || t === doc || t === doc.documentElement || t === doc.body;
  }
  function noteWheel(target, type, options) {
    if (type !== "wheel" && type !== "mousewheel" && type !== "DOMMouseScroll") return;
    var passive = null;
    if (options && typeof options === "object" && "passive" in options) passive = !!options.passive;
    if (passive === true) return;
    if (passive === null && passiveByDefault(target)) return;
    if (blockingWheel && target && typeof target === "object") {
      try { blockingWheel.add(target); } catch (e) { /* not an object we can key on */ }
    }
    blockingAny = true;
    if (!wheelReported) {
      wheelReported = true;
      try { console.log(WHEELMARK + JSON.stringify({ top: TOP })); } catch (e) { /* ignore */ }
    }
  }
  (function () {
    var ET = window.EventTarget && EventTarget.prototype;
    if (!ET || !ET.addEventListener) return;
    var add = ET.addEventListener;
    var hooked = function addEventListener(type, listener, options) {
      if (listener) noteWheel(this, String(type), options);
      return add.apply(this, arguments);
    };
    try {
      Object.defineProperty(ET, "addEventListener", { value: hooked, writable: true, configurable: true });
    } catch (e) { /* frozen: keep the original */ }
    ["onwheel", "onmousewheel"].forEach(function (name) {
      [window.HTMLElement && HTMLElement.prototype, window.Document && Document.prototype, window.Window && Window.prototype].forEach(function (proto) {
        if (!proto) return;
        var d = Object.getOwnPropertyDescriptor(proto, name);
        if (!d || !d.set || !d.configurable) return;
        Object.defineProperty(proto, name, {
          configurable: true, enumerable: d.enumerable, get: d.get,
          set: function (v) { if (v) noteWheel(this, name.slice(2), null); return d.set.call(this, v); }
        });
      });
    });
  })();
  // Does the element under (x, y) (viewport CSS px) or one of its ancestors have such a
  // listener? An <iframe> answers "frame" (its own document decides: the shell asks that
  // gesture through the DOM path if any frame reported).
  window.__mbWheelBlockingAt = function (x, y) {
    if (!blockingAny) return "no";
    var el = doc.elementFromPoint(x, y);
    if (!el) return "no";
    if (!blockingWheel) return "yes";
    for (var n = el; n; n = n.parentNode || n.host) {
      if (blockingWheel.has(n)) return "yes";
      if (n === doc) break;
    }
    if (el.localName === "iframe" || el.localName === "frame") return "frame";
    return "no";
  };

  // ------------------------------------------------------------------ helpers
  function editable(el) {
    for (var n = el; n && n.nodeType === 1; n = n.parentNode) {
      var t = n.localName;
      if (t === "input" || t === "textarea" || t === "select" || t === "option") return true;
      if (n.isContentEditable || n.getAttribute("contenteditable") === "" || n.getAttribute("contenteditable") === "true") return true;
    }
    return false;
  }
  function noSelect(el) {
    try {
      var cs = getComputedStyle(el);
      var v = cs.userSelect || cs.webkitUserSelect || cs.getPropertyValue("-webkit-user-select");
      return v === "none";
    } catch (e) { return false; }
  }
  function linkOf(el) {
    for (var n = el; n && n.nodeType === 1; n = n.parentNode) {
      if ((n.localName === "a" || n.localName === "area") && n.href) return n;
    }
    return null;
  }
  function elementOf(node) { return node && node.nodeType === 3 ? node.parentNode : node; }
  function rectsOf(node) {
    var r = doc.createRange();
    try { r.selectNodeContents(node); } catch (e) { return []; }
    var list = r.getClientRects(), out = [];
    for (var i = 0; i < list.length; i++) if (list[i].width > 0 && list[i].height > 0) out.push(list[i]);
    return out;
  }
  function textNodesIn(root, limit) {
    var out = [];
    if (!root) return out;
    var w = doc.createTreeWalker(root, 4 /* SHOW_TEXT */, null);
    var n;
    while ((n = w.nextNode()) && out.length < limit) {
      if (/\S/.test(n.data)) out.push(n);
    }
    return out;
  }

  // Canvas text measurement in the node's own font.
  var ctx = null;
  function measurer(el) {
    if (!ctx) {
      try { ctx = doc.createElement("canvas").getContext("2d"); } catch (e) { ctx = null; }
      if (!ctx) return null;
    }
    var cs = getComputedStyle(el);
    ctx.font = [cs.fontStyle, cs.fontWeight, cs.fontSize, cs.fontFamily].join(" ");
    var ls = parseFloat(cs.letterSpacing) || 0;
    var upper = cs.textTransform === "uppercase", lower = cs.textTransform === "lowercase";
    return function (s) {
      if (upper) s = s.toUpperCase(); else if (lower) s = s.toLowerCase();
      return ctx.measureText(s).width + ls * s.length;
    };
  }

  // Lay a text node's characters out over its line boxes (greedy, breaking at spaces), the
  // way the engine wrapped it. Returns per-line {rect, start, end, xs[]} with xs the x of
  // every caret position in [start, end]. Cached per node until the layout may change.
  var layoutCache = new Map();
  function layoutOf(node) {
    var hit = layoutCache.get(node);
    if (hit) return hit;
    var rects = rectsOf(node), el = elementOf(node), m = el && measurer(el), s = node.data;
    var lines = [];
    if (!rects.length) { layoutCache.set(node, lines); return lines; }
    var pre = /^pre/.test(getComputedStyle(el).whiteSpace || "");
    var i = 0;
    // Leading collapsible white space is not drawn.
    if (!pre) while (i < s.length && /\s/.test(s[i])) i++;
    for (var li = 0; li < rects.length; li++) {
      var r = rects[li], xs = [r.left], start = i, x = r.left, lastBreak = -1, lastBreakXs = 0;
      var last = li === rects.length - 1;
      while (i < s.length) {
        var c = s[i];
        if (!pre && /\s/.test(c)) {
          // Collapsed run of white space: one space wide.
          var j = i; while (j < s.length && /\s/.test(s[j])) j++;
          var w = m ? m(" ") : r.width / Math.max(1, s.length);
          if (!last && x + w > r.right + 1) { // the break is here
            for (var k = i; k < j; k++) xs.push(x);
            i = j; break;
          }
          for (var k2 = i; k2 < j; k2++) xs.push(k2 === j - 1 ? x + w : x);
          lastBreak = j; lastBreakXs = xs.length;
          x += w; i = j; continue;
        }
        if (pre && c === "\n") { xs.push(x); i++; if (!last) break; continue; }
        var cw = m ? m(c) : r.width / Math.max(1, s.length);
        if (!last && x + cw > r.right + 1 && i > start) {
          if (lastBreak > start) { // wrap at the last space
            i = lastBreak;
            xs = xs.slice(0, lastBreakXs);
          }
          break;
        }
        x += cw; xs.push(Math.min(x, r.right + 2)); i++;
      }
      lines.push({ rect: r, start: start, end: i, xs: xs });
    }
    layoutCache.set(node, lines);
    return lines;
  }
  function clearLayout() { layoutCache = new Map(); }

  // Caret position {node, offset} nearest to client point (x, y), or null.
  function caretAt(x, y) {
    var el = doc.elementFromPoint(x, y);
    if (!el) return null;
    var scope = el, nodes = [];
    // Climb until the subtree has text (a click in a padding area of a paragraph).
    for (var up = 0; scope && up < 4; up++, scope = scope.parentElement) {
      nodes = textNodesIn(scope, MAX_NODES);
      if (nodes.length) break;
    }
    if (!nodes.length) return null;
    var best = null, bestD = Infinity;
    for (var n = 0; n < nodes.length; n++) {
      var lines = layoutOf(nodes[n]);
      for (var l = 0; l < lines.length; l++) {
        var r = lines[l].rect;
        var dy = y < r.top ? r.top - y : y > r.bottom ? y - r.bottom : 0;
        var dx = x < r.left ? r.left - x : x > r.right ? x - r.right : 0;
        var d = dy * 1000 + dx; // same line first
        if (d < bestD) { bestD = d; best = { node: nodes[n], line: lines[l] }; }
        if (d === 0) break;
      }
      if (bestD === 0) break;
    }
    if (!best) return null;
    var ln = best.line, xs = ln.xs, off = ln.start;
    for (var k = 0; k < xs.length - 1; k++) {
      if (x >= (xs[k] + xs[k + 1]) / 2) off = ln.start + k + 1; else break;
    }
    return { node: best.node, offset: Math.min(off, best.node.data.length) };
  }

  function cmp(a, b) {
    if (a.node === b.node) return a.offset - b.offset;
    var p = a.node.compareDocumentPosition(b.node);
    return p & 4 /* FOLLOWING */ ? -1 : 1;
  }

  // ------------------------------------------------------------------ selection state
  var sel = null; // { anchor, focus } caret positions
  var overlay = null;

  function domSelect() {
    var s = window.getSelection && getSelection();
    if (!s) return;
    try {
      if (!sel) { if (s.rangeCount) s.removeAllRanges(); return; }
      s.setBaseAndExtent(sel.anchor.node, sel.anchor.offset, sel.focus.node, sel.focus.offset);
    } catch (e) { /* node gone */ }
  }

  function ensureOverlay() {
    if (overlay && overlay.isConnected) return overlay;
    overlay = doc.createElement("mb-selection");
    overlay.setAttribute("aria-hidden", "true");
    overlay.style.cssText = "position:absolute;left:0;top:0;width:0;height:0;margin:0;padding:0;border:0;pointer-events:none;z-index:2147483647;contain:layout style;";
    (doc.documentElement || doc.body).appendChild(overlay);
    return overlay;
  }

  function lineRange(ln, from, to) {
    var a = Math.max(from, ln.start), b = Math.min(to, ln.end);
    if (b <= a) return null;
    var x0 = ln.xs[a - ln.start], x1 = ln.xs[Math.min(b - ln.start, ln.xs.length - 1)];
    if (x0 === undefined || x1 === undefined || x1 <= x0) return null;
    return { left: x0, right: x1, top: ln.rect.top, bottom: ln.rect.bottom };
  }

  var paintQueued = false;
  function paint() {
    if (paintQueued) return;
    paintQueued = true;
    requestAnimationFrame(function () { paintQueued = false; paintNow(); });
  }
  function paintNow() {
    if (!sel || cmp(sel.anchor, sel.focus) === 0) {
      if (overlay) overlay.textContent = "";
      selBox = null;
      return;
    }
    var o = ensureOverlay();
    var a = sel.anchor, f = sel.focus;
    var s = cmp(a, f) < 0 ? a : f, e = s === a ? f : a;
    var boxes = [];
    var range = doc.createRange();
    try { range.setStart(s.node, s.offset); range.setEnd(e.node, e.offset); } catch (err) { return; }
    var root = range.commonAncestorContainer;
    var nodes = root.nodeType === 3 ? [root] : textNodesIn(root, MAX_NODES);
    for (var i = 0; i < nodes.length && boxes.length < 2000; i++) {
      var n = nodes[i];
      if (n !== s.node && n !== e.node && !range.intersectsNode(n)) continue;
      if (n.parentNode && n.parentNode.closest && n.parentNode.closest("script,style,noscript,mb-selection")) continue;
      var from = n === s.node ? s.offset : 0, to = n === e.node ? e.offset : n.data.length;
      var lines = layoutOf(n);
      for (var l = 0; l < lines.length; l++) {
        var b = lineRange(lines[l], from, to);
        if (b) boxes.push(b);
      }
    }
    selBox = null;
    for (var q = 0; q < boxes.length; q++) {
      var bq = boxes[q];
      if (!selBox) selBox = { l: bq.left, t: bq.top, r: bq.right, b: bq.bottom };
      else {
        selBox.l = Math.min(selBox.l, bq.left); selBox.t = Math.min(selBox.t, bq.top);
        selBox.r = Math.max(selBox.r, bq.right); selBox.b = Math.max(selBox.b, bq.bottom);
      }
    }
    var sx = window.scrollX, sy = window.scrollY, html = "";
    for (var k = 0; k < boxes.length; k++) {
      var bx = boxes[k];
      html += '<div style="position:absolute;left:' + (bx.left + sx).toFixed(1) + "px;top:" + (bx.top + sy).toFixed(1) +
        "px;width:" + (bx.right - bx.left).toFixed(1) + "px;height:" + (bx.bottom - bx.top).toFixed(1) +
        "px;background:" + HL + ';"></div>';
    }
    o.innerHTML = html;
  }

  function setSel(anchor, focus) {
    sel = anchor && focus ? { anchor: anchor, focus: focus } : null;
    domSelect();
    paint();
  }
  function clearSel() { if (sel) setSel(null, null); else if (overlay) overlay.textContent = ""; }
  function selectedText() {
    var s = window.getSelection && getSelection();
    var t = s ? String(s) : "";
    return t;
  }

  // Word around a caret (double click).
  var WORD = /[\p{L}\p{N}_'\u2019-]/u;
  function wordAt(c) {
    var s = c.node.data, a = c.offset, b = c.offset;
    if (a < s.length && !WORD.test(s[a]) && a > 0 && WORD.test(s[a - 1])) a--, b--;
    if (a < s.length && !WORD.test(s[a])) return [{ node: c.node, offset: a }, { node: c.node, offset: Math.min(a + 1, s.length) }];
    while (a > 0 && WORD.test(s[a - 1])) a--;
    while (b < s.length && WORD.test(s[b])) b++;
    return [{ node: c.node, offset: a }, { node: c.node, offset: b }];
  }
  // Block around a caret (triple click).
  function blockAt(c) {
    var el = elementOf(c.node);
    while (el && el !== doc.body) {
      var d = getComputedStyle(el).display;
      if (d !== "inline" && d !== "contents") break;
      el = el.parentElement;
    }
    var nodes = textNodesIn(el || doc.body, MAX_NODES);
    if (!nodes.length) return null;
    var last = nodes[nodes.length - 1];
    return [{ node: nodes[0], offset: 0 }, { node: last, offset: last.data.length }];
  }

  // ------------------------------------------------------------------ autoscroll
  // A middle press here may start autoscroll: not on a form field, media or a plugin, and
  // only when something under the pointer (or the page) can scroll.
  function canScroll(el) {
    if (el.scrollHeight <= el.clientHeight + 1 && el.scrollWidth <= el.clientWidth + 1) return false;
    if (el === doc.scrollingElement || el === doc.documentElement || el === doc.body) return true;
    var cs = getComputedStyle(el);
    return /(auto|scroll)/.test(cs.overflowY + " " + cs.overflowX);
  }
  function autoscrollable(t) {
    if (!t || t.nodeType !== 1 || editable(t)) return false;
    for (var n = t; n && n.nodeType === 1; n = n.parentElement) {
      var l = n.localName;
      if (l === "video" || l === "audio" || l === "embed" || l === "object" || l === "canvas" || l === "select") return false;
    }
    for (var m = t; m && m.nodeType === 1; m = m.parentElement) {
      if (canScroll(m)) return true;
    }
    var se = doc.scrollingElement || doc.documentElement;
    return !!se && canScroll(se);
  }

  // ------------------------------------------------------------------ pointer
  var drag = null; // { start: caret, moved, x, y, mode: 1|2|3, unit: [a, b] }
  var movePending = null;

  doc.addEventListener("mousedown", function (e) {
    if (!e.isTrusted) return;
    if (e.button === 1) {
      // Middle button: no page-level default (the click is handled on mouseup).
      if (linkOf(e.target)) { e.preventDefault(); return; }
      if (!e.defaultPrevented && autoscrollable(e.target)) {
        e.preventDefault();
        console.log(ASMARK + "{}");
      }
      return;
    }
    if (e.button !== 0) return;
    var t = e.target;
    if (!t || t.nodeType !== 1 || editable(t)) { clearSel(); return; }
    if (noSelect(t)) return;
    clearLayout();
    var c = caretAt(e.clientX, e.clientY);
    if (!c) { clearSel(); drag = null; return; }
    var clicks = e.detail || 1;
    if (e.shiftKey && sel && clicks === 1) {
      setSel(sel.anchor, c);
      drag = { start: sel.anchor, moved: true, x: e.clientX, y: e.clientY, mode: 1 };
      return;
    }
    if (clicks === 2) {
      var w = wordAt(c);
      setSel(w[0], w[1]);
      drag = { start: c, moved: true, x: e.clientX, y: e.clientY, mode: 2, unit: w };
      return;
    }
    if (clicks >= 3) {
      var b = blockAt(c);
      if (b) setSel(b[0], b[1]);
      drag = null;
      return;
    }
    // Single click: collapse now, select on drag.
    if (sel) clearSel();
    drag = { start: c, moved: false, x: e.clientX, y: e.clientY, mode: 1 };
  }, true);

  function onMove(x, y) {
    if (!drag) return;
    if (!drag.moved && Math.abs(x - drag.x) + Math.abs(y - drag.y) < 4) return;
    var c = caretAt(x, y);
    if (!c) return;
    drag.moved = true;
    if (drag.mode === 2 && drag.unit) {
      var w = wordAt(c);
      if (cmp(c, drag.unit[0]) < 0) setSel(drag.unit[1], w[0]); else setSel(drag.unit[0], w[1]);
    } else {
      setSel(drag.start, c);
    }
  }
  doc.addEventListener("mousemove", function (e) {
    if (!drag || !e.isTrusted) return;
    if (!(e.buttons & 1) && e.buttons !== undefined && e.buttons !== 0) { drag = null; return; }
    if (movePending) { movePending = [e.clientX, e.clientY]; return; }
    movePending = [e.clientX, e.clientY];
    requestAnimationFrame(function () {
      var p = movePending; movePending = null;
      if (p) onMove(p[0], p[1]);
    });
  }, true);
  doc.addEventListener("mouseup", function (e) {
    if (!e.isTrusted) return;
    if (e.button === 1) { openLink(e, e.shiftKey); return; }
    if (e.button === 0 && drag) {
      if (drag.moved) onMove(e.clientX, e.clientY);
      drag = null;
    }
  }, true);

  // Selection pill: report the box (after the highlight is painted), or that it is gone.
  function reportSel() {
    if (!TOP) return;
    requestAnimationFrame(function () {
      requestAnimationFrame(function () {
        var on = sel && cmp(sel.anchor, sel.focus) !== 0 && selBox;
        if (on) {
          var n = selectedText().length;
          if (!n) on = false;
          else {
            console.log(SELMARK + JSON.stringify({
              len: n, x: Math.round(selBox.l), y: Math.round(selBox.t),
              w: Math.round(selBox.r - selBox.l), h: Math.round(selBox.b - selBox.t)
            }));
            selReported = true;
            return;
          }
        }
        if (selReported) { console.log(SELMARK + '{"len":0}'); selReported = false; }
      });
    });
  }
  doc.addEventListener("mouseup", function (e) {
    if (e.isTrusted && e.button === 0) reportSel();
  }, true);
  window.addEventListener("scroll", function () {
    if (selReported) { console.log(SELMARK + '{"len":0}'); selReported = false; }
  }, { passive: true });

  // A click that ended a drag-selection must not follow a link.
  var suppressClick = false;
  doc.addEventListener("mouseup", function (e) {
    if (e.button === 0 && sel && cmp(sel.anchor, sel.focus) !== 0 && linkOf(e.target)) suppressClick = true;
  }, false);

  // ------------------------------------------------------------------ links
  function openLink(e, foreground) {
    var a = linkOf(e.target);
    if (!a) return false;
    var href = String(a.href || "");
    if (!/^https?:/i.test(href)) return false;
    e.preventDefault();
    e.stopPropagation();
    console.log(MARK + JSON.stringify({ href: href.slice(0, 8192), foreground: !!foreground }));
    return true;
  }
  doc.addEventListener("click", function (e) {
    if (!e.isTrusted || e.button !== 0) return;
    if (suppressClick) { suppressClick = false; if (linkOf(e.target)) { e.preventDefault(); return; } }
    if (e.ctrlKey || e.metaKey) {
      if (sel && cmp(sel.anchor, sel.focus) !== 0) clearSel();
      openLink(e, e.shiftKey);
    }
  }, true);

  // Links the engine cannot load (mailto:, tel:, app schemes): the shell decides. Bubble
  // phase on the document, so a page that handles the click itself (and cancels it) wins.
  var WEB_SCHEME = /^(https?|javascript|data|blob|about|file|ftp|ws|wss):/i;
  doc.addEventListener("click", function (e) {
    if (!e.isTrusted || e.button !== 0 || e.defaultPrevented) return;
    var a = linkOf(e.target);
    if (!a) return;
    var href = String(a.href || "");
    if (!href || WEB_SCHEME.test(href) || !/^[a-z][a-z0-9+.-]*:/i.test(href)) return;
    e.preventDefault();
    try { console.log(EXTMARK + JSON.stringify({ href: href.slice(0, 8192) })); } catch (err) { /* ignore */ }
  }, false);

  // ------------------------------------------------------------------ keys, copy
  function selectAll() {
    var nodes = textNodesIn(doc.body, 1e9);
    if (!nodes.length) return false;
    clearLayout();
    var last = nodes[nodes.length - 1];
    setSel({ node: nodes[0], offset: 0 }, { node: last, offset: last.data.length });
    return true;
  }
  doc.addEventListener("keydown", function (e) {
    if (!e.isTrusted) return;
    var ae = doc.activeElement;
    if (ae && editable(ae)) return;
    if ((e.ctrlKey || e.metaKey) && !e.altKey && (e.key === "a" || e.key === "A")) {
      if (selectAll()) e.preventDefault();
    } else if (e.key === "Escape" && sel) {
      clearSel();
      reportSel();
    }
  }, false);

  function onCopy(e) {
    if (!e.isTrusted || e.defaultPrevented) return;
    var ae = doc.activeElement;
    if (ae && editable(ae)) return; // Servo copies form-field selections itself
    if (!sel) return;
    var text = selectedText();
    if (!text || !e.clipboardData) return;
    e.clipboardData.setData("text/plain", text);
    e.preventDefault();
  }
  window.addEventListener("copy", onCopy, false);

  // Keep the highlight on the text when the layout changes.
  function relayout() {
    if (sel) { clearLayout(); paint(); }
    if (find) findRepaint();
  }
  window.addEventListener("resize", relayout);
  doc.addEventListener("scroll", function (e) {
    // The window scroll moves the overlay with the page; inner scrollers need a repaint.
    if ((sel || find) && e.target !== doc && e.target !== doc.documentElement) relayout();
  }, true);

  // ------------------------------------------------------------------ media (v0.4.2)
  // This build has no media backend (servo-script 0006): every <video> / <audio> source
  // fails as MEDIA_ERR_SRC_NOT_SUPPORTED, so a <video> would stay an empty box. Such a video
  // gets a dark background saying why, set as an inline style on the element only (no DOM is
  // inserted, so page frameworks never see a change; its useless controls are turned off);
  // a poster keeps showing instead.
  var MEDIA_SVG = "data:image/svg+xml," + encodeURIComponent(
    "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 320 180'>" +
    "<g fill='none' stroke='#9aa3b5' stroke-width='3' stroke-linecap='round' stroke-linejoin='round'>" +
    "<circle cx='160' cy='70' r='22'/><path d='M154 60l14 10-14 10z'/><path d='M140 50l40 40'/></g>" +
    "<text x='160' y='122' fill='#d7dbe4' font-family='Segoe UI,system-ui,sans-serif' font-size='13' text-anchor='middle'>" +
    "Video can’t play in this preview of MythBrowse</text>" +
    "<text x='160' y='140' fill='#9aa3b5' font-family='Segoe UI,system-ui,sans-serif' font-size='11' text-anchor='middle'>" +
    "Audio and video are not supported yet</text></svg>");
  function noMedia(el) {
    if (!el || el.__mbNoMedia || el.tagName !== "VIDEO") return;
    var failed = (el.error && el.error.code === 4) || el.networkState === 3;
    if (!failed) return;
    el.__mbNoMedia = true;
    try {
      // Controls that cannot play anything would cover the message.
      if (el.controls) el.controls = false;
      if (!el.getAttribute("poster")) {
        el.style.setProperty("background", "#161922 url(\"" + MEDIA_SVG + "\") center / contain no-repeat", "important");
      }
      if (!el.getAttribute("title")) el.setAttribute("title", "Video playback is not supported in this preview of MythBrowse");
    } catch (e) { /* a page that froze the element's style */ }
  }
  // Media `error` events do not bubble: listen in the capture phase. With <source> children
  // the error fires at each <source> and the video ends up with networkState NO_SOURCE.
  doc.addEventListener("error", function (e) {
    var t = e.target;
    if (!t || !t.tagName) return;
    if (t.tagName === "VIDEO") noMedia(t);
    else if (t.tagName === "SOURCE" && t.parentNode && t.parentNode.tagName === "VIDEO") {
      var v = t.parentNode;
      setTimeout(function () { noMedia(v); }, 0);
    }
  }, true);

  // ------------------------------------------------------------------ find in page (v0.4.2)
  var FIND_HL = "rgba(255, 214, 0, 0.42)", FIND_CUR = "rgba(255, 138, 0, 0.72)";
  var FIND_MAX = 1000, FIND_SCAN = 60000;
  var find = null; // { q, matches: [{ node, start, end }], cur }
  var findLayer = null, findQueued = false;
  function findLayerEl() {
    if (findLayer && findLayer.isConnected) return findLayer;
    findLayer = doc.createElement("mb-find");
    findLayer.setAttribute("aria-hidden", "true");
    findLayer.style.cssText = "position:absolute;left:0;top:0;width:0;height:0;margin:0;padding:0;border:0;pointer-events:none;z-index:2147483646;contain:layout style;";
    (doc.documentElement || doc.body).appendChild(findLayer);
    return findLayer;
  }
  function findable(n) {
    var p = n.parentNode;
    if (!p || p.nodeType !== 1) return false;
    return !(p.closest && p.closest("script,style,noscript,template,title,mb-selection,mb-find"));
  }
  function findAll(q) {
    var out = [];
    if (!q || !doc.body) return out;
    var fq = q.toLowerCase();
    var w = doc.createTreeWalker(doc.body, 4 /* SHOW_TEXT */, null), n, scanned = 0;
    while ((n = w.nextNode()) && out.length < FIND_MAX && scanned++ < FIND_SCAN) {
      var s = n.data;
      if (s.length < q.length || !findable(n)) continue;
      var ls = s.toLowerCase();
      // Offsets must line up with the node's text: where lowercasing changes the length
      // (rare scripts), match the exact text instead.
      var hay = ls.length === s.length ? ls : s, needle = hay === ls ? fq : q;
      var i = hay.indexOf(needle), first = true;
      while (i >= 0 && out.length < FIND_MAX) {
        if (first) { first = false; if (!layoutOf(n).length) break; } // not rendered
        out.push({ node: n, start: i, end: i + q.length });
        i = hay.indexOf(needle, i + Math.max(1, q.length));
      }
    }
    return out;
  }
  function matchBoxes(m) {
    var lines = layoutOf(m.node), out = [];
    for (var l = 0; l < lines.length; l++) {
      var b = lineRange(lines[l], m.start, m.end);
      if (b) out.push(b);
    }
    return out;
  }
  function boxesInView(bs) {
    var H = window.innerHeight, W = window.innerWidth;
    for (var i = 0; i < bs.length; i++) {
      var b = bs[i];
      if (b.bottom > 0 && b.top < H && b.right > 0 && b.left < W) return true;
    }
    return false;
  }
  function revealCurrent() {
    if (!find || find.cur < 0) return;
    var m = find.matches[find.cur];
    if (boxesInView(matchBoxes(m))) return;
    var el = elementOf(m.node);
    try { el.scrollIntoView({ block: "center", inline: "nearest" }); } catch (e) {
      try { el.scrollIntoView(); } catch (e2) { /* ignore */ }
    }
    clearLayout();
  }
  function paintFind() {
    var o = findLayerEl();
    if (!find || !find.matches.length) { o.textContent = ""; return; }
    clearLayout();
    var sx = window.scrollX, sy = window.scrollY, html = "";
    for (var i = 0; i < find.matches.length; i++) {
      var bs = matchBoxes(find.matches[i]), bg = i === find.cur ? FIND_CUR : FIND_HL;
      for (var k = 0; k < bs.length; k++) {
        var bx = bs[k];
        html += '<div style="position:absolute;left:' + (bx.left + sx - 1).toFixed(1) + "px;top:" + (bx.top + sy).toFixed(1) +
          "px;width:" + (bx.right - bx.left + 2).toFixed(1) + "px;height:" + (bx.bottom - bx.top).toFixed(1) +
          "px;border-radius:2px;background:" + bg + ';"></div>';
      }
    }
    o.innerHTML = html;
  }
  function findRepaint() {
    if (findQueued) return;
    findQueued = true;
    requestAnimationFrame(function () { findQueued = false; if (find) paintFind(); });
  }
  function findStale() {
    if (!find) return true;
    for (var i = 0; i < find.matches.length; i++) if (!find.matches[i].node.isConnected) return true;
    return false;
  }
  // dir: 0 = (re)search (keeps the position when the query grows), 1 = next, -1 = previous.
  window.__mbFind = function (q, dir) {
    if (!TOP) return "";
    q = String(q == null ? "" : q).slice(0, 500);
    if (!q) { window.__mbFindClear(); return "0/0"; }
    if (!find || find.q !== q || dir === 0 || findStale()) {
      var old = find && find.cur >= 0 ? find.matches[find.cur] : null;
      if (old && !old.node.isConnected) old = null;
      clearLayout();
      var all = findAll(q), cur = all.length ? 0 : -1;
      if (old) {
        for (var k = 0; k < all.length; k++) {
          if (cmp({ node: all[k].node, offset: all[k].start }, { node: old.node, offset: old.start }) >= 0) { cur = k; break; }
        }
      }
      find = { q: q, matches: all, cur: cur };
    } else if (find.matches.length) {
      var n = find.matches.length;
      find.cur = (find.cur + (dir > 0 ? 1 : -1) + n) % n;
    }
    revealCurrent();
    paintFind();
    return (find.cur + 1) + "/" + find.matches.length;
  };
  window.__mbFindClear = function () {
    find = null;
    if (findLayer) { try { findLayer.remove(); } catch (e) { /* ignore */ } }
    findLayer = null;
  };

  window.__mbSelection = {
    text: selectedText,
    clear: clearSel,
    selectAll: selectAll,
    active: function () { return !!sel && cmp(sel.anchor, sel.focus) !== 0; },
    // Tests only (hit-testing accuracy against a browser that has caretPositionFromPoint).
    _caretAt: caretAt
  };
})();
