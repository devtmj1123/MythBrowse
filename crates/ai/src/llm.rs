// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Model clients: OpenAI-compatible Chat Completions (any base URL: OpenAI, OpenRouter, Groq,
//! LM Studio, vLLM, ...) and Ollama's native `/api/chat`. Both stream: OpenAI as server-sent
//! events, Ollama as newline-delimited JSON. Requests are blocking (`ureq`) and meant to run on
//! a worker thread; `cancel` is checked between streamed lines.
//!
//! The API key only ever goes into the `Authorization` header. Every error text that leaves
//! this module passes through [`redact`] first.

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::chat::{AiError, ChatMessage, ChatRole, ToolCall, Turn};
use crate::config::ProviderConfig;
use crate::secrets::{Secret, redact};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKind {
    OpenAi,
    Ollama,
}

/// A configured model endpoint, ready to call.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub kind: ApiKind,
    pub base_url: String,
    pub model: String,
    pub key: Option<Secret>,
    pub keep_alive: Option<String>,
    pub temperature: Option<f32>,
    /// Give up when the server sends nothing for this long (headers or stream lines).
    /// `None`: [`IDLE_TIMEOUT`].
    pub idle_timeout: Option<Duration>,
}

/// No data from the model server for this long ends the request. As long as the response
/// header timeout always was: local servers may load a model for minutes, and some hosted
/// endpoints send nothing until a reasoning model has finished thinking (NVIDIA's
/// nemotron-3.5: over a minute before the first byte).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Extra attempts after a rate limit (429), a server overload (500/502/503/504) or an error
/// the server streams before any text; never after text has been shown.
pub const MAX_RETRIES: u32 = 3;
/// Longest `Retry-After` we honour; longer waits surface the error instead.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(20);
/// An attempt that failed only after this long is not retried: the user has already waited,
/// and a gateway timeout (NVIDIA: 504 after 120 s) would just repeat.
pub const RETRY_ONLY_FASTER_THAN: Duration = Duration::from_secs(30);

const USER_AGENT: &str = concat!("MythBrowse/", env!("CARGO_PKG_VERSION"));
/// Cap on an error body we read (and show, redacted).
const MAX_ERROR_BODY: u64 = 8 * 1024;

impl Endpoint {
    pub fn from_config(cfg: &ProviderConfig, key: Option<Secret>) -> Self {
        match cfg {
            ProviderConfig::OpenaiCompatible {
                base_url, model, ..
            } => Self {
                kind: ApiKind::OpenAi,
                base_url: base_url.trim_end_matches('/').to_owned(),
                model: model.clone(),
                key,
                keep_alive: None,
                temperature: None,
                idle_timeout: None,
            },
            ProviderConfig::Ollama {
                base_url,
                model,
                keep_alive,
            } => Self {
                kind: ApiKind::Ollama,
                base_url: base_url.trim_end_matches('/').to_owned(),
                model: model.clone(),
                key,
                keep_alive: Some(keep_alive.clone()),
                temperature: None,
                idle_timeout: None,
            },
        }
    }

    /// True for loopback base URLs (Ollama, LM Studio, ...): nothing leaves the PC.
    pub fn is_local(&self) -> bool {
        ProviderConfig::Ollama {
            base_url: self.base_url.clone(),
            model: String::new(),
            keep_alive: String::new(),
        }
        .is_local()
    }

    pub fn chat_url(&self) -> String {
        match self.kind {
            ApiKind::OpenAi => format!("{}/chat/completions", self.base_url),
            ApiKind::Ollama => format!("{}/api/chat", self.base_url),
        }
    }

    pub fn models_url(&self) -> String {
        match self.kind {
            ApiKind::OpenAi => format!("{}/models", self.base_url),
            ApiKind::Ollama => format!("{}/api/tags", self.base_url),
        }
    }

