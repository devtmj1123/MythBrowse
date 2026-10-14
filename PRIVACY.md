# MythBrowse privacy notice

_Last updated: [DATE OF PUBLICATION]. Applies to MythBrowse built from this repository, version
0.4 and later. Official JustMyth builds publish their own notice with the differences listed
in [Official builds](#official-builds)._

**Short version.** MythBrowse is a local-first browser. It has no telemetry, no analytics, no
advertising identifiers and no account. Your history, bookmarks, passwords, chats and settings
stay on your computer. Data leaves your device only in the cases listed below, and AI requests
go only to the AI provider *you* configure.

## Who is responsible

MythBrowse is published by JustMyth / Tan Ming Jing, Kuala Lumpur, Malaysia ("we"). Contact:
**opensource@apeironlabs.tech** (security reports: security@apeironlabs.tech, see
[SECURITY.md](SECURITY.md)). We do not receive or store your browsing data, so in most cases we
hold no personal data about you at all. Where we do (for example if you email us or open a
GitHub issue), we are the controller of that data.

## What leaves your device, and when

| What | Sent to | When | What is sent | How to turn it off |
|---|---|---|---|---|
| Web pages you visit | The websites themselves | When you browse | What any browser sends: your IP address, that site's cookies, a Firefox-style user-agent string, request headers | n/a |
| Address-bar search suggestions | DuckDuckGo (duckduckgo.com) | As you type in the address bar | The text you type (never full addresses), your IP address | Settings > Suggestions (on by default) |
| Searches | Your chosen search engine (DuckDuckGo by default) | When you search | Your query | Choose another engine |
| Update check | GitHub (api.github.com) | Once a day at start-up, and when you click "Check for updates" | A read-only request for the latest release of this repository (your IP address, a MythBrowse user agent). No identifier, no usage data | Settings > About > daily check (on by default). Never in private sessions |
| Update download | GitHub release files of this repository | Only when you click "Download & install" | A download request; the file is checked against its published SHA-256 before it runs | Don't install |
| AI requests (Myth) | **The AI provider you configure**: a local Ollama server, or an OpenAI-compatible API at the address you enter, with your own key | Only when you send Myth a message, use a Myth panel action, or click "Ask Myth to fix" on a broken page | Your message and the conversation so far; when you ask about a page: that page's address, title and visible text and/or your selection (never form fields, never from sites you set to "Don't share with Myth"); for "Ask Myth to fix": the compatibility findings and a structure-only outline of the page (no form values) | Don't configure a provider; use Ollama to keep everything on your computer |
| Downloads | The file's server | When you download a file | A normal download request with that site's cookies | n/a |
| Site-fix rule updates | **Nobody, in builds from this repository** (no update server is configured) | — | — | Settings > Site compatibility > Rule updates |
| Cloud sync | **Not included** (off and compiled out by default) | — | — | n/a |

We do **not** receive the content of your AI conversations: they go straight from your
computer to the provider you chose, whose privacy policy applies to them. Check whether your
provider uses API data for training. AI output may be wrong; see
[docs/AI-DISCLAIMER.md](docs/AI-DISCLAIMER.md).

There are no crash reports, usage statistics or "phone home" pings. Diagnostic information is
written only to a local log file (below).

## Official builds

Official JustMyth builds may be configured at build time with JustMyth's Appwrite Cloud project
(Singapore region). They then also:

- download **signed site-fix rules** about 20 seconds after start and then every 6 hours: an
  anonymous HTTPS download of a public, Ed25519-signed rules file (your IP address and a
  MythBrowse user agent reach the server; no cookies, no account, no browsing data). Rules are
  declarative (CSS, block patterns, user-agent presets, reader mode); executable code may only
  be a fix that already ships inside the browser. A file that fails the signature check, is
  older than the installed one or exceeds 2 MB is refused. Turn this off in
  Settings > Site compatibility > **Rule updates** ("Automatically update site-fix rules") (already-installed rules stay
  until the profile is deleted);
- include the proprietary Myth agent, which can open and act on web pages when you ask it to.
  Its own notice describes what it sends to your AI provider.

## What is stored on your device

In your MythBrowse profile (`%LOCALAPPDATA%\MythBrowse Open Build` on Windows, `$XDG_DATA_HOME/mythbrowse-open` elsewhere; official JustMyth builds keep a separate `MythBrowse` profile and never read this one):

- browsing history, bookmarks, downloads list, open tabs (for session restore), site
  permissions and settings;
- cookies, site storage and cache of the websites you visit. By default cookies are **not**
  kept after MythBrowse closes. If you turn on Settings > Cookies > **Stay signed in** (off by
  default, never in private sessions), cookies are saved in `engine-data\cookie_jar.sealed`,
  encrypted (AES-256-GCM) with a key protected by Windows (DPAPI) or the Linux Secret Service;
  site storage (`localStorage`, IndexedDB) in that folder is not encrypted, and HTTP sign-in
  passwords are never written. Turning the switch off deletes that folder at the next start;
- saved Myth conversations (kept forever by default; you can choose 90 or 30 days); page text
  attached to a question is used for that one request and never saved;
- saved passwords, encrypted with a key protected by Windows (DPAPI, your Windows account only)
  or the Linux Secret Service;
- AI provider settings (`ai.toml`); API keys are kept in the operating system's secure storage,
  never in `ai.toml` or the logs;
- a log file (`logs\mythbrowse.log`, replaced at each start, previous run kept). It can contain
  web addresses (for example of a download or a page that crashed). It never leaves your
  computer unless you send it to us.

**Private session** (`mythbrowse --private`): no page or chat history is saved and no update
check is made.

## Your choices and rights

Because your data stays on your device, you control it directly:

- **Access / export:** Library shows your history, bookmarks, downloads and chats; passwords can
  be exported to CSV (Passwords > Export; the file is not encrypted).
- **Delete:** Settings > Clear browsing data (history, chats, cookies, site storage, cache), per
  site or everything; delete single chats; uninstalling and deleting the profile folder removes
  everything.
- **Opt out:** turn off search suggestions, the daily update check and site-fix rule updates;
  set sites to "Don't share with Myth".

If you contacted us and want your message deleted, write to the contact above. Depending on
where you live you may have rights under the EU/UK GDPR, the Malaysian Personal Data Protection
Act 2010 (as amended in 2024), the California Consumer Privacy Act or similar laws, including
access, correction, deletion and complaint to your data-protection authority. We do not sell or
share personal information.

## Importing passwords from other browsers

"Import from Chrome / Edge" runs only when you start it. It reads that browser's password file
on your own computer, decrypts it with your Windows account and stores the passwords in
MythBrowse's encrypted store. Nothing is sent over the network.

## Children

MythBrowse has no accounts and collects no personal data from anyone. It is a general-purpose
browser and is not directed at children under 13. AI providers set their own age limits.

## Changes

Changes to this notice are described in [CHANGELOG.md](CHANGELOG.md) and the date above is
updated.
