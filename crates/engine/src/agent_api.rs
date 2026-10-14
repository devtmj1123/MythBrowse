// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! RPCs for the AI conversation, model settings and the Tasks panel (`/api/chat_*`, `/api/ai_*`,
//! `/api/task*`). Any internal page may call them (the AI home, the Tasks page); web content
//! cannot (see `chrome::route`). An API key typed in the settings form travels in the
//! `x-mb-secret` request header, never in the URL, so it cannot end up in a log line that
//! prints request URLs.

use url::Url;

use crate::chrome::ApiError;

/// Request header carrying an API key from the settings form.
pub const SECRET_HEADER: &str = "x-mb-secret";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCall {
    /// Send a user message. `conv` = None starts a new conversation. `today` is the page's
    /// local date ("Thu Oct 08 2026"), used in the system prompt.
    ChatSend {
        conv: Option<u64>,
        text: String,
        today: Option<String>,
    },
    /// Events of a conversation from index `since` (streaming by polling).
    ChatPoll {
        conv: u64,
        since: usize,
    },
    /// Stop the running reply / task.
    ChatStop {
        conv: u64,
    },
    /// Answer an approval request.
    ChatApprove {
        conv: u64,
        request: u64,
        allow: bool,
    },
    /// Current model settings (never the key).
    Settings,
    /// Save model settings; the key (if any) comes from [`SECRET_HEADER`].
    SettingsSave {
        kind: String,
        base_url: String,
        model: String,
        clear_key: bool,
        /// `auto` / `on` / `off`; `None` keeps the saved value.
        vision: Option<String>,
        /// Privacy switch: send screenshots to the model at all; `None` keeps it.
        screenshots: Option<bool>,
    },
    /// Test a connection and list models (async; poll with `ProbeResult`).
    Probe {
        kind: String,
        base_url: String,
    },
    ProbeResult {
        id: u64,
    },
    /// All tasks (conversations that used the browser).
    Tasks,
    /// Latest screenshot of an agent tab; `fresh` asks for a new one if the last is stale.
    TaskShot {
        tab: u64,
        fresh: bool,
    },
    /// Hand an agent tab to the user: open its page as a normal tab and stop the task.
    TaskTakeover {
        tab: u64,
    },
    /// A run is paused on a human-verification check: show its tab (window to the front, the
    /// tab focused; a background agent tab is brought into the tab strip first) so the person
    /// can complete the check themselves. The task keeps waiting and continues by itself.
    HandoffShow {
        conv: u64,
    },
    /// Saved conversations for the sidebar / history page, grouped Pinned / Today / This week
    /// / Earlier in the page's time zone (`tz_offset_min` as JS `getTimezoneOffset()`).
    ChatList {
        query: Option<String>,
        tz_offset_min: i32,
    },
    /// Load a saved conversation to continue it; answers with its live `conv` number.
    ChatOpen {
        id: String,
    },
    ChatRename {
        id: String,
        title: String,
    },
    ChatPin {
        id: String,
        pinned: bool,
    },
    ChatDelete {
        id: String,
    },
    /// Delete every saved conversation (here and on synced devices).
    ChatClear,
    /// Chat history settings: retention, private session, chat sync.
    ChatSettings,
    /// `retention`: forever / 30 / 90; `sync`: chats sync on / off.
    ChatSettingsSave {
        retention: Option<String>,
        sync: Option<bool>,
    },
}

/// A conversation id in the chat_* RPCs: a saved chat's hex id, or `s<n>` for a chat of this
/// session that is not saved (private session).
pub fn is_chat_ref(id: &str) -> bool {
    let hex = (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit());
    let session = id.len() > 1
        && id.len() <= 21
        && id.starts_with('s')
        && id[1..].bytes().all(|b| b.is_ascii_digit());
    hex || session
}

pub fn is_agent_op(op: &str) -> bool {
    op.starts_with("chat_")
        || op.starts_with("ai_")
        || op == "tasks"
        || op.starts_with("task_")
        || op.starts_with("handoff_")
}

const MAX_TEXT: usize = 32 * 1024;

