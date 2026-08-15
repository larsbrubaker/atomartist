//! Unit tests for [`super`] — the browser widget's pure geometry.
//!
//! These need no `AppState`, no font and no frame, which is the point of
//! keeping the arithmetic in its own module: the tiling, the scroll
//! extent, and the visible-row gate can be pinned exactly, while the
//! widget tests in `atomartist-ui-test` cover the wiring.

use super::*;
use agg_gui::Point;
use atomartist_storage::StorageUri;

fn crumb(label: &str) -> Crumb {
    Crumb {
        label: label.to_string(),
        uri: StorageUri::new("mem", "/"),
    }
}

/// Chrome stacks downward from the top edge, and the grid takes what is
/// left — with the save-mode name row eating into its bottom.
#[test]
fn save_mode_gives_the_grid_less_height_than_open_mode() {
    let size = Size::new(800.0, 600.0);
    let open = BrowserLayout::compute(size, BrowserMode::Open);
    let save = BrowserLayout::compute(size, BrowserMode::Save);

    assert!(open.name.is_none());
    let name = save.name.expect("save mode shows the name field");
    assert!(name.y < save.grid.y, "the name row sits below the grid");
    assert!(
        save.grid.height < open.grid.height,
        "the name row must come out of the grid, not off the bottom of the widget"
    );
    // Search on top, crumbs directly under it.
    assert!(open.crumbs.y + open.crumbs.height <= open.search.y + 0.001);
    assert!(open.grid.y + open.grid.height <= open.crumbs.y);
}

/// Row 0 is the top row: Y-up means later rows have *smaller* y.
#[test]
fn sidebar_rows_run_top_down() {
    let sidebar = Rect::new(0.0, 0.0, SIDEBAR_W, 400.0);
    let rows = sidebar_rows(sidebar, 3);
    assert_eq!(rows.len(), 3);
    assert!(rows[0].y > rows[1].y && rows[1].y > rows[2].y);
    assert!(rows[0].y + rows[0].height <= sidebar.height);
}

/// Crumbs are laid left to right with room for the separator glyph, and
/// never overlap.
#[test]
fn crumb_rects_advance_left_to_right() {
    let area = Rect::new(150.0, 100.0, 500.0, CRUMB_H);
    let crumbs = vec![crumb("This PC"), crumb("Projects"), crumb("Robots")];
    let rects = crumb_rects(area, &crumbs);
    assert_eq!(rects.len(), 3);
    for pair in rects.windows(2) {
        assert!(
            pair[1].x >= pair[0].x + pair[0].width,
            "crumb rectangles must not overlap"
        );
    }
    assert!(rects[0].x >= area.x);
}

/// The grid tiles into whole columns and only scrolls when the content
/// overflows.
#[test]
fn grid_geometry_tiles_and_bounds_the_scroll() {
    let grid = Rect::new(0.0, 0.0, CELL_W * 3.5, CELL_H * 2.0);
    let geo = grid_geometry(grid, 7);
    assert_eq!(geo.cols, 3, "a partial fourth column is not usable");
    assert_eq!(geo.rows, 3);
    assert_eq!(geo.content_height, CELL_H * 3.0);
    assert!((geo.max_scroll - CELL_H).abs() < 1e-9);

    // Everything fits: nothing to scroll.
    let geo = grid_geometry(grid, 3);
    assert_eq!(geo.max_scroll, 0.0);
}

/// Cell 0 sits against the grid's top-left; scrolling moves cells up.
#[test]
fn cells_start_at_the_top_left_and_scroll_upward() {
    let grid = Rect::new(10.0, 20.0, CELL_W * 2.0, CELL_H * 2.0);
    let geo = grid_geometry(grid, 6);
    let first = cell_rect(grid, &geo, 0, 0.0);
    assert_eq!(first.x, grid.x);
    assert!((first.y + first.height - (grid.y + grid.height)).abs() < 1e-9);

    let second_row = cell_rect(grid, &geo, 2, 0.0);
    assert!(second_row.y < first.y, "row 1 is below row 0 (Y-up)");

    let scrolled = cell_rect(grid, &geo, 0, CELL_H);
    assert!((scrolled.y - (first.y + CELL_H)).abs() < 1e-9);
}

/// The hit-test arithmetic and the paint rectangles must name the same
/// cell — including after a scroll, which is the case a stale visible
/// range gets wrong.
#[test]
fn cell_index_at_agrees_with_cell_rect() {
    let grid = Rect::new(10.0, 20.0, CELL_W * 3.0, CELL_H * 2.0);
    let geo = grid_geometry(grid, 100);
    for scroll in [0.0, 37.0, CELL_H * 9.0] {
        for index in 0..60 {
            let rect = cell_rect(grid, &geo, index, scroll);
            let centre = rect.center();
            if !grid.contains(centre) {
                continue; // scrolled out of the viewport
            }
            assert_eq!(
                cell_index_at(grid, &geo, centre, scroll),
                Some(index),
                "cell {index} at scroll {scroll}"
            );
        }
    }
    // Outside the grid, and inside the unused strip right of the last
    // whole column, name nothing.
    assert_eq!(
        cell_index_at(grid, &geo, Point::new(grid.x - 1.0, grid.y + 1.0), 0.0),
        None
    );
    let wide = Rect::new(0.0, 0.0, CELL_W * 2.5, CELL_H);
    let geo = grid_geometry(wide, 10);
    assert_eq!(
        cell_index_at(wide, &geo, Point::new(CELL_W * 2.2, CELL_H * 0.5), 0.0),
        None,
        "the partial trailing column holds no cells"
    );
}

/// The visible gate is what keeps the thumbnail cache honest: a viewport
/// two rows tall never reports more than three rows' worth of cells (two
/// full rows plus the partial one scrolled into view).
#[test]
fn visible_range_covers_only_the_rows_on_screen() {
    let grid = Rect::new(0.0, 0.0, CELL_W * 4.0, CELL_H * 2.0);
    let count = 400;
    let geo = grid_geometry(grid, count);

    let (start, end) = visible_range(&geo, grid, count, 0.0);
    assert_eq!(start, 0);
    assert_eq!(end, 8, "two rows of four");

    // Scrolled half a row: the partially visible third row counts.
    let (start, end) = visible_range(&geo, grid, count, CELL_H * 0.5);
    assert_eq!(start, 0);
    assert_eq!(end, 12);

    // Deep into a long listing, the window stays the same size.
    let (start, end) = visible_range(&geo, grid, count, CELL_H * 20.0);
    assert_eq!(start, 80);
    assert_eq!(end, 88);

    // Nothing to show, nothing visible.
    let geo = grid_geometry(grid, 0);
    assert_eq!(visible_range(&geo, grid, 0, 0.0), (0, 0));
}
