//! `FavoritesBar` — the edge-docked favorites strip and its expanded
//! browser panel (`docs/file-browser-design.md` §1.2, §2, §4; steps 6d-2
//! and 6f-1).
//!
//! AtomArtist's descendant of NodeDesigner's parts bar
//! (`static/js/node-editor/ui/parts-bar.js`), moved to the **left** edge
//! per product direction (the ancestor docks right). Step 6f-1 docks it
//! on the **3-D viewport pane** — the favourites insert *parts*, which
//! belong to the model, and the node canvas keeps its full width.
//!
//! # The strip never collapses
//!
//! Two pieces, mirroring the ancestor's DOM order:
//!
//! ```text
//!   | strip (72) | panel (browser) | handle (16) |
//! ```
//!
//! The 72 px icon strip is *always* on screen: it is the primitive
//! palette. Collapsing hides only the browser panel, which is why the
//! persisted width is the **panel's** width, not the bar's.
//!
//! * **Collapsed** — strip + handle. Each strip item is a 44 × 44 icon
//!   slot with a 9 px label under it. NodeType favourites get a render
//!   of the real primitive once [`crate::node_icons`] has produced one
//!   (6f-2) and their palette category's glyph until then, Project
//!   favourites a file glyph, and anything that
//!   no longer [`resolve`](crate::file_browser::Favorite::resolve)s is
//!   greyed rather than pruned (design §2: the provider may come back).
//! * **Expanded** — strip + the shared [`FileBrowser`] in its *embedded*
//!   face + handle. The panel is *only* the browser; the favourites live
//!   in the strip, exactly as in the ancestor.
//!
//! # The handle is a toggle and a resize grip
//!
//! One 16 × 56 grip on the bar's right edge does both, with the
//! ancestor's constants: a press released within [`DRAG_THRESHOLD`]
//! pixels toggles, and anything further drags the panel's width. Because
//! the bar is docked left, dragging **right** widens it. Pulling right
//! out of the collapsed state opens the panel and keeps sizing in the
//! same gesture; releasing below [`COLLAPSE_THRESHOLD_W`] snaps it closed
//! but **keeps** the stored width, so the next open comes back at the
//! user's size.
//!
//! # Lazy mount
//!
//! The panel's browser is built on the first expand and dropped on
//! collapse, so a user who never opens it never pays for a listing
//! (design §2, "lazy mount"). The [`BrowserModel`] and the
//! [`ThumbnailCache`] outlive the widget, so re-opening returns to the
//! same directory with its previews already in hand.
//!
//! # Ownership
//!
//! The bar renders [`AppState::favorites`] and writes
//! [`AppState::favorites_bar_expanded`] / `favorites_bar_width` — all three
//! live on `AppState`, not here, because the shells persist them through
//! [`AppState::ui_settings`] on a frame tick that cannot reach into the
//! widget tree.
//!
//! # Affordances
//!
//! * **Pin current project** is a strip item anchored to the strip's
//!   bottom, outside the scroll region, offered only while an unpinned
//!   project is open.
//! * **Scrolling.** A palette taller than the pane scrolls under the
//!   wheel (ND's `overflow-y: auto`), so every favourite is reachable at
//!   any window height. The offset is view state, not persisted.
//!
//! # Not in this step
//!
//! * **Unpin.** 6f-1 deliberately ships *no* unpin gesture. The obvious
//!   one — right-click deletes — is un-undoable and unrecoverable for
//!   `NodeType` favourites (seeding runs once ever, and there is no
//!   re-pin path for a palette entry yet), and a proper confirm needs a
//!   popup that survives the paint order: the bar paints *before* the
//!   3-D viewport it sits beside, so anything it draws outside its own
//!   width is covered. That belongs with 6f-3's context-menu work, on
//!   the floating-overlay host the drag ghost already uses.
//!   [`Favorites::remove`] stays the model-level operation.
//! * **Drag-to-reorder** the strip. [`Favorites::move_favorite`] is
//!   already there for it; the gesture belongs with the drag controller,
//!   which owns the threshold / ghost machinery this would duplicate.
//!
//! # Coordinates
//!
//! Bar-local and Y-up; see [`crate::favorites_bar_geom`], which owns every
//! rectangle this file hit-tests.

