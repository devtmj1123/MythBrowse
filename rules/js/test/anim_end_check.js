// Run >= 2 s after loading anim-end.html with the rule's JS + CSS installed (see README.md).
// Returns {ok, fails, results}. Copyright (c) 2026 JustMyth / Tan Ming Jing. SPDX-License-Identifier: MPL-2.0.
var want = { d1: ["opacity", "1"], d2: ["opacity", "1"], d3: ["transform", "none"], d4: ["opacity", "0.3"], d5: ["opacity", "0.7"] };
var results = {}, fails = 0;
Object.keys(want).forEach(function (id) {
  var got = getComputedStyle(document.getElementById(id))[want[id][0]];
  var ok = got === want[id][1] || Math.abs(parseFloat(got) - parseFloat(want[id][1])) < 0.01;
  results[id] = (ok ? "ok " : "FAIL ") + want[id][0] + "=" + got + " (want " + want[id][1] + ")";
  if (!ok) fails++;
});
return { ok: fails === 0, fails: fails, results: results };
