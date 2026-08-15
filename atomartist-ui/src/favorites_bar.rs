//! `FavoritesBar` — the left, edge-docked favorites rail and its expanded
//! panel (`docs/file-browser-design.md` §1.2, §2, §4, step 6d-2).
//!
//! AtomArtist's descendant of NodeDesigner's parts bar
//! (`static/js/node-editor/ui/parts-bar.js`), moved to the canvas's **left**
//! edge per product direction. Two faces:
//!
//! * **Collapsed** — a narrow rail of favourite glyphs, stacked top-down.
//!   NodeType favourites get their palette category's icon, Project
//!   favourites a file glyph, and anything that no longer
//!   [`resolve`](crate::file_browser::Favorite::resolve)s is greyed rather
//!   than pruned (design §2: the provider may come back).
//! * **Expanded** — the same favourites as labelled rows with an unpin
//!   affordance, above the shared [`FileBrowser`] in its *embedded* face.
//!
//! # The handle is a toggle and a resize grip
//!
//! One 6 px strip on the bar's right edge does both, with the ancestor's
//! constants: a press released within [`DRAG_THRESHOLD`] pixels toggles,
//! and anything further drags the width. Because the bar is docked left,
//! dragging **right** widens it. Pulling right out of the collapsed rail
//! opens the bar and keeps sizing in the same gesture; releasing below
//! [`MIN_EXPANDED_W`] snaps it closed but **keeps** the stored width, so
//! the next open comes back at the user's size.
//!
//! # Lazy mount
//!
//! The panel's browser is built on the first expand and dropped on
//! collapse, so a user who never opens the bar never pays for a listing
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
//! # Not in this step
//!
//! * **Drag-insert** (step 6e). Clicking a `NodeType` favourite is
//!   deliberately inert until then — the bar must not add a node the user
//!   never placed.
//! * **Drag-to-reorder** the row. [`Favorites::move_favorite`] is already
//!   there for it; the gesture belongs with 6e's drag controller, which
//!   owns the threshold / ghost machinery this would otherwise duplicate.
//! * **A scrolling favourites list.** The list takes at most
//!   [`MAX_LIST_FRACTION`](crate::favorites_bar_geom::MAX_LIST_FRACTION)
//!   of the bar and simply does not place what will not fit.
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
use atomartist_lib::registry::NodeDef;
use atomartist_storage::StorageUri;

use crate::app_state::AppState;
use crate::app_state_storage::uri_extension;
use crate::drag_insert::{DragInsertHandle, DragPayload, GestureEnd};
use crate::favorites_bar_geom::{self as geom, BarLayout, RAIL_W};
use crate::favorites_bar_host::PaneWidth;
use crate::file_browser::favorites::{Favorite, FavoriteKind, FavoriteResolution};
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
/// Narrowest usable expanded panel. A drag released below this snaps the
/// bar closed instead of leaving a sliver.
pub const MIN_EXPANDED_W: f32 = 120.0;
/// Width a never-resized bar opens to.
pub const DEFAULT_EXPANDED_W: f32 = 260.0;
/// Largest share of the window the bar may occupy.
pub const MAX_WIDTH_FRACTION: f64 = 0.6;
/// Absolute cap applied when no layout width is known yet (settings
/// parsing runs long before the first frame).
pub const MAX_STORED_W: f32 = 2000.0;

/// Clamp a persisted / restored width into the range the bar can actually
/// open at. Non-finite values (a hand-edited `NaN`) fall back to the
/// default rather than propagating into layout arithmetic.
pub fn clamp_stored_width(width: f32) -> f32 {
    if !width.is_finite() {
        return DEFAULT_EXPANDED_W;
    }
    width.clamp(MIN_EXPANDED_W, MAX_STORED_W)
}

/// One favourite as the bar needs it for a frame: resolved label, glyph,
/// and whether it is still live.
struct RowInfo {
    kind: FavoriteKind,
    stable_key: String,
    label: String,
    glyph: char,
    alive: bool,
}

