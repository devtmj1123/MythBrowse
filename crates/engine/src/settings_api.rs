// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! RPC ops for Settings (v0.4): general / appearance preferences that have no op of their own,
//! the downloads folder, cookies and site data, the HTTP cache, and updates
//! (`/api/<op>`, see [`crate::chrome::ApiCall::Settings`]). Internal pages only (the Settings
//! page and the theme loader on every UI page); never a web page or an agent tab.
//!
//! | op | params |
//! |---|---|
//! | `settings` | |
//! | `settings_save` | any of `search_engine` (id), `ask_save` = 1 / 0, `update_auto` = 1 / 0, `theme` = time / light / dark |
//! | `downloads_folder_pick` | (native folder chooser; the choice is saved) |
//! | `downloads_folder_reset` | (back to the system Downloads folder) |
//! | `site_data` | (sites with cookies / local / session storage) |
//! | `site_data_clear` | `what` = comma list of cookies, storage, cache; `site` = one registrable domain (optional; cache is always global) |
//! | `update_status` | |
//! | `update_check` | (the user's "Check for updates": ignores the once-a-day limit) |
//! | `update_install` | (the user's "Download & install": download, verify, run the installer) |
//! | `cookies`, `cookies_list`, `cookie_delete`, `cookie_policy`, `cookies_prefs_save`, `cookie_site` | see [`crate::cookies_api`] |

use crate::chrome::ApiError;
use crate::cookies_api::valid_site;
use crate::omnibox::SearchEngine;

/// The app theme: follow the time of day (default), or pin the light / dark look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Time,
    Light,
    Dark,
}

