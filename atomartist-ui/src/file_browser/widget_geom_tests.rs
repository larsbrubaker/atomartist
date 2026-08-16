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

/// Width of a grid holding exactly `cols` minimum-width columns and
/// their gaps — the auto-fill boundary, to the pixel.
fn grid_for(cols: usize, rows: f64) -> Rect {
    let w = cols as f64 * CARD_MIN_W + (cols - 1) as f64 * GRID_GAP;
    Rect::new(0.0, 0.0, w, rows * (CARD_H + GRID_GAP))
}

/// `auto-fill minmax(120px, 1fr)`: as many whole 120 px columns as fit
/// once the 12 px gaps are counted, then each is stretched to share the
/// remainder.
#[test]
fn grid_columns_auto_fill_and_stretch_to_the_width() {
    // Exactly three minimum columns: no stretch, no fourth.
    let grid = grid_for(3, 2.0);
    let geo = grid_geometry(grid, 7);
    assert_eq!(geo.cols, 3);
    assert!((geo.card_w - CARD_MIN_W).abs() < 1e-9);

    // One pixel short of a fourth column's *minimum* — still three, but
    // now wider than the minimum.
    let grid = Rect::new(0.0, 0.0, grid_for(4, 1.0).width - 1.0, 400.0);
    let geo = grid_geometry(grid, 7);
    assert_eq!(geo.cols, 3, "a partial fourth column is not usable");
    assert!(geo.card_w > CARD_MIN_W, "1fr shares out the leftover");
    // The cards plus their gaps fill the grid exactly.
    let used = geo.cols as f64 * geo.card_w + (geo.cols - 1) as f64 * GRID_GAP;
    assert!((used - grid.width).abs() < 1e-9);

    // One more pixel and the fourth column appears at its minimum.
    let grid = grid_for(4, 1.0);
    assert_eq!(grid_geometry(grid, 7).cols, 4);

    // Degenerate widths still name one column rather than zero.
    assert_eq!(grid_geometry(Rect::new(0.0, 0.0, 10.0, 10.0), 3).cols, 1);
}

/// Rows are `CARD_H` tall with a gap between them — and no gap after the
/// last one, so a grid that exactly fits does not scroll.
#[test]
fn grid_rows_are_gapped_and_bound_the_scroll() {
    let grid = grid_for(3, 2.0);
    let geo = grid_geometry(grid, 7);
    assert_eq!(geo.rows, 3);
    assert_eq!(geo.card_h, CARD_H);
    assert!((geo.content_height - (3.0 * (CARD_H + GRID_GAP) - GRID_GAP)).abs() < 1e-9);
    assert!(geo.max_scroll > 0.0);

    // Two whole rows in a viewport exactly two rows tall: nothing to
    // scroll (the trailing gap is not content).
    let exact = Rect::new(0.0, 0.0, grid.width, 2.0 * CARD_H + GRID_GAP);
    assert_eq!(grid_geometry(exact, 6).max_scroll, 0.0);
    assert_eq!(grid_geometry(exact, 0).content_height, 0.0);
}

/// Card 0 sits against the grid's top-left; scrolling moves cards up.
#[test]
fn cards_start_at_the_top_left_and_scroll_upward() {
    let grid = grid_for(2, 2.0);
    let geo = grid_geometry(grid, 6);
    let first = cell_rect(grid, &geo, 0, 0.0);
    assert_eq!(first.x, grid.x);
    assert_eq!(first.width, geo.card_w);
    assert_eq!(first.height, CARD_H);
    assert!((first.y + first.height - (grid.y + grid.height)).abs() < 1e-9);

    // The next column starts one card plus one gap to the right.
    let second_col = cell_rect(grid, &geo, 1, 0.0);
    assert!((second_col.x - (first.x + geo.card_w + GRID_GAP)).abs() < 1e-9);

    let second_row = cell_rect(grid, &geo, 2, 0.0);
    assert!(second_row.y < first.y, "row 1 is below row 0 (Y-up)");
    assert!((first.y - (second_row.y + CARD_H + GRID_GAP)).abs() < 1e-9);

    let scrolled = cell_rect(grid, &geo, 0, CARD_H);
    assert!((scrolled.y - (first.y + CARD_H)).abs() < 1e-9);
}

/// The nav row carries a Back button left of the crumbs, and the search
/// box is right-aligned in its own row above.
#[test]
fn nav_row_reserves_back_and_the_search_box_sits_top_right() {
    let layout = BrowserLayout::compute(Size::new(800.0, 600.0), BrowserMode::Open);

    assert_eq!(layout.back.width, BACK_W);
    assert_eq!(layout.back.y, layout.crumbs.y);
    assert!(
        layout.crumbs.x >= layout.back.x + layout.back.width,
        "the crumbs start right of the Back button"
    );
    assert!(layout.back.x >= layout.sidebar.width);

    // Search: right-aligned, at least ND's 200 px, with the field inset
    // from both the leading glyph and the trailing clear button.
    assert!(layout.search.width >= 200.0);
    let content_right = layout.crumbs.x + layout.crumbs.width;
    assert!((layout.search.x + layout.search.width - content_right).abs() < 1e-9);
    assert!(layout.search_field.x > layout.search.x);
    assert!(
        layout.search_field.x + layout.search_field.width <= layout.search_clear.x,
        "the field must not cover the clear button"
    );
    assert!(layout.search.contains(layout.search_clear.center()));
}