use std::sync::Arc;

use agg_gui::{
    DrawCtx, Event, EventResult, HAnchor, Insets, MouseButton, Point, Rect, Size, VAnchor, Widget,
    WidgetBase,
};
use atomartist_storage::StorageUri;

use crate::app_state::AppState;
use crate::app_state_storage::uri_extension;
use crate::drag_insert::{DragInsertHandle, GestureEnd};
use crate::favorites_bar_geom::{self as geom, BarLayout, COLLAPSED_W};
use crate::favorites_bar_handle::{clamp_panel, HandleGesture};
use crate::favorites_bar_host::PaneRect;
use crate::favorites_strip::{self, StripItem};
use crate::file_browser::favorites::{Favorite, FavoriteKind};
use crate::file_browser::model::BrowserModel;
use crate::file_browser::thumbs::ThumbnailCache;
use crate::file_browser::widget::{BrowserMode, FileBrowser};
use crate::top_menu_bar::FileDialogProvider;

/// Widget id of the bar itself — the harness and the inspector look it up
/// with this (design §6).
pub const BAR_ID: &str = "favorites-bar";
/// Widget id of the browser *inside* the bar. Deliberately **not**
/// `"file-browser"`: the Open/Save modal's instance owns that id, and a
/// `find_widget_by_id` walk must never have to guess which of the two it
/// found.
pub const EMBEDDED_BROWSER_ID: &str = "favorites-browser";

/// Pointer travel that turns a handle press from a toggle into a resize.
/// `parts-bar.js`'s constant.
pub const DRAG_THRESHOLD: f64 = 3.0;
/// Narrowest usable browser panel — the width below which the grid stops
/// being a grid. `parts-bar.js`'s `MIN_WIDTH`.
pub const MIN_EXPANDED_W: f32 = 240.0;
/// Panel width a never-resized bar opens to. `parts-bar.js`'s
/// `DEFAULT_WIDTH`.
pub const DEFAULT_EXPANDED_W: f32 = 380.0;
/// A drag released with the panel narrower than this snaps the bar closed
/// instead of leaving a sliver. `parts-bar.js`'s `COLLAPSE_THRESHOLD`.
pub const COLLAPSE_THRESHOLD_W: f32 = 120.0;
/// Largest share of the host pane the *panel* may occupy.
pub const MAX_WIDTH_FRACTION: f64 = 0.7;
/// Width of the 3-D viewport the bar refuses to eat into, whatever the
/// fraction above would allow. A viewport narrower than this is not a
/// viewport — the model would be a sliver — so in a pane too small for
/// both, the panel yields first.
pub const MIN_VIEWPORT_W: f64 = 160.0;
/// Pixels one wheel notch scrolls the strip. Matches agg-gui's
/// `ScrollView`, so the bar scrolls at the same speed as every other
/// scrollable surface in the app.
pub const SCROLL_STEP: f64 = 40.0;
/// Absolute cap applied when no layout width is known yet (settings
/// parsing runs long before the first frame).
pub const MAX_STORED_W: f32 = 2000.0;

/// Clamp a persisted / restored panel width into the range the panel can
/// actually open at. Non-finite values (a hand-edited `NaN`) fall back to
/// the default rather than propagating into layout arithmetic.
pub fn clamp_stored_width(width: f32) -> f32 {
    if !width.is_finite() {
        return DEFAULT_EXPANDED_W;
    }
    width.clamp(MIN_EXPANDED_W, MAX_STORED_W)
}

