//! winit → agg-gui input translation for the native shell.
//!
//! Pure, stateless helpers lifted out of `main.rs` so that file stays
//! under the 800-line cap: winit's mouse-button and key enums mapped onto
//! agg-gui's, plus the one platform query the file-drop path needs.
//!
//! Nothing here touches the `App` or the event loop — `main.rs` owns all
//! of that. Adding a key mapping is a change to this file alone.

use agg_gui::{Key, MouseButton};
use winit::window::Window;

pub fn translate_winit_button(b: winit::event::MouseButton) -> Option<MouseButton> {
    use winit::event::MouseButton as W;
    match b {
        W::Left => Some(MouseButton::Left),
        W::Middle => Some(MouseButton::Middle),
        W::Right => Some(MouseButton::Right),
        W::Other(n) => Some(MouseButton::Other(n as u8)),
        _ => None,
    }
}

pub fn translate_winit_key(key: &winit::keyboard::Key) -> Option<Key> {
    use winit::keyboard::{Key as W, NamedKey};
    match key {
        W::Character(s) => s.chars().next().map(Key::Char),
        W::Named(n) => match n {
            NamedKey::Backspace => Some(Key::Backspace),
            NamedKey::Delete => Some(Key::Delete),
            NamedKey::Insert => Some(Key::Insert),
            NamedKey::ArrowLeft => Some(Key::ArrowLeft),
            NamedKey::ArrowRight => Some(Key::ArrowRight),
            NamedKey::ArrowUp => Some(Key::ArrowUp),
            NamedKey::ArrowDown => Some(Key::ArrowDown),
            NamedKey::Home => Some(Key::Home),
            NamedKey::End => Some(Key::End),
            NamedKey::Tab => Some(Key::Tab),
            NamedKey::Enter => Some(Key::Enter),
            NamedKey::Escape => Some(Key::Escape),
            NamedKey::Space => Some(Key::Char(' ')),
            _ => None,
        },
        _ => None,
    }
}

/// Physical-pixel cursor position relative to the window's client
/// area, queried live from the OS. Used by the file-drop handler
/// because winit's tracked cursor is stale during an OLE drag (see
/// the DroppedFile arm). Returns `None` off-Windows or when the
/// window position is unavailable — callers fall back to the last
/// tracked cursor position.
#[cfg(target_os = "windows")]
pub fn live_cursor_in_window(window: &Window) -> Option<(f64, f64)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos writes into the POINT we own; no other
    // preconditions.
    if unsafe { GetCursorPos(&mut pt) } == 0 {
        return None;
    }
    let client_origin = window.inner_position().ok()?;
    Some((
        (pt.x - client_origin.x) as f64,
        (pt.y - client_origin.y) as f64,
    ))
}

#[cfg(not(target_os = "windows"))]
pub fn live_cursor_in_window(_window: &Window) -> Option<(f64, f64)> {
    None
}
