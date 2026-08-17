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

use agg_gui::widget::{
    find_widget_by_id, find_widget_by_id_mut, find_widget_by_type, InspectorNode,
};
use agg_gui::{App, Key, Modifiers, MouseButton, Size, Widget};
use atomartist_storage::{MemoryProvider, StorageRegistry};
use atomartist_ui::file_browser::{FileBrowserModalHandle, ModalFileDialogs};
use atomartist_ui::{
    build_app, fresh_state_with_builtins_and_storage, fresh_state_with_starter_graph_and_storage,
    AppState, DebugWindowHandles,
};
use atomartist_ui::top_menu_bar::{FileDialogProvider, NoFileDialogs};

/// Default viewport size — matches NodeDesigner's reference window so
/// hit-testing coordinates ported from those tests land on the same widgets.
pub const DEFAULT_WIDTH: f64 = 1280.0;
pub const DEFAULT_HEIGHT: f64 = 720.0;

// The bundled font used to be installed here from a local
// `include_bytes!`. It now arrives through
// `atomartist_ui::install_theme_and_fonts` — the shells' own startup —
// so the harness cannot drift from what ships.

/// Scheme of the harness's in-memory store. Tests address projects as
/// `mem:///whatever.atmr` and never touch the filesystem.
pub const MEMORY_SCHEME: &str = "mem";

/// Storage registry every harness state is built over: an in-memory
/// provider for project IO plus, on native, the real filesystem for the
/// `file:` URIs that OS file-drops produce.
pub fn test_storage_registry() -> Arc<StorageRegistry> {
    let mut registry = StorageRegistry::new();
    registry
        .register(Arc::new(MemoryProvider::new(MEMORY_SCHEME, "Test Memory")))
        .expect("fresh registry accepts the memory provider");
    #[cfg(not(target_arch = "wasm32"))]
    registry
        .register(Arc::new(atomartist_storage::LocalFsProvider::new()))
        .expect("fresh registry accepts the local filesystem provider");
    Arc::new(registry)
}

