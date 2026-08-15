//! Painting for [`crate::favorites_bar::FavoritesBar`].
//!
//! Split out of `favorites_bar.rs` (assembly + events) so neither file
//! approaches the 800-line cap. Everything here reads the [`BarLayout`]
//! that `FavoritesBar::layout` already computed — no measuring against the
//! model, no favourite resolution, no storage calls.
//!
//! Coordinates are widget-local and **Y-up**: `rect.y + rect.height` is a
//! rectangle's top edge, and the favourites were stacked downward from the
//! bar's top by [`crate::favorites_bar_geom`].
//!
//! Dead favourites (a node type this build no longer registers, a URI that
//! no longer parses) are painted in the dim text colour rather than
//! dropped — the design's explicit rule, so the user can see *and unpin*
//! an entry whose provider is away.

use agg_gui::{theme::current_visuals, DrawCtx, Widget};

use crate::favorites_bar::FavoritesBar;
use crate::favorites_bar_geom::{self as geom, PAD};

/// Label font size in the expanded panel.
const LABEL_SIZE: f64 = 12.0;
/// Glyph size in the expanded panel's rows.
const ROW_GLYPH_SIZE: f64 = 13.0;
/// Glyph size in the collapsed rail, which has nothing else to show.
const RAIL_GLYPH_SIZE: f64 = 17.0;

pub(crate) fn paint_bar(bar: &mut FavoritesBar, ctx: &mut dyn DrawCtx) {
    let bounds = bar.bounds();
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let visuals = current_visuals();
    if let Some(font) = agg_gui::font_settings::current_system_font() {
        ctx.set_font(font);
    }
    let expanded = bar.expanded();
    let layout = bar.layout_rects().clone();

    ctx.save();
    ctx.clip_rect(0.0, 0.0, bounds.width, bounds.height);

    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rect(0.0, 0.0, bounds.width, bounds.height);
    ctx.fill();

    // Rows.
    let glyph_size = if expanded {
        ROW_GLYPH_SIZE
    } else {
        RAIL_GLYPH_SIZE
    };
    ctx.set_font_size(glyph_size);
    for (index, row) in layout.rows.iter().enumerate() {
        let Some((glyph, label, alive)) = bar.row_glyph(index) else {
            continue;
        };
        let color = if alive {
            visuals.text_color
        } else {
            visuals.text_dim
        };
        ctx.set_fill_color(color);
        ctx.set_font_size(glyph_size);
        let baseline = row.y + (row.height - glyph_size) * 0.5 + 1.0;
        ctx.fill_text(&glyph.to_string(), row.x + PAD + 2.0, baseline);
        if !expanded {
            continue;
        }
        ctx.set_font_size(LABEL_SIZE);
        let text_x = row.x + PAD + 2.0 + glyph_size + PAD;
        let unpin = geom::unpin_rect(*row);
        let label = elide(label, (unpin.x - text_x - PAD).max(0.0), LABEL_SIZE);
        let text_baseline = row.y + (row.height - LABEL_SIZE) * 0.5 + 1.0;
        ctx.fill_text(&label, text_x, text_baseline);
        // Unpin affordance: dim, so it reads as secondary to the entry.
        ctx.set_fill_color(visuals.text_dim);
        ctx.fill_text(&crate::fa::TIMES.to_string(), unpin.x + 4.0, text_baseline);
    }

    // "Pin current project" row.
    if let Some(pin) = layout.pin {
        ctx.set_fill_color(visuals.text_dim);
        ctx.set_font_size(LABEL_SIZE);
        let baseline = pin.y + (pin.height - LABEL_SIZE) * 0.5 + 1.0;
        ctx.fill_text(
            &crate::fa::PLUS_CIRCLE.to_string(),
            pin.x + PAD + 2.0,
            baseline,
        );
        if expanded {
            ctx.fill_text("Pin current project", pin.x + PAD * 2.0 + 14.0, baseline);
        }
    }

    // Handle: a separator line plus a short centred grip, so the strip
    // reads as grabbable without a hover state.
    let handle = layout.handle;
    ctx.set_stroke_color(visuals.separator);
    ctx.set_line_width(1.0);
    ctx.begin_path();
    ctx.move_to(handle.x + handle.width - 0.5, handle.y);
    ctx.line_to(handle.x + handle.width - 0.5, handle.y + handle.height);
    ctx.stroke();

    let grip_h = (handle.height * 0.12).clamp(0.0, 40.0);
    let grip_y = handle.y + (handle.height - grip_h) * 0.5;
    ctx.set_fill_color(visuals.text_dim);
    ctx.begin_path();
    ctx.rect(handle.x + handle.width * 0.5 - 1.0, grip_y, 2.0, grip_h);
    ctx.fill();

    ctx.restore();
}

/// Truncate `text` with an ellipsis so it fits `max_w`. Same shape as the
/// browser's — duplicated rather than shared because the two measure with
/// different chrome constants and the helper is four lines of policy.
fn elide(text: &str, max_w: f64, size: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if crate::file_browser::widget_geom::measure(text, size) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push('…');
        if crate::file_browser::widget_geom::measure(&candidate, size) > max_w {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}
