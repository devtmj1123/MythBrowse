// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! `/api/pw_*` and `/api/import_*`: the password manager and browser import (shell side in
//! `crates/shell/src/password_host.rs`).
//!
//! * `pw_panel`, `pw_fill`, `pw_prompt` drive the toolbar panel: chrome webview only (see
//!   [`PasswordOp::chrome_only`]); a web page or the agent can never fill or save.
//! * The rest serve the internal passwords page. Passwords never travel in a URL: a new one
//!   comes in the `x-mb-secret` header as a form (`p=…`); a revealed one only in a response
//!   body, and only after a fresh OS re-authentication.

use crate::chrome::ApiError;

/// The answer to "Save password?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptChoice {
    Save,
    Never,
    NotNow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordOp {
    /// Open / close the passwords panel over the active page.
    Panel {
        open: bool,
    },
    /// Fill saved login `id` into the active page (the user clicked it in the panel).
    Fill {
        id: i64,
    },
    /// Answer the pending "Save password?" prompt.
    Prompt {
        choice: PromptChoice,
    },
    /// Store availability, re-auth and import state.
    Status,
    /// Saved logins (no passwords), filtered by `q`.
    List {
        q: String,
    },
    /// Ask the OS to confirm it is the user (Windows sign-in prompt) before reveal / edit /
    /// export. `confirm`: the in-app confirmation, accepted only where the OS has no prompt.
    Reauth {
        confirm: bool,
    },
    /// The password (and note) of login `id` (needs a fresh re-auth).
    Reveal {
        id: i64,
    },
    /// Edit login `id`; a new password comes in the secret header (needs a fresh re-auth).
    Update {
        id: i64,
        username: Option<String>,
        note: Option<String>,
    },
    Delete {
        id: i64,
    },
    /// Write every login to a CSV in Downloads (needs a fresh re-auth).
    Export,
    /// Sites where the user chose "Never for this site".
    NeverList,
    NeverDelete {
        origin: String,
    },
    /// Start over when the store is locked (its key is gone).
    Reset,
    /// Chrome / Edge / Chromium profiles found on this computer.
    ImportSources,
    /// Import from one profile.
    ImportRun {
        browser: String,
        profile: String,
        bookmarks: bool,
        history: bool,
        passwords: bool,
    },
    /// Import a password CSV file (a local path).
    ImportCsv {
        path: String,
    },
    /// Password CSV files in Downloads.
    ImportCsvFiles,
}

impl PasswordOp {
    /// Ops only the chrome webview may send (the toolbar panel).
    pub fn chrome_only(&self) -> bool {
        matches!(
            self,
            PasswordOp::Panel { .. } | PasswordOp::Fill { .. } | PasswordOp::Prompt { .. }
        )
    }
}

/// Whether `op` is one of ours.
pub fn is_password_op(op: &str) -> bool {
    op.starts_with("pw_") || op.starts_with("import_")
}

