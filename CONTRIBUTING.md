# Contributing to MythBrowse

Thanks for helping. MythBrowse is maintained by Tan Ming Jing (JustMyth). It is early, so small,
focused changes are easiest to review.

## Good first contributions

- **Site compatibility reports**: a site that breaks, with the URL, what you expected and a
  screenshot (issue template "Site compatibility").
- **Compat rules** (`rules/`): feature-guarded polyfills and per-site fixes. Every rule needs a
  reason and a host list; see `docs/COMPAT.md` and `rules/schema.json`.
- **Servo fixes**: if the bug is in Servo, the fix should go to
  [servo/servo](https://github.com/servo/servo) first. `third_party/` carries a patch only until a
  Servo release ships it; see `third_party/README.md`.
- Issues labelled `good first issue`.

## Development

The light crates build and test without Servo:

```sh
cargo fmt --all -- --check
cargo test -p mythbrowse-ai -p mythbrowse-blocker -p mythbrowse-compat -p mythbrowse-storage \
  -p mythbrowse-sync -p mythbrowse-files -p mythbrowse-notify -p mythbrowse-passwords -p mythbrowse-update
cargo test -p mythbrowse-engine --no-default-features
cargo clippy --all-targets -p <crate you touched> -- -D warnings
```

Building the browser itself compiles Servo; see the README. The UI (`ui/`) is plain HTML/CSS/JS;
`tests/ui/*.mjs` are headless-Chromium smoke tests of it against mocked browser RPCs
(`PW=<playwright-core dir> CHROME=<chrome-headless-shell> node tests/ui/<test>.mjs`).

## Pull requests

- One topic per pull request; describe what changed and how you tested it.
- `cargo fmt` clean, no new clippy warnings, tests for logic changes.
- Do not add telemetry, analytics or new network connections without discussing them first:
  every outbound connection must be listed in `PRIVACY.md`.
- Never add code that solves or bypasses CAPTCHAs, forges browser fingerprints or otherwise evades
  bot detection. Such changes will be closed.
- Do not commit secrets, API keys or personal data. CI runs a secret scan.
- AI-assisted contributions are fine; you are responsible for them like any other code you sign
  off (licence, correctness, no copied code you may not contribute).

## Developer Certificate of Origin (DCO)

MythBrowse uses the [Developer Certificate of Origin 1.1](https://developercertificate.org/)
instead of a CLA. Every commit must carry a `Signed-off-by` line with your name and an email you
can be reached at, matching the commit author:

```
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it. By signing off you certify the DCO below. CI checks every commit of a pull
request and fails when a sign-off is missing.

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## Licensing of contributions

Contributions are licensed under the repository's licence, the Mozilla Public License 2.0
(inbound = outbound). New source files should start with:

```
// SPDX-License-Identifier: MPL-2.0
```

## Code of conduct

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
