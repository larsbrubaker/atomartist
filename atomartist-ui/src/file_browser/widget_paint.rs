//! Painting for [`super::widget::FileBrowser`].
//!
//! Split out of `widget.rs` (assembly + events) so neither file approaches
//! the 800-line cap. Everything here reads the `Frame` that
//! `FileBrowser::layout` already computed — no measuring of layout, no
//! model calls, and above all no cache *requests*: the visibility round
//! runs in layout (see the widget's module docs), so paint only renders
//! answers. One card's internal arithmetic lives in
//! [`super::widget_cards`], which is pure and unit-tested.
//!
//! Coordinates are widget-local and **Y-up**: `rect.y + rect.height` is a
//! rectangle's top edge, and text baselines sit *above* the y they are
//! drawn at. Decoded previews are the one top-down buffer in the file —
//! [`super::thumbs::ThumbnailImage`] is PNG row order, which is what
//! `DrawCtx::draw_image_rgba` wants.
//!
//! The three non-`Ready` listing states each get their own centred
//! icon-above-text column: a browser must never show a blank pane
//! (design §2), and the icon is what tells "still loading" from "nothing
//! here" at a glance (step 6f-3, ND's empty/loading/error blocks).

use agg_gui::{theme::current_visuals, Color, DrawCtx, Rect, Widget};
use atomartist_storage::Entry;

use super::model::Listing;
use super::thumbs::ThumbState;
use super::widget::FileBrowser;
use super::widget_cards::{self as cards, EntryAccent};
use super::widget_geom::{
    self as geom, CARD_BORDER, CARD_PAD_X, CARD_RADIUS, DATE_SIZE, FONT_SIZE, NAME_SIZE,
    SEARCH_CLEAR_W, SEARCH_GLYPH_W, TEXT_INSET,
};
use crate::app_state_storage::uri_extension;
use crate::fa;

/// Fallback glyph size inside a card's thumbnail box.
const GLYPH_SIZE: f64 = 26.0;
/// Icon size in a centred listing-state column.
const STATE_GLYPH_SIZE: f64 = 26.0;

pub(super) fn paint_browser(browser: &mut FileBrowser, ctx: &mut dyn DrawCtx) {
    let bounds = browser.bounds();
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let visuals = current_visuals();
    if let Some(font) = agg_gui::font_settings::current_system_font() {
        ctx.set_font(font);
    }

    ctx.save();
    ctx.clip_rect(0.0, 0.0, bounds.width, bounds.height);
    ctx.set_fill_color(visuals.bg_color);
    ctx.begin_path();
    ctx.rect(0.0, 0.0, bounds.width, bounds.height);
    ctx.fill();

    paint_sidebar(browser, ctx);
    paint_search(browser, ctx);
    paint_nav(browser, ctx);
    paint_grid(browser, ctx);

    ctx.restore();
}

fn paint_sidebar(browser: &FileBrowser, ctx: &mut dyn DrawCtx) {
    let visuals = current_visuals();
    let frame = &browser.frame;
    let sidebar = frame.layout.sidebar;

    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rect(sidebar.x, sidebar.y, sidebar.width, sidebar.height);
    ctx.fill();
    ctx.set_stroke_color(visuals.separator);
    ctx.set_line_width(1.0);
    ctx.begin_path();
    ctx.move_to(sidebar.x + sidebar.width, sidebar.y);
    ctx.line_to(sidebar.x + sidebar.width, sidebar.y + sidebar.height);
    ctx.stroke();

    ctx.set_font_size(FONT_SIZE);
    for (root, row) in frame.roots.iter().zip(frame.sidebar_rows.iter()) {
        let current = frame.current_scheme.as_deref() == Some(root.scheme.as_str());
        if current {
            ctx.set_fill_color(visuals.selection_bg);
            ctx.begin_path();
            ctx.rect(row.x, row.y, row.width, row.height);
            ctx.fill();
        }
        let color = if current {
            visuals.text_color
        } else {
            visuals.text_dim
        };
        ctx.set_fill_color(color);
        let baseline = row.y + (row.height - FONT_SIZE) * 0.5 + 1.0;
        ctx.fill_text(&fa::HDD.to_string(), row.x + TEXT_INSET, baseline);
        let label = geom::elide(&root.display_name, row.width - TEXT_INSET * 3.0, FONT_SIZE);
        ctx.fill_text(&label, row.x + TEXT_INSET * 2.5, baseline);
    }
}

