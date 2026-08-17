//! Orientation test for the Boolean operation artwork.
//!
//! [`super`]'s own tests check the *data* (ids, colour roles, fill
//! rules). This file checks where that data lands on screen, because the
//! icon space the SVGs are authored in is Y-down and agg-gui is Y-up:
//! the whole family is drawn with the kept block up and left and the
//! operand down and right, and a missing — or doubled — flip would turn
//! every icon upside down while every data-level assertion stayed green.
//!
//! The check is deliberately about *relative placement of the colour
//! roles*, not pixels: it survives re-drawing the artwork and still
//! fails the moment the Y term in `VectorIcon::paint` is wrong.

use std::sync::Arc;

use agg_gui::draw_ctx::FillRule;
use agg_gui::vector_icon::icon;
use agg_gui::{Color, CompOp, DrawCtx, LineCap, LineJoin, Rect, TextMetrics, TransAffine};

use super::*;

/// Records the bounding box and colour of every filled path.
#[derive(Default)]
struct FillRecorder {
    color: Color,
    points: Vec<[f64; 2]>,
    fills: Vec<(Color, [f64; 4])>,
}

impl FillRecorder {
    /// Centre of the (single) fill painted in `color`.
    fn centre_of(&self, color: Color) -> [f64; 2] {
        let matches: Vec<&(Color, [f64; 4])> = self
            .fills
            .iter()
            .filter(|(c, _)| {
                (c.r - color.r).abs() < 1e-6
                    && (c.g - color.g).abs() < 1e-6
                    && (c.b - color.b).abs() < 1e-6
            })
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one fill in {color:?}, got {}",
            matches.len()
        );
        let b = matches[0].1;
        [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5]
    }
}

impl DrawCtx for FillRecorder {
    fn set_fill_color(&mut self, color: Color) {
        self.color = color;
    }
    fn set_stroke_color(&mut self, _c: Color) {}
    fn set_line_width(&mut self, _w: f64) {}
    fn set_line_join(&mut self, _j: LineJoin) {}
    fn set_line_cap(&mut self, _c: LineCap) {}
    fn set_miter_limit(&mut self, _l: f64) {}
    fn set_line_dash(&mut self, _d: &[f64], _o: f64) {}
    fn set_blend_mode(&mut self, _m: CompOp) {}
    fn set_global_alpha(&mut self, _a: f64) {}
    fn set_fill_rule(&mut self, _r: FillRule) {}
    fn set_font(&mut self, _f: Arc<agg_gui::text::Font>) {}
    fn set_font_size(&mut self, _s: f64) {}
    fn clip_rect(&mut self, _x: f64, _y: f64, _w: f64, _h: f64) {}
    fn reset_clip(&mut self) {}
    fn clear(&mut self, _c: Color) {}
    fn begin_path(&mut self) {
        self.points.clear();
    }
    fn move_to(&mut self, x: f64, y: f64) {
        self.points.push([x, y]);
    }
    fn line_to(&mut self, x: f64, y: f64) {
        self.points.push([x, y]);
    }
    fn cubic_to(&mut self, _a: f64, _b: f64, _c: f64, _d: f64, _e: f64, _f: f64) {}
    fn quad_to(&mut self, _a: f64, _b: f64, _c: f64, _d: f64) {}
    fn arc_to(&mut self, _cx: f64, _cy: f64, _r: f64, _s: f64, _e: f64, _ccw: bool) {}
    fn circle(&mut self, _cx: f64, _cy: f64, _r: f64) {}
    fn rect(&mut self, _x: f64, _y: f64, _w: f64, _h: f64) {}
    fn rounded_rect(&mut self, _x: f64, _y: f64, _w: f64, _h: f64, _r: f64) {}
    fn close_path(&mut self) {}
    fn fill(&mut self) {
        if self.points.len() >= 3 {
            let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for p in &self.points {
                b[0] = b[0].min(p[0]);
                b[1] = b[1].min(p[1]);
                b[2] = b[2].max(p[0]);
                b[3] = b[3].max(p[1]);
            }
            self.fills.push((self.color, b));
        }
        self.points.clear();
    }
    fn stroke(&mut self) {}
    fn fill_and_stroke(&mut self) {}
    fn draw_triangles_aa(&mut self, _v: &[[f32; 3]], _i: &[u32], _c: Color) {}
    fn fill_text(&mut self, _t: &str, _x: f64, _y: f64) {}
    fn fill_text_gsv(&mut self, _t: &str, _x: f64, _y: f64, _s: f64) {}
    fn measure_text(&self, _t: &str) -> Option<TextMetrics> {
        None
    }
    fn transform(&self) -> TransAffine {
        TransAffine::new()
    }
    fn set_transform(&mut self, _m: TransAffine) {}
    fn reset_transform(&mut self) {}
    fn save(&mut self) {}
    fn restore(&mut self) {}
    fn translate(&mut self, _x: f64, _y: f64) {}
    fn rotate(&mut self, _r: f64) {}
    fn scale(&mut self, _x: f64, _y: f64) {}
}