/// The left favorites bar. See the module docs.
pub struct FavoritesBar {
    bounds: Rect,
    base: WidgetBase,
    state: AppState,
    dialogs: Arc<dyn FileDialogProvider>,
    /// Shared across every expand — previews are expensive and immutable
    /// for a given `(uri, stamp)`.
    cache: ThumbnailCache,
    /// Built with the first expanded panel; kept afterwards so a re-open
    /// returns to the directory the user left.
    model: Option<BrowserModel>,
    /// Empty while collapsed, exactly one [`FileBrowser`] while expanded.
    children: Vec<Box<dyn Widget>>,
    layout: BarLayout,
    items: Vec<StripItem>,
    /// Whether the pin-current-project item was offered this frame.
    show_pin: bool,
    drag: Option<HandleGesture>,
    /// Drag-insert controller shared with the embedded browser (step
    /// 6e). `None` when the host never supplied one (unit tests that
    /// build a bare bar) — the bar then behaves exactly as it did
    /// before drag-insert existed.
    insert: Option<DragInsertHandle>,
    /// How far the strip is scrolled, in pixels from the top. View
    /// state, re-clamped every layout; deliberately not persisted.
    scroll: f64,
    /// Strip item the current press landed on, keyed by its
    /// `(kind, stable_key)` rather than its index: the favourites vec
    /// can be spliced between the press and the release (a background
    /// removal, a re-seed), and an index would then activate whichever
    /// entry slid into that slot.
    pressed_item: Option<(FavoriteKind, String)>,
    /// Rectangle of the pane the bar is docked in (the 3-D viewport
    /// pane) — the basis of the [`MAX_WIDTH_FRACTION`] cap. Published by
    /// the [`PaneRectProbe`](crate::favorites_bar_host::PaneRectProbe)
    /// that wraps the pane; see that module for why the bar cannot read
    /// it off its own `available.width`.
    pane: PaneRect,
    /// Rectangle of the node-canvas pane, in the same (splitter) space —
    /// the drag-insert drop target, which since 6f-1 is in the *other*
    /// pane of the splitter.
    canvas_pane: PaneRect,
}

