// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Appwrite Cloud as the sync server (REST, client role: only the signed-in user's session,
//! never a server API key in the browser).
//!
//! Server layout (created once with `server/appwrite/setup.py`; done for the JustMyth project
//! 2026-10-09):
//! - Database `mythbrowse`, table `sync_rows`, row security on, table permission
//!   `create("users")`. One column: `ct` (ciphertext, text). Row ids are the HMAC ids from
//!   [`crate::crypto::RecordKeys::row_id`]; each row is readable/writable only by its owner.
//! - The wrapped sync key lives in the user's account prefs (`mb_sync_key`).
//!
//! Sign-in: email + password session. Appwrite returns the session in a `Set-Cookie:
//! a_session_<project>=<secret>` header (as for its native SDKs); we keep that secret in the
//! OS secret store and send it back as a cookie.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;
use zeroize::Zeroizing;

use crate::crypto::WrappedKey;
use crate::engine::{Remote, RemoteRow};

pub const PREFS_KEY: &str = "mb_sync_key";
/// Appwrite endpoint and project of the official builds, set at build time
/// (`MYTHBROWSE_APPWRITE_ENDPOINT`, `MYTHBROWSE_APPWRITE_PROJECT`). A source build without
/// them has no cloud project: sync and the cloud site-fix rules stay off and nothing is sent to
/// JustMyth's backend. A project id is not a secret (clients send it with every request).
pub const DEFAULT_ENDPOINT: &str = match option_env!("MYTHBROWSE_APPWRITE_ENDPOINT") {
    Some(e) => e,
    None => "https://sgp.cloud.appwrite.io/v1",
};
pub const DEFAULT_PROJECT: &str = match option_env!("MYTHBROWSE_APPWRITE_PROJECT") {
    Some(p) => p,
    None => "",
};

/// Whether this build was given a cloud project (see [`DEFAULT_PROJECT`]) on an HTTPS endpoint.
pub fn cloud_configured() -> bool {
    !DEFAULT_PROJECT.is_empty() && DEFAULT_ENDPOINT.starts_with("https://")
}
/// Public bucket with signed compat rulesets.
pub const RULES_BUCKET: &str = "compat_rules";
/// File ids of the published compat ruleset and its signature in [`RULES_BUCKET`]
/// (`server/appwrite/publish_rules.py`).
pub const RULES_FILE: &str = "ruleset-toml";
pub const RULES_SIG_FILE: &str = "ruleset-sig";
/// Largest ruleset the browser downloads.
pub const MAX_RULES_BYTES: u64 = 2 * 1024 * 1024;
const USER_AGENT: &str = concat!("MythBrowse/", env!("CARGO_PKG_VERSION"), " sync");
/// Appwrite response format this client is written against.
const RESPONSE_FORMAT: &str = "1.8.0";
const MAX_BODY: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppwriteConfig {
    /// e.g. `https://sgp.cloud.appwrite.io/v1`
    pub endpoint: Url,
    pub project: String,
    #[serde(default = "default_database")]
    pub database: String,
    #[serde(default = "default_table")]
    pub table: String,
}

impl Default for AppwriteConfig {
    fn default() -> Self {
        AppwriteConfig {
            endpoint: Url::parse(DEFAULT_ENDPOINT).expect("valid default endpoint"),
            project: DEFAULT_PROJECT.into(),
            database: default_database(),
            table: default_table(),
        }
    }
}

fn default_database() -> String {
    "mythbrowse".into()
}
fn default_table() -> String {
    "sync_rows".into()
}

#[derive(Debug, thiserror::Error)]
pub enum AppwriteError {
    #[error("could not reach the sync server: {0}")]
    Transport(String),
    #[error("sync server error {status}: {message}")]
    Http {
        status: u16,
        kind: String,
        message: String,
    },
    #[error("unexpected reply from the sync server: {0}")]
    Protocol(String),
    #[error("not signed in")]
    NoSession,
}

impl AppwriteError {
    pub fn is_auth(&self) -> bool {
        matches!(
            self,
            AppwriteError::NoSession | AppwriteError::Http { status: 401, .. }
        )
    }
}

