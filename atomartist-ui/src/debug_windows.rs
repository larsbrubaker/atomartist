//! `View → Debug` floating windows.
//!
//! Mounts two `agg_gui::widgets::Window`s on top of the application
//! tree, both hidden by default:
//!
//!   - **Inspector** — agg-gui's `InspectorPanel` (widget-tree
//!     debugger, à la Chrome DevTools). Reads a refreshable
//!     `Vec<InspectorNode>` collected by the shell each frame and
//!     pushes live `WidgetBaseEdit` / `InspectorEdit` operations
//!     back through edit queues that the shell drains before the
//!     next layout.
//!   - **Performance** — agg-gui's `PerformanceView` (mean ms/frame
//!     label + 60-sample sparkline) reading from a shared
//!     `FrameHistory` that the shell pushes per-frame samples into.
//!
//! All cells are owned by [`DebugWindowHandles`]. The handles are
//! returned to the shell from [`crate::top_level::build_app`] so the
//! same `Rc<…>` slots are shared between the widget tree, the menu
//! action callbacks, the paint loop, and on-disk persistence
//! (`UiSettings::debug_windows`).
//!
//! Modeled on Marbles' `MarblesWindowLayoutHandles` + agg-gui's
//! `demo-wgpu::render_app_frame`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use agg_gui::text::Font;
use agg_gui::widget::{InspectorNode, InspectorOverlay, WidgetBaseEdit};
use agg_gui::widgets::{InspectorPanel, PerformanceView, Window};
use agg_gui::{shared_frame_history, InspectorEdit, Rect, SharedFrameHistory, Widget};

use crate::settings::{DebugWindowState, DebugWindowsState};

/// All the shared mutable handles the `View → Debug` windows wire
/// into. Lives for the lifetime of the app — clone-on-share via the
/// internal `Rc`s.
#[derive(Clone)]
pub struct DebugWindowHandles {
    pub inspector_visible: Rc<Cell<bool>>,
    pub inspector_bounds: Rc<Cell<Rect>>,
    pub perf_visible: Rc<Cell<bool>>,
    pub perf_bounds: Rc<Cell<Rect>>,

    pub inspector_nodes: Rc<RefCell<Vec<InspectorNode>>>,
    pub hovered_bounds: Rc<RefCell<Option<InspectorOverlay>>>,
    pub base_edits: Rc<RefCell<Vec<WidgetBaseEdit>>>,
    pub inspector_edits: Rc<RefCell<Vec<InspectorEdit>>>,

    pub frame_history: SharedFrameHistory,
}

impl DebugWindowHandles {
    /// Build a fresh handle set seeded with `saved` (or the
    /// documented defaults if `saved` is missing or carries
    /// zero-area bounds for a window).
    pub fn new(saved: DebugWindowsState) -> Self {
        let inspector_bounds = resolve_bounds(&saved.inspector, &DebugWindowsState::default().inspector);
        let perf_bounds = resolve_bounds(&saved.performance, &DebugWindowsState::default().performance);
        Self {
            inspector_visible: Rc::new(Cell::new(saved.inspector.open)),
            inspector_bounds: Rc::new(Cell::new(inspector_bounds)),
            perf_visible: Rc::new(Cell::new(saved.performance.open)),
            perf_bounds: Rc::new(Cell::new(perf_bounds)),

            inspector_nodes: Rc::new(RefCell::new(Vec::new())),
            hovered_bounds: Rc::new(RefCell::new(None)),
            base_edits: Rc::new(RefCell::new(Vec::new())),
            inspector_edits: Rc::new(RefCell::new(Vec::new())),

            frame_history: shared_frame_history(),
        }
    }

    /// Snapshot the live cells back into a serialisable
    /// [`DebugWindowsState`] for persistence.
    pub fn current_state(&self) -> DebugWindowsState {
        DebugWindowsState {
            inspector: state_from_cells(&self.inspector_visible, &self.inspector_bounds),
            performance: state_from_cells(&self.perf_visible, &self.perf_bounds),
        }
    }
}

