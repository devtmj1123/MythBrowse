// MythBrowse compat: performance.measure() with navigation-timing names (rule
// polyfill.performance-measure).
// performance.measure(name, "fetchStart") (and the other PerformanceTiming attribute names) is
// valid in browsers: the name stands for that navigation-timing time. Servo throws
// ("OperationError: fetchStart hasn't been implemented"), uncaught on aliexpress.com (93 errors
// per load). On that error the call is retried with the names turned into times relative to
// navigationStart; if the engine cannot take the options form either, the measure starts at 0.
// Other errors (an unknown mark name, a bad argument) are thrown as before.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
(function () {
  var P = window.Performance;
  if (!P || !P.prototype || typeof P.prototype.measure !== "function") return;
  var orig = P.prototype.measure;
  if (orig.__mbNav) return;
  var NAV = /^(navigationStart|unloadEvent(Start|End)|redirect(Start|End)|fetchStart|domainLookup(Start|End)|connect(Start|End)|secureConnectionStart|request(Start)|response(Start|End)|domLoading|domInteractive|domContentLoadedEvent(Start|End)|domComplete|loadEvent(Start|End))$/;
  function isNav(x) { return typeof x === "string" && NAV.test(x); }
  function navTime(perf, x) {
    var t = perf.timing, ns = t && t.navigationStart;
    var v = t && t[x];
    if (!ns || !v) return 0;
    return Math.max(0, v - ns);
  }
  function measure(name, a, b) {
    try { return orig.apply(this, arguments); }
    catch (e) {
      var opt = a && typeof a === "object" ? a : null;
      var hasNav = isNav(a) || isNav(b) || (opt && (isNav(opt.start) || isNav(opt.end)));
      if (!hasNav) throw e;
      var self = this, o = {};
      var s = opt ? opt.start : a, en = opt ? opt.end : b;
      if (s !== undefined) o.start = isNav(s) ? navTime(self, s) : s;
      if (en !== undefined) o.end = isNav(en) ? navTime(self, en) : en;
      if (opt && opt.detail !== undefined) o.detail = opt.detail;
      if (opt && opt.duration !== undefined) o.duration = opt.duration;
      try { return orig.call(self, name, o); }
      catch (e2) { return orig.call(self, name); }
    }
  }
  measure.__mbNav = true;
  Object.defineProperty(P.prototype, "measure", { value: measure, configurable: true, writable: true, enumerable: true });
})();
