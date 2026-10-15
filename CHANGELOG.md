# Changelog

All notable changes to the open-source MythBrowse are listed here.

## 0.4.2: open-source release (tag `v0.4.2`)

First public release of the MythBrowse source under the Mozilla Public License 2.0.

### Included

- The complete browser: shell, Servo embedding, browser UI, site compatibility layer, password
  manager, downloads, cookies and site data management, updater, local profile storage, and the
  Servo / Stylo / surfman fixes in `third_party/`.
- AI chat with your own model (Ollama or any OpenAI-compatible API): chat history, the Myth panel
  beside pages (summarize, key points, selection actions, compare tabs, find, questions about the
  page) and "Ask Myth to fix" for broken pages.
- `AgentProvider` interface (`crates/ai/src/agent_api.rs`) and the shell's `agent` feature: the
  boundary at which official builds add the proprietary Myth agent.
- Settings > Site compatibility > **Rule updates** ("Automatically update site-fix rules") (on by default; builds from this
  repository have no update server configured).

### Open Build identity (separate from official JustMyth builds)

- Version 0.4.2 everywhere (Cargo workspace, Windows version resource, MSI).
- Installs and runs side by side with an official MythBrowse and shares nothing with it: its
  own profile folder (`%LOCALAPPDATA%\MythBrowse Open Build`, `$XDG_DATA_HOME/mythbrowse-open`;
  `MYTHBROWSE_PROFILE_DIR` still overrides), key-store labels (DPAPI entropy, Secret Service
  `mythbrowse-open`), notification id `JustMyth.MythBrowse.OpenBuild`, MSI product
  "MythBrowse (Open Build)" with its own UpgradeCode and install folder, MSIX identity.
- Updates: both builds read `devtmj1123/MythBrowse` releases; the Open Build installs only
  `MythBrowse-OpenBuild-<version>-<arch>.msi` assets, official builds ignore them.
- Cargo feature `official` (off by default) on the shell and the light crates selects the
  official identity; it is for JustMyth's official builds only.

### Security

- WebCrypto no longer offers the draft ML-DSA-44/65/87 algorithms (`crypto.subtle` calls with
  those names reject with `NotSupportedError`). Servo 0.1.4 implements them with `ml-dsa` 0.0.4,
  which has open advisories (CVE-2026-24850, CVE-2026-22705 / RUSTSEC-2025-0144,
  GHSA-h37v-hp6w-2pp8) and no fixed version on the Servo 0.1 line; see `third_party/README.md`
  (servo-script 0007). The `ml-dsa` crate is no longer built.
- Dependency updates: rustls 0.23.46 (RUSTSEC-2026-0346 to -0349), time 0.3.55
  (RUSTSEC-2026-0009), rand 0.8.8 (RUSTSEC-2026-0097), anyhow 1.0.104 (RUSTSEC-2026-0190), and
  grid 1.0.1, smallbitvec 2.6.1, tar 0.4.46.

### Changed compared with the preview builds

- The Myth agent (browser control), its prompts and its agent-specific safeguards are not part
  of the open build; asking Myth to act on a page shows a notice instead.
- Remote site-fix rules may no longer carry JavaScript that is not built into the browser.
- The cloud project is a build-time setting (`MYTHBROWSE_APPWRITE_ENDPOINT`,
  `MYTHBROWSE_APPWRITE_PROJECT`); unset, the browser contacts no JustMyth service.
- Password reveal uses an in-app confirmation instead of the Windows credential prompt.

### Highlights of the 0.4 preview line (before open-sourcing)

- 0.4.2: cookie management (per-site allow / block / clear on exit), compositor-path smooth
  scrolling, middle-click autoscroll, a performance HUD, a fix for pages (e.g. Google results)
  that never finished loading when a media element could not play.
- 0.4.1: fix for a Servo layout panic on the Passwords page; Myth panel over web pages.
- 0.4: setup UI for the MSI, Firefox user-agent default, text selection and copy on pages,
  background tabs from Ctrl/middle-click.
