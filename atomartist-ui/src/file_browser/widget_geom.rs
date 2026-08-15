//! Geometry for the shared file-browser widget — where every region and
//! every clickable row lands inside [`super::widget::FileBrowser`]'s
//! bounds.
//!
//! Split out of `widget.rs` so the widget file stays assembly + event
//! routing and this file stays pure arithmetic: everything here is a free
//! function over rectangles, testable without an [`crate::AppState`], a
//! font, or a frame. `widget_paint.rs` consumes the same values the widget
//! hit-tests against, so what the user clicks is by construction what the
//! user sees.
//!
//! # Coordinate system
//!
//! Widget-local, **Y-up** (agg-gui's convention, CLAUDE.md): the origin is
//! the widget's bottom-left corner and `y + height` is its top edge. The
//! chrome therefore stacks *downward* from `available.height`: search row
//! first, breadcrumbs under it, the grid filling what is left above the
//! (save-mode) name row at the bottom.
//!
//! The grid's own scroll is the one place a top-down number survives:
//! [`GridGeometry::max_scroll`] and the `scroll` argument below are "how
//! far the user has scrolled *down* from the top of the content", matching
//! `agg_gui::widgets::ScrollView`'s `scroll_offset` so the two never
//! disagree if the grid is ever re-hosted inside one.
//!
//! # The tile grid (step 6f-3)
//!
//! NodeDesigner's file grid is CSS `repeat(auto-fill, minmax(120px, 1fr))`
//! with a 12 px gap: as many whole columns of at least 120 px as fit, each
//! then stretched to share the leftover width. [`grid_geometry`] is that
//! rule in arithmetic — which is why a card's width lives on
//! [`GridGeometry`] rather than in a constant, while its *height* is fixed
//! (thumbnail box + two name lines + an optional date line).

use agg_gui::{font_settings, text::measure_text_metrics, Point, Rect, Size};

use super::model::Crumb;
use super::widget::BrowserMode;

/// Width of the provider sidebar.
pub const SIDEBAR_W: f64 = 150.0;
/// Height of one provider row in the sidebar.
pub const SIDEBAR_ROW_H: f64 = 24.0;
/// Height of the search box.
pub const SEARCH_H: f64 = 24.0;
/// Width of the search box, ND's `min-width: 200` plus room for the
/// leading glyph and the trailing clear button. Clamped to the content
/// width on a narrow browser.
pub const SEARCH_W: f64 = 224.0;
/// Slot for the search box's leading magnifier glyph.
pub const SEARCH_GLYPH_W: f64 = 20.0;
/// Side of the round "clear the search" button at the box's right end.
pub const SEARCH_CLEAR_W: f64 = 16.0;
/// Height of the nav row (back button + breadcrumbs).
pub const CRUMB_H: f64 = 28.0;
/// Side of the square Back button at the left of the nav row.
pub const BACK_W: f64 = 28.0;
/// Height of the save-mode name field.
pub const NAME_H: f64 = 24.0;
/// Gap between the panes and around the chrome.
pub const PAD: f64 = 6.0;
/// Narrowest a card may be — ND's `minmax(120px, 1fr)` lower bound.
pub const CARD_MIN_W: f64 = 120.0;
/// Gap between cards, both axes (ND's `gap: 12px`).
pub const GRID_GAP: f64 = 12.0;
/// The card's thumbnail box: ND's 80×60 `object-fit: cover` frame.
pub const THUMB_W: f64 = 80.0;
pub const THUMB_H: f64 = 60.0;
/// Padding inside a card (ND: `padding: 8px 12px`).
pub const CARD_PAD_X: f64 = 12.0;
pub const CARD_PAD_Y: f64 = 8.0;
/// Corner radius of a card, and the width of its selection border.
pub const CARD_RADIUS: f64 = 8.0;
pub const CARD_BORDER: f64 = 2.0;
/// Gap between the thumbnail box and the name below it.
pub const THUMB_NAME_GAP: f64 = 6.0;
/// The name under a card's thumbnail: 12 px, at most two wrapped lines.
pub const NAME_SIZE: f64 = 12.0;
pub const NAME_LINE_H: f64 = 15.0;
pub const NAME_LINES: usize = 2;
/// The optional modified-date line under the name.
pub const DATE_SIZE: f64 = 10.0;
pub const DATE_LINE_H: f64 = 12.0;
/// Height of one card. Fixed: the width is what `1fr` stretches.
pub const CARD_H: f64 =
    CARD_PAD_Y * 2.0 + THUMB_H + THUMB_NAME_GAP + NAME_LINE_H * NAME_LINES as f64 + DATE_LINE_H;
/// Body font size for sidebar / crumb text.
pub const FONT_SIZE: f64 = 12.0;
/// Horizontal padding inside the breadcrumb strip and the sidebar rows.
pub const TEXT_INSET: f64 = 8.0;
/// Width reserved for the "›" between two crumbs.
pub const CRUMB_SEP_W: f64 = 14.0;