/// The search box: a rounded well with a magnifier glyph, the bound
/// `TextField` child in the middle (it paints itself), and a round clear
/// button that only exists while there is something to clear.
fn paint_search(browser: &FileBrowser, ctx: &mut dyn DrawCtx) {
    let visuals = current_visuals();
    let box_rect = browser.frame.layout.search;
    if box_rect.width <= 0.0 || box_rect.height <= 0.0 {
        return;
    }
    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rounded_rect(
        box_rect.x,
        box_rect.y,
        box_rect.width,
        box_rect.height,
        box_rect.height * 0.5,
    );
    ctx.fill();
    ctx.set_stroke_color(visuals.separator);
    ctx.set_line_width(1.0);
    ctx.begin_path();
    ctx.rounded_rect(
        box_rect.x,
        box_rect.y,
        box_rect.width,
        box_rect.height,
        box_rect.height * 0.5,
    );
    ctx.stroke();

    ctx.set_font_size(FONT_SIZE);
    ctx.set_fill_color(visuals.text_dim);
    let glyph = fa::SEARCH.to_string();
    let w = geom::measure(&glyph, FONT_SIZE);
    ctx.fill_text(
        &glyph,
        box_rect.x + (SEARCH_GLYPH_W - w) * 0.5 + 2.0,
        box_rect.y + (box_rect.height - FONT_SIZE) * 0.5 + 1.0,
    );

    if browser.search_text().is_empty() {
        return;
    }
    let clear = browser.frame.layout.search_clear;
    let glyph = fa::TIMES_CIRCLE.to_string();
    let w = geom::measure(&glyph, SEARCH_CLEAR_W);
    ctx.set_font_size(SEARCH_CLEAR_W);
    ctx.set_fill_color(visuals.text_dim);
    // Same vertical centring every other glyph in this file uses — the
    // baseline sits inside the box, not on its bottom edge.
    ctx.fill_text(
        &glyph,
        clear.x + (clear.width - w) * 0.5,
        clear.y + (clear.height - SEARCH_CLEAR_W) * 0.5 + 1.0,
    );
}

/// The nav row: Back button, then the breadcrumb trail.
fn paint_nav(browser: &FileBrowser, ctx: &mut dyn DrawCtx) {
    let visuals = current_visuals();
    let frame = &browser.frame;
    let back = frame.layout.back;
    if back.width > 0.0 && back.height > 0.0 {
        let enabled = browser.model().can_go_back();
        ctx.set_fill_color(visuals.panel_fill);
        ctx.begin_path();
        ctx.rounded_rect(back.x, back.y + 2.0, back.width, back.height - 4.0, 4.0);
        ctx.fill();
        ctx.set_fill_color(if enabled {
            visuals.text_color
        } else {
            visuals.text_dim.with_alpha(0.4)
        });
        ctx.set_font_size(FONT_SIZE);
        let glyph = fa::ARROW_LEFT.to_string();
        let w = geom::measure(&glyph, FONT_SIZE);
        ctx.fill_text(
            &glyph,
            back.x + (back.width - w) * 0.5,
            back.y + (back.height - FONT_SIZE) * 0.5 + 1.0,
        );
    }

    let area = frame.layout.crumbs;
    if area.width <= 0.0 || area.height <= 0.0 {
        return;
    }
    ctx.save();
    ctx.clip_rect(area.x, area.y, area.width, area.height);
    ctx.set_font_size(FONT_SIZE);
    let last = frame.crumbs.len().saturating_sub(1);
    for (i, (crumb, rect)) in frame
        .crumbs
        .iter()
        .zip(frame.crumb_rects.iter())
        .enumerate()
    {
        let baseline = rect.y + (rect.height - FONT_SIZE) * 0.5 + 1.0;
        if i > 0 {
            ctx.set_fill_color(visuals.text_dim);
            ctx.fill_text(
                &fa::CHEVRON_RIGHT.to_string(),
                rect.x - geom::CRUMB_SEP_W + 2.0,
                baseline,
            );
        }
        ctx.set_fill_color(if i == last {
            visuals.text_color
        } else {
            visuals.text_link
        });
        ctx.fill_text(&crumb.label, rect.x, baseline);
    }
    ctx.restore();
}

