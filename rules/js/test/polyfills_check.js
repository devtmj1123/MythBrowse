// In-engine checks for the rules/js polyfills (run on polyfills.html via WebDriver
// execute/sync or the devtools console; returns a JSON string {ok, fails, results}).
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
var R = {}, F = [];
var t = function (name, fn) {
  try { var v = fn(); R[name] = v; if (v !== true) F.push(name + "=" + JSON.stringify(v)); }
  catch (e) { R[name] = "ERR " + e; F.push(name + ": " + e); }
};
var $ = function (id) { return document.getElementById(id); };
// checkVisibility
t("cv.visible", function () { return $("vis").checkVisibility(); });
t("cv.hidden", function () { return $("hid").checkVisibility() === false; });
t("cv.inHidden", function () { return $("inhid").checkVisibility() === false; });
t("cv.visibilityOpt", function () { return $("vh").checkVisibility() === true && $("vh").checkVisibility({ visibilityProperty: true }) === false; });
t("cv.opacityOpt", function () { return $("op").checkVisibility({ checkOpacity: true }) === false; });
t("cv.detached", function () { return document.createElement("p").checkVisibility() === false; });
// visualViewport
t("vv.object", function () { return typeof visualViewport === "object" && visualViewport.scale === 1; });
t("vv.size", function () { return visualViewport.width > 0 && visualViewport.height > 0 && visualViewport.width <= innerWidth; });
t("vv.instanceof", function () { return visualViewport instanceof VisualViewport; });
t("vv.listener", function () { var n = 0; var f = function () { n++; }; visualViewport.addEventListener("resize", f); visualViewport.dispatchEvent({ type: "resize" }); visualViewport.removeEventListener("resize", f); return n === 1; });
// popover
t("pop.hiddenInitially", function () { return getComputedStyle($("pop")).display === "none" && $("pop").popover === "auto"; });
t("pop.show", function () { $("pop").showPopover(); return getComputedStyle($("pop")).display !== "none" && $("pop").matches(":popover-open"); });
t("pop.query", function () { return document.querySelector(":popover-open") === $("pop"); });
t("pop.hide", function () { $("pop").hidePopover(); return getComputedStyle($("pop")).display === "none"; });
t("pop.toggle", function () { return $("pop").togglePopover() === true && $("pop").togglePopover() === false; });
t("pop.button", function () { $("btn").click(); var open = $("pop").matches(":popover-open"); $("btn").click(); return open && !$("pop").matches(":popover-open"); });
t("pop.autoClosesOther", function () { $("man").showPopover(); $("pop").showPopover(); var both = $("man").matches(":popover-open") && $("pop").matches(":popover-open"); $("pop").hidePopover(); $("man").hidePopover(); return both; });
t("pop.beforetoggleCancel", function () { var f = function (e) { e.preventDefault(); }; $("pop").addEventListener("beforetoggle", f); $("pop").showPopover(); $("pop").removeEventListener("beforetoggle", f); return !$("pop").matches(":popover-open"); });
// OffscreenCanvas
t("oc.ctor", function () { var c = new OffscreenCanvas(20, 10); return c.width === 20 && c.height === 10; });
t("oc.2d", function () { var c = new OffscreenCanvas(4, 4); var g = c.getContext("2d"); g.fillStyle = "#f00"; g.fillRect(0, 0, 4, 4); return g.getImageData(1, 1, 1, 1).data[0] === 255; });
t("oc.blob", function () { return typeof new OffscreenCanvas(2, 2).convertToBlob().then === "function"; });
// fonts
t("fonts.load", function () { return typeof document.fonts.load === "function" && typeof document.fonts.load("12px x").then === "function"; });
t("fonts.check", function () { return document.fonts.check("12px serif") === true; });
t("fonts.ready", function () { return typeof document.fonts.ready.then === "function"; });
t("fonts.FontFace", function () { var f = new FontFace("MbTest", "url(data:font/woff2;base64,AA==)"); document.fonts.add(f); var ok = document.fonts.has(f); var st = document.querySelector("style[data-mb-fontfaces]"); return ok && !!st && st.textContent.indexOf("MbTest") > 0; });
// IntersectionObserver (set up by the page; real geometry, not "always visible")
t("io.visibleTrue", function () { return !!(window.__io.first && window.__io.first.vis === true); });
t("io.belowFalse", function () { return !!(window.__io.first && window.__io.first.below === false); });
t("io.afterScroll", function () { return window.__io.afterScroll === true; });
t("io.fewCalls", function () { return window.__io.calls <= 4; });
// indexedDB / Notification presence (behaviour is covered by the node test)
t("idb.present", function () { return typeof indexedDB.open === "function" && typeof IDBKeyRange.only === "function"; });
t("notification", function () { return Notification.permission === "denied" && typeof Notification.requestPermission === "function"; });
return JSON.stringify({ ok: F.length === 0, fails: F, results: R });
