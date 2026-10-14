// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/.
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! MythBrowse AI layer (open-source build).
//!
//! * [`config`]: `ai.toml` (providers, roles). API keys never live in it.
//! * [`secrets`]: local key storage (Windows DPAPI; Secret Service or an encrypted file
//!   elsewhere) and the redacting [`Secret`] type.
//! * [`llm`]: streaming clients for OpenAI-compatible servers and Ollama (bring your own key,
//!   or a local model).
//! * [`chat`]: messages, turns, context budgets.
//! * [`history`]: saved conversations (storable shape, resume context, date groups).
//! * [`agent_api`]: the boundary between the browser and an assistant implementation
//!   ([`AgentProvider`]). The open build ships [`ChatOnly`]: plain chat with the user's model,
//!   no browser control.
//! * [`approval`]: the generic "ask the user first" request shown on the approval card.
//! * [`page_chat`]: the Myth panel over web pages (page text / selection as context for a
//!   chat message, per-site "Don't share with Myth").
//! * [`reauth`]: OS re-authentication before revealing saved passwords (see its docs).
//!
//! The autonomous Myth agent (browser control, its tools, prompts and safety heuristics) is
//! proprietary and not part of this repository. Official JustMyth builds plug it in through
//! [`AgentProvider`]; see `ARCHITECTURE.md`.
//!
//! Network work is blocking and runs on worker threads; nothing here touches Servo.

pub mod agent_api;
pub mod approval;
pub mod chat;
pub mod config;
pub mod history;
pub mod llm;
pub mod page_chat;
pub mod reauth;
pub mod secrets;

pub use agent_api::{
    AgentEvent, AgentProvider, Capabilities, ChatOnly, DEFAULT_MAX_STEPS, RunRequest,
    current_tab_prompt, system_prompt,
};
pub use approval::{AllowAll, ApprovalRequest, Approver};
pub use chat::{
    AiError, ChatMessage, ChatRole, ImageData, ToolCall, Turn, context_budget_chars, message_chars,
    trim_history,
};
pub use config::{
    AiConfig, CONFIG_FILE, ConfigError, MAIN_PROVIDER, ProviderConfig, Role, Roles, VisionMode,
    VisionSettings, secret_name,
};
pub use llm::{ApiKind, Endpoint, ModelInfo};
pub use secrets::{MemoryStore, PlatformStore, Secret, SecretError, SecretStore};
