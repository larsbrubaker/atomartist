//! Top-level widget tree builder.
//!
//! Assembles the application root from the canvas widget, the 3D viewport,
//! and a vertical splitter — the simplest layout that exercises every
//! piece. Property panel and menu bar land in later phases.

use std::sync::Arc;

use agg_gui::{
    font_settings::current_system_font, widgets::menu::MENU_BAR_H, FlexColumn, FlexRow, HAnchor,
    Insets, Label, Spacer, Splitter, Stack, VAnchor, Widget,
};

use crate::app_state::AppState;
use agg_gui_node_editor::NodeEditor;
use crate::app_state_model::shared_model_for;
use crate::breadcrumb_bar::BreadcrumbBar;
use crate::debug_windows::{build_debug_windows, DebugWindowHandles};
use crate::file_browser::{FileBrowserModalHandle, FileBrowserModalHost};
use crate::floating_overlay::{FloatingOverlayHandle, FloatingOverlayHost};
use crate::settings::UiSettings;
use crate::status_bar::StatusBar;
use crate::top_menu_bar::{build_menu_bar_sized, FileDialogProvider};
#[cfg(test)]
use crate::top_menu_bar::NoFileDialogs;
use crate::viewport_overlay::build_viewport_overlay;

/// Build the application root widget tree.
///
/// Layout (matching NodeDesigner): vertical stack — top menu bar, then
/// 3D viewport (60% of remaining height), then node canvas (40%). The
/// `View → Debug` floating windows (Inspector, Performance graph) are
/// stacked on top of the column so they paint above the splitter and
/// hit-test first.
///
/// `saved_ui` seeds the debug-window visibility and bounds; pass
/// `None` to use first-launch defaults.
///
/// `browser_modal` is the shell's handle to the in-app Open/Save picker
/// (`file_browser::modal`). It is constructed *before* the tree — like
/// `dialogs` — because the code that opens the picker (step 6c-2's
/// `FileDialogProvider`) lives outside the widget tree and cannot fish a
/// handle back out of it.
///
/// Returns the root widget and a [`DebugWindowHandles`] the platform
/// shell uses to (a) push per-frame samples into the performance
/// history, (b) drain inspector edits + refresh the inspector node
/// snapshot, and (c) read the live window state back for persistence.
pub fn build_app(
    state: AppState,
    dialogs: Arc<dyn FileDialogProvider>,
    saved_ui: Option<UiSettings>,
    browser_modal: FileBrowserModalHandle,
) -> (Box<dyn Widget>, DebugWindowHandles) {
    // Window / node snap-layout default — ON for AtomArtist so the
    // PowerPoint-style alignment + spacing guides engage as soon as
    // the user drags a node.  Users can wire a menu toggle later;
    // the framework's `agg_gui::snap::set_enabled` flips the thread-
    // local at any time.
    agg_gui::snap::set_enabled(true);
    let saved_windows = saved_ui.map(|s| s.debug_windows).unwrap_or_default();
    let debug = DebugWindowHandles::new(saved_windows);

    // The node-canvas widget is now the generic `agg_gui_node_editor::NodeEditor`
    // driven by an `AppStateModel` adapter. We keep the widget id "node-canvas"
    // so existing tests (find_widget_by_id("node-canvas")) and external
    // selection mirroring continue to work.
    //
    // Floating-overlay handle: shared between the editor (which spawns
    // the ColorWheelPicker dialog when a color row is clicked) and a
    // top-level `FloatingOverlayHost` widget further down in this fn.
    // The handle is the channel that lets the dialog be reparented from
    // the editor's pane to the entire main window's coordinate space,
    // so the user can drag the picker anywhere on screen.
    let overlay_handle = FloatingOverlayHandle::new();
    let sink_handle = overlay_handle.clone();
    let drop_state = state.clone();
    let canvas: Box<dyn Widget> = Box::new(
        NodeEditor::new(shared_model_for(state.clone()))
            .with_id("node-canvas")
            .with_overlay_sink(move |dialog, close_flag| {
                sink_handle.set(dialog, close_flag);
            })
            // File-drop hook: meshes (`.stl` / `.obj` / `.3mf`) spawn
            // a MeshNode at the drop position; scene formats (`.mcx`,
            // `.atmr`) import into the current graph like File →
            // Import. Other extensions are ignored silently — future
            // asset-backed nodes (image, vector, …) can be routed
            // here once they exist.
            //
            // The OS hands agg-gui `PathBuf`s (a fixed API), so this is
            // the boundary where a dropped file becomes a `file:`
            // [`StorageUri`] — the only representation the rest of the
            // app deals in.
            .with_file_drop_handler(move |paths, canvas_pos| {
                for path in paths {
                    // A UNC / verbatim path has no round-trippable URI
                    // form (see `StorageUri::from_local_path`); skip it
                    // loudly rather than importing the wrong file.
                    let Some(uri) = atomartist_storage::StorageUri::from_local_path(path) else {
                        eprintln!(
                            "drop import skipped: {} is a network (UNC) or verbatim path, \
                             which is not supported yet — map the share to a drive letter",
                            path.display()
                        );
                        continue;
                    };
                    // The import submits its read to the frame pump and
                    // reports failures as notices, so there is no result
                    // to inspect here — a `file:` drop settles inline, a
                    // future async provider a frame later. The same call
                    // backs the favorites-bar drag-insert drop, so the
                    // two surfaces cannot diverge.
                    drop_state.import_dropped_file(&uri, canvas_pos);
                }
            }),
    );
    // The favorites bar is docked on the *canvas's* left edge, not the
    // window's: the 3-D viewport keeps the full width, and the bar reads
    // as chrome belonging to the node graph (its favourites add nodes to
    // it). So the canvas pane of the splitter is a row — bar first (=
    // leftmost), canvas taking whatever is left. Zero gap so the bar's
    // handle sits flush against the canvas it resizes into.
    //
    // The row is wrapped in a `PaneWidthProbe` so the bar can cap itself
    // at a fraction of the *pane* — a widget cannot ask its parent how
    // big it is, and inferring it from the flex row's two layout passes
    // is the bug that module documents.
    let pane_width = crate::favorites_bar_host::PaneWidth::new();
    // Drag-insert (step 6e): the bar and its embedded browser share one
    // gesture controller, and the ghost it floats goes into the same
    // top-of-`Stack` overlay slot the colour picker uses.
    let drag_insert =
        crate::drag_insert::DragInsertHandle::new(state.clone(), overlay_handle.clone());
    let favorites_bar: Box<dyn Widget> = Box::new(
        crate::favorites_bar::FavoritesBar::new(state.clone(), dialogs.clone(), pane_width.clone())
            .with_drag_insert(drag_insert),
    );
    let canvas_row: Box<dyn Widget> = Box::new(
        FlexRow::new()
            .with_gap(0.0)
            .with_h_anchor(HAnchor::STRETCH)
            .with_v_anchor(VAnchor::STRETCH)
            .add(favorites_bar)
            .add_flex(canvas, 1.0),
    );
    let canvas: Box<dyn Widget> = Box::new(crate::favorites_bar_host::PaneWidthProbe::new(
        pane_width,
        canvas_row,
    ));

    // Menu bar needs a font; the demo shells install one into
    // font_settings before building the tree, so this fall-through is safe.
    let font: Arc<agg_gui::text::Font> =
        current_system_font().expect("system font must be installed before build_app");

    let viewport: Box<dyn Widget> = build_viewport_overlay(state.clone(), font.clone());

    let menu_bar: Box<dyn Widget> =
        build_menu_bar_sized(state.clone(), font.clone(), dialogs.clone(), debug.clone());

    // Top chrome row: menu bar on the left, spacer pushes the project
    // title to the right. License / About live inside the Help menu —
    // we deliberately keep the main bar uncluttered.
    //
    // The row is locked to `MENU_BAR_H` (agg-gui's canonical menu-bar
    // height) so the menu hugs the top of the window like a traditional
    // Windows menu — no wasted strip of chrome above the items. The
    // project title is sized + centered to fit inside that same
    // height so it doesn't force the row taller.
    let project_title: Box<dyn Widget> = Box::new(
        Label::new("Untitled Project", font.clone())
            .with_font_size(12.0)
            .with_align(agg_gui::widgets::label::LabelAlign::Right)
            .with_v_anchor(VAnchor::CENTER)
            .with_margin(Insets::symmetric(8.0, 0.0)),
    );

    // Drill-in navigation chrome: back button + breadcrumb trail. Hidden
    // (zero-slot) at the root, appears the moment the user drills into a
    // component. Sits just right of the menu bar so it reads as chrome,
    // not content.
    let breadcrumb: Box<dyn Widget> = Box::new(BreadcrumbBar::new(state.clone()));

    let top_row: Box<dyn Widget> = Box::new(
        FlexRow::new()
            .with_h_anchor(HAnchor::STRETCH)
            .with_v_anchor(VAnchor::FIT)
            .with_max_size(agg_gui::Size::new(f64::INFINITY, MENU_BAR_H))
            .add(menu_bar)
            .add(breadcrumb)
            .add_flex(Box::new(Spacer::new().with_h_anchor(HAnchor::STRETCH)), 1.0)
            .add(project_title),
    );

    // Vertical Splitter sits below the menu bar so the user can drag
    // the divider between viewport and canvas. ratio=0.6 = top pane gets
    // 60% of the height (matching NodeDesigner's default).
    let split: Box<dyn Widget> = Box::new(
        Splitter::vertical(viewport, canvas)
            .with_ratio(0.6)
            .with_h_anchor(HAnchor::STRETCH)
            .with_v_anchor(VAnchor::STRETCH),
    );

    let status: Box<dyn Widget> = Box::new(StatusBar::new(state.clone()));

    // FlexColumn lays out top→bottom in Y-up coords (first add = top).
    let column: Box<dyn Widget> = Box::new(
        FlexColumn::new()
            .with_h_anchor(HAnchor::STRETCH)
            .with_v_anchor(VAnchor::STRETCH)
            .add(top_row)
            .add_flex(split, 1.0)
            .add(status),
    );

    // Stack: column behind, debug windows in front, floating-overlay
    // host above them, Open/Save modal host last of all. Stack
    // hit-tests last-child first, so:
    //   1. the floating color picker (if open) gets input first,
    //   2. then the debug windows,
    //   3. then the main column.
    // The host's `hit_test` returns false when no dialog is active,
    // so events pass through cleanly to the rest of the UI most of
    // the time.
    //
    // The file-browser modal is added *after* the floating-overlay host
    // even though it never competes on hit-testing (it always answers
    // `hit_test` false and is reached through agg-gui's
    // `active_modal_path` instead): being last means it paints on top of
    // everything, which is what a scrim has to do.
    let mut stack = Stack::new()
        .with_h_anchor(HAnchor::STRETCH)
        .with_v_anchor(VAnchor::STRETCH)
        .add(column);
    for w in build_debug_windows(font.clone(), &debug) {
        stack = stack.add(w);
    }
    stack = stack.add(Box::new(FloatingOverlayHost::new(overlay_handle)));
    stack = stack.add(Box::new(FileBrowserModalHost::new(
        state,
        font,
        browser_modal,
    )));
    (Box::new(stack), debug)
}