    /// The JSON request body for a streamed chat call.
    pub fn build_body(&self, messages: &[ChatMessage], tools: Option<&Value>) -> Value {
        let msgs: Vec<Value> = messages.iter().map(|m| self.message_json(m)).collect();
        let mut body = json!({ "model": self.model, "messages": msgs, "stream": true });
        if let Some(tools) = tools {
            body["tools"] = tools.clone();
        }
        match self.kind {
            ApiKind::OpenAi => {
                if let Some(t) = self.temperature {
                    body["temperature"] = json!(t);
                }
            }
            ApiKind::Ollama => {
                if let Some(k) = &self.keep_alive {
                    body["keep_alive"] = json!(k);
                }
                if let Some(t) = self.temperature {
                    body["options"] = json!({ "temperature": t });
                }
            }
        }
        body
    }

    fn message_json(&self, m: &ChatMessage) -> Value {
        let mut v = json!({ "role": m.role.as_str(), "content": m.content });
        if !m.images.is_empty() {
            match self.kind {
                // OpenAI-compatible: content parts, images as base64 data URLs.
                ApiKind::OpenAi => {
                    let mut parts = vec![json!({ "type": "text", "text": m.content })];
                    parts.extend(m.images.iter().map(
                        |i| json!({ "type": "image_url", "image_url": { "url": i.data_url() } }),
                    ));
                    v["content"] = Value::Array(parts);
                }
                // Ollama: plain base64 strings next to the text.
                ApiKind::Ollama => {
                    v["images"] =
                        Value::Array(m.images.iter().map(|i| json!(i.data_b64)).collect());
                }
            }
        }
        if m.role == ChatRole::Assistant && !m.tool_calls.is_empty() {
            let calls: Vec<Value> = m
                .tool_calls
                .iter()
                .map(|c| match self.kind {
                    ApiKind::OpenAi => json!({
                        "id": c.id, "type": "function",
                        "function": { "name": c.name, "arguments": c.arguments }
                    }),
                    ApiKind::Ollama => json!({
                        "function": {
                            "name": c.name,
                            "arguments": serde_json::from_str::<Value>(&c.arguments)
                                .unwrap_or_else(|_| json!({}))
                        }
                    }),
                })
                .collect();
            v["tool_calls"] = Value::Array(calls);
        }
        if m.role == ChatRole::Tool {
            match self.kind {
                ApiKind::OpenAi => {
                    if let Some(id) = &m.tool_call_id {
                        v["tool_call_id"] = json!(id);
                    }
                }
                ApiKind::Ollama => {
                    if let Some(name) = &m.name {
                        v["tool_name"] = json!(name);
                    }
                }
            }
        }
        v
    }

    fn agent(&self) -> ureq::Agent {
        let mut cfg = ureq::Agent::config_builder();
        // A model server on this machine is never reached through an HTTP(S)_PROXY.
        if self.is_local() {
            cfg = cfg.proxy(None);
        }
        cfg.http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            // Local models can take a while to load before the first token.
            .timeout_recv_response(Some(Duration::from_secs(300)))
            // Bounds a stream reader thread left behind by Stop on a silent server.
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
            .user_agent(USER_AGENT)
            .build()
            .into()
    }

    fn err(&self, text: impl AsRef<str>) -> String {
        redact(text.as_ref(), self.key.as_ref())
    }

    fn transport_error(&self, e: ureq::Error) -> AiError {
        let what = match self.kind {
            ApiKind::Ollama => "Ollama",
            ApiKind::OpenAi => "the model server",
        };
        AiError::Transport(self.err(format!("Could not reach {what} at {}: {e}", self.base_url)))
    }

    fn http_error(&self, status: u16, body: &str) -> AiError {
        let detail = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .or_else(|| v.get("error"))
                    .or_else(|| v.get("message"))
                    .and_then(|m| m.as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| body.chars().take(300).collect());
        let low = detail.to_ascii_lowercase();
        if (status == 400 || status == 500)
            && (low.contains("does not support tools") || low.contains("tools are not supported"))
        {
            return AiError::ToolsUnsupported;
        }
        let prefix = match status {
            401 => "The API key was rejected (401)",
            403 => "Access denied (403)",
            404 => "Not found (404)",
            429 => "Rate limited (429)",
            _ => "The model server returned an error",
        };
        let message = if detail.trim().is_empty() {
            format!("{prefix} [HTTP {status}]")
        } else {
            format!("{prefix} [HTTP {status}]: {}", detail.trim())
        };
        AiError::Http {
            status,
            message: self.err(message),
        }
    }

