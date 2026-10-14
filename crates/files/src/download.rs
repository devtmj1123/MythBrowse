// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The download manager: MythBrowse fetches downloads itself (Servo 0.1.4 cannot hand one
//! to the embedder, see [`crate::detect`]).
//!
//! * One worker thread per running transfer (blocking `ureq`), bytes go to `name.ext.part`
//!   next to the final file, renamed when complete.
//! * Redirects are followed by hand (`max_redirects(0)`, at most 10) so the page's cookies go
//!   only to the host they were read for, and `Referer` follows
//!   `strict-origin-when-cross-origin` (full URL same-origin, origin cross-origin, nothing on
//!   an https -> http downgrade).
//! * **Dangerous types** ([`crate::danger`]): the transfer stops in [`State::Confirm`] after
//!   the response headers and before any byte is written; [`Manager::keep`] continues on the
//!   same connection (refetched once if it went stale), [`Manager::discard`] drops it.
//! * **Pause / resume**: pause keeps the `.part`; resume sends `Range: bytes=N-` with
//!   `If-Range` (ETag, else Last-Modified). `206` with the right start appends, `200` starts
//!   over, `416` with the whole file present completes, else starts over.
//! * **Cancel** deletes the `.part`. **Retry** resumes a failed transfer the same way.
//! * `data:` URLs are decoded in-process; `blob:` URLs cannot be read from outside Servo and
//!   fail with a plain message.
//! * Every worker carries a generation number: a pause / cancel / retry bumps it, and a stale
//!   worker (still blocked in a read) drops its results and never writes again.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{danger, names, now_ms};

/// A download's id (unique per profile; the shell seeds it past the stored history).
pub type Id = i64;

/// At most this many redirects are followed.
const MAX_REDIRECTS: usize = 10;
/// Progress events are sent at most this often per transfer.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// The worker checks for pause / cancel between reads of at most this size.
const CHUNK: usize = 64 * 1024;
/// The user agent when the shell does not pass Servo's.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) MythBrowse";

/// Where a download stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Connecting / waiting for the response headers.
    Pending,
    /// A type that can run code: waiting for Keep or Discard. Nothing written yet.
    Confirm,
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Confirm => "confirm",
            State::Running => "running",
            State::Paused => "paused",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "pending" => State::Pending,
            "confirm" => State::Confirm,
            "running" => State::Running,
            "paused" => State::Paused,
            "done" => State::Done,
            "failed" => State::Failed,
            "cancelled" => State::Cancelled,
            _ => return None,
        })
    }

    /// Still taking part in the transfer list's "in progress" count.
    pub fn is_active(self) -> bool {
        matches!(self, State::Pending | State::Confirm | State::Running)
    }

    /// Finished one way or another (nothing more will happen without the user).
    pub fn is_final(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }
}

/// One download, as shown on the Downloads page and kept in history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: Id,
    /// The URL the download was asked for (what Retry fetches again).
    pub url: String,
    /// Where the bytes came from after redirects ("" until known).
    #[serde(default)]
    pub final_url: String,
    /// File name (no folder).
    pub name: String,
    /// Full path of the finished file ("" until the name is known).
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub mime: String,
    /// Total size in bytes when the server said.
    pub total: Option<u64>,
    pub received: u64,
    pub state: State,
    /// Plain-words reason for `Failed`.
    #[serde(default)]
    pub error: String,
    /// A type that can run code (`.exe`, `.msi`, scripts…).
    #[serde(default)]
    pub danger: bool,
    /// The user chose Keep for a dangerous type.
    #[serde(default)]
    pub confirmed: bool,
    /// The server supports resuming (ranges, with a validator).
    #[serde(default)]
    pub resumable: bool,
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub last_modified: String,
    #[serde(default)]
    pub referrer: String,
    pub started_ms: i64,
    #[serde(default)]
    pub finished_ms: i64,
    /// Bytes per second, smoothed (0 when not running).
    #[serde(default)]
    pub speed: u64,
    /// Started from a private window: never written to history.
    #[serde(default)]
    pub private: bool,
    /// Started in a tab Myth was controlling (shown on the row).
    #[serde(default)]
    pub by_agent: bool,
}

impl Item {
    /// The danger warning for this item (empty when not dangerous).
    pub fn warning(&self) -> String {
        if self.danger {
            danger::warning(&self.name)
        } else {
            String::new()
        }
    }
}

