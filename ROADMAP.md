# Roadmap

This is a direction, not a promise. "Committed" items are being worked on for the open build;
"Experimental" items may change shape or be dropped. Dates are not given on purpose.

## Committed

- **Site compatibility**: keep measuring the top sites on Servo, ship declarative rules and
  feature-guarded polyfills, and upstream engine fixes from `third_party/` to Servo.
- **Signed, verifiable releases**: Authenticode-signed MSI, published SHA-256 sums, release
  notes from this changelog.
- **Follow Servo releases**: move from the 0.1.x LTS line to newer Servo releases once our
  patches are upstream or rebased.
- **Accessibility**: keyboard reachability of every control, screen-reader names for the custom
  title bar and panels, contrast checks of the glass theme, Reduce motion everywhere.
- **Privacy controls**: every network feature listed in PRIVACY.md with its own switch.
- **Linux build in CI** (packaging later).

## Experimental

- **Myth panel**: better page-text extraction, citations back to page blocks, more languages.
- **Local-first AI**: smoother Ollama setup and model suggestions for low-memory machines.
- **Plugin or theme surface** for the UI.
- **Opening more safety components** of the agent (for example the approval card logic) where
  doing so does not expose the proprietary agent.

## Not planned for the open build

- The autonomous Myth agent (browser control) and its prompts.
- A hosted model or a JustMyth account requirement.
- Anything that solves CAPTCHAs, forges browser fingerprints or otherwise evades bot detection.
