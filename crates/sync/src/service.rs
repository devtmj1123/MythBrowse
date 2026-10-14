// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! The sync service the browser runs: a background thread that owns the server connection and
//! its own handle on the profile, takes [`Command`]s and publishes a [`Status`] the UI polls.
//! Network calls never run on the browser's UI thread.
//!
//! Flow: sign up / sign in -> (first device) set a sync passphrase, shown a recovery code once
//! -> or (other devices) unlock with the passphrase or the recovery code -> sync now and every
//! [`INTERVAL`].

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::appwrite::{Appwrite, AppwriteError, Session};
use crate::crypto::{CryptoError, KdfParams, SyncKey, WrappedKey};
use crate::engine::{LocalStore, Remote, SyncReport, sync};

pub const INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Secret-store names.
pub const SESSION_SECRET: &str = "sync_session";
pub const KEY_SECRET: &str = "sync_key";

/// Where the device keeps the session and the sync key (the OS secret store in the browser).
pub trait KeyStore: Send {
    fn get(&self, name: &str) -> Option<Zeroizing<String>>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    fn delete(&self, name: &str);
}

/// The server operations the service needs ([`Appwrite`] in the browser, a fake in tests).
pub trait Account: Remote + Send {
    fn create_account(&mut self, email: &str, password: &str) -> Result<(), AppwriteError>;
    /// Returns the user id.
    fn sign_in(&mut self, email: &str, password: &str) -> Result<String, AppwriteError>;
    fn restore(&mut self, user_id: &str, secret: &str);
    /// (user id, session secret) to persist.
    fn session(&self) -> Option<(String, String)>;
    fn sign_out(&mut self) -> Result<(), AppwriteError>;
    fn wrapped_key(&self) -> Result<Option<WrappedKey>, AppwriteError>;
    fn set_wrapped_key(&self, w: &WrappedKey) -> Result<(), AppwriteError>;
    /// Is the session still valid on the server? `Err` with [`AppwriteError::is_auth`] when it
    /// was revoked or expired. Needed because Appwrite answers a row listing with a dead
    /// session as a guest (200, no rows) instead of 401.
    fn check_session(&self) -> Result<(), AppwriteError>;
}

