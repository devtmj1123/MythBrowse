# MythBrowse

An AI-native desktop web browser written in Rust on the [Servo](https://servo.org) web engine.
No Chromium, no WebView2: even the browser's own interface is HTML/CSS rendered by Servo. The
built-in assistant, **Myth**, talks to **your own** model: a local [Ollama](https://ollama.com)
server or any OpenAI-compatible API with your key.

> **Status: preview (v0.4.2, Windows).** This repository is the open-source core of
> MythBrowse under the [Mozilla Public License 2.0](LICENSE). It builds a complete, runnable
> browser with AI chat. The autonomous **Myth agent** (Myth opening, reading and acting on
> web pages for you) is proprietary and is **not** part of this repository or of builds made
> from it. See [What is not in the open build](#what-is-not-in-the-open-build).

## What runs today

Built from this repository (Windows 10/11 x64; other platforms are not built or tested in CI yet):

- **Browsing**: tabs in a custom title bar (Windows Snap Layouts), tab sleeping, session
  restore, history, bookmarks, address bar with suggestions, downloads, a password manager
  (OS-protected encryption, Chrome/Edge import), cookies and site data management, site
  permissions, pop-up blocking, page dialogs, IME input, smooth scrolling and middle-click
  autoscroll.
- **Look**: a glass interface with adjustable transparency, Reduce transparency and Reduce
  motion; time-of-day theme.
- **Site compatibility**: a breakage detector and declarative per-site fix rules (CSS, feature-
  guarded JS polyfills shipped with the browser, user-agent presets, reader mode), plus fixes
  to Servo itself in `third_party/`.
- **AI chat (bring your own model)**: chat on the new tab page with saved history; the Myth
  panel beside a page (summarize, key points, explain / translate / rewrite a selection,
  compare tabs, find on the page, questions about the page); "Ask Myth to fix" for a broken
  page (a suggested CSS/JS patch you preview and keep or discard). Keys stay in the OS's
  secure storage. Per-site "Don't share with Myth".
- **Updates**: optional daily check of this repository's GitHub Releases; downloads are
  SHA-256-verified and limited to this repository's release files.

No telemetry, no analytics, no account. What leaves your computer and when: [PRIVACY.md](PRIVACY.md).

## What is not in the open build

- **The Myth agent**: browser control (opening tabs, reading pages, clicking and typing with
  your approval), its tool definitions, prompts, vision pipeline and agent-specific safety
  heuristics. In the open build Myth answers from your model only; asking it to do something
  on a page gets a short notice instead. Official JustMyth builds plug the agent in through
  the `AgentProvider` interface in `crates/ai` (see [ARCHITECTURE.md](ARCHITECTURE.md)).
- **JustMyth cloud services** (sync backend, publishing of signed site-fix rules). Builds from
  this repository have no cloud project configured, so they never contact JustMyth's servers.
- **OS re-authentication** before revealing saved passwords: the open build uses an in-app
  confirmation instead.
- Code signing: Windows installers built from this repository are unsigned.

## Download

Preview builds are not published yet. Until then, build from source (below) or use the MSI
artifact of a run of the `Windows build` workflow.

## Build from source

Rust 1.92.0 is pinned in `rust-toolchain.toml`; it is also the declared minimum (`rust-version`),
the only version CI builds and tests.

**Light crates** (no Servo; any OS, a few minutes):

```sh
cargo fmt --all -- --check
cargo test -p mythbrowse-ai -p mythbrowse-blocker -p mythbrowse-compat -p mythbrowse-storage \
  -p mythbrowse-sync -p mythbrowse-files -p mythbrowse-notify -p mythbrowse-passwords -p mythbrowse-update
cargo test -p mythbrowse-engine --no-default-features
```

**The browser** (compiles Servo: 16 GB RAM recommended, 30-90 minutes):

- Windows: Visual Studio 2022 Build Tools (C++), LLVM/clang (`LIBCLANG_PATH`), CMake, Ninja,
  Python 3. Then `cargo build --release -p mythbrowse-shell`. The executable needs ANGLE's
  `libEGL.dll` and `libGLESv2.dll` next to it (built by `mozangle`, copied by the packaging
  step in `.github/workflows/windows.yml`). `packaging/windows/` builds an MSI with WiX v5.
- Linux / macOS: not built in CI; Servo's build prerequisites and the same `cargo build` are
  the starting point, but expect platform gaps.

Optional build-time settings: `MYTHBROWSE_APPWRITE_ENDPOINT` / `MYTHBROWSE_APPWRITE_PROJECT`
point the browser at an Appwrite project for signed site-fix rule updates (HTTPS only). Leave
them unset for a build that contacts no JustMyth service.

## Set up AI chat

Open the new tab page, then the model settings: choose **Ollama** (local; nothing leaves your
computer) or **OpenAI-compatible** (base URL, model, API key). AI output can be wrong; see
[docs/AI-DISCLAIMER.md](docs/AI-DISCLAIMER.md).

## Known limitations

- Many sites still break on Servo. We measure the top sites, fix what we can with rules and
  upstream Servo patches, and never work around bot detection.
- Servo does not yet isolate web content in sandboxed processes the way mainstream browsers do.
  Do not use preview builds for high-risk browsing.
- Only Windows is built and tested in CI. Installers are not code-signed.

## Project documents

[ARCHITECTURE.md](ARCHITECTURE.md) · [ROADMAP.md](ROADMAP.md) · [CHANGELOG.md](CHANGELOG.md) ·
[CONTRIBUTING.md](CONTRIBUTING.md) (DCO sign-off) · [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) ·
[SECURITY.md](SECURITY.md) · [PRIVACY.md](PRIVACY.md) · [TRADEMARK.md](TRADEMARK.md) ·
[NOTICE](NOTICE) · [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) · [docs/COMPAT.md](docs/COMPAT.md)

## Legal

- Code: [MPL-2.0](LICENSE). Vendored Servo, Stylo and surfman code keeps its own licence and
  notices ([NOTICE](NOTICE), [third_party/README.md](third_party/README.md)).
- Provided "as is", without warranty of any kind (MPL-2.0 sections 6 and 7).
- "MythBrowse", "Myth", "JustMyth" and "ApeironLabs" and their logos are not licensed under the
  MPL; see [TRADEMARK.md](TRADEMARK.md).
- MythBrowse sends a Firefox-style user-agent string for site compatibility. It is not
  affiliated with or endorsed by Mozilla, Google, Microsoft, Apple or any other browser vendor.
- MythBrowse never solves or bypasses CAPTCHAs or other human-verification checks and does not
  forge browser fingerprints.

Copyright © 2026 JustMyth / Tan Ming Jing.
