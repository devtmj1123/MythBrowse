// MythBrowse compat: missing console methods (rule polyfill.console-methods).
// Servo's console lacks some of the less common methods (console.table in the top-1000 run).
// Sites that call them unguarded throw "console.table is not a function" and the rest of that
// script never runs (squarespace.com's homepage bundle stopped before it rendered anything).
// Each missing method forwards to console.log (or a no-op), so output still reaches the log.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
(function () {
  var c = window.console;
  if (!c) return;
  var log = typeof c.log === "function" ? c.log : function () {};
  function def(name, fn) {
    if (typeof c[name] === "function") return;
    try { Object.defineProperty(c, name, { value: fn, configurable: true, writable: true, enumerable: true }); }
    catch (e) { try { c[name] = fn; } catch (e2) {} }
  }
  function fwd() { try { log.apply(c, arguments); } catch (e) {} }
  ["debug", "info", "warn", "error", "trace", "dir", "dirxml", "group", "groupCollapsed"].forEach(function (n) { def(n, fwd); });
  def("groupEnd", function () {});
  def("table", function (data) { fwd(data); });
  def("assert", function (cond) {
    if (cond) return;
    fwd.apply(null, ["Assertion failed:"].concat(Array.prototype.slice.call(arguments, 1)));
  });
  var counts = Object.create(null), timers = Object.create(null);
  def("count", function (label) { label = label === undefined ? "default" : String(label); counts[label] = (counts[label] || 0) + 1; fwd(label + ": " + counts[label]); });
  def("countReset", function (label) { counts[label === undefined ? "default" : String(label)] = 0; });
  var now = function () { return (window.performance && performance.now) ? performance.now() : Date.now(); };
  def("time", function (label) { timers[label === undefined ? "default" : String(label)] = now(); });
  def("timeLog", function (label) { label = label === undefined ? "default" : String(label); if (label in timers) fwd(label + ": " + (now() - timers[label]) + " ms"); });
  def("timeEnd", function (label) { label = label === undefined ? "default" : String(label); if (label in timers) { fwd(label + ": " + (now() - timers[label]) + " ms"); delete timers[label]; } });
})();