fn paint_grid(browser: &FileBrowser, ctx: &mut dyn DrawCtx) {
    let visuals = current_visuals();
    let frame = &browser.frame;
    let grid = frame.layout.grid;
    if grid.width <= 0.0 || grid.height <= 0.0 {
        return;
    }
    ctx.save();
    ctx.clip_rect(grid.x, grid.y, grid.width, grid.height);
    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rect(grid.x, grid.y, grid.width, grid.height);
    ctx.fill();

    match &frame.listing {
        Listing::Loading => state_column(
            ctx,
            grid,
            fa::HOURGLASS,
            "Loading...",
            visuals.text_dim,
            visuals.text_dim,
        ),
        Listing::Empty => state_column(
            ctx,
            grid,
            fa::FOLDER_OPEN,
            "This folder is empty",
            visuals.text_dim,
            visuals.text_dim,
        ),
        Listing::Error(message) => state_column(
            ctx,
            grid,
            fa::EXCLAMATION_TRIANGLE,
            message,
            error_color(),
            error_color(),
        ),
        Listing::Ready(_) if frame.entries.is_empty() => {
            // Ready, but the search filtered everything out — a different
            // sentence from an empty directory, because the fix differs.
            state_column(
                ctx,
                grid,
                fa::SEARCH,
                "No entries match the search",
                visuals.text_dim,
                visuals.text_dim,
            )
        }
        Listing::Ready(_) => paint_cards(browser, ctx),
    }
    ctx.restore();
}

/// Every card on screen, ND's centred column: thumbnail, name below it,
/// optional date.
fn paint_cards(browser: &FileBrowser, ctx: &mut dyn DrawCtx) {
    let visuals = current_visuals();
    let frame = &browser.frame;
    let selected = browser.model().selected();
    let scroll = browser.scroll_offset();
    for index in frame.visible.clone() {
        let entry = &frame.entries[index];
        let card = geom::cell_rect(frame.layout.grid, &frame.grid, index, scroll);
        let layout = cards::card_layout(card);
        let accent = cards::accent_of(entry);
        let is_selected = selected.as_ref() == Some(&entry.uri);

        if is_selected {
            ctx.set_fill_color(visuals.selection_bg);
            ctx.begin_path();
            ctx.rounded_rect(card.x, card.y, card.width, card.height, CARD_RADIUS);
            ctx.fill();
            // The 2 px border is transparent until selected, so a card
            // never moves when it gains one.
            ctx.set_stroke_color(visuals.accent);
            ctx.set_line_width(CARD_BORDER);
            ctx.begin_path();
            ctx.rounded_rect(
                card.x + CARD_BORDER * 0.5,
                card.y + CARD_BORDER * 0.5,
                (card.width - CARD_BORDER).max(0.0),
                (card.height - CARD_BORDER).max(0.0),
                CARD_RADIUS,
            );
            ctx.stroke();
        }

        let thumb = frame
            .thumbs
            .get(index - frame.visible.start)
            .cloned()
            .unwrap_or(ThumbState::NotRequested);
        paint_preview(ctx, layout.thumb, entry, accent, &thumb);

        ctx.set_font_size(NAME_SIZE);
        ctx.set_fill_color(visuals.text_color);
        let text_w = (card.width - CARD_PAD_X * 2.0).max(0.0);
        for (line, rect) in cards::wrap_name(&entry.name, text_w)
            .iter()
            .zip(layout.name_lines.iter())
        {
            let w = geom::measure(line, NAME_SIZE);
            ctx.fill_text(
                line,
                rect.x + (rect.width - w) * 0.5,
                rect.y + (rect.height - NAME_SIZE) * 0.5 + 1.0,
            );
        }

        if let Some(modified) = entry.modified {
            let text = cards::format_modified(modified);
            ctx.set_font_size(DATE_SIZE);
            ctx.set_fill_color(visuals.text_dim);
            let w = geom::measure(&text, DATE_SIZE);
            ctx.fill_text(
                &text,
                layout.date.x + (layout.date.width - w) * 0.5,
                layout.date.y + (layout.date.height - DATE_SIZE) * 0.5 + 1.0,
            );
        }
    }
}

