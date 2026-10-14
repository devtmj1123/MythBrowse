// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Site settings: what the user decided for a site ("Remember for this site" on a permission
//! sheet, "Always allow pop-ups", or the Site settings page), kept per **origin** and
//! **feature** in the profile's `site_permissions` table, plus the search-suggestions switch
//! (prefs table). Local only (not synced).

use rusqlite::params;

use crate::{Result, Store};

/// v4 -> v5: remembered permission / pop-up decisions per origin (local only, not synced).
pub(crate) const SCHEMA_V5: &str = "
CREATE TABLE IF NOT EXISTS site_permissions (
    origin     TEXT NOT NULL,
    feature    TEXT NOT NULL,
    decision   TEXT NOT NULL,
    updated_ms INTEGER NOT NULL,
    PRIMARY KEY (origin, feature)
) WITHOUT ROWID;
";

const PREF_SUGGEST: &str = "omnibox_search_suggestions";

/// Features a site can be allowed or blocked. The names are what the UI and the table use.
pub const FEATURES: &[&str] = &[
    "geolocation",
    "notifications",
    "camera",
    "microphone",
    "popups",
    "midi",
    "bluetooth",
    "persistent-storage",
    "background-sync",
    "speaker",
    "device-info",
    "push",
];

/// A remembered decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Block => "block",
        }
    }

    pub fn parse(s: &str) -> Option<Decision> {
        match s {
            "allow" => Some(Decision::Allow),
            "block" => Some(Decision::Block),
            _ => None,
        }
    }
}

/// One row of the Site settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePermission {
    pub origin: String,
    pub feature: String,
    pub decision: Decision,
    pub updated_ms: i64,
}

/// The origin key for a URL: `scheme://host[:port]` for http(s); `None` for anything else
/// (no site settings for `file:`, `data:`, internal pages...).
pub fn origin_key(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    Some(match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    })
}

fn valid(origin: &str, feature: &str) -> bool {
    FEATURES.contains(&feature)
        && origin_key(origin).as_deref() == Some(origin)
        && origin.len() <= 300
}

impl Store {
    /// The decision for `origin` + `feature`, if the user made one.
    pub fn site_permission(&self, origin: &str, feature: &str) -> Result<Option<Decision>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT decision FROM site_permissions WHERE origin = ?1 AND feature = ?2",
        )?;
        let mut rows = stmt.query(params![origin, feature])?;
        Ok(match rows.next()? {
            Some(r) => Decision::parse(&r.get::<_, String>(0)?),
            None => None,
        })
    }

    /// Remember (`Some`) or forget (`None` = ask again) a decision. Unknown features and
    /// malformed origins are ignored (returns false).
    pub fn set_site_permission(
        &self,
        origin: &str,
        feature: &str,
        decision: Option<Decision>,
        at_ms: i64,
    ) -> Result<bool> {
        if !valid(origin, feature) {
            return Ok(false);
        }
        match decision {
            Some(d) => {
                self.conn.execute(
                    "INSERT INTO site_permissions (origin, feature, decision, updated_ms)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(origin, feature) DO UPDATE SET
                        decision = excluded.decision, updated_ms = excluded.updated_ms",
                    params![origin, feature, d.as_str(), at_ms],
                )?;
            }
            None => {
                self.conn.execute(
                    "DELETE FROM site_permissions WHERE origin = ?1 AND feature = ?2",
                    params![origin, feature],
                )?;
            }
        }
        Ok(true)
    }

    /// Every remembered decision, by origin then feature.
    pub fn site_permissions(&self) -> Result<Vec<SitePermission>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT origin, feature, decision, updated_ms FROM site_permissions
             ORDER BY origin, feature",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (origin, feature, decision, updated_ms) = row?;
            if let Some(decision) = Decision::parse(&decision) {
                out.push(SitePermission {
                    origin,
                    feature,
                    decision,
                    updated_ms,
                });
            }
        }
        Ok(out)
    }

    /// Forget everything remembered for `origin` ("Reset" on the Site settings page).
    pub fn clear_site(&self, origin: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM site_permissions WHERE origin = ?1", [origin])?)
    }

    /// Search suggestions in the address bar (sent to DuckDuckGo as you type). On unless
    /// the user turned them off.
    pub fn search_suggestions(&self) -> Result<bool> {
        Ok(self.pref(PREF_SUGGEST)?.as_deref() != Some("0"))
    }

    pub fn set_search_suggestions(&self, on: bool) -> Result<()> {
        self.set_pref(PREF_SUGGEST, if on { "1" } else { "0" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_key_normalises_http_only() {
        assert_eq!(
            origin_key("https://Maps.Example.com/a?b").as_deref(),
            Some("https://maps.example.com")
        );
        assert_eq!(
            origin_key("http://localhost:8080/x").as_deref(),
            Some("http://localhost:8080")
        );
        assert_eq!(
            origin_key("https://example.com:443/").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(origin_key("file:///c:/x"), None);
        assert_eq!(origin_key("data:text/html,hi"), None);
        assert_eq!(origin_key("not a url"), None);
    }

    #[test]
    fn permissions_round_trip_update_and_forget() {
        let s = Store::open_in_memory().unwrap();
        let o = "https://maps.example.com";
        assert_eq!(s.site_permission(o, "geolocation").unwrap(), None);
        assert!(
            s.set_site_permission(o, "geolocation", Some(Decision::Allow), 1)
                .unwrap()
        );
        assert!(
            s.set_site_permission(o, "notifications", Some(Decision::Block), 2)
                .unwrap()
        );
        assert_eq!(
            s.site_permission(o, "geolocation").unwrap(),
            Some(Decision::Allow)
        );
        // Update in place.
        assert!(
            s.set_site_permission(o, "geolocation", Some(Decision::Block), 3)
                .unwrap()
        );
        assert_eq!(
            s.site_permission(o, "geolocation").unwrap(),
            Some(Decision::Block)
        );
        let all = s.site_permissions().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].feature, "geolocation");
        assert_eq!(all[0].updated_ms, 3);
        // Forget one, then the whole site.
        assert!(s.set_site_permission(o, "geolocation", None, 4).unwrap());
        assert_eq!(s.site_permission(o, "geolocation").unwrap(), None);
        assert_eq!(s.clear_site(o).unwrap(), 1);
        assert!(s.site_permissions().unwrap().is_empty());
    }

    #[test]
    fn rejects_unknown_features_and_bad_origins() {
        let s = Store::open_in_memory().unwrap();
        assert!(
            !s.set_site_permission("https://a.example", "root-access", Some(Decision::Allow), 1)
                .unwrap()
        );
        assert!(
            !s.set_site_permission("https://a.example/path", "camera", Some(Decision::Allow), 1)
                .unwrap()
        );
        assert!(
            !s.set_site_permission("file:///x", "camera", Some(Decision::Allow), 1)
                .unwrap()
        );
        assert!(s.site_permissions().unwrap().is_empty());
    }

    #[test]
    fn suggestions_switch_defaults_on() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.search_suggestions().unwrap());
        s.set_search_suggestions(false).unwrap();
        assert!(!s.search_suggestions().unwrap());
        s.set_search_suggestions(true).unwrap();
        assert!(s.search_suggestions().unwrap());
    }
}
