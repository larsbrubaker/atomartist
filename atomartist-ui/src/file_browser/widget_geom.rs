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

use agg_gui::{font_settings, text::measure_text_metrics, Point, Rect, Size};

use super::model::Crumb;
use super::widget::BrowserMode;

/// Width of the provider sidebar.
pub const SIDEBAR_W: f64 = 150.0;
/// Height of one provider row in the sidebar.
pub const SIDEBAR_ROW_H: f64 = 24.0;
/// Height of the search field.
pub const SEARCH_H: f64 = 24.0;
/// Height of the breadcrumb strip.
pub const CRUMB_H: f64 = 22.0;
/// Height of the save-mode name field.
pub const NAME_H: f64 = 24.0;
/// Gap between the panes and around the chrome.
pub const PAD: f64 = 6.0;
/// One grid cell: a preview box plus a caption line.
pub const CELL_W: f64 = 104.0;
pub const CELL_H: f64 = 104.0;
/// Side of the square preview box inside a cell.
pub const THUMB_BOX: f64 = 72.0;
/// Body font size for sidebar / crumb / caption text.
pub const FONT_SIZE: f64 = 12.0;
/// Horizontal padding inside the breadcrumb strip and the sidebar rows.
pub const TEXT_INSET: f64 = 8.0;
/// Width reserved for the "›" between two crumbs.
pub const CRUMB_SEP_W: f64 = 14.0;

/// The widget's top-level regions, all widget-local and Y-up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrowserLayout {
    pub sidebar: Rect,
    pub search: Rect,
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

        let search_y = (h - PAD - SEARCH_H).max(0.0);
        let search = Rect::new(content_x, search_y, content_w, SEARCH_H.min(h));
        let crumbs_y = (search_y - CRUMB_H).max(0.0);
        let crumbs = Rect::new(content_x, crumbs_y, content_w, CRUMB_H.min(h));

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

/// How the grid tiles: column count plus the scroll extent that follows
/// from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridGeometry {
    pub cols: usize,
    pub rows: usize,
    /// Total height the tiled cells occupy.
    pub content_height: f64,
    /// Largest useful `scroll` — 0 when everything fits.
    pub max_scroll: f64,
}

pub fn grid_geometry(grid: Rect, count: usize) -> GridGeometry {
    let cols = ((grid.width / CELL_W).floor() as usize).max(1);
    let rows = count.div_ceil(cols);
    let content_height = rows as f64 * CELL_H;
    GridGeometry {
        cols,
        rows,
        content_height,
        max_scroll: (content_height - grid.height).max(0.0),
    }
}

/// Rectangle of the cell at `index`, in widget-local Y-up coordinates,
/// with `scroll` pixels of downward scrolling applied.
pub fn cell_rect(grid: Rect, geo: &GridGeometry, index: usize, scroll: f64) -> Rect {
    let row = index / geo.cols;
    let col = index % geo.cols;
    // `row * CELL_H` is a top-down offset into the content; the grid's top
    // edge minus that (plus the scroll) is the cell's top in Y-up space.
    let top = grid.y + grid.height - (row as f64 * CELL_H - scroll);
    Rect::new(grid.x + col as f64 * CELL_W, top - CELL_H, CELL_W, CELL_H)
}

/// Index of the cell under `pos`, derived from `scroll` rather than from
/// a cached visible range.
///
/// Hit-testing must not lean on the range the last `layout` published:
/// agg-gui delivers every queued event between redraws, so a wheel and a
/// click can land in the same batch and the click has to see the scroll
/// the wheel just applied. The caller still bounds the result against the
/// listing length — the arithmetic happily names a cell past the end of a
/// short last row.
pub fn cell_index_at(grid: Rect, geo: &GridGeometry, pos: Point, scroll: f64) -> Option<usize> {
    if !grid.contains(pos) || geo.cols == 0 {
        return None;
    }
    let col = ((pos.x - grid.x) / CELL_W).floor();
    if col < 0.0 || col as usize >= geo.cols {
        return None;
    }
    // Top-down distance from the top of the *content* (not the viewport).
    let down = (grid.y + grid.height) - pos.y + scroll;
    if down < 0.0 {
        return None;
    }
    let row = (down / CELL_H).floor() as usize;
    Some(row * geo.cols + col as usize)
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
    let first_row = (scroll / CELL_H).floor().max(0.0) as usize;
    let last_row = ((scroll + grid.height) / CELL_H).ceil().max(0.0) as usize;
    let start = (first_row * geo.cols).min(count);
    let end = (last_row * geo.cols).min(count);
    (start, end)
}

#[cfg(test)]
#[path = "widget_geom_tests.rs"]
mod widget_geom_tests;