fn paint(id: &str, rect: Rect) -> FillRecorder {
    register_boolean_icons().expect("the bundled path data parses");
    let art = icon(id).unwrap_or_else(|| panic!("no icon registered for {id}"));
    let mut rec = FillRecorder::default();
    art.paint(&mut rec, rect, Color::black());
    rec
}

/// Subtract's kept blue block sits **up and to the left**; the grey
/// operand it is being cut with sits **down and to the right**.
///
/// The destination rect is Y-up (agg-gui's bottom-left origin), so
/// "up" means a *larger* y. Drop the flip in `VectorIcon::paint`, or
/// apply it twice, and the y half of this inverts.
#[test]
fn subtracts_block_paints_upper_left_and_its_operand_lower_right() {
    let rect = Rect::new(100.0, 200.0, 64.0, 64.0);
    let rec = paint(OPERATION_ICONS[1], rect);

    let kept = rec.centre_of(Color::from_rgb8(0x4B, 0xA9, 0xE8));
    let removed = rec.centre_of(Color::from_rgb8(0x9A, 0x9A, 0x9D));

    assert!(
        kept[0] < removed[0],
        "the kept block should sit left of the operand ({kept:?} vs {removed:?})"
    );
    assert!(
        kept[1] > removed[1],
        "the kept block should sit ABOVE the operand in Y-up space \
         ({kept:?} vs {removed:?}) — the icon is upside down"
    );
    // …and both stay inside the rect they were given.
    for c in [kept, removed] {
        assert!(c[0] > rect.x && c[0] < rect.x + rect.width, "{c:?}");
        assert!(c[1] > rect.y && c[1] < rect.y + rect.height, "{c:?}");
    }
}

/// …and the absolute version of the same check, which no relative
/// comparison can give: the kept block's fill runs from icon y 4 to
/// y 48, an *asymmetric* span, so its exact placement in the
/// destination rect pins the flip's offset term as well as its sign.
///
/// At 1:1 scale in a 64-unit rect the block must occupy
/// `[rect.top - 48, rect.top - 4]`; without the flip it would sit at
/// `[rect.bottom + 4, rect.bottom + 48]`.
#[test]
fn the_kept_block_lands_at_the_exact_flipped_offsets() {
    let rect = Rect::new(0.0, 0.0, 64.0, 64.0);
    let rec = paint(OPERATION_ICONS[1], rect);
    let blue = Color::from_rgb8(0x4B, 0xA9, 0xE8);
    let b = rec
        .fills
        .iter()
        .find(|(c, _)| *c == blue)
        .expect("Subtract paints a kept block")
        .1;

    let top = rect.y + rect.height;
    assert!(
        (b[3] - (top - 4.0)).abs() < 0.5,
        "the block's top edge landed at {} instead of {}",
        b[3],
        top - 4.0
    );
    assert!(
        (b[1] - (top - 48.0)).abs() < 0.5,
        "the block's bottom edge landed at {} instead of {}",
        b[1],
        top - 48.0
    );
    assert!((b[0] - (rect.x + 4.0)).abs() < 0.5, "left edge at {}", b[0]);
}