/// Convenience: build a fresh `AppState` with all built-in node types
/// pre-registered and an **empty** storage registry — project IO fails
/// until a shell registers a provider. Tests that never touch storage
/// use this directly.
pub fn fresh_state_with_builtins() -> AppState {
    fresh_state_with_builtins_and_storage(Arc::new(atomartist_storage::StorageRegistry::new()))
}

/// Same, with the shell's storage providers supplied. `demo-native`
/// passes a registry holding `LocalFsProvider`; the test harness passes
/// a `MemoryProvider` (plus `LocalFsProvider` on native). This crate
/// never picks a backend for its callers.
pub fn fresh_state_with_builtins_and_storage(
    storage: Arc<atomartist_storage::StorageRegistry>,
) -> AppState {
    use atomartist_lib::{nodes, registry::NodeRegistry, Graph};
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    AppState::with_storage(Graph::new(), reg, storage)
}

/// Same as `fresh_state_with_builtins`, but seeds the graph with the
/// default Rectangle → Inflate → Extrude pipeline (matching the
/// NodeDesigner reference scene) and runs the first evaluation so the
/// 3D viewport shows a rounded extruded plate on app start.
pub fn fresh_state_with_starter_graph() -> AppState {
    fresh_state_with_starter_graph_and_storage(Arc::new(
        atomartist_storage::StorageRegistry::new(),
    ))
}