    /// Stream one chat turn. `on_text` gets each text fragment as it arrives.
    ///
    /// Rate limits (429), overloaded servers (500/502/503/504) and errors the server streams
    /// before any text are retried up to [`MAX_RETRIES`] times (honouring `Retry-After` up to
    /// [`MAX_RETRY_WAIT`], otherwise 1 s, 2 s, 4 s). Nothing is retried once text has reached
    /// `on_text`. `cancel` is honoured at any point, also while the server is silent.
    pub fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&Value>,
        cancel: &AtomicBool,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<Turn, AiError> {
        let body = self.build_body(messages, tools).to_string();
        let mut attempt = 0;
        loop {
            let mut shown = false;
            let started = std::time::Instant::now();
            let result = self.stream_once(&body, cancel, &mut |t| {
                shown = true;
                on_text(t);
            });
            let (err, retry_after) = match result {
                Ok(turn) => return Ok(turn),
                Err(e) => e,
            };
            let Some(wait) = next_retry(&err, retry_after, attempt, shown, started.elapsed())
            else {
                return Err(err);
            };
            attempt += 1;
            log::info!(
                "model request failed ({err}); retry {attempt}/{MAX_RETRIES} in {} ms",
                wait.as_millis()
            );
            let until = std::time::Instant::now() + wait;
            while std::time::Instant::now() < until {
                if cancel.load(Ordering::Relaxed) {
                    return Err(AiError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }

    fn idle_timeout(&self) -> Duration {
        self.idle_timeout.unwrap_or(IDLE_TIMEOUT)
    }

    /// One attempt. The blocking request runs on a helper thread that forwards the stream
    /// line by line, so Stop and the idle timeout work even while the server sends nothing
    /// (no response headers yet, or a stalled stream). On Stop the helper is left to finish
    /// on its own; it exits at its next line or when the connection ends.
    fn stream_once(
        &self,
        body: &str,
        cancel: &AtomicBool,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<Turn, (AiError, Option<Duration>)> {
        enum Wire {
            Head {
                status: u16,
                retry_after: Option<Duration>,
                error_body: String,
            },
            Line(String),
            Fail(AiError),
        }
        let (tx, rx) = std::sync::mpsc::sync_channel::<Wire>(256);
        let ep = self.clone();
        let body = body.to_owned();
        std::thread::Builder::new()
            .name("llm-stream".into())
            .spawn(move || {
                let mut req = ep.agent().post(ep.chat_url());
                req = req.header("Content-Type", "application/json").header(
                    "Accept",
                    "text/event-stream, application/x-ndjson, application/json",
                );
                if let Some(k) = ep.key.as_ref().filter(|k| !k.is_empty()) {
                    req = req.header("Authorization", format!("Bearer {}", k.expose()));
                }
                let mut resp = match req.send(body) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.send(Wire::Fail(ep.transport_error(e)));
                        return;
                    }
                };
                let status = resp.status().as_u16();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let error_body = if status == 200 {
                    String::new()
                } else {
                    resp.body_mut()
                        .with_config()
                        .limit(MAX_ERROR_BODY)
                        .read_to_string()
                        .unwrap_or_default()
                };
                if tx
                    .send(Wire::Head {
                        status,
                        retry_after,
                        error_body,
                    })
                    .is_err()
                    || status != 200
                {
                    return;
                }
                let reader = std::io::BufReader::new(resp.body_mut().as_reader());
                for line in reader.lines() {
                    let msg = match line {
                        Ok(l) => Wire::Line(l),
                        Err(e) => Wire::Fail(AiError::Transport(ep.err(e.to_string()))),
                    };
                    let fail = matches!(msg, Wire::Fail(_));
                    if tx.send(msg).is_err() || fail {
                        return;
                    }
                }
            })
            .map_err(|e| {
                (
                    AiError::Transport(format!("cannot start request: {e}")),
                    None,
                )
            })?;
        let idle = self.idle_timeout();
        let mut decoder = Decoder::new(self.kind);
        let mut last = std::time::Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err((AiError::Cancelled, None));
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Wire::Head { status: 200, .. }) => {}
                Ok(Wire::Head {
                    status,
                    retry_after,
                    error_body,
                }) => return Err((self.http_error(status, &error_body), retry_after)),
                Ok(Wire::Line(line)) => {
                    let done = decoder.push_line(&line, on_text).map_err(|e| {
                        let e = match e {
                            AiError::Protocol(m) => AiError::Protocol(self.err(m)),
                            AiError::Http { status, message } => AiError::Http {
                                status,
                                message: self.err(message),
                            },
                            other => other,
                        };
                        (e, None)
                    })?;
                    if done {
                        break;
                    }
                }
                Ok(Wire::Fail(e)) => return Err((e, None)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if last.elapsed() >= idle {
                        return Err((
                            AiError::Transport(format!(
                                "The model server stopped responding (nothing received for {} s).",
                                idle.as_secs()
                            )),
                            None,
                        ));
                    }
                    continue;
                }
                // The stream ended (the helper is done).
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            last = std::time::Instant::now();
        }
        if cancel.load(Ordering::Relaxed) {
            return Err((AiError::Cancelled, None));
        }
        Ok(decoder.finish())
    }

    /// Model names the server offers (`/models` or Ollama `/api/tags`). Also a connection test.
    pub fn list_models(&self) -> Result<Vec<String>, AiError> {
        Ok(self
            .list_models_detailed()?
            .into_iter()
            .map(|m| m.name)
            .collect())
    }

    /// Like [`Endpoint::list_models`], with each model's size on disk when the server says
    /// (Ollama `/api/tags` gives `size` in bytes; OpenAI-compatible servers give none).
    pub fn list_models_detailed(&self) -> Result<Vec<ModelInfo>, AiError> {
        let mut req = self.agent().get(self.models_url());
        if let Some(k) = self.key.as_ref().filter(|k| !k.is_empty()) {
            req = req.header("Authorization", format!("Bearer {}", k.expose()));
        }
        let mut resp = req.call().map_err(|e| self.transport_error(e))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| AiError::Transport(self.err(e.to_string())))?;
        if status != 200 {
            return Err(self.http_error(status, &text));
        }
        let v: Value =
            serde_json::from_str(&text).map_err(|e| AiError::Protocol(self.err(e.to_string())))?;
        Ok(parse_model_list(self.kind, &v))
    }
}

