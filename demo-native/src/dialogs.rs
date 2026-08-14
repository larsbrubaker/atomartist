//! Native file dialogs — `rfd`-backed implementation of
//! `atomartist_ui::top_menu_bar::FileDialogProvider`. Split from
//! `main.rs` to keep it under the 800-line cap.

use std::path::PathBuf;

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
pub struct NativeDialogs;
impl FileDialogProvider for NativeDialogs {
    fn pick_open_project(&self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["atmr"])
            .add_filter("All files", &["*"])
            .pick_file()
    }
    fn pick_save_project(&self, default_name: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["atmr"])
            .set_file_name(default_name)
            .save_file()
    }
    fn pick_save_export(&self, extension: &str, default_name: &str) -> Option<PathBuf> {
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
    }
    fn pick_import_file(&self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter(
                "All importable files",
                &["stl", "3mf", "obj", "mcx", "atmr"],
            )
            .add_filter("Meshes", &["stl", "3mf", "obj"])
            .add_filter("MatterControl scene", &["mcx"])
            .add_filter("AtomArtist project", &["atmr"])
            .pick_file()
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
