# Security policy

## Reporting a vulnerability

Please **do not** open a public issue for security problems.

Report privately through GitHub's private vulnerability reporting on this repository
("Security" tab > "Report a vulnerability"), or by email to **security@apeironlabs.tech**.

Include the MythBrowse version (Settings > About), your OS, steps to reproduce and the impact you
see. We aim to acknowledge reports within 7 days and to agree a disclosure date with you; the
default is 90 days after the report or when a fix ships, whichever comes first. We credit
reporters in the release notes unless you prefer otherwise. There is no paid bug bounty.

## Supported versions

MythBrowse is preview software. Only the latest release receives security fixes.

## Scope

In scope: the code in this repository and builds made from it, including

- the browser shell and the internal `https://mythbrowse.internal/` UI / RPC surface (token-
  protected; web pages must never reach it);
- password and API-key storage and redaction;
- the updater (release lookup, download, SHA-256 verification, allow-listed URLs, installer
  hand-off);
- site-fix rules: signature verification, anti-rollback, size limits, the rule that remote rules
  may not carry new JavaScript, the trust levels of built-in / remote / local rules, and the static
  review of AI-suggested fixes;
- the Myth panel's page-context extraction (what page data reaches your AI provider) and the
  "Don't share with Myth" setting;
- navigation safeguards of ordinary browsing (URL schemes, internal pages, pop-up and permission
  prompts).

Bugs in the Servo engine are welcome too; we will help get them to
[servo/servo](https://github.com/servo/servo/security) when the fix belongs upstream.

The proprietary Myth agent is not in this repository. Reports about official JustMyth builds that
include it are welcome at the same address.

Out of scope: sites that do not render correctly (please file a compatibility issue), and attacks
that need an already-compromised device or account.

## Known limitations

- Windows installers are **not Authenticode-signed** yet. Releases publish a SHA-256 checksum; the
  built-in updater refuses a download whose checksum does not match and only downloads from this
  repository's GitHub release files.
- The Servo engine is pre-1.0 and does not yet isolate web content in sandboxed processes the way
  mainstream browsers do. Do not use preview builds for high-risk browsing.
- Builds from this repository have no OS re-authentication before showing saved passwords (an
  in-app confirmation is used instead); anyone with access to your unlocked Windows session can
  view them.
