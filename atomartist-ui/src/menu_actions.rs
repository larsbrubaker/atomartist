//! Menu action routing — translates the string action ids emitted by
//! the top menu bar (see `top_menu_bar::compose_menus`) into `AppState`
//! mutations, file dialogs, and debug-window toggles.
//!
//! Destructive file actions (`file.new`, `file.open`, recent-file
//! opens) funnel through [`confirm_discard_unsaved`] so unsaved work
//! always gets a Save / Discard / Cancel prompt first. The same helper
//! is exported for the platform shell's window-close path.

use agg_gui::theme::{AccentColor, ThemePreference};
use atomartist_lib::graph::undo_commands::AddNodeCmd;
use atomartist_storage::StorageUri;

use crate::app_state::AppState;
use crate::app_state_files::MeshExportFormat;
use crate::app_state_storage::{display_uri, uri_exists, uri_file_stem};
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

/// Save to the current location, prompting for one when the project has
/// never been saved. Returns `true` on a completed save.
pub fn save_current(state: &AppState, dialogs: &dyn FileDialogProvider) -> bool {
    let existing = state.current_file.lock().unwrap().clone();
    let target = match existing {
        Some(uri) => Some(uri),
        None => dialogs.pick_save_project("untitled.atmr"),
    };
    let Some(uri) = target else { return false };
    match state.save_graph_to_uri(&uri) {
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
        .and_then(uri_file_stem)
        .unwrap_or_else(|| "export".to_string());
    format!("{stem}.{ext}")
}

fn open_project_at(state: &AppState, dialogs: &dyn FileDialogProvider, uri: &StorageUri) {
    if let Err(e) = state.load_graph_from_uri(uri) {
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
            // Add into the *active* graph (the component template when
            // drilled in, else the root) and push the undo command onto
            // that graph's stack — mirrors `AppStateModel::add_node`.
            // Build the node off-graph, then hand the full instance to
            // AddNodeCmd so redo can re-insert it.
            let ag = state.active_graph();
            let node = {
                let mut g = ag.lock().unwrap();
                // Menu-add has no cursor position; drop the node in open
                // space to the right of the current graph so it doesn't
                // stack on prior adds or hide behind existing nodes.
                let pos = crate::node_helpers::next_menu_add_position(&g);
                crate::node_helpers::add_node_with_defaults(
                    &mut g,
                    &state.registry,
                    static_id,
                    pos,
                )
                .and_then(|id| g.remove_node(id).ok().map(|(node, _detached)| node))
            };
            if let Some(node) = node {
                let cmd = AddNodeCmd::new(ag, node);
                state.active_undo().lock().unwrap().add_and_do(Box::new(cmd));
                state.schedule_evaluate_after_edit();
            }
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
        let Some(uri) = idx
            .parse::<usize>()
            .ok()
            .and_then(|i| state.recent_projects.lock().unwrap().get(i).cloned())
        else {
            return;
        };
        // Ask the provider whether the project is still there. An
        // unknown scheme (a provider the current build doesn't
        // register) reads the same as a deleted file: the entry can't
        // be opened, so it leaves the list.
        if !uri_exists(&state.storage, &uri) {
            dialogs.show_error(&format!(
                "{} no longer exists — removing it from the recent list.",
                display_uri(&uri)
            ));
            state.recent_projects.lock().unwrap().retain(|u| u != &uri);
            return;
        }
        if confirm_discard_unsaved(state, dialogs) {
            open_project_at(state, dialogs, &uri);
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
            if let Some(uri) = dialogs.pick_save_export(format.extension(), &name) {
                if let Err(e) = state.export_mesh_to_uri(&uri, format) {
                    dialogs.show_error(&format!("Export failed: {}", e));
                }
            }
        } else if ext == "atmr" {
            let name = export_default_name(state, "atmr");
            if let Some(uri) = dialogs.pick_save_export("atmr", &name) {
                if let Err(e) = state.export_project_copy_to_uri(&uri) {
                    dialogs.show_error(&format!("Export failed: {}", e));
                }
            }
        }
        return;
    }
    match action {
        "edit.undo" => {
            // Route to the active graph's undo stack — the component
            // template's stack when drilled in, else the root stack.
            state.active_undo().lock().unwrap().undo();
            state.schedule_evaluate_after_edit();
        }
        "edit.redo" => {
            state.active_undo().lock().unwrap().redo();
            state.schedule_evaluate_after_edit();
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
            if let Some(uri) = dialogs.pick_open_project() {
                open_project_at(state, dialogs, &uri);
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
                .and_then(|uri| uri.file_name().map(|n| n.to_string()))
                .unwrap_or_else(|| "untitled.atmr".to_string());
            if let Some(uri) = dialogs.pick_save_project(&suggested) {
                if let Err(e) = state.save_graph_to_uri(&uri) {
                    dialogs.show_error(&format!("Save failed: {}", e));
                }
            }
        }
        "file.import" => {
            // Import adds to the scene rather than replacing it, so no
            // unsaved-changes gate.
            if let Some(uri) = dialogs.pick_import_file() {
                if let Err(e) = state.import_scene_file(&uri) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug_windows::DebugWindowHandles;
    use crate::settings::DebugWindowsState;
    use crate::top_level::fresh_state_with_starter_graph;
    use crate::top_menu_bar::NoFileDialogs;
    use atomartist_lib::graph::node::NodeId;

    fn debug_handles() -> DebugWindowHandles {
        DebugWindowHandles::new(DebugWindowsState::default())
    }

    /// (position, id) for every node of `type_id` in the active graph.
    fn nodes_of_type(state: &AppState, type_id: &str) -> Vec<([f64; 2], NodeId)> {
        let ag = state.active_graph();
        let g = ag.lock().unwrap();
        g.nodes()
            .filter(|n| n.type_id.as_ref() == type_id)
            .map(|n| (n.position, n.id))
            .collect()
    }

    /// Reproduces the user-reported "menu-added node can't be selected /
    /// moved / connected, and the next add doesn't appear" cluster. Root
    /// cause: menu-add dropped every node at a single fixed canvas point
    /// that overlapped a starter node, so successive adds stacked on top
    /// of each other and behind existing nodes — invisible to hit-testing.
    #[test]
    fn menu_add_places_nodes_without_overlap_or_stacking() {
        let state = fresh_state_with_starter_graph();
        let dialogs = NoFileDialogs;
        let debug = debug_handles();

        handle_action(&state, &dialogs, &debug, "add.Cylinder");
        handle_action(&state, &dialogs, &debug, "add.Sphere");

        // Symptom 4: both nodes actually get added, with distinct ids.
        let cyl = nodes_of_type(&state, "Cylinder");
        let sph = nodes_of_type(&state, "Sphere");
        assert_eq!(cyl.len(), 1, "Cylinder should be added exactly once");
        assert_eq!(sph.len(), 1, "Sphere should be added exactly once");
        assert_ne!(cyl[0].1, sph[0].1, "added nodes must have distinct ids");

        // Symptoms 1/4: successive menu-adds must not stack on the same
        // canvas position.
        assert_ne!(
            cyl[0].0, sph[0].0,
            "successive menu-added nodes must not stack at one position",
        );

        // Symptoms 1/2: a new node must not land on top of an existing
        // node, or its title bar / sockets are unreachable by hit-testing.
        let existing: Vec<[f64; 2]> = {
            let ag = state.active_graph();
            let g = ag.lock().unwrap();
            g.nodes()
                .filter(|n| {
                    n.type_id.as_ref() != "Cylinder" && n.type_id.as_ref() != "Sphere"
                })
                .map(|n| n.position)
                .collect()
        };
        // A node header is roughly 170 wide × 120 tall in canvas units;
        // anything closer than that overlaps enough to steal hit-testing.
        let overlaps = |a: [f64; 2], b: [f64; 2]| {
            (a[0] - b[0]).abs() < 170.0 && (a[1] - b[1]).abs() < 120.0
        };
        for new_pos in [cyl[0].0, sph[0].0] {
            for e in &existing {
                assert!(
                    !overlaps(new_pos, *e),
                    "new node at {:?} overlaps existing node at {:?}",
                    new_pos,
                    e,
                );
            }
        }
    }

    /// Follow-up to the placement fix: the rightward cascade must not run
    /// off toward +X forever. Once a row fills, menu-add wraps to a new row
    /// below (Y-up: smaller Y). Guarantees X stays bounded and no two nodes
    /// ever collide, however many are added.
    #[test]
    fn menu_add_wraps_row_keeping_x_bounded_without_overlap() {
        let state = fresh_state_with_starter_graph();
        let dialogs = NoFileDialogs;
        let debug = debug_handles();

        // Left-most column across the starter graph anchors the wrap bound.
        let leftmost_x = {
            let ag = state.active_graph();
            let g = ag.lock().unwrap();
            g.nodes().map(|n| n.position[0]).fold(f64::INFINITY, f64::min)
        };

        // Add well past one row's worth (~6 columns) to force several wraps.
        for _ in 0..18 {
            handle_action(&state, &dialogs, &debug, "add.Cylinder");
        }

        let positions: Vec<[f64; 2]> = {
            let ag = state.active_graph();
            let g = ag.lock().unwrap();
            g.nodes().map(|n| n.position).collect()
        };

        // X stays bounded: nothing cascades past the wrap extent from the
        // left-most column (must match ROW_MAX_EXTENT in node_helpers).
        const ROW_MAX_EXTENT: f64 = 1400.0;
        for p in &positions {
            assert!(
                p[0] <= leftmost_x + ROW_MAX_EXTENT + 1.0,
                "node X {} exceeded the wrap bound {}",
                p[0],
                leftmost_x + ROW_MAX_EXTENT,
            );
        }

        // No two nodes (added or starter) collide.
        let overlaps = |a: [f64; 2], b: [f64; 2]| {
            (a[0] - b[0]).abs() < 170.0 && (a[1] - b[1]).abs() < 120.0
        };
        for i in 0..positions.len() {
            for j in (i + 1)..positions.len() {
                assert!(
                    !overlaps(positions[i], positions[j]),
                    "nodes at {:?} and {:?} overlap",
                    positions[i],
                    positions[j],
                );
            }
        }
    }
}
