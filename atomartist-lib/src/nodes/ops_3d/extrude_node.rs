//! Extrude — convert a `CrossSection` (2D path) into a 3D `MeshGL` solid by
//! sweeping it along Z by `height`.
//!
//! Algorithm:
//!   1. Tessellate the cross-section's contours via `tess2-rust` with the
//!      Odd (even-odd) winding rule. This handles outer + hole contours
//!      correctly without requiring the caller to flag hole-ness.
//!   2. Top cap (Z = +h/2): use the tessellated triangles as-is, normal +Z.
//!   3. Bottom cap (Z = -h/2): reverse winding, normal -Z.
//!   4. Side walls: for every edge in every contour, emit a quad (two
//!      triangles) connecting the contour at top to the contour at bottom.
//!      Side normal is the outward perpendicular to the edge in XY.
//!
//! Side winding is determined by the contour's signed area: CCW contours
//! produce outward-facing sides; CW (hole) contours produce inward-facing
//! sides (i.e. the hole's interior wall, normal pointing INTO the cavity
//! from the surrounding material's perspective). This is what we want.

use std::sync::Arc;

use manifold_rust::types::MeshGL;
use tess2_rust::{ElementType, Tessellator, WindingRule};

use crate::geometry::mesh3d::{make_mesh, NUM_PROP, STRIDE};
use crate::geometry::path2d::{is_ccw, CrossSection, Vec2D};
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct ExtrudeNode;

impl NodeDef for ExtrudeNode {
    fn type_id(&self) -> &'static str { "Extrude" }
    fn display_name(&self) -> &'static str { "Extrude" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("input", SocketType::Path2d)]
    }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("height", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
        ]
    }

    fn evaluate(&self, inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let cross_section = match inputs.get("input") {
            PortValue::Path2d(p) => p.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Extrude: expected Path2d input, got {:?}", other.socket_type()
            ))),
        };
        let height = props.number("height", 10.0).max(1e-6);
        let mesh = extrude_cross_section(&cross_section, height as f32)
            .map_err(|e| NodeError::msg(e))?;
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(mesh)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(ExtrudeNode);
}