/// A signed-in session. The secret is wiped from memory on drop.
pub struct Session {
    pub user_id: String,
    secret: Zeroizing<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn new(user_id: String, secret: String) -> Self {
        Session {
            user_id,
            secret: Zeroizing::new(secret),
        }
    }
    /// For saving in the OS secret store.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

pub struct Appwrite {
    cfg: AppwriteConfig,
    agent: ureq::Agent,
    session: Option<Session>,
}

/// One Appwrite query, in its JSON wire form.
pub fn query(method: &str, attribute: Option<&str>, values: &[Value]) -> String {
    let mut q = serde_json::Map::new();
    q.insert("method".into(), json!(method));
    if let Some(a) = attribute {
        q.insert("attribute".into(), json!(a));
    }
    if !values.is_empty() {
        q.insert("values".into(), Value::Array(values.to_vec()));
    }
    Value::Object(q).to_string()
}

/// Queries for one page of [`Remote::list_rows`].
pub fn list_queries(since: Option<&str>, after_row: Option<&str>, limit: usize) -> Vec<String> {
    let mut q = Vec::new();
    if let Some(s) = since {
        q.push(query("greaterThanEqual", Some("$updatedAt"), &[json!(s)]));
    }
    q.push(query("orderAsc", Some("$updatedAt"), &[]));
    q.push(query("orderAsc", Some("$id"), &[]));
    if let Some(a) = after_row {
        q.push(query("cursorAfter", None, &[json!(a)]));
    }
    q.push(query("limit", None, &[json!(limit)]));
    q
}

/// Row permissions: only the owner.
pub fn owner_permissions(user_id: &str) -> Value {
    json!([
        format!("read(\"user:{user_id}\")"),
        format!("update(\"user:{user_id}\")"),
        format!("delete(\"user:{user_id}\")"),
    ])
}

/// The session secret from `Set-Cookie` headers (`a_session_<project>=...`; the `_legacy`
/// twin is ignored).
pub fn session_from_cookies<'a>(
    project: &str,
    set_cookie: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let name = format!("a_session_{}=", project.to_ascii_lowercase());
    set_cookie.into_iter().find_map(|h| {
        let first = h.split(';').next()?.trim();
        let lower = first.to_ascii_lowercase();
        if lower.starts_with(&name) {
            let v = &first[name.len()..];
            (!v.is_empty()).then(|| v.to_owned())
        } else {
            None
        }
    })
}

impl Appwrite {
    pub fn new(cfg: AppwriteConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(USER_AGENT)
            .build()
            .into();
        Appwrite {
            cfg,
            agent,
            session: None,
        }
    }

    pub fn with_session(mut self, s: Session) -> Self {
        self.session = Some(s);
        self
    }

