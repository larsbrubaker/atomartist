//! `FileBrowser` — the shared browsing widget (design §4, step 6b-3).
//!
//! One widget, painted from a [`BrowserModel`] and a [`ThumbnailCache`],
//! that the Open/Save modal (6c) and the favorites bar (6d) both embed. It
//! owns no navigation logic of its own: every click turns into a
//! `BrowserModel` call, and everything it paints is read back out of the
//! model, so the widget-free tests in `model_tests.rs` remain the
//! authority on *what* navigation does.
//!
//! # What it shows
//!
//! - **Provider sidebar** — one row per [`BrowserModel::roots`], in
//!   registration order; clicking one navigates to that provider's root
//!   and the row for the current provider is highlighted.
//! - **Breadcrumb strip** — [`BrowserModel::breadcrumbs`], each crumb
//!   clickable. (`crate::breadcrumb_bar` is the *node-graph* drill trail,
//!   bound to `AppState`'s drill stack; the crumbs here are storage URIs,
//!   so they get their own row rather than a forced-generic shared one.)
//! - **Listing grid** — [`BrowserModel::visible_entries`] tiled into
//!   scrollable cells: preview or fallback glyph plus the entry name.
//!   Single click selects, double click enters a directory or *activates*
//!   a file (see below). The three non-`Ready` [`Listing`] states each
//!   paint their own message — never a blank pane (design §2).
//! - **Search field** — bound to [`BrowserModel::set_search`], which
//!   filters the current listing.
//! - **Name field** — save mode only ([`BrowserMode::Save`]), pre-filled
//!   from the selection the way both ancestors do.
//!
//! # Activation is a callback, not a behaviour
//!
//! Double-clicking a *file* fires [`FileBrowser::on_activate`] and does
//! nothing else. What a pick means — open the project, fill a save name,
//! insert a part — belongs to the host (modal, bar), which is exactly the
//! `mountEmbedded()` split NodeDesigner's dialog uses.
//!
//! # The visibility round runs in `layout`
//!
//! [`ThumbnailCache`] only fetches previews for rows re-requested during
//! the current frame, so *something* has to declare the visible rows once
//! per frame. That is `layout`, not `paint`: every shell calls
//! `App::layout` once per frame right before painting (`demo-native`'s
//! frame loop), layout is where the visible range is computed anyway, and
//! doing it there keeps the gate honest in the headless UI-test harness,
//! which never paints. `paint` then only reads what layout already asked
//! for.
//!
//! Rows are requested **bottom-up** so the *top* row is the most recently
//! requested one: the cache serves its queue most-recent-first, so this is
//! what makes a freshly-scrolled grid fill in from the top down instead of
//! from the bottom up.
//!
//! # Hosts must `set_bounds` *after* `layout`
//!
//! [`Widget::layout`] resets this widget's bounds to the origin
//! (`0, 0, w, h`) — it describes a size, not a placement — so a host that
//! positions the browser itself must call `set_bounds` **after** laying it
//! out, or the placement is thrown away. Anything that reads the widget's
//! absolute rectangle later (`agg_gui::widget::find_widget_screen_rect`,
//! which walks the tree's transforms) depends on that ordering; getting it
//! backwards is exactly what made the 6a thumbnail capture frame the node
//! canvas instead of the viewport.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use agg_gui::widgets::multi_click::MultiClickTracker;
use agg_gui::{
    font_settings, DrawCtx, Event, EventResult, HAnchor, Insets, MouseButton, Point, Rect, Size,
    TextField, VAnchor, Widget, WidgetBase,
};
use atomartist_storage::Entry;

use super::model::{BrowserModel, Crumb, Listing, ProviderRoot};
use super::thumbs::{ThumbState, ThumbnailCache, DEFAULT_THUMB_SIZE};
use super::widget_geom::{
    self as geom, BrowserLayout, GridGeometry, CELL_H, FONT_SIZE, NAME_H, SEARCH_H,
};
use crate::app_state::AppState;

/// Which face of the browser is on screen (design §2, row 1).
///
/// v1 carries the two the modal needs; the bar's embedded `component`
/// face joins in 6d, and adding a variant here is additive because the
/// only thing the enum drives is the name field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    /// Pick an existing entry. No name field.
    Open,
    /// Name a destination. Shows the name field, pre-filled from the
    /// selection.
    Save,
}

impl BrowserMode {
    pub fn shows_name_field(self) -> bool {
        matches!(self, BrowserMode::Save)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BrowserMode::Open => "Open",
            BrowserMode::Save => "Save",
        }
    }
}

