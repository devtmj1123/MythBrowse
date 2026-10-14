// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Desktop notifications.
//!
//! * Windows: a WinRT toast (`ToastNotificationManager`). A packaged build (MSIX) has an
//!   identity and uses it; an unpackaged build (MSI / dev) uses [`APP_ID`], which the MSI's
//!   Start-menu shortcut registers (`System.AppUserModel.ID`) and [`init`] sets on the process.
//!   Without a registered id Windows may drop the toast silently; that is logged, never fatal.
//! * Elsewhere: logged only (no desktop integration yet).
//!
//! [`toast`] never blocks the caller: the toast is shown from a short-lived thread.

/// AppUserModelID of unpackaged builds (also in `packaging/windows/mythbrowse.wxs`).
pub const APP_ID: &str = "JustMyth.MythBrowse";

/// A notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub body: String,
    /// Replaces an earlier toast with the same tag (Windows), e.g. one per paused task.
    pub tag: Option<String>,
}

/// Call once at start-up (before the first window): unpackaged Windows builds take
/// [`APP_ID`] as their AppUserModelID, so toasts and the taskbar group under it.
pub fn init() {
    #[cfg(windows)]
    win::set_app_id();
}

/// Show `t` (fire and forget). Returns `false` when it is only logged (not Windows).
pub fn toast(t: Toast) -> bool {
    log::info!("notification: {} \u{2014} {}", t.title, t.body);
    #[cfg(windows)]
    {
        std::thread::Builder::new()
            .name("myth-toast".into())
            .spawn(move || {
                if let Err(e) = win::show(&t) {
                    log::warn!("toast not shown: {e}");
                }
            })
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = t;
        false
    }
}

/// Escape text for the toast XML.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// The toast XML (ToastGeneric: title + one line).
pub fn toast_xml(t: &Toast) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(&t.title),
        xml_escape(&t.body)
    )
}

#[cfg(windows)]
mod win {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::core::{HSTRING, PCWSTR};

    use super::{APP_ID, Toast, toast_xml};

    pub(super) fn set_app_id() {
        let id: Vec<u16> = APP_ID.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `id` is a NUL-terminated UTF-16 string that outlives the call.
        if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr())) } {
            log::warn!("AppUserModelID not set: {e}");
        }
    }

    pub(super) fn show(t: &Toast) -> windows::core::Result<()> {
        // This thread's apartment (a fresh thread: MTA). Already initialised is fine.
        // SAFETY: plain COM/WinRT initialisation of the current thread.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(toast_xml(t)))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        if let Some(tag) = &t.tag {
            toast.SetTag(&HSTRING::from(tag.as_str()))?;
            toast.SetGroup(&HSTRING::from("mythbrowse"))?;
        }
        // Packaged (MSIX): the package identity; unpackaged: our AppUserModelID.
        let notifier = ToastNotificationManager::CreateToastNotifier().or_else(|_| {
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        })?;
        notifier.Show(&toast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_escaped() {
        let t = Toast {
            title: "Verification needed on a&b.example".into(),
            body: "<script>\"x\"</script>\u{7}".into(),
            tag: None,
        };
        let x = toast_xml(&t);
        assert!(x.contains("a&amp;b.example"));
        assert!(x.contains("&lt;script&gt;&quot;x&quot;&lt;/script&gt;"));
        assert!(!x.contains('\u{7}'));
        assert!(x.starts_with("<toast><visual><binding template=\"ToastGeneric\">"));
    }

    #[test]
    fn toast_is_log_only_off_windows() {
        let shown = toast(Toast {
            title: "t".into(),
            body: "b".into(),
            tag: Some("x".into()),
        });
        assert_eq!(shown, cfg!(windows));
        init();
    }
}
