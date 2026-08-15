//! Painting for [`crate::favorites_bar::FavoritesBar`].
//!
//! Split out of `favorites_bar.rs` (assembly + events) so neither file
//! approaches the 800-line cap. Everything here reads the
//! [`BarLayout`](crate::favorites_bar_geom::BarLayout) that
//! `FavoritesBar::layout` already computed — no measuring against the
//! model, no favourite resolution, no storage calls. The strip's own
//! items are painted by [`crate::favorites_strip`].
//!
//! Coordinates are widget-local and **Y-up**: `rect.y + rect.height` is a
//! rectangle's top edge.
//!
//! The handle carries a chevron pointing the direction its click will
//! move things (ND's affordance): **right** while collapsed ("this opens
//! the panel"), **left** while expanded ("this closes it").

use agg_gui::{theme::current_visuals, DrawCtx, Widget};

use crate::favorites_bar::FavoritesBar;
use crate::favorites_bar_geom as geom;

/// Chevron size in the handle grip.
const CHEVRON_SIZE: f64 = 10.0;

pub(crate) fn paint_bar(bar: &mut FavoritesBar, ctx: &mut dyn DrawCtx) {
    let bounds = bar.bounds();
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let visuals = current_visuals();
    if let Some(font) = agg_gui::font_settings::current_system_font() {
        ctx.set_font(font);
    }
    let layout = bar.layout_rects().clone();

    ctx.save();
    ctx.clip_rect(0.0, 0.0, bounds.width, bounds.height);

    // Bar background (the panel's own chrome paints over it).
    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rect(0.0, 0.0, bounds.width, bounds.height);
    ctx.fill();

    // Only the items inside the scroll viewport are drawn, and the strip
    // clips them, so a half-scrolled item is cut exactly where the
    // hit-test stops accepting it.
    let items: Vec<_> = layout
        .items
        .iter()
        .enumerate()
        .filter(|(_, rect)| geom::item_visible(**rect, layout.items_viewport))
        .filter_map(|(index, rect)| bar.strip_item(index).map(|info| (*rect, info)))
        .collect();
    crate::favorites_strip::paint_strip(
        ctx,
        layout.strip,
        layout.items_viewport,
        &items,
        layout.pin,
    );

    // Separator between the strip and whatever is to its right.
    ctx.set_stroke_color(visuals.separator);
    ctx.set_line_width(1.0);
    ctx.begin_path();
    ctx.move_to(layout.strip.x + layout.strip.width - 0.5, layout.strip.y);
    ctx.line_to(
        layout.strip.x + layout.strip.width - 0.5,
        layout.strip.y + layout.strip.height,
    );
    ctx.stroke();

    // Handle: a separator line plus the centred grip, so the strip reads
    // as grabbable without a hover state.
    let handle = layout.handle;
    ctx.begin_path();
    ctx.move_to(handle.x + handle.width - 0.5, handle.y);
    ctx.line_to(handle.x + handle.width - 0.5, handle.y + handle.height);
    ctx.stroke();

    let grip = geom::handle_grip(handle);
    ctx.set_fill_color(visuals.text_color.with_alpha(0.10));
    ctx.begin_path();
    ctx.rect(grip.x, grip.y, grip.width, grip.height);
    ctx.fill();

    let chevron = if bar.expanded() {
        crate::fa::CHEVRON_LEFT
    } else {
        crate::fa::CHEVRON_RIGHT
    };
    ctx.set_fill_color(visuals.text_dim);
    ctx.set_font_size(CHEVRON_SIZE);
    ctx.fill_text(
        &chevron.to_string(),
        grip.x + (grip.width - CHEVRON_SIZE) * 0.5,
        grip.y + (grip.height - CHEVRON_SIZE) * 0.5,
    );

    ctx.restore();
}