/// The embedded face has no provider sidebar (step 6g-2), so its grid
/// gets the whole pane — which is what turns the favorites panel's single
/// narrow column into a real grid.
#[test]
fn the_embedded_face_gives_its_grid_the_whole_pane() {
    // The panel width the bar opens at: `DEFAULT_EXPANDED_W`.
    let size = Size::new(380.0, 600.0);
    let embedded = BrowserLayout::compute(size, BrowserMode::Embedded);
    let modal = BrowserLayout::compute(size, BrowserMode::Open);

    assert_eq!(
        embedded.sidebar.width, 0.0,
        "no provider list when embedded"
    );
    assert_eq!(modal.sidebar.width, SIDEBAR_W, "the modal keeps it");
    assert_eq!(embedded.grid.x, PAD, "the grid starts at the padding");
    assert!((embedded.grid.width - (380.0 - PAD * 2.0)).abs() < 1e-9);
    assert!(
        embedded.grid.width > modal.grid.width + SIDEBAR_W - 1e-9,
        "the sidebar's width goes to the grid, not to more padding"
    );

    // The point of the change: two auto-fill columns at the default
    // panel width (368 px holds 120 + 12 + 120 with 116 to share out),
    // where the sidebar left room for exactly one.
    assert_eq!(grid_geometry(embedded.grid, 6).cols, 2);
    assert_eq!(
        grid_geometry(modal.grid, 6).cols,
        1,
        "…which a 380 px panel with a sidebar could never have shown"
    );
}

/// One wheel notch scrolls the grid a browser-normal distance — pinned,
/// because the number the shells feed in is a *notch* count and a
/// per-notch step of a whole row reads as a page jump (step 6g-2).
#[test]
fn the_grid_scroll_step_is_a_browser_normal_notch() {
    assert!(
        (50.0..=100.0).contains(&GRID_SCROLL_STEP),
        "a wheel notch is 50-100 px in every browser we mirror, got {GRID_SCROLL_STEP}"
    );
    let pitch = grid_geometry(grid_for(3, 2.0), 9).row_pitch();
    assert!(
        GRID_SCROLL_STEP < pitch,
        "one notch must be less than a whole row ({pitch} px)"
    );
}

/// The hit-test arithmetic and the paint rectangles must name the same
/// cell — including after a scroll, which is the case a stale visible
/// range gets wrong.
#[test]
fn cell_index_at_agrees_with_cell_rect() {
    let grid = Rect::new(10.0, 20.0, grid_for(3, 1.0).width, CARD_H * 2.0);
    let geo = grid_geometry(grid, 100);
    for scroll in [0.0, 37.0, CARD_H * 9.0] {
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
    // Outside the grid names nothing.
    assert_eq!(
        cell_index_at(grid, &geo, Point::new(grid.x - 1.0, grid.y + 1.0), 0.0),
        None
    );

    // Neither does the gap between two cards — that is background, and
    // clicking it clears the selection.
    let first = cell_rect(grid, &geo, 0, 0.0);
    let in_col_gap = Point::new(first.x + first.width + GRID_GAP * 0.5, first.center().y);
    assert_eq!(cell_index_at(grid, &geo, in_col_gap, 0.0), None);
    let in_row_gap = Point::new(first.center().x, first.y - GRID_GAP * 0.5);
    assert_eq!(cell_index_at(grid, &geo, in_row_gap, 0.0), None);
}

/// The visible gate is what keeps the thumbnail cache honest: a viewport
/// two rows tall never reports more than three rows' worth of cells (two
/// full rows plus the partial one scrolled into view).
#[test]
fn visible_range_covers_only_the_rows_on_screen() {
    let grid = grid_for(4, 2.0);
    let count = 400;
    let geo = grid_geometry(grid, count);
    let pitch = geo.row_pitch();
    assert_eq!(geo.cols, 4);

    let (start, end) = visible_range(&geo, grid, count, 0.0);
    assert_eq!(start, 0);
    assert_eq!(end, 8, "two rows of four");

    // Scrolled half a row: the partially visible third row counts.
    let (start, end) = visible_range(&geo, grid, count, pitch * 0.5);
    assert_eq!(start, 0);
    assert_eq!(end, 12);

    // Deep into a long listing, the window stays the same size.
    let (start, end) = visible_range(&geo, grid, count, pitch * 20.0);
    assert_eq!(start, 80);
    assert_eq!(end, 88);

    // Nothing to show, nothing visible.
    let geo = grid_geometry(grid, 0);
    assert_eq!(visible_range(&geo, grid, 0, 0.0), (0, 0));
}
