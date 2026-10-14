// MythBrowse compat: BaseAudioContext.createDynamicsCompressor (rule polyfill.dynamics-compressor).
// Servo's Web Audio has no DynamicsCompressorNode. Audio-fingerprint code (tinyurl.com's
// start-up) calls createDynamicsCompressor() on an OfflineAudioContext, throws, and the app
// never leaves its loading screen. The fallback is a GainNode at unity gain (audio passes
// through uncompressed) with the compressor's AudioParams (threshold, knee, ratio, attack,
// release: the spec defaults, settable, not applied) and `reduction` 0.
// Also guards AudioBuffer.copyFromChannel(): on a buffer whose data has not been handed to JS
// yet (a new AudioBuffer, a rendered OfflineAudioContext result) Servo 0.5 builds a copy twice
// the destination's length and the script thread panics (`data.len() <= buffer.len()`,
// shopee.co.id once this fallback let its fingerprint code reach copyFromChannel). Calling
// getChannelData() first moves the data to JS, where the copy is correct. The engine fix is
// servo-script patch 0005; the guard keeps unpatched engines alive.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
(function () {
  var AB = window.AudioBuffer;
  if (!AB || !AB.prototype || AB.prototype.__mbCopyGuard) return;
  var copy = AB.prototype.copyFromChannel, data = AB.prototype.getChannelData;
  if (typeof copy !== "function" || typeof data !== "function") return;
  Object.defineProperty(AB.prototype, "__mbCopyGuard", { value: true });
  Object.defineProperty(AB.prototype, "copyFromChannel", {
    value: function copyFromChannel(destination, channelNumber) {
      try { data.call(this, channelNumber >>> 0); } catch (e) { /* the real call reports it */ }
      return copy.apply(this, arguments);
    },
    configurable: true, writable: true,
  });
})();
(function () {
  var Base = window.BaseAudioContext || window.AudioContext || window.OfflineAudioContext;
  if (!Base || !Base.prototype || typeof Base.prototype.createGain !== "function") return;
  if (typeof Base.prototype.createDynamicsCompressor === "function") return;
  var param = function (value, min, max) {
    var p = {
      value: value, defaultValue: value, minValue: min, maxValue: max, automationRate: "k-rate",
      setValueAtTime: function (v) { p.value = v; return p; },
      linearRampToValueAtTime: function (v) { p.value = v; return p; },
      exponentialRampToValueAtTime: function (v) { p.value = v; return p; },
      setTargetAtTime: function (v) { p.value = v; return p; },
      setValueCurveAtTime: function () { return p; },
      cancelScheduledValues: function () { return p; },
      cancelAndHoldAtTime: function () { return p; },
    };
    return p;
  };
  var create = function () {
    var node = this.createGain();
    node.gain.value = 1;
    var params = {
      threshold: param(-24, -100, 0), knee: param(30, 0, 40), ratio: param(12, 1, 20),
      attack: param(0.003, 0, 1), release: param(0.25, 0, 1),
    };
    for (var k in params) Object.defineProperty(node, k, { value: params[k], enumerable: true });
    Object.defineProperty(node, "reduction", { value: 0, enumerable: true });
    return node;
  };
  Object.defineProperty(Base.prototype, "createDynamicsCompressor", { value: create, configurable: true, writable: true });
  if (typeof window.DynamicsCompressorNode === "undefined") {
    var D = function DynamicsCompressorNode(context) {
      if (!context || typeof context.createDynamicsCompressor !== "function") throw new TypeError("DynamicsCompressorNode needs a BaseAudioContext");
      return context.createDynamicsCompressor();
    };
    Object.defineProperty(window, "DynamicsCompressorNode", { value: D, configurable: true, writable: true });
  }
})();
