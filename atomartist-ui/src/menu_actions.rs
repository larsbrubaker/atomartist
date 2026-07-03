//! Menu action routing — translates the string action ids emitted by
//! the top menu bar (see `top_menu_bar::compose_menus`) into `AppState`
//! mutations, file dialogs, and debug-window toggles.
//!
//! Destructive file actions (`file.new`, `file.open`, recent-file
//! opens) funnel through [`confirm_discard_unsaved`] so unsaved work
//! always gets a Save / Discard / Cancel prompt first. The same helper
//! is exported for the platform shell's window-close path.

use agg_gui::theme::{AccentColor, ThemePreference};

use crate::app_state::AppState;
use crate::app_state_files::MeshExportFormat;
use crate::debug_windows::DebugWindowHandles;
use crate::top_menu_bar::{FileDialogProvider, UnsavedChoice};

/// Gate for actions that would discard the current graph. Returns
/// `true` when the caller may proceed:
/// - no unsaved changes → proceed silently,
/// - user picks **Save** → saved successfully (prompting for a path if
///   the project was never saved); a cancelled or failed save blocks,
/// - user picks **Discard** → proceed,
/// - user picks **Cancel** → block.
pub fn confirm_discard_unsaved(state: &AppState, dialogs: &dyn FileDialogProvider) -> bool {
    if !state.has_unsaved_changes() {
        return true;
    }
    match dialogs.confirm_unsaved_changes() {
        UnsavedChoice::Save => save_current(state, dialogs),
        UnsavedChoice::Discard => true,
        UnsavedChoice::Cancel => false,
    }
}

/// Save to the current file, prompting for a path when the project has
/// never been saved. Returns `true` on a completed save.
pub fn save_current(state: &AppState, dialogs: &dyn FileDialogProvider) -> bool {
    let existing = state.current_file.lock().unwrap().clone();
    let path = match existing {
        Some(p) => Some(p),
        None => dialogs.pick_save_project("untitled.atmr"),
    };
    let Some(p) = path else { return false };
    match state.save_graph_to_path(&p) {
        Ok(()) => true,
        Err(e) => {
            dialogs.show_error(&format!("Save failed: {}", e));
            false
        }
    }
}

/// Suggested export filename: current project stem (or "export") plus
/// the format extension.
fn export_default_name(state: &AppState, ext: &str) -> String {
    let stem = state
        .current_file
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "export".to_string());
    format!("{stem}.{ext}")
}

fn open_project_at(state: &AppState, dialogs: &dyn FileDialogProvider, path: &std::path::Path) {
    if let Err(e) = state.load_graph_from_path(path) {
        dialogs.show_error(&format!("Open failed: {}", e));
    }
}

/// Apply the current theme + accent combination to agg-gui's live
/// visuals. Called whenever either changes — same shape as the demo's
/// `apply_theme_visuals`.
fn apply_theme_visuals(theme: ThemePreference, accent: AccentColor) {
    use agg_gui::theme::{set_visuals, Visuals};
    let base = match theme {
        ThemePreference::Light => Visuals::light(),
        // System currently falls back to Dark; if agg-gui later grows
        // a `detect_system_theme()` AtomArtist can plug it in here.
        ThemePreference::Dark | ThemePreference::System => Visuals::dark(),
    };
    set_visuals(base.with_accent_color(accent));
}

