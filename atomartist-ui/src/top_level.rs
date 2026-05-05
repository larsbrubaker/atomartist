//! Top-level widget tree builder.
//!
//! Assembles the application root from the canvas widget, the 3D viewport,
//! and a vertical splitter — the simplest layout that exercises every
//! piece. Property panel and menu bar land in later phases.

use std::sync::Arc;

use agg_gui::{
    font_settings::current_system_font, FlexColumn, HAnchor, Splitter, VAnchor, Widget,
};
use atomartist_renderer::{Viewport3dWidget, ViewportInputs};

use crate::app_state::AppState;
use crate::canvas_widget::NodeCanvas;
use crate::top_menu_bar::{build_menu_bar, FileDialogProvider};
#[cfg(test)]
use crate::top_menu_bar::NoFileDialogs;

/// Build the application root widget tree.
///
/// Layout (matching NodeDesigner): vertical stack — top menu bar, then
/// 3D viewport (60% of remaining height), then node canvas (40%).
pub fn build_app(state: AppState, dialogs: Arc<dyn FileDialogProvider>) -> Box<dyn Widget> {
    let canvas: Box<dyn Widget> = Box::new(NodeCanvas::new(state.clone()));
    let viewport: Box<dyn Widget> = Box::new(Viewport3dWidget::new(ViewportInputs {
        last_mesh_output: state.last_mesh_output.clone(),
    }));

    // Menu bar needs a font; the demo shells install one into
    // font_settings before building the tree, so this fall-through is safe.
    let font: Arc<agg_gui::text::Font> =
        current_system_font().expect("system font must be installed before build_app");
    let menu_bar: Box<dyn Widget> = Box::new(build_menu_bar(state.clone(), font, dialogs));

    // Vertical Splitter sits below the menu bar so the user can drag
    // the divider between viewport and canvas. ratio=0.6 = top pane gets
    // 60% of the height (matching NodeDesigner's default).
    let split: Box<dyn Widget> = Box::new(
        Splitter::vertical(viewport, canvas)
            .with_ratio(0.6)
            .with_h_anchor(HAnchor::STRETCH)
            .with_v_anchor(VAnchor::STRETCH),
    );

    let column = FlexColumn::new()
        .with_h_anchor(HAnchor::STRETCH)
        .with_v_anchor(VAnchor::STRETCH)
        .add(menu_bar)
        .add_flex(split, 1.0);
    Box::new(column)
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
            Edge { from: SocketId { node: inflate, name: "out" }, to: SocketId { node: extrude, name: "input" } },
            &state.registry,
        );
        let _ = g.connect(
            Edge { from: SocketId { node: extrude, name: "out" }, to: SocketId { node: output, name: "in" } },
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
