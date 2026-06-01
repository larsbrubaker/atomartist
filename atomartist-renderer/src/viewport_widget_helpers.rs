//! Free helper functions for `viewport_widget`: small vector ops,
//! the wgpu-style NDC projection helper, the rotate cursor's
//! polyline-circle stroker, and the `MouseButton` → bitmask map
//! used by the "stale drag" safety net.
//!
//! Split out of `viewport_widget.rs` to keep that file under the
//! repository line-count guardrail. Everything here is a leaf
//! utility; no widget-tree state is touched.

use agg_gui::{DrawCtx, MouseButton};
use manifold_rust::types::MeshGL;

use crate::camera::transform_point4;

/// Extract the (x, y, z) position of vertex `i` from a `MeshGL`
/// with vertex stride `stride` floats per vertex.
pub(crate) fn vert_pos(mesh: &MeshGL, i: usize, stride: usize) -> [f32; 3] {
    [
        mesh.vert_properties[i * stride],
        mesh.vert_properties[i * stride + 1],
        mesh.vert_properties[i * stride + 2],
    ]
}

/// World-space AABB of the body whose `origin` matches `selection`,
/// computed by transforming the body's local AABB corners through
/// `body.matrix`. Returns `None` when no body in `geometry` claims
/// the selection (or when the selected body has empty geometry).
///
/// Loose under rotation (transformed-AABB envelope, not the true
/// world AABB of every vertex) — same trade-off `FitToBounds` and
/// `Align` make. Tight enough for sizing the Z control / bounds
/// gizmo above the selected body.
pub(crate) fn selected_body_world_aabb(
    geometry: Option<&atomartist_lib::geometry::Geometry3d>,
    selection: atomartist_lib::graph::node::NodeId,
) -> Option<([f32; 3], [f32; 3])> {
    let geom = geometry?;
    for body in geom.iter() {
        if body.origin == Some(selection) {
            let local = mesh_aabb(&body.mesh)?;
            return Some(world_aabb_from_local(local, &body.matrix));
        }
    }
    None
}

/// Transform the 8 corners of a local AABB by `matrix` and return the
/// world-space AABB enclosing the transformed corners. Mirrors the
/// helper in `nodes/ops_3d/fit_to_bounds_node.rs`; we keep two copies
/// because the renderer doesn't want to depend on internal node code
/// and the cost of inlining is trivial.
fn world_aabb_from_local(
    local: ([f32; 3], [f32; 3]),
    matrix: &[f32; 16],
) -> ([f32; 3], [f32; 3]) {
    let (mn, mx) = local;
    let corners = [
        [mn[0], mn[1], mn[2]], [mx[0], mn[1], mn[2]],
        [mn[0], mx[1], mn[2]], [mx[0], mx[1], mn[2]],
        [mn[0], mn[1], mx[2]], [mx[0], mn[1], mx[2]],
        [mn[0], mx[1], mx[2]], [mx[0], mx[1], mx[2]],
    ];
    let mut wmn = [f32::INFINITY; 3];
    let mut wmx = [f32::NEG_INFINITY; 3];
    for c in &corners {
        let t = mat4_transform_point(matrix, *c);
        for k in 0..3 {
            if t[k] < wmn[k] { wmn[k] = t[k]; }
            if t[k] > wmx[k] { wmx[k] = t[k]; }
        }
    }
    (wmn, wmx)
}

fn mat4_transform_point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
    let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
    let z = m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14];
    let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
    if (w - 1.0).abs() < 1e-6 || w == 0.0 { [x, y, z] } else { [x / w, y / w, z / w] }
}

/// Axis-aligned bounding box of a mesh, returned as `(min, max)`.
/// `None` when the mesh has no usable vertex data — caller should
/// fall back to a sensible default. Used by viewport's auto-fit /
/// bounds-gizmo / outline-width-estimate paths.
pub(crate) fn mesh_aabb(mesh: &MeshGL) -> Option<([f32; 3], [f32; 3])> {
    if mesh.num_prop == 0 || mesh.vert_properties.is_empty() {
        return None;
    }
    let stride = mesh.num_prop as usize;
    let n = mesh.vert_properties.len() / stride;
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    for i in 0..n {
        for k in 0..3 {
            let v = mesh.vert_properties[i * stride + k];
            if v < mn[k] { mn[k] = v; }
            if v > mx[k] { mx[k] = v; }
        }
    }
    if !mn[0].is_finite() || !mx[0].is_finite() {
        return None;
    }
    Some((mn, mx))
}

