// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/.
// Copyright (c) 2026 JustMyth / Tan Ming Jing
//
// Page context for the Myth panel (crates/ai/src/page_chat.rs). Runs in the page only when the
// user sends a message that includes the page. Reads visible text blocks (paragraphs, list
// items, headings, table cells, ...) as numbered lines "[n] text" and, when asked, the current
// selection. Never reads form controls (inputs, textareas, selects, buttons), editable areas or
// hidden elements. Changes nothing in the page. The browser caps everything again in Rust.
(function (o) {
  var BLOCKS = "p,li,h1,h2,h3,h4,h5,h6,blockquote,pre,td,th,dt,dd,figcaption,caption,summary";
  var SKIP = "script,style,noscript,template,form,input,textarea,select,button,[contenteditable],[aria-hidden=true],[hidden]";
  function clean(s) { return String(s || "").replace(/\s+/g, " ").trim(); }
  function visible(el) {
    if (el.closest && el.closest(SKIP)) return false;
    var r = el.getClientRects ? el.getClientRects() : null;
    return !r || r.length > 0;
  }
  function blocks() {
    var out = [];
    var body = document.body;
    if (!body || !body.querySelectorAll) return out;
    var list = body.querySelectorAll(BLOCKS);
    for (var i = 0; i < list.length; i++) {
      var el = list[i];
      // Nested blocks (a <p> inside an <li>) are covered by the outer one.
      var up = el.parentElement && el.parentElement.closest ? el.parentElement.closest(BLOCKS) : null;
      if (up) continue;
      if (!visible(el)) continue;
      var t = clean(el.innerText !== undefined ? el.innerText : el.textContent);
      if (t) out.push({ el: el, text: t });
    }
    if (!out.length) {
      var all = clean(body.innerText !== undefined ? body.innerText : body.textContent);
      if (all) out.push({ el: body, text: all });
    }
    return out;
  }
  if (o.reveal !== undefined) {
    var b = blocks()[o.reveal - 1];
    if (b && b.el.scrollIntoView) b.el.scrollIntoView({ block: "center", behavior: "smooth" });
    return "";
  }
  var res = { url: String(location.href), title: clean(document.title), lang: String(document.documentElement.lang || ""),
              text: "", blocks: 0, truncated: false, selection: "", sel_truncated: false };
  try {
    var lines = [], used = 0, list = blocks();
    for (var i = 0; i < list.length; i++) {
      var line = "[" + (i + 1) + "] " + list[i].text;
      if (used + line.length + 1 > o.max) { res.truncated = true; break; }
      lines.push(line);
      used += line.length + 1;
    }
    res.text = lines.join("\n");
    res.blocks = lines.length;
    if (o.selection && window.getSelection) {
      var a = document.activeElement;
      var inField = a && a.closest && a.closest("input,textarea,select,[contenteditable]");
      if (!inField) {
        var s = String(window.getSelection() || "");
        if (s.length > o.selMax) { s = s.slice(0, o.selMax); res.sel_truncated = true; }
        res.selection = s.trim();
      }
    }
  } catch (e) {
    res.text = "";
  }
  return JSON.stringify(res);
})(__MB_OPTS__);
