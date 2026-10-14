// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/.
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! "Is this really the signed-in user?" before showing, editing or exporting saved passwords.
//!
//! The open-source build has no OS credential prompt yet: [`verify_user`] answers
//! [`Verdict::Unavailable`] and the Passwords page falls back to its in-app confirmation
//! (the same path every platform without OS verification uses). Official builds verify the
//! current Windows account with the system credential prompt.

/// The outcome of [`verify_user`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Verified,
    /// Wrong password, or the user cancelled.
    Denied,
    /// No OS verification available here; fall back to an in-app confirmation.
    Unavailable,
}

/// Ask the OS to verify the current user. `message` would be shown in the dialog.
pub fn verify_user(_message: &str) -> Verdict {
    Verdict::Unavailable
}

#[cfg(test)]
mod tests {
    #[test]
    fn open_build_falls_back_to_in_app_confirmation() {
        assert_eq!(super::verify_user("x"), super::Verdict::Unavailable);
    }
}