/// The thumbnail box: a decoded image cropped to *cover* it when there is
/// one, otherwise the type-tinted fallback glyph for what the entry is
/// (or for why there is no image).
fn paint_preview(
    ctx: &mut dyn DrawCtx,
    frame: Rect,
    entry: &Entry,
    accent: EntryAccent,
    thumb: &ThumbState,
) {
    let visuals = current_visuals();

    if let Some(image) = thumb.image() {
        // ND's `object-fit: cover`: scale until both axes are filled and
        // clip the overflow, so a grid of mixed aspect ratios reads as one
        // tidy column of boxes. The buffer is top-down RGBA8, which is
        // what `draw_image_rgba` expects.
        //
        // Portability caveat, not a performance one: `draw_image_rgba`
        // has a *no-op* default on `DrawCtx`, and agg-gui's `gl_renderer`
        // implements neither it nor the `_arc` variant — so on a GL
        // backend previews would silently vanish (glyph fallbacks and the
        // rest of the grid would still paint). Both shells run demo-wgpu,
        // which implements the slice form and caches uploads by data
        // identity, so a stable `Arc<ThumbnailImage>` re-uses its texture
        // across frames; there is nothing to optimise here today.
        let dest = cards::cover_rect(frame, image.width, image.height);
        ctx.save();
        ctx.clip_rect(frame.x, frame.y, frame.width, frame.height);
        ctx.draw_image_rgba(
            &image.rgba,
            image.width,
            image.height,
            dest.x,
            dest.y,
            dest.width,
            dest.height,
        );
        ctx.restore();
        return;
    }

    // The glyph carries the type accent; the thumbnail never does — a
    // real preview must not be tinted.
    let (glyph, color) = match thumb {
        // Distinct but quiet: a failed preview is still a browsable file,
        // so it gets a dimmed warning rather than a red alarm.
        ThumbState::Failed(_) => (fa::EXCLAMATION_TRIANGLE, visuals.text_dim.with_alpha(0.7)),
        ThumbState::Pending => (fa::CIRCLE_O_NOTCH, visuals.text_dim.with_alpha(0.5)),
        _ if entry.is_dir => (fa::FOLDER, accent.color()),
        _ => (format_glyph(entry), accent.color()),
    };
    ctx.set_font_size(GLYPH_SIZE);
    ctx.set_fill_color(color);
    let text = glyph.to_string();
    let w = geom::measure(&text, GLYPH_SIZE);
    ctx.fill_text(
        &text,
        frame.x + (frame.width - w) * 0.5,
        frame.y + (frame.height - GLYPH_SIZE) * 0.5,
    );
}

/// Glyph for a file with no preview, chosen from its extension.
fn format_glyph(entry: &Entry) -> char {
    match uri_extension(&entry.uri).as_str() {
        "atmr" => fa::CUBES,
        "stl" | "obj" | "3mf" => fa::CUBE,
        _ => fa::FILE_NEW,
    }
}

/// One listing state: icon above, text below, both centred — ND's
/// empty/loading/error block.
fn state_column(
    ctx: &mut dyn DrawCtx,
    area: Rect,
    glyph: char,
    text: &str,
    glyph_color: Color,
    text_color: Color,
) {
    let centre_x = area.x + area.width * 0.5;
    let centre_y = area.y + area.height * 0.5;

    ctx.set_font_size(STATE_GLYPH_SIZE);
    ctx.set_fill_color(glyph_color);
    let icon = glyph.to_string();
    let w = geom::measure(&icon, STATE_GLYPH_SIZE);
    ctx.fill_text(&icon, centre_x - w * 0.5, centre_y + 8.0);

    ctx.set_font_size(FONT_SIZE);
    ctx.set_fill_color(text_color);
    let label = geom::elide(text, area.width - TEXT_INSET * 2.0, FONT_SIZE);
    let w = geom::measure(&label, FONT_SIZE);
    ctx.fill_text(&label, centre_x - w * 0.5, centre_y - 14.0);
}

/// Error text colour. Same local constant `status_bar` keeps for the same
/// reason: agg-gui's `Visuals` has no error token yet, and introducing one
/// touches every palette.
fn error_color() -> Color {
    Color::rgb(0.85, 0.25, 0.25)
}
