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
use crate::debug_windows::{build_debug_windows, DebugWindowHandles};
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
/// Returns the root widget and a [`DebugWindowHandles`] the platform
/// shell uses to (a) push per-frame samples into the performance
/// history, (b) drain inspector edits + refresh the inspector node
/// snapshot, and (c) read the live window state back for persistence.
pub fn build_app(
    state: AppState,
    dialogs: Arc<dyn FileDialogProvider>,
    saved_ui: Option<UiSettings>,
) -> (Box<dyn Widget>, DebugWindowHandles) {
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
            // File-drop hook: when the user drags `.stl` / `.obj` /
            // `.3mf` onto the canvas, import the file as a project
            // asset and spawn a MeshNode at the drop position.
            // Non-mesh extensions are ignored silently for now —
            // future asset-backed nodes (image, vector, …) can be
            // routed here once they exist.
            .with_file_drop_handler(move |paths, canvas_pos| {
                for path in paths {
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase())
                        .unwrap_or_default();
                    if !matches!(ext.as_str(), "stl" | "obj" | "3mf") {
                        continue;
                    }
                    if let Err(e) =
                        drop_state.import_mesh_file(path.as_path(), canvas_pos)
                    {
                        eprintln!("drop import failed: {}", e);
                    }
                }
            }),
    );
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

    let top_row: Box<dyn Widget> = Box::new(
        FlexRow::new()
            .with_h_anchor(HAnchor::STRETCH)
            .with_v_anchor(VAnchor::FIT)
            .with_max_size(agg_gui::Size::new(f64::INFINITY, MENU_BAR_H))
            .add(menu_bar)
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
    // host on top. Stack hit-tests last-child first, so:
    //   1. the floating color picker (if open) gets input first,
    //   2. then the debug windows,
    //   3. then the main column.
    // The host's `hit_test` returns false when no dialog is active,
    // so events pass through cleanly to the rest of the UI most of
    // the time.
    let mut stack = Stack::new()
        .with_h_anchor(HAnchor::STRETCH)
        .with_v_anchor(VAnchor::STRETCH)
        .add(column);
    for w in build_debug_windows(font, &debug) {
        stack = stack.add(w);
    }
    stack = stack.add(Box::new(FloatingOverlayHost::new(overlay_handle)));
    (Box::new(stack), debug)
}

/// Convenience: build a fresh `AppState` with all built-in node types
/// pre-registered. Callers (demo-native / demo-wasm) typically pass this
/// straight to `build_app`.
pub fn fresh_state_with_builtins() -> AppState {
    use atomartist_lib::{nodes, registry::NodeRegistry, Graph};
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    AppState::new(Graph::new(), reg)
}

/// Same as `fresh_state_with_builtins`, but seeds the graph with the
/// default Rectangle → Inflate → Extrude pipeline (matching the
/// NodeDesigner reference scene) and runs the first evaluation so the
/// 3D viewport shows a rounded extruded plate on app start.
pub fn fresh_state_with_starter_graph() -> AppState {
    use atomartist_lib::graph::graph::Noodle;
    let state = fresh_state_with_builtins();
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