/// Extrude a CrossSection into a 3D mesh. `height` is the total Z extent;
/// the resulting solid spans Z = [-height/2, +height/2].
pub fn extrude_cross_section(cs: &CrossSection, height: f32) -> Result<MeshGL, String> {
    let polys: Vec<Vec<Vec2D>> = cs.to_polygons();
    if polys.is_empty() {
        return Err("CrossSection has no contours".into());
    }
    let h_top = height * 0.5;
    let h_bot = -height * 0.5;

    // 1. Tessellate the cap.
    let mut tess = Tessellator::new();
    for contour in &polys {
        let mut flat: Vec<f64> = Vec::with_capacity(contour.len() * 2);
        for v in contour {
            flat.push(v.x);
            flat.push(v.y);
        }
        tess.add_contour(2, &flat);
    }
    let ok = tess.tessellate(
        WindingRule::Odd,
        ElementType::Polygons,
        3,
        2,
        Some([0.0, 0.0, 1.0]),
    );
    if !ok {
        return Err("tess2 tessellation failed".into());
    }
    let cap_verts: &[f64] = tess.vertices(); // flat [x, y, x, y, ...]
    let cap_indices: &[u32] = tess.elements(); // flat [v0, v1, v2, v0, v1, v2, ...]

    let mut vert_properties: Vec<f32> = Vec::new();
    let mut tri_verts: Vec<u32> = Vec::new();

    // ── Top cap (z = +h_top, normal = +Z, original winding from tess2) ───────
    let n_cap_v = cap_verts.len() / 2;
    let top_base = (vert_properties.len() / STRIDE) as u32;
    for i in 0..n_cap_v {
        vert_properties.extend_from_slice(&[
            cap_verts[i * 2] as f32,
            cap_verts[i * 2 + 1] as f32,
            h_top,
            0.0, 0.0, 1.0,
        ]);
    }
    for tri in cap_indices.chunks_exact(3) {
        if tri.iter().any(|&v| v == u32::MAX) { continue; }
        tri_verts.extend_from_slice(&[
            top_base + tri[0],
            top_base + tri[1],
            top_base + tri[2],
        ]);
    }

    // ── Bottom cap (z = -h_top, normal = -Z, reversed winding) ───────────────
    let bot_base = (vert_properties.len() / STRIDE) as u32;
    for i in 0..n_cap_v {
        vert_properties.extend_from_slice(&[
            cap_verts[i * 2] as f32,
            cap_verts[i * 2 + 1] as f32,
            h_bot,
            0.0, 0.0, -1.0,
        ]);
    }
    for tri in cap_indices.chunks_exact(3) {
        if tri.iter().any(|&v| v == u32::MAX) { continue; }
        // Reverse winding for the bottom face so the normal we wrote (-Z)
        // matches the cross-product orientation.
        tri_verts.extend_from_slice(&[
            bot_base + tri[0],
            bot_base + tri[2],
            bot_base + tri[1],
        ]);
    }

    // ── Side walls ──────────────────────────────────────────────────────────
    // Per contour, per edge: emit two triangles forming a quad. Side normal
    // is the outward perpendicular of the edge in XY (rotation of the edge
    // tangent). For CCW contours this points outward; for CW (hole) contours
    // we flip so the normal points into the cavity.
    for contour in &polys {
        if contour.len() < 2 { continue; }
        let ccw = is_ccw(contour);
        for i in 0..contour.len() {
            let a = contour[i];
            let b = contour[(i + 1) % contour.len()];
            let ex = b.x - a.x;
            let ey = b.y - a.y;
            let len = (ex * ex + ey * ey).sqrt().max(1e-12);
            // Right-hand normal of a CCW edge points outward; for CW, flip.
            let mut nx = ey / len;
            let mut ny = -ex / len;
            if !ccw {
                nx = -nx;
                ny = -ny;
            }
            let n = [nx as f32, ny as f32, 0.0];

            let base = (vert_properties.len() / STRIDE) as u32;
            // Quad verts: a-bot, b-bot, b-top, a-top
            for &(x, y, z) in &[
                (a.x as f32, a.y as f32, h_bot),
                (b.x as f32, b.y as f32, h_bot),
                (b.x as f32, b.y as f32, h_top),
                (a.x as f32, a.y as f32, h_top),
            ] {
                vert_properties.extend_from_slice(&[x, y, z, n[0], n[1], n[2]]);
            }
            // CCW from outside (looking at +n): a-bot → b-bot → b-top is one
            // triangle, a-bot → b-top → a-top the second.
            tri_verts.extend_from_slice(&[
                base, base + 1, base + 2,
                base, base + 2, base + 3,
            ]);
        }
    }

    // Cross-check: vert_properties is multiple of STRIDE, indices multiple of 3.
    if vert_properties.len() % STRIDE != 0 {
        return Err("internal: vert stride mismatch".into());
    }
    if tri_verts.len() % 3 != 0 {
        return Err("internal: tri index count mismatch".into());
    }

    let _ = NUM_PROP; // silence unused-import on builds without the constant
    Ok(make_mesh(vert_properties, tri_verts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_rust::cross_section::CrossSection as CS;

    #[test]
    fn extrude_unit_square_produces_solid_box() {
        let cs = CS::square(1.0);
        let mesh = extrude_cross_section(&cs, 2.0).unwrap();
        // Bounding box should span [-0.5, 0.5] in XY (CrossSection::square
        // is centered at origin) and [-1.0, 1.0] in Z.
        let stride = mesh.num_prop as usize;
        assert!(stride >= 6);
        let n = mesh.vert_properties.len() / stride;
        assert!(n > 0);
        let mut z_min = f32::INFINITY; let mut z_max = f32::NEG_INFINITY;
        for i in 0..n {
            let z = mesh.vert_properties[i * stride + 2];
            if z < z_min { z_min = z; }
            if z > z_max { z_max = z; }
        }
        assert!((z_min + 1.0).abs() < 1e-5, "z_min was {}", z_min);
        assert!((z_max - 1.0).abs() < 1e-5, "z_max was {}", z_max);
        // Triangle count: cap-tris × 2 (top+bot) + side-tris (4 edges × 2).
        // tess2 may triangulate the unit square as 2 tris, so caps = 4
        // total, sides = 8, total = 12.
        assert!(mesh.tri_verts.len() / 3 >= 12);
    }

    #[test]
    fn extrude_ring_has_inner_wall() {
        let outer = CS::circle(2.0, 24);
        let inner = CS::circle(1.0, 24);
        let ring = outer.difference(&inner);
        let mesh = extrude_cross_section(&ring, 1.0).unwrap();
        // Inner wall: vertices with x*x + y*y close to 1.
        let stride = mesh.num_prop as usize;
        let n = mesh.vert_properties.len() / stride;
        let mut has_inner = false;
        for i in 0..n {
            let x = mesh.vert_properties[i * stride];
            let y = mesh.vert_properties[i * stride + 1];
            let r = (x * x + y * y).sqrt();
            if (r - 1.0).abs() < 0.05 {
                has_inner = true;
                break;
            }
        }
        assert!(has_inner, "ring extrude should produce inner-wall vertices at r≈1");
    }
}
