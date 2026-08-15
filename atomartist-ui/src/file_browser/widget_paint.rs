//! Painting for [`super::widget::FileBrowser`].
//!
//! Split out of `widget.rs` (assembly + events) so neither file approaches
//! the 800-line cap. Everything here reads the `Frame` that
//! `FileBrowser::layout` already computed — no measuring, no model calls,
//! and above all no cache *requests*: the visibility round runs in layout
//! (see the widget's module docs), so paint only renders answers.
//!
//! Coordinates are widget-local and **Y-up**: `rect.y + rect.height` is a
//! rectangle's top edge, and text baselines sit *above* the y they are
//! drawn at. Decoded previews are the one top-down buffer in the file —
//! [`super::thumbs::ThumbnailImage`] is PNG row order, which is what
//! `DrawCtx::draw_image_rgba` wants.
//!
//! The three non-`Ready` listing states each get their own centred
//! message: a browser must never show a blank pane (design §2).

use agg_gui::{theme::current_visuals, Color, DrawCtx, Rect, Widget};
use atomartist_storage::Entry;

use super::model::Listing;
use super::thumbs::ThumbState;
use super::widget::FileBrowser;
use super::widget_geom::{self as geom, CELL_H, CELL_W, FONT_SIZE, TEXT_INSET, THUMB_BOX};
use crate::app_state_storage::uri_extension;
use crate::fa;

/// Caption font, a touch smaller than the chrome so two-line-ish names
/// still fit a cell.
const CAPTION_SIZE: f64 = 11.0;
/// Fallback glyph size inside a cell's preview box.
const GLYPH_SIZE: f64 = 30.0;

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
    paint_crumbs(browser, ctx);
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
        let label = elide(&root.display_name, row.width - TEXT_INSET * 3.0, FONT_SIZE);
        ctx.fill_text(&label, row.x + TEXT_INSET * 2.5, baseline);
    }
}

fn paint_crumbs(browser: &FileBrowser, ctx: &mut dyn DrawCtx) {
    let visuals = current_visuals();
    let frame = &browser.frame;
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
        Listing::Loading => centred_message(
            ctx,
            grid,
            &format!("{} Loading…", fa::CIRCLE_O_NOTCH),
            visuals.text_dim,
        ),
        Listing::Empty => centred_message(ctx, grid, "This folder is empty", visuals.text_dim),
        Listing::Error(message) => centred_message(
            ctx,
            grid,
            &format!("{} {message}", fa::EXCLAMATION_TRIANGLE),
            error_color(),
        ),
        Listing::Ready(_) if frame.entries.is_empty() => {
            // Ready, but the search filtered everything out — a different
            // sentence from an empty directory, because the fix differs.
            centred_message(ctx, grid, "No entries match the search", visuals.text_dim)
        }
        Listing::Ready(_) => paint_cells(browser, ctx),
    }
    ctx.restore();
}

fn paint_cells(browser: &FileBrowser, ctx: &mut dyn DrawCtx) {
    let visuals = current_visuals();
    let frame = &browser.frame;
    let selected = browser.model().selected();
    let scroll = browser.scroll_offset();
    for index in frame.visible.clone() {
        let entry = &frame.entries[index];
        let cell = geom::cell_rect(frame.layout.grid, &frame.grid, index, scroll);
        if selected.as_ref() == Some(&entry.uri) {
            ctx.set_fill_color(visuals.selection_bg);
            ctx.begin_path();
            ctx.rounded_rect(cell.x + 2.0, cell.y + 2.0, CELL_W - 4.0, CELL_H - 4.0, 4.0);
            ctx.fill();
        }
        let thumb = frame
            .thumbs
            .get(index - frame.visible.start)
            .cloned()
            .unwrap_or(ThumbState::NotRequested);
        paint_preview(ctx, cell, entry, &thumb);

        ctx.set_font_size(CAPTION_SIZE);
        ctx.set_fill_color(visuals.text_color);
        let label = elide(&entry.name, CELL_W - 8.0, CAPTION_SIZE);
        let w = geom::measure(&label, CAPTION_SIZE);
        ctx.fill_text(&label, cell.x + (CELL_W - w) * 0.5, cell.y + 8.0);
    }
}

/// The preview box: a decoded image when there is one, otherwise the
/// quiet fallback glyph for what the entry is (or for why there is no
/// image).
fn paint_preview(ctx: &mut dyn DrawCtx, cell: Rect, entry: &Entry, thumb: &ThumbState) {
    let visuals = current_visuals();
    let box_x = cell.x + (CELL_W - THUMB_BOX) * 0.5;
    let box_y = cell.y + CELL_H - THUMB_BOX - 6.0;

    if let Some(image) = thumb.image() {
        // Fit inside the box, preserving aspect. The buffer is top-down
        // RGBA8, which is what `draw_image_rgba` expects.
        //
        // Portability caveat, not a performance one: `draw_image_rgba`
        // has a *no-op* default on `DrawCtx`, and agg-gui's `gl_renderer`
        // implements neither it nor the `_arc` variant — so on a GL
        // backend previews would silently vanish (glyph fallbacks and the
        // rest of the grid would still paint). Both shells run demo-wgpu,
        // which implements the slice form and caches uploads by data
        // identity, so a stable `Arc<ThumbnailImage>` re-uses its texture
        // across frames; there is nothing to optimise here today.
        let scale =
            (THUMB_BOX / image.width.max(1) as f64).min(THUMB_BOX / image.height.max(1) as f64);
        let w = image.width as f64 * scale;
        let h = image.height as f64 * scale;
        ctx.draw_image_rgba(
            &image.rgba,
            image.width,
            image.height,
            box_x + (THUMB_BOX - w) * 0.5,
            box_y + (THUMB_BOX - h) * 0.5,
            w,
            h,
        );
        return;
    }

    let (glyph, color) = match thumb {
        // Distinct but quiet: a failed preview is still a browsable file,
        // so it gets a dimmed warning rather than a red alarm.
        ThumbState::Failed(_) => (fa::EXCLAMATION_TRIANGLE, visuals.text_dim.with_alpha(0.7)),
        ThumbState::Pending => (fa::CIRCLE_O_NOTCH, visuals.text_dim.with_alpha(0.5)),
        _ if entry.is_dir => (fa::FOLDER, visuals.text_dim),
        _ => (format_glyph(entry), visuals.text_dim),
    };
    ctx.set_font_size(GLYPH_SIZE);
    ctx.set_fill_color(color);
    let text = glyph.to_string();
    let w = geom::measure(&text, GLYPH_SIZE);
    ctx.fill_text(
        &text,
        box_x + (THUMB_BOX - w) * 0.5,
        box_y + (THUMB_BOX - GLYPH_SIZE) * 0.5,
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

fn centred_message(ctx: &mut dyn DrawCtx, area: Rect, text: &str, color: Color) {
    ctx.set_font_size(FONT_SIZE);
    ctx.set_fill_color(color);
    let w = geom::measure(text, FONT_SIZE);
    ctx.fill_text(
        text,
        area.x + (area.width - w) * 0.5,
        area.y + area.height * 0.5,
    );
}

/// Error text colour. Same local constant `status_bar` keeps for the same
/// reason: agg-gui's `Visuals` has no error token yet, and introducing one
/// touches every palette.
fn error_color() -> Color {
    Color::rgb(0.85, 0.25, 0.25)
}

/// Truncate `text` with an ellipsis so it fits `max_w`.
fn elide(text: &str, max_w: f64, size: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if geom::measure(text, size) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push('…');
        if geom::measure(&candidate, size) > max_w {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}
