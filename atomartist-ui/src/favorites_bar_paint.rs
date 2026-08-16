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
/// Corner radius of the floating grip (ND's `border-radius: 6px`).
const GRIP_RADIUS: f64 = 6.0;

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

    ctx.set_stroke_color(visuals.separator);
    ctx.set_line_width(1.0);
    // Separator between the browser panel and the strip, when the panel
    // is showing (6g-2: the panel is *outboard* of the strip).
    if layout.panel.is_some() {
        ctx.begin_path();
        ctx.move_to(layout.strip.x + 0.5, layout.strip.y);
        ctx.line_to(layout.strip.x + 0.5, layout.strip.y + layout.strip.height);
        ctx.stroke();
    }

    // The bar's outer edge, against the 3-D viewport.
    ctx.begin_path();
    ctx.move_to(bounds.width - 0.5, 0.0);
    ctx.line_to(bounds.width - 0.5, bounds.height);
    ctx.stroke();

    // The grip: a floating 16 × 56 affordance drawn *over* the strip's
    // right edge, so the strip reads as grabbable without a hover state
    // and without a full-height lane of dead width.
    let grip = layout.handle;
    ctx.set_fill_color(visuals.text_color.with_alpha(0.10));
    ctx.begin_path();
    // ND rounds only the two corners facing away from the edge
    // (`border-radius: 6px 0 0 6px`); `DrawCtx` has no per-corner radii,
    // so all four are rounded and the two against the viewport edge are
    // hidden by the bar's own boundary.
    ctx.rounded_rect(grip.x, grip.y, grip.width, grip.height, GRIP_RADIUS);
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

    // Last thing in the frame: render *one* still-missing primitive
    // icon (step 6f-2). Doing it here rather than in `layout` is what
    // makes the fill-in "deferred past first paint" — the strip has
    // already been drawn with its glyphs by the time the first render
    // runs, and paint happens exactly once per frame, so the palette
    // fills in at a steady one icon per frame.
    crate::favorites_strip::pump_icons(bar.app_state(), bar.strip_items());
}