pub fn parse(op: &str, param: &dyn Fn(&str) -> Option<String>) -> Result<PasswordOp, ApiError> {
    let flag = |name: &'static str| -> Result<bool, ApiError> {
        match param(name).as_deref() {
            Some("1") => Ok(true),
            Some("0") | None => Ok(false),
            Some(_) => Err(ApiError::BadParam(name)),
        }
    };
    let id = || -> Result<i64, ApiError> {
        param("id")
            .ok_or(ApiError::MissingParam("id"))?
            .parse::<i64>()
            .ok()
            .filter(|i| *i > 0)
            .ok_or(ApiError::BadParam("id"))
    };
    let short = |name: &'static str, max: usize| -> Result<Option<String>, ApiError> {
        match param(name) {
            Some(v) if v.chars().count() > max => Err(ApiError::BadParam(name)),
            v => Ok(v),
        }
    };
    Ok(match op {
        "pw_panel" => PasswordOp::Panel {
            open: match param("open").as_deref() {
                Some("1") => true,
                Some("0") => false,
                Some(_) => return Err(ApiError::BadParam("open")),
                None => return Err(ApiError::MissingParam("open")),
            },
        },
        "pw_fill" => PasswordOp::Fill { id: id()? },
        "pw_prompt" => PasswordOp::Prompt {
            choice: match param("choice").as_deref() {
                Some("save") => PromptChoice::Save,
                Some("never") => PromptChoice::Never,
                Some("not_now") => PromptChoice::NotNow,
                Some(_) => return Err(ApiError::BadParam("choice")),
                None => return Err(ApiError::MissingParam("choice")),
            },
        },
        "pw_status" => PasswordOp::Status,
        "pw_list" => PasswordOp::List {
            q: short("q", 200)?.unwrap_or_default(),
        },
        "pw_reauth" => PasswordOp::Reauth {
            confirm: flag("confirm")?,
        },
        "pw_reveal" => PasswordOp::Reveal { id: id()? },
        "pw_update" => PasswordOp::Update {
            id: id()?,
            username: short("username", 512)?,
            note: short("note", 2000)?,
        },
        "pw_delete" => PasswordOp::Delete { id: id()? },
        "pw_export" => PasswordOp::Export,
        "pw_never" => PasswordOp::NeverList,
        "pw_never_delete" => PasswordOp::NeverDelete {
            origin: short("origin", 300)?.ok_or(ApiError::MissingParam("origin"))?,
        },
        "pw_reset" => PasswordOp::Reset,
        "import_sources" => PasswordOp::ImportSources,
        "import_run" => PasswordOp::ImportRun {
            browser: match param("browser").as_deref() {
                Some(b @ ("chrome" | "edge" | "chromium")) => b.to_owned(),
                Some(_) => return Err(ApiError::BadParam("browser")),
                None => return Err(ApiError::MissingParam("browser")),
            },
            profile: {
                let p = param("profile").ok_or(ApiError::MissingParam("profile"))?;
                // A profile folder name (`Default`, `Profile 3`), never a path.
                let ok = !p.is_empty()
                    && p.len() <= 64
                    && !p.contains(['/', '\\'])
                    && p != "."
                    && p != "..";
                if !ok {
                    return Err(ApiError::BadParam("profile"));
                }
                p
            },
            bookmarks: flag("bookmarks")?,
            history: flag("history")?,
            passwords: flag("passwords")?,
        },
        "import_csv" => PasswordOp::ImportCsv {
            path: short("path", 1024)?
                .filter(|p| !p.trim().is_empty())
                .ok_or(ApiError::MissingParam("path"))?,
        },
        "import_csv_files" => PasswordOp::ImportCsvFiles,
        other => return Err(ApiError::UnknownOp(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(op: &str, q: &[(&str, &str)]) -> Result<PasswordOp, ApiError> {
        let q: Vec<(String, String)> = q
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        parse(op, &move |k: &str| {
            q.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn parses_and_validates() {
        assert_eq!(p("pw_fill", &[("id", "7")]), Ok(PasswordOp::Fill { id: 7 }));
        assert_eq!(p("pw_fill", &[("id", "-1")]), Err(ApiError::BadParam("id")));
        assert_eq!(
            p("pw_prompt", &[("choice", "never")]),
            Ok(PasswordOp::Prompt {
                choice: PromptChoice::Never
            })
        );
        assert_eq!(
            p("pw_reauth", &[]),
            Ok(PasswordOp::Reauth { confirm: false })
        );
        assert_eq!(
            p(
                "import_run",
                &[
                    ("browser", "edge"),
                    ("profile", "Profile 1"),
                    ("history", "1")
                ]
            ),
            Ok(PasswordOp::ImportRun {
                browser: "edge".into(),
                profile: "Profile 1".into(),
                bookmarks: false,
                history: true,
                passwords: false
            })
        );
        assert_eq!(
            p("import_run", &[("browser", "chrome"), ("profile", "../x")]),
            Err(ApiError::BadParam("profile"))
        );
        assert_eq!(
            p("pw_nope", &[]),
            Err(ApiError::UnknownOp("pw_nope".into()))
        );
        // A password parameter is not a thing: updates take it from the secret header.
        assert_eq!(
            p("pw_update", &[("id", "2"), ("password", "x")]),
            Ok(PasswordOp::Update {
                id: 2,
                username: None,
                note: None
            })
        );
    }

    #[test]
    fn panel_fill_and_prompt_are_chrome_only() {
        assert!(PasswordOp::Fill { id: 1 }.chrome_only());
        assert!(PasswordOp::Panel { open: true }.chrome_only());
        assert!(
            PasswordOp::Prompt {
                choice: PromptChoice::Save
            }
            .chrome_only()
        );
        assert!(!PasswordOp::List { q: String::new() }.chrome_only());
    }
}