impl Theme {
    pub fn parse(s: &str) -> Option<Theme> {
        match s {
            "time" => Some(Theme::Time),
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Time => "time",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// What `site_data_clear` removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClearWhat {
    pub cookies: bool,
    /// localStorage + sessionStorage.
    pub storage: bool,
    /// Servo's HTTP cache (all sites).
    pub cache: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsOp {
    Get,
    Save {
        search_engine: Option<SearchEngine>,
        ask_save: Option<bool>,
        update_auto: Option<bool>,
        theme: Option<Theme>,
    },
    DownloadsFolderPick,
    DownloadsFolderReset,
    SiteData,
    SiteDataClear {
        what: ClearWhat,
        site: Option<String>,
    },
    UpdateStatus,
    UpdateCheck,
    UpdateInstall,
    /// Settings > Cookies and the site popover (v0.4.2).
    Cookies(crate::cookies_api::CookieOp),
}

const OPS: &[&str] = &[
    "settings",
    "settings_save",
    "downloads_folder_pick",
    "downloads_folder_reset",
    "site_data",
    "site_data_clear",
    "update_status",
    "update_check",
    "update_install",
];

pub fn is_settings_op(op: &str) -> bool {
    OPS.contains(&op) || crate::cookies_api::is_cookie_op(op)
}

impl SettingsOp {
    pub fn parse(op: &str, param: &dyn Fn(&str) -> Option<String>) -> Result<SettingsOp, ApiError> {
        let flag = |name: &'static str| -> Result<Option<bool>, ApiError> {
            match param(name).as_deref() {
                None => Ok(None),
                Some("1") => Ok(Some(true)),
                Some("0") => Ok(Some(false)),
                Some(_) => Err(ApiError::BadParam(name)),
            }
        };
        Ok(match op {
            "settings" => SettingsOp::Get,
            "settings_save" => {
                let search_engine = match param("search_engine") {
                    None => None,
                    Some(v) => {
                        Some(SearchEngine::parse(&v).ok_or(ApiError::BadParam("search_engine"))?)
                    }
                };
                let theme = match param("theme") {
                    None => None,
                    Some(v) => Some(Theme::parse(&v).ok_or(ApiError::BadParam("theme"))?),
                };
                let ask_save = flag("ask_save")?;
                let update_auto = flag("update_auto")?;
                if search_engine.is_none()
                    && theme.is_none()
                    && ask_save.is_none()
                    && update_auto.is_none()
                {
                    return Err(ApiError::MissingParam("search_engine"));
                }
                SettingsOp::Save {
                    search_engine,
                    ask_save,
                    update_auto,
                    theme,
                }
            }
            "downloads_folder_pick" => SettingsOp::DownloadsFolderPick,
            "downloads_folder_reset" => SettingsOp::DownloadsFolderReset,
            "site_data" => SettingsOp::SiteData,
            "site_data_clear" => {
                let raw = param("what").ok_or(ApiError::MissingParam("what"))?;
                let mut what = ClearWhat::default();
                for w in raw.split(',').map(str::trim).filter(|w| !w.is_empty()) {
                    match w {
                        "cookies" => what.cookies = true,
                        "storage" => what.storage = true,
                        "cache" => what.cache = true,
                        _ => return Err(ApiError::BadParam("what")),
                    }
                }
                if what == ClearWhat::default() {
                    return Err(ApiError::BadParam("what"));
                }
                let site = match param("site") {
                    None => None,
                    Some(s) => {
                        let s = s.trim().to_ascii_lowercase();
                        if !valid_site(&s) {
                            return Err(ApiError::BadParam("site"));
                        }
                        Some(s)
                    }
                };
                SettingsOp::SiteDataClear { what, site }
            }
            "update_status" => SettingsOp::UpdateStatus,
            "update_check" => SettingsOp::UpdateCheck,
            "update_install" => SettingsOp::UpdateInstall,
            op if crate::cookies_api::is_cookie_op(op) => {
                SettingsOp::Cookies(crate::cookies_api::CookieOp::parse(op, param)?)
            }
            other => return Err(ApiError::UnknownOp(other.to_owned())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(op: &str, q: &[(&str, &str)]) -> Result<SettingsOp, ApiError> {
        let m: HashMap<String, String> = q
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        SettingsOp::parse(op, &|k| m.get(k).cloned())
    }

    #[test]
    fn ops() {
        for op in OPS {
            assert!(is_settings_op(op));
        }
        assert!(!is_settings_op("prefs"));
        assert_eq!(parse("settings", &[]), Ok(SettingsOp::Get));
        assert_eq!(parse("update_check", &[]), Ok(SettingsOp::UpdateCheck));
        assert!(is_settings_op("cookies_list"));
        assert_eq!(
            parse("cookie_site", &[]),
            Ok(SettingsOp::Cookies(
                crate::cookies_api::CookieOp::ActiveSite
            ))
        );
        assert!(matches!(parse("nope", &[]), Err(ApiError::UnknownOp(_))));
    }

    #[test]
    fn save() {
        assert_eq!(
            parse(
                "settings_save",
                &[("search_engine", "google"), ("ask_save", "1")]
            ),
            Ok(SettingsOp::Save {
                search_engine: Some(SearchEngine::Google),
                ask_save: Some(true),
                update_auto: None,
                theme: None,
            })
        );
        assert_eq!(
            parse("settings_save", &[("theme", "dark"), ("update_auto", "0")]),
            Ok(SettingsOp::Save {
                search_engine: None,
                ask_save: None,
                update_auto: Some(false),
                theme: Some(Theme::Dark),
            })
        );
        assert!(parse("settings_save", &[]).is_err());
        assert!(parse("settings_save", &[("search_engine", "yahoo")]).is_err());
        assert!(parse("settings_save", &[("ask_save", "yes")]).is_err());
        assert!(parse("settings_save", &[("theme", "blue")]).is_err());
    }

    #[test]
    fn clear() {
        assert_eq!(
            parse("site_data_clear", &[("what", "cookies,storage,cache")]),
            Ok(SettingsOp::SiteDataClear {
                what: ClearWhat {
                    cookies: true,
                    storage: true,
                    cache: true
                },
                site: None
            })
        );
        assert_eq!(
            parse(
                "site_data_clear",
                &[("what", "cookies"), ("site", " Example.co.UK ")]
            ),
            Ok(SettingsOp::SiteDataClear {
                what: ClearWhat {
                    cookies: true,
                    ..Default::default()
                },
                site: Some("example.co.uk".into())
            })
        );
        for bad in ["", "cookies,history", ","] {
            assert!(
                parse("site_data_clear", &[("what", bad)]).is_err(),
                "{bad:?}"
            );
        }
        assert!(parse("site_data_clear", &[]).is_err());
        for site in ["", "a..b", "a b.com", "-", "x/y.com", &"a".repeat(300)] {
            if site == "-" {
                continue; // "-" is a valid (odd) label; Servo simply has no such site
            }
            assert!(
                parse("site_data_clear", &[("what", "cookies"), ("site", site)]).is_err(),
                "{site:?}"
            );
        }
    }
}