/// The widget's top-level regions, all widget-local and Y-up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrowserLayout {
    pub sidebar: Rect,
    /// The whole search box: glyph, text field, and clear button.
    pub search: Rect,
    /// The part of the box the `TextField` child occupies — the box
    /// minus the leading glyph and the trailing clear button, so a click
    /// on either reaches this widget instead of the field.
    pub search_field: Rect,
    /// The round clear button. Painted (and clickable) only while the
    /// search is non-empty; the caller gates that.
    pub search_clear: Rect,
    /// Back button, at the left of the nav row.
    pub back: Rect,
    /// Breadcrumb strip, filling the nav row right of [`Self::back`].
    pub crumbs: Rect,
    pub grid: Rect,
    /// Only in [`BrowserMode::Save`].
    pub name: Option<Rect>,
}

impl BrowserLayout {
    /// Carve `available` into the browser's regions. Degenerate sizes
    /// produce zero-area rectangles rather than negative ones, so the
    /// caller can paint and hit-test them without special cases.
    pub fn compute(available: Size, mode: BrowserMode) -> BrowserLayout {
        let w = available.width.max(0.0);
        let h = available.height.max(0.0);
        let sidebar = Rect::new(0.0, 0.0, SIDEBAR_W.min(w), h);
        let content_x = sidebar.width + PAD;
        let content_w = (w - content_x - PAD).max(0.0);

        // Search sits at the *right* end of its row (ND's top-right
        // corner), not stretched across it.
        let search_y = (h - PAD - SEARCH_H).max(0.0);
        let search_w = SEARCH_W.min(content_w);
        let search = Rect::new(
            content_x + content_w - search_w,
            search_y,
            search_w,
            SEARCH_H.min(h),
        );
        let clear_inset = (SEARCH_H - SEARCH_CLEAR_W) * 0.5;
        let search_clear = Rect::new(
            (search.x + search.width - SEARCH_CLEAR_W - clear_inset).max(search.x),
            search.y + clear_inset,
            SEARCH_CLEAR_W.min(search.width),
            SEARCH_CLEAR_W.min(search.height),
        );
        let field_x = search.x + SEARCH_GLYPH_W;
        let search_field = Rect::new(
            field_x,
            search.y,
            (search_clear.x - clear_inset - field_x).max(0.0),
            search.height,
        );

        let crumbs_y = (search_y - CRUMB_H).max(0.0);
        let back = Rect::new(content_x, crumbs_y, BACK_W.min(content_w), CRUMB_H.min(h));
        let crumbs_x = content_x + back.width + PAD;
        let crumbs = Rect::new(
            crumbs_x,
            crumbs_y,
            (content_x + content_w - crumbs_x).max(0.0),
            CRUMB_H.min(h),
        );

        let name = mode
            .shows_name_field()
            .then(|| Rect::new(content_x, PAD, content_w, NAME_H));
        let grid_bottom = match name {
            Some(rect) => rect.y + rect.height + PAD,
            None => PAD,
        };
        let grid_h = (crumbs_y - PAD - grid_bottom).max(0.0);
        let grid = Rect::new(content_x, grid_bottom, content_w, grid_h);

        BrowserLayout {
            sidebar,
            search,
            search_field,
            search_clear,
            back,
            crumbs,
            grid,
            name,
        }
    }
}

/// Sidebar rows, top-down: row 0 sits against the sidebar's top edge.
pub fn sidebar_rows(sidebar: Rect, count: usize) -> Vec<Rect> {
    (0..count)
        .map(|i| {
            let top = sidebar.y + sidebar.height - PAD - i as f64 * SIDEBAR_ROW_H;
            Rect::new(sidebar.x, top - SIDEBAR_ROW_H, sidebar.width, SIDEBAR_ROW_H)
        })
        .collect()
}

/// Advance of `text` in the current system font, with a proportional
/// estimate when no font is installed so layout stays stable headless.
pub fn measure(text: &str, size: f64) -> f64 {
    match font_settings::current_system_font() {
        Some(font) => measure_text_metrics(&font, text, size).width,
        None => text.chars().count() as f64 * size * 0.55,
    }
}

