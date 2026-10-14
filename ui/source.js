// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// "View page source": fetch the capture named in the URL hash and show it as text.
"use strict";

(function () {
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;
  const id = (location.hash || "").replace(/^#/, "");
  const pre = document.getElementById("src");
  const where = document.getElementById("src-url");
  if (!/^\d+$/.test(id)) { pre.textContent = "No page source to show."; return; }
  fetch("/api/source?id=" + id + "&t=" + encodeURIComponent(TOKEN))
    .then((r) => r.json())
    .then((r) => {
      if (!r || r.ok === false) { pre.textContent = (r && r.error) || "That page source is no longer available."; return; }
      where.textContent = r.url || "";
      document.title = "Source of " + (r.url || "page");
      pre.textContent = r.source || "";
    })
    .catch(() => { pre.textContent = "That page source is no longer available."; });
})();