/// An in-flight handle gesture. Starts as a possible toggle and becomes a
/// resize once the pointer has travelled past [`DRAG_THRESHOLD`].
struct HandleDrag {
    /// Bar-local x of the press. The bar's origin is pinned to the canvas
    /// area's left edge, so this stays comparable as the bar resizes.
    press_x: f64,
    start_w: f64,
    live_w: f64,
    moved: bool,
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
    rows: Vec<RowInfo>,
    /// Whether the pin-current-project row was offered this frame.
    show_pin: bool,
    drag: Option<HandleDrag>,
    /// Drag-insert controller shared with the embedded browser (step
    /// 6e). `None` when the host never supplied one (unit tests that
    /// build a bare bar) — the bar then behaves exactly as it did
    /// before drag-insert existed.
    insert: Option<DragInsertHandle>,
    /// Favourite row the current press landed on. Activation waits for
    /// the release so a press that turns into a drag does not also open
    /// a project (`parts-bar-drag.js`'s rule).
    pressed_row: Option<usize>,
    /// Width of the pane the bar is docked in — the basis of the
    /// [`MAX_WIDTH_FRACTION`] cap. Published by the
    /// [`PaneWidthProbe`](crate::favorites_bar_host::PaneWidthProbe) that
    /// wraps the canvas pane; see that module for why the bar cannot read
    /// it off its own `available.width`.
    pane_w: PaneWidth,
}

