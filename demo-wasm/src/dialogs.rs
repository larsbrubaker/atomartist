//! File-dialog hooks for the web shell — **temporary, Phase 5b only**.
//!
//! The native shell answers these with `rfd`; the browser has no equivalent
//! for the `browser:` scheme, and the in-app file browser that will answer
//! them properly lands in Phase 6 of `docs/storage-architecture-plan.md`.
//! Until then this exists so Ctrl+S actually persists something: the save
//! picker returns a fixed location under the origin's private file system
//! instead of `None` (which is what `NoFileDialogs` returns, and why saving
//! on the web did nothing at all).
//!
//! Everything here is deliberately crude and clearly marked so it is not
//! mistaken for the real thing:
//!
//! - **Save** goes to `browser:///projects/<name>`; the parent directory is
//!   created by the write, so a first-ever save works.
//! - **Open / Import / Export** still return `None` — picking an *existing*
//!   location needs the listing UI, and inventing a default would open the
//!   wrong file.
//! - **Unsaved-changes** answers `Cancel`. There is no modal to ask with (a
//!   modal is a widget, and this shell has none yet), and now that Ctrl+S
//!   really persists, answering `Discard` would silently destroy real work
//!   the moment someone picked File → New. `Cancel` makes New / Open look
//!   inert instead — the same shape as `pick_open_project` returning `None`
//!   — which is recoverable, where discarding is not.
//! - **Errors and notices** go to the browser console.

use atomartist_storage::{StorageUri, BROWSER_SCHEME};
use atomartist_ui::top_menu_bar::{FileDialogProvider, UnsavedChoice};
use wasm_bindgen::JsValue;

/// Directory every web save lands in until the file browser exists.
const DEFAULT_SAVE_DIR: &str = "/projects";

/// Name used when the caller has no better suggestion.
const DEFAULT_SAVE_NAME: &str = "untitled.atmr";

pub struct WebDialogs;

impl WebDialogs {
    /// `browser:///projects/<name>`, falling back to the fixed default when
    /// `name` has no URI form (it reaches us from a file name, which may
    /// contain anything).
    fn save_target(name: &str) -> StorageUri {
        let dir = StorageUri::new(BROWSER_SCHEME, DEFAULT_SAVE_DIR);
        dir.try_join(name)
            .unwrap_or_else(|_| dir.join(DEFAULT_SAVE_NAME))
    }
}

impl FileDialogProvider for WebDialogs {
    fn pick_open_project(&self) -> Option<StorageUri> {
        None
    }

    fn pick_save_project(&self, default_name: &str) -> Option<StorageUri> {
        let name = if default_name.is_empty() {
            DEFAULT_SAVE_NAME
        } else {
            default_name
        };
        Some(WebDialogs::save_target(name))
    }

    fn pick_save_export(&self, _extension: &str, _default_name: &str) -> Option<StorageUri> {
        None
    }

    fn pick_import_file(&self) -> Option<StorageUri> {
        None
    }

    /// Without a modal there is no way to ask, and guessing `Discard` would
    /// throw away unsaved work that Ctrl+S could have kept.
    fn confirm_unsaved_changes(&self) -> UnsavedChoice {
        UnsavedChoice::Cancel
    }

    fn show_error(&self, message: &str) {
        web_sys::console::error_1(&JsValue::from_str(message));
    }

    fn show_info(&self, title: &str, message: &str) {
        web_sys::console::log_1(&JsValue::from_str(&format!("{title}: {message}")));
    }
}
