// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Provider-neutral chat types shared by the model clients and the agent loop.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl ChatRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        }
    }
}

/// A tool call requested by the model. `arguments` is the raw JSON text the model produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// An image sent to a vision-capable model (a page screenshot): base64 data, never a URL.
/// Images live only in memory for the run; they are never written to the profile (see
/// `history::to_saved`) or the logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    /// `image/jpeg` or `image/png`.
    pub mime: String,
    /// Base64 (standard alphabet, padded) of the encoded image.
    pub data_b64: String,
    pub width: u32,
    pub height: u32,
}

impl ImageData {
    /// Rough context cost in characters (for the history budget): providers bill a 1024 px
    /// screenshot at about 800-1500 tokens.
    pub const BUDGET_CHARS: usize = 4_000;

    /// `data:image/jpeg;base64,...` (OpenAI-compatible `image_url` parts).
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.data_b64)
    }
}

impl std::fmt::Debug for ImageData {
    // Never dump a megabyte of base64 into a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImageData({} {}x{}, {} b64 chars)",
            self.mime,
            self.width,
            self.height,
            self.data_b64.len()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// Assistant messages: the tools it asked to run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Tool messages: which call this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool messages: the tool's name (Ollama wants it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Images sent with this message (vision models only; stripped for text-only models).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageData>,
    /// A user-role message the agent added to carry a tool's screenshot (providers only
    /// accept images in user messages). Not the user's words: its text never names trusted
    /// sites for the navigation guard.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub attachment: bool,
    /// Page context the browser added for this run (the Myth panel: page text, selection,
    /// open tabs; see `assist`). Always an `attachment` too (never names trusted sites);
    /// removed from the history when the run ends, so it is never saved or resent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub context: bool,
}

impl ChatMessage {
    fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            images: Vec::new(),
            attachment: false,
            context: false,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(ChatRole::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ChatRole::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(ChatRole::Assistant, content)
    }

    pub fn assistant_with_tools(content: impl Into<String>, calls: Vec<ToolCall>) -> Self {
        let mut m = Self::new(ChatRole::Assistant, content);
        m.tool_calls = calls;
        m
    }

    /// The user's message with a screenshot of their tab ("Ask about this page").
    pub fn user_with_image(content: impl Into<String>, image: ImageData) -> Self {
        let mut m = Self::new(ChatRole::User, content);
        m.images.push(image);
        m
    }

    /// A tool's screenshot, carried in a user-role message right after the tool results.
    pub fn screenshot(content: impl Into<String>, image: ImageData) -> Self {
        let mut m = Self::new(ChatRole::User, content);
        m.images.push(image);
        m.attachment = true;
        m
    }

    /// Page context for the next user message (see [`ChatMessage::context`]).
    pub fn page_context(content: impl Into<String>) -> Self {
        let mut m = Self::new(ChatRole::User, content);
        m.attachment = true;
        m.context = true;
        m
    }

    pub fn tool_result(call: &ToolCall, content: impl Into<String>) -> Self {
        let mut m = Self::new(ChatRole::Tool, content);
        m.tool_call_id = Some(call.id.clone());
        m.name = Some(call.name.clone());
        m
    }
}

/// Rough context budget, in characters, for a resumed conversation (~4 characters per token):
/// local models usually run with an 8K-token window, hosted ones with far more (kept modest
/// so a long old chat does not cost a fortune on every turn).
pub fn context_budget_chars(local: bool) -> usize {
    if local { 24_000 } else { 120_000 }
}

/// Size of a message for the context budget: its text plus its tool calls.
pub fn message_chars(m: &ChatMessage) -> usize {
    m.content.len()
        + m.images.len() * ImageData::BUDGET_CHARS
        + m.tool_calls
            .iter()
            .map(|c| c.name.len() + c.arguments.len() + 16)
            .sum::<usize>()
        + 8
}

