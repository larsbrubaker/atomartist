//! Native file dialogs — `rfd`-backed implementation of
//! `atomartist_ui::top_menu_bar::FileDialogProvider`. Split from
//! `main.rs` to keep it under the 800-line cap.

use atomartist_storage::StorageUri;
use atomartist_ui::top_menu_bar::{FileDialogProvider, UnsavedChoice};

/// File-dialog provider for native — backed by `rfd`. Blocking dialogs
/// are fine: the agg-gui App's render loop is paused while the modal is
/// up, and the user's response unblocks it.
///
/// Filter ordering matters: rfd uses the first filter as the default
/// "Save as type", so `.atmr` — the only project format — comes first.
/// Open additionally offers "All files" for users whose projects are
/// named something else; Save does not, because the extension decides
/// the name the file ends up with.
///
/// `rfd` answers with a `PathBuf`; this is the shell boundary that turns
/// it into a `file:` [`StorageUri`], the only project identity the app
/// layer knows about. A path with no round-trippable URI form (a UNC
/// share or a verbatim prefix) is reported to the user and the pick is
/// treated as cancelled — see [`uri_or_explain`].
pub struct NativeDialogs;

/// Message shown when the user picks a location AtomArtist cannot yet
/// address. Stated as a workaround rather than a flat refusal, because
/// mapping the share to a drive letter genuinely works.
const UNC_NOT_SUPPORTED: &str =
    "Network (UNC) paths are not yet supported — map the share to a drive letter";
impl FileDialogProvider for NativeDialogs {
    fn pick_open_project(&self) -> Option<StorageUri> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["atmr"])
            .add_filter("All files", &["*"])
            .pick_file()
            .and_then(uri_or_explain)
    }
    fn pick_save_project(&self, default_name: &str) -> Option<StorageUri> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["atmr"])
            .set_file_name(default_name)
            .save_file()
            .and_then(uri_or_explain)
    }
    fn pick_save_export(&self, extension: &str, default_name: &str) -> Option<StorageUri> {
        let label = match extension {
            "stl" => "Binary STL",
            "3mf" => "3MF model",
            "obj" => "Wavefront OBJ",
            "atmr" => "AtomArtist project",
            _ => "Export",
        };
        rfd::FileDialog::new()
            .add_filter(label, &[extension])
            .set_file_name(default_name)
            .save_file()
            .and_then(uri_or_explain)
    }
    fn pick_import_file(&self) -> Option<StorageUri> {
        rfd::FileDialog::new()
            .add_filter(
                "All importable files",
                &["stl", "3mf", "obj", "mcx", "atmr"],
            )
            .add_filter("Meshes", &["stl", "3mf", "obj"])
            .add_filter("MatterControl scene", &["mcx"])
            .add_filter("AtomArtist project", &["atmr"])
            .pick_file()
            .and_then(uri_or_explain)
    }
    fn confirm_unsaved_changes(&self) -> UnsavedChoice {
        // Yes = save first, No = discard, Cancel = keep working.
        match rfd::MessageDialog::new()
            .set_title("Unsaved Changes")
            .set_description(
                "The current project has unsaved changes.\n\n\
                Save them before continuing?",
            )
            .set_level(rfd::MessageLevel::Warning)
            .set_buttons(rfd::MessageButtons::YesNoCancel)
            .show()
        {
            rfd::MessageDialogResult::Yes => UnsavedChoice::Save,
            rfd::MessageDialogResult::No => UnsavedChoice::Discard,
            _ => UnsavedChoice::Cancel,
        }
    }
    fn show_error(&self, message: &str) {
        rfd::MessageDialog::new()
            .set_title("AtomArtist")
            .set_description(message)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    fn show_info(&self, title: &str, message: &str) {
        rfd::MessageDialog::new()
            .set_title(title)
            .set_description(message)
            .set_level(rfd::MessageLevel::Info)
            .show();
    }
}

/// Convert a picked path to a `file:` URI, or put up an explanatory
/// dialog and return `None` (which every caller already treats as
/// "the user cancelled").
fn uri_or_explain(path: std::path::PathBuf) -> Option<StorageUri> {
    match StorageUri::from_local_path(&path) {
        Some(uri) => Some(uri),
        None => {
            rfd::MessageDialog::new()
                .set_title("AtomArtist")
                .set_description(format!(
                    "{}\n\n{}",
                    path.display(),
                    UNC_NOT_SUPPORTED
                ))
                .set_level(rfd::MessageLevel::Error)
                .show();
            None
        }
    }
}