/// Starter graph over a caller-supplied storage registry — the shape
/// both demo shells and the UI test harness use.
pub fn fresh_state_with_starter_graph_and_storage(
    storage: Arc<atomartist_storage::StorageRegistry>,
) -> AppState {
    use atomartist_lib::graph::graph::Noodle;
    let state = fresh_state_with_builtins_and_storage(storage);
    let display_target = {
        let mut g = state.graph.lock().unwrap();

        // Y is up in canvas-space and node.position is the node's top-left.
        let rect = g.add_new_node("Rectangle", [40.0, 240.0], &state.registry).unwrap();
        let inflate = g.add_new_node("Inflate", [260.0, 240.0], &state.registry).unwrap();
        let extrude = g.add_new_node("Extrude", [480.0, 240.0], &state.registry).unwrap();
        let output = g.add_new_node("Output", [700.0, 240.0], &state.registry).unwrap();

        // Resolve socket uids on the fresh instances, then connect.
        let connect_by_name =
            |g: &mut atomartist_lib::Graph,
             from: atomartist_lib::graph::node::NodeId,
             from_name: &str,
             to: atomartist_lib::graph::node::NodeId,
             to_name: &str| {
                let from_uid = g.get(from).unwrap().output_by_name(from_name).unwrap().uid;
                let to_uid = g.get(to).unwrap().input_by_name(to_name).unwrap().uid;
                let _ = g.connect(Noodle::new(from, from_uid, to, to_uid), &state.registry);
            };
        connect_by_name(&mut g, rect, "out", inflate, "input");
        connect_by_name(&mut g, inflate, "out", extrude, "Paths");
        // The Output node uses the dynamic multi-input model: its first
        // input is the empty trailing placeholder (name "") that adopts
        // the source on connect. Resolve its uid directly rather than
        // looking up the legacy "in" name.
        {
            let extrude_uid = g.get(extrude).unwrap().output_by_name("Geometry").unwrap().uid;
            let output_in_uid = g.get(output).unwrap().inputs[0].uid;
            let _ = g.connect(
                Noodle::new(extrude, extrude_uid, output, output_in_uid),
                &state.registry,
            );
        }
        output
    };
    state.set_display_node(Some(display_target));
    // The starter pipeline is the app's clean launch state — don't let
    // it count as "unsaved changes" the moment the window opens.
    state.mark_saved_baseline();
    state.evaluate_now();
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_has_all_builtin_node_types() {
        let s = fresh_state_with_builtins();
        // Every built-in we register should be discoverable.
        assert!(s.registry.get("Box").is_some());
        assert!(s.registry.get("Rectangle").is_some());
        assert!(s.registry.get("Extrude").is_some());
        assert!(s.registry.get("Output").is_some());
        // Sanity: build_app with NoFileDialogs is constructable, but we
        // skip exercising it here because it needs a system font installed
        // by the platform shell — covered by tests/live_eval.rs which
        // installs the font and then exercises the full pipeline.
        let _ = NoFileDialogs;
    }
}
