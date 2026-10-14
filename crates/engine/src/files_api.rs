// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! RPC ops for downloads, the "On startup" setting and pinned tabs (`/api/<op>`, see
//! [`crate::chrome::ApiCall::Files`]).
//!
//! | op | params | who |
//! |---|---|---|
//! | `downloads` | | internal pages |
//! | `download_action` | `id`, `op` = pause / resume / retry / cancel / keep / discard / open / show / remove, `confirm=1` (open a dangerous file) | internal pages |
//! | `downloads_clear` | | internal pages |
//! | `downloads_folder` | | internal pages (opens the folder) |
//! | `open_downloads` | | chrome only (opens / switches to the Downloads page) |
//! | `startup_pref` | | internal pages |
//! | `startup_pref_save` | `mode` = continue / newtab | internal pages |
//! | `pin_tab` | `id`, `on` = 1 / 0 | chrome only |
//!
//! "Internal pages" are the chrome and the browser's own pages (Downloads, Library), never a
//! web page and never a tab Myth controls (agent tabs cannot load internal pages).

use url::Url;

use crate::chrome::{ApiError, INTERNAL_ORIGIN};
use crate::tabs::TabId;

/// What to do with one download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadOp {
    Pause,
    Resume,
    Retry,
    Cancel,
    Keep,
    Discard,
    Open,
    Show,
    Remove,
}

