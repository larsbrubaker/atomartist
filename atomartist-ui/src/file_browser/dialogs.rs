//! [`ModalFileDialogs`] — the [`FileDialogProvider`] every pick goes
//! through when the shell has no OS dialog to offer (step 6c-2 of
//! `docs/file-browser-design.md`).
//!
//! It is the whole of the web shell's file-dialog story — it replaced
//! `demo-wasm`'s `WebDialogs` placeholder, which saved to one fixed
//! `browser:///projects/…` location and could not open anything at all —
//! and it is what a native build gains the moment it registers a provider
//! `rfd` cannot address (`demo-native`'s `NativeDialogs` documents that
//! hand-off).
//!
//! # Everything here is a thin wrapper over the handle
//!
//! [`FileBrowserModalHandle::open`] already returns exactly the
//! `Job<Option<StorageUri>>` the trait wants, so each picker is one call.
//! The interesting decisions are the three the handle does *not* answer:
//!
//! - **Export needs its own extension.** A save-mode pick forces the
//!   caller's extension onto the typed name, and the plain `open` forces
//!   `.atmr`. `pick_save_export` therefore goes through
//!   [`FileBrowserModalHandle::open_with_extension`] so "part" typed into
//!   an STL export comes back as `part.stl` and not `part.atmr`.
//! - **Import is an Open pick.** Filtering the listing down to importable
//!   formats is deliberately not done yet (design §5, 6d polish): the
//!   browser shows everything, and an unsupported file is reported by the
//!   import path itself, which already refuses unknown extensions.
//! - **There is no confirm modal yet**, so
//!   [`confirm_unsaved_changes`](FileDialogProvider::confirm_unsaved_changes)
//!   answers [`UnsavedChoice::Cancel`] — see its doc comment.
//!
//! # Where messages go
//!
//! [`AppState`] is `!Send` and this type must be `Send + Sync` (the trait
//! is), so `ModalFileDialogs` cannot hold one. It holds the state's
//! *notice queue* instead — which is `Arc<Mutex<…>>`, is the same queue
//! every storage continuation posts to, and is drained into the status bar
//! once per frame by `AppState::pump_notices`. So `show_error` and
//! `show_info` reach the user through the status bar rather than a modal.
//! That is honest but lossy for long text (About / License are several
//! lines and the status bar paints one), and it is the reason a real
//! message dialog is on the 6d list.

use atomartist_storage::{Job, StorageUri};

use super::modal::FileBrowserModalHandle;
use super::widget::BrowserMode;
use crate::app_state::AppState;
use crate::storage_ops::{push_notice, NoticeLevel, Notices};
use crate::top_menu_bar::{FileDialogProvider, UnsavedChoice};

/// File dialogs served by the in-app browser modal.
pub struct ModalFileDialogs {
    modal: FileBrowserModalHandle,
    notices: Notices,
}

impl ModalFileDialogs {
    /// Build over the handle the shell also passed to
    /// [`crate::build_app`], and the state whose status bar shows the
    /// messages.
    pub fn new(modal: FileBrowserModalHandle, state: &AppState) -> Self {
        ModalFileDialogs {
            modal,
            notices: state.notice_queue(),
        }
    }
}

impl FileDialogProvider for ModalFileDialogs {
    fn pick_open_project(&self) -> Job<Option<StorageUri>> {
        self.modal.open(BrowserMode::Open, "")
    }

    fn pick_save_project(&self, default_name: &str) -> Job<Option<StorageUri>> {
        self.modal.open(BrowserMode::Save, default_name)
    }

    fn pick_save_export(&self, extension: &str, default_name: &str) -> Job<Option<StorageUri>> {
        self.modal
            .open_with_extension(BrowserMode::Save, default_name, extension)
    }

    fn pick_import_file(&self) -> Job<Option<StorageUri>> {
        self.modal.open(BrowserMode::Open, "")
    }

    /// There is no in-app confirm modal yet (design §5 puts one with the
    /// 6d chrome work), and this is not a question that can be guessed:
    /// answering `Discard` would silently destroy unsaved work the moment
    /// someone picked File → New. `Cancel` makes New / Open look inert
    /// instead — recoverable, where discarding is not. Same reasoning the
    /// retired `WebDialogs` gave, and it stops being needed the moment a
    /// generic confirm sheet exists.
    fn confirm_unsaved_changes(&self) -> UnsavedChoice {
        UnsavedChoice::Cancel
    }

    fn show_error(&self, message: &str) {
        push_notice(&self.notices, NoticeLevel::Error, message);
    }

    /// Long-form text (About, License) is squeezed into the status bar's
    /// single line — see the module docs. Better than the console-only
    /// route it replaces, worse than the dialog 6d will bring.
    fn show_info(&self, title: &str, message: &str) {
        push_notice(
            &self.notices,
            NoticeLevel::Info,
            format!("{title}: {message}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::top_level::fresh_state_with_builtins;

    fn dialogs(state: &AppState) -> (ModalFileDialogs, FileBrowserModalHandle) {
        let handle = FileBrowserModalHandle::new();
        (ModalFileDialogs::new(handle.clone(), state), handle)
    }

    /// Every picker asks the *shared* handle to put a dialog up and hands
    /// back a job that is still pending — nothing can settle before the
    /// widget tree has run a frame.
    #[test]
    fn each_picker_queues_a_dialog_and_returns_a_pending_job() {
        let state = fresh_state_with_builtins();
        type Picker = fn(&ModalFileDialogs) -> Job<Option<StorageUri>>;
        let pickers: [Picker; 4] = [
            |d| d.pick_open_project(),
            |d| d.pick_save_project("untitled.atmr"),
            |d| d.pick_save_export("stl", "part.stl"),
            |d| d.pick_import_file(),
        ];
        for open in pickers {
            let (provider, handle) = dialogs(&state);
            assert!(!handle.is_open());
            let job = open(&provider);
            assert!(handle.is_open(), "the picker put a dialog up");
            assert!(job.poll().is_pending(), "and nothing is settled yet");
        }
    }

    /// Messages land in the queue the status bar drains, at the severity
    /// the caller meant.
    #[test]
    fn messages_reach_the_status_bar_notice_queue() {
        let state = fresh_state_with_builtins();
        let (provider, _handle) = dialogs(&state);

        provider.show_error("Save failed: disk on fire");
        provider.show_info("About", "AtomArtist");

        let drained = state.drain_notices();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].level, NoticeLevel::Error);
        assert!(drained[0].text.contains("disk on fire"));
        assert_eq!(drained[1].level, NoticeLevel::Info);
        assert!(drained[1].text.starts_with("About: "));
    }

    /// No confirm modal exists yet, so the gate refuses rather than
    /// guessing — the one answer that cannot lose work.
    #[test]
    fn the_unsaved_prompt_cancels_rather_than_guessing() {
        let state = fresh_state_with_builtins();
        let (provider, _handle) = dialogs(&state);
        assert_eq!(provider.confirm_unsaved_changes(), UnsavedChoice::Cancel);
    }
}