impl FavoritesBar {
    /// `pane` is the rectangle of the pane the bar is docked in (its
    /// width caps the panel); `canvas_pane` is the node-canvas pane's,
    /// which the drag controller needs as a drop target. Both are
    /// published by
    /// [`PaneRectProbe`](crate::favorites_bar_host::PaneRectProbe)s in
    /// the same coordinate space (the splitter's).
    pub fn new(
        state: AppState,
        dialogs: Arc<dyn FileDialogProvider>,
        pane: PaneRect,
        canvas_pane: PaneRect,
    ) -> Self {
        FavoritesBar {
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::LEFT)
                .with_v_anchor(VAnchor::STRETCH),
            state,
            dialogs,
            cache: ThumbnailCache::new(),
            model: None,
            children: Vec::new(),
            layout: geom::compute(Size::new(0.0, 0.0), false, 0, false, 0.0),
            items: Vec::new(),
            show_pin: false,
            drag: None,
            insert: None,
            scroll: 0.0,
            pressed_item: None,
            pane,
            canvas_pane,
        }
    }

    /// Attach the drag-insert controller (step 6e). The shell builds it
    /// from the app's floating-overlay handle — the ghost has to live at
    /// the top of the window's `Stack`, which only the shell can reach.
    pub fn with_drag_insert(mut self, insert: DragInsertHandle) -> Self {
        self.insert = Some(insert);
        self
    }

    pub fn expanded(&self) -> bool {
        *self.state.favorites_bar_expanded.lock().unwrap()
    }

    fn set_expanded(&self, expanded: bool) {
        *self.state.favorites_bar_expanded.lock().unwrap() = expanded;
    }

    /// The stored panel width, clamped to what the current host can show.
    pub fn stored_width(&self) -> f64 {
        let stored = clamp_stored_width(*self.state.favorites_bar_width.lock().unwrap()) as f64;
        stored.min(self.max_panel_width())
    }

    fn set_stored_width(&self, width: f64) {
        *self.state.favorites_bar_width.lock().unwrap() = clamp_stored_width(width as f32);
    }

    /// Widest the *panel* may open right now. Before the first layout the
    /// pane width is unknown, and "unknown" must not read as "no room" —
    /// the persisted cap stands until the probe publishes.
    fn max_panel_width(&self) -> f64 {
        let pane = self.pane.width();
        if pane <= 0.0 {
            return MAX_STORED_W as f64;
        }
        // The strip and the handle are never squeezed out, and neither is
        // a usable 3-D viewport, so the cap is whichever is smaller: the
        // ancestor's fraction of the pane, or what is left beside them
        // once [`MIN_VIEWPORT_W`] is reserved.
        (pane * MAX_WIDTH_FRACTION).min((pane - COLLAPSED_W - MIN_VIEWPORT_W).max(0.0))
    }

    /// Width of the browser panel right now — the live gesture width
    /// mid-drag, the stored width while open, zero while collapsed.
    pub fn panel_width(&self) -> f64 {
        match self.drag.as_ref().filter(|d| d.is_resizing()) {
            Some(drag) => clamp_panel(drag.raw(), self.max_panel_width()),
            None => {
                if self.expanded() {
                    self.stored_width()
                } else {
                    0.0
                }
            }
        }
    }

    /// Total width the bar occupies: strip + panel + handle.
    pub fn visible_width(&self) -> f64 {
        COLLAPSED_W + self.panel_width()
    }

    /// The project the "pin current project" item would pin, if there is
    /// one and it is not pinned already.
    fn pinnable_project(&self) -> Option<StorageUri> {
        let uri = self.state.current_file.lock().unwrap().clone()?;
        let pinned = self
            .state
            .favorites
            .lock()
            .unwrap()
            .contains(FavoriteKind::Project, &uri.to_string());
        (!pinned).then_some(uri)
    }

    /// Mount the embedded browser on the first expand; drop it when the
    /// panel goes away. The model survives so navigation is not lost.
    fn sync_children(&mut self, panel_open: bool) {
        if panel_open {
            if self.children.is_empty() {
                let browser = self.build_browser();
                self.children.push(Box::new(browser));
            }
        } else {
            self.children.clear();
        }
    }

    /// The shared browser in its embedded face: no OK/Cancel footer, no
    /// name field, no keyboard capture — picks arrive through
    /// `on_activate` (design §2, row 1).
    fn build_browser(&mut self) -> FileBrowser {
        let model = match self.model.clone() {
            Some(model) => model,
            None => {
                let model = BrowserModel::opened_on(&self.state);
                self.model = Some(model.clone());
                model
            }
        };
        let state = self.state.clone();
        let dialogs = self.dialogs.clone();
        let mut browser = FileBrowser::new(
            self.state.clone(),
            model,
            self.cache.clone(),
            BrowserMode::Embedded,
        )
        .with_id(EMBEDDED_BROWSER_ID);
        // Third drag surface (design §1.3): entries in the embedded grid
        // feed the same controller the strip does.
        if let Some(insert) = self.insert.clone() {
            browser = browser.with_drag_insert(insert);
        }
        browser.on_activate(move |entry| {
            // Only projects are actionable here in v1; a mesh activated
            // in the bar is a drag-insert concern, not an open.
            if uri_extension(&entry.uri) == crate::file_browser::PROJECT_EXTENSION {
                crate::menu_actions::open_project_gated(&state, &dialogs, entry.uri.clone());
            }
        })
    }

    /// Index of the strip item under `pos`, if any. A scrolled-away
    /// item is not clickable, so the viewport gates the hit exactly as
    /// the paint clip does.
    fn item_at(&self, pos: Point) -> Option<usize> {
        if !self.layout.items_viewport.contains(pos) {
            return None;
        }
        self.layout.items.iter().position(|r| r.contains(pos))
    }

    /// Activate the favourite the press landed on, looked up by identity
    /// rather than position (see [`FavoritesBar::pressed_item`]).
    fn activate_item(&mut self, key: &(FavoriteKind, String)) {
        let Some(item) = self
            .items
            .iter()
            .find(|i| i.kind == key.0 && i.stable_key == key.1)
        else {
            return;
        };
        match item.kind {
            // Inserting a node type from the bar is a *drag*; clicking
            // one is deliberately inert rather than quietly adding a
            // node the user did not place.
            FavoriteKind::NodeType => {}
            FavoriteKind::Project => {
                if let Ok(uri) = item.stable_key.parse::<StorageUri>() {
                    crate::menu_actions::open_project_gated(&self.state, &self.dialogs, uri);
                }
            }
        }
    }

    fn pin_current_project(&mut self) {
        if let Some(uri) = self.pinnable_project() {
            self.state
                .favorites
                .lock()
                .unwrap()
                .add(Favorite::project(&uri));
            agg_gui::animation::request_draw();
        }
    }

    /// The node canvas's rectangle in bar-local coordinates — the
    /// drag-insert drop target in the splitter's *other* pane. Derived in
    /// [`favorites_bar_host`](crate::favorites_bar_host), which owns the
    /// pane-rect arithmetic.
    fn canvas_rect_local(&self) -> Option<Rect> {
        crate::favorites_bar_host::canvas_rect_local(self.pane.get(), self.canvas_pane.get())
    }

    /// The 3-D viewport's rectangle in bar-local coordinates — the
    /// second drop target (step 6f-4), likewise derived in
    /// [`favorites_bar_host`](crate::favorites_bar_host).
    fn viewport_rect_local(&self, bar: Size) -> Option<Rect> {
        crate::favorites_bar_host::viewport_rect_local(self.pane.get(), bar.width)
    }

    fn on_mouse_down(&mut self, pos: Point) -> EventResult {
        if self.layout.handle.contains(pos) {
            // A press that starts a *different* gesture takes over the
            // mouse capture, so the drag-insert in flight would never
            // see its release: end it here rather than orphan whatever
            // it was carrying.
            if let Some(insert) = self.insert.clone() {
                insert.cancel();
            }
            self.pressed_item = None;
            self.drag = Some(HandleGesture::begin(pos.x, self.panel_width()));
            return EventResult::Consumed;
        }
        if let Some(index) = self.item_at(pos) {
            // Activation is deferred to the release: this press may
            // still turn into a drag, and a drag must not *also* open
            // the project it was carrying.
            self.pressed_item = self
                .items
                .get(index)
                .map(|item| (item.kind, item.stable_key.clone()));
            let payload = self.items.get(index).and_then(StripItem::payload);
            if let (Some(insert), Some(payload)) = (self.insert.clone(), payload) {
                insert.press(payload, pos);
            }
            return EventResult::Consumed;
        }
        if self.layout.pin.is_some_and(|rect| rect.contains(pos)) {
            self.pin_current_project();
            return EventResult::Consumed;
        }
        // The bar is opaque chrome: a click on its background must not
        // fall through to whatever is behind it.
        EventResult::Consumed
    }

    /// Wheel over the strip scrolls the favourites (ND's
    /// `overflow-y: auto`). agg-gui's sign convention: positive
    /// `delta_y` means "show me what is above", i.e. *decrease* the
    /// offset. The clamp is re-applied every layout, so a wheel spun
    /// against a short palette does nothing at all.
    fn on_wheel(&mut self, pos: Point, delta_y: f64) -> EventResult {
        if !self.layout.strip.contains(pos) || self.max_scroll() <= 0.0 {
            return EventResult::Ignored;
        }
        let next = (self.scroll - delta_y * SCROLL_STEP).clamp(0.0, self.max_scroll());
        if next != self.scroll {
            self.scroll = next;
            agg_gui::animation::request_draw();
        }
        EventResult::Consumed
    }

    /// Furthest the strip may scroll at the size it was last laid out
    /// at. Zero whenever every favourite already fits.
    fn max_scroll(&self) -> f64 {
        geom::max_scroll(
            Size::new(self.bounds.width, self.bounds.height),
            self.items.len(),
            self.show_pin,
        )
    }

    fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        let Some(drag) = self.drag.as_mut() else {
            // No handle gesture — the press may instead be a
            // drag-insert (a favourite on its way to the canvas).
            if let Some(insert) = self.insert.clone() {
                if insert.pointer_move(pos) {
                    return EventResult::Consumed;
                }
            }
            return EventResult::Ignored;
        };
        if !drag.pointer_x(pos.x) {
            // Still inside the toggle threshold: the release will be a
            // click, and there is nothing to redraw.
            return EventResult::Consumed;
        }
        // Pull-open: a rightward drag out of the collapsed bar opens the
        // panel as it sizes. The *width* is deliberately not committed
        // here — see [`FavoritesBar::on_mouse_up`]. Mid-drag the panel
        // renders the gesture's raw width anyway, so the user still sees
        // it follow the pointer.
        if drag.wants_open() {
            self.set_expanded(true);
        }
        agg_gui::animation::request_draw();
        EventResult::Consumed
    }

    /// End of the gesture — and the only place the stored width moves.
    ///
    /// Committing on each `MouseMove` looks equivalent and is not: a drag
    /// that closes the panel sweeps through every width on its way down,
    /// so the last one above the threshold would be written just before
    /// the release. "Snap closed keeps the stored width" would then mean
    /// "keeps ≈120 px", not the width the user actually sized to. So a
    /// released-narrow gesture writes nothing at all and the previous
    /// size stands.
    fn on_mouse_up(&mut self, pos: Point) -> EventResult {
        let Some(drag) = self.drag.take() else {
            // Drag-insert release: a sub-threshold press is still the
            // item's click, anything else was handled by the controller.
            let pressed = self.pressed_item.take();
            if let Some(insert) = self.insert.clone() {
                match insert.pointer_up(pos) {
                    GestureEnd::Click => {
                        if let Some(key) = pressed {
                            self.activate_item(&key);
                        }
                        return EventResult::Consumed;
                    }
                    GestureEnd::Dropped | GestureEnd::Cancelled => {
                        agg_gui::animation::request_draw();
                        return EventResult::Consumed;
                    }
                    GestureEnd::None => {}
                }
            }
            // No controller attached: keep the pre-drag-insert behaviour
            // of activating the pressed item.
            if let Some(key) = pressed {
                self.activate_item(&key);
                return EventResult::Consumed;
            }
            return EventResult::Ignored;
        };
        if !drag.is_resizing() {
            // A press released in place is the toggle.
            let expanded = self.expanded();
            self.set_expanded(!expanded);
        } else if drag.wants_open() {
            self.set_expanded(true);
            self.set_stored_width(clamp_panel(drag.raw(), self.max_panel_width()));
        } else {
            // Snap closed — and keep the stored width, so the next open
            // is the size the user had chosen.
            self.set_expanded(false);
        }
        agg_gui::animation::request_draw();
        EventResult::Consumed
    }
}

