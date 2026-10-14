// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Settings > Appearance, kept in the profile's `prefs` table: the Transparency slider
//! (0–100) and the Reduce transparency switch (on / off, or absent = follow the OS).
//! Local only (not synced: how see-through the glass is depends on the screen and the room).

use crate::{Result, Store};

const PREF_TRANSPARENCY: &str = "ui_transparency";
const PREF_REDUCE: &str = "ui_reduce_transparency";

impl Store {
    /// The saved Transparency slider (0–100); `None` if never set (or the stored value is
    /// not a number in range, e.g. written by a future version: ignored, not trusted).
    pub fn transparency(&self) -> Result<Option<u8>> {
        Ok(self
            .pref(PREF_TRANSPARENCY)?
            .and_then(|v| v.trim().parse::<u8>().ok())
            .filter(|v| *v <= 100))
    }

    /// Save the slider; values above 100 are clamped.
    pub fn set_transparency(&self, value: u8) -> Result<()> {
        self.set_pref(PREF_TRANSPARENCY, &value.min(100).to_string())
    }

    /// The saved Reduce transparency switch: `Some(on)` when the user set it, `None` to
    /// follow the OS setting (also for an unreadable value).
    pub fn reduce_transparency(&self) -> Result<Option<bool>> {
        Ok(match self.pref(PREF_REDUCE)?.as_deref() {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        })
    }

    /// Set the switch, or `None` to follow the OS setting again.
    pub fn set_reduce_transparency(&self, value: Option<bool>) -> Result<()> {
        match value {
            Some(on) => self.set_pref(PREF_REDUCE, if on { "1" } else { "0" }),
            None => {
                self.conn
                    .execute("DELETE FROM prefs WHERE key = ?1", [PREF_REDUCE])?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_round_trips_and_survives_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "mb-appearance-{}-{}",
            std::process::id(),
            crate::unix_now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let s = Store::open_in_dir(&dir).unwrap();
            assert_eq!(s.transparency().unwrap(), None);
            assert_eq!(s.reduce_transparency().unwrap(), None);
            s.set_transparency(73).unwrap();
            s.set_reduce_transparency(Some(true)).unwrap();
        }
        {
            // A new handle on the same profile (the next start) reads it back.
            let s = Store::open_in_dir(&dir).unwrap();
            assert_eq!(s.transparency().unwrap(), Some(73));
            assert_eq!(s.reduce_transparency().unwrap(), Some(true));
            s.set_reduce_transparency(Some(false)).unwrap();
            assert_eq!(s.reduce_transparency().unwrap(), Some(false));
            s.set_reduce_transparency(None).unwrap();
            assert_eq!(s.reduce_transparency().unwrap(), None);
            s.set_transparency(250).unwrap();
            assert_eq!(s.transparency().unwrap(), Some(100), "clamped");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bad_stored_values_are_ignored() {
        let s = Store::open_in_memory().unwrap();
        s.set_pref(PREF_TRANSPARENCY, "101").unwrap();
        assert_eq!(s.transparency().unwrap(), None);
        s.set_pref(PREF_TRANSPARENCY, "-3").unwrap();
        assert_eq!(s.transparency().unwrap(), None);
        s.set_pref(PREF_TRANSPARENCY, "abc").unwrap();
        assert_eq!(s.transparency().unwrap(), None);
        s.set_pref(PREF_TRANSPARENCY, "0").unwrap();
        assert_eq!(s.transparency().unwrap(), Some(0));
        s.set_pref(PREF_REDUCE, "yes").unwrap();
        assert_eq!(s.reduce_transparency().unwrap(), None);
    }
}
