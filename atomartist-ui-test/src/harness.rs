//! `TestHarness` — synthetic event dispatch over the real AtomArtist
//! widget tree. See the crate-level docs for design notes.
//!
//! All public methods return `&mut Self` where it doesn't break the
//! reflection-borrow pattern, so tests chain naturally:
//!
//! ```ignore
//! TestHarness::new()
//!     .with_size(1024, 768)
//!     .click(100.0, 100.0, MouseButton::Left);
//! ```
//!
//! The harness owns:
//! - `AppState` — read live by the widget tree on every event; tests
//!    assert on it directly (`harness.state().graph.lock()...`).
//! - `agg_gui::App` — wraps the root widget; routes events to the right
//!    leaf via `find_widget_by_id` / hit-testing.

use std::sync::Arc;

use agg_gui::text::Font;
use agg_gui::widget::{
    find_widget_by_id, find_widget_by_id_mut, find_widget_by_type, InspectorNode,
};
use agg_gui::{App, Key, Modifiers, MouseButton, Size, Widget};
use atomartist_ui::{
    build_app, fresh_state_with_builtins, fresh_state_with_starter_graph, AppState,
    DebugWindowHandles,
};
use atomartist_ui::top_menu_bar::NoFileDialogs;

/// Default viewport size — matches NodeDesigner's reference window so
/// hit-testing coordinates ported from those tests land on the same widgets.
pub const DEFAULT_WIDTH: f64 = 1280.0;
pub const DEFAULT_HEIGHT: f64 = 720.0;

/// Bundled NotoSans font — same bytes the demo shells `include_bytes!` so
/// the harness produces a layout that exactly matches running natives.
const FONT_BYTES: &[u8] =
    include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");

/// State + driver for one UI test scenario.
pub struct TestHarness {
    state: AppState,
    app: App,
    /// Handles owned by the View → Debug floating windows. Tests
    /// use these to assert visibility toggles fire on menu clicks,
    /// to push synthetic frame samples into the performance graph,
    /// and to drain the same inspector edit queue the production
    /// shell drains each paint.
    debug: DebugWindowHandles,
    cursor: (f64, f64),
    modifiers: Modifiers,
    size: (f64, f64),
}

impl TestHarness {
    /// Empty graph + bundled font + a fully-built widget tree at
    /// 1280×720. The widget tree is the *real* production tree — not a
    /// mock — so anything tested here exercises the same code paths as
    /// `cargo dev`.
    pub fn new() -> Self {
        Self::from_state(fresh_state_with_builtins())
    }

    /// Same as [`Self::new`] but the graph is preloaded with the
    /// canonical "Box → Output" starter graph from
    /// `fresh_state_with_starter_graph`.
    pub fn with_starter_graph() -> Self {
        Self::from_state(fresh_state_with_starter_graph())
    }

    /// Resize the harness viewport. Re-runs `App::layout` so widget
    /// bounds reflect the new size on the next event.
    pub fn with_size(mut self, w: u32, h: u32) -> Self {
        self.size = (w as f64, h as f64);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    fn from_state(state: AppState) -> Self {
        // Install the bundled font into agg-gui's thread-local font
        // slot — most chrome widgets (MenuBar, Label) need it. Idempotent
        // across multiple harness instances in one test process.
        let font = Arc::new(Font::from_bytes(FONT_BYTES.to_vec()).expect("bundled NotoSans"));
        agg_gui::font_settings::set_system_font(Some(font));
        let dialogs: Arc<dyn atomartist_ui::top_menu_bar::FileDialogProvider> =
            Arc::new(NoFileDialogs);
        // Harness always starts with the documented default debug
        // window layout — tests that care about persistence build
        // their own UiSettings and pass it directly to build_app.
        let (root, debug): (Box<dyn Widget>, DebugWindowHandles) =
            build_app(state.clone(), dialogs, None);
        let mut app = App::new(root);
        app.layout(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));
        Self {
            state,
            app,
            debug,
            cursor: (0.0, 0.0),
            modifiers: Modifiers::default(),
            size: (DEFAULT_WIDTH, DEFAULT_HEIGHT),
        }
    }

    // ── State accessors ────────────────────────────────────────────────

    /// Borrow the live `AppState`. Tests inspect graphs / selection /
    /// display node through this. Mutating it directly is fine — the
    /// widget tree picks up the change on the next event because both
    /// share the same `Arc`s.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Synchronous evaluator — `AppState::evaluate_now` runs in the
    /// calling thread (no background spawn) so the test can assert on
    /// `last_mesh_output` immediately after the call.
    pub fn evaluate_now(&self) {
        self.state.evaluate_now();
    }

    /// Borrow the agg-gui `App`. Useful for low-level reflection / focus
    /// checks the harness doesn't expose helpers for.
    pub fn app(&self) -> &App {
        &self.app
    }
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// Borrow the View → Debug window handles. Lets tests assert on
    /// the inspector / performance window visibility cells and push
    /// synthetic samples into the shared frame history.
    pub fn debug(&self) -> &DebugWindowHandles {
        &self.debug
    }

    // ── Reflection-based widget lookup ────────────────────────────────

    /// Find a widget by its `id()` override (e.g. `"node-canvas"`,
    /// `"viewport-3d"`, `"status-bar"`). DFS through the tree.
    pub fn find_by_id(&self, id: &str) -> Option<&dyn Widget> {
        find_widget_by_id(self.app.root(), id)
    }

    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut dyn Widget> {
        find_widget_by_id_mut(self.app.root_mut(), id)
    }

