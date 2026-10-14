// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/.
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The AI side of the shell, open-source build: conversations with the user's own model,
//! saved chat history and model settings. Compiled as `crate::agent_host` when the `agent`
//! feature is off (see `main.rs`).
//!
//! Each user message starts a worker thread that hands the conversation to an
//! [`AgentProvider`] (here [`ChatOnly`]: one streamed answer, no browser control). The worker
//! never touches Servo: its events travel to the main thread over one channel ([`HostMsg`])
//! and the event loop is woken to handle them in [`AppState::agent_pump`].
//!
//! The UI streams by polling `/api/chat_poll` (an append-only event log per conversation), so
//! a reloaded page can replay it.
//!
//! Chat history: every conversation is saved in the profile (`mythbrowse_storage::chats`) as it
//! goes, the user's message when sent and the reply when the run ends. `chat_open` loads a
//! saved chat back. In a private session (`--private`) nothing is saved.
//!
//! The browser-facing hooks of the Myth agent (background tabs, approvals, navigation checks
//! in pages the agent controls) keep their signatures so the rest of the shell is the same in
//! both builds; here they report that Myth controls no page.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::StatusCode;
use log::{info, warn};
use mythbrowse_ai::secrets::self_test;
use mythbrowse_ai::{
    AgentEvent, AgentProvider, AiConfig, AiError, ApprovalRequest, ChatMessage, ChatOnly,
    DEFAULT_MAX_STEPS, Endpoint, MAIN_PROVIDER, ProviderConfig, RunRequest, Secret, SecretStore,
    VisionMode, VisionSettings, secret_name,
};
use mythbrowse_engine::agent_api::AgentCall;
use mythbrowse_engine::chrome;
use mythbrowse_engine::glow::{AiActivity, Flash, Motion};
use mythbrowse_engine::servo::{
    CreateNewWebViewRequest, NavigationRequest, OffscreenRenderingContext, WebResourceLoad, WebView,
};
use mythbrowse_engine::tabs::TabId;
use serde_json::{Value, json};
use url::Url;

use crate::AppState;
use mythbrowse_ai::history::{date_group, from_saved, replay_events, resume_context, to_saved};
use mythbrowse_storage::chats::{
    Retention, StoredMessage, new_chat_id, summarize_tool_output, title_from_message,
};

/// Viewport of the (unused in this build) agent rendering context, physical pixels at scale 1.
pub(crate) const AGENT_VIEWPORT: (u32, u32) = (1280, 800);
/// How often streamed text is flushed to the UI's event log.
const TEXT_FLUSH: Duration = Duration::from_millis(60);

/// The assistant this build answers with (official builds plug in the Myth agent instead).
fn provider() -> impl AgentProvider {
    ChatOnly
}

// ------------------------------------------------------------------- worker <-> main

enum HostMsg {
    Event {
        conv: u64,
        run: u64,
        event: Value,
    },
    Ended {
        conv: u64,
        run: u64,
        history: Vec<ChatMessage>,
        outcome: Outcome,
    },
    Probe {
        id: u64,
        result: Value,
    },
}

enum Outcome {
    Done,
    Stopped,
    Failed { text: String, hint: Option<String> },
}

pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// Worker side of a run: sends to the main thread.
#[derive(Clone)]
struct Link {
    conv: u64,
    run: u64,
    tx: Sender<HostMsg>,
    wake: Wake,
}

impl Link {
    fn send(&self, msg: HostMsg) {
        if self.tx.send(msg).is_ok() {
            (self.wake)();
        }
    }

    fn event(&self, event: Value) {
        self.send(HostMsg::Event {
            conv: self.conv,
            run: self.run,
            event,
        });
    }
}

// ------------------------------------------------------------------------- state

/// What a Myth panel message adds to a run (see `agent_send_panel`).
pub(crate) struct PanelSend {
    /// Page context attachment (`mythbrowse_ai::page_chat::context_message`).
    pub(crate) context: Option<ChatMessage>,
    /// Browser control requested (always `false` from the open panel).
    pub(crate) tools: bool,
    /// The user's date, for the system prompt.
    pub(crate) today: String,
}

struct Run {
    id: u64,
    cancel: Arc<AtomicBool>,
}

struct Conversation {
    /// Stable id of the saved chat (`None`: not saved, i.e. a private session).
    key: Option<String>,
    /// How many of `messages` are saved already.
    saved: usize,
    /// Seq of the next message to save.
    next_seq: i64,
    /// Model of the newest run.
    model: String,
    updated_ms: u64,
    title: String,
    messages: Vec<ChatMessage>,
    events: Vec<Value>,
    run: Option<Run>,
    /// The newest run (still accepted for its final history after a stop).
    last_run: u64,
    /// idle | running | done | stopped | error
    status: &'static str,
    step: String,
    /// Asked from this (visible) user tab: the Myth pill shows there.
    here: Option<TabId>,
    /// The user closed the Myth pill of this finished run.
    dismissed: bool,
}