impl Account for Appwrite {
    fn create_account(&mut self, email: &str, password: &str) -> Result<(), AppwriteError> {
        let name = email.split('@').next().unwrap_or("");
        Appwrite::create_account(self, email, password, name).map(|_| ())
    }
    fn sign_in(&mut self, email: &str, password: &str) -> Result<String, AppwriteError> {
        Appwrite::sign_in(self, email, password).map(|s| s.user_id.clone())
    }
    fn restore(&mut self, user_id: &str, secret: &str) {
        self.set_session(Session::new(user_id.into(), secret.into()));
    }
    fn session(&self) -> Option<(String, String)> {
        Appwrite::session(self).map(|s| (s.user_id.clone(), s.secret().to_owned()))
    }
    fn sign_out(&mut self) -> Result<(), AppwriteError> {
        Appwrite::sign_out(self)
    }
    fn wrapped_key(&self) -> Result<Option<WrappedKey>, AppwriteError> {
        Appwrite::wrapped_key(self)
    }
    fn set_wrapped_key(&self, w: &WrappedKey) -> Result<(), AppwriteError> {
        Appwrite::set_wrapped_key(self, w)
    }
    fn check_session(&self) -> Result<(), AppwriteError> {
        Appwrite::account(self).map(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    SignedOut,
    /// Signed in, this account has no sync key yet: choose a sync passphrase.
    NeedsSetup,
    /// Signed in, the account has a key: unlock it with the passphrase or recovery code.
    NeedsUnlock,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub email: Option<String>,
    pub busy: bool,
    /// Last problem, in words for the user.
    pub error: Option<String>,
    pub last_sync_ms: Option<i64>,
    pub last: Option<Report>,
    /// Shown once after setup or recovery; cleared by [`Command::AckRecovery`].
    pub recovery_code: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub pulled: usize,
    pub applied: usize,
    pub pushed: usize,
    pub unreadable: usize,
}

impl From<SyncReport> for Report {
    fn from(r: SyncReport) -> Self {
        Report {
            pulled: r.pulled,
            applied: r.applied,
            pushed: r.pushed,
            unreadable: r.unreadable,
        }
    }
}

pub enum Command {
    SignUp {
        email: String,
        password: Zeroizing<String>,
    },
    SignIn {
        email: String,
        password: Zeroizing<String>,
    },
    SetUp {
        passphrase: Zeroizing<String>,
    },
    Unlock {
        passphrase: Zeroizing<String>,
    },
    /// Unlock with the recovery code and set a new passphrase.
    Recover {
        code: Zeroizing<String>,
        passphrase: Zeroizing<String>,
    },
    SyncNow,
    AckRecovery,
    SignOut,
    Shutdown,
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    user: String,
    secret: String,
    email: String,
}

/// Handle owned by the browser.
pub struct SyncService {
    tx: Sender<Command>,
    status: Arc<Mutex<Status>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SyncService {
    /// `open_store` runs on the worker thread (an SQLite connection stays on one thread).
    /// `on_change` is called after every status change (wake the UI) and after a sync that
    /// changed the profile (`true`: refresh bookmarks UI).
    pub fn spawn<A, S>(
        account: A,
        open_store: impl FnOnce() -> Result<S, String> + Send + 'static,
        keys: Box<dyn KeyStore>,
        kdf: KdfParams,
        on_change: Arc<dyn Fn(bool) + Send + Sync>,
    ) -> SyncService
    where
        A: Account + 'static,
        S: LocalStore + 'static,
    {
        let status = Arc::new(Mutex::new(Status {
            phase: Phase::SignedOut,
            email: None,
            busy: true,
            error: None,
            last_sync_ms: None,
            last: None,
            recovery_code: None,
        }));
        let (tx, rx) = channel();
        let st = status.clone();
        let thread = std::thread::Builder::new()
            .name("mythbrowse-sync".into())
            .spawn(move || {
                let store = match open_store() {
                    Ok(s) => s,
                    Err(e) => {
                        let mut s = st.lock().unwrap();
                        s.busy = false;
                        s.error = Some(format!("profile unavailable: {e}"));
                        drop(s);
                        on_change(false);
                        return;
                    }
                };
                Worker {
                    account,
                    store,
                    keys,
                    kdf,
                    status: st,
                    on_change,
                    key: None,
                }
                .run(rx);
            })
            .expect("spawn sync thread");
        SyncService {
            tx,
            status,
            thread: Some(thread),
        }
    }

    pub fn send(&self, c: Command) {
        if let Ok(mut s) = self.status.lock() {
            s.busy = true;
            s.error = None;
        }
        let _ = self.tx.send(c);
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for SyncService {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Worker<A, S> {
    account: A,
    store: S,
    keys: Box<dyn KeyStore>,
    kdf: KdfParams,
    status: Arc<Mutex<Status>>,
    on_change: Arc<dyn Fn(bool) + Send + Sync>,
    key: Option<SyncKey>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// User-facing words for a server error.
fn words(e: &AppwriteError) -> String {
    match e {
        AppwriteError::Http {
            status: 401, kind, ..
        } if kind == "user_invalid_credentials" => "Wrong email or password.".into(),
        AppwriteError::Http { status: 409, .. } => {
            "An account with this email already exists.".into()
        }
        AppwriteError::Http { status: 429, .. } => {
            "Too many attempts. Try again in a few minutes.".into()
        }
        AppwriteError::Http {
            status: 400,
            message,
            ..
        } => message.clone(),
        e if e.is_auth() => "Signed out by the server. Sign in again.".into(),
        AppwriteError::Transport(_) => "Can't reach the sync server. Check your connection.".into(),
        e => e.to_string(),
    }
}

fn crypto_words(e: &CryptoError) -> String {
    match e {
        CryptoError::Unwrap => "Wrong sync passphrase.".into(),
        CryptoError::RecoveryCode => {
            "That recovery code isn't right. Check it and try again.".into()
        }
        e => e.to_string(),
    }
}

impl<A: Account, S: LocalStore> Worker<A, S> {
    fn update(&self, f: impl FnOnce(&mut Status)) {
        {
            let mut s = self.status.lock().unwrap();
            f(&mut s);
        }
        (self.on_change)(false);
    }

    fn fail(&self, msg: String) {
        self.update(|s| {
            s.busy = false;
            s.error = Some(msg);
        });
    }

    fn signed_out(&mut self) {
        self.key = None;
        self.keys.delete(SESSION_SECRET);
        self.keys.delete(KEY_SECRET);
        self.update(|s| {
            s.phase = Phase::SignedOut;
            s.email = None;
            s.last = None;
            s.last_sync_ms = None;
            s.recovery_code = None;
        });
    }

    fn run(mut self, rx: Receiver<Command>) {
        self.restore();
        let mut next = Instant::now();
        loop {
            let wait = next.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(cmd) => {
                    self.handle(cmd);
                    next = Instant::now() + INTERVAL;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.key.is_some() {
                        self.sync_now();
                    }
                    next = Instant::now() + INTERVAL;
                }
            }
        }
    }

    /// Back to where the last run left off.
    fn restore(&mut self) {
        let saved = self
            .keys
            .get(SESSION_SECRET)
            .and_then(|s| serde_json::from_str::<SavedSession>(&s).ok());
        let Some(saved) = saved else {
            self.update(|s| s.busy = false);
            return;
        };
        self.account.restore(&saved.user, &saved.secret);
        let email = saved.email.clone();
        self.update(|s| s.email = Some(email));
        let key = self
            .keys
            .get(KEY_SECRET)
            .and_then(|k| data_encoding::BASE64URL_NOPAD.decode(k.as_bytes()).ok())
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .map(SyncKey::from_bytes);
        match key {
            Some(k) => {
                self.key = Some(k);
                self.update(|s| s.phase = Phase::Ready);
                self.sync_now();
            }
            None => self.after_sign_in(),
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::SignUp { email, password } => {
                if let Err(e) = self.account.create_account(&email, &password) {
                    return self.fail(words(&e));
                }
                self.sign_in(&email, &password);
            }
            Command::SignIn { email, password } => self.sign_in(&email, &password),
            Command::SetUp { passphrase } => self.set_up(&passphrase),
            Command::Unlock { passphrase } => self.unlock(&passphrase),
            Command::Recover { code, passphrase } => self.recover(&code, &passphrase),
            Command::SyncNow => {
                if self.key.is_some() {
                    self.sync_now()
                } else {
                    self.update(|s| s.busy = false)
                }
            }
            Command::AckRecovery => self.update(|s| {
                s.recovery_code = None;
                s.busy = false;
            }),
            Command::SignOut => {
                let _ = self.account.sign_out();
                self.signed_out();
                self.update(|s| s.busy = false);
            }
            Command::Shutdown => {}
        }
    }

    fn sign_in(&mut self, email: &str, password: &str) {
        let user = match self.account.sign_in(email, password) {
            Ok(u) => u,
            Err(e) => return self.fail(words(&e)),
        };
        if let Some((user_id, secret)) = self.account.session() {
            let saved = SavedSession {
                user: user_id,
                secret,
                email: email.to_owned(),
            };
            let json = Zeroizing::new(serde_json::to_string(&saved).expect("serializable"));
            if let Err(e) = self.keys.set(SESSION_SECRET, &json) {
                log::warn!("sync: session not saved ({e}); sign-in lasts until restart");
            }
        }
        log::info!("sync: signed in as user {user}");
        let email = email.to_owned();
        self.update(|s| s.email = Some(email));
        self.after_sign_in();
    }

    fn after_sign_in(&mut self) {
        match self.account.wrapped_key() {
            Ok(Some(_)) => self.update(|s| {
                s.phase = Phase::NeedsUnlock;
                s.busy = false;
            }),
            Ok(None) => self.update(|s| {
                s.phase = Phase::NeedsSetup;
                s.busy = false;
            }),
            Err(e) if e.is_auth() => {
                self.signed_out();
                self.fail(words(&e));
            }
            Err(e) => self.fail(words(&e)),
        }
    }

    fn keep_key(&mut self, key: SyncKey) {
        let b64 = Zeroizing::new(data_encoding::BASE64URL_NOPAD.encode(key.expose()));
        if let Err(e) = self.keys.set(KEY_SECRET, &b64) {
            log::warn!("sync: key not saved ({e}); you'll unlock again after restart");
        }
        self.key = Some(key);
    }

    fn set_up(&mut self, passphrase: &str) {
        // Never overwrite a key another device already set.
        match self.account.wrapped_key() {
            Ok(Some(_)) => {
                self.update(|s| s.phase = Phase::NeedsUnlock);
                return self.fail(
                    "This account already has sync set up. Unlock it with your passphrase.".into(),
                );
            }
            Ok(None) => {}
            Err(e) => return self.fail(words(&e)),
        }
        let key = match SyncKey::generate() {
            Ok(k) => k,
            Err(e) => return self.fail(crypto_words(&e)),
        };
        let wrapped = match key.wrap(passphrase, self.kdf) {
            Ok(w) => w,
            Err(e) => return self.fail(crypto_words(&e)),
        };
        if let Err(e) = self.account.set_wrapped_key(&wrapped) {
            return self.fail(words(&e));
        }
        let code = key.recovery_code();
        self.keep_key(key);
        if let Err(e) = self.store_reset() {
            return self.fail(e);
        }
        self.update(|s| {
            s.phase = Phase::Ready;
            s.recovery_code = Some(code);
        });
        self.sync_now();
    }

    fn unlock(&mut self, passphrase: &str) {
        let wrapped = match self.account.wrapped_key() {
            Ok(Some(w)) => w,
            Ok(None) => {
                self.update(|s| s.phase = Phase::NeedsSetup);
                return self.fail("Sync isn't set up on this account yet.".into());
            }
            Err(e) => return self.fail(words(&e)),
        };
        match SyncKey::unwrap(&wrapped, passphrase) {
            Ok(key) => {
                self.keep_key(key);
                if let Err(e) = self.store_reset() {
                    return self.fail(e);
                }
                self.update(|s| s.phase = Phase::Ready);
                self.sync_now();
            }
            Err(e) => self.fail(crypto_words(&e)),
        }
    }

    fn recover(&mut self, code: &str, passphrase: &str) {
        let key = match SyncKey::from_recovery_code(code) {
            Ok(k) => k,
            Err(e) => return self.fail(crypto_words(&e)),
        };
        let wrapped = match key.wrap(passphrase, self.kdf) {
            Ok(w) => w,
            Err(e) => return self.fail(crypto_words(&e)),
        };
        if let Err(e) = self.account.set_wrapped_key(&wrapped) {
            return self.fail(words(&e));
        }
        self.keep_key(key);
        if let Err(e) = self.store_reset() {
            return self.fail(e);
        }
        self.update(|s| s.phase = Phase::Ready);
        self.sync_now();
    }

    /// A freshly unlocked key: upload everything once and pull from the start.
    fn store_reset(&mut self) -> Result<(), String> {
        self.store.reset_sync()
    }

    fn sync_now(&mut self) {
        let Some(key) = &self.key else { return };
        let keys = key.record_keys();
        self.update(|s| s.busy = true);
        // A revoked / expired session would otherwise "sync" as a guest: the listing comes
        // back empty with 200 and, with nothing to upload, nothing ever fails.
        match self.account.check_session() {
            Ok(()) => {}
            Err(e) if e.is_auth() => {
                self.signed_out();
                return self.fail("Signed out by the server. Sign in again.".into());
            }
            Err(e) => return self.fail(words(&e)),
        }
        match sync(&keys, &mut self.store, &mut self.account) {
            Ok(r) => {
                let changed = r.applied > 0;
                {
                    let mut s = self.status.lock().unwrap();
                    s.busy = false;
                    s.error = None;
                    s.last_sync_ms = Some(now_ms());
                    s.last = Some(r.into());
                }
                (self.on_change)(changed);
            }
            Err(crate::engine::SyncError::Remote(msg))
                if msg.contains("401") || msg.contains("not signed in") =>
            {
                self.signed_out();
                self.fail("Signed out by the server. Sign in again.".into());
            }
            Err(e) => self.fail(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyncKind;
    use crate::engine::memory::{MemoryRemote, MemoryStore};
    use crate::engine::{Record, RemoteRow};
    use std::collections::BTreeMap;

    const FAST: KdfParams = KdfParams { m: 64, t: 1, p: 1 };

    /// One fake server shared by "devices".
    #[derive(Default)]
    struct Server {
        users: BTreeMap<String, String>,
        wrapped: Option<WrappedKey>,
        rows: MemoryRemote,
        issued: usize,
        /// Session secrets revoked on the server (signed out elsewhere, expired).
        revoked: Vec<String>,
    }

    #[derive(Clone)]
    struct FakeAccount {
        server: Arc<Mutex<Server>>,
        session: Option<(String, String)>,
    }

    impl Remote for FakeAccount {
        fn put_rows(&mut self, rows: &[(String, String)]) -> Result<(), String> {
            self.server.lock().unwrap().rows.put_rows(rows)
        }
        fn list_rows(
            &mut self,
            s: Option<&str>,
            a: Option<&str>,
            l: usize,
        ) -> Result<Vec<RemoteRow>, String> {
            if self.check_session().is_err() {
                // Like Appwrite: a dead session lists as a guest, who sees no rows.
                return Ok(vec![]);
            }
            self.server.lock().unwrap().rows.list_rows(s, a, l)
        }
    }

    impl Account for FakeAccount {
        fn create_account(&mut self, email: &str, password: &str) -> Result<(), AppwriteError> {
            let mut s = self.server.lock().unwrap();
            if s.users.contains_key(email) {
                return Err(AppwriteError::Http {
                    status: 409,
                    kind: "user_already_exists".into(),
                    message: String::new(),
                });
            }
            s.users.insert(email.into(), password.into());
            Ok(())
        }
        fn sign_in(&mut self, email: &str, password: &str) -> Result<String, AppwriteError> {
            if self
                .server
                .lock()
                .unwrap()
                .users
                .get(email)
                .map(String::as_str)
                != Some(password)
            {
                return Err(AppwriteError::Http {
                    status: 401,
                    kind: "user_invalid_credentials".into(),
                    message: String::new(),
                });
            }
            let mut srv = self.server.lock().unwrap();
            srv.issued += 1;
            self.session = Some(("u1".into(), format!("sess{}", srv.issued)));
            Ok("u1".into())
        }
        fn restore(&mut self, u: &str, s: &str) {
            self.session = Some((u.into(), s.into()));
        }
        fn session(&self) -> Option<(String, String)> {
            self.session.clone()
        }
        fn sign_out(&mut self) -> Result<(), AppwriteError> {
            self.session = None;
            Ok(())
        }
        fn wrapped_key(&self) -> Result<Option<WrappedKey>, AppwriteError> {
            Ok(self.server.lock().unwrap().wrapped.clone())
        }
        fn set_wrapped_key(&self, w: &WrappedKey) -> Result<(), AppwriteError> {
            self.server.lock().unwrap().wrapped = Some(w.clone());
            Ok(())
        }
        fn check_session(&self) -> Result<(), AppwriteError> {
            match &self.session {
                None => Err(AppwriteError::NoSession),
                Some((_, s)) if self.server.lock().unwrap().revoked.contains(s) => {
                    Err(AppwriteError::Http {
                        status: 401,
                        kind: "user_unauthorized".into(),
                        message: String::new(),
                    })
                }
                Some(_) => Ok(()),
            }
        }
    }

    #[derive(Default, Clone)]
    struct Keys(Arc<Mutex<BTreeMap<String, String>>>);
    impl KeyStore for Keys {
        fn get(&self, n: &str) -> Option<Zeroizing<String>> {
            self.0.lock().unwrap().get(n).cloned().map(Zeroizing::new)
        }
        fn set(&self, n: &str, v: &str) -> Result<(), String> {
            self.0.lock().unwrap().insert(n.into(), v.into());
            Ok(())
        }
        fn delete(&self, n: &str) {
            self.0.lock().unwrap().remove(n);
        }
    }

    /// Run one command to completion (the worker is driven directly, no thread).
    fn worker(
        server: &Arc<Mutex<Server>>,
        keys: Keys,
        store: MemoryStore,
    ) -> Worker<FakeAccount, MemoryStore> {
        let status = Arc::new(Mutex::new(Status {
            phase: Phase::SignedOut,
            email: None,
            busy: false,
            error: None,
            last_sync_ms: None,
            last: None,
            recovery_code: None,
        }));
        Worker {
            account: FakeAccount {
                server: server.clone(),
                session: None,
            },
            store,
            keys: Box::new(keys),
            kdf: FAST,
            status,
            on_change: Arc::new(|_| {}),
            key: None,
        }
    }

    fn z(s: &str) -> Zeroizing<String> {
        Zeroizing::new(s.into())
    }

    fn st<A: Account, S: LocalStore>(w: &Worker<A, S>) -> Status {
        w.status.lock().unwrap().clone()
    }

    #[test]
    fn first_device_then_second_device() {
        let server = Arc::new(Mutex::new(Server::default()));
        let mut store = MemoryStore::default();
        store.local_write(Record::new(
            SyncKind::Bookmarks,
            "https://x/",
            1,
            serde_json::json!({"title":"X"}),
        ));
        let mut a = worker(&server, Keys::default(), store);
        a.handle(Command::SignUp {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        assert_eq!(st(&a).phase, Phase::NeedsSetup);
        a.handle(Command::SetUp {
            passphrase: z("short"),
        });
        assert!(st(&a).error.unwrap().contains("at least"));
        a.handle(Command::SetUp {
            passphrase: z("sync passphrase"),
        });
        let s = st(&a);
        assert_eq!(s.phase, Phase::Ready);
        let code = s.recovery_code.clone().unwrap();
        assert_eq!(s.last.unwrap().pushed, 1);
        a.handle(Command::AckRecovery);
        assert!(st(&a).recovery_code.is_none());

        // Second device: wrong passphrase, then right one.
        let mut b = worker(&server, Keys::default(), MemoryStore::default());
        b.handle(Command::SignIn {
            email: "me@x.my".into(),
            password: z("nope"),
        });
        assert_eq!(st(&b).error.as_deref(), Some("Wrong email or password."));
        b.handle(Command::SignIn {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        assert_eq!(st(&b).phase, Phase::NeedsUnlock);
        b.handle(Command::SetUp {
            passphrase: z("another one!"),
        });
        assert_eq!(
            st(&b).phase,
            Phase::NeedsUnlock,
            "never replaces an existing key"
        );
        b.handle(Command::Unlock {
            passphrase: z("wrong passphrase"),
        });
        assert_eq!(st(&b).error.as_deref(), Some("Wrong sync passphrase."));
        b.handle(Command::Unlock {
            passphrase: z("sync passphrase"),
        });
        assert_eq!(st(&b).phase, Phase::Ready);
        assert!(b.store.live(SyncKind::Bookmarks, "https://x/").is_some());

        // Third device recovers with the code and sets a new passphrase.
        let mut c = worker(&server, Keys::default(), MemoryStore::default());
        c.handle(Command::SignIn {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        c.handle(Command::Recover {
            code: z(&code),
            passphrase: z("brand new pass"),
        });
        assert_eq!(st(&c).phase, Phase::Ready);
        assert!(c.store.live(SyncKind::Bookmarks, "https://x/").is_some());
        let w = server.lock().unwrap().wrapped.clone().unwrap();
        assert!(SyncKey::unwrap(&w, "brand new pass").is_ok());
    }

    #[test]
    fn restart_restores_without_asking_and_sign_out_forgets() {
        let server = Arc::new(Mutex::new(Server::default()));
        let keys = Keys::default();
        let mut a = worker(&server, keys.clone(), MemoryStore::default());
        a.handle(Command::SignUp {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        a.handle(Command::SetUp {
            passphrase: z("sync passphrase"),
        });
        let saved = keys.0.lock().unwrap().clone();
        assert!(saved.contains_key(SESSION_SECRET) && saved.contains_key(KEY_SECRET));
        assert!(!saved[SESSION_SECRET].contains("password1"));

        let mut again = worker(&server, keys.clone(), MemoryStore::default());
        again.restore();
        let s = st(&again);
        assert_eq!(
            (s.phase, s.email.as_deref()),
            (Phase::Ready, Some("me@x.my"))
        );
        again.handle(Command::SignOut);
        assert_eq!(st(&again).phase, Phase::SignedOut);
        assert!(keys.0.lock().unwrap().is_empty());
    }

    /// Found by the real-Appwrite e2e run: with nothing to upload, a revoked session used to
    /// "sync" fine as a guest (empty listing) and never noticed it was signed out.
    #[test]
    fn revoked_session_is_noticed_with_nothing_to_upload() {
        let server = Arc::new(Mutex::new(Server::default()));
        let keys = Keys::default();
        let mut a = worker(&server, keys.clone(), MemoryStore::default());
        a.handle(Command::SignUp {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        a.handle(Command::SetUp {
            passphrase: z("sync passphrase"),
        });
        assert_eq!(st(&a).phase, Phase::Ready);
        let secret = a.account.session().unwrap().1;
        server.lock().unwrap().revoked.push(secret);
        a.handle(Command::SyncNow);
        let s = st(&a);
        assert_eq!(s.phase, Phase::SignedOut);
        assert_eq!(
            s.error.as_deref(),
            Some("Signed out by the server. Sign in again.")
        );
        assert!(keys.0.lock().unwrap().is_empty(), "device secrets wiped");
        // Signing in again gives a new session; unlock and sync work again.
        a.handle(Command::SignIn {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        assert_eq!(st(&a).phase, Phase::NeedsUnlock);
        a.handle(Command::Unlock {
            passphrase: z("sync passphrase"),
        });
        let s = st(&a);
        assert_eq!((s.phase, s.error), (Phase::Ready, None));
    }

    #[test]
    fn duplicate_sign_up_is_explained() {
        let server = Arc::new(Mutex::new(Server::default()));
        let mut a = worker(&server, Keys::default(), MemoryStore::default());
        a.handle(Command::SignUp {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        let mut b = worker(&server, Keys::default(), MemoryStore::default());
        b.handle(Command::SignUp {
            email: "me@x.my".into(),
            password: z("password1"),
        });
        assert_eq!(
            st(&b).error.as_deref(),
            Some("An account with this email already exists.")
        );
    }

    #[test]
    fn service_thread_runs_and_shuts_down() {
        let server = Arc::new(Mutex::new(Server::default()));
        let svc = SyncService::spawn(
            FakeAccount {
                server,
                session: None,
            },
            || Ok(MemoryStore::default()),
            Box::new(Keys::default()),
            FAST,
            Arc::new(|_| {}),
        );
        svc.send(Command::SignUp {
            email: "a@b.c".into(),
            password: z("password1"),
        });
        let t = Instant::now();
        while svc.status().phase != Phase::NeedsSetup && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(svc.status().phase, Phase::NeedsSetup);
        drop(svc);
    }
}