fn resolve_bounds(saved: &DebugWindowState, fallback: &DebugWindowState) -> Rect {
    if saved.has_valid_bounds() {
        Rect::new(saved.x, saved.y, saved.width, saved.height)
    } else {
        Rect::new(fallback.x, fallback.y, fallback.width, fallback.height)
    }
}

fn state_from_cells(visible: &Rc<Cell<bool>>, bounds: &Rc<Cell<Rect>>) -> DebugWindowState {
    let r = bounds.get();
    DebugWindowState {
        open: visible.get(),
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Construct the Inspector and Performance windows already wired
/// against `handles`. Returns them in z-order from back to front;
/// the caller stacks them on top of the main UI so they paint above
/// the splitter and consume input first.
pub fn build_debug_windows(font: Arc<Font>, handles: &DebugWindowHandles) -> Vec<Box<dyn Widget>> {
    let inspector_panel = InspectorPanel::new(
        font.clone(),
        handles.inspector_nodes.clone(),
        handles.hovered_bounds.clone(),
    )
    .with_base_edit_queue(handles.base_edits.clone())
    .with_edit_queue(handles.inspector_edits.clone());

    let inspector_window = Window::new("Inspector", font.clone(), Box::new(inspector_panel))
        .with_bounds(handles.inspector_bounds.get())
        .with_visible_cell(handles.inspector_visible.clone())
        .with_position_cell(handles.inspector_bounds.clone())
        .with_resizable(true);

    let perf_view = PerformanceView::new(font.clone(), handles.frame_history.clone())
        .with_padding(12.0)
        .with_sparkline_height(64.0)
        // The perf window only exists to show live numbers, so opt
        // in to the per-revision redraw path — same convention
        // Solitaire and Marbles use.
        .with_history_redraw(true);

    let perf_window = Window::new("Performance", font, Box::new(perf_view))
        .with_bounds(handles.perf_bounds.get())
        .with_visible_cell(handles.perf_visible.clone())
        .with_position_cell(handles.perf_bounds.clone())
        .with_resizable(true);

    vec![Box::new(inspector_window), Box::new(perf_window)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_seed_from_default_state() {
        let h = DebugWindowHandles::new(DebugWindowsState::default());
        assert!(!h.inspector_visible.get());
        assert!(!h.perf_visible.get());
        // Default placement is positive-area in the bottom-left.
        let ib = h.inspector_bounds.get();
        assert!(ib.width > 0.0 && ib.height > 0.0);
        let pb = h.perf_bounds.get();
        assert!(pb.width > 0.0 && pb.height > 0.0);
    }

    #[test]
    fn invalid_saved_bounds_fall_back_to_default() {
        let mut saved = DebugWindowsState::default();
        saved.inspector.width = 0.0;
        saved.inspector.height = -10.0;
        saved.inspector.open = true; // open flag still honoured
        let h = DebugWindowHandles::new(saved);
        assert!(h.inspector_visible.get());
        let ib = h.inspector_bounds.get();
        let default_ib = DebugWindowsState::default().inspector;
        assert_eq!(ib.x, default_ib.x);
        assert_eq!(ib.y, default_ib.y);
        assert_eq!(ib.width, default_ib.width);
        assert_eq!(ib.height, default_ib.height);
    }

    #[test]
    fn current_state_round_trips_through_handles() {
        let original = DebugWindowsState {
            inspector: DebugWindowState {
                open: true,
                x: 80.0,
                y: 90.0,
                width: 500.0,
                height: 600.0,
            },
            performance: DebugWindowState {
                open: false,
                x: 700.0,
                y: 50.0,
                width: 320.0,
                height: 180.0,
            },
        };
        let h = DebugWindowHandles::new(original);
        assert_eq!(h.current_state(), original);
    }
}