impl AgentCall {
    pub fn parse(op: &str, url: &Url) -> Result<AgentCall, ApiError> {
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        let required = |name: &'static str| param(name).ok_or(ApiError::MissingParam(name));
        let num = |name: &'static str| -> Result<u64, ApiError> {
            required(name)?
                .trim()
                .parse::<u64>()
                .map_err(|_| ApiError::BadParam(name))
        };
        let flag = |name: &str| matches!(param(name).as_deref(), Some("1" | "true" | "yes"));
        let chat_id = || -> Result<String, ApiError> {
            let id = required("id")?;
            if is_chat_ref(&id) {
                Ok(id.to_ascii_lowercase())
            } else {
                Err(ApiError::BadParam("id"))
            }
        };
        Ok(match op {
            "chat_send" => {
                let text = required("text")?;
                if text.trim().is_empty() || text.len() > MAX_TEXT {
                    return Err(ApiError::BadParam("text"));
                }
                AgentCall::ChatSend {
                    conv: match param("conv").filter(|c| !c.is_empty()) {
                        Some(c) => Some(c.parse().map_err(|_| ApiError::BadParam("conv"))?),
                        None => None,
                    },
                    text,
                    today: param("today").filter(|t| !t.trim().is_empty() && t.len() < 80),
                }
            }
            "chat_poll" => AgentCall::ChatPoll {
                conv: num("conv")?,
                since: param("since")
                    .map(|s| s.parse().map_err(|_| ApiError::BadParam("since")))
                    .transpose()?
                    .unwrap_or(0),
            },
            // `handoff_cancel`: "Cancel task" on the verification card is a plain stop.
            "chat_stop" | "task_stop" | "handoff_cancel" => {
                AgentCall::ChatStop { conv: num("conv")? }
            }
            "handoff_show" => AgentCall::HandoffShow { conv: num("conv")? },
            "chat_approve" => AgentCall::ChatApprove {
                conv: num("conv")?,
                request: num("request")?,
                allow: flag("allow"),
            },
            "ai_settings" => AgentCall::Settings,
            "ai_settings_save" => AgentCall::SettingsSave {
                kind: required("kind")?,
                base_url: required("base_url")?.trim().to_owned(),
                model: required("model")?.trim().to_owned(),
                clear_key: flag("clear_key"),
                vision: param("vision")
                    .map(|v| v.trim().to_ascii_lowercase())
                    .map(|v| match v.as_str() {
                        "auto" | "on" | "off" => Ok(v),
                        _ => Err(ApiError::BadParam("vision")),
                    })
                    .transpose()?,
                screenshots: param("screenshots").map(|v| v == "1"),
            },
            "ai_probe" => AgentCall::Probe {
                kind: required("kind")?,
                base_url: required("base_url")?.trim().to_owned(),
            },
            "ai_probe_result" => AgentCall::ProbeResult { id: num("id")? },
            "tasks" => AgentCall::Tasks,
            "task_shot" => AgentCall::TaskShot {
                tab: num("tab")?,
                fresh: flag("fresh"),
            },
            "task_takeover" => AgentCall::TaskTakeover { tab: num("tab")? },
            "chat_list" => AgentCall::ChatList {
                query: param("q")
                    .map(|q| q.trim().to_owned())
                    .filter(|q| !q.is_empty() && q.len() <= 200),
                tz_offset_min: match param("tzo") {
                    None => 0,
                    Some(t) => t
                        .trim()
                        .parse::<i32>()
                        .ok()
                        .filter(|t| t.abs() <= 16 * 60)
                        .ok_or(ApiError::BadParam("tzo"))?,
                },
            },
            "chat_open" => AgentCall::ChatOpen { id: chat_id()? },
            "chat_rename" => {
                let title = required("title")?;
                let title = title.trim();
                if title.is_empty() || title.len() > 300 {
                    return Err(ApiError::BadParam("title"));
                }
                AgentCall::ChatRename {
                    id: chat_id()?,
                    title: title.to_owned(),
                }
            }
            "chat_pin" => AgentCall::ChatPin {
                id: chat_id()?,
                pinned: match required("on")?.as_str() {
                    "1" => true,
                    "0" => false,
                    _ => return Err(ApiError::BadParam("on")),
                },
            },
            "chat_delete" => AgentCall::ChatDelete { id: chat_id()? },
            "chat_clear" => AgentCall::ChatClear,
            "chat_settings" => AgentCall::ChatSettings,
            "chat_settings_save" => AgentCall::ChatSettingsSave {
                retention: match param("retention") {
                    None => None,
                    Some(r) if matches!(r.as_str(), "forever" | "30" | "90") => Some(r),
                    Some(_) => return Err(ApiError::BadParam("retention")),
                },
                sync: match param("sync").as_deref() {
                    None => None,
                    Some("1") => Some(true),
                    Some("0") => Some(false),
                    Some(_) => return Err(ApiError::BadParam("sync")),
                },
            },
            other => return Err(ApiError::UnknownOp(other.to_owned())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<AgentCall, ApiError> {
        let url = Url::parse(&format!("https://mythbrowse.internal/api/{s}")).unwrap();
        let op = url.path().strip_prefix("/api/").unwrap().to_owned();
        AgentCall::parse(&op, &url)
    }

    #[test]
    fn parses_chat_ops() {
        assert_eq!(
            p("chat_send?text=hi%20there&today=Thu").unwrap(),
            AgentCall::ChatSend {
                conv: None,
                text: "hi there".into(),
                today: Some("Thu".into())
            }
        );
        assert_eq!(
            p("chat_send?conv=3&text=x").unwrap(),
            AgentCall::ChatSend {
                conv: Some(3),
                text: "x".into(),
                today: None
            }
        );
        assert_eq!(p("chat_send?text=%20"), Err(ApiError::BadParam("text")));
        assert_eq!(
            p("chat_poll?conv=2&since=5").unwrap(),
            AgentCall::ChatPoll { conv: 2, since: 5 }
        );
        assert_eq!(
            p("chat_poll?conv=2").unwrap(),
            AgentCall::ChatPoll { conv: 2, since: 0 }
        );
        assert_eq!(
            p("chat_approve?conv=1&request=4&allow=1").unwrap(),
            AgentCall::ChatApprove {
                conv: 1,
                request: 4,
                allow: true
            }
        );
        assert_eq!(
            p("chat_approve?conv=1&request=4").unwrap(),
            AgentCall::ChatApprove {
                conv: 1,
                request: 4,
                allow: false
            }
        );
        assert_eq!(
            p("task_stop?conv=7").unwrap(),
            AgentCall::ChatStop { conv: 7 }
        );
        assert_eq!(p("chat_stop"), Err(ApiError::MissingParam("conv")));
        assert_eq!(
            p("handoff_cancel?conv=3").unwrap(),
            AgentCall::ChatStop { conv: 3 }
        );
        assert_eq!(
            p("handoff_show?conv=3").unwrap(),
            AgentCall::HandoffShow { conv: 3 }
        );
        assert_eq!(p("handoff_show"), Err(ApiError::MissingParam("conv")));
    }

    #[test]
    fn parses_settings_and_tasks() {
        assert_eq!(
            p("ai_settings_save?kind=ollama&base_url=http://localhost:11434&model=qwen2.5:7b")
                .unwrap(),
            AgentCall::SettingsSave {
                kind: "ollama".into(),
                base_url: "http://localhost:11434".into(),
                model: "qwen2.5:7b".into(),
                clear_key: false,
                vision: None,
                screenshots: None
            }
        );
        assert_eq!(
            p("ai_settings_save?kind=openai&base_url=https://h/v1&model=m&vision=off&screenshots=0")
                .unwrap(),
            AgentCall::SettingsSave {
                kind: "openai".into(),
                base_url: "https://h/v1".into(),
                model: "m".into(),
                clear_key: false,
                vision: Some("off".into()),
                screenshots: Some(false)
            }
        );
        assert_eq!(
            p("ai_settings_save?kind=openai&base_url=https://h/v1&model=m&vision=maybe"),
            Err(ApiError::BadParam("vision"))
        );
        assert_eq!(
            p("task_shot?tab=3&fresh=1").unwrap(),
            AgentCall::TaskShot {
                tab: 3,
                fresh: true
            }
        );
        assert_eq!(p("tasks").unwrap(), AgentCall::Tasks);
        assert!(is_agent_op("chat_poll") && is_agent_op("tasks") && !is_agent_op("history"));
        assert!(matches!(p("ai_nope"), Err(ApiError::UnknownOp(_))));
    }

    #[test]
    fn parses_chat_history_ops() {
        assert_eq!(
            p("chat_list?q=%20tea%20&tzo=-480").unwrap(),
            AgentCall::ChatList {
                query: Some("tea".into()),
                tz_offset_min: -480
            }
        );
        assert_eq!(
            p("chat_list").unwrap(),
            AgentCall::ChatList {
                query: None,
                tz_offset_min: 0
            }
        );
        assert_eq!(p("chat_list?tzo=99999"), Err(ApiError::BadParam("tzo")));
        let id = "0123456789abcdef0123456789ABCDEF";
        assert_eq!(
            p(&format!("chat_open?id={id}")).unwrap(),
            AgentCall::ChatOpen {
                id: id.to_ascii_lowercase()
            }
        );
        assert_eq!(
            p("chat_open?id=s12").unwrap(),
            AgentCall::ChatOpen { id: "s12".into() }
        );
        assert_eq!(p("chat_open?id=../x"), Err(ApiError::BadParam("id")));
        assert_eq!(p("chat_open?id=s"), Err(ApiError::BadParam("id")));
        assert_eq!(
            p("chat_rename?id=s1&title=%20Trip%20").unwrap(),
            AgentCall::ChatRename {
                id: "s1".into(),
                title: "Trip".into()
            }
        );
        assert_eq!(
            p("chat_rename?id=s1&title=%20"),
            Err(ApiError::BadParam("title"))
        );
        assert_eq!(
            p("chat_pin?id=s1&on=1").unwrap(),
            AgentCall::ChatPin {
                id: "s1".into(),
                pinned: true
            }
        );
        assert_eq!(p("chat_pin?id=s1&on=yes"), Err(ApiError::BadParam("on")));
        assert_eq!(
            p("chat_delete?id=s1").unwrap(),
            AgentCall::ChatDelete { id: "s1".into() }
        );
        assert_eq!(p("chat_clear").unwrap(), AgentCall::ChatClear);
        assert_eq!(
            p("chat_settings_save?retention=30&sync=0").unwrap(),
            AgentCall::ChatSettingsSave {
                retention: Some("30".into()),
                sync: Some(false)
            }
        );
        assert_eq!(
            p("chat_settings_save?retention=7"),
            Err(ApiError::BadParam("retention"))
        );
    }
}
