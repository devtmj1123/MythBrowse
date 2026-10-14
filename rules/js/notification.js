// MythBrowse compat: Notification stub (rule polyfill.notification).
// Servo has no Notifications API; scripts that read Notification.permission without checking
// throw a ReferenceError ("Notification is not defined") and stop. The stub reports
// "denied" (the truth: this browser cannot show web notifications yet) and never prompts.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (typeof window.Notification === "undefined") {
  var Notification = function Notification(title) {
    if (!(this instanceof Notification)) throw new TypeError("Constructor Notification requires 'new'");
    throw new TypeError("Notifications are not available in this browser");
  };
  Object.defineProperty(Notification, "permission", { get: function () { return "denied"; }, configurable: true, enumerable: true });
  Notification.maxActions = 0;
  Notification.requestPermission = function (cb) {
    if (typeof cb === "function") setTimeout(function () { cb("denied"); }, 0);
    return Promise.resolve("denied");
  };
  Object.defineProperty(window, "Notification", { value: Notification, configurable: true, writable: true });
}
