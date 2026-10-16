# Cookies and site data (v0.4.2)

Settings > **Cookies and site data** (`ui/cookies.js`) and the address bar's **site popover**
(click the lock / globe at the left of the address bar on a web page; `ui/popups.js`).

## What the user can do

| Where | What |
| --- | --- |
| Settings > Cookies | Search sites; per site: cookie count, bytes, whether it also keeps local/session storage; expand to see every cookie (name, domain or host, path, expiry, Secure / HttpOnly, size); delete one cookie; **Clear** (the site's cookies + storage); per site **Allow / Block / Clear on exit** (click the active one again for the default); **Clear all cookies** (asks once); **Clear on exit** switch (all sites except Allow). |
| Site popover | Host and connection, cookie count for the site, the same Allow / Block / Clear on exit, Clear cookies, **Manage** (opens Settings > Cookies filtered to the site: `library.html#cookies=<site>`). |
| Settings > Privacy > Clear browsing data | Unchanged: still clears cookies + storage for every site. |

A "site" is the registrable domain (eTLD+1) the cookie jar files cookies under, so
`mail.example.com` and `www.example.com` cookies are both under `example.com`.

## Privacy rules

* **Cookie values never leave the network layer through this feature.** The vendored servo-net
  (patch 0002) answers the listing with each value replaced by `mbmeta.<size>.<host-only>`;
  the RPC rows have no value field; the UI says so.
* The ops are internal-page only (Settings page, chrome UI). Web pages and Myth's agent tabs
  cannot call them; Myth's page tools never see cookies.
* Logs name sites only. Patch 0002 also removed the two servo-net log lines that printed cookie
  values (`COOKIES SENT` at info, `SENT COOKIE` at debug).

## How it is enforced

* **Block**: the site is put into servo-net's blocked set (`mythbrowse_cookies::set_blocked`)
  at start-up and when chosen. `CookieStorage::push` then stores nothing for it and
  `cookies_data_for_url` sends nothing (HTTP and `document.cookie` alike); existing cookies are
  cleared. Signing in to a blocked site will not stick.
* **Clear on exit** (per site, or global with Allow as the exception): cleared in the shell's
  `shutdown()` while Servo still runs, and again at the next start in case MythBrowse did not
  close cleanly.
* Policies live in the profile pref `cookie_policy` (JSON `{v, clear_on_exit, sites}`).

## Not supported yet (honest gaps)

* **Third-party cookie blocking.** Servo 0.1.4's cookie paths (HTTP responses, `document.cookie`,
  CookieStore) do not carry the top-level site of the request, so the cookie store cannot tell a
  third-party cookie from a first-party one. A request-origin check would get iframes wrong
  (their own subresources look first-party) and would not cover script-set cookies, so it is not
  offered; the UI shows "Not supported yet" and suggests blocking the tracker's site. Needs the
  top-level site plumbed through `Request` / `CoreResourceMsg` upstream.
* **Cookies are kept after MythBrowse closes by default** ("Staying signed in" below). With the
  switch off, or in a private session, every cookie ends with the session.
* Private sessions: the listing reads the public cookie jar only (Servo's
  `SiteDataManager::cookies_for_url` asks the public resource threads); clearing and blocking
  cover both.
* SameSite is not shown (Servo 0.1.4 does not implement it in the store).

## Staying signed in (on by default since 0.4.2)

Settings > Cookies > **Stay signed in** ("Stay signed in to sites after restart") writes
`persist_cookies` = `1` / `0` to `<profile>/ui.json` (`cookies_prefs_save?persist=`). It is read
before the engine starts, so a change applies at the next start; the row says so while the saved
value and the running state differ.

**Default: on.** A missing `ui.json`, or one without `persist_cookies` (a new profile, or a
profile whose switch was never touched), means on; only an explicit `"0"` (the user turned it
off) means off, and that choice is kept across upgrades
(`mythbrowse_engine::cookies_api::persist_cookies_from_pref`). A private session (`--private`)
never keeps cookies, whatever the setting; its Cookies page says so.

Upgrading from a build where the default was off: cookies of sessions run before the upgrade
were never written to disk, so there is nothing to restore and sites are signed out once, at
the first start after the upgrade. From then on, with the switch left on, sign-ins survive a
clean exit and restart. A profile whose switch was explicitly turned off stays off.

With it on (and never in a `--private` session):

* `crates/shell/src/cookies_host.rs::persistent_engine_dir` creates `<profile>/engine-data`
  (0700 on Linux), loads or creates a random 256-bit **jar key** in the platform key store
  (`mythbrowse_passwords::crypto::JarSealer`, secret `cookie-jar`: Windows DPAPI for this
  Windows user; Linux Secret Service, else the 0600 encrypted-file fallback, the same store as
  the password-vault key and AI API keys) and installs it as the jar codec in servo-net
  (patch **servo-net-0003**, `mythbrowse_cookies::set_jar_codec`).
* The engine gets `Opts::config_dir = <profile>/engine-data`. Servo then reads the jar at start
  and writes it at a clean exit as `cookie_jar.sealed` (AES-256-GCM, associated data
  `mythbrowse-cookie-jar/v1`, temp file + rename). A jar that does not open (other user, other
  machine, key gone) is ignored: the user starts signed out, nothing crashes.
* `auth_cache.json` (HTTP Basic/Digest user names and passwords) is **never** written with the
  codec installed; HTTP auth stays in memory.
* Turning the switch off deletes `engine-data` at the next start (cookies and site storage).
  If the key store cannot be used, cookies are simply not kept (logged), never written in the
  clear.

What is kept, and the risks:

| Data | Where | Protection |
| --- | --- | --- |
| Cookies (session tokens: "being signed in") | `engine-data/cookie_jar.sealed` | Encrypted; key bound to the OS user (DPAPI) / keyring. Malware running *as the user* can still ask DPAPI to decrypt it, as with every browser's cookie store. |
| Session cookies (no expiry) | same | Kept too (like Chrome's "Continue where you left off"), so "signed in until the browser closes" sites stay signed in. |
| `localStorage`, IndexedDB | `engine-data/localstorage.json`, `engine-data/webstorage/`, IndexedDB files | **Not encrypted** (Servo writes them itself). Some sites keep tokens there. Same protection as the history database: the user's profile folder. |
| HSTS list | `engine-data/hsts_list.json` | Plain (host names only). |
| HTTP auth credentials | not written | — |

Other notes:

* The jar is written only at a clean exit (Servo's `CoreResourceMsg::Exit`): after a crash or a
  killed process the cookies of that session are lost (the previous jar stays).
* "Clear on exit" (global or per site) runs before the jar is written, so it keeps working.
  Block still clears and refuses a site's cookies.
* Settings > Privacy > Clear browsing data clears the live jar; the next clean exit writes the
  emptied jar.
* Needs a real-device check on Windows: sign in to a site, restart, still signed in; switch
  off, restart, signed out and `engine-data` gone; copy `cookie_jar.sealed` to another Windows
  account, it does not open there.

## Code

* `third_party/servo-net/mythbrowse_cookies.rs` + `patches/servo-net-0002-cookie-management-hooks.patch`,
  `patches/servo-net-0003-sealed-cookie-jar.patch` (sealed jar)
* `crates/passwords/src/crypto.rs` (`JarSealer`)
* `crates/engine/src/cookies_api.rs` (ops, policy model, unit tests)
* `crates/shell/src/cookies_host.rs` (SiteDataManager calls, enforcement)
* `ui/cookies.js`, `ui/popups.js` (site popover), `tests/ui/cookies_smoke.mjs`
