//! The persistent 72 px icon strip inside
//! [`FavoritesBar`](crate::favorites_bar::FavoritesBar)
//! (`docs/file-browser-design.md` §5b, step 6f-1).
//!
//! NodeDesigner's parts bar keeps its icon strip on screen at all times —
//! expanding the bar grows the *browser panel* beside it, never the
//! strip. This module owns everything about that strip that is not
//! rectangle arithmetic (which lives in
//! [`crate::favorites_bar_geom`]): resolving the favourites into
//! displayable items, turning an item into a drag payload, and painting
//! them.
//!
//! Split out of `favorites_bar.rs` so that file stays assembly + event
//! routing and both stay well under the 800-line cap.
//!
//! # Icons
//!
//! Step 6f-1 paints a Font Awesome glyph placeholder in the 44 × 44 slot.
//! Step 6f-2 replaces it with an offscreen render of the primitive's own
//! generator, which is why the slot is already carved as a square the
//! glyph is merely centred in.
//!
//! Coordinates are widget-local and **Y-up**; items were stacked downward
//! from the strip's top by `favorites_bar_geom::compute`.

use std::sync::Arc;

use agg_gui::{theme::current_visuals, DrawCtx, Rect};
use atomartist_lib::registry::NodeDef;
use atomartist_storage::StorageUri;

use crate::app_state::AppState;
use crate::drag_insert::DragPayload;
use crate::favorites_bar_geom::{self as geom, LABEL_SIZE};
use crate::file_browser::favorites::{FavoriteKind, FavoriteResolution};

/// Glyph size inside the 44 × 44 slot — the placeholder standing in for
/// 6f-2's rendered primitive icons.
const SLOT_GLYPH_SIZE: f64 = 22.0;

/// One favourite as the strip needs it for a frame: resolved label,
/// glyph, and whether it is still live.
pub(crate) struct StripItem {
    pub(crate) kind: FavoriteKind,
    pub(crate) stable_key: String,
    pub(crate) label: String,
    pub(crate) glyph: char,
    /// A favourite whose node type / URI no longer resolves. Kept and
    /// greyed rather than pruned (design §2: the provider may come back).
    pub(crate) alive: bool,
}

impl StripItem {
    /// What dragging this item out of the strip would insert. Dead
    /// favourites (unregistered type, unparsable URI) are not draggable —
    /// there is nothing to insert.
    pub(crate) fn payload(&self) -> Option<DragPayload> {
        if !self.alive {
            return None;
        }
        match self.kind {
            FavoriteKind::NodeType => Some(DragPayload::NodeType {
                type_id: self.stable_key.clone(),
                label: self.label.clone(),
                glyph: self.glyph,
            }),
            FavoriteKind::Project => Some(DragPayload::File {
                uri: self.stable_key.parse::<StorageUri>().ok()?,
                label: self.label.clone(),
                glyph: self.glyph,
            }),
        }
    }
}

/// The favourites, resolved against the live registry for this frame.
pub(crate) fn collect_items(state: &AppState) -> Vec<StripItem> {
    let favorites = state.favorites.lock().unwrap().clone();
    favorites
        .list()
        .iter()
        .map(|fav| {
            let (label, glyph, alive) = match fav.resolve(&state.registry) {
                FavoriteResolution::NodeType { def, display_name } => {
                    (display_name, node_type_glyph(def), true)
                }
                FavoriteResolution::Project { display_name, .. } => {
                    (display_name, crate::fa::FILE_NEW, true)
                }
                FavoriteResolution::Dead => (
                    fav.display_name.clone(),
                    match fav.kind {
                        FavoriteKind::NodeType => crate::fa::CUBE,
                        FavoriteKind::Project => crate::fa::FILE_NEW,
                    },
                    false,
                ),
            };
            StripItem {
                kind: fav.kind,
                stable_key: fav.stable_key.clone(),
                label,
                glyph,
                alive,
            }
        })
        .collect()
}

/// Palette glyph for a node type — the same category icon the Add Node
/// menu shows, so a favourite reads as the thing it adds.
fn node_type_glyph(def: &Arc<dyn NodeDef>) -> char {
    crate::top_menu_bar::category_icon(def.category()).unwrap_or(crate::fa::CUBE)
}

/// Paint the strip's background, its (scrolled) items, and the pin
/// affordance.
///
/// `items_viewport` is the scroll region: items are clipped to it, so a
/// partly scrolled item is cut off exactly where the bar stops
/// hit-testing it, and the anchored pin below is never painted over.
pub(crate) fn paint_strip(
    ctx: &mut dyn DrawCtx,
    strip: Rect,
    items_viewport: Rect,
    items: &[(Rect, &StripItem)],
    pin: Option<Rect>,
) {
    let visuals = current_visuals();
    // The strip reads as its own column of chrome next to the panel.
    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rect(strip.x, strip.y, strip.width, strip.height);
    ctx.fill();

    ctx.save();
    ctx.clip_rect(
        items_viewport.x,
        items_viewport.y,
        items_viewport.width,
        items_viewport.height,
    );
    for (rect, item) in items {
        paint_item(ctx, *rect, item);
    }
    ctx.restore();

    if let Some(pin) = pin {
        let slot = geom::icon_slot(pin);
        ctx.set_fill_color(visuals.text_dim);
        ctx.set_font_size(SLOT_GLYPH_SIZE);
        ctx.fill_text(
            &crate::fa::PLUS_CIRCLE.to_string(),
            slot.x + (slot.width - SLOT_GLYPH_SIZE) * 0.5,
            slot.y + (slot.height - SLOT_GLYPH_SIZE) * 0.5,
        );
        paint_label(ctx, pin, "Pin", visuals.text_dim);
    }
}

fn paint_item(ctx: &mut dyn DrawCtx, item: Rect, info: &StripItem) {
    let visuals = current_visuals();
    let color = if info.alive {
        visuals.text_color
    } else {
        visuals.text_dim
    };
    // The slot: a faint tile so the icon reads as a button even before
    // 6f-2 renders the real primitive into it.
    let slot = geom::icon_slot(item);
    ctx.set_fill_color(visuals.text_color.with_alpha(0.06));
    ctx.begin_path();
    ctx.rect(slot.x, slot.y, slot.width, slot.height);
    ctx.fill();

    ctx.set_fill_color(color);
    ctx.set_font_size(SLOT_GLYPH_SIZE);
    // `slot` is already clamped to the item by `icon_slot`, so both axes
    // centre on its own size.
    ctx.fill_text(
        &info.glyph.to_string(),
        slot.x + (slot.width - SLOT_GLYPH_SIZE) * 0.5,
        slot.y + (slot.height - SLOT_GLYPH_SIZE) * 0.5,
    );
    paint_label(ctx, item, &info.label, color);
}

fn paint_label(ctx: &mut dyn DrawCtx, item: Rect, label: &str, color: agg_gui::Color) {
    let box_ = geom::label_box(item);
    ctx.set_fill_color(color);
    ctx.set_font_size(LABEL_SIZE);
    let text = elide(label, (box_.width - 4.0).max(0.0), LABEL_SIZE);
    let w = crate::file_browser::widget_geom::measure(&text, LABEL_SIZE);
    ctx.fill_text(
        &text,
        box_.x + (box_.width - w) * 0.5,
        box_.y + (box_.height - LABEL_SIZE) * 0.5,
    );
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