    /// Find a widget by its `type_name()`. The first match in DFS order
    /// is returned — convenient for unique widgets.
    pub fn find_by_type(&self, type_name: &str) -> Option<&dyn Widget> {
        find_widget_by_type(self.app.root(), type_name)
    }

    /// Snapshot the inspector tree — the same data the production
    /// inspector uses to render type-aware property editors.
    pub fn snapshot(&self) -> Vec<InspectorNode> {
        self.app.collect_inspector_nodes()
    }

    // ── Modifier state ────────────────────────────────────────────────

    /// Set the modifier flags that subsequent click / key events will
    /// carry. Persists until cleared.
    pub fn set_modifiers(&mut self, mods: Modifiers) -> &mut Self {
        self.modifiers = mods;
        self
    }

    pub fn clear_modifiers(&mut self) -> &mut Self {
        self.modifiers = Modifiers::default();
        self
    }

    // ── Mouse helpers ─────────────────────────────────────────────────

    /// Move the synthetic cursor. Coordinates are agg-gui's
    /// physical-pixel screen space — origin top-left, Y-down — same as
    /// the platform shell hands to `App::on_mouse_move`.
    pub fn mouse_move(&mut self, x: f64, y: f64) -> &mut Self {
        self.cursor = (x, y);
        self.app.on_mouse_move(x, y);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    pub fn mouse_down(&mut self, button: MouseButton) -> &mut Self {
        let (x, y) = self.cursor;
        self.app.on_mouse_down(x, y, button, self.modifiers);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    pub fn mouse_up(&mut self, button: MouseButton) -> &mut Self {
        let (x, y) = self.cursor;
        self.app.on_mouse_up(x, y, button, self.modifiers);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    /// Move + down + up in one call — the most common pattern. Coordinates
    /// are screen-space.
    pub fn click(&mut self, x: f64, y: f64, button: MouseButton) -> &mut Self {
        self.mouse_move(x, y);
        self.mouse_down(button);
        self.mouse_up(button)
    }

    /// Drag from `(x0, y0)` to `(x1, y1)` while holding `button`. Fires
    /// `down → move(x1, y1) → up` so the widget's `on_mouse_move` sees a
    /// non-trivial delta.
    pub fn drag(
        &mut self,
        from: (f64, f64),
        to: (f64, f64),
        button: MouseButton,
    ) -> &mut Self {
        self.mouse_move(from.0, from.1);
        self.mouse_down(button);
        self.mouse_move(to.0, to.1);
        self.mouse_up(button)
    }

    pub fn scroll(&mut self, delta_y: f64) -> &mut Self {
        let (x, y) = self.cursor;
        self.app.on_mouse_wheel(x, y, delta_y);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    // ── Keyboard helpers ──────────────────────────────────────────────

    pub fn key_down(&mut self, key: Key) -> &mut Self {
        self.app.on_key_down(key, self.modifiers);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    /// Press `key` while holding the given `mods`. The harness's
    /// persistent modifier state is *not* changed.
    pub fn key_chord(&mut self, mods: Modifiers, key: Key) -> &mut Self {
        self.app.on_key_down(key, mods);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }
}

impl Default for TestHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_constructs_at_default_size() {
        let h = TestHarness::new();
        assert!(h.find_by_id("node-canvas").is_some());
        assert!(h.find_by_id("viewport-3d").is_some());
        assert!(h.find_by_id("status-bar").is_some());
    }

    #[test]
    fn starter_graph_has_initial_geometry() {
        let h = TestHarness::with_starter_graph();
        h.evaluate_now();
        let mesh = h.state().last_mesh_output.lock().unwrap().clone();
        assert!(mesh.is_some(), "starter graph should produce a mesh");
    }

    #[test]
    fn empty_canvas_click_clears_selection() {
        let mut h = TestHarness::with_starter_graph();
        // Pre-seed a selection so we can verify the click clears it.
        h.state().set_selection(Some(atomartist_lib::graph::node::NodeId(99)));

        // Compute a click position firmly inside the canvas widget by
        // reading its bounds (agg-gui local Y-up coords) and converting
        // to top-down screen pixels — see the comment on `mouse_move`
        // for the conversion.  Click on the far-right edge of the canvas
        // so we're guaranteed not to hit the starter-graph nodes (which
        // anchor near the centre).
        let (canvas_screen_x, canvas_screen_y) = {
            let canvas = h.find_by_id("node-canvas").expect("canvas must exist");
            let b = canvas.bounds();
            // Bounds are widget-local; convert to screen by walking up,
            // but in the AtomArtist tree the canvas is a top-level
            // child so its origin is its layout origin in the parent.
            // For our purposes, picking a point near the canvas's max-X
            // edge works either way because both tests below use the
            // same layout.
            //
            // Use the widget's bottom-right region in *its* local coords,
            // then flip Y to top-down screen coords against the harness
            // size.  Canvas height < total height, so this approximates
            // a real click in the empty area.
            let local_x = b.x + b.width * 0.95;
            let local_y_yup = b.y + b.height * 0.5;
            let screen_x = local_x;
            let screen_y = h.size.1 - local_y_yup;
            (screen_x, screen_y)
        };
        h.click(canvas_screen_x, canvas_screen_y, MouseButton::Left);
        let sel = *h.state().selection.lock().unwrap();
        assert_eq!(sel, None, "empty-canvas click should clear selection");
    }

    #[test]
    fn resize_relayouts_widgets() {
        let mut h = TestHarness::new();
        let canvas = h.find_by_id("node-canvas").unwrap();
        let original = canvas.bounds();
        h = h.with_size(800, 600);
        let canvas = h.find_by_id("node-canvas").unwrap();
        assert_ne!(original, canvas.bounds(), "bounds should change after resize");
    }
}