pub(crate) struct AgentHost {
    tx: Sender<HostMsg>,
    rx: Receiver<HostMsg>,
    wake: Wake,
    /// Offscreen context the shell creates for agent tabs; unused by the chat-only host.
    _ctx: Rc<OffscreenRenderingContext>,
    convs: RefCell<BTreeMap<u64, Conversation>>,
    next_id: Cell<u64>,
    profile_dir: Option<PathBuf>,
    secrets: Box<dyn SecretStore>,
    /// `None` = not set up (no ai.toml): the local Ollama default is used.
    config: RefCell<Option<AiConfig>>,
    config_error: RefCell<Option<String>>,
    probes: Arc<Mutex<BTreeMap<u64, Value>>>,
    /// A run asked from a visible tab ended: flash the ring of that tab (taken by the shell).
    pub(crate) flash: Cell<Option<(TabId, Flash)>>,
    /// Ring pulse for a visible tab (taken by the shell; never set by the chat-only host).
    pub(crate) pulse: Cell<Option<(TabId, f32, f32)>>,
    /// Effective motion setting.
    pub(crate) motion: Cell<Motion>,
    /// Private session: no chat (or page) history is saved.
    pub(crate) private: bool,
}

fn unix_ms() -> u64 {
    mythbrowse_storage::unix_now_ms() as u64
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}

fn kind_name(p: &ProviderConfig) -> &'static str {
    match p {
        ProviderConfig::OpenaiCompatible { .. } => "openai",
        ProviderConfig::Ollama { .. } => "ollama",
    }
}

impl AgentHost {
    pub(crate) fn new(
        wake: Wake,
        ctx: Rc<OffscreenRenderingContext>,
        profile_dir: Option<PathBuf>,
        private: bool,
    ) -> Self {
        let (tx, rx) = channel();
        let secrets: Box<dyn SecretStore> = match &profile_dir {
            Some(dir) => Box::new(mythbrowse_ai::PlatformStore::new(dir)),
            None => Box::new(mythbrowse_ai::MemoryStore::default()),
        };
        let (config, error) = match &profile_dir {
            Some(dir) => match AiConfig::load(dir) {
                Ok(c) => (c, None),
                Err(e) => (None, Some(e.to_string())),
            },
            None => (None, None),
        };
        match (&config, &error) {
            (Some(c), _) => {
                let (name, p) = c.provider_for(mythbrowse_ai::Role::Chat);
                info!(
                    "AI: provider `{name}` ({}) model `{}` at {}",
                    kind_name(p),
                    p.model(),
                    p.base_url()
                );
            }
            (None, Some(e)) => warn!("AI settings ignored: {e}"),
            (None, None) => info!("AI: not set up yet (local Ollama default)"),
        }
        info!(
            "AI keys: {}; assistant: {} (no browser control)",
            secrets.backend(),
            provider().name()
        );
        Self {
            tx,
            rx,
            wake,
            _ctx: ctx,
            convs: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(1),
            profile_dir,
            secrets,
            config: RefCell::new(config),
            config_error: RefCell::new(error),
            probes: Arc::new(Mutex::new(BTreeMap::new())),
            flash: Cell::new(None),
            pulse: Cell::new(None),
            motion: Cell::new(Motion::Full),
            private,
        }
    }

    /// The user's vision settings (`[vision]` in ai.toml; kept for the settings form).
    fn vision_settings(&self) -> VisionSettings {
        self.config
            .borrow()
            .as_ref()
            .map(|c| c.vision)
            .unwrap_or_default()
    }

    fn id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    /// True if `webview` is one of the agent's background tabs (there are none in this build).
    pub(crate) fn owns(&self, _webview: &WebView) -> bool {
        false
    }

    /// The saved chat of the run working in `webview` (never: Myth works in no page here).
    pub(crate) fn conv_key_of_view(&self, _webview: &WebView) -> Option<Option<String>> {
        None
    }

    fn effective_provider(&self) -> ProviderConfig {
        let cfg = self.config.borrow().clone().unwrap_or_default();
        cfg.provider_for(mythbrowse_ai::Role::Chat).1.clone()
    }

    /// The chat model's endpoint (key resolved). Site-compat repair asks the same model.
    pub(crate) fn chat_endpoint(&self) -> Endpoint {
        let provider = self.effective_provider();
        let key = provider.resolve_key(self.secrets.as_ref());
        Endpoint::from_config(&provider, key)
    }