/// One model a server lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: String,
    /// Size on disk in bytes (Ollama only).
    pub size: Option<u64>,
}

/// The models in a `/models` (OpenAI) or `/api/tags` (Ollama) answer, sorted by name.
fn parse_model_list(kind: ApiKind, v: &Value) -> Vec<ModelInfo> {
    let list = match kind {
        ApiKind::OpenAi => v.get("data"),
        ApiKind::Ollama => v.get("models"),
    };
    let mut models: Vec<ModelInfo> = list
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let name = m
                        .get("id")
                        .or_else(|| m.get("name"))
                        .and_then(Value::as_str)?;
                    Some(ModelInfo {
                        name: name.to_owned(),
                        size: m.get("size").and_then(Value::as_u64).filter(|&n| n > 0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models.dedup_by(|a, b| a.name == b.name);
    models
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// Incremental decoder for a streamed reply (SSE for OpenAI, NDJSON for Ollama).
#[derive(Debug)]
pub struct Decoder {
    kind: ApiKind,
    content: String,
    calls: Vec<PartialCall>,
    finish_reason: Option<String>,
    done: bool,
}

impl Decoder {
    pub fn new(kind: ApiKind) -> Self {
        Self {
            kind,
            content: String::new(),
            calls: Vec::new(),
            finish_reason: None,
            done: false,
        }
    }

    /// Feed one line. Returns `true` when the stream is complete.
    pub fn push_line(
        &mut self,
        line: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<bool, AiError> {
        let line = line.trim_end_matches('\r');
        if self.done {
            return Ok(true);
        }
        match self.kind {
            ApiKind::OpenAi => {
                let Some(data) = line.strip_prefix("data:") else {
                    // Comments (":"), "event:", "id:", blank separators.
                    if line.trim_start().starts_with('{') {
                        // Some servers ignore stream=true and answer with one JSON object.
                        return self.openai_whole(line, on_text);
                    }
                    return Ok(false);
                };
                let data = data.trim();
                if data == "[DONE]" {
                    self.done = true;
                    return Ok(true);
                }
                if data.is_empty() {
                    return Ok(false);
                }
                let v: Value = serde_json::from_str(data)
                    .map_err(|e| AiError::Protocol(format!("bad event: {e}")))?;
                if let Some(err) = v.get("error") {
                    return Err(stream_error(err));
                }
                let Some(choice) = v.pointer("/choices/0") else {
                    return Ok(false);
                };
                if let Some(delta) = choice.get("delta") {
                    if let Some(t) = delta.get("content").and_then(Value::as_str) {
                        if !t.is_empty() {
                            self.content.push_str(t);
                            on_text(t);
                        }
                    }
                    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                        for c in calls {
                            let index = c
                                .get("index")
                                .and_then(Value::as_u64)
                                .unwrap_or(self.calls.len().saturating_sub(1) as u64)
                                as usize;
                            while self.calls.len() <= index {
                                self.calls.push(PartialCall::default());
                            }
                            let p = &mut self.calls[index];
                            if let Some(id) = c.get("id").and_then(Value::as_str) {
                                p.id = id.to_owned();
                            }
                            if let Some(f) = c.get("function") {
                                if let Some(n) = f.get("name").and_then(Value::as_str) {
                                    p.name.push_str(n);
                                }
                                if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                                    p.arguments.push_str(a);
                                }
                            }
                        }
                    }
                }
                if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
                    self.finish_reason = Some(r.to_owned());
                }
                Ok(false)
            }
            ApiKind::Ollama => {
                if line.trim().is_empty() {
                    return Ok(false);
                }
                let v: Value = serde_json::from_str(line)
                    .map_err(|e| AiError::Protocol(format!("bad line: {e}")))?;
                if let Some(err) = v.get("error") {
                    return Err(stream_error(err));
                }
                if let Some(msg) = v.get("message") {
                    if let Some(t) = msg.get("content").and_then(Value::as_str) {
                        if !t.is_empty() {
                            self.content.push_str(t);
                            on_text(t);
                        }
                    }
                    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                        for c in calls {
                            let f = c.get("function").cloned().unwrap_or(Value::Null);
                            let args = match f.get("arguments") {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => other.to_string(),
                                None => "{}".to_owned(),
                            };
                            self.calls.push(PartialCall {
                                id: c
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                name: f
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                arguments: args,
                            });
                        }
                    }
                }
                if v.get("done").and_then(Value::as_bool) == Some(true) {
                    self.finish_reason = v
                        .get("done_reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or(Some("stop".to_owned()));
                    self.done = true;
                    return Ok(true);
                }
                Ok(false)
            }
        }
    }

    fn openai_whole(&mut self, line: &str, on_text: &mut dyn FnMut(&str)) -> Result<bool, AiError> {
        let v: Value =
            serde_json::from_str(line).map_err(|e| AiError::Protocol(format!("bad reply: {e}")))?;
        if let Some(err) = v.get("error") {
            return Err(stream_error(err));
        }
        let msg = v
            .pointer("/choices/0/message")
            .ok_or_else(|| AiError::Protocol("no choices in reply".into()))?;
        if let Some(t) = msg.get("content").and_then(Value::as_str) {
            self.content.push_str(t);
            on_text(t);
        }
        if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                self.calls.push(PartialCall {
                    id: c
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: c
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: c
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_owned(),
                });
            }
        }
        self.finish_reason = v
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.done = true;
        Ok(true)
    }

    pub fn finish(self) -> Turn {
        let tool_calls = self
            .calls
            .into_iter()
            .enumerate()
            .filter(|(_, c)| !c.name.trim().is_empty())
            .map(|(i, c)| ToolCall {
                id: if c.id.is_empty() {
                    format!("call_{i}")
                } else {
                    c.id
                },
                name: clean_tool_name(&c.name),
                arguments: if c.arguments.trim().is_empty() {
                    "{}".to_owned()
                } else {
                    c.arguments
                },
            })
            .collect();
        Turn {
            content: self.content,
            tool_calls,
            finish_reason: self.finish_reason,
        }
    }
}

