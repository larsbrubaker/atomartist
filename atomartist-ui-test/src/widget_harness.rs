//! `WidgetHarness` — synthetic event dispatch over a *single* widget.
//!
//! [`TestHarness`](crate::TestHarness) boots the whole production tree
//! (`build_app`), which is right for anything wired into the app. A widget
//! that is not wired in yet — the file browser, which the Open/Save modal
//! (step 6c) and the favorites bar (6d) will mount later — still deserves
//! to be driven by real events rather than by direct method calls, so this
//! mounts one widget as the root of its own `agg_gui::App`.
//!
//! Everything else matches `TestHarness`: the bundled NotoSans font is
//! installed (widgets that build a `TextField` need one), `App::layout`
//! runs after every event so bounds never drift, and the storage job pump
//! is driven explicitly instead of slept on.
//!
//! # Coordinates
//!
//! The mouse helpers take agg-gui's **screen** coordinates — origin
//! top-left, Y-down, exactly what a platform shell hands to
//! `App::on_mouse_*`. Widget-local rectangles are Y-up, so
//! [`WidgetHarness::click_local`] converts for tests that aim at a
//! rectangle they read off the widget.

use std::sync::Arc;

use agg_gui::text::Font;
use agg_gui::widget::{find_widget_by_id, find_widget_by_id_mut, InspectorNode};
use agg_gui::{App, Key, Modifiers, MouseButton, Point, Size, Widget};
use atomartist_ui::AppState;

const FONT_BYTES: &[u8] =
    include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");

/// One widget, one `AppState`, real event dispatch.
pub struct WidgetHarness {
    state: AppState,
    app: App,
    cursor: (f64, f64),
    modifiers: Modifiers,
    size: (f64, f64),
}

impl WidgetHarness {
    /// Mount `root` as the whole tree at `w × h`.
    pub fn mount(state: AppState, root: Box<dyn Widget>, w: f64, h: f64) -> Self {
        let font = Arc::new(Font::from_bytes(FONT_BYTES.to_vec()).expect("bundled NotoSans"));
        agg_gui::font_settings::set_system_font(Some(font));
        let mut app = App::new(root);
        app.layout(Size::new(w, h));
        // A second pass: widgets that build children lazily (a `TextField`
        // that needed the font) only place them once they exist.
        app.layout(Size::new(w, h));
        WidgetHarness {
            state,
            app,
            cursor: (0.0, 0.0),
            modifiers: Modifiers::default(),
            size: (w, h),
        }
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    pub fn size(&self) -> (f64, f64) {
        self.size
    }

    /// The mounted root. `agg_gui::Widget` has no downcast hook, so tests
    /// assert through [`Self::property`] (the design's reflection channel)
    /// and through the model / cache handles they cloned before mounting.
    pub fn root(&self) -> &dyn Widget {
        self.app.root()
    }

    pub fn find_by_id(&self, id: &str) -> Option<&dyn Widget> {
        find_widget_by_id(self.app.root(), id)
    }

    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut dyn Widget> {
        find_widget_by_id_mut(self.app.root_mut(), id)
    }

    /// Reflection snapshot, same channel the production inspector reads.
    pub fn snapshot(&self) -> Vec<InspectorNode> {
        self.app.collect_inspector_nodes()
    }

    /// Property value from the root widget's `properties()`.
    pub fn property(&self, key: &str) -> Option<String> {
        self.app
            .root()
            .properties()
            .into_iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value)
    }

    /// One frame of the storage job pump.
    pub fn pump(&self) -> bool {
        self.state.pump_storage()
    }

    pub fn pump_until_idle(&self, max_frames: usize) {
        for _ in 0..max_frames {
            if !self.pump() {
                return;
            }
        }
        panic!("storage ops still pending after {max_frames} pump frames");
    }

    /// Re-run layout without an event — a frame boundary, which is also
    /// when a widget's per-frame work (the thumbnail visibility round)
    /// happens.
    pub fn frame(&mut self) -> &mut Self {
        self.app.layout(Size::new(self.size.0, self.size.1));
        self
    }

    /// Paint the tree once into a throwaway software framebuffer.
    ///
    /// The other helpers never paint (that is what makes them fast and
    /// GPU-free), which leaves `paint` — arithmetic over text metrics,
    /// image buffers and clip rects — completely unexercised. This runs
    /// the real `GfxCtx` software rasteriser over it so a panic or an
    /// out-of-range blit fails a test instead of a user's frame. Pixels
    /// are discarded: assertions belong on state, not on colours.
    pub fn paint_once(&mut self) -> &mut Self {
        let mut fb = agg_gui::Framebuffer::new(self.size.0 as u32, self.size.1 as u32);
        let mut ctx = agg_gui::GfxCtx::new(&mut fb);
        self.app.paint(&mut ctx);
        self
    }

    pub fn mouse_move(&mut self, x: f64, y: f64) -> &mut Self {
        self.cursor = (x, y);
        self.app.on_mouse_move(x, y);
        self.frame()
    }

    pub fn mouse_down(&mut self, button: MouseButton) -> &mut Self {
        let (x, y) = self.cursor;
        self.app.on_mouse_down(x, y, button, self.modifiers);
        self.frame()
    }

    pub fn mouse_up(&mut self, button: MouseButton) -> &mut Self {
        let (x, y) = self.cursor;
        self.app.on_mouse_up(x, y, button, self.modifiers);
        self.frame()
    }

    pub fn click(&mut self, x: f64, y: f64, button: MouseButton) -> &mut Self {
        self.mouse_move(x, y);
        self.mouse_down(button);
        self.mouse_up(button)
    }

    /// Click a point given in the *root widget's* Y-up local coordinates.
    pub fn click_local(&mut self, p: Point, button: MouseButton) -> &mut Self {
        let (x, y) = self.to_screen(p);
        self.click(x, y, button)
    }

    /// Two clicks in a row at the same point — a double-click, since the
    /// harness runs far inside agg-gui's 400 ms multi-click window.
    pub fn double_click_local(&mut self, p: Point, button: MouseButton) -> &mut Self {
        self.click_local(p, button);
        self.click_local(p, button)
    }

    /// Convert a root-local Y-up point to the Y-down screen coordinates
    /// the event helpers take.
    pub fn to_screen(&self, p: Point) -> (f64, f64) {
        (p.x, self.size.1 - p.y)
    }

    pub fn scroll_at(&mut self, p: Point, delta_y: f64) -> &mut Self {
        let (x, y) = self.to_screen(p);
        self.cursor = (x, y);
        self.app.on_mouse_wheel(x, y, delta_y);
        self.frame()
    }

    pub fn key_down(&mut self, key: Key) -> &mut Self {
        self.app.on_key_down(key, self.modifiers);
        self.frame()
    }

    /// A key with explicit modifiers — for shortcuts (Alt+Left) rather
    /// than typing, which the harness's ambient modifier state does not
    /// cover.
    pub fn key_down_with(&mut self, key: Key, modifiers: Modifiers) -> &mut Self {
        self.app.on_key_down(key, modifiers);
        self.frame()
    }

    /// Type a string one `Key::Char` at a time into whatever has focus.
    pub fn type_text(&mut self, text: &str) -> &mut Self {
        for ch in text.chars() {
            self.key_down(Key::Char(ch));
        }
        self
    }
}