/// `mem:///name` — the canonical way for a test to name a project.
pub fn memory_uri(name: &str) -> atomartist_storage::StorageUri {
    atomartist_storage::StorageUri::new(MEMORY_SCHEME, name)
}

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
    /// Handle to the in-app Open/Save picker the tree hosts. Tests open
    /// the dialog through this, exactly as the dialog provider does.
    browser_modal: FileBrowserModalHandle,
    /// The provider the tree's menu callbacks use — and the one
    /// [`TestHarness::menu_action`] dispatches through, so a test drives
    /// the same routing a real click does.
    dialogs: Arc<dyn FileDialogProvider>,
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
        Self::from_state(fresh_state_with_builtins_and_storage(
            test_storage_registry(),
        ))
    }

    /// Same as [`Self::new`] but the graph is preloaded with the
    /// canonical "Box → Output" starter graph from
    /// `fresh_state_with_starter_graph`.
    pub fn with_starter_graph() -> Self {
        Self::from_state(fresh_state_with_starter_graph_and_storage(
            test_storage_registry(),
        ))
    }

    /// Boot the real widget tree over a caller-supplied [`AppState`].
    ///
    /// The harness registry is an immutable `Arc<NodeRegistry>` captured
    /// by `build_app`, so a test that needs a component (subgraph) type
    /// registered — or any other non-default registry / graph seeding —
    /// builds its own `AppState` (registering the subgraph *before*
    /// construction) and hands it here. Everything downstream (widget
    /// tree, event dispatch, evaluation) is identical to the default
    /// constructors; only the seeded state differs.
    pub fn with_app_state(state: AppState) -> Self {
        Self::from_state(state)
    }

    /// Boot the tree with the **real** in-app file dialogs
    /// ([`ModalFileDialogs`]) instead of [`NoFileDialogs`] — the web
    /// shell's exact wiring.
    ///
    /// This is what lets a test drive a whole File-menu flow end to end:
    /// [`menu_action`](Self::menu_action) → the picker puts the browser
    /// modal up → synthetic clicks answer it → [`pump`](Self::pump)
    /// applies the pick. Every other constructor uses `NoFileDialogs`,
    /// whose pickers answer "cancelled" immediately, because most tests
    /// want the menu action and not the dialog.
    pub fn with_modal_dialogs(state: AppState) -> Self {
        Self::build(state, |handle, state| {
            Arc::new(ModalFileDialogs::new(handle.clone(), state))
        })
    }

    /// Boot the tree with a caller-supplied dialog provider.
    ///
    /// The provider is not only what [`menu_action`](Self::menu_action)
    /// routes through — the widget tree captures it too (the favorites
    /// bar opens projects through the same gate the File menu uses), so
    /// this is how a test scripts the unsaved-changes prompt for a flow
    /// that starts with a *click* rather than a menu action.
    pub fn with_dialogs(state: AppState, dialogs: Arc<dyn FileDialogProvider>) -> Self {
        Self::build(state, move |_handle, _state| dialogs)
    }

    /// Resize the harness viewport. Re-runs `App::layout` so widget
    /// bounds reflect the new size on the next event.
    pub fn with_size(mut self, w: u32, h: u32) -> Self {
        self.size = (w as f64, h as f64);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    fn from_state(state: AppState) -> Self {
        Self::build(state, |_handle, _state| Arc::new(NoFileDialogs))
    }

    /// Shared constructor: the dialog provider is built *from* the modal
    /// handle, so it has to be made between the handle and the tree.
    fn build(
        state: AppState,
        make_dialogs: impl FnOnce(&FileBrowserModalHandle, &AppState) -> Arc<dyn FileDialogProvider>,
    ) -> Self {
        // Run the shells' own startup: theme, fonts, text-quality
        // recipe, and the vector icons property rows name by id. Calling
        // the *same function* the shells call — rather than re-doing a
        // subset of it here — is what makes a test able to catch a
        // registration that startup stopped performing. A harness that
        // installed its own copy would stay green while the shipped app
        // lost the artwork. Idempotent across harness instances.
        atomartist_ui::install_theme_and_fonts(1.0);
        // Same storage completion hook both shells install, so a test
        // exercising an off-thread settle sees production wiring rather
        // than a harness that gets away with polling every frame.
        atomartist_ui::install_storage_wakeups();
        // Harness always starts with the documented default debug
        // window layout — tests that care about persistence build
        // their own UiSettings and pass it directly to build_app.
        let browser_modal = FileBrowserModalHandle::new();
        let dialogs = make_dialogs(&browser_modal, &state);
        let (root, debug): (Box<dyn Widget>, DebugWindowHandles) = build_app(
            state.clone(),
            dialogs.clone(),
            None,
            browser_modal.clone(),
        );
        let mut app = App::new(root);
        app.layout(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));
        Self {
            state,
            app,
            debug,
            browser_modal,
            dialogs,
            cursor: (0.0, 0.0),
            modifiers: Modifiers::default(),
            size: (DEFAULT_WIDTH, DEFAULT_HEIGHT),
        }
    }

    /// Dispatch a menu action id (`"file.open"`, `"file.save_as"`,
    /// `"edit.undo"`, …) through the production routing the top menu
    /// bar's callback uses, with this harness's dialog provider and debug
    /// handles. A layout pass follows, so a picker that put the browser
    /// modal up is on screen when this returns.
    pub fn menu_action(&mut self, action: &str) -> &mut Self {
        atomartist_ui::menu_actions::handle_action(
            &self.state,
            &self.dialogs,
            &self.debug,
            action,
        );
        self.frame()
    }

    // ── State accessors ────────────────────────────────────────────────

    /// Borrow the live `AppState`. Tests inspect graphs / selection /
    /// display node through this. Mutating it directly is fine — the
    /// widget tree picks up the change on the next event because both
    /// share the same `Arc`s.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The state's storage registry — for tests that want to seed or
    /// inspect bytes directly (`storage().by_scheme("mem")`) rather
    /// than going through an `AppState` file operation.
    pub fn storage(&self) -> &Arc<StorageRegistry> {
        &self.state.storage
    }

    /// Synchronous evaluator — `AppState::evaluate_now` runs in the
    /// calling thread (no background spawn) so the test can assert on
    /// `last_mesh_output` immediately after the call.
    pub fn evaluate_now(&self) {
        self.state.evaluate_now();
    }

    /// One frame of the storage job pump — the same
    /// [`AppState::pump_storage`] call the native and web shells make
    /// once per frame. Returns `true` while operations are still in
    /// flight.
    ///
    /// Tests drive this explicitly instead of sleeping: an asynchronous
    /// provider under test (`FlakyProvider`) advances its own simulated
    /// clock with `pump()`, and this advances the app's.
    pub fn pump(&self) -> bool {
        self.state.pump_storage()
    }

    /// Pump up to `max_frames` times, stopping as soon as the queue
    /// drains.
    ///
    /// Panics — naming the operations still outstanding — rather than
    /// looping forever, so a test that never settles fails fast instead
    /// of hanging CI. Note this only advances the *app's* clock: a
    /// provider with its own simulated latency must be pumped alongside.
    pub fn pump_until_idle(&self, max_frames: usize) {
        // An already-idle queue is idle regardless of the budget, so
        // `pump_until_idle(0)` on a quiet state must not panic. The gate
        // and the diagnostic below both count *every* operation, quiet
        // background work included: `pump` drains the whole queue, so
        // asking the loud-only question here would return unpumped from a
        // state that still has thumbnail reads to settle.
        if self.state.pending_op_count_all() == 0 {
            return;
        }
        for _ in 0..max_frames {
            if !self.pump() {
                return;
            }
        }
        let outstanding: Vec<String> = self
            .state
            .pending_op_status_all()
            .into_iter()
            .map(|(label, _progress)| label)
            .collect();
        panic!(
            "storage ops still pending after {max_frames} pump frames: {}",
            outstanding.join(", ")
        );
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

    /// Handle to the tree's Open/Save picker — `open(mode, name)` puts
    /// the dialog up and returns the job its outcome settles into.
    pub fn browser_modal(&self) -> &FileBrowserModalHandle {
        &self.browser_modal
    }

    /// The harness viewport size, `(width, height)` in screen pixels.
    pub fn size(&self) -> (f64, f64) {
        self.size
    }

    /// Re-run `App::layout` without an event — a frame boundary, which is
    /// when per-frame widget work (claiming a queued dialog, the
    /// thumbnail visibility round) happens.
    pub fn frame(&mut self) -> &mut Self {
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    /// Paint the whole tree once into a throwaway software framebuffer.
    ///
    /// Mirrors [`crate::widget_harness::WidgetHarness::paint_once`]: the
    /// other helpers never paint, which leaves `paint` — text metrics,
    /// image blits, clip arithmetic — unexercised. Running agg-gui's
    /// software rasteriser over it turns a panic or an out-of-range blit
    /// into a test failure instead of a user's frame. Pixels are
    /// discarded; assertions belong on state.
    pub fn paint_once(&mut self) -> &mut Self {
        let mut fb = agg_gui::Framebuffer::new(self.size.0 as u32, self.size.1 as u32);
        let mut ctx = agg_gui::GfxCtx::new(&mut fb);
        self.app.paint(&mut ctx);
        self
    }

    /// Convert a Y-up point in *root* (screen-absolute) coordinates to the
    /// Y-down screen coordinates the event helpers take.
    pub fn to_screen(&self, p: agg_gui::Point) -> (f64, f64) {
        (p.x, self.size.1 - p.y)
    }

    /// Click a point given in root-local Y-up coordinates.
    pub fn click_local(&mut self, p: agg_gui::Point, button: MouseButton) -> &mut Self {
        let (x, y) = self.to_screen(p);
        self.click(x, y, button)
    }

    /// Two clicks at the same point, far inside agg-gui's 400 ms
    /// multi-click window.
    pub fn double_click_local(&mut self, p: agg_gui::Point, button: MouseButton) -> &mut Self {
        self.click_local(p, button);
        self.click_local(p, button)
    }

    /// Type a string one `Key::Char` at a time into whatever has focus.
    pub fn type_text(&mut self, text: &str) -> &mut Self {
        for ch in text.chars() {
            self.key_down(Key::Char(ch));
        }
        self
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

    /// Simulate one or more files being dropped onto the window at
    /// `(x, y)` (screen coords; Y-down to match the other event
    /// helpers). Mirrors what `WindowEvent::DroppedFile` triggers in
    /// the native shell. The harness handles the cursor update and
    /// layout pass so the test can immediately assert against new
    /// graph state.
    pub fn drop_files(
        &mut self,
        x: f64,
        y: f64,
        paths: Vec<std::path::PathBuf>,
    ) -> &mut Self {
        self.cursor = (x, y);
        self.app.on_mouse_move(x, y);
        self.app.on_file_dropped(x, y, paths);
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    /// Convenience for the single-file case — the common shape coming
    /// out of winit (one `DroppedFile` event per file).
    pub fn drop_file(&mut self, x: f64, y: f64, path: std::path::PathBuf) -> &mut Self {
        self.drop_files(x, y, vec![path])
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