/// Whether (and after how long) to retry a failed attempt: never once text was shown, never
/// after [`MAX_RETRIES`], never when the attempt itself was slow, else per [`retry_wait`].
fn next_retry(
    err: &AiError,
    retry_after: Option<Duration>,
    attempt: u32,
    shown: bool,
    took: Duration,
) -> Option<Duration> {
    if shown || attempt >= MAX_RETRIES || took >= RETRY_ONLY_FASTER_THAN {
        return None;
    }
    retry_wait(err, retry_after, attempt)
}

/// How long to wait before retrying after `err`, or `None` if it is not worth retrying.
fn retry_wait(err: &AiError, retry_after: Option<Duration>, attempt: u32) -> Option<Duration> {
    let retryable = match err {
        AiError::Http { status, .. } => matches!(status, 200 | 429 | 500 | 502 | 503 | 504),
        _ => false,
    };
    if !retryable {
        return None;
    }
    match retry_after {
        Some(d) if d > MAX_RETRY_WAIT => None,
        Some(d) => Some(d.max(Duration::from_millis(200))),
        None => Some(Duration::from_secs(1u64 << attempt.min(4))),
    }
}

/// A tool name as the model meant it. Some OpenAI-compatible servers for gpt-oss leak chat
/// template tokens into the name (`read_page<|channel|>commentary`, observed on NVIDIA's API,
/// sometimes with a zero-width space inside the token) or keep the Harmony recipient prefix
/// (`functions.read_page`). Tool names are plain identifiers, so keep the leading identifier.
pub fn clean_tool_name(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_prefix("functions.").unwrap_or(t);
    let end = t
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(t.len());
    if end == 0 {
        return t.to_owned();
    }
    t[..end].to_owned()
}