/// Everything `layout` derives and `paint` / `on_event` consume. Kept in
/// one struct so the two never read half-updated state.
pub(super) struct Frame {
    pub layout: BrowserLayout,
    pub roots: Vec<ProviderRoot>,
    pub sidebar_rows: Vec<Rect>,
    pub current_scheme: Option<String>,
    pub crumbs: Vec<Crumb>,
    pub crumb_rects: Vec<Rect>,
    pub listing: Listing,
    pub entries: Vec<Entry>,
    pub grid: GridGeometry,
    pub visible: Range<usize>,
    /// Preview state per *visible* entry, index-aligned with `visible`.
    pub thumbs: Vec<ThumbState>,
}

impl Frame {
    fn empty() -> Frame {
        Frame {
            layout: BrowserLayout::compute(Size::new(0.0, 0.0), BrowserMode::Open),
            roots: Vec::new(),
            sidebar_rows: Vec::new(),
            current_scheme: None,
            crumbs: Vec::new(),
            crumb_rects: Vec::new(),
            listing: Listing::Loading,
            entries: Vec::new(),
            grid: GridGeometry {
                cols: 1,
                rows: 0,
                content_height: 0.0,
                max_scroll: 0.0,
            },
            visible: 0..0,
            thumbs: Vec::new(),
        }
    }
}

/// The shared browser widget. See the module docs.
pub struct FileBrowser {
    bounds: Rect,
    base: WidgetBase,
    /// `[0]` = search field, `[1]` = name field (save mode). Built lazily
    /// in `layout` because `TextField` needs a font and a widget may be
    /// constructed before the shell installs one.
    children: Vec<Box<dyn Widget>>,
    fields_built: bool,
    state: AppState,
    model: BrowserModel,
    cache: ThumbnailCache,
    mode: BrowserMode,
    /// Live text of the name field — shared with the `TextField` so the
    /// host can read the typed name and the widget can pre-fill it from a
    /// selection.
    name_cell: Rc<RefCell<String>>,
    on_activate: Option<Box<dyn FnMut(&Entry)>>,
    /// Pixels scrolled down from the top of the grid content.
    scroll: f64,
    /// Whether a layout has run — i.e. whether `frame.grid`'s extent is
    /// real. Gates the eager clamp in [`FileBrowser::set_scroll_offset`].
    laid_out: bool,
    clicks: MultiClickTracker,
    pub(super) frame: Frame,
}

impl FileBrowser {
    /// Build a browser over an existing model and cache. The caller owns
    /// both (the modal shares its model with its OK button; the bar keeps
    /// its cache across collapses), which is why neither is constructed
    /// here.
    pub fn new(
        state: AppState,
        model: BrowserModel,
        cache: ThumbnailCache,
        mode: BrowserMode,
    ) -> Self {
        FileBrowser {
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
            children: Vec::new(),
            fields_built: false,
            state,
            model,
            cache,
            mode,
            name_cell: Rc::new(RefCell::new(String::new())),
            on_activate: None,
            scroll: 0.0,
            laid_out: false,
            clicks: MultiClickTracker::default(),
            frame: Frame::empty(),
        }
    }

    /// Called when a *file* is double-clicked. Directories are navigated
    /// into instead and never reach this callback.
    pub fn on_activate(mut self, cb: impl FnMut(&Entry) + 'static) -> Self {
        self.on_activate = Some(Box::new(cb));
        self
    }

    pub fn model(&self) -> &BrowserModel {
        &self.model
    }

    pub fn cache(&self) -> &ThumbnailCache {
        &self.cache
    }

    pub fn mode(&self) -> BrowserMode {
        self.mode
    }

    /// Text currently in the save-mode name field (empty in open mode).
    pub fn name_text(&self) -> String {
        self.name_cell.borrow().clone()
    }

    /// Overwrite the name field. The bound `TextField` picks the new value
    /// up on its next layout.
    pub fn set_name_text(&self, name: impl Into<String>) {
        *self.name_cell.borrow_mut() = name.into();
    }

    /// The search text, which lives on the model (it is the filter).
    pub fn search_text(&self) -> String {
        self.model.search()
    }

    /// Entries currently tiled in the grid — the filtered listing.
    pub fn visible_entry_count(&self) -> usize {
        self.frame.entries.len()
    }

    /// Half-open index range of the cells on screen, as of the last
    /// layout.
    pub fn visible_range(&self) -> Range<usize> {
        self.frame.visible.clone()
    }

    /// Grid scroll offset, in pixels down from the top of the content.
    pub fn scroll_offset(&self) -> f64 {
        self.scroll
    }

