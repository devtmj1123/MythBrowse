// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/.
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! "Ask the user first": the generic request shown on the approval card (Allow / Don't allow,
//! Esc = don't allow, Enter never allows by default).
//!
//! The browser uses it for page dialogs and permission prompts that come up while an
//! assistant run is active, and an [`crate::AgentProvider`] with browser control uses it for
//! every consequential step. Which steps need an OK is the provider's decision; this module
//! only defines the request and the answer channel.

/// One request for the user's OK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// What asks (a tool name, `page_dialog`, `download`, ...), for logs.
    pub tool: String,
    pub tab: Option<u64>,
    /// What will happen, e.g. `Open the page's confirmation dialog`.
    pub action: String,
    /// Why it needs an OK.
    pub reason: String,
    /// The page it happens on.
    pub url: String,
    /// For a navigation: the full destination URL (the card shows it whole).
    pub destination: Option<String>,
}

/// Something that can ask the user.
pub trait Approver {
    /// Block until the user allows (`true`) or declines (`false`). Implementations must
    /// answer `false` on timeout, cancellation or when they cannot ask.
    fn approve(&self, request: &ApprovalRequest) -> bool;
}

/// Approves everything (tests only).
pub struct AllowAll;

impl Approver for AllowAll {
    fn approve(&self, _: &ApprovalRequest) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DenyAll;
    impl Approver for DenyAll {
        fn approve(&self, _: &ApprovalRequest) -> bool {
            false
        }
    }

    #[test]
    fn approvers_answer() {
        let r = ApprovalRequest {
            tool: "page_dialog".into(),
            tab: None,
            action: "Answer the page's question".into(),
            reason: "the page asked".into(),
            url: "https://example.com/".into(),
            destination: None,
        };
        assert!(AllowAll.approve(&r));
        assert!(!DenyAll.approve(&r));
    }
}
