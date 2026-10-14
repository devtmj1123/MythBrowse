# Architecture

MythBrowse is one Rust process embedding the Servo 0.1.4 web engine. Web pages *and* the
browser's own interface are rendered by Servo; the shell composes them into one frameless
window.

```
 ui/*.html,css,js (tabs, address bar, chat, settings)
        │ loaded from the binary
        ▼
 ┌─────────────────────────────── mythbrowse.exe (crates/shell) ───────────────────────────────┐
 │  chrome webview (Servo) ──── token-protected RPC: https://mythbrowse.internal/api/* ───┐    │
 │        ▲ ChromeState JSON                                                               │    │
 │  ┌─────┴───────────────────── AppState (winit event loop) ───────────────────────────┐ │    │
 │  │ ui_host  files_host  password_host  cookies_host  settings_host  perf_host        │◀┘    │
 │  │ compat_host  sync_host*  glass / compose (GL compositing of chrome + page card)   │      │
 │  │ agent_host = ai_host.rs (open build: chat only) | private overlay (official only) │      │
 │  │ assist_host (Myth panel over pages)                                               │      │
 │  └────┬─────────────────┬───────────────────┬──────────────────────┬─────────────────┘      │
 │       │                 │                   │                      │                        │
 │  web page webviews  crates/engine       crates/ai               crates/compat               │
 │  (Servo, per tab)   tabs, omnibox,      config, secrets, llm,   detector, rules, signed     │
 │                     chrome RPC,         chat, history,          remote rules, AI-repair     │
 │                     page input          page_chat, approval,    review                      │
 │                                         AgentProvider + ChatOnly                            │
 └─────────────────────────────────────────────────────────────────────────────────────────────┘
   crates/storage (SQLite profile) · crates/passwords · crates/files (downloads) · crates/update
   crates/blocker · crates/notify · crates/sync* · third_party/ (patched Servo crates)

 Network: websites (Servo) · your AI provider (crates/ai) · DuckDuckGo suggestions ·
          GitHub Releases (updater) · signed site-fix rules (official builds only)
 * = compiled out by default
```

## Crates

| Crate | Role |
|---|---|
| `crates/shell` | The executable: window, event loop, tab strip, composition of chrome + page card, all `*_host` modules that serve the UI's RPCs. |
| `crates/engine` | Servo embedding: tab model and sleeping, omnibox, chrome RPC parsing (`chrome.rs`, `agent_api.rs`), page input and smooth scrolling, window layout, glass/glow helpers. Its pure modules test without Servo (`--no-default-features`). |
| `crates/ai` | Model clients (OpenAI-compatible, Ollama; streaming), `ai.toml`, OS key storage, chat types and history, `page_chat` (Myth panel context), `approval` (generic approval request) and the **`AgentProvider` boundary**. |
| `crates/compat` | Site compatibility: detector, declarative rules (`rules/`), Ed25519-signed remote rulesets, local rules, static review of AI-suggested fixes, crash log, UA presets. |
| `crates/storage` | SQLite profile: history, bookmarks, chats, permissions, downloads, session, prefs. |
| `crates/passwords`, `crates/files`, `crates/update`, `crates/sync`, `crates/blocker`, `crates/notify` | Password store, downloads and pickers, updater, E2E sync client (off by default), blocklist, Windows toasts. |
| `third_party/` | Servo 0.1.4 crates, Stylo and surfman with our fixes (MPL / MIT / Apache, original notices). |

## The open-core boundary

What is open and what is not is decided at two seams, both visible in this repository:

1. **`mythbrowse_ai::AgentProvider`** (`crates/ai/src/agent_api.rs`). The shell hands a
   conversation to a provider on a worker thread and renders the `AgentEvent`s it emits (text,
   notices, and tool start/end for providers with browser control). The open build ships
   `ChatOnly`: one streamed answer from the user's model, `Capabilities { browser_control:
   false }`, page context attached only as an untrusted, unsaved attachment. A provider that
   asks the user anything goes through the generic `approval::ApprovalRequest` card.
2. **The shell's `agent` cargo feature** (`crates/shell/Cargo.toml`). Off (this repository):
   `mod agent_host` is `src/ai_host.rs` (chat host, saved history, model settings; every hook
   for "pages Myth controls" answers "none"). Official builds apply a private overlay that turns
   `agent` on and compiles the proprietary agent host as `mod agent_host` instead: the agent
   provider, hidden agent tabs, its tools and its agent-specific safeguards. The rest of the
   shell is the same code in both builds. Enabling `agent` in this repository stops the build
   with a clear error (`compile_error!`) because the private overlay is absent.

Safeguards that protect ordinary browsing are open and active in every build: the internal RPC
answers only requests carrying the per-session token that only the browser's own pages receive;
pop-ups, permissions and page dialogs go through the user; downloads check dangerous file types;
the updater only accepts this repository's release files with a matching SHA-256; remote
site-fix rules must be signed, newer than the installed set, at most 2 MB, and may not carry
JavaScript that is not already built into the browser.

## Data flow of a chat message

1. The UI calls `chat_send` (new tab) or `assist_send` (Myth panel) on the internal RPC.
2. `ai_host` records the user message (saved to SQLite unless private), adds the page context
   attachment for that request only, and spawns a worker.
3. The worker runs `ChatOnly::run`: `Endpoint::stream_chat` to the configured provider; text
   fragments flow back over a channel and are appended to the conversation's event log.
4. The UI polls `chat_poll` and renders the log. When the run ends the reply is saved and the
   attachment is dropped.

## The UI

`ui/` is plain HTML/CSS/JS embedded in the binary. `ui/design/` holds the design tokens
(`tokens.css`), the time-of-day theme (`theme.js`), the sky (`sky.js`), line icons
(`icons.js`), motion and glass helpers (`motion.js`) and the custom select (`select.js`).