/// Pick an outline thickness scaled to the model's bounding-box
/// extent so the silhouette reads at any model size without
/// micro-tuning per scene. 0.6% of the largest dimension is enough
/// to be visible from typical orbit distances, small enough not to
/// obscure surface detail.
pub(crate) fn estimate_outline_width(mesh: &MeshGL) -> f32 {
    let Some((mn, mx)) = mesh_aabb(mesh) else {
        return 0.05;
    };
    let dx = mx[0] - mn[0];
    let dy = mx[1] - mn[1];
    let dz = mx[2] - mn[2];
    let extent = dx.max(dy).max(dz).max(1e-3);
    (extent * 0.006).max(0.005)
}

pub(crate) fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    [v[0] / l, v[1] / l, v[2] / l]
}

/// Bitmask slot for a `MouseButton` inside `Viewport3dWidget`'s
/// `pressed_buttons` field. Bit 0 = Left, bit 1 = Right, bit 2 =
/// Middle. `Other(_)` is ignored — those buttons don't control
/// any of our drag modes.
pub(crate) fn mouse_button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        MouseButton::Other(_) => 0,
    }
}

/// Stroke a small approximate-circle in widget-local pixels.
/// 24-segment polyline keeps the cursor smooth at the sizes we
/// draw it (radius ≈ 8 px).
pub(crate) fn stroke_circle(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    use std::f64::consts::TAU;
    let steps = 24;
    ctx.begin_path();
    for i in 0..=steps {
        let a = (i as f64 / steps as f64) * TAU;
        let x = cx + r * a.cos();
        let y = cy + r * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
}

/// Draw a MatterCAD-style readout label centred at widget-local
/// `(sx, sy)`: theme **text colour** on a rounded **background-colour**
/// pill with a faint border, so it reads against the 3-D scene. Shared
/// by the rotate-angle and Z-drag distance readouts. Falls back to
/// plain text if the backend can't measure the run.
pub(crate) fn paint_text_pill(ctx: &mut dyn DrawCtx, sx: f64, sy: f64, label: &str) {
    let visuals = ctx.visuals();
    ctx.set_font_size(14.0);
    let Some(m) = ctx.measure_text(label) else {
        ctx.set_fill_color(visuals.text_color);
        ctx.fill_text(label, sx, sy);
        return;
    };
    let pad = 6.0;
    let radius = 6.0;
    let bx = sx - m.width * 0.5 - pad;
    let by = sy - (m.ascent + m.descent) * 0.5 - pad;
    let bw = m.width + pad * 2.0;
    let bh = m.ascent + m.descent + pad * 2.0;
    // Rounded theme-background pill.
    ctx.set_fill_color(visuals.bg_color);
    ctx.begin_path();
    ctx.rounded_rect(bx, by, bw, bh, radius);
    ctx.fill();
    // Faint border so the pill separates from a same-coloured backdrop.
    ctx.set_stroke_color(visuals.text_color.with_alpha(0.25));
    ctx.set_line_width(1.0);
    ctx.begin_path();
    ctx.rounded_rect(bx, by, bw, bh, radius);
    ctx.stroke();
    // Centred text in the theme text colour.
    let baseline = sy - (m.ascent - m.descent) * 0.5;
    ctx.set_fill_color(visuals.text_color);
    ctx.fill_text(label, sx - m.width * 0.5, baseline);
}

/// Project a world-space point through the MVP matrix and map to
/// widget-local pixel coords. Returns `None` if the point is
/// behind the near plane (`w ≤ 0`). Matches the wgpu / Vulkan NDC
/// convention (z in `[0, 1]`).
pub(crate) fn project(mvp: &[f32; 16], p: [f32; 3], w: f64, h: f64) -> Option<(f64, f64)> {
    let h4 = transform_point4(mvp, p);
    if h4[3].abs() < 1e-6 {
        return None;
    }
    let inv_w = 1.0 / h4[3];
    if h4[3] <= 0.0 {
        return None;
    }
    let ndc_x = h4[0] * inv_w;
    let ndc_y = h4[1] * inv_w;
    // Map NDC [-1,1] to widget local pixel space, Y-up.
    let sx = (ndc_x as f64 * 0.5 + 0.5) * w;
    let sy = (ndc_y as f64 * 0.5 + 0.5) * h;
    Some((sx, sy))
}
