//! Top-level widget tree builder.
//!
//! Assembles the application root from the canvas widget, the 3D viewport,
//! and a vertical splitter — the simplest layout that exercises every
//! piece. Property panel and menu bar land in later phases.

use agg_gui::{FlexColumn, HAnchor, VAnchor, Widget};
use atomartist_renderer::{Viewport3dWidget, ViewportInputs};

use crate::app_state::AppState;
use crate::canvas_widget::NodeCanvas;

/// Build the application root widget tree.
///
/// Layout (matching NodeDesigner): vertical stack — 3D viewport on top
/// (60% of height), node canvas on bottom (40%).
pub fn build_app(state: AppState) -> Box<dyn Widget> {
    let canvas: Box<dyn Widget> = Box::new(NodeCanvas::new(state.clone()));
    let viewport: Box<dyn Widget> = Box::new(Viewport3dWidget::new(ViewportInputs {
        last_mesh_output: state.last_mesh_output.clone(),
    }));

    let column = FlexColumn::new()
        .with_h_anchor(HAnchor::STRETCH)
        .with_v_anchor(VAnchor::STRETCH)
        .add_flex(viewport, 1.5)
        .add_flex(canvas, 1.0);
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

        let _ = g.connect(
            Edge { from: SocketId { node: rect, name: "out" }, to: SocketId { node: inflate, name: "input" } },
            &state.registry,
        );
        let _ = g.connect(
            Edge { from: SocketId { node: inflate, name: "out" }, to: SocketId { node: extrude, name: "input" } },
            &state.registry,
        );
        extrude
    };
    state.set_display_node(Some(extrude_id));
    state.evaluate_now();
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use agg_gui::Size;

    #[test]
    fn build_app_returns_a_widget() {
        let s = fresh_state_with_builtins();
        let mut w = build_app(s);
        let _ = w.layout(Size::new(800.0, 600.0));
    }
}
