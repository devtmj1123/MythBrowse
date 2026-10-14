// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
// Passwords & import page (passwords.html). Saved logins are listed without passwords; a
// password is fetched only after the OS confirmed it is the user (pw_reauth), shown in place
// and hidden again after 30 s. Edits send a new password in the x-mb-secret header.
"use strict";

(function () {
  const $ = (id) => document.getElementById(id);
  const I = window.JMIcons;
  const TOKEN = document.querySelector('meta[name="mb-token"]').content;

  function api(op, params, secret) {
    const q = new URLSearchParams(params || {});
    q.set("t", TOKEN);
    const init = secret ? { headers: { "x-mb-secret": new URLSearchParams(secret).toString() } } : undefined;
    return fetch("/api/" + op + "?" + q.toString(), init)
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function btn(label, cls, fn, title) {
    const b = el("button", cls || "ghost", label);
    b.type = "button";
    if (title) b.title = title;
    b.addEventListener("click", fn);
    return b;
  }
  function hostOf(origin) {
    const m = /^[a-z]+:\/\/([^/:]+)/i.exec(origin || "");
    return m ? m[1] : origin;
  }
  const DOTS = "\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022";

  let status = null;
  let logins = [];
  const shown = new Map(); // id -> timer

  function msg(text, bad) {
    const m = $("pw-msg");
    m.textContent = text || "";
    m.className = "set-msg" + (bad ? " pw-bad" : text ? " pw-ok" : "");
  }

  // ---- re-authentication -------------------------------------------------------------------
  // Resolves true once the shell says reveal / edit / export are allowed for the next minute.
  function reauth() {
    return api("pw_reauth").then((r) => {
      if (!r.ok) { msg(r.error || "Could not confirm it is you.", true); return false; }
      if (r.granted) return true;
      if (r.confirm) return confirmInApp();
      return waitForOs();
    });
  }
  function waitForOs() {
    const box = $("pw-auth");
    box.hidden = false;
    box.textContent = "Confirm with Windows that it is you\u2026";
    return new Promise((done) => {
      const tick = () => api("pw_status").then((s) => {
        if (s.reauthed) { box.hidden = true; done(true); return; }
        if (s.reauth_pending) { setTimeout(tick, 400); return; }
        if (s.reauth_note === "confirm") { box.hidden = true; confirmInApp().then(done); return; }
        box.textContent = s.reauth_note || "Not confirmed.";
        setTimeout(() => { box.hidden = true; }, 4000);
        done(false);
      });
      setTimeout(tick, 400);
    });
  }
  // Where the OS has no sign-in prompt (Linux, or a PIN-only Windows account): ask here.
  function confirmInApp() {
    const box = $("pw-auth");
    box.hidden = false;
    box.textContent = "";
    box.appendChild(el("span", "", "This computer has no system sign-in prompt for MythBrowse. Show saved passwords anyway? Anyone at this computer can then see them."));
    const row = el("div", "pw-confirm");
    box.appendChild(row);
    return new Promise((done) => {
      row.appendChild(btn("Yes, it is me", "primary", () => {
        api("pw_reauth", { confirm: "1" }).then((r) => { box.hidden = true; done(!!(r.ok && r.granted)); });
      }));
      row.appendChild(btn("Cancel", "ghost", () => { box.hidden = true; done(false); }));
    });
  }

  // ---- list ----------------------------------------------------------------------------------
  function load() {
    return api("pw_status").then((s) => {
      status = s;
      $("pw-locked").hidden = !s.locked;
      $("pw-export").disabled = !s.available;
      if (!s.available) {
        $("pw-count").textContent = "";
        $("pw-list").textContent = "";
        if (!s.locked && s.problem) msg(s.problem, true);
        return;
      }
      return api("pw_list", { q: $("pw-filter").value }).then((r) => {
        logins = r.ok ? r.logins : [];
        render();
      });
    });
  }

  function render() {
    const list = $("pw-list");
    list.textContent = "";
    $("pw-count").textContent = status && status.count ? String(status.count) : "";
    if (!logins.length) {
      list.appendChild(el("li", "pw-empty soft", $("pw-filter").value ? "Nothing matches." : "No saved passwords yet. When you sign in to a site, MythBrowse offers to save the password."));
      return;
    }
    logins.forEach((l) => list.appendChild(row(l)));
  }

  function row(l) {
    const li = el("li", "pw-row");
    const host = hostOf(l.origin);
    li.appendChild(el("span", "pw-av", (host[0] || "?").toUpperCase()));
    const site = el("span", "pw-site", host);
    site.title = l.origin;
    li.appendChild(site);
    li.appendChild(el("span", "pw-user", l.username || "(no username)"));
    const pass = el("span", "pw-pass", DOTS);
    li.appendChild(pass);
    const acts = el("span", "pw-acts");
    function hide() {
      clearTimeout(shown.get(l.id));
      shown.delete(l.id);
      pass.textContent = DOTS;
      show.textContent = "Show";
    }
    const show = btn("Show", "ghost", () => {
      if (shown.has(l.id)) { hide(); return; }
      reauth().then((ok) => {
        if (!ok) return;
        api("pw_reveal", { id: String(l.id) }).then((r) => {
          if (!r.ok) { msg(r.error === "reauth" ? "Confirm it is you first." : r.error, true); return; }
          pass.textContent = r.password;
          show.textContent = "Hide";
          shown.set(l.id, setTimeout(hide, 30000));
        });
      });
    }, "Show the password (asks Windows to confirm it is you)");
    acts.appendChild(show);
    acts.appendChild(btn("Edit", "ghost", () => {
      if (li.querySelector(".pw-edit")) return;
      reauth().then((ok) => {
        if (!ok) return;
        api("pw_reveal", { id: String(l.id) }).then((r) => {
          if (r.ok) li.appendChild(editor(l, r.note || ""));
        });
      });
    }));
    let armed = false;
    const del = btn("Delete", "ghost", () => {
      if (!armed) {
        armed = true;
        del.textContent = "Delete?";
        setTimeout(() => { armed = false; del.textContent = "Delete"; }, 3000);
        return;
      }
      api("pw_delete", { id: String(l.id) }).then((r) => {
        if (r.ok) { msg("Deleted the password for " + host + "."); load(); }
        else msg(r.error, true);
      });
    }, "Click twice to delete");
    acts.appendChild(del);
    li.appendChild(acts);
    return li;
  }

  function editor(l, note) {
    const f = el("div", "pw-edit");
    f.appendChild(el("span", "gen-label", "Username"));
    const u = el("input", "field");
    u.value = l.username;
    f.appendChild(u);
    f.appendChild(el("span", "gen-label", "New password"));
    const p = el("input", "field");
    p.type = "password";
    p.placeholder = "Leave empty to keep the current one";
    p.autocomplete = "new-password";
    f.appendChild(p);
    f.appendChild(el("span", "gen-label", "Note"));
    const n = el("input", "field");
    n.value = note;
    f.appendChild(n);
    const acts = el("div", "pw-edit-acts");
    acts.appendChild(btn("Save", "primary", () => {
      const secret = p.value ? { p: p.value } : null;
      api("pw_update", { id: String(l.id), username: u.value, note: n.value }, secret).then((r) => {
        p.value = "";
        if (r.ok) { msg("Saved."); load(); }
        else msg(r.error === "reauth" ? "Confirm it is you again, then save." : r.error, true);
      });
    }));
    acts.appendChild(btn("Cancel", "ghost", () => { p.value = ""; f.remove(); }));
    f.appendChild(acts);
    return f;
  }

  let filterTimer = 0;
  $("pw-filter").addEventListener("input", () => {
    clearTimeout(filterTimer);
    filterTimer = setTimeout(load, 150);
  });

  // ---- export --------------------------------------------------------------------------------
  $("pw-export").addEventListener("click", () => { $("pw-export-box").hidden = false; });
  $("pw-export-cancel").addEventListener("click", () => { $("pw-export-box").hidden = true; });
  $("pw-export-go").addEventListener("click", () => {
    reauth().then((ok) => {
      if (!ok) return;
      api("pw_export").then((r) => {
        $("pw-export-box").hidden = true;
        if (r.ok) msg("Exported " + r.count + " passwords to " + r.path + ". Delete the file when you are done.");
        else msg(r.error === "reauth" ? "Confirm it is you first." : r.error, true);
      });
    });
  });
  $("pw-reset").addEventListener("click", () => {
    const b = $("pw-reset");
    if (b.dataset.armed !== "1") {
      b.dataset.armed = "1";
      b.textContent = "Click again to delete all saved passwords";
      return;
    }
    api("pw_reset").then((r) => {
      if (r.ok) { msg("Started over with an empty password store."); load(); loadNever(); }
      else msg(r.error, true);
    });
  });

  // ---- never saved ---------------------------------------------------------------------------
  function loadNever() {
    api("pw_never").then((r) => {
      const list = $("never-list");
      list.textContent = "";
      const sites = r.ok ? r.sites : [];
      $("never-count").textContent = sites.length ? String(sites.length) : "";
      if (!sites.length) {
        list.appendChild(el("li", "pw-empty soft", "Sites where you chose \u201cNever for this site\u201d appear here."));
        return;
      }
      sites.forEach((o) => {
        const li = el("li", "pw-row");
        li.appendChild(el("span", "pw-av", (hostOf(o)[0] || "?").toUpperCase()));
        li.appendChild(el("span", "pw-site", o));
        li.appendChild(el("span", ""));
        li.appendChild(el("span", ""));
        const acts = el("span", "pw-acts");
        acts.appendChild(btn("Remove", "ghost", () => api("pw_never_delete", { origin: o }).then(loadNever), "Offer to save passwords on this site again"));
        li.appendChild(acts);
        list.appendChild(li);
      });
    });
  }

  // ---- import from a browser -----------------------------------------------------------------
  let profiles = [];
  function loadSources() {
    api("import_sources").then((r) => {
      const sel = $("imp-profile");
      sel.textContent = "";
      profiles = r.ok ? r.profiles : [];
      const direct = !!(r.ok && r.direct_passwords);
      if (!profiles.length) {
        const o = el("option", "", "No Chrome, Edge or Chromium profile found");
        o.value = "";
        sel.appendChild(o);
        $("imp-go").disabled = true;
      } else {
        profiles.forEach((p, i) => {
          const o = el("option", "", p.label + " \u2013 " + p.name);
          o.value = String(i);
          sel.appendChild(o);
        });
        $("imp-go").disabled = false;
      }
      $("imp-note").textContent = direct
        ? "Close the browser first if the import cannot read its files. Chrome 127+ protects newer passwords with app-bound encryption that other apps cannot read; for those, export a CSV from Chrome and import it below."
        : "On this system passwords come in through a CSV file (below); bookmarks and history are read directly.";
      if (!direct) $("imp-passwords").checked = false;
      $("imp-passwords").disabled = !direct;
      if (r.result) showImport(r.result);
    });
  }

  function showImport(res) {
    const box = $("imp-result");
    box.textContent = "";
    const lines = [];
    if (res.bookmarks_found) lines.push(res.bookmarks + " new bookmarks (" + res.bookmarks_found + " found)");
    if (res.history_found) lines.push(res.history + " new history entries (" + res.history_found + " read)");
    if (res.passwords) lines.push(res.passwords.added + " passwords added, " + res.passwords.updated + " updated, " + res.passwords.unchanged + " already saved");
    const rep = res.password_report || {};
    box.appendChild(el("div", "pw-ok", (res.source ? res.source + ": " : "") + (lines.join(" \u00b7 ") || "Nothing new to import.")));
    if (rep.app_bound) {
      box.appendChild(el("div", "pw-warn", rep.app_bound + " passwords use Chrome's app-bound encryption (Chrome 127 and later), which only Chrome itself can read. In Chrome open Settings \u203a Passwords and autofill \u203a Google Password Manager \u203a Settings \u203a Export passwords, then import that CSV file below."));
    }
    if (rep.failed) box.appendChild(el("div", "pw-bad", rep.failed + " passwords could not be decrypted."));
    if (res.wanted_passwords && !res.direct_passwords) box.appendChild(el("div", "soft small", "Passwords: use a CSV export on this system."));
    (res.errors || []).forEach((e) => box.appendChild(el("div", "pw-bad", e)));
  }

  $("imp-go").addEventListener("click", () => {
    const p = profiles[+$("imp-profile").value];
    if (!p) return;
    const want = {
      browser: p.browser, profile: p.dir,
      bookmarks: $("imp-bookmarks").checked ? "1" : "0",
      history: $("imp-history").checked ? "1" : "0",
      passwords: $("imp-passwords").checked ? "1" : "0",
    };
    $("imp-go").disabled = true;
    $("imp-result").textContent = "Importing\u2026";
    api("import_run", want).then((r) => {
      if (!r.ok) { $("imp-go").disabled = false; $("imp-result").textContent = r.error; return; }
      const tick = () => api("pw_status").then((s) => {
        if (s.import_running) { setTimeout(tick, 400); return; }
        $("imp-go").disabled = false;
        if (s.import_result) showImport(s.import_result);
        load();
      });
      setTimeout(tick, 300);
    });
  });

  // ---- CSV -----------------------------------------------------------------------------------
  function loadCsvFiles() {
    api("import_csv_files").then((r) => {
      const box = $("csv-files");
      box.textContent = "";
      const files = r.ok ? r.files : [];
      if (!files.length) { box.textContent = "No password CSV files in " + (r.downloads || "Downloads") + "."; return; }
      files.forEach((f) => box.appendChild(btn(f.split(/[\\/]/).pop(), "ghost", () => { $("csv-path").value = f; }, f)));
    });
  }
  $("csv-go").addEventListener("click", () => {
    const path = $("csv-path").value.trim();
    if (!path) return;
    $("csv-result").textContent = "Importing\u2026";
    api("import_csv", { path: path }).then((r) => {
      const box = $("csv-result");
      box.textContent = "";
      if (!r.ok) { box.appendChild(el("div", "pw-bad", r.error)); return; }
      const c = r.counts;
      box.appendChild(el("div", "pw-ok", c.added + " added, " + c.updated + " updated, " + c.unchanged + " already saved" + (c.skipped ? ", " + c.skipped + " skipped (no web address or password)" : "") + "."));
      box.appendChild(el("div", "soft small", "Now delete the CSV file: it is not encrypted."));
      load();
    });
  });

  I.paint(document);
  load();
  loadNever();
  loadSources();
  loadCsvFiles();
})();
