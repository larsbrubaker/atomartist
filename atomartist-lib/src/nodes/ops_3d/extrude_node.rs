//! Extrude — convert a `CrossSection` (2D path) into a 3D `MeshGL` solid by
//! sweeping it along Z by `height`, then applying an optional 4×4 matrix
//! transform.
//!
//! NodeDesigner parity: the node exposes eight input rows — `Paths`,
//! `Height`, `Radius`, `Segments`, `Bottom Radius`, `Bottom Segments`,
//! `Color`, and `Matrix` — each with an inline fallback editor. Any
//! connected upstream value overrides the inline property. Bevels are
//! schema-only at the moment (the geometry path still extrudes straight
//! walls); `Color` is preserved as graph metadata pending the renderer's
//! per-mesh material story.
//!
//! Property layout lives on the typed [`ExtrudeProps`] struct which
//! derives [`bevy_reflect::Reflect`] so future inspector / form-driven UI
//! can iterate fields by type. The `props_layout` table pairs each field
//! with its editor metadata + bound input socket.
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
//! sides.

use std::sync::Arc;

use bevy_reflect::Reflect;
use manifold_rust::types::MeshGL;
use tess2_rust::{ElementType, Tessellator, WindingRule};

use crate::geometry::apply_transform;
use crate::geometry::mesh3d::{make_mesh, NUM_PROP, STRIDE};
use crate::geometry::path2d::{is_ccw, CrossSection, Vec2D};
use crate::graph::node::{identity_matrix, PortValue};
use crate::registry::{
    EditorKind, NodeDef, NodeError, NodeFieldAttrs, NodeInputs, NodeOutputs, NodeProperties,
    NodeRegistry, NumberAttrs, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

/// Typed property struct for the Extrude node — mirrors NodeDesigner's
/// `ExtrudeNode.properties`. Derives [`Reflect`] so reflection-driven
/// tooling can iterate field types at runtime; the matching
/// [`props_layout`] table carries the editor / label / socket-binding
/// metadata that lives outside the reflected type system.
#[derive(Clone, Debug, Reflect)]
pub struct ExtrudeProps {
    pub height: f64,
    pub bevel_radius: f64,
    pub bevel_segments: f64,
    pub bottom_radius: f64,
    pub bottom_segments: f64,
    pub color: [f32; 4],
    pub matrix: [f32; 16],
}

impl Default for ExtrudeProps {
    fn default() -> Self {
        Self {
            height: 5.0,
            bevel_radius: 0.0,
            bevel_segments: 8.0,
            bottom_radius: 0.0,
            bottom_segments: 8.0,
            color: [1.0, 1.0, 1.0, 1.0],
            matrix: identity_matrix(),
        }
    }
}

impl ExtrudeProps {
    /// Read a snapshot from a property bag, applying upstream input
    /// overrides first and falling back to the property's stored value
    /// (then to the type default).
    pub fn resolve(inputs: &NodeInputs, props: &NodeProperties) -> Self {
        let def = Self::default();
        let height = match inputs.get("Height") {
            PortValue::Number(n) => *n,
            _ => props.number("height", def.height),
        };
        let bevel_radius = match inputs.get("Radius") {
            PortValue::Number(n) => *n,
            _ => props.number("bevel_radius", def.bevel_radius),
        };
        let bevel_segments = match inputs.get("Segments") {
            PortValue::Number(n) => *n,
            _ => props.number("bevel_segments", def.bevel_segments),
        };
        let bottom_radius = match inputs.get("Bottom Radius") {
            PortValue::Number(n) => *n,
            _ => props.number("bottom_radius", def.bottom_radius),
        };
        let bottom_segments = match inputs.get("Bottom Segments") {
            PortValue::Number(n) => *n,
            _ => props.number("bottom_segments", def.bottom_segments),
        };
        let color = match inputs.get("Color") {
            PortValue::Color(c) => *c,
            _ => match props.get("color") {
                PortValue::Color(c) => *c,
                _ => def.color,
            },
        };
        let matrix = match inputs.get("Matrix") {
            PortValue::Matrix4x4(m) => *m,
            _ => match props.get("matrix") {
                PortValue::Matrix4x4(m) => *m,
                _ => def.matrix,
            },
        };
        Self {
            height,
            bevel_radius,
            bevel_segments,
            bottom_radius,
            bottom_segments,
            color,
            matrix,
        }
    }
}

/// Static layout describing each [`ExtrudeProps`] field: the canonical
/// property name (also the JSON key), its default `PortValue`, and the
/// editor + binding metadata. The node's [`NodeDef::properties`] and
/// [`NodeDef::input_sockets`] are both derived from this table so we
/// never duplicate the schema across two sources.
fn props_layout() -> Vec<(&'static str, PortValue, NodeFieldAttrs)> {
    let def = ExtrudeProps::default();
    vec![
        (
            "height",
            PortValue::Number(def.height),
            NodeFieldAttrs::new()
                .with_label("Height")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(0.1, 40.0)
                        .with_ease_in(2.0)
                        .with_snap_grid(),
                ))
                .bound_to("Height"),
        ),
        (
            "bevel_radius",
            PortValue::Number(def.bevel_radius),
            NodeFieldAttrs::new()
                .with_label("Radius")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(0.0, 10.0).with_ease_in(2.0),
                ))
                .bound_to("Radius"),
        ),
        (
            "bevel_segments",
            PortValue::Number(def.bevel_segments),
            NodeFieldAttrs::new()
                .with_label("Segments")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(1.0, 30.0)
                        .integer()
                        .with_step(1.0),
                ))
                .bound_to("Segments"),
        ),
        (
            "bottom_radius",
            PortValue::Number(def.bottom_radius),
            NodeFieldAttrs::new()
                .with_label("Bottom Radius")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(0.0, 10.0).with_ease_in(2.0),
                ))
                .bound_to("Bottom Radius"),
        ),
        (
            "bottom_segments",
            PortValue::Number(def.bottom_segments),
            NodeFieldAttrs::new()
                .with_label("Bottom Segments")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(1.0, 30.0)
                        .integer()
                        .with_step(1.0),
                ))
                .bound_to("Bottom Segments"),
        ),
        (
            "color",
            PortValue::Color(def.color),
            NodeFieldAttrs::new()
                .with_label("Color")
                .with_editor(EditorKind::ColorPicker)
                .bound_to("Color"),
        ),
        (
            "matrix",
            PortValue::Matrix4x4(def.matrix),
            NodeFieldAttrs::new()
                .with_label("Matrix")
                .with_editor(EditorKind::Matrix)
                .bound_to("Matrix"),
        ),
    ]
}

