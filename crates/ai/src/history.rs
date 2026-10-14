// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Saved conversations, model side: turning [`ChatMessage`]s into storable parts and back,
//! rebuilding the model context of a resumed chat within its budget, replaying a saved chat as
//! UI events, and the sidebar's date groups. Storage itself is `mythbrowse-storage`; this
//! module only knows the shapes, so it stays testable without a database.

use serde_json::{Value, json};

use crate::chat::{ChatMessage, ChatRole, ToolCall, trim_history};

/// Longest tool-call `arguments` kept in a saved chat (bytes). Arguments carry what Myth typed
/// or which URL it opened; a page's worth of text is never needed to continue a chat.
pub const MAX_SAVED_ARGUMENTS: usize = 2 * 1024;

/// A message as stored: role name, text, tool calls as JSON, tool call id and tool name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedParts {
    pub role: &'static str,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}

/// What is stored of a message. Tool output is summarized by the caller (the storage crate's
/// `summarize_tool_output`), so this only shortens tool-call arguments.
pub fn to_saved(m: &ChatMessage) -> SavedParts {
    let calls: Vec<ToolCall> = m
        .tool_calls
        .iter()
        .map(|c| ToolCall {
            id: c.id.clone(),
            name: c.name.clone(),
            arguments: clip(&c.arguments, MAX_SAVED_ARGUMENTS),
        })
        .collect();
    SavedParts {
        role: m.role.as_str(),
        content: m.content.clone(),
        tool_calls: (!calls.is_empty())
            .then(|| serde_json::to_string(&calls).expect("tool calls serialize")),
        tool_call_id: m.tool_call_id.clone(),
        name: m.name.clone(),
    }
}

/// A stored message back as a [`ChatMessage`]. System messages and unknown roles are skipped
/// (the system prompt is rebuilt for every run).
pub fn from_saved(
    role: &str,
    content: &str,
    tool_calls: Option<&str>,
    tool_call_id: Option<&str>,
    name: Option<&str>,
) -> Option<ChatMessage> {
    let mut m = match role {
        "user" => ChatMessage::user(content),
        "assistant" => ChatMessage::assistant(content),
        "tool" => {
            let mut m = ChatMessage::assistant(content);
            m.role = ChatRole::Tool;
            m.tool_call_id = tool_call_id.map(str::to_owned);
            m.name = name.map(str::to_owned);
            m
        }
        _ => return None,
    };
    if m.role == ChatRole::Assistant {
        m.tool_calls = tool_calls
            .and_then(|j| serde_json::from_str::<Vec<ToolCall>>(j).ok())
            .unwrap_or_default();
    }
    Some(m)
}

