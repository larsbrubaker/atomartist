//! Top-level widget tree builder.
//!
//! Assembles the application root from the canvas widget, the 3D viewport,
//! and a vertical splitter — the simplest layout that exercises every
//! piece. Property panel and menu bar land in later phases.

use agg_gui::{HAnchor, Splitter, VAnchor, Widget};
use atomartist_renderer::{Viewport3dWidget, ViewportInputs};

use crate::app_state::AppState;
use crate::canvas_widget::NodeCanvas;

/// Build the application root widget tree.
///
/// Layout: a horizontal split — canvas on the left, viewport on the right.
/// The splitter ratio defaults to 0.6 (canvas takes ~60% of the width)
/// and is interactively draggable.
pub fn build_app(state: AppState) -> Box<dyn Widget> {
    let canvas: Box<dyn Widget> = Box::new(NodeCanvas::new(state.clone()));
    let viewport: Box<dyn Widget> = Box::new(Viewport3dWidget::new(ViewportInputs {
        last_mesh_output: state.last_mesh_output.clone(),
    }));

    let split = Splitter::new(canvas, viewport)
        .with_ratio(0.6)
        .with_h_anchor(HAnchor::STRETCH)
        .with_v_anchor(VAnchor::STRETCH);

    Box::new(split)
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
