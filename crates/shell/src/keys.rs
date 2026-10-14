// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! winit → DOM input mapping.

use mythbrowse_engine::servo::{Cursor, Key, Modifiers, MouseButton, NamedKey};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey as WNamedKey};
use winit::window::CursorIcon;

pub fn to_servo_modifiers(m: ModifiersState) -> Modifiers {
    let mut out = Modifiers::empty();
    out.set(Modifiers::SHIFT, m.shift_key());
    out.set(Modifiers::CONTROL, m.control_key());
    out.set(Modifiers::ALT, m.alt_key());
    out.set(Modifiers::META, m.super_key());
    out
}

pub fn to_servo_button(button: winit::event::MouseButton) -> MouseButton {
    match button {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        winit::event::MouseButton::Back => MouseButton::Back,
        winit::event::MouseButton::Forward => MouseButton::Forward,
        winit::event::MouseButton::Other(n) => MouseButton::Other(n),
    }
}

/// winit → DOM key mapping (typing, editing and navigation keys).
/// TODO: physical `Code`s (see servoshell keyutils).
pub fn to_servo_key(key: &WKey) -> Option<Key> {
    Some(match key {
        WKey::Character(c) => Key::Character(c.to_string()),
        WKey::Named(named) => Key::Named(match named {
            WNamedKey::Space => return Some(Key::Character(" ".into())),
            WNamedKey::Enter => NamedKey::Enter,
            WNamedKey::Backspace => NamedKey::Backspace,
            WNamedKey::Delete => NamedKey::Delete,
            WNamedKey::Insert => NamedKey::Insert,
            WNamedKey::Tab => NamedKey::Tab,
            WNamedKey::Escape => NamedKey::Escape,
            WNamedKey::ArrowLeft => NamedKey::ArrowLeft,
            WNamedKey::ArrowRight => NamedKey::ArrowRight,
            WNamedKey::ArrowUp => NamedKey::ArrowUp,
            WNamedKey::ArrowDown => NamedKey::ArrowDown,
            WNamedKey::Home => NamedKey::Home,
            WNamedKey::End => NamedKey::End,
            WNamedKey::PageUp => NamedKey::PageUp,
            WNamedKey::PageDown => NamedKey::PageDown,
            WNamedKey::Shift => NamedKey::Shift,
            WNamedKey::Control => NamedKey::Control,
            WNamedKey::Alt => NamedKey::Alt,
            WNamedKey::Super => NamedKey::Meta,
            WNamedKey::CapsLock => NamedKey::CapsLock,
            WNamedKey::ContextMenu => NamedKey::ContextMenu,
            WNamedKey::F1 => NamedKey::F1,
            WNamedKey::F2 => NamedKey::F2,
            WNamedKey::F3 => NamedKey::F3,
            WNamedKey::F4 => NamedKey::F4,
            WNamedKey::F7 => NamedKey::F7,
            WNamedKey::F8 => NamedKey::F8,
            WNamedKey::F9 => NamedKey::F9,
            WNamedKey::F10 => NamedKey::F10,
            WNamedKey::F11 => NamedKey::F11,
            WNamedKey::F12 => NamedKey::F12,
            _ => return None,
        }),
        _ => return None,
    })
}

pub fn to_cursor_icon(cursor: Cursor) -> CursorIcon {
    match cursor {
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Help => CursorIcon::Help,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
        Cursor::None | Cursor::Default => CursorIcon::Default,
    }
}