pub(crate) fn handle_action(
    state: &AppState,
    dialogs: &dyn FileDialogProvider,
    debug: &DebugWindowHandles,
    action: &str,
) {
    if let Some(type_id) = action.strip_prefix("add.") {
        // Find the action's NodeDef by its dynamic type_id string and
        // intern it. Registry stores &'static str ids; we look up the
        // exact one rather than leaking new memory each call.
        let interned = state
            .registry
            .iter()
            .map(|d| d.type_id())
            .find(|s| *s == type_id);
        if let Some(static_id) = interned {
            let mut g = state.graph.lock().unwrap();
            let _ = crate::node_helpers::add_node_with_defaults(
                &mut g,
                &state.registry,
                static_id,
                [80.0, 220.0],
            );
            drop(g);
            state.schedule_evaluate();
        }
        return;
    }
    // Accent swatch picker — routes to the shared `Visuals` apply path
    // so the chosen colour flows through every widget on the next frame.
    if let Some(key) = action.strip_prefix("view.accent.") {
        if let Some(accent) = AccentColor::from_key(key) {
            *state.accent_color.lock().unwrap() = accent;
            let theme = *state.theme.lock().unwrap();
            apply_theme_visuals(theme, accent);
        }
        return;
    }
    if let Some(theme) = match action {
        "view.theme.light" => Some(ThemePreference::Light),
        "view.theme.dark" => Some(ThemePreference::Dark),
        "view.theme.system" => Some(ThemePreference::System),
        _ => None,
    } {
        *state.theme.lock().unwrap() = theme;
        let accent = *state.accent_color.lock().unwrap();
        apply_theme_visuals(theme, accent);
        return;
    }
    // Recent-file opens: the index refers into the live MRU list.
    if let Some(idx) = action.strip_prefix("file.recent.") {
        let Some(path) = idx
            .parse::<usize>()
            .ok()
            .and_then(|i| state.recent_projects.lock().unwrap().get(i).cloned())
        else {
            return;
        };
        if !path.exists() {
            dialogs.show_error(&format!(
                "{} no longer exists — removing it from the recent list.",
                path.display()
            ));
            state.recent_projects.lock().unwrap().retain(|p| p != &path);
            return;
        }
        if confirm_discard_unsaved(state, dialogs) {
            open_project_at(state, dialogs, &path);
        }
        return;
    }
    if let Some(ext) = action.strip_prefix("file.export.") {
        let format = match ext {
            "stl" => Some(MeshExportFormat::Stl),
            "3mf" => Some(MeshExportFormat::ThreeMf),
            "obj" => Some(MeshExportFormat::Obj),
            _ => None,
        };
        if let Some(format) = format {
            let name = export_default_name(state, format.extension());
            if let Some(path) = dialogs.pick_save_export(format.extension(), &name) {
                if let Err(e) = state.export_mesh_to_path(&path, format) {
                    dialogs.show_error(&format!("Export failed: {}", e));
                }
            }
        } else if ext == "atmr" {
            let name = export_default_name(state, "atmr");
            if let Some(path) = dialogs.pick_save_export("atmr", &name) {
                if let Err(e) = state.export_project_copy_to_path(&path) {
                    dialogs.show_error(&format!("Export failed: {}", e));
                }
            }
        }
        return;
    }
    match action {
        "edit.undo" => {
            let mut buf = state.undo.lock().unwrap();
            buf.undo();
            state.schedule_evaluate();
        }
        "edit.redo" => {
            let mut buf = state.undo.lock().unwrap();
            buf.redo();
            state.schedule_evaluate();
        }
        "file.new" => {
            if confirm_discard_unsaved(state, dialogs) {
                state.new_empty_project();
            }
        }
        "file.open" => {
            if !confirm_discard_unsaved(state, dialogs) {
                return;
            }
            if let Some(path) = dialogs.pick_open_project() {
                open_project_at(state, dialogs, &path);
            }
        }
        "file.save" => {
            save_current(state, dialogs);
        }
        "file.save_as" => {
            let suggested = state
                .current_file
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "untitled.atmr".to_string());
            if let Some(path) = dialogs.pick_save_project(&suggested) {
                if let Err(e) = state.save_graph_to_path(&path) {
                    dialogs.show_error(&format!("Save failed: {}", e));
                }
            }
        }
        "file.import" => {
            // Import adds to the scene rather than replacing it, so no
            // unsaved-changes gate.
            if let Some(path) = dialogs.pick_import_file() {
                if let Err(e) = state.import_scene_file(&path) {
                    dialogs.show_error(&format!("Import failed: {}", e));
                }
            }
        }
        "help.about" => {
            dialogs.show_info(
                "About AtomArtist",
                &format!(
                    "AtomArtist v{}\n\n\
                    A pure-Rust visual node-based 3D design tool.\n\
                    Built on agg-gui + manifold-rust + clipper2-rust + tess2-rust.\n\n\
                    https://github.com/larsbrubaker/atomartist",
                    env!("CARGO_PKG_VERSION"),
                ),
            );
        }
        "help.license" => {
            dialogs.show_info(
                "License",
                "AtomArtist is licensed under the MIT License.\n\
                See the LICENSE file in the project root for the full text.",
            );
        }
        "help.docs" => {
            dialogs.show_info(
                "Documentation",
                "Documentation lives in README.md and CLAUDE.md\n\
                in the project repository.\n\n\
                https://github.com/larsbrubaker/atomartist",
            );
        }
        "view.debug.inspector" => {
            debug.inspector_visible.set(!debug.inspector_visible.get());
        }
        "view.debug.performance" => {
            debug.perf_visible.set(!debug.perf_visible.get());
        }
        _ => {}
    }
}