    /// Cancel every run (before Servo shuts down).
    pub(crate) fn shutdown(&self) {
        for conv in self.convs.borrow_mut().values_mut() {
            if let Some(run) = conv.run.take() {
                run.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

// --------------------------------------------------------------- main-thread side

impl AppState {
    /// Handle everything the workers sent. Called on every event-loop wake-up.
    pub(crate) fn agent_pump(&self) {
        while let Ok(msg) = self.agent.rx.try_recv() {
            match msg {
                HostMsg::Event { conv, run, event } => self.agent_on_event(conv, run, event),
                HostMsg::Ended {
                    conv,
                    run,
                    history,
                    outcome,
                } => self.agent_on_ended(conv, run, history, outcome),
                HostMsg::Probe { id, result } => {
                    self.agent.probes.lock().expect("probes").insert(id, result);
                }
            }
        }
    }

    fn agent_on_event(&self, conv: u64, run: u64, event: Value) {
        let mut convs = self.agent.convs.borrow_mut();
        let Some(c) = convs.get_mut(&conv) else {
            return;
        };
        if c.run.as_ref().is_none_or(|r| r.id != run) {
            return;
        }
        c.events.push(event);
    }

    fn agent_on_ended(&self, conv: u64, run: u64, history: Vec<ChatMessage>, outcome: Outcome) {
        {
            let mut convs = self.agent.convs.borrow_mut();
            let Some(c) = convs.get_mut(&conv) else {
                return;
            };
            if run != c.last_run {
                return; // a newer message already started another run
            }
            c.messages = history;
            c.updated_ms = unix_ms();
        }
        self.agent_save(conv);
        let mut convs = self.agent.convs.borrow_mut();
        let Some(c) = convs.get_mut(&conv) else {
            return;
        };
        if !c.run.as_ref().is_some_and(|r| r.id == run) {
            return; // stopped by the user: the UI already shows it
        }
        c.run = None;
        let (state, event) = match outcome {
            Outcome::Done => ("done", None),
            Outcome::Stopped => ("stopped", None),
            Outcome::Failed { text, hint } => {
                let settings = hint.is_some();
                (
                    "error",
                    Some(json!({ "k": "error", "text": text, "hint": hint, "settings": settings })),
                )
            }
        };
        if let Some(e) = event {
            c.events.push(e);
        }
        if let Some(user_tab) = c.here {
            match state {
                "done" => self.agent.flash.set(Some((user_tab, Flash::Done))),
                "error" => self.agent.flash.set(Some((user_tab, Flash::Error))),
                _ => {}
            }
        }
        c.events.push(json!({ "k": "end", "state": state }));
        c.status = state;
        if state == "done" {
            c.step = "Done".to_owned();
        }
        drop(convs);
        self.mark_chrome_dirty();
    }

    /// The user closed their tab, or it left the web: its Myth pill goes away (the chat
    /// itself goes on and stays in the chat list).
    pub(crate) fn agent_unbind_user_tab(&self, user_tab: TabId) {
        for c in self.agent.convs.borrow_mut().values_mut() {
            if c.here == Some(user_tab) {
                c.here = None;
            }
        }
    }

    /// True while a run works in this user tab (never: Myth acts on no page here).
    pub(crate) fn agent_uses_user_tab(&self, _user_tab: TabId) -> bool {
        false
    }

    /// What Myth is doing in this user tab, for the ring (it does nothing in pages here).
    pub(crate) fn agent_activity(&self, _user_tab: TabId) -> Option<AiActivity> {
        None
    }

    /// The Myth pill for this user tab: the newest chat asked from it, while it runs and
    /// after it ended until dismissed.
    pub(crate) fn agent_bar(&self, user_tab: TabId) -> Option<Value> {
        let convs = self.agent.convs.borrow();
        let (id, c) = convs.iter().rev().find(|(_, c)| c.here == Some(user_tab))?;
        if c.run.is_none() && c.dismissed {
            return None;
        }
        let state = if c.run.is_some() {
            "thinking"
        } else {
            c.status
        };
        Some(json!({
            "conv": id, "state": state, "step": c.step, "title": c.title,
            "running": c.run.is_some(), "approval": null, "handoff": null,
        }))
    }

    /// A run waiting on a human-verification check (only the agent has those).
    pub(crate) fn agent_handoff_bar(&self, _shown_over: Option<TabId>) -> Option<Value> {
        None
    }

    /// A page finished loading (the agent looks again at paused checks; nothing here).
    pub(crate) fn agent_load_kick(&self, _webview: &WebView) {}

    /// A download or file chooser in a page Myth controls: `None`, Myth controls no page, so
    /// the user's own action is handled normally.
    pub(crate) fn agent_side_approval(
        &self,
        _webview: &WebView,
        _user_tab: Option<TabId>,
        _action: &str,
        _reason: &str,
        _url: &str,
    ) -> Option<Receiver<bool>> {
        None
    }

    /// The conversation whose run works in `webview` (none in this build).
    pub(crate) fn agent_conv_of_page(&self, _webview: &WebView) -> Option<u64> {
        None
    }

    /// Put a page's dialog on a run's approval card (never needed here: no run works in a
    /// page, so page dialogs always go to the user directly).
    pub(crate) fn agent_page_approval(
        &self,
        _conv: u64,
        _request: ApprovalRequest,
    ) -> Option<Receiver<bool>> {
        None
    }

    /// A line in `conv`'s log that is not part of the reply.
    pub(crate) fn agent_note(&self, conv: u64, text: &str) {
        if let Some(c) = self.agent.convs.borrow_mut().get_mut(&conv) {
            c.events.push(json!({ "k": "note", "text": text }));
        }
    }

    /// Esc on the active tab answers a pending Myth request with "Don't allow" (nothing ever
    /// waits here, so the key goes on to the page).
    pub(crate) fn deny_pending_here(&self) -> bool {
        false
    }

    /// Myth pill close button.
    pub(crate) fn agent_dismiss_bar(&self, user_tab: TabId) {
        for c in self.agent.convs.borrow_mut().values_mut() {
            if c.here == Some(user_tab) && c.run.is_none() {
                c.dismissed = true;
            }
        }
    }

    /// Delegate hook: a frame is ready in `webview`; true only for agent tabs (none here).
    pub(crate) fn agent_frame_ready(&self, _webview: &WebView) -> bool {
        false
    }

    /// Delegate hook: a popup from an agent tab (none here): handled as a normal popup.
    pub(crate) fn agent_popup(
        &self,
        _parent: &WebView,
        request: CreateNewWebViewRequest,
    ) -> Result<(), CreateNewWebViewRequest> {
        Err(request)
    }

    /// Delegate hook (`request_navigation`) for pages Myth works in; none here, so every
    /// navigation goes on under the browser's normal rules.
    pub(crate) fn agent_request_navigation(&self, _webview: &WebView, request: NavigationRequest) {
        request.allow();
    }

    /// From `load_web_resource`: a top-level POST in a page Myth works in (none here).
    pub(crate) fn agent_hold_post(
        &self,
        _webview: &WebView,
        load: WebResourceLoad,
    ) -> Option<WebResourceLoad> {
        Some(load)
    }

    // ------------------------------------------------------------------------ RPCs

    /// Run an AI RPC. `secret` is the `x-mb-secret` header (settings form only).
    pub(crate) fn agent_api(
        &self,
        call: AgentCall,
        secret: Option<Secret>,
    ) -> (StatusCode, String) {
        let ok = |v: Value| (StatusCode::OK, chrome::json_ok(v));
        let err = |m: &str| (StatusCode::OK, chrome::json_error(m));
        match call {
            AgentCall::ChatSend { conv, text, today } => match self.agent_send(conv, text, today) {
                Ok(id) => {
                    let key = self
                        .agent
                        .convs
                        .borrow()
                        .get(&id)
                        .and_then(|c| c.key.clone());
                    ok(json!({ "conv": id, "id": key }))
                }
                Err(e) => err(&e),
            },
            AgentCall::ChatList {
                query,
                tz_offset_min,
            } => match self.agent_chat_list(query.as_deref().unwrap_or(""), tz_offset_min) {
                Ok(v) => ok(v),
                Err(e) => err(&e),
            },
            AgentCall::ChatOpen { id } => match self.agent_chat_open(&id) {
                Ok(v) => ok(v),
                Err(e) => err(&e),
            },
            AgentCall::ChatRename { id, title } => match self.agent_chat_rename(&id, &title) {
                Ok(()) => ok(Value::Null),
                Err(e) => err(&e),
            },
            AgentCall::ChatPin { id, pinned } => {
                if id.starts_with('s') {
                    return err("Only saved chats can be pinned.");
                }
                match self.store.pin_chat(&id, pinned) {
                    Ok(_) => ok(json!({ "pinned": pinned })),
                    Err(e) => err(&e.to_string()),
                }
            }
            AgentCall::ChatDelete { id } => match self.agent_chat_delete(&id) {
                Ok(()) => ok(Value::Null),
                Err(e) => err(&e),
            },
            AgentCall::ChatClear => match self.agent_chat_clear() {
                Ok(n) => ok(json!({ "deleted": n })),
                Err(e) => err(&e),
            },
            AgentCall::ChatSettings => ok(self.agent_chat_settings_json()),
            AgentCall::ChatSettingsSave { retention, sync } => {
                if let Some(r) = retention.as_deref().and_then(Retention::parse) {
                    if let Err(e) = self.store.set_chat_retention(r) {
                        return err(&e.to_string());
                    }
                    match self.store.prune_chats(mythbrowse_storage::unix_now_ms()) {
                        Ok(n) if n > 0 => {
                            info!("chat retention {}: {n} old chats removed", r.as_str())
                        }
                        Ok(_) => {}
                        Err(e) => warn!("chat prune failed: {e}"),
                    }
                }
                if let Some(on) = sync
                    && let Err(e) = self.store.set_sync_chats(on)
                {
                    return err(&e.to_string());
                }
                ok(self.agent_chat_settings_json())
            }
            AgentCall::ChatPoll { conv, since } => {
                let convs = self.agent.convs.borrow();
                let Some(c) = convs.get(&conv) else {
                    return err("unknown conversation");
                };
                let since = since.min(c.events.len());
                ok(json!({
                    "events": &c.events[since..],
                    "next": c.events.len(),
                    "status": c.status,
                    "running": c.run.is_some(),
                    "step": c.step,
                    "tabs": [],
                }))
            }
            AgentCall::ChatStop { conv } => {
                self.agent_stop(conv);
                ok(Value::Null)
            }
            AgentCall::ChatApprove { .. } => err("that request is no longer waiting"),
            AgentCall::Settings => ok(self.agent_settings_json()),
            AgentCall::SettingsSave {
                kind,
                base_url,
                model,
                clear_key,
                vision,
                screenshots,
            } => match self.agent_save_settings(
                &kind,
                &base_url,
                &model,
                secret,
                clear_key,
                vision.as_deref().and_then(VisionMode::parse),
                screenshots,
            ) {
                Ok(()) => ok(self.agent_settings_json()),
                Err(e) => err(&e),
            },
            AgentCall::Probe { kind, base_url } => {
                match self.agent_probe(&kind, &base_url, secret) {
                    Ok(id) => ok(json!({ "id": id })),
                    Err(e) => err(&e),
                }
            }
            AgentCall::ProbeResult { id } => {
                let r = self.agent.probes.lock().expect("probes").remove(&id);
                match r {
                    Some(v) => ok(json!({ "done": true, "result": v })),
                    None => ok(json!({ "done": false })),
                }
            }
            // Browser tasks belong to the Myth agent, which this build does not include.
            AgentCall::Tasks => ok(json!({ "tasks": [] })),
            AgentCall::TaskShot { .. } => err("no such tab"),
            AgentCall::HandoffShow { .. } => err("Myth is not waiting for a verification"),
            AgentCall::TaskTakeover { .. } => err("no such tab"),
        }
    }

    /// Stop a conversation's run (the Myth panel's Stop).
    pub(crate) fn agent_stop_conv(&self, conv: u64) {
        self.agent_stop(conv);
    }

    /// Whether a conversation has a run going.
    pub(crate) fn agent_conv_running(&self, conv: u64) -> bool {
        self.agent
            .convs
            .borrow()
            .get(&conv)
            .is_some_and(|c| c.run.is_some())
    }

    fn agent_stop(&self, conv: u64) {
        let mut convs = self.agent.convs.borrow_mut();
        let Some(c) = convs.get_mut(&conv) else {
            return;
        };
        if let Some(run) = c.run.take() {
            run.cancel.store(true, Ordering::Relaxed);
            c.events.push(json!({ "k": "end", "state": "stopped" }));
            c.status = "stopped";
            c.step = "Stopped".to_owned();
            info!("conversation {conv}: stopped by the user");
        }
    }

    /// Address-bar prompt on a web page: a chat about that page (its address and title; this
    /// build cannot act on it). The Myth pill shows the progress over the tab.
    pub(crate) fn agent_send_here(
        &self,
        user_tab: TabId,
        view: WebView,
        text: String,
        _attach_shot: bool,
    ) {
        let url = view.url().map(|u| u.to_string()).unwrap_or_default();
        let title = view.page_title().unwrap_or_default();
        let here = Some((user_tab, url, title));
        if let Err(e) = self.agent_send_inner(None, text, None, here, None) {
            warn!("Myth on this page: {e}");
        }
        self.mark_chrome_dirty();
    }

    fn agent_send(
        &self,
        conv: Option<u64>,
        text: String,
        today: Option<String>,
    ) -> Result<u64, String> {
        self.agent_send_inner(conv, text, today, None, None)
    }

    /// A message from the Myth panel (`assist_host`): `context` is the page context
    /// attachment (dropped after the run). `here` is the tab the panel belongs to.
    pub(crate) fn agent_send_panel(
        &self,
        conv: Option<u64>,
        text: String,
        here: Option<(TabId, WebView)>,
        _attach_shot: bool,
        panel: PanelSend,
    ) -> Result<u64, String> {
        let here = here.map(|(t, v)| {
            (
                t,
                v.url().map(|u| u.to_string()).unwrap_or_default(),
                v.page_title().unwrap_or_default(),
            )
        });
        self.agent_send_inner(conv, text, None, here, Some(panel))
    }

    fn agent_send_inner(
        &self,
        conv: Option<u64>,
        text: String,
        today: Option<String>,
        here: Option<(TabId, String, String)>,
        panel: Option<PanelSend>,
    ) -> Result<u64, String> {
        let provider_cfg = self.agent.effective_provider();
        let key = provider_cfg.resolve_key(self.agent.secrets.as_ref());
        let endpoint = Endpoint::from_config(&provider_cfg, key);

        let id = match conv {
            Some(id) if self.agent.convs.borrow().contains_key(&id) => id,
            Some(_) | None => {
                let id = self.agent.id();
                let title = title_from_message(&text);
                let now = unix_ms();
                let key = if self.agent.private {
                    None
                } else {
                    let key = new_chat_id();
                    match self
                        .store
                        .create_chat(&key, &title, &endpoint.model, now as i64)
                    {
                        Ok(()) => Some(key),
                        Err(e) => {
                            warn!("chat history: could not save the new chat: {e}");
                            None
                        }
                    }
                };
                self.agent.convs.borrow_mut().insert(
                    id,
                    Conversation {
                        key,
                        saved: 0,
                        next_seq: 0,
                        model: endpoint.model.clone(),
                        updated_ms: now,
                        title: clip(&title, 80),
                        messages: Vec::new(),
                        events: Vec::new(),
                        run: None,
                        last_run: 0,
                        status: "idle",
                        step: String::new(),
                        here: here.as_ref().map(|(t, _, _)| *t),
                        dismissed: false,
                    },
                );
                id
            }
        };
        let run_id = self.agent.id();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut history = {
            let mut convs = self.agent.convs.borrow_mut();
            let c = convs.get_mut(&id).expect("just ensured");
            if c.run.is_some() {
                return Err("Myth is still working on the last message. Stop it first.".into());
            }
            c.messages.push(ChatMessage::user(text.clone()));
            if let Some((t, _, _)) = &here {
                c.here = Some(*t);
                c.dismissed = false;
            }
            c.model = endpoint.model.clone();
            c.updated_ms = unix_ms();
            c.events.push(json!({ "k": "user", "text": text }));
            c.events.push(json!({
                "k": "start", "model": endpoint.model, "local": endpoint.is_local(),
            }));
            c.run = Some(Run {
                id: run_id,
                cancel: cancel.clone(),
            });
            c.last_run = run_id;
            c.status = "running";
            c.step = "Thinking".to_owned();
            let mut h = c.messages.clone();
            // The panel's page context goes right before the user's message, for this run
            // only (never in `c.messages`, so never saved).
            if let Some(ctx) = panel.as_ref().and_then(|p| p.context.clone()) {
                let at = h.len().saturating_sub(1);
                h.insert(at, ctx);
            }
            h
        };
        // The user's message is saved right away (a crash mid-run keeps it).
        self.agent_save(id);
        let link = Link {
            conv: id,
            run: run_id,
            tx: self.agent.tx.clone(),
            wake: self.agent.wake.clone(),
        };
        let today = today.or_else(|| panel.as_ref().map(|p| p.today.clone()));
        let mut system = mythbrowse_ai::system_prompt(today.as_deref().unwrap_or("unknown"));
        // Asked from the address bar on a page: the user may expect Myth to act on it.
        let wants_browser = panel.as_ref().map_or(here.is_some(), |p| p.tools);
        if let Some((user_tab, url, title)) = &here {
            system.push_str(&mythbrowse_ai::current_tab_prompt(user_tab.0, url, title));
        }
        info!(
            "conversation {id}: run {run_id} with model `{}` ({})",
            endpoint.model,
            if endpoint.is_local() {
                "local"
            } else {
                "remote"
            }
        );
        let spawned = std::thread::Builder::new()
            .name("myth-chat".into())
            .spawn(move || {
                let request = RunRequest {
                    endpoint: &endpoint,
                    system,
                    cancel: &cancel,
                    wants_browser,
                    max_steps: DEFAULT_MAX_STEPS,
                };
                let mut buf = String::new();
                let mut last_flush = Instant::now();
                let result = provider().run(&request, &mut history, &mut |ev| {
                    if let AgentEvent::Text(t) = &ev {
                        buf.push_str(t);
                        if last_flush.elapsed() >= TEXT_FLUSH {
                            link.event(json!({ "k": "text", "text": std::mem::take(&mut buf) }));
                            last_flush = Instant::now();
                        }
                        return;
                    }
                    if !buf.is_empty() {
                        link.event(json!({ "k": "text", "text": std::mem::take(&mut buf) }));
                        last_flush = Instant::now();
                    }
                    link.event(match ev {
                        AgentEvent::ToolStart { id, name, summary } => json!({
                            "k": "tool", "id": id, "name": name, "state": "running", "summary": summary,
                        }),
                        AgentEvent::ToolEnd { id, name, ok, denied, summary, tab } => json!({
                            "k": "tool", "id": id, "name": name, "summary": summary, "tab": tab,
                            "state": if denied { "denied" } else if ok { "done" } else { "error" },
                        }),
                        AgentEvent::Notice(t) => json!({ "k": "notice", "text": t }),
                        AgentEvent::Text(_) => unreachable!(),
                    });
                });
                if !buf.is_empty() {
                    link.event(json!({ "k": "text", "text": buf }));
                }
                let outcome = match result {
                    Ok(()) => Outcome::Done,
                    Err(AiError::Cancelled) => Outcome::Stopped,
                    Err(e) => Outcome::Failed {
                        hint: e.hint().map(str::to_owned),
                        text: e.to_string(),
                    },
                };
                link.send(HostMsg::Ended {
                    conv: link.conv,
                    run: link.run,
                    history,
                    outcome,
                });
            });
        if let Err(e) = spawned {
            self.agent_stop(id);
            return Err(format!("could not start the chat: {e}"));
        }
        Ok(id)
    }

    // ---------------------------------------------------------------- chat history

    /// Save the messages of `conv` not saved yet (no-op for unsaved chats).
    fn agent_save(&self, conv: u64) {
        let (key, first_seq, model, items) = {
            let mut convs = self.agent.convs.borrow_mut();
            let Some(c) = convs.get_mut(&conv) else {
                return;
            };
            let Some(key) = c.key.clone() else {
                return;
            };
            let from = c.saved.min(c.messages.len());
            let items: Vec<ChatMessage> = c.messages[from..].to_vec();
            let first = c.next_seq;
            c.saved = c.messages.len();
            c.next_seq += items.len() as i64;
            (key, first, c.model.clone(), items)
        };
        let now = mythbrowse_storage::unix_now_ms();
        let model = (!model.is_empty()).then_some(model.as_str());
        for (i, m) in items.iter().enumerate() {
            let p = to_saved(m);
            let content = if p.role == "tool" {
                summarize_tool_output(&p.content)
            } else {
                p.content
            };
            let stored = StoredMessage {
                seq: first_seq + i as i64,
                role: p.role.to_owned(),
                content,
                tool_calls: p.tool_calls,
                tool_call_id: p.tool_call_id,
                name: p.name,
                created_ms: now,
            };
            if let Err(e) = self.store.append_chat_message(&key, &stored, model) {
                warn!("chat history: could not save a message: {e}");
                return;
            }
        }
    }

    /// The live conversation for a chat id (`s<n>` or a saved id), if loaded.
    fn agent_live_conv(&self, id: &str) -> Option<u64> {
        let convs = self.agent.convs.borrow();
        if let Some(n) = id.strip_prefix('s') {
            let n: u64 = n.parse().ok()?;
            return convs.contains_key(&n).then_some(n);
        }
        convs
            .iter()
            .find(|(_, c)| c.key.as_deref() == Some(id))
            .map(|(n, _)| *n)
    }

    fn agent_chat_list(&self, query: &str, tz_offset_min: i32) -> Result<Value, String> {
        let now = mythbrowse_storage::unix_now_ms();
        let q = query.trim().to_lowercase();
        let mut items: Vec<(bool, i64, Value)> = Vec::new();
        {
            let convs = self.agent.convs.borrow();
            let live = |key: &str| {
                convs
                    .iter()
                    .find(|(_, c)| c.key.as_deref() == Some(key))
                    .map(|(n, c)| (*n, c.run.is_some()))
            };
            if !self.agent.private {
                for m in self.store.list_chats(&q, 300).map_err(|e| e.to_string())? {
                    let l = live(&m.id);
                    items.push((
                        m.pinned,
                        m.updated_ms,
                        json!({
                            "id": m.id, "title": m.title, "updated": m.updated_ms,
                            "pinned": m.pinned, "model": m.model, "saved": true,
                            "conv": l.map(|(n, _)| n), "running": l.is_some_and(|(_, r)| r),
                        }),
                    ));
                }
            }
            // Chats of this session that are not saved (private session).
            for (n, c) in convs.iter() {
                if c.key.is_some() || c.messages.is_empty() {
                    continue;
                }
                if !q.is_empty() && !c.title.to_lowercase().contains(&q) {
                    continue;
                }
                items.push((
                    false,
                    c.updated_ms as i64,
                    json!({
                        "id": format!("s{n}"), "title": c.title, "updated": c.updated_ms,
                        "pinned": false, "model": c.model, "saved": false,
                        "conv": n, "running": c.run.is_some(),
                    }),
                ));
            }
        }
        items.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut groups: Vec<(&str, Vec<Value>)> = Vec::new();
        for (pinned, updated, v) in items {
            let label = if pinned {
                "Pinned"
            } else {
                date_group(updated, now, tz_offset_min)
            };
            match groups.iter_mut().find(|(l, _)| *l == label) {
                Some((_, list)) => list.push(v),
                None => groups.push((label, vec![v])),
            }
        }
        let order = ["Pinned", "Today", "This week", "Earlier"];
        groups.sort_by_key(|(l, _)| order.iter().position(|o| o == l));
        Ok(json!({
            "private": self.agent.private,
            "groups": groups
                .into_iter()
                .map(|(label, items)| json!({ "label": label, "items": items }))
                .collect::<Vec<_>>(),
        }))
    }

    /// Load a saved chat (or find the live one) to show and continue it.
    fn agent_chat_open(&self, id: &str) -> Result<Value, String> {
        if let Some(n) = self.agent_live_conv(id) {
            let convs = self.agent.convs.borrow();
            let c = &convs[&n];
            return Ok(json!({ "conv": n, "id": c.key, "title": c.title }));
        }
        if id.starts_with('s') || self.agent.private {
            return Err("That chat is no longer open.".into());
        }
        let meta = self
            .store
            .chat(id)
            .map_err(|e| e.to_string())?
            .ok_or("That chat was deleted.")?;
        let stored = self.store.chat_messages(id).map_err(|e| e.to_string())?;
        let next_seq = stored.last().map_or(0, |m| m.seq + 1);
        let messages: Vec<ChatMessage> = stored
            .iter()
            .filter_map(|m| {
                from_saved(
                    &m.role,
                    &m.content,
                    m.tool_calls.as_deref(),
                    m.tool_call_id.as_deref(),
                    m.name.as_deref(),
                )
            })
            .collect();
        let local = self.agent.chat_endpoint().is_local();
        let context = resume_context(&messages, mythbrowse_ai::context_budget_chars(local));
        let mut events = vec![json!({
            "k": "history", "title": meta.title, "created": meta.created_ms,
            "updated": meta.updated_ms,
        })];
        events.extend(replay_events(&messages));
        info!(
            "chat history: reopened a chat ({} messages, {} in the model context)",
            messages.len(),
            context.len()
        );
        let n = self.agent.id();
        self.agent.convs.borrow_mut().insert(
            n,
            Conversation {
                key: Some(id.to_owned()),
                saved: context.len(),
                next_seq,
                model: meta.model.clone(),
                updated_ms: meta.updated_ms as u64,
                title: clip(&meta.title, 80),
                messages: context,
                events,
                run: None,
                last_run: 0,
                status: "done",
                step: String::new(),
                here: None,
                dismissed: true,
            },
        );
        Ok(json!({ "conv": n, "id": id, "title": meta.title }))
    }

    fn agent_chat_rename(&self, id: &str, title: &str) -> Result<(), String> {
        let saved = !id.starts_with('s');
        if saved
            && !self
                .store
                .rename_chat(id, title)
                .map_err(|e| e.to_string())?
        {
            return Err("That chat was deleted.".into());
        }
        let live = self.agent_live_conv(id);
        let renamed = live.is_some_and(|n| {
            self.agent
                .convs
                .borrow_mut()
                .get_mut(&n)
                .map(|c| c.title = clip(title, 80))
                .is_some()
        });
        if renamed || saved {
            Ok(())
        } else {
            Err("That chat is no longer open.".into())
        }
    }

    /// Stop a conversation and drop it from memory.
    fn agent_forget(&self, conv: u64) {
        self.agent_stop(conv);
        self.agent.convs.borrow_mut().remove(&conv);
    }

    fn agent_chat_delete(&self, id: &str) -> Result<(), String> {
        if let Some(n) = self.agent_live_conv(id) {
            self.agent_forget(n);
        }
        if !id.starts_with('s') {
            self.store.delete_chat(id).map_err(|e| e.to_string())?;
        }
        self.mark_chrome_dirty();
        Ok(())
    }

    /// Delete every chat: saved ones (here and on synced devices) and the ones in memory.
    pub(crate) fn agent_chat_clear(&self) -> Result<usize, String> {
        let live: Vec<u64> = self.agent.convs.borrow().keys().copied().collect();
        let mut n = live.len();
        for conv in live {
            self.agent_forget(conv);
        }
        if !self.agent.private {
            n = self.store.clear_chats().map_err(|e| e.to_string())?;
        }
        self.mark_chrome_dirty();
        Ok(n)
    }

    fn agent_chat_settings_json(&self) -> Value {
        json!({
            "private": self.agent.private,
            "retention": self
                .store
                .chat_retention()
                .map(|r| r.as_str())
                .unwrap_or_else(|_| "forever".into()),
            "sync": self.store.sync_chats_enabled().unwrap_or(true),
            "saved": self.store.chats_len().unwrap_or(0),
        })
    }

    // -------------------------------------------------------------------- settings

    fn agent_settings_json(&self) -> Value {
        let cfg = self.agent.config.borrow();
        let configured = cfg.is_some();
        let p = self.agent.effective_provider();
        let (has_key, key_from_env) = match &p {
            ProviderConfig::OpenaiCompatible { api_key_env, .. } => (
                p.resolve_key(self.agent.secrets.as_ref()).is_some(),
                api_key_env.is_some(),
            ),
            ProviderConfig::Ollama { .. } => (false, false),
        };
        json!({
            "configured": configured,
            "kind": kind_name(&p),
            "base_url": p.base_url(),
            "model": p.model(),
            "local": p.is_local(),
            "has_key": has_key,
            "key_from_env": key_from_env,
            "key_store": self.agent.secrets.backend(),
            "error": *self.agent.config_error.borrow(),
            "persist": self.agent.profile_dir.is_some(),
            "vision": cfg.as_ref().map(|c| c.vision.mode).unwrap_or_default().as_str(),
            "screenshots": cfg.as_ref().map(|c| c.vision.send_screenshots).unwrap_or(true),
            "agent": provider().capabilities().browser_control,
        })
    }

    fn agent_provider_from_form(
        &self,
        kind: &str,
        base_url: &str,
        model: &str,
    ) -> Result<ProviderConfig, String> {
        let base_url = base_url.trim().trim_end_matches('/').to_owned();
        let p = match kind {
            "ollama" => ProviderConfig::Ollama {
                base_url: if base_url.is_empty() {
                    mythbrowse_ai::config::DEFAULT_OLLAMA_URL.to_owned()
                } else {
                    base_url
                },
                model: model.trim().to_owned(),
                keep_alive: mythbrowse_ai::config::DEFAULT_OLLAMA_KEEP_ALIVE.to_owned(),
            },
            "openai" => ProviderConfig::OpenaiCompatible {
                base_url,
                model: model.trim().to_owned(),
                api_key_env: None,
                api_key_ref: Some(secret_name(MAIN_PROVIDER)),
            },
            other => return Err(format!("unknown provider kind `{other}`")),
        };
        let url = p.base_url();
        if !(url.starts_with("http://") || url.starts_with("https://")) || Url::parse(url).is_err()
        {
            return Err("The base URL must start with http:// or https://".into());
        }
        Ok(p)
    }

    #[allow(clippy::too_many_arguments)]
    fn agent_save_settings(
        &self,
        kind: &str,
        base_url: &str,
        model: &str,
        key: Option<Secret>,
        clear_key: bool,
        vision: Option<VisionMode>,
        screenshots: Option<bool>,
    ) -> Result<(), String> {
        let provider = self.agent_provider_from_form(kind, base_url, model)?;
        if provider.model().is_empty() {
            return Err("Choose a model.".into());
        }
        let name = secret_name(MAIN_PROVIDER);
        let store = self.agent.secrets.as_ref();
        if let Some(k) = key.filter(|k| !k.is_empty()) {
            store
                .set(&name, &k)
                .map_err(|e| format!("Could not store the API key: {e}"))?;
            info!("AI: API key saved ({})", store.backend());
        } else if clear_key || kind == "ollama" {
            store
                .delete(&name)
                .map_err(|e| format!("Could not remove the API key: {e}"))?;
        }
        let mut cfg = AiConfig::single(MAIN_PROVIDER, provider);
        // Vision / screenshot settings are kept unless the form changed them (the open build
        // never sends screenshots, but keeps the file compatible with official builds).
        cfg.vision = self.agent.vision_settings();
        if let Some(m) = vision {
            cfg.vision.mode = m;
        }
        if let Some(on) = screenshots {
            cfg.vision.send_screenshots = on;
        }
        if let Some(dir) = &self.agent.profile_dir {
            cfg.save(dir).map_err(|e| e.to_string())?;
        }
        let p = &cfg.providers[MAIN_PROVIDER];
        info!(
            "AI: settings saved: {} model `{}` at {}",
            kind_name(p),
            p.model(),
            p.base_url()
        );
        *self.agent.config.borrow_mut() = Some(cfg);
        *self.agent.config_error.borrow_mut() = None;
        Ok(())
    }

    fn agent_probe(&self, kind: &str, base_url: &str, key: Option<Secret>) -> Result<u64, String> {
        let provider = self.agent_provider_from_form(kind, base_url, "probe")?;
        let key = key.filter(|k| !k.is_empty()).or_else(|| {
            matches!(provider, ProviderConfig::OpenaiCompatible { .. })
                .then(|| {
                    self.agent
                        .effective_provider()
                        .resolve_key(self.agent.secrets.as_ref())
                })
                .flatten()
        });
        let endpoint = Endpoint::from_config(&provider, key);
        let id = self.agent.id();
        let tx = self.agent.tx.clone();
        let wake = self.agent.wake.clone();
        std::thread::Builder::new()
            .name("myth-probe".into())
            .spawn(move || {
                let result = match endpoint.list_models_detailed() {
                    Ok(models) => {
                        // `sizes[i]` belongs to `models[i]` (bytes, null when unknown).
                        let models: Vec<_> = models.into_iter().take(300).collect();
                        json!({
                            "ok": true,
                            "models": models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
                            "sizes": models.iter().map(|m| m.size).collect::<Vec<_>>(),
                        })
                    }
                    Err(e) => json!({ "ok": false, "error": e.to_string(), "hint": e.hint() }),
                };
                if tx.send(HostMsg::Probe { id, result }).is_ok() {
                    wake();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(id)
    }
}

/// `mythbrowse --keystore-selftest`: store, read and delete a dummy key in the platform store.
pub(crate) fn keystore_selftest() -> Result<String, String> {
    let dir = mythbrowse_storage::default_profile_dir().ok_or("no profile directory")?;
    let store = mythbrowse_ai::PlatformStore::new(&dir);
    self_test(&store)
}