pub struct ExtrudeNode;

impl NodeDef for ExtrudeNode {
    fn type_id(&self) -> &'static str { "Extrude" }
    fn display_name(&self) -> &'static str { "Extrude" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![
            SocketDef::required("Paths", SocketType::Path2d).with_label("Paths"),
            SocketDef::optional("Height", SocketType::Number).with_label("Height"),
            SocketDef::optional("Radius", SocketType::Number).with_label("Radius"),
            SocketDef::optional("Segments", SocketType::Number).with_label("Segments"),
            SocketDef::optional("Bottom Radius", SocketType::Number).with_label("Bottom Radius"),
            SocketDef::optional("Bottom Segments", SocketType::Number).with_label("Bottom Segments"),
            SocketDef::optional("Color", SocketType::Color).with_label("Color"),
            SocketDef::optional("Matrix", SocketType::Matrix4x4).with_label("Matrix"),
        ]
    }

    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("Geometry", SocketType::Geometry3d).with_label("Geometry")]
    }

    fn properties(&self) -> Vec<PropDef> {
        props_layout()
            .into_iter()
            .map(|(name, default, attrs)| PropDef::from_attrs(name, default, &attrs))
            .collect()
    }

    fn evaluate(&self, inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let cross_section = match inputs.get("Paths") {
            PortValue::Path2d(p) => p.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Extrude: expected Path2d on 'Paths', got {:?}", other.socket_type()
            ))),
        };
        let resolved = ExtrudeProps::resolve(inputs, props);
        let height = resolved.height.max(1e-6);
        let mut mesh = extrude_cross_section(&cross_section, height as f32)
            .map_err(|e| NodeError::msg(e))?;
        if resolved.matrix != identity_matrix() {
            mesh = apply_transform(&mesh, &resolved.matrix);
        }
        let mut out = NodeOutputs::default();
        out.set("Geometry", PortValue::Geometry3d(Arc::new(mesh)));
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
    let cap_verts: &[f64] = tess.vertices();
    let cap_indices: &[u32] = tess.elements();

    let mut vert_properties: Vec<f32> = Vec::new();
    let mut tri_verts: Vec<u32> = Vec::new();

    // Top cap (z = +h_top, normal = +Z, original winding from tess2).
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

    // Bottom cap (z = -h_top, normal = -Z, reversed winding).
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
        tri_verts.extend_from_slice(&[
            bot_base + tri[0],
            bot_base + tri[2],
            bot_base + tri[1],
        ]);
    }

    // Side walls.
    for contour in &polys {
        if contour.len() < 2 { continue; }
        let ccw = is_ccw(contour);
        for i in 0..contour.len() {
            let a = contour[i];
            let b = contour[(i + 1) % contour.len()];
            let ex = b.x - a.x;
            let ey = b.y - a.y;
            let len = (ex * ex + ey * ey).sqrt().max(1e-12);
            let mut nx = ey / len;
            let mut ny = -ex / len;
            if !ccw {
                nx = -nx;
                ny = -ny;
            }
            let n = [nx as f32, ny as f32, 0.0];

            let base = (vert_properties.len() / STRIDE) as u32;
            for &(x, y, z) in &[
                (a.x as f32, a.y as f32, h_bot),
                (b.x as f32, b.y as f32, h_bot),
                (b.x as f32, b.y as f32, h_top),
                (a.x as f32, a.y as f32, h_top),
            ] {
                vert_properties.extend_from_slice(&[x, y, z, n[0], n[1], n[2]]);
            }
            tri_verts.extend_from_slice(&[
                base, base + 1, base + 2,
                base, base + 2, base + 3,
            ]);
        }
    }

    if vert_properties.len() % STRIDE != 0 {
        return Err("internal: vert stride mismatch".into());
    }
    if tri_verts.len() % 3 != 0 {
        return Err("internal: tri index count mismatch".into());
    }

    let _ = NUM_PROP;
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
        let stride = mesh.num_prop as usize;
        assert!(stride >= 6);
        let n = mesh.vert_properties.len() / stride;
        assert!(n > 0);
        let mut z_min = f32::INFINITY;
        let mut z_max = f32::NEG_INFINITY;
        for i in 0..n {
            let z = mesh.vert_properties[i * stride + 2];
            if z < z_min { z_min = z; }
            if z > z_max { z_max = z; }
        }
        assert!((z_min + 1.0).abs() < 1e-5, "z_min was {}", z_min);
        assert!((z_max - 1.0).abs() < 1e-5, "z_max was {}", z_max);
        assert!(mesh.tri_verts.len() / 3 >= 12);
    }

    #[test]
    fn extrude_ring_has_inner_wall() {
        let outer = CS::circle(2.0, 24);
        let inner = CS::circle(1.0, 24);
        let ring = outer.difference(&inner);
        let mesh = extrude_cross_section(&ring, 1.0).unwrap();
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

    #[test]
    fn schema_matches_nodedesigner_inputs() {
        let n = ExtrudeNode;
        let inputs: Vec<&str> = n.input_sockets().iter().map(|s| s.name).collect();
        assert_eq!(
            inputs,
            vec![
                "Paths",
                "Height",
                "Radius",
                "Segments",
                "Bottom Radius",
                "Bottom Segments",
                "Color",
                "Matrix",
            ]
        );
        let outputs: Vec<&str> = n.output_sockets().iter().map(|s| s.name).collect();
        assert_eq!(outputs, vec!["Geometry"]);
    }

    #[test]
    fn every_optional_input_has_a_bound_property() {
        let n = ExtrudeNode;
        let optional_inputs: Vec<&str> = n
            .input_sockets()
            .iter()
            .filter(|s| s.optional)
            .map(|s| s.name)
            .collect();
        let props = n.properties();
        for input in optional_inputs {
            let matched = props.iter().any(|p| p.bound_input == Some(input));
            assert!(matched, "no property bound to input '{}'", input);
        }
    }

    #[test]
    fn height_property_defaults_to_5_with_node_designer_range() {
        let n = ExtrudeNode;
        let props = n.properties();
        let height = props.iter().find(|p| p.name == "height").unwrap();
        match &height.default {
            PortValue::Number(v) => assert!((v - 5.0).abs() < 1e-9),
            _ => panic!("height default should be a Number"),
        }
        assert_eq!(height.min, Some(0.1));
        assert_eq!(height.max, Some(40.0));
        assert_eq!(height.bound_input, Some("Height"));
        assert_eq!(height.label, Some("Height"));
    }

    #[test]
    fn segments_property_marked_integer() {
        let n = ExtrudeNode;
        let props = n.properties();
        let segments = props.iter().find(|p| p.name == "bevel_segments").unwrap();
        let attrs = segments
            .editor
            .number_attrs()
            .expect("segments should be a numeric editor");
        assert!(attrs.integer, "bevel_segments should be an integer field");
    }

    #[test]
    fn color_default_is_white() {
        let n = ExtrudeNode;
        let props = n.properties();
        let color = props.iter().find(|p| p.name == "color").unwrap();
        match &color.default {
            PortValue::Color(c) => assert_eq!(*c, [1.0, 1.0, 1.0, 1.0]),
            _ => panic!("color default should be a Color"),
        }
        assert!(matches!(color.editor, EditorKind::ColorPicker));
    }

    #[test]
    fn matrix_default_is_identity() {
        let n = ExtrudeNode;
        let props = n.properties();
        let m = props.iter().find(|p| p.name == "matrix").unwrap();
        match &m.default {
            PortValue::Matrix4x4(mat) => assert_eq!(*mat, identity_matrix()),
            _ => panic!("matrix default should be a Matrix4x4"),
        }
        assert!(matches!(m.editor, EditorKind::Matrix));
    }

    #[test]
    fn resolve_prefers_connected_input_over_property() {
        let mut inputs = NodeInputs::default();
        inputs.insert("Height", PortValue::Number(12.5));
        let mut props = NodeProperties::default();
        props.insert("height", PortValue::Number(3.0));
        let resolved = ExtrudeProps::resolve(&inputs, &props);
        assert!((resolved.height - 12.5).abs() < 1e-9);
    }

    #[test]
    fn resolve_falls_back_to_property_when_input_unconnected() {
        let inputs = NodeInputs::default();
        let mut props = NodeProperties::default();
        props.insert("height", PortValue::Number(7.5));
        let resolved = ExtrudeProps::resolve(&inputs, &props);
        assert!((resolved.height - 7.5).abs() < 1e-9);
    }

    #[test]
    fn resolve_falls_back_to_default_when_property_missing() {
        let inputs = NodeInputs::default();
        let props = NodeProperties::default();
        let resolved = ExtrudeProps::resolve(&inputs, &props);
        let def = ExtrudeProps::default();
        assert!((resolved.height - def.height).abs() < 1e-9);
        assert!((resolved.bevel_radius - def.bevel_radius).abs() < 1e-9);
        assert_eq!(resolved.color, def.color);
        assert_eq!(resolved.matrix, def.matrix);
    }

    #[test]
    fn evaluate_applies_matrix_input_to_output_mesh() {
        let cs = CS::square(2.0);
        let mut inputs = NodeInputs::default();
        inputs.insert("Paths", PortValue::Path2d(Arc::new(cs)));
        // Translate the result by (0, 0, 10).
        let translate_z10: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 10.0, 1.0,
        ];
        inputs.insert("Matrix", PortValue::Matrix4x4(translate_z10));
        let mut props = NodeProperties::default();
        props.insert("height", PortValue::Number(2.0));
        let out = ExtrudeNode.evaluate(&inputs, &props).unwrap();
        match out.by_name.get("Geometry").unwrap() {
            PortValue::Geometry3d(m) => {
                let stride = m.num_prop as usize;
                let nv = m.vert_properties.len() / stride;
                let mut z_min = f32::INFINITY;
                let mut z_max = f32::NEG_INFINITY;
                for i in 0..nv {
                    let z = m.vert_properties[i * stride + 2];
                    if z < z_min { z_min = z; }
                    if z > z_max { z_max = z; }
                }
                // Original mesh spans z in [-1, 1]; translated by +10.
                assert!((z_min - 9.0).abs() < 1e-4, "z_min was {}", z_min);
                assert!((z_max - 11.0).abs() < 1e-4, "z_max was {}", z_max);
            }
            _ => panic!("expected Geometry3d output"),
        }
    }
}