impl FavoritesBar {
    /// `pane_w` is the channel the bar's width cap reads; the pane it is
    /// docked in publishes into it through a
    /// [`PaneWidthProbe`](crate::favorites_bar_host::PaneWidthProbe).
    pub fn new(state: AppState, dialogs: Arc<dyn FileDialogProvider>, pane_w: PaneWidth) -> Self {
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
            layout: geom::compute(Size::new(0.0, 0.0), false, 0, false),
            rows: Vec::new(),
            show_pin: false,
            drag: None,
            insert: None,
            pressed_row: None,
            pane_w,
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

    /// The stored open width, clamped to what the current host can show.
    pub fn stored_width(&self) -> f64 {
        let stored = clamp_stored_width(*self.state.favorites_bar_width.lock().unwrap()) as f64;
        stored.min(self.max_width())
    }

    fn set_stored_width(&self, width: f64) {
        *self.state.favorites_bar_width.lock().unwrap() = clamp_stored_width(width as f32);
    }

    /// Widest the bar may open right now. Before the first layout the
    /// pane width is unknown, and "unknown" must not read as "no room" —
    /// the persisted cap stands until the probe publishes.
    fn max_width(&self) -> f64 {
        let pane = self.pane_w.get();
        if pane <= 0.0 {
            return MAX_STORED_W as f64;
        }
        (pane * MAX_WIDTH_FRACTION).max(MIN_EXPANDED_W as f64)
    }

    /// Width the bar occupies right now — the rail when collapsed, the
    /// live gesture width mid-drag, the stored width otherwise.
    pub fn visible_width(&self) -> f64 {
        match self.drag.as_ref().filter(|d| d.moved) {
            Some(drag) => drag.live_w,
            None => {
                if self.expanded() {
                    self.stored_width()
                } else {
                    RAIL_W
                }
            }
        }
    }

    /// The favourites, resolved against the live registry for this frame.
    fn collect_rows(&self) -> Vec<RowInfo> {
        let favorites = self.state.favorites.lock().unwrap().clone();
        favorites
            .list()
            .iter()
            .map(|fav| {
                let (label, glyph, alive) = match fav.resolve(&self.state.registry) {
                    FavoriteResolution::NodeType { def, display_name } => {
                        (display_name, node_type_glyph(def), true)
                    }
                    FavoriteResolution::Project { display_name, .. } => {
                        (display_name, crate::fa::FILE_NEW, true)
                    }
                    // Dead entries keep their stored label and their
                    // kind's glyph; the bar greys them (design §2).
                    FavoriteResolution::Dead => (
                        fav.display_name.clone(),
                        match fav.kind {
                            FavoriteKind::NodeType => crate::fa::CUBE,
                            FavoriteKind::Project => crate::fa::FILE_NEW,
                        },
                        false,
                    ),
                };
                RowInfo {
                    kind: fav.kind,
                    stable_key: fav.stable_key.clone(),
                    label,
                    glyph,
                    alive,
                }
            })
            .collect()
    }

    /// The project the "pin current project" row would pin, if there is
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

    /// Mount the embedded browser on the first expand; drop it on
    /// collapse. The model survives so navigation is not lost.
    fn sync_children(&mut self) {
        if self.expanded() {
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
        // feed the same controller the rail and the rows do.
        if let Some(insert) = self.insert.clone() {
            browser = browser.with_drag_insert(insert);
        }
        browser.on_activate(move |entry| {
            // Only projects are actionable here in v1; a mesh activated
            // in the bar is a 6e drag-insert concern, not an open.
            if uri_extension(&entry.uri) == crate::file_browser::PROJECT_EXTENSION {
                crate::menu_actions::open_project_gated(&state, &dialogs, entry.uri.clone());
            }
        })
    }

    fn activate_row(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        match row.kind {
            // Inserting a node type from the bar is step 6e (drag-drop
            // insert); clicking one is deliberately inert until then
            // rather than quietly adding a node the user did not place.
            FavoriteKind::NodeType => {}
            FavoriteKind::Project => {
                if let Ok(uri) = row.stable_key.parse::<StorageUri>() {
                    crate::menu_actions::open_project_gated(&self.state, &self.dialogs, uri);
                }
            }
        }
    }

    fn unpin_row(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        self.state
            .favorites
            .lock()
            .unwrap()
            .remove(row.kind, &row.stable_key);
        agg_gui::animation::request_draw();
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

    /// What dragging favourite `index` out of the bar would insert.
    /// Dead favourites (unregistered type, unparsable URI) are not
    /// draggable — there is nothing to insert.
    fn row_payload(&self, index: usize) -> Option<DragPayload> {
        let row = self.rows.get(index)?;
        if !row.alive {
            return None;
        }
        match row.kind {
            FavoriteKind::NodeType => Some(DragPayload::NodeType {
                type_id: row.stable_key.clone(),
                label: row.label.clone(),
                glyph: row.glyph,
            }),
            FavoriteKind::Project => Some(DragPayload::File {
                uri: row.stable_key.parse::<StorageUri>().ok()?,
                label: row.label.clone(),
                glyph: row.glyph,
            }),
        }
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
            self.pressed_row = None;
            let width = self.visible_width();
            self.drag = Some(HandleDrag {
                press_x: pos.x,
                start_w: width,
                live_w: width,
                moved: false,
            });
            return EventResult::Consumed;
        }
        let expanded = self.expanded();
        if let Some(index) = self.layout.rows.iter().position(|r| r.contains(pos)) {
            if expanded && geom::unpin_rect(self.layout.rows[index]).contains(pos) {
                self.unpin_row(index);
                return EventResult::Consumed;
            }
            // Activation is deferred to the release: this press may
            // still turn into a drag, and a drag must not *also* open
            // the project it was carrying.
            self.pressed_row = Some(index);
            if let (Some(insert), Some(payload)) = (self.insert.clone(), self.row_payload(index)) {
                insert.press(payload, pos);
            }
            return EventResult::Consumed;
        }
        if self.layout.pin.is_some_and(|rect| rect.contains(pos)) {
            self.pin_current_project();
            return EventResult::Consumed;
        }
        // The bar is opaque chrome: a click on its background must not
        // fall through to the canvas behind it.
        EventResult::Consumed
    }

    fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        // `max_width` reads `self`, so the gesture's numbers come out
        // before the mutable borrow starts.
        let max_width = self.max_width();
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
        let dx = pos.x - drag.press_x;
        if !drag.moved {
            if dx.abs() <= DRAG_THRESHOLD {
                return EventResult::Consumed;
            }
            drag.moved = true;
        }
        let live = (drag.start_w + dx).max(0.0).min(max_width);
        drag.live_w = live;
        // Pull-open: a rightward drag out of the rail expands as it sizes.
        // The *width* is deliberately not committed here — see
        // [`FavoritesBar::on_mouse_up`]. Mid-drag the bar renders
        // `drag.live_w` anyway, so the user still sees it follow the
        // pointer.
        if live >= MIN_EXPANDED_W as f64 {
            self.set_expanded(true);
        }
        agg_gui::animation::request_draw();
        EventResult::Consumed
    }

    /// End of the gesture — and the only place the stored width moves.
    ///
    /// Committing on each `MouseMove` looks equivalent and is not: a drag
    /// that closes the bar sweeps through every width on its way down, so
    /// the last one above [`MIN_EXPANDED_W`] would be written just before
    /// the release. "Snap closed keeps the stored width" would then mean
    /// "keeps ≈120 px", not the width the user actually sized to. So a
    /// released-narrow gesture writes nothing at all and the previous
    /// size stands.
    fn on_mouse_up(&mut self, pos: Point) -> EventResult {
        let Some(drag) = self.drag.take() else {
            // Drag-insert release: a sub-threshold press is still the
            // row's click, anything else was handled by the controller.
            let pressed = self.pressed_row.take();
            if let Some(insert) = self.insert.clone() {
                match insert.pointer_up(pos) {
                    GestureEnd::Click => {
                        if let Some(index) = pressed {
                            self.activate_row(index);
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
            // No controller attached: keep the pre-6e behaviour of
            // activating on the press's row.
            if let Some(index) = pressed {
                self.activate_row(index);
                return EventResult::Consumed;
            }
            return EventResult::Ignored;
        };
        if !drag.moved {
            // A press released in place is the toggle.
            let expanded = self.expanded();
            self.set_expanded(!expanded);
        } else if drag.live_w >= MIN_EXPANDED_W as f64 {
            self.set_stored_width(drag.live_w);
        } else {
            // Snap closed — and keep the stored width, so the next open
            // is the size the user had chosen.
            self.set_expanded(false);
        }
        agg_gui::animation::request_draw();
        EventResult::Consumed
    }
}

/// Palette glyph for a node type — the same category icon the Add Node
/// menu shows, so a favourite reads as the thing it adds.
fn node_type_glyph(def: &Arc<dyn NodeDef>) -> char {
    crate::top_menu_bar::category_icon(def.category()).unwrap_or(crate::fa::CUBE)
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
    /// (non-flex) child of the row it shares with the node canvas: the
    /// canvas takes whatever is left.
    fn layout(&mut self, available: Size) -> Size {
        self.sync_children();
        let width = self.visible_width().min(available.width.max(0.0));
        let size = Size::new(width, available.height);
        self.bounds = Rect::new(0.0, 0.0, size.width, size.height);
        self.rows = self.collect_rows();
        self.show_pin = self.expanded() && self.pinnable_project().is_some();
        self.layout = geom::compute(size, self.expanded(), self.rows.len(), self.show_pin);
        // Publish the canvas-boundary geometry the drag controller
        // hit-tests against: the canvas is the rest of the pane to our
        // right (see `drag_insert`'s coordinate notes).
        if let Some(insert) = &self.insert {
            insert.set_geometry(size.width, self.pane_w.get(), size.height);
        }
        if let (Some(child), Some(rect)) = (self.children.first_mut(), self.layout.browser) {
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
                self.pressed_row = None;
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
            (
                "stored_width",
                format!("{:.1}", *self.state.favorites_bar_width.lock().unwrap()),
            ),
            ("favorites", self.rows.len().to_string()),
            (
                "dead",
                self.rows.iter().filter(|r| !r.alive).count().to_string(),
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
                self.drag.as_ref().is_some_and(|d| d.moved).to_string(),
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
    pub(crate) fn row_glyph(&self, index: usize) -> Option<(char, &str, bool)> {
        self.rows
            .get(index)
            .map(|row| (row.glyph, row.label.as_str(), row.alive))
    }
}