/// Truncate `text` with an ellipsis so it fits `max_w` at `size`.
///
/// Lives beside [`measure`] because it *is* a measuring decision, and one
/// copy serves the browser's chrome, its cards, and the favorites strip's
/// labels — the three places that used to carry the same four lines.
pub fn elide(text: &str, max_w: f64, size: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if measure(text, size) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push('…');
        if measure(&candidate, size) > max_w {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

/// Clickable rectangle per crumb, laid left to right from the strip's left
/// edge. Crumbs that run past the strip's right edge still get a rectangle
/// (so paint and hit-test agree); the caller clips them.
pub fn crumb_rects(area: Rect, crumbs: &[Crumb]) -> Vec<Rect> {
    let mut out = Vec::with_capacity(crumbs.len());
    let mut x = area.x + TEXT_INSET;
    for (i, crumb) in crumbs.iter().enumerate() {
        if i > 0 {
            x += CRUMB_SEP_W;
        }
        let w = measure(&crumb.label, FONT_SIZE);
        out.push(Rect::new(x, area.y, w, area.height));
        x += w;
    }
    out
}

/// How the grid tiles: the auto-fill column count, the width `1fr`
/// stretched a card to, and the scroll extent that follows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridGeometry {
    pub cols: usize,
    pub rows: usize,
    /// Width of one card after the leftover space is shared out.
    pub card_w: f64,
    /// Height of one card. Constant ([`CARD_H`]), carried here so callers
    /// never have to know which of the two axes is fixed.
    pub card_h: f64,
    /// Total height the tiled cards occupy, gaps between rows included
    /// and no trailing gap.
    pub content_height: f64,
    /// Largest useful `scroll` — 0 when everything fits.
    pub max_scroll: f64,
}

impl GridGeometry {
    /// Distance from one card's left edge to the next one's.
    pub fn col_pitch(&self) -> f64 {
        self.card_w + GRID_GAP
    }

    /// Distance from one row's top edge to the next one's.
    pub fn row_pitch(&self) -> f64 {
        self.card_h + GRID_GAP
    }
}

/// `repeat(auto-fill, minmax(CARD_MIN_W, 1fr))` with a [`GRID_GAP`] gap:
/// as many whole `CARD_MIN_W` columns as fit (counting the gaps between
/// them), each then widened to share the remainder.
pub fn grid_geometry(grid: Rect, count: usize) -> GridGeometry {
    let cols = (((grid.width + GRID_GAP) / (CARD_MIN_W + GRID_GAP)).floor() as usize).max(1);
    let card_w = ((grid.width - (cols - 1) as f64 * GRID_GAP) / cols as f64).max(0.0);
    let rows = count.div_ceil(cols);
    let content_height = match rows {
        0 => 0.0,
        n => n as f64 * (CARD_H + GRID_GAP) - GRID_GAP,
    };
    GridGeometry {
        cols,
        rows,
        card_w,
        card_h: CARD_H,
        content_height,
        max_scroll: (content_height - grid.height).max(0.0),
    }
}

/// Rectangle of the card at `index`, in widget-local Y-up coordinates,
/// with `scroll` pixels of downward scrolling applied.
pub fn cell_rect(grid: Rect, geo: &GridGeometry, index: usize, scroll: f64) -> Rect {
    let row = index / geo.cols.max(1);
    let col = index % geo.cols.max(1);
    // `row * row_pitch` is a top-down offset into the content; the grid's
    // top edge minus that (plus the scroll) is the card's top in Y-up
    // space.
    let top = grid.y + grid.height - (row as f64 * geo.row_pitch() - scroll);
    Rect::new(
        grid.x + col as f64 * geo.col_pitch(),
        top - geo.card_h,
        geo.card_w,
        geo.card_h,
    )
}

/// Index of the card under `pos`, derived from `scroll` rather than from
/// a cached visible range.
///
/// Hit-testing must not lean on the range the last `layout` published:
/// agg-gui delivers every queued event between redraws, so a wheel and a
/// click can land in the same batch and the click has to see the scroll
/// the wheel just applied. The caller still bounds the result against the
/// listing length — the arithmetic happily names a card past the end of a
/// short last row.
///
/// A point in the [`GRID_GAP`] between two cards belongs to neither, so
/// it reports `None` and the caller treats it as empty space (which
/// clears the selection, exactly as ND's grid background does).
pub fn cell_index_at(grid: Rect, geo: &GridGeometry, pos: Point, scroll: f64) -> Option<usize> {
    if !grid.contains(pos) || geo.cols == 0 || geo.card_w <= 0.0 {
        return None;
    }
    let across = pos.x - grid.x;
    if across < 0.0 {
        return None;
    }
    let col = (across / geo.col_pitch()).floor();
    if col < 0.0 || col as usize >= geo.cols || across - col * geo.col_pitch() > geo.card_w {
        return None;
    }
    // Top-down distance from the top of the *content* (not the viewport).
    let down = (grid.y + grid.height) - pos.y + scroll;
    if down < 0.0 {
        return None;
    }
    let row = (down / geo.row_pitch()).floor();
    if down - row * geo.row_pitch() > geo.card_h {
        return None;
    }
    Some(row as usize * geo.cols + col as usize)
}

/// Half-open index range of the cells that intersect the grid viewport.
///
/// Rows are whole: a row peeking in by one pixel counts as visible, which
/// is exactly the gate the thumbnail cache wants — a row the user can see
/// any part of should be fetching its preview.
pub fn visible_range(geo: &GridGeometry, grid: Rect, count: usize, scroll: f64) -> (usize, usize) {
    if count == 0 || geo.cols == 0 || grid.height <= 0.0 {
        return (0, 0);
    }
    let pitch = geo.row_pitch();
    let first_row = (scroll / pitch).floor().max(0.0) as usize;
    let last_row = ((scroll + grid.height) / pitch).ceil().max(0.0) as usize;
    let start = (first_row * geo.cols).min(count);
    let end = (last_row * geo.cols).min(count);
    (start, end)
}

#[cfg(test)]
#[path = "widget_geom_tests.rs"]
mod widget_geom_tests;