/// The model context for continuing a saved chat: the newest messages within `budget_chars`
/// (see [`trim_history`]), with any tool exchange left incomplete by an interrupted run
/// closed off, so every tool call has its result (providers reject a dangling call).
pub fn resume_context(messages: &[ChatMessage], budget_chars: usize) -> Vec<ChatMessage> {
    let mut out = trim_history(messages, budget_chars);
    let mut i = 0;
    while i < out.len() {
        if out[i].role == ChatRole::Assistant && !out[i].tool_calls.is_empty() {
            let calls = out[i].tool_calls.clone();
            let mut j = i + 1;
            let mut answered = Vec::new();
            while j < out.len() && out[j].role == ChatRole::Tool {
                if let Some(id) = &out[j].tool_call_id {
                    answered.push(id.clone());
                }
                j += 1;
            }
            for c in calls.iter().filter(|c| !answered.contains(&c.id)) {
                out.insert(
                    j,
                    ChatMessage::tool_result(c, "Stopped before this finished."),
                );
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn tool_label(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut c = words.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "Tool".to_owned(),
    }
}

/// A saved chat as the UI event log (`user`, `start`, `text`, `tool`, `end`), so the chat
/// page shows it like a live one. Tool steps show their stored summary's first line.
pub fn replay_events(messages: &[ChatMessage]) -> Vec<Value> {
    let mut ev = Vec::new();
    let mut open = false;
    let mut had_text = false;
    for m in messages {
        match m.role {
            ChatRole::User => {
                if open {
                    ev.push(json!({ "k": "end", "state": "done" }));
                }
                ev.push(json!({ "k": "user", "text": m.content }));
                open = false;
            }
            ChatRole::Assistant | ChatRole::Tool => {
                if !open {
                    ev.push(json!({ "k": "start", "replay": true }));
                    open = true;
                    had_text = false;
                }
                if m.role == ChatRole::Tool {
                    let first = m.content.lines().next().unwrap_or("").trim();
                    let failed = first.starts_with("Error") || first.starts_with("Stopped");
                    let name = m.name.clone().unwrap_or_default();
                    ev.push(json!({
                        "k": "tool", "id": m.tool_call_id, "name": name,
                        "state": if failed { "error" } else { "done" },
                        "summary": if first.is_empty() { tool_label(&name) } else { clip(first, 120) },
                    }));
                    continue;
                }
                if !m.content.trim().is_empty() {
                    let text = if had_text {
                        format!("\n\n{}", m.content)
                    } else {
                        m.content.clone()
                    };
                    ev.push(json!({ "k": "text", "text": text }));
                    had_text = true;
                }
                for c in &m.tool_calls {
                    ev.push(json!({
                        "k": "tool", "id": c.id, "name": c.name, "state": "done",
                        "summary": tool_label(&c.name),
                    }));
                }
            }
            ChatRole::System => {}
        }
    }
    if open {
        ev.push(json!({ "k": "end", "state": "done" }));
    }
    ev
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Sidebar group of a chat last changed at `updated_ms`, seen at `now_ms` by a page whose
/// `getTimezoneOffset()` is `tz_offset_min`: Today, This week (the six days before today) or
/// Earlier. (Pinned chats are grouped apart by the caller.)
pub fn date_group(updated_ms: i64, now_ms: i64, tz_offset_min: i32) -> &'static str {
    let shift = i64::from(tz_offset_min) * 60_000;
    let day = |ms: i64| (ms - shift).div_euclid(DAY_MS);
    let diff = day(now_ms) - day(updated_ms);
    match diff {
        i64::MIN..=0 => "Today",
        1..=6 => "This week",
        _ => "Earlier",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    #[test]
    fn saved_round_trip() {
        let c = call(
            "c1",
            "open_tab",
            &format!("{{\"url\":\"{}\"}}", "u".repeat(5000)),
        );
        let msgs = [
            ChatMessage::user("hi"),
            ChatMessage::assistant_with_tools("", vec![c.clone()]),
            ChatMessage::tool_result(&c, "Opened"),
            ChatMessage::assistant("done"),
        ];
        let back: Vec<ChatMessage> = msgs
            .iter()
            .map(to_saved)
            .filter_map(|p| {
                from_saved(
                    p.role,
                    &p.content,
                    p.tool_calls.as_deref(),
                    p.tool_call_id.as_deref(),
                    p.name.as_deref(),
                )
            })
            .collect();
        assert_eq!(back.len(), 4);
        assert_eq!(back[0], msgs[0]);
        assert_eq!(back[2], msgs[2]);
        assert_eq!(back[3], msgs[3]);
        assert_eq!(back[1].tool_calls[0].id, "c1");
        assert!(back[1].tool_calls[0].arguments.len() <= MAX_SAVED_ARGUMENTS);
        assert!(from_saved("system", "x", None, None, None).is_none());
        assert!(to_saved(&msgs[0]).tool_calls.is_none());
    }

    #[test]
    fn resume_trims_to_budget_and_closes_dangling_calls() {
        let c = call("c9", "click", "{}");
        let mut msgs = Vec::new();
        for i in 0..50 {
            msgs.push(ChatMessage::user(format!(
                "question {i} {}",
                "q".repeat(400)
            )));
            msgs.push(ChatMessage::assistant(format!(
                "answer {i} {}",
                "a".repeat(400)
            )));
        }
        msgs.push(ChatMessage::user("click it"));
        msgs.push(ChatMessage::assistant_with_tools("", vec![c])); // the run was stopped here
        let ctx = resume_context(&msgs, 5_000);
        let total: usize = ctx.iter().map(crate::chat::message_chars).sum();
        assert!(total < 5_200, "about the budget: {total}");
        assert_eq!(ctx[0].role, ChatRole::User);
        let last = ctx.last().unwrap();
        assert_eq!(last.role, ChatRole::Tool);
        assert_eq!(last.tool_call_id.as_deref(), Some("c9"));
        assert!(ctx.len() < msgs.len());
        // Everything fits: nothing dropped, only the dangling call answered.
        assert_eq!(resume_context(&msgs, usize::MAX).len(), msgs.len() + 1);
    }

    #[test]
    fn replay_matches_the_live_event_shapes() {
        let c = call("c1", "open_tab", "{}");
        let msgs = vec![
            ChatMessage::user("open servo"),
            ChatMessage::assistant_with_tools("Opening.", vec![c.clone()]),
            ChatMessage::tool_result(&c, "Opened https://servo.org/\nmore"),
            ChatMessage::assistant("It is open."),
            ChatMessage::user("thanks"),
        ];
        let ev = replay_events(&msgs);
        let kinds: Vec<&str> = ev.iter().map(|e| e["k"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "user", "start", "text", "tool", "tool", "text", "end", "user"
            ]
        );
        assert_eq!(ev[3]["summary"], "Open tab");
        assert_eq!(ev[4]["summary"], "Opened https://servo.org/");
        assert_eq!(ev[5]["text"], "\n\nIt is open.");
    }

    #[test]
    fn date_groups_follow_the_page_time_zone() {
        // 2026-10-11 00:30 in Kuala Lumpur (UTC+8, getTimezoneOffset() = -480).
        let now = 1_791_649_800_000_i64; // 2026-10-10T16:30:00Z
        let h = 3_600_000;
        assert_eq!(date_group(now - h / 4, now, -480), "Today");
        assert_eq!(
            date_group(now - h, now, -480),
            "This week",
            "23:30 yesterday in KL"
        );
        assert_eq!(date_group(now - h, now, 0), "Today", "same instant in UTC");
        assert_eq!(date_group(now - 6 * 24 * h, now, -480), "This week");
        assert_eq!(date_group(now - 8 * 24 * h, now, -480), "Earlier");
        assert_eq!(date_group(now + h, now, -480), "Today", "clock skew");
    }
}