    /// Scroll the grid. Public so the future modal can reveal a
    /// selection.
    ///
    /// The upper clamp is **deferred to the next layout** when the widget
    /// has not been laid out yet: before the first layout the grid's
    /// extent is zero, so clamping eagerly would turn a
    /// `set_scroll_offset` issued while building the modal into a silent
    /// no-op. `rebuild` clamps every frame regardless, so the offset can
    /// never be observed out of range once a frame has run.
    pub fn set_scroll_offset(&mut self, offset: f64) {
        self.scroll = if self.laid_out {
            offset.clamp(0.0, self.frame.grid.max_scroll)
        } else {
            offset.max(0.0)
        };
    }

    /// Build the two text fields once a font is available.
    fn ensure_fields(&mut self) {
        if self.fields_built {
            return;
        }
        let Some(font) = font_settings::current_system_font() else {
            return;
        };
        let model = self.model.clone();
        let search = TextField::new(font.clone())
            .with_font_size(FONT_SIZE)
            .with_placeholder("Search")
            .with_max_size(Size::new(f64::INFINITY, SEARCH_H))
            .on_change(move |text| model.set_search(text));
        self.children.push(Box::new(search));
        if self.mode.shows_name_field() {
            let name = TextField::new(font)
                .with_font_size(FONT_SIZE)
                .with_placeholder("File name")
                .with_max_size(Size::new(f64::INFINITY, NAME_H))
                .with_text_cell(self.name_cell.clone());
            self.children.push(Box::new(name));
        }
        self.fields_built = true;
    }

    /// Recompute everything paint and event handling read, and run the
    /// thumbnail visibility round for this frame.
    fn rebuild(&mut self, available: Size) {
        let layout = BrowserLayout::compute(available, self.mode);
        let roots = self.model.roots();
        let sidebar_rows = geom::sidebar_rows(layout.sidebar, roots.len());
        let crumbs = self.model.breadcrumbs();
        let crumb_rects = geom::crumb_rects(layout.crumbs, &crumbs);
        let listing = self.model.listing();
        let entries = self.model.visible_entries();
        let grid = geom::grid_geometry(layout.grid, entries.len());

        // A shrinking listing (search typed, directory changed) can leave
        // the old offset past the end of the content.
        self.scroll = self.scroll.clamp(0.0, grid.max_scroll);
        let (start, end) = geom::visible_range(&grid, layout.grid, entries.len(), self.scroll);

        self.frame = Frame {
            layout,
            roots,
            sidebar_rows,
            current_scheme: self.model.provider_scheme(),
            crumbs,
            crumb_rects,
            listing,
            entries,
            grid,
            visible: start..end,
            thumbs: Vec::new(),
        };
        self.laid_out = true;
        self.request_thumbnails();
        self.layout_fields();
    }

    /// One visibility round: exactly the rows on screen, requested
    /// bottom-up so the topmost is the cache's most-recent (and therefore
    /// first-served) key.
    fn request_thumbnails(&mut self) {
        self.cache.begin_frame();
        let range = self.frame.visible.clone();
        let mut states = vec![ThumbState::NotRequested; range.len()];
        for index in range.clone().rev() {
            let entry = &self.frame.entries[index];
            states[index - range.start] = self.cache.request(&self.state, entry);
        }
        // A synchronous provider settles inline, so re-peek the rows that
        // were requested before those completions landed.
        for (offset, index) in range.clone().enumerate() {
            if states[offset].is_pending() {
                states[offset] = self
                    .cache
                    .peek_entry(&self.frame.entries[index], DEFAULT_THUMB_SIZE);
            }
        }
        self.frame.thumbs = states;
    }

    /// Place the search / name fields into the regions layout carved for
    /// them.
    fn layout_fields(&mut self) {
        let search = self.frame.layout.search;
        let name = self.frame.layout.name;
        if let Some(field) = self.children.get_mut(0) {
            field.layout(Size::new(search.width, search.height));
            field.set_bounds(search);
        }
        if let (Some(field), Some(rect)) = (self.children.get_mut(1), name) {
            field.layout(Size::new(rect.width, rect.height));
            field.set_bounds(rect);
        }
    }

    /// Entry under a widget-local point, if the grid is showing one there.
    ///
    /// Derived from the *current* scroll, not from the visible range the
    /// last layout published: a wheel and a click can arrive in the same
    /// batch of queued events, and the click must see the row the wheel
    /// just brought into view rather than fall through to empty space.
    fn entry_at(&self, pos: Point) -> Option<usize> {
        let index =
            geom::cell_index_at(self.frame.layout.grid, &self.frame.grid, pos, self.scroll)?;
        (index < self.frame.entries.len()).then_some(index)
    }

