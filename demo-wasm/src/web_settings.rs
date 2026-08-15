//! Settings persistence for the WASM shell — the web mirror of
//! `demo-native::shell_settings`.
//!
//! Same two questions that module answers, with a browser backend:
//! *where do the settings live* (a single `localStorage` entry under
//! [`SETTINGS_KEY`]) and *what exactly goes into it* (the very same
//! `UiSettings` text blob the native shell writes to disk, produced by
//! [`atomartist_ui::UiSettings::to_text`] — one format, both shells).
//!
//! Why `localStorage` and not OPFS: settings are shell-owned
//! *application configuration*, not project data. The native shell keeps
//! them outside project storage too (`%APPDATA%\atomartist\settings.txt`,
//! never under a `file:` provider), so the browser's project storage —
//! `BrowserProvider` / OPFS under the `browser:` scheme — stays reserved
//! for projects. `localStorage` is also synchronous, which lets the
//! per-frame [`AutoSave`](agg_gui::persistence::AutoSave) tick in
//! `lib.rs::render` write without an async round trip.
//!
//! Every operation degrades to defaults rather than failing: a browser
//! with storage disabled (private mode, blocked cookies), an absent key,
//! and a corrupted value all produce [`UiSettings::default`] on load and
//! a silently-logged no-op on save. Settings must never block startup.

use atomartist_ui::{AppState, DebugWindowHandles, UiSettings};

/// `localStorage` key holding the settings blob. Namespaced so the app
/// can add sibling keys later without collision.
pub const SETTINGS_KEY: &str = "atomartist.settings";

/// The origin's `localStorage`, or `None` when the browser denies it
/// (private mode / blocked storage) — in which case the shell runs with
/// defaults and simply never persists.
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

/// Raw stored blob, if any. Exposed so startup can seed `AutoSave` with
/// exactly what is already stored and avoid an identical first write.
pub fn read_settings_blob() -> Option<String> {
    local_storage().and_then(|s| s.get_item(SETTINGS_KEY).ok().flatten())
}

/// Parse a stored blob into settings. Pure — no browser involved — so
/// the absent / corrupted / valid cases are all directly testable.
///
/// `UiSettings::from_text` is deliberately forgiving (unknown keys
/// skipped, malformed values fall back per-field), so a value written by
/// an older build, or one somebody hand-edited in devtools, degrades to
/// defaults instead of blocking startup.
pub fn settings_from_stored(raw: Option<&str>) -> UiSettings {
    match raw {
        Some(text) => UiSettings::from_text(text),
        None => UiSettings::default(),
    }
}

/// Load persisted settings, falling back to defaults.
pub fn load_settings() -> UiSettings {
    settings_from_stored(read_settings_blob().as_deref())
}

/// Compose the persistence blob from its owners — HUD state on
/// `AppState`, floating-window layout on the debug handles.
///
/// `main_window` is left at [`atomartist_ui::MainWindowState::default`]
/// (the "no saved geometry" sentinel `AppState::ui_settings` already
/// supplies): the browser owns the window, so there is nothing for the
/// shell to restore. The field is stored anyway rather than forked out
/// of the struct — a zero-area sentinel is exactly what a native build
/// reading the same blob would want.
pub fn compose_settings_blob(state: &AppState, debug: &DebugWindowHandles) -> String {
    let mut s = state.ui_settings();
    s.debug_windows = debug.current_state();
    s.to_text()
}

/// Write the settings blob to `localStorage`. Failures (quota exceeded,
/// storage disabled mid-session) are logged and swallowed — losing
/// settings must never take the app down.
pub fn write_settings_blob(blob: &str) {
    let Some(storage) = local_storage() else {
        return;
    };
    if storage.set_item(SETTINGS_KEY, blob).is_err() {
        web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(
            "warning: failed to save UI settings to localStorage",
        ));
    }
}