/// What to download.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub url: String,
    /// `<a download="…">` name.
    pub suggested_name: Option<String>,
    /// The page the download was started from.
    pub referrer: Option<String>,
    /// `Cookie` header value for `url`'s host (sent to that host only).
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    /// Started by navigating to a URL that only *looked* like a download: if the server
    /// answers with an HTML page, give up with [`Event::NotADownload`] so the tab loads it.
    pub sniff_html: bool,
    pub private: bool,
    pub by_agent: bool,
}

/// What the manager tells the shell (from worker threads; the callback must be cheap).
#[derive(Debug, Clone)]
// One event per progress tick or state change, sent over a channel: boxing `Item` would only
// add an allocation per event.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    /// The item changed (state, progress, name…).
    Changed(Item),
    /// A `sniff_html` request turned out to be a web page: the item is gone; load `url` in
    /// the tab instead.
    NotADownload { id: Id, url: String },
}

type Callback = Arc<dyn Fn(Event) + Send + Sync>;

struct Entry {
    item: Item,
    request: Request,
    generation: u64,
    /// Serialises writes to the `.part` file between an old and a new worker.
    file_lock: Arc<Mutex<()>>,
    /// Set while a worker waits in `Confirm`: the user's answer.
    decision: Option<mpsc::Sender<bool>>,
}

/// Settings > Downloads > "Ask where to save each file": given the folder and the suggested
/// name, the path to save to, or `None` (the user cancelled). Runs on the transfer's worker
/// thread and may block on a dialog.
pub type AskPath = Arc<dyn Fn(&Path, &str) -> Option<PathBuf> + Send + Sync>;

/// The error text of a download whose "Save as" dialog was cancelled.
pub const NOT_SAVED: &str = "Not saved: the Save as dialog was closed. Retry to choose again.";

struct Inner {
    dir: PathBuf,
    ask: Option<AskPath>,
    entries: HashMap<Id, Entry>,
    order: Vec<Id>,
}

/// The transfer manager. Cheap to clone (shared state).
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Mutex<Inner>>,
    next_id: Arc<AtomicU64>,
    next_gen: Arc<AtomicU64>,
    on_event: Callback,
    agent: ureq::Agent,
    /// The same without any HTTP(S)_PROXY: a server on this machine is never reached through
    /// a proxy (as in `mythbrowse_ai::llm`).
    direct: ureq::Agent,
}