    /// Select `entry`, and in save mode carry its name into the name
    /// field — the ancestors' behaviour, and what makes "click a file,
    /// press Save" overwrite it.
    fn select(&mut self, entry: &Entry) {
        self.model.select(Some(entry.uri.clone()));
        if self.mode.shows_name_field() && !entry.is_dir {
            *self.name_cell.borrow_mut() = entry.name.clone();
        }
    }

    /// Double-click: directories are entered, files are handed to the
    /// host.
    fn activate(&mut self, entry: &Entry) {
        if entry.is_dir {
            self.model.enter_dir(&self.state, entry);
            self.scroll = 0.0;
            return;
        }
        if let Some(cb) = self.on_activate.as_mut() {
            cb(entry);
        }
    }

    fn on_mouse_down(&mut self, pos: Point) -> EventResult {
        let clicks = self.clicks.register(pos);

        if let Some(index) = self
            .frame
            .sidebar_rows
            .iter()
            .position(|row| row.contains(pos))
        {
            let root = self.frame.roots[index].root.clone();
            self.model.navigate_to(&self.state, root);
            self.scroll = 0.0;
            return EventResult::Consumed;
        }

        if self.frame.layout.crumbs.contains(pos) {
            if let Some(index) = self
                .frame
                .crumb_rects
                .iter()
                .position(|rect| rect.contains(pos))
            {
                let uri = self.frame.crumbs[index].uri.clone();
                self.model.navigate_to(&self.state, uri);
                self.scroll = 0.0;
                return EventResult::Consumed;
            }
            return EventResult::Consumed;
        }

        if let Some(index) = self.entry_at(pos) {
            let entry = self.frame.entries[index].clone();
            self.select(&entry);
            // Exactly two, not "two or more": the tracker counts
            // 1, 2, 3, 1, … within its window, so `>= 2` would let a
            // third rapid press activate a second time — a double-click
            // into a folder plus one more tap would land two levels
            // deep, and a file would be handed to the host twice.
            if clicks == 2 {
                self.activate(&entry);
            }
            return EventResult::Consumed;
        }

        // Empty space inside the grid clears the selection, the way both
        // ancestors' file panes do.
        if self.frame.layout.grid.contains(pos) {
            self.model.select(None);
            return EventResult::Consumed;
        }
        EventResult::Ignored
    }
}

impl Widget for FileBrowser {
    fn type_name(&self) -> &'static str {
        "FileBrowser"
    }
    /// Stable id for the harness and the inspector (design §6).
    fn id(&self) -> Option<&str> {
        Some("file-browser")
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
    fn margin(&self) -> Insets {
        self.base.margin
    }
    fn widget_base(&self) -> Option<&WidgetBase> {
        Some(&self.base)
    }

    fn layout(&mut self, available: Size) -> Size {
        self.ensure_fields();
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        self.rebuild(available);
        available
    }

    /// Keep the host drawing while previews are still arriving, so the
    /// grid fills in without needing a mouse move.
    fn needs_draw(&self) -> bool {
        self.frame.listing.is_loading() || self.frame.thumbs.iter().any(ThumbState::is_pending)
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        super::widget_paint::paint_browser(self, ctx);
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        match event {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => self.on_mouse_down(*pos),
            Event::MouseWheel { pos, delta_y, .. } if self.frame.layout.grid.contains(*pos) => {
                // Y-up: a positive wheel delta scrolls the content up,
                // i.e. *decreases* how far down we are.
                let next = self.scroll - delta_y * CELL_H * 0.5;
                self.scroll = next.clamp(0.0, self.frame.grid.max_scroll);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    /// Browser state for the inspector and the UI tests (design §6 — the
    /// same reflection channel `StatusBar` uses).
    fn properties(&self) -> Vec<(&'static str, String)> {
        let listing = match &self.frame.listing {
            Listing::Loading => "Loading".to_string(),
            Listing::Ready(_) => "Ready".to_string(),
            Listing::Empty => "Empty".to_string(),
            Listing::Error(message) => format!("Error: {message}"),
        };
        vec![
            ("mode", self.mode.as_str().to_string()),
            (
                "cwd",
                self.model
                    .cwd()
                    .map(|uri| uri.to_string())
                    .unwrap_or_default(),
            ),
            ("listing", listing),
            ("entries", self.frame.entries.len().to_string()),
            (
                "selected",
                self.model
                    .selected_entry()
                    .map(|entry| entry.name)
                    .unwrap_or_default(),
            ),
            ("search", self.model.search()),
            ("name", self.name_text()),
            (
                "visible",
                format!("{}..{}", self.frame.visible.start, self.frame.visible.end),
            ),
        ]
    }
}
