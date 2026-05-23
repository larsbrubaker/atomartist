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
    let canvas: Box<dyn Widget> = Box::new(
        NodeEditor::new(shared_model_for(state.clone()))
            .with_id("node-canvas")
            .with_overlay_sink(move |dialog, close_flag| {
                sink_handle.set(dialog, close_flag);
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
    use atomartist_lib::graph::node::NodeInstance;
    use atomartist_lib::graph::graph::Edge;
    use atomartist_lib::graph::node::SocketId;
    let state = fresh_state_with_builtins();
    let extrude_id = {
        let mut g = state.graph.lock().unwrap();
        let mk = |g: &mut atomartist_lib::Graph,
                  type_id: &'static str,
                  pos: [f64; 2],
                  reg: &atomartist_lib::registry::NodeRegistry| {
            let id = g.allocate_id();
            let mut n = NodeInstance::new(id, type_id, pos);
            if let Some(def) = reg.get(type_id) {
                for prop in def.properties() {
                    n.properties.insert(prop.name, prop.default);
                }
            }
            let _ = g.add_node(n);
            id
        };

        // Y is up in canvas-space and node.position is the node's top-left.
        // Place starter nodes near the top of the bottom panel so they sit
        // entirely inside the visible area on first launch.
        let rect = mk(&mut g, "Rectangle", [40.0, 240.0], &state.registry);
        let inflate = mk(&mut g, "Inflate", [260.0, 240.0], &state.registry);
        let extrude = mk(&mut g, "Extrude", [480.0, 240.0], &state.registry);
        let output = mk(&mut g, "Output", [700.0, 240.0], &state.registry);

        let _ = g.connect(
            Edge { from: SocketId { node: rect, name: "out" }, to: SocketId { node: inflate, name: "input" } },
            &state.registry,
        );
        let _ = g.connect(
            Edge { from: SocketId { node: inflate, name: "out" }, to: SocketId { node: extrude, name: "Paths" } },
            &state.registry,
        );
        let _ = g.connect(
            Edge { from: SocketId { node: extrude, name: "Geometry" }, to: SocketId { node: output, name: "in" } },
            &state.registry,
        );
        output
    };
    state.set_display_node(Some(extrude_id));
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