impl Manager {
    /// `dir`: where finished files go (created when needed). `on_event` runs on worker threads.
    pub fn new(
        dir: PathBuf,
        first_id: Id,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> Manager {
        let build = |proxy: bool| -> ureq::Agent {
            let mut cfg = ureq::Agent::config_builder();
            if !proxy {
                cfg = cfg.proxy(None);
            }
            cfg.http_status_as_error(false)
                .max_redirects(0)
                .timeout_connect(Some(Duration::from_secs(30)))
                .timeout_recv_response(Some(Duration::from_secs(120)))
                .user_agent(DEFAULT_USER_AGENT)
                .build()
                .into()
        };
        let (agent, direct) = (build(true), build(false));
        Manager {
            inner: Arc::new(Mutex::new(Inner {
                dir,
                ask: None,
                entries: HashMap::new(),
                order: Vec::new(),
            })),
            next_id: Arc::new(AtomicU64::new(first_id.max(1) as u64)),
            next_gen: Arc::new(AtomicU64::new(1)),
            on_event: Arc::new(on_event),
            agent,
            direct,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn dir(&self) -> PathBuf {
        self.lock().dir.clone()
    }

    /// Change the folder new downloads go to.
    pub fn set_dir(&self, dir: PathBuf) {
        self.lock().dir = dir;
    }

    /// Ask for a path for every new download (`Some`), or save straight to the folder (`None`).
    pub fn set_ask(&self, ask: Option<AskPath>) {
        self.lock().ask = ask;
    }

    pub fn asks(&self) -> bool {
        self.lock().ask.is_some()
    }

    /// Put history rows back (at startup). Transfers that were running are paused: the user
    /// resumes them from the Downloads page.
    pub fn restore(&self, items: Vec<Item>) {
        let mut g = self.lock();
        for mut item in items {
            if item.state.is_active() {
                item.state = State::Paused;
                item.speed = 0;
            }
            let id = item.id;
            self.next_id.fetch_max(id as u64 + 1, Ordering::SeqCst);
            if !g.entries.contains_key(&id) {
                g.order.push(id);
            }
            let request = Request {
                url: item.url.clone(),
                referrer: (!item.referrer.is_empty()).then(|| item.referrer.clone()),
                private: item.private,
                by_agent: item.by_agent,
                ..Request::default()
            };
            g.entries.insert(
                id,
                Entry {
                    item,
                    request,
                    generation: 0,
                    file_lock: Arc::default(),
                    decision: None,
                },
            );
        }
    }

    /// All items, newest first.
    pub fn list(&self) -> Vec<Item> {
        let g = self.lock();
        g.order
            .iter()
            .rev()
            .filter_map(|id| g.entries.get(id).map(|e| e.item.clone()))
            .collect()
    }

    pub fn get(&self, id: Id) -> Option<Item> {
        self.lock().entries.get(&id).map(|e| e.item.clone())
    }

    /// Number of downloads in progress (and their combined fraction when every size is known).
    pub fn progress(&self) -> (usize, Option<f64>) {
        let g = self.lock();
        let active: Vec<&Item> = g
            .entries
            .values()
            .map(|e| &e.item)
            .filter(|i| i.state.is_active())
            .collect();
        let (mut got, mut total, mut known) = (0u64, 0u64, true);
        for i in &active {
            got += i.received;
            match i.total {
                Some(t) if t > 0 => total += t,
                _ => known = false,
            }
        }
        let frac = (known && total > 0 && !active.is_empty()).then(|| got as f64 / total as f64);
        (active.len(), frac)
    }

    /// Whether any download is waiting for Keep / Discard.
    pub fn needs_attention(&self) -> bool {
        self.lock()
            .entries
            .values()
            .any(|e| e.item.state == State::Confirm)
    }

    /// Start a download. Returns its id.
    pub fn start(&self, request: Request) -> Id {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as Id;
        let guess = Url::parse(&request.url).ok();
        let name = names::choose_name(
            None,
            request.suggested_name.as_deref(),
            guess.as_ref(),
            None,
        );
        let item = Item {
            id,
            url: request.url.clone(),
            final_url: String::new(),
            name,
            path: String::new(),
            mime: String::new(),
            total: None,
            received: 0,
            state: State::Pending,
            error: String::new(),
            danger: false,
            confirmed: false,
            resumable: false,
            etag: String::new(),
            last_modified: String::new(),
            referrer: request.referrer.clone().unwrap_or_default(),
            started_ms: now_ms(),
            finished_ms: 0,
            speed: 0,
            private: request.private,
            by_agent: request.by_agent,
        };
        {
            let mut g = self.lock();
            g.order.push(id);
            g.entries.insert(
                id,
                Entry {
                    item: item.clone(),
                    request,
                    generation: 0,
                    file_lock: Arc::default(),
                    decision: None,
                },
            );
        }
        self.emit(Event::Changed(item));
        self.spawn(id);
        id
    }

    /// Start `request` unless the same URL is already being fetched, or was started less
    /// than `within` ago (`None`: nothing new was fetched). The shell sees one file from
    /// several places (the navigation hook, the main-frame load hook, Servo's "unknown
    /// content type" page, `<a download>`); each of them goes through here, so a file is
    /// fetched once.
    pub fn start_once(&self, request: Request, within: Duration) -> Option<Id> {
        let now = now_ms();
        let within_ms = within.as_millis() as i64;
        let busy = self.lock().entries.values().any(|e| {
            e.item.url == request.url
                && (matches!(e.item.state, State::Running | State::Pending)
                    || now.saturating_sub(e.item.started_ms) < within_ms)
        });
        if busy {
            log::info!(
                "download of {} already started; not fetched again",
                request.url
            );
            return None;
        }
        Some(self.start(request))
    }

    /// Pause a running (or pending) transfer; the `.part` stays.
    pub fn pause(&self, id: Id) -> Result<(), String> {
        let item = {
            let mut g = self.lock();
            let e = g.entries.get_mut(&id).ok_or("No such download.")?;
            if !matches!(e.item.state, State::Running | State::Pending) {
                return Err("This download is not running.".into());
            }
            e.generation = self.next_gen.fetch_add(1, Ordering::SeqCst);
            e.item.state = State::Paused;
            e.item.speed = 0;
            e.item.clone()
        };
        self.emit(Event::Changed(item));
        Ok(())
    }

    /// Resume a paused transfer, or retry a failed / cancelled one.
    pub fn resume(&self, id: Id) -> Result<(), String> {
        let item = {
            let mut g = self.lock();
            let e = g.entries.get_mut(&id).ok_or("No such download.")?;
            match e.item.state {
                State::Paused | State::Failed | State::Cancelled => {}
                _ => return Err("This download cannot be resumed now.".into()),
            }
            e.item.state = State::Pending;
            e.item.error.clear();
            e.item.finished_ms = 0;
            e.item.clone()
        };
        self.emit(Event::Changed(item));
        self.spawn(id);
        Ok(())
    }

    /// Same as [`Manager::resume`] (named for the UI's Retry).
    pub fn retry(&self, id: Id) -> Result<(), String> {
        self.resume(id)
    }

    /// Cancel: stops the transfer and deletes the `.part`.
    pub fn cancel(&self, id: Id) -> Result<(), String> {
        let (item, lock) = {
            let mut g = self.lock();
            let e = g.entries.get_mut(&id).ok_or("No such download.")?;
            if e.item.state == State::Done {
                return Err("This download has finished.".into());
            }
            e.generation = self.next_gen.fetch_add(1, Ordering::SeqCst);
            if let Some(tx) = e.decision.take() {
                let _ = tx.send(false);
            }
            e.item.state = State::Cancelled;
            e.item.speed = 0;
            e.item.finished_ms = now_ms();
            (e.item.clone(), e.file_lock.clone())
        };
        if !item.path.is_empty() {
            let _l = lock.lock().unwrap_or_else(|p| p.into_inner());
            let _ = std::fs::remove_file(names::part_path(Path::new(&item.path)));
        }
        self.emit(Event::Changed(item));
        Ok(())
    }

    /// Keep a dangerous download: writing starts.
    pub fn keep(&self, id: Id) -> Result<(), String> {
        let mut g = self.lock();
        let e = g.entries.get_mut(&id).ok_or("No such download.")?;
        if e.item.state != State::Confirm {
            return Err("This download is not waiting for you.".into());
        }
        e.item.confirmed = true;
        match e.decision.take() {
            Some(tx) => {
                let _ = tx.send(true);
                Ok(())
            }
            None => {
                // Restored from history in Confirm: fetch again.
                e.item.state = State::Pending;
                let item = e.item.clone();
                drop(g);
                self.emit(Event::Changed(item));
                self.spawn(id);
                Ok(())
            }
        }
    }

    /// Discard a dangerous download: nothing was written; it ends as cancelled.
    pub fn discard(&self, id: Id) -> Result<(), String> {
        {
            let g = self.lock();
            let e = g.entries.get(&id).ok_or("No such download.")?;
            if e.item.state != State::Confirm {
                return Err("This download is not waiting for you.".into());
            }
        }
        self.cancel(id)
    }

    /// Forget a finished item (the file stays). Running items are cancelled first.
    pub fn remove(&self, id: Id) -> Option<Item> {
        let active = self
            .get(id)
            .map(|i| !i.state.is_final() && i.state != State::Paused);
        if active == Some(true) {
            let _ = self.cancel(id);
        }
        let mut g = self.lock();
        g.order.retain(|x| *x != id);
        let e = g.entries.remove(&id)?;
        if e.item.state == State::Paused && !e.item.path.is_empty() {
            let _ = std::fs::remove_file(names::part_path(Path::new(&e.item.path)));
        }
        Some(e.item)
    }

    /// Forget every finished / failed / cancelled item. Returns their ids.
    pub fn clear_finished(&self) -> Vec<Id> {
        let mut g = self.lock();
        let gone: Vec<Id> = g
            .entries
            .iter()
            .filter(|(_, e)| e.item.state.is_final())
            .map(|(id, _)| *id)
            .collect();
        for id in &gone {
            g.entries.remove(id);
        }
        g.order.retain(|id| !gone.contains(id));
        gone
    }

    /// Paths running transfers hold (so two downloads never pick the same name).
    fn reserved(g: &Inner, except: Id) -> Vec<PathBuf> {
        g.entries
            .iter()
            .filter(|(id, e)| **id != except && !e.item.path.is_empty() && !e.item.state.is_final())
            .map(|(_, e)| PathBuf::from(&e.item.path))
            .collect()
    }

    fn emit(&self, event: Event) {
        (self.on_event)(event);
    }

    fn spawn(&self, id: Id) {
        let generation = self.next_gen.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.lock().entries.get_mut(&id) {
            e.generation = generation;
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("mb-download-{id}"))
            .spawn(move || me.run(id, generation));
        if let Err(err) = spawned {
            self.fail(
                id,
                generation,
                &format!("Could not start the download ({err})."),
            );
        }
    }

    /// Apply `f` to the item if `generation` is still current; emit the change.
    fn update(&self, id: Id, generation: u64, f: impl FnOnce(&mut Item)) -> bool {
        let item = {
            let mut g = self.lock();
            match g.entries.get_mut(&id) {
                Some(e) if e.generation == generation => {
                    f(&mut e.item);
                    e.item.clone()
                }
                _ => return false,
            }
        };
        self.emit(Event::Changed(item));
        true
    }

    fn current(&self, id: Id, generation: u64) -> bool {
        self.lock()
            .entries
            .get(&id)
            .is_some_and(|e| e.generation == generation)
    }

    fn fail(&self, id: Id, generation: u64, msg: &str) {
        self.update(id, generation, |i| {
            i.state = State::Failed;
            i.error = msg.to_owned();
            i.speed = 0;
            i.finished_ms = now_ms();
        });
    }

    fn run(&self, id: Id, generation: u64) {
        let Some((request, item, lock)) = self
            .lock()
            .entries
            .get(&id)
            .map(|e| (e.request.clone(), e.item.clone(), e.file_lock.clone()))
        else {
            return;
        };
        let result = if item.url.len() >= 5 && item.url[..5].eq_ignore_ascii_case("data:") {
            self.run_data(id, generation, &request, &lock)
        } else if item.url.len() >= 5 && item.url[..5].eq_ignore_ascii_case("blob:") {
            Err("This file was made by the page itself (a blob: link), which MythBrowse cannot save yet.".into())
        } else {
            self.run_http(id, generation, &request, &lock)
        };
        if let Err(msg) = result {
            self.fail(id, generation, &msg);
        }
    }

    /// Resolve name/path for the first response (or reuse the stored ones on resume).
    fn place(&self, id: Id, name: &str) -> Result<PathBuf, String> {
        let g = self.lock();
        let dir = g.dir.clone();
        let ask = g.ask.clone();
        let reserved = Self::reserved(&g, id);
        drop(g);
        if let Some(ask) = ask {
            // The suggested name is the one the folder would get, so the dialog never proposes
            // overwriting an existing file; the user may still choose one (the dialog asks).
            let _ = std::fs::create_dir_all(&dir);
            let suggested = names::unique_path(&dir, name, &reserved);
            let suggested = suggested
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| name.to_owned());
            let chosen = ask(&dir, &suggested).ok_or_else(|| NOT_SAVED.to_owned())?;
            if chosen
                .parent()
                .is_some_and(|p| !p.as_os_str().is_empty() && !p.is_dir())
            {
                return Err("The folder chosen in Save as does not exist.".into());
            }
            return Ok(chosen);
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Could not create the download folder ({e})."))?;
        Ok(names::unique_path(&dir, name, &reserved))
    }

    /// Wait for Keep / Discard on a dangerous type. `Ok(true)` = keep.
    fn confirm(&self, id: Id, generation: u64) -> bool {
        let (tx, rx) = mpsc::channel();
        let item = {
            let mut g = self.lock();
            match g.entries.get_mut(&id) {
                Some(e) if e.generation == generation => {
                    e.item.state = State::Confirm;
                    e.item.speed = 0;
                    e.decision = Some(tx);
                    e.item.clone()
                }
                _ => return false,
            }
        };
        self.emit(Event::Changed(item));
        let keep = rx.recv().unwrap_or(false);
        keep && self.update(id, generation, |i| i.state = State::Running)
    }

    fn run_data(
        &self,
        id: Id,
        generation: u64,
        request: &Request,
        lock: &Mutex<()>,
    ) -> Result<(), String> {
        let (mime, bytes) = decode_data_url(&request.url).ok_or("This data: link is not valid.")?;
        let name = names::choose_name(None, request.suggested_name.as_deref(), None, Some(&mime));
        let confirmed = self.get(id).is_some_and(|i| i.confirmed);
        let dangerous = danger::is_dangerous(&name, Some(&mime));
        let path = self.place(id, &name)?;
        let named = self.update(id, generation, |i| {
            i.name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(name.clone());
            i.path = path.to_string_lossy().into_owned();
            i.mime = mime.clone();
            i.total = Some(bytes.len() as u64);
            i.danger = dangerous;
            i.state = State::Running;
        });
        if !named || (dangerous && !confirmed && !self.confirm(id, generation)) {
            return Ok(());
        }
        let _l = lock.lock().unwrap_or_else(|p| p.into_inner());
        if !self.current(id, generation) {
            return Ok(());
        }
        let part = names::part_path(&path);
        std::fs::write(&part, &bytes).map_err(|e| write_error(&e))?;
        self.finish(id, generation, &path, bytes.len() as u64)
    }

    fn run_http(
        &self,
        id: Id,
        generation: u64,
        request: &Request,
        lock: &Mutex<()>,
    ) -> Result<(), String> {
        let mut refetched_after_confirm = false;
        let mut force_full = false;
        loop {
            // How much is already on disk (resume).
            let cur = self.get(id).ok_or("gone")?;
            let existing = if !force_full && !cur.path.is_empty() {
                std::fs::metadata(names::part_path(Path::new(&cur.path)))
                    .map(|m| m.len())
                    .unwrap_or(0)
            } else {
                0
            };
            let validator = if !cur.etag.is_empty() && !cur.etag.starts_with("W/") {
                Some(cur.etag.clone())
            } else if !cur.last_modified.is_empty() {
                Some(cur.last_modified.clone())
            } else {
                None
            };
            let range = (existing > 0 && cur.resumable).then_some(existing);
            let fetched = self.fetch(request, range.map(|r| (r, validator.clone())))?;
            let Fetched {
                final_url,
                status,
                headers,
                body,
            } = fetched;
            if !self.current(id, generation) {
                return Ok(());
            }
            if status == 416 && range.is_some() {
                // The server says our range is past the end: either we already have it all…
                if cur.total == Some(existing) {
                    let path = PathBuf::from(&cur.path);
                    let _l = lock.lock().unwrap_or_else(|p| p.into_inner());
                    return self.finish(id, generation, &path, existing);
                }
                force_full = true;
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(http_error(status));
            }
            let ct = headers.get("content-type").cloned();
            let disposition = headers.get("content-disposition").cloned();
            let length = headers
                .get("content-length")
                .and_then(|v| v.trim().parse::<u64>().ok());
            let etag = headers.get("etag").cloned().unwrap_or_default();
            let last_modified = headers.get("last-modified").cloned().unwrap_or_default();
            let accepts_ranges = headers
                .get("accept-ranges")
                .is_some_and(|v| v.to_ascii_lowercase().contains("bytes"));

            // Appending to the .part, or starting at byte 0?
            let mut offset = 0u64;
            let mut total = length;
            if status == 206 {
                let cr = headers
                    .get("content-range")
                    .and_then(|v| parse_content_range(v));
                match (cr, range) {
                    (Some((start, _end, full)), Some(want)) if start == want => {
                        offset = start;
                        total = full.or(length.map(|l| l + start));
                    }
                    _ => {
                        // A range we did not ask for: start over without one.
                        force_full = true;
                        continue;
                    }
                }
            }

            let first = offset == 0 && cur.path.is_empty();
            if first
                && request.sniff_html
                && disposition
                    .as_deref()
                    .is_none_or(|d| !names::is_attachment(d))
                && ct.as_deref().is_some_and(crate::detect::is_renderable)
            {
                // A page after all (a login wall, "your download starts soon"…).
                let gone = {
                    let mut g = self.lock();
                    let current = g
                        .entries
                        .get(&id)
                        .is_some_and(|e| e.generation == generation);
                    if current {
                        g.entries.remove(&id);
                        g.order.retain(|x| *x != id);
                    }
                    current
                };
                if gone {
                    self.emit(Event::NotADownload {
                        id,
                        url: request.url.clone(),
                    });
                }
                return Ok(());
            }

            let mime = ct
                .as_deref()
                .map(|c| {
                    c.split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase()
                })
                .unwrap_or_default();
            let path = if cur.path.is_empty() {
                let parsed = Url::parse(&final_url).ok();
                let name = names::choose_name(
                    disposition.as_deref(),
                    request.suggested_name.as_deref(),
                    parsed.as_ref(),
                    (!mime.is_empty()).then_some(mime.as_str()),
                );
                self.place(id, &name)?
            } else {
                PathBuf::from(&cur.path)
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dangerous =
                danger::is_dangerous(&name, (!mime.is_empty()).then_some(mime.as_str()));
            let ok = self.update(id, generation, |i| {
                i.final_url = final_url.clone();
                i.name = name.clone();
                i.path = path.to_string_lossy().into_owned();
                if !mime.is_empty() {
                    i.mime = mime.clone();
                }
                i.total = total;
                i.received = offset;
                i.danger = dangerous;
                i.etag = etag.clone();
                i.last_modified = last_modified.clone();
                i.resumable = (accepts_ranges || status == 206)
                    && (!etag.is_empty() || !last_modified.is_empty() || status == 206);
                i.state = State::Running;
            });
            if !ok {
                return Ok(());
            }
            let mut body = body;
            if dangerous && !self.get(id).is_some_and(|i| i.confirmed) {
                if !self.confirm(id, generation) {
                    return Ok(());
                }
                // The connection may have gone stale while the user decided: if the first read
                // fails, fetch once more.
                let mut probe = vec![0u8; CHUNK];
                match body.read(&mut probe) {
                    Ok(n) => {
                        return self.write_body(
                            id,
                            generation,
                            lock,
                            &path,
                            offset,
                            total,
                            Some(probe[..n].to_vec()),
                            body,
                        );
                    }
                    Err(_) if !refetched_after_confirm => {
                        refetched_after_confirm = true;
                        force_full = true;
                        continue;
                    }
                    Err(e) => return Err(net_error(&e)),
                }
            }
            return self.write_body(id, generation, lock, &path, offset, total, None, body);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_body(
        &self,
        id: Id,
        generation: u64,
        lock: &Mutex<()>,
        path: &Path,
        offset: u64,
        total: Option<u64>,
        first: Option<Vec<u8>>,
        mut body: Box<dyn Read + Send>,
    ) -> Result<(), String> {
        let part = names::part_path(path);
        let mut file = {
            let _l = lock.lock().unwrap_or_else(|p| p.into_inner());
            if !self.current(id, generation) {
                return Ok(());
            }
            open_part(&part, offset)?
        };
        let mut received = offset;
        let mut last_emit = Instant::now();
        let mut window_start = Instant::now();
        let mut window_bytes = 0u64;
        let mut speed = 0u64;
        let mut buf = vec![0u8; CHUNK];
        let mut pending = first;
        loop {
            let n = match pending.take() {
                Some(bytes) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    bytes.len()
                }
                None => match body.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = file.flush();
                        return Err(net_error(&e));
                    }
                },
            };
            if n == 0 {
                break;
            }
            {
                let _l = lock.lock().unwrap_or_else(|p| p.into_inner());
                if !self.current(id, generation) {
                    return Ok(());
                }
                file.write_all(&buf[..n]).map_err(|e| write_error(&e))?;
            }
            received += n as u64;
            window_bytes += n as u64;
            let el = window_start.elapsed();
            if el >= Duration::from_millis(500) {
                let now = (window_bytes as f64 / el.as_secs_f64()) as u64;
                speed = if speed == 0 {
                    now
                } else {
                    (speed * 2 + now) / 3
                };
                window_start = Instant::now();
                window_bytes = 0;
            }
            if last_emit.elapsed() >= PROGRESS_EVERY {
                last_emit = Instant::now();
                if !self.update(id, generation, |i| {
                    i.received = received;
                    i.speed = speed;
                }) {
                    return Ok(());
                }
            }
        }
        file.flush().map_err(|e| write_error(&e))?;
        drop(file);
        if let Some(t) = total
            && received < t
        {
            self.update(id, generation, |i| i.received = received);
            return Err(
                "The connection closed before the whole file arrived. Retry to continue.".into(),
            );
        }
        let _l = lock.lock().unwrap_or_else(|p| p.into_inner());
        self.finish(id, generation, path, received)
    }

    /// Rename `.part` to the final name (a fresh unique name if that appeared meanwhile).
    fn finish(&self, id: Id, generation: u64, path: &Path, received: u64) -> Result<(), String> {
        if !self.current(id, generation) {
            return Ok(());
        }
        let part = names::part_path(path);
        let mut target = path.to_path_buf();
        if target.exists() {
            let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
            let name = target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            target = names::unique_path(&dir, &name, &[]);
        }
        rename_with_retry(&part, &target)
            .map_err(|e| format!("Could not finish saving the file ({e})."))?;
        self.update(id, generation, |i| {
            i.path = target.to_string_lossy().into_owned();
            i.name = target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            i.received = received;
            if i.total.is_none() {
                i.total = Some(received);
            }
            i.state = State::Done;
            i.speed = 0;
            i.finished_ms = now_ms();
        });
        Ok(())
    }

    /// GET with manual redirects. `range`: (first byte, If-Range validator).
    fn fetch(
        &self,
        request: &Request,
        range: Option<(u64, Option<String>)>,
    ) -> Result<Fetched, String> {
        let original =
            Url::parse(&request.url).map_err(|_| "This link is not a valid address.".to_owned())?;
        if !matches!(original.scheme(), "http" | "https") {
            return Err("MythBrowse can only download http and https links.".into());
        }
        let referrer = request.referrer.as_deref().and_then(|r| Url::parse(r).ok());
        let mut url = original.clone();
        for _ in 0..=MAX_REDIRECTS {
            let agent = if is_loopback(&url) {
                &self.direct
            } else {
                &self.agent
            };
            let mut req = agent.get(url.as_str());
            if let Some(ua) = request.user_agent.as_deref().filter(|s| !s.is_empty()) {
                req = req.header("User-Agent", ua);
            }
            if let Some(r) = referrer.as_ref().and_then(|r| referrer_for(r, &url)) {
                req = req.header("Referer", r.as_str());
            }
            if let Some(c) = request.cookies.as_deref().filter(|c| !c.is_empty())
                && same_host(&url, &original)
            {
                req = req.header("Cookie", c);
            }
            if let Some((start, validator)) = &range {
                req = req.header("Range", format!("bytes={start}-").as_str());
                if let Some(v) = validator {
                    req = req.header("If-Range", v.as_str());
                }
            }
            let resp = req.call().map_err(|e| transport_error(&e))?;
            let status = resp.status().as_u16();
            let mut headers = HashMap::new();
            for (k, v) in resp.headers() {
                if let Ok(v) = v.to_str() {
                    headers.insert(k.as_str().to_ascii_lowercase(), v.to_owned());
                }
            }
            if (300..400).contains(&status) && status != 304 {
                let loc = headers
                    .get("location")
                    .ok_or("The server redirected without saying where.")?;
                let next = url
                    .join(loc)
                    .map_err(|_| "The server redirected to an invalid address.".to_owned())?;
                if !matches!(next.scheme(), "http" | "https") {
                    return Err(
                        "The server redirected to an address MythBrowse cannot download from."
                            .into(),
                    );
                }
                url = next;
                continue;
            }
            let body: Box<dyn Read + Send> = Box::new(resp.into_body().into_reader());
            return Ok(Fetched {
                final_url: url.to_string(),
                status,
                headers,
                body,
            });
        }
        Err("Too many redirects.".into())
    }
}

struct Fetched {
    final_url: String,
    status: u16,
    headers: HashMap<String, String>,
    body: Box<dyn Read + Send>,
}

fn is_loopback(u: &Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn same_host(a: &Url, b: &Url) -> bool {
    a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
}

/// `Referer` under `strict-origin-when-cross-origin`.
pub fn referrer_for(referrer: &Url, target: &Url) -> Option<String> {
    if !matches!(referrer.scheme(), "http" | "https") {
        return None;
    }
    if referrer.scheme() == "https" && target.scheme() == "http" {
        return None;
    }
    if referrer.origin() == target.origin() {
        let mut r = referrer.clone();
        r.set_fragment(None);
        let _ = r.set_username("");
        let _ = r.set_password(None);
        Some(r.to_string())
    } else {
        Some(format!("{}/", referrer.origin().ascii_serialization()))
    }
}

/// `bytes 100-199/1000` -> (100, 199, Some(1000)); `/*` -> None total.
pub fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, total))
}

/// `data:[mime][;base64],payload` -> (mime, bytes).
pub fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.get(5..)?;
    let (meta, payload) = rest.split_once(',')?;
    let (meta, base64) = match meta.strip_suffix(";base64") {
        Some(m) => (m, true),
        None => (meta, meta.eq_ignore_ascii_case("base64")),
    };
    let mime = meta
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mime = if mime.is_empty() {
        "text/plain".to_owned()
    } else {
        mime
    };
    let raw = names::percent_decode(payload);
    let bytes = if base64 { base64_decode(&raw)? } else { raw };
    Some((mime, bytes))
}

fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let clean: Vec<u8> = input
        .iter()
        .copied()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
        if chunk.len() == 1 {
            return None;
        }
    }
    Some(out)
}

fn open_part(part: &Path, offset: u64) -> Result<File, String> {
    if offset > 0 {
        let f = OpenOptions::new()
            .append(true)
            .open(part)
            .map_err(|e| write_error(&e))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len != offset {
            f.set_len(offset).map_err(|e| write_error(&e))?;
        }
        Ok(f)
    } else {
        File::create(part).map_err(|e| write_error(&e))
    }
}

/// Windows: an antivirus scanner can hold a just-written file for a moment.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for attempt in 0..10 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(e),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

fn http_error(status: u16) -> String {
    match status {
        401 | 403 => format!(
            "The server refused the download ({status}). You may need to sign in on the site."
        ),
        404 | 410 => format!("The file is not on the server any more ({status})."),
        429 => "The server is busy (429). Retry in a moment.".into(),
        500..=599 => format!("The server had a problem ({status}). Retry later."),
        _ => format!("The server answered {status} instead of the file."),
    }
}

fn transport_error(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "The server took too long to answer.".into(),
        ureq::Error::HostNotFound => "That server could not be found.".into(),
        ureq::Error::ConnectionFailed => "Could not connect to the server.".into(),
        ureq::Error::Io(io) => net_error(io),
        other => format!("The download failed ({other})."),
    }
}

fn net_error(e: &std::io::Error) -> String {
    format!(
        "The network connection was lost ({}). Retry to continue.",
        e.kind()
    )
}

fn write_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            "MythBrowse is not allowed to write to the download folder.".into()
        }
        std::io::ErrorKind::StorageFull => "The disk is full.".into(),
        _ => format!("Could not write the file ({e})."),
    }
}

#[cfg(test)]
mod tests;
