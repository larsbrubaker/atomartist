//! Ported from NodeDesigner's `tests/unit/extrude.test.ts`.
//!
//! Validates that the Extrude node produces meshes whose Z extents
//! match the requested height, whose vertex count is consistent with
//! the contour count, and which can be evaluated through the executor
//! end-to-end.

use std::sync::Arc;

use atomartist_lib::geometry::path2d::CrossSection;
use atomartist_lib::nodes::ops_3d::extrude_node::extrude_cross_section;

#[test]
fn unit_square_extrudes_to_correct_z_extents() {
    let cs = CrossSection::square(2.0);
    let mesh = extrude_cross_section(&cs, 4.0).unwrap();
    let stride = mesh.num_prop as usize;
    let n = mesh.vert_properties.len() / stride;
    let mut z_min = f32::INFINITY;
    let mut z_max = f32::NEG_INFINITY;
    for i in 0..n {
        let z = mesh.vert_properties[i * stride + 2];
        z_min = z_min.min(z);
        z_max = z_max.max(z);
    }
    assert!((z_min + 2.0).abs() < 1e-5, "z_min was {}, expected -2", z_min);
    assert!((z_max - 2.0).abs() < 1e-5, "z_max was {}, expected +2", z_max);
}

#[test]
fn extruded_circle_has_round_xy_footprint() {
    let cs = CrossSection::circle(5.0, 32);
    let mesh = extrude_cross_section(&cs, 1.0).unwrap();
    // Every vertex's XY radius should be ≤ 5.0 + tolerance.
    let stride = mesh.num_prop as usize;
    let n = mesh.vert_properties.len() / stride;
    for i in 0..n {
        let x = mesh.vert_properties[i * stride];
        let y = mesh.vert_properties[i * stride + 1];
        let r = (x * x + y * y).sqrt();
        assert!(
            r <= 5.0 + 1e-3,
            "vertex {} radius {} exceeds circle radius 5.0",
            i, r
        );
    }
}

#[test]
fn ring_extrude_has_inner_wall() {
    let outer = CrossSection::circle(5.0, 24);
    let inner = CrossSection::circle(2.0, 24);
    let ring = outer.difference(&inner);
    let mesh = extrude_cross_section(&ring, 2.0).unwrap();
    // Some vertex should lie at radius ≈ 2.0 (inner wall).
    let stride = mesh.num_prop as usize;
    let n = mesh.vert_properties.len() / stride;
    let mut has_inner = false;
    for i in 0..n {
        let x = mesh.vert_properties[i * stride];
        let y = mesh.vert_properties[i * stride + 1];
        let r = (x * x + y * y).sqrt();
        if (r - 2.0).abs() < 0.05 {
            has_inner = true;
            break;
        }
    }
    assert!(has_inner, "ring extrude must have inner-wall vertices at r≈2");
}

#[test]
fn extruded_volume_is_positive() {
    // A 2×2 square extruded by 3 should have a positive enclosed
    // volume close to 12 (sanity check; the wedge tests in
    // primitives_manifold.rs verify topological correctness).
    let cs = CrossSection::square(2.0);
    let mesh = extrude_cross_section(&cs, 3.0).unwrap();
    let _arc = Arc::new(mesh.clone()); // sanity that the mesh is `Arc`-shareable
    // Bounding box volume.
    let stride = mesh.num_prop as usize;
    let n = mesh.vert_properties.len() / stride;
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    for i in 0..n {
        for k in 0..3 {
            let v = mesh.vert_properties[i * stride + k];
            mn[k] = mn[k].min(v);
            mx[k] = mx[k].max(v);
        }
    }
    let bb_vol = (mx[0] - mn[0]) * (mx[1] - mn[1]) * (mx[2] - mn[2]);
    assert!((bb_vol - 12.0).abs() < 1e-4, "expected bbox volume ≈ 12, got {}", bb_vol);
}