fn stream_error(err: &Value) -> AiError {
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| err.as_str().map(str::to_owned))
        .unwrap_or_else(|| err.to_string());
    let low = message.to_ascii_lowercase();
    if low.contains("does not support tools") {
        return AiError::ToolsUnsupported;
    }
    AiError::Http {
        status: 200,
        message: format!("The model server reported an error: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(kind: ApiKind, lines: &[&str]) -> (Turn, String) {
        let mut d = Decoder::new(kind);
        let mut streamed = String::new();
        for l in lines {
            if d.push_line(l, &mut |t| streamed.push_str(t)).unwrap() {
                break;
            }
        }
        (d.finish(), streamed)
    }

    #[test]
    fn openai_sse_text_and_tool_calls() {
        let lines = [
            ": keep-alive",
            r#"data: {"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"open_tab","arguments":"{\"url\":"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"https://example.com\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
            r#"data: {"choices":[{"delta":{"content":"ignored"}}]}"#,
        ];
        let (turn, streamed) = collect(ApiKind::OpenAi, &lines);
        assert_eq!(streamed, "Hello");
        assert_eq!(turn.content, "Hello");
        assert_eq!(turn.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            turn.tool_calls,
            vec![ToolCall {
                id: "call_a".into(),
                name: "open_tab".into(),
                arguments: r#"{"url":"https://example.com"}"#.into()
            }]
        );
    }

    #[test]
    fn tool_names_are_cleaned_of_template_tokens() {
        assert_eq!(
            clean_tool_name("read_page<\u{200b}|channel|>commentary"),
            "read_page"
        );
        assert_eq!(clean_tool_name("click<|channel|>analysis"), "click");
        assert_eq!(clean_tool_name("functions.type_text"), "type_text");
        assert_eq!(clean_tool_name(" list_tabs "), "list_tabs");
        assert_eq!(clean_tool_name("open_tab"), "open_tab");
        assert_eq!(clean_tool_name("<|x|>"), "<|x|>");
        let (turn, _) = collect(
            ApiKind::OpenAi,
            &[
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"read_page<\u200b|channel|>analysis","arguments":"{}"}}]}}]}"#,
            ],
        );
        assert_eq!(turn.tool_calls[0].name, "read_page");
    }

    #[test]
    fn which_errors_are_retried() {
        let http = |status| AiError::Http {
            status,
            message: String::new(),
        };
        assert_eq!(
            retry_wait(&http(429), None, 0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            retry_wait(&http(503), None, 2),
            Some(Duration::from_secs(4))
        );
        assert_eq!(
            retry_wait(&http(429), Some(Duration::from_secs(3)), 0),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            retry_wait(&http(429), Some(Duration::from_secs(600)), 0),
            None
        );
        assert_eq!(
            retry_wait(&http(200), None, 0),
            Some(Duration::from_secs(1))
        );
        for e in [
            http(400),
            http(401),
            http(404),
            AiError::Transport("x".into()),
            AiError::ToolsUnsupported,
            AiError::Cancelled,
        ] {
            assert_eq!(retry_wait(&e, None, 0), None, "{e:?}");
        }
        let fast = Duration::from_millis(500);
        assert!(next_retry(&http(504), None, 0, false, fast).is_some());
        // NVIDIA's 504 'upstream request deadline exceeded' arrives after 120 s.
        assert_eq!(
            next_retry(&http(504), None, 0, false, Duration::from_secs(120)),
            None
        );
        assert_eq!(
            next_retry(&http(429), None, 0, true, fast),
            None,
            "text was shown"
        );
        assert_eq!(next_retry(&http(429), None, MAX_RETRIES, false, fast), None);
    }

    #[test]
    fn openai_non_streamed_reply_is_accepted() {
        let line = r#"{"choices":[{"message":{"role":"assistant","content":"hi there"},"finish_reason":"stop"}]}"#;
        let (turn, streamed) = collect(ApiKind::OpenAi, &[line]);
        assert_eq!(streamed, "hi there");
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn openai_stream_error_event() {
        let mut d = Decoder::new(ApiKind::OpenAi);
        let e = d
            .push_line(r#"data: {"error":{"message":"overloaded"}}"#, &mut |_| {})
            .unwrap_err();
        assert!(e.to_string().contains("overloaded"));
    }

    #[test]
    fn ollama_ndjson_text_and_tools() {
        let lines = [
            r#"{"model":"qwen","message":{"role":"assistant","content":"Sure"},"done":false}"#,
            r#"{"model":"qwen","message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_page","arguments":{"tab":1}}}]},"done":false}"#,
            r#"{"model":"qwen","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"}"#,
        ];
        let (turn, streamed) = collect(ApiKind::Ollama, &lines);
        assert_eq!(streamed, "Sure");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "read_page");
        assert_eq!(turn.tool_calls[0].id, "call_0");
        assert_eq!(turn.tool_calls[0].arguments, r#"{"tab":1}"#);
    }

    #[test]
    fn ollama_tools_unsupported_error() {
        let mut d = Decoder::new(ApiKind::Ollama);
        let e = d
            .push_line(
                r#"{"error":"registry.ollama.ai/library/gemma:2b does not support tools"}"#,
                &mut |_| {},
            )
            .unwrap_err();
        assert_eq!(e, AiError::ToolsUnsupported);
    }

    #[test]
    fn request_bodies() {
        let call = ToolCall {
            id: "c1".into(),
            name: "open_tab".into(),
            arguments: r#"{"url":"https://example.com"}"#.into(),
        };
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hi"),
            ChatMessage::assistant_with_tools("", vec![call.clone()]),
            ChatMessage::tool_result(&call, "ok"),
        ];
        let tools = json!([{"type":"function","function":{"name":"open_tab"}}]);

        let openai = Endpoint::from_config(
            &ProviderConfig::OpenaiCompatible {
                base_url: "http://h/v1/".into(),
                model: "m".into(),
                api_key_env: None,
                api_key_ref: None,
            },
            None,
        );
        assert_eq!(openai.chat_url(), "http://h/v1/chat/completions");
        let b = openai.build_body(&history, Some(&tools));
        assert_eq!(b["stream"], true);
        assert_eq!(
            b["messages"][2]["tool_calls"][0]["function"]["arguments"],
            call.arguments
        );
        assert_eq!(b["messages"][3]["tool_call_id"], "c1");
        assert_eq!(b["tools"], tools);

        let ollama = Endpoint::from_config(
            &ProviderConfig::Ollama {
                base_url: "http://localhost:11434".into(),
                model: "qwen2.5:7b".into(),
                keep_alive: "5m".into(),
            },
            None,
        );
        assert_eq!(ollama.chat_url(), "http://localhost:11434/api/chat");
        let b = ollama.build_body(&history, None);
        assert_eq!(b["keep_alive"], "5m");
        assert_eq!(
            b["messages"][2]["tool_calls"][0]["function"]["arguments"]["url"],
            "https://example.com"
        );
        assert_eq!(b["messages"][3]["tool_name"], "open_tab");
        assert!(b.get("tools").is_none());
    }

    #[test]
    fn images_are_encoded_per_provider() {
        let img = crate::chat::ImageData {
            mime: "image/jpeg".into(),
            data_b64: "QUJD".into(),
            width: 2,
            height: 1,
        };
        let history = vec![
            ChatMessage::user("plain"),
            ChatMessage::screenshot("Screenshot of tab 1", img.clone()),
        ];
        let openai = Endpoint::from_config(
            &ProviderConfig::OpenaiCompatible {
                base_url: "http://h/v1".into(),
                model: "m".into(),
                api_key_env: None,
                api_key_ref: None,
            },
            None,
        );
        let b = openai.build_body(&history, None);
        assert_eq!(
            b["messages"][0]["content"], "plain",
            "text-only stays a string"
        );
        let parts = b["messages"][1]["content"].as_array().unwrap();
        assert_eq!(
            parts[0],
            json!({"type": "text", "text": "Screenshot of tab 1"})
        );
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
        assert_eq!(b["messages"][1]["role"], "user");
        assert!(
            b["messages"][1].get("attachment").is_none(),
            "internal flag never sent"
        );

        let ollama = Endpoint::from_config(
            &ProviderConfig::Ollama {
                base_url: "http://localhost:11434".into(),
                model: "llava".into(),
                keep_alive: "5m".into(),
            },
            None,
        );
        let b = ollama.build_body(&history, None);
        assert_eq!(b["messages"][1]["content"], "Screenshot of tab 1");
        assert_eq!(b["messages"][1]["images"], json!(["QUJD"]));
        assert!(b["messages"][0].get("images").is_none());
    }

    #[test]
    fn http_errors_are_redacted_and_classified() {
        let ep = Endpoint {
            kind: ApiKind::OpenAi,
            base_url: "http://h/v1".into(),
            model: "m".into(),
            key: Some(Secret::new("sk-secret-abcdef")),
            keep_alive: None,
            temperature: None,
            idle_timeout: None,
        };
        let e = ep.http_error(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-secret-abcdef"}}"#,
        );
        let text = e.to_string();
        assert!(text.contains("401"), "{text}");
        assert!(!text.contains("sk-secret-abcdef"), "{text}");
        assert!(e.hint().is_some());
        assert_eq!(
            ep.http_error(
                400,
                r#"{"error":{"message":"model x does not support tools"}}"#
            ),
            AiError::ToolsUnsupported
        );
    }

    #[test]
    fn model_lists_keep_ollama_sizes() {
        let tags = serde_json::json!({ "models": [
            { "name": "qwen3:8b", "size": 5_200_000_000u64 },
            { "name": "gemma3:12b", "size": 8_100_000_000u64 },
            { "name": "qwen3:8b", "size": 1 },
            { "model": "no-name" }
        ] });
        let m = parse_model_list(ApiKind::Ollama, &tags);
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[0],
            ModelInfo {
                name: "gemma3:12b".into(),
                size: Some(8_100_000_000)
            }
        );
        assert_eq!(m[1].name, "qwen3:8b");
        let openai = serde_json::json!({ "data": [{ "id": "gpt-4.1-mini", "object": "model" }] });
        let m = parse_model_list(ApiKind::OpenAi, &openai);
        assert_eq!(
            m,
            vec![ModelInfo {
                name: "gpt-4.1-mini".into(),
                size: None
            }]
        );
        assert!(parse_model_list(ApiKind::Ollama, &serde_json::json!({})).is_empty());
    }
}