/// The newest part of `messages` that fits in `max_chars`, cut so it starts at a user
/// message (never with a tool result or tool call whose other half was dropped). The last
/// user turn is always kept, even when it alone is over the budget.
pub fn trim_history(messages: &[ChatMessage], max_chars: usize) -> Vec<ChatMessage> {
    let mut used = 0usize;
    let mut start = messages.len();
    while start > 0 {
        let n = message_chars(&messages[start - 1]);
        if used + n > max_chars {
            break;
        }
        used += n;
        start -= 1;
    }
    let first_user = messages[start..]
        .iter()
        .position(|m| m.role == ChatRole::User)
        .map(|i| start + i)
        .or_else(|| messages.iter().rposition(|m| m.role == ChatRole::User));
    match first_user {
        Some(i) => messages[i..].to_vec(),
        None => Vec::new(),
    }
}

/// One finished model turn: the text it streamed and the tools it wants to run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum AiError {
    /// The server answered with an error status. `message` is already redacted.
    #[error("{message}")]
    Http { status: u16, message: String },
    /// Could not reach the server (DNS, refused, TLS, timeout). Redacted.
    #[error("{0}")]
    Transport(String),
    /// The server's reply could not be understood.
    #[error("unexpected reply from the model server: {0}")]
    Protocol(String),
    #[error("missing API key: {0}")]
    MissingKey(String),
    /// The model does not accept tool definitions (some Ollama models).
    #[error("this model does not support tools")]
    ToolsUnsupported,
    #[error("stopped")]
    Cancelled,
    #[error("{0}")]
    Config(String),
}

impl AiError {
    /// Short hint shown under an error in the chat (what the user can do about it).
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            AiError::Http { status: 401, .. } | AiError::Http { status: 403, .. } => {
                Some("Check the API key in model settings.")
            }
            AiError::Http { status: 404, .. } => {
                Some("Check the base URL and the model name in model settings.")
            }
            AiError::Transport(_) => {
                Some("Check that the model server is running and the base URL is right.")
            }
            AiError::MissingKey(_) => Some("Add an API key in model settings."),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv() -> Vec<ChatMessage> {
        let call = ToolCall {
            id: "c1".into(),
            name: "open_tab".into(),
            arguments: "{}".into(),
        };
        vec![
            ChatMessage::user("a".repeat(100)),
            ChatMessage::assistant("b".repeat(100)),
            ChatMessage::user("c".repeat(100)),
            ChatMessage::assistant_with_tools("", vec![call.clone()]),
            ChatMessage::tool_result(&call, "d".repeat(300)),
            ChatMessage::assistant("e".repeat(100)),
            ChatMessage::user("f".repeat(50)),
        ]
    }

    #[test]
    fn trim_keeps_everything_that_fits() {
        let m = conv();
        assert_eq!(trim_history(&m, 1_000_000), m);
        assert!(trim_history(&[], 10).is_empty());
    }

    #[test]
    fn trim_starts_at_a_user_message_and_keeps_tool_pairs() {
        let m = conv();
        // Room for the last four messages: the cut lands inside the tool exchange, so the
        // result moves on to the next user message rather than start with a tool result.
        let budget: usize = m[3..].iter().map(message_chars).sum();
        let t = trim_history(&m, budget);
        assert_eq!(t, m[6..].to_vec());
        // Room for the whole second turn: it is kept with its tool call and result.
        let budget: usize = m[2..].iter().map(message_chars).sum();
        let t = trim_history(&m, budget);
        assert_eq!(t, m[2..].to_vec());
        assert_eq!(t[0].role, ChatRole::User);
    }

    #[test]
    fn trim_always_keeps_the_last_user_turn() {
        let mut m = conv();
        m.push(ChatMessage::assistant("g".repeat(10_000)));
        let t = trim_history(&m, 10);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].role, ChatRole::User);
        assert!(context_budget_chars(true) < context_budget_chars(false));
    }
}