impl DownloadOp {
    pub fn parse(s: &str) -> Option<DownloadOp> {
        Some(match s {
            "pause" => DownloadOp::Pause,
            "resume" => DownloadOp::Resume,
            "retry" => DownloadOp::Retry,
            "cancel" => DownloadOp::Cancel,
            "keep" => DownloadOp::Keep,
            "discard" => DownloadOp::Discard,
            "open" => DownloadOp::Open,
            "show" => DownloadOp::Show,
            "remove" => DownloadOp::Remove,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesOp {
    Downloads,
    Action {
        id: i64,
        op: DownloadOp,
        confirm: bool,
    },
    Clear,
    Folder,
    OpenDownloads,
    StartupPref,
    StartupPrefSave {
        mode: String,
    },
    PinTab {
        id: TabId,
        on: bool,
    },
}

const OPS: &[&str] = &[
    "downloads",
    "download_action",
    "downloads_clear",
    "downloads_folder",
    "open_downloads",
    "startup_pref",
    "startup_pref_save",
    "pin_tab",
];

pub fn is_files_op(op: &str) -> bool {
    OPS.contains(&op)
}

impl FilesOp {
    pub fn parse(op: &str, param: &dyn Fn(&str) -> Option<String>) -> Result<FilesOp, ApiError> {
        let flag = |name: &'static str| -> Result<bool, ApiError> {
            match param(name).as_deref() {
                None | Some("0") => Ok(false),
                Some("1") => Ok(true),
                Some(_) => Err(ApiError::BadParam(name)),
            }
        };
        Ok(match op {
            "downloads" => FilesOp::Downloads,
            "download_action" => {
                let id = param("id")
                    .ok_or(ApiError::MissingParam("id"))?
                    .parse::<i64>()
                    .map_err(|_| ApiError::BadParam("id"))?;
                let op = DownloadOp::parse(&param("op").ok_or(ApiError::MissingParam("op"))?)
                    .ok_or(ApiError::BadParam("op"))?;
                FilesOp::Action {
                    id,
                    op,
                    confirm: flag("confirm")?,
                }
            }
            "downloads_clear" => FilesOp::Clear,
            "downloads_folder" => FilesOp::Folder,
            "open_downloads" => FilesOp::OpenDownloads,
            "startup_pref" => FilesOp::StartupPref,
            "startup_pref_save" => match param("mode").as_deref() {
                Some(m @ ("continue" | "newtab")) => {
                    FilesOp::StartupPrefSave { mode: m.to_owned() }
                }
                Some(_) => return Err(ApiError::BadParam("mode")),
                None => return Err(ApiError::MissingParam("mode")),
            },
            "pin_tab" => FilesOp::PinTab {
                id: param("id")
                    .ok_or(ApiError::MissingParam("id"))?
                    .parse::<u64>()
                    .map(TabId)
                    .map_err(|_| ApiError::BadParam("id"))?,
                on: match param("on").as_deref() {
                    Some("1") => true,
                    Some("0") => false,
                    Some(_) => return Err(ApiError::BadParam("on")),
                    None => return Err(ApiError::MissingParam("on")),
                },
            },
            other => return Err(ApiError::UnknownOp(other.to_owned())),
        })
    }

    /// Ops that drive the browser window itself.
    pub fn chrome_only(&self) -> bool {
        matches!(self, FilesOp::OpenDownloads | FilesOp::PinTab { .. })
    }
}

/// The Downloads page.
pub fn downloads_url() -> Url {
    Url::parse(INTERNAL_ORIGIN)
        .and_then(|u| u.join("/downloads.html"))
        .expect("valid internal URL")
}

pub fn is_downloads_page(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| crate::chrome::is_internal(&u) && u.path() == "/downloads.html")
}

/// The toolbar Downloads button's state: `{active, frac, attention}` (`frac` 0..1 when every
/// running size is known), or `None` when there is nothing to show (no downloads this session).
pub fn toolbar_state(
    active: usize,
    frac: Option<f64>,
    attention: bool,
    any: bool,
) -> Option<serde_json::Value> {
    any.then(|| {
        serde_json::json!({
            "active": active,
            "frac": frac.map(|f| (f.clamp(0.0, 1.0) * 1000.0).round() / 1000.0),
            "attention": attention,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::ApiCall;

    fn call(q: &str) -> Result<ApiCall, ApiError> {
        ApiCall::parse(&Url::parse(&format!("{INTERNAL_ORIGIN}/api/{q}")).unwrap())
    }

    #[test]
    fn parses_ops() {
        assert_eq!(
            call("downloads").unwrap(),
            ApiCall::Files(FilesOp::Downloads)
        );
        assert_eq!(
            call("download_action?id=7&op=open&confirm=1").unwrap(),
            ApiCall::Files(FilesOp::Action {
                id: 7,
                op: DownloadOp::Open,
                confirm: true
            })
        );
        assert_eq!(
            call("download_action?id=7&op=keep").unwrap(),
            ApiCall::Files(FilesOp::Action {
                id: 7,
                op: DownloadOp::Keep,
                confirm: false
            })
        );
        assert!(matches!(
            call("download_action?id=x&op=open"),
            Err(ApiError::BadParam("id"))
        ));
        assert!(matches!(
            call("download_action?id=1&op=explode"),
            Err(ApiError::BadParam("op"))
        ));
        assert!(matches!(
            call("download_action?id=1"),
            Err(ApiError::MissingParam("op"))
        ));
        assert!(matches!(
            call("download_action?id=1&op=open&confirm=yes"),
            Err(ApiError::BadParam("confirm"))
        ));
        assert_eq!(
            call("startup_pref_save?mode=newtab").unwrap(),
            ApiCall::Files(FilesOp::StartupPrefSave {
                mode: "newtab".into()
            })
        );
        assert!(matches!(
            call("startup_pref_save?mode=x"),
            Err(ApiError::BadParam("mode"))
        ));
        assert_eq!(
            call("pin_tab?id=3&on=1").unwrap(),
            ApiCall::Files(FilesOp::PinTab {
                id: TabId(3),
                on: true
            })
        );
        assert!(matches!(
            call("pin_tab?id=3"),
            Err(ApiError::MissingParam("on"))
        ));
    }

    #[test]
    fn only_window_ops_are_chrome_only() {
        assert!(call("open_downloads").unwrap().chrome_only());
        assert!(call("pin_tab?id=1&on=0").unwrap().chrome_only());
        for q in [
            "downloads",
            "download_action?id=1&op=pause",
            "downloads_clear",
            "downloads_folder",
            "startup_pref",
        ] {
            assert!(!call(q).unwrap().chrome_only(), "{q}");
        }
    }

    #[test]
    fn page_url_and_toolbar() {
        assert!(is_downloads_page(downloads_url().as_str()));
        assert!(!is_downloads_page("https://example.com/downloads.html"));
        assert_eq!(toolbar_state(0, None, false, false), None);
        let v = toolbar_state(2, Some(0.33333), true, true).unwrap();
        assert_eq!(v["active"], 2);
        assert_eq!(v["frac"], 0.333);
        assert_eq!(v["attention"], true);
    }
}
