//! Settings + window-geometry persistence for the native shell.
//!
//! Split out of `main.rs` (which is at the 800-line cap) so the event
//! loop keeps only event handling. Everything here answers one of two
//! questions: *where does the settings file live*, and *what exactly do
//! we write into it*. `main.rs` calls these from three places — the
//! per-frame `AutoSave` tick, the immediate window close, and the
//! deferred close that waits for an in-flight save to land.

use std::path::PathBuf;

use atomartist_ui::{AppState, DebugWindowHandles, MainWindowState};
use winit::dpi::PhysicalPosition;
use winit::window::Window;

/// Compose the persistence blob from its three owners — HUD state on
/// `AppState`, floating-window layout on the debug handles, and the
/// window's last non-maximized bounds plus its current maximized flag.
/// None of them owns the others, so the shell stitches them together.
pub fn compose_settings_blob(
    state: &AppState,
    debug: &DebugWindowHandles,
    normal_bounds: &std::rc::Rc<std::cell::Cell<MainWindowState>>,
    window: &Window,
) -> String {
    let mut s = state.ui_settings();
    s.debug_windows = debug.current_state();
    let mut main = normal_bounds.get();
    main.maximized = window.is_maximized();
    s.main_window = main;
    s.to_text()
}

/// Write the settings blob, creating its directory. Used by the two
/// shutdown paths (immediate close, and the deferred close that waits on
/// a save); the per-frame `AutoSave` has its own diff-guarded writer.
pub fn write_settings_blob(path: &std::path::Path, blob: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, blob) {
        eprintln!(
            "warning: failed to save UI settings to {}: {}",
            path.display(),
            e
        );
    }
}

/// Cast a winit `MonitorHandle` to a plain `(x, y, w, h)` rect in
/// physical pixels — the shape `MainWindowState::fits_on_monitors`
/// expects so the validation helper stays winit-agnostic.
pub fn monitor_to_rect(m: winit::monitor::MonitorHandle) -> (i32, i32, u32, u32) {
    let pos = m.position();
    let size = m.size();
    (pos.x, pos.y, size.width, size.height)
}

/// Read the current outer position + inner size + maximized state
/// from the live `Window`. Used at startup to seed the
/// "last normal bounds" cache and during persistence to capture
/// the current maximized flag.
pub fn current_main_window_state(window: &Window) -> MainWindowState {
    let pos = window.outer_position().unwrap_or(PhysicalPosition::new(0, 0));
    let size = window.inner_size();
    MainWindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized(),
    }
}

/// Pick the initial value for the live "last normal bounds" cache.
///
/// We always prefer the saved bounds (with the recentered position
/// if the saved one was off-screen now) over reading the live
/// window — when the shell has just called `set_maximized(true)`,
/// `outer_position()` and `inner_size()` report the maximized
/// monitor-fill geometry, which is exactly the wrong thing to seed
/// the "last non-maximized bounds" cache with. Falling back to the
/// live window is reserved for the genuine first-launch case where
/// no saved bounds exist at all.
pub fn initial_normal_bounds(window: &Window, saved: Option<MainWindowState>) -> MainWindowState {
    match saved {
        Some(s) if s.has_valid_geometry() => MainWindowState {
            maximized: window.is_maximized(),
            ..s
        },
        _ => current_main_window_state(window),
    }
}

/// Cross-platform "user config dir" for AtomArtist's settings file.
/// We avoid the `dirs` crate dependency by reading the well-known
/// environment variables directly:
///   - Windows: `%APPDATA%\atomartist\settings.txt`
///   - macOS: `$HOME/Library/Application Support/atomartist/settings.txt`
///   - Linux / BSD: `$XDG_CONFIG_HOME/atomartist/settings.txt`
///     or `$HOME/.config/atomartist/settings.txt`
pub fn settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("atomartist").join("settings.txt"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|p| {
            p.join("Library")
                .join("Application Support")
                .join("atomartist")
                .join("settings.txt")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|p| p.join("atomartist").join("settings.txt"))
    }
}
