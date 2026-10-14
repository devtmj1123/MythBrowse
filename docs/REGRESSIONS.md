# Regression notes

Bugs that took a page or a CI run down, what triggered them, and what guards against them now.
Check here before undoing a workaround.

## Servo layout panic on the Passwords page (v0.4 -> fixed v0.4.1)

- **Seen:** Windows feature shots, a private CI run, shot f02. The
  page stayed at "Looking…" with an empty "From" dropdown: the script thread had panicked at
  `servo-layout-0.1.4/fragment_tree/box_fragment.rs:266`, "Should only call
  `scrollable_overflow()` after calculating overflow" (f02-passwords-stderr.log).
- **Cause:** Servo 0.1.4 `expect`s that the overflow pass has visited every fragment it is later
  asked about (scroll-area queries such as `scrollWidth` / `scrollHeight` / `scrollTop`, scroll
  boxes, the display list's scroll frames). A fragment the pass never reached panics. New in
  v0.4 on that page: the glass dropdown (`ui/design/select.js`) kept the native `<select>` as a
  1px, `overflow: hidden` box (a scroll container) whose UA content changes when
  `passwords.js` fills its options right after load. That is the likely trigger; without a
  Windows backtrace (Servo printed "details omitted") it is not proven.
- **Guards (v0.4.1):**
  1. Engine: vendored `third_party/servo-layout` with
     `patches/servo-layout-0001-scrollable-overflow-no-panic.patch`: the three readers fall back
     to a safe rectangle (the fragment's padding box / own rect / the initial containing block)
     and log `layout: … read before overflow was calculated` once. Upstream Servo replaced the
     `expect`s with lazily recalculated overflow (servo/servo#44854); drop the patch when we
     move to a Servo release with it.
  2. UI: `.dd-native { display: none !important; }` (design/tokens.css): the hidden `<select>`
     has no box at all. `select.js` sizes its list with `offsetHeight` instead of
     `scrollHeight` and scrolls the chosen option into view on the next animation frame, not in
     the same task that moved the list into `<body>`.
- **Do not:** give hidden helper elements `overflow: hidden` boxes, or read `scroll*` on an
  element in the same task that inserted or re-parented it, on internal pages. Measure with
  `offsetWidth` / `offsetHeight` / `getBoundingClientRect()` where that is enough.
- **Check:** the f02 feature shot shows the import sources ("No Chrome, Edge or Chromium profile
  found" on the CI runner) and "No password CSV files in …" instead of "Looking…"; its stderr
  has no `scrollable_overflow` panic.

## CI runner lost during the MSI smoke test (v0.4 -> isolated v0.4.1)

- **Seen:** a private CI run: "MSI smoke test (quiet install, launch, uninstall)" was cancelled
  17 s after it started, then "Complete job" hung for ~45 min until GitHub failed the job with
  "The hosted runner lost communication with the server". Every later step (MSIX, AI e2e, light
  tests) never ran, and no log of the job was kept (the logs API returns 404).
- **Cause:** not established (no log survived). The MSI smoke passed on v0.3 (a private CI run);
  v0.4 added the WixUI setup UI, the Util extension (WixShellExec, only run from the UI's Finish
  button, never in a quiet install), the desktop-shortcut component, and an app that checks for
  updates at start-up. None of these kill processes; the quiet install schedules no custom
  action. The most likely explanations are a hosted-runner/VM loss or Restart Manager / the
  installed app disturbing the runner; real Windows testing of the MSI is still needed.
- **Guards (v0.4.1):** the smoke test runs in its own job (`msi-smoke`, needs the build job and
  downloads `mythbrowse-windows-msi`), so a lost runner there no longer costs the build job's
  results. `ci/msi-smoke-windows.ps1` logs each step with a timestamp and memory snapshots,
  runs msiexec with a 300 s timeout and `MSIRESTARTMANAGERCONTROL=Disable MSIDISABLERMRESTART=1
  REBOOT=ReallySuppress`, starts the app with `MYTHBROWSE_NO_UPDATE_CHECK=1`, and only ever
  stops `mythbrowse` processes whose image is in the install folder.