    pub fn set_session(&mut self, s: Session) {
        self.session = Some(s);
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    fn url(&self, path: &str) -> Url {
        let mut u = self.cfg.endpoint.clone();
        let base = u.path().trim_end_matches('/').to_owned();
        u.set_path(&format!("{base}{path}"));
        u
    }

    fn rows_path(&self) -> String {
        format!(
            "/tablesdb/{}/tables/{}/rows",
            self.cfg.database, self.cfg.table
        )
    }

    fn cookie(&self) -> Option<String> {
        self.session.as_ref().map(|s| {
            format!(
                "a_session_{}={}",
                self.cfg.project.to_ascii_lowercase(),
                s.secret()
            )
        })
    }

    /// Send a request; returns (status, body JSON, set-cookie values).
    fn call(
        &self,
        method: &str,
        url: Url,
        body: Option<&Value>,
        auth: bool,
    ) -> Result<(Value, Vec<String>), AppwriteError> {
        let cookie = if auth {
            Some(self.cookie().ok_or(AppwriteError::NoSession)?)
        } else {
            None
        };
        let u = url.as_str();
        macro_rules! headers {
            ($r:expr) => {{
                let mut r = $r
                    .header("X-Appwrite-Project", &self.cfg.project)
                    .header("X-Appwrite-Response-Format", RESPONSE_FORMAT)
                    .header("Accept", "application/json");
                if let Some(c) = &cookie {
                    r = r.header("Cookie", c);
                }
                r
            }};
        }
        let data = body.map(|b| b.to_string()).unwrap_or_default();
        let res = match method {
            "GET" => headers!(self.agent.get(u)).call(),
            "DELETE" => headers!(self.agent.delete(u)).call(),
            "POST" => headers!(self.agent.post(u))
                .header("Content-Type", "application/json")
                .send(&data),
            "PUT" => headers!(self.agent.put(u))
                .header("Content-Type", "application/json")
                .send(&data),
            "PATCH" => headers!(self.agent.patch(u))
                .header("Content-Type", "application/json")
                .send(&data),
            m => return Err(AppwriteError::Protocol(format!("method {m}"))),
        };
        let mut resp = res.map_err(|e| AppwriteError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let cookies: Vec<String> = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect();
        let text = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()
            .map_err(|e| AppwriteError::Transport(e.to_string()))?;
        let v: Value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(|e| AppwriteError::Protocol(e.to_string()))?
        };
        if !(200..300).contains(&status) {
            return Err(AppwriteError::Http {
                status,
                kind: v["type"].as_str().unwrap_or_default().to_owned(),
                message: v["message"].as_str().unwrap_or("request failed").to_owned(),
            });
        }
        Ok((v, cookies))
    }

    /// Download a public file from a bucket as text (no session needed). `Ok(None)` when it
    /// does not exist.
    pub fn download_public(
        &self,
        bucket: &str,
        file: &str,
    ) -> Result<Option<String>, AppwriteError> {
        let mut u = self.url(&format!("/storage/buckets/{bucket}/files/{file}/download"));
        u.query_pairs_mut()
            .append_pair("project", &self.cfg.project);
        let res = self
            .agent
            .get(u.as_str())
            .header("X-Appwrite-Project", &self.cfg.project)
            .call();
        let mut resp = match res {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            Err(ureq::Error::StatusCode(status)) => {
                return Err(AppwriteError::Http {
                    status,
                    kind: String::new(),
                    message: "download failed".into(),
                });
            }
            Err(e) => return Err(AppwriteError::Transport(e.to_string())),
        };
        resp.body_mut()
            .with_config()
            .limit(MAX_RULES_BYTES)
            .read_to_string()
            .map(Some)
            .map_err(|e| AppwriteError::Transport(e.to_string()))
    }

    /// The published compat ruleset and its signature, if both exist.
    pub fn compat_ruleset(&self) -> Result<Option<(String, String)>, AppwriteError> {
        let Some(toml) = self.download_public(RULES_BUCKET, RULES_FILE)? else {
            return Ok(None);
        };
        let Some(sig) = self.download_public(RULES_BUCKET, RULES_SIG_FILE)? else {
            return Ok(None);
        };
        Ok(Some((toml, sig)))
    }

    /// Create an account (email + password). Does not sign in.
    pub fn create_account(
        &self,
        email: &str,
        password: &str,
        name: &str,
    ) -> Result<String, AppwriteError> {
        let body =
            json!({ "userId": "unique()", "email": email, "password": password, "name": name });
        let (v, _) = self.call("POST", self.url("/account"), Some(&body), false)?;
        v["$id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| AppwriteError::Protocol("no user id".into()))
    }

    pub fn sign_in(&mut self, email: &str, password: &str) -> Result<&Session, AppwriteError> {
        let body = json!({ "email": email, "password": password });
        let (v, cookies) = self.call(
            "POST",
            self.url("/account/sessions/email"),
            Some(&body),
            false,
        )?;
        let user = v["userId"]
            .as_str()
            .ok_or_else(|| AppwriteError::Protocol("no userId".into()))?;
        let secret = session_from_cookies(&self.cfg.project, cookies.iter().map(String::as_str))
            .or_else(|| {
                v["secret"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
            .ok_or_else(|| AppwriteError::Protocol("no session cookie".into()))?;
        self.session = Some(Session::new(user.to_owned(), secret));
        Ok(self.session.as_ref().unwrap())
    }

    pub fn sign_out(&mut self) -> Result<(), AppwriteError> {
        if self.session.is_some() {
            let r = self.call("DELETE", self.url("/account/sessions/current"), None, true);
            self.session = None;
            r?;
        }
        Ok(())
    }

    pub fn account(&self) -> Result<Value, AppwriteError> {
        Ok(self.call("GET", self.url("/account"), None, true)?.0)
    }

    /// The wrapped sync key, if this account has set up sync.
    pub fn wrapped_key(&self) -> Result<Option<WrappedKey>, AppwriteError> {
        let (prefs, _) = self.call("GET", self.url("/account/prefs"), None, true)?;
        match prefs.get(PREFS_KEY) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                // Stored as a JSON string: Appwrite prefs values are kept as given.
                let parsed = match v {
                    Value::String(s) => serde_json::from_str(s),
                    other => serde_json::from_value(other.clone()),
                };
                parsed
                    .map(Some)
                    .map_err(|e| AppwriteError::Protocol(format!("bad {PREFS_KEY}: {e}")))
            }
        }
    }

    /// Store the wrapped key, keeping other prefs (Appwrite replaces the whole prefs object).
    pub fn set_wrapped_key(&self, w: &WrappedKey) -> Result<(), AppwriteError> {
        let (mut prefs, _) = self.call("GET", self.url("/account/prefs"), None, true)?;
        if !prefs.is_object() {
            prefs = json!({});
        }
        prefs[PREFS_KEY] = json!(serde_json::to_string(w).expect("serializable"));
        self.call(
            "PATCH",
            self.url("/account/prefs"),
            Some(&json!({ "prefs": prefs })),
            true,
        )?;
        Ok(())
    }

    fn user_id(&self) -> Result<&str, AppwriteError> {
        self.session
            .as_ref()
            .map(|s| s.user_id.as_str())
            .ok_or(AppwriteError::NoSession)
    }

    pub fn put_row(&self, row_id: &str, ct: &str) -> Result<(), AppwriteError> {
        let body =
            json!({ "data": { "ct": ct }, "permissions": owner_permissions(self.user_id()?) });
        let url = self.url(&format!("{}/{row_id}", self.rows_path()));
        self.call("PUT", url, Some(&body), true)?;
        Ok(())
    }

    pub fn list(
        &self,
        since: Option<&str>,
        after_row: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RemoteRow>, AppwriteError> {
        let mut url = self.url(&self.rows_path());
        {
            let mut qp = url.query_pairs_mut();
            for q in list_queries(since, after_row, limit) {
                qp.append_pair("queries[]", &q);
            }
            qp.append_pair("total", "false");
        }
        let (v, _) = self.call("GET", url, None, true)?;
        parse_rows(&v)
    }
}

pub fn parse_rows(v: &Value) -> Result<Vec<RemoteRow>, AppwriteError> {
    let rows = v["rows"]
        .as_array()
        .ok_or_else(|| AppwriteError::Protocol("no rows".into()))?;
    rows.iter()
        .map(|r| {
            let s = |k: &str| {
                r[k].as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| AppwriteError::Protocol(format!("row without {k}")))
            };
            Ok(RemoteRow {
                row_id: s("$id")?,
                ct: s("ct")?,
                updated_at: s("$updatedAt")?,
            })
        })
        .collect()
}

impl Remote for Appwrite {
    fn put_rows(&mut self, rows: &[(String, String)]) -> Result<(), String> {
        // Appwrite's bulk upsert is server-key only; one PUT per row from a client session.
        for (id, ct) in rows {
            self.put_row(id, ct).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn list_rows(
        &mut self,
        since: Option<&str>,
        after_row: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RemoteRow>, String> {
        self.list(since, after_row, limit)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AppwriteConfig {
        AppwriteConfig {
            endpoint: Url::parse("https://sgp.cloud.appwrite.io/v1").unwrap(),
            project: "MythBrowse01".into(),
            database: default_database(),
            table: default_table(),
        }
    }

    #[test]
    fn default_config() {
        let c = AppwriteConfig::default();
        assert_eq!(c.endpoint.host_str(), Some("sgp.cloud.appwrite.io"));
        assert_eq!(
            (c.database.as_str(), c.table.as_str()),
            ("mythbrowse", "sync_rows")
        );
    }

    #[test]
    fn urls() {
        let a = Appwrite::new(cfg());
        assert_eq!(
            a.url("/account").as_str(),
            "https://sgp.cloud.appwrite.io/v1/account"
        );
        assert_eq!(
            a.url(&a.rows_path()).as_str(),
            "https://sgp.cloud.appwrite.io/v1/tablesdb/mythbrowse/tables/sync_rows/rows"
        );
    }

    #[test]
    fn queries_wire_form() {
        let q = list_queries(Some("2026-10-09T01:02:03.004+00:00"), Some("abc"), 100);
        assert_eq!(
            q,
            vec![
                r#"{"attribute":"$updatedAt","method":"greaterThanEqual","values":["2026-10-09T01:02:03.004+00:00"]}"#,
                r#"{"attribute":"$updatedAt","method":"orderAsc"}"#,
                r#"{"attribute":"$id","method":"orderAsc"}"#,
                r#"{"method":"cursorAfter","values":["abc"]}"#,
                r#"{"method":"limit","values":[100]}"#,
            ]
        );
        assert_eq!(list_queries(None, None, 5).len(), 3);
    }

    #[test]
    fn session_cookie_parsing() {
        let h = [
            "a_session_mythbrowse01_legacy=old; path=/; httponly",
            "a_session_mythbrowse01=s3cret; expires=Fri, 09 Oct 2027 00:00:00 GMT; path=/; secure; httponly",
        ];
        assert_eq!(
            session_from_cookies("MythBrowse01", h.iter().copied()).as_deref(),
            Some("s3cret")
        );
        assert_eq!(session_from_cookies("other", h.iter().copied()), None);
        assert_eq!(
            session_from_cookies("MythBrowse01", ["a_session_mythbrowse01=; path=/"]),
            None
        );
    }

    #[test]
    fn rows_and_permissions() {
        let v = json!({ "rows": [ { "$id": "ab", "ct": "v1.x", "$updatedAt": "2026-10-09T00:00:00.000+00:00", "$permissions": [] } ] });
        let rows = parse_rows(&v).unwrap();
        assert_eq!(rows[0].row_id, "ab");
        assert!(parse_rows(&json!({ "rows": [ { "$id": "x" } ] })).is_err());
        assert_eq!(owner_permissions("u1")[0], "read(\"user:u1\")");
    }

    #[test]
    fn session_debug_hides_secret() {
        let s = Session::new("u1".into(), "topsecret".into());
        assert!(!format!("{s:?}").contains("topsecret"));
    }

    #[test]
    fn no_session_is_auth_error() {
        let a = Appwrite::new(cfg());
        let e = a.account().unwrap_err();
        assert!(e.is_auth());
    }
}