impl Widget for FavoritesBar {
    fn type_name(&self) -> &'static str {
        "FavoritesBar"
    }
    fn id(&self) -> Option<&str> {
        Some(BAR_ID)
    }
    fn bounds(&self) -> Rect {
        self.bounds
    }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        &mut self.children
    }
    fn margin(&self) -> Insets {
        Insets::ZERO
    }
    fn h_anchor(&self) -> HAnchor {
        self.base.h_anchor
    }
    fn v_anchor(&self) -> VAnchor {
        self.base.v_anchor
    }
    fn min_size(&self) -> Size {
        self.base.min_size
    }
    fn max_size(&self) -> Size {
        self.base.max_size
    }
    fn widget_base(&self) -> Option<&WidgetBase> {
        Some(&self.base)
    }
    fn widget_base_mut(&mut self) -> Option<&mut WidgetBase> {
        Some(&mut self.base)
    }

    /// Reports its *own* width, which is what makes the bar a fixed
    /// (non-flex) child of the row it shares with the 3-D viewport: the
    /// viewport takes whatever is left.
    fn layout(&mut self, available: Size) -> Size {
        let panel_w = self.panel_width();
        self.sync_children(panel_w > 0.0);
        let width = (COLLAPSED_W + panel_w).min(available.width.max(0.0));
        let size = Size::new(width, available.height);
        self.bounds = Rect::new(0.0, 0.0, size.width, size.height);
        self.items = favorites_strip::collect_items(&self.state);
        self.show_pin = self.pinnable_project().is_some();
        // Re-clamp the scroll offset against *this* size: a taller
        // window, or an unpinned favourite, can shrink the range under a
        // user who scrolled to the bottom.
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        self.layout = geom::compute(
            size,
            panel_w > 0.0,
            self.items.len(),
            self.show_pin,
            self.scroll,
        );
        // Publish the drop target the drag controller hit-tests against:
        // the node canvas, which lives in the splitter's *other* pane
        // (see `drag_insert`'s coordinate notes).
        if let (Some(insert), Some(canvas)) = (&self.insert, self.canvas_rect_local()) {
            insert.set_canvas_rect(canvas);
        }
        // …and the second drop target (step 6f-4): the 3-D viewport
        // beside the bar, in the same pane.
        if let (Some(insert), Some(viewport)) = (&self.insert, self.viewport_rect_local(size)) {
            insert.set_viewport_rect(viewport);
        }
        if let (Some(child), Some(rect)) = (self.children.first_mut(), self.layout.panel) {
            // Layout *then* place: agg-gui widgets reset their own origin
            // in `layout` (see `FileBrowser`'s module docs).
            child.layout(Size::new(rect.width, rect.height));
            child.set_bounds(rect);
        }
        size
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        crate::favorites_bar_paint::paint_bar(self, ctx);
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        match event {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => self.on_mouse_down(*pos),
            Event::MouseWheel { pos, delta_y, .. } => self.on_wheel(*pos, *delta_y),
            Event::MouseMove { pos } => self.on_mouse_move(*pos),
            Event::MouseUp {
                pos,
                button: MouseButton::Left,
                ..
            } => self.on_mouse_up(*pos),
            _ => EventResult::Ignored,
        }
    }

    /// Escape aborts a drag-insert: the carried node goes away and the
    /// ghost drops, leaving the undo stack untouched.
    ///
    /// It arrives here rather than through `on_event` because nothing in
    /// the bar takes keyboard focus — agg-gui offers focus-less keys to
    /// the visible tree through this hook.
    fn on_unconsumed_key(
        &mut self,
        key: &agg_gui::Key,
        _modifiers: agg_gui::Modifiers,
    ) -> EventResult {
        if !matches!(key, agg_gui::Key::Escape) {
            return EventResult::Ignored;
        }
        match self.insert.clone() {
            Some(insert) if insert.cancel() => {
                self.pressed_item = None;
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    /// Bar state for the inspector and the UI tests (design §6 — the same
    /// reflection channel `StatusBar` and `FileBrowser` use).
    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![
            ("expanded", self.expanded().to_string()),
            ("width", format!("{:.1}", self.visible_width())),
            ("panel_width", format!("{:.1}", self.panel_width())),
            (
                "stored_width",
                format!("{:.1}", *self.state.favorites_bar_width.lock().unwrap()),
            ),
            // How many favourites the strip actually placed this frame —
            // the strip is on screen collapsed *and* expanded, so this
            // must not depend on `expanded`.
            ("favorites", self.items.len().to_string()),
            // How many of them are *on screen* right now — the rest are
            // scrolled away, not dropped.
            (
                "strip_items",
                self.layout
                    .items
                    .iter()
                    .filter(|r| geom::item_visible(**r, self.layout.items_viewport))
                    .count()
                    .to_string(),
            ),
            ("scroll", format!("{:.1}", self.scroll)),
            ("max_scroll", format!("{:.1}", self.max_scroll())),
            (
                "dead",
                self.items.iter().filter(|i| !i.alive).count().to_string(),
            ),
            // How many favourites are showing a *rendered* primitive
            // rather than the glyph fallback — the 6f-2 fill-in probe.
            (
                "icons",
                self.items
                    .iter()
                    .filter(|i| i.icon.is_some())
                    .count()
                    .to_string(),
            ),
            // `dragging` covers both gestures the bar can be running: a
            // handle resize and a drag-insert past its threshold
            // (design §6 — the harness's drag-in-flight probe).
            (
                "dragging",
                (self.drag.is_some() || self.insert.as_ref().is_some_and(|i| i.is_dragging()))
                    .to_string(),
            ),
            (
                "carrying",
                self.insert
                    .as_ref()
                    .and_then(|i| i.carried_node())
                    .is_some()
                    .to_string(),
            ),
            (
                "resizing",
                self.drag
                    .as_ref()
                    .is_some_and(|d| d.is_resizing())
                    .to_string(),
            ),
        ]
    }
}

/// Read access for the paint module, which lives in its own file to keep
/// both under the 800-line cap.
impl FavoritesBar {
    pub(crate) fn layout_rects(&self) -> &BarLayout {
        &self.layout
    }
    pub(crate) fn strip_item(&self, index: usize) -> Option<&StripItem> {
        self.items.get(index)
    }
    pub(crate) fn strip_items(&self) -> &[StripItem] {
        &self.items
    }
    pub(crate) fn app_state(&self) -> &AppState {
        &self.state
    }
}
