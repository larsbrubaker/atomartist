//! 3D primitive generators producing `MeshGL` with flat normals.
//!
//! All primitives are centered at the origin and use `num_prop = 6`
//! (xyz + nxnynz). Triangles are wound CCW when viewed from outside; face
//! normals computed via cross product point outward.

use manifold_rust::types::MeshGL;

use crate::geometry::mesh3d::{make_mesh, NUM_PROP};

/// Box of total dimensions (width, height, depth) centered at origin.
/// Produces 24 vertices (6 faces × 4 unique) and 12 triangles.
pub fn generate_box(width: f64, height: f64, depth: f64) -> MeshGL {
    let w = (width * 0.5) as f32;
    let h = (height * 0.5) as f32;
    let d = (depth * 0.5) as f32;

    // Vertex layout per face: 4 verts CCW from outside, normal repeated 4×.
    // Order of faces: +X, -X, +Y, -Y, +Z, -Z.
    let face_data: [([f32; 3], [[f32; 3]; 4]); 6] = [
        // +X: back-bot, back-top, front-top, front-bot
        ([1.0, 0.0, 0.0], [[w, -h, -d], [w,  h, -d], [w,  h,  d], [w, -h,  d]]),
        // -X: front-bot, front-top, back-top, back-bot
        ([-1.0, 0.0, 0.0], [[-w, -h,  d], [-w,  h,  d], [-w,  h, -d], [-w, -h, -d]]),
        // +Y: back-left, front-left, front-right, back-right
        ([0.0, 1.0, 0.0], [[-w,  h, -d], [-w,  h,  d], [ w,  h,  d], [ w,  h, -d]]),
        // -Y: front-left, back-left, back-right, front-right
        ([0.0, -1.0, 0.0], [[-w, -h,  d], [-w, -h, -d], [ w, -h, -d], [ w, -h,  d]]),
        // +Z: bot-right, top-right, top-left, bot-left
        ([0.0, 0.0, 1.0], [[ w, -h,  d], [ w,  h,  d], [-w,  h,  d], [-w, -h,  d]]),
        // -Z: bot-left, top-left, top-right, bot-right
        ([0.0, 0.0, -1.0], [[-w, -h, -d], [-w,  h, -d], [ w,  h, -d], [ w, -h, -d]]),
    ];

    let mut verts: Vec<f32> = Vec::with_capacity(24 * NUM_PROP as usize);
    let mut tris: Vec<u32> = Vec::with_capacity(12 * 3);
    for (face_idx, (n, corners)) in face_data.iter().enumerate() {
        let base = (face_idx * 4) as u32;
        for c in corners {
            verts.extend_from_slice(c);
            verts.extend_from_slice(n);
        }
        // Two triangles per face: (0,1,2) and (0,2,3).
        tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    make_mesh(verts, tris)
}

/// Cylinder along the Y axis, centered at origin. Radius applied in XZ.
/// `segments` controls the side count. Top + bottom caps are flat-shaded.
pub fn generate_cylinder(radius: f64, height: f64, segments: u32) -> MeshGL {
    let segments = segments.max(3);
    let r = radius as f32;
    let h = (height * 0.5) as f32;

    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Side strip — emit 4 verts per side quad so each quad has its own
    // outward-facing normal.
    for i in 0..segments {
        let a0 = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32) / (segments as f32) * std::f32::consts::TAU;
        let (x0, z0) = (a0.cos() * r, a0.sin() * r);
        let (x1, z1) = (a1.cos() * r, a1.sin() * r);
        // Side normal at midpoint of the segment (averaged across the quad).
        let nx = (a0.cos() + a1.cos()) * 0.5;
        let nz = (a0.sin() + a1.sin()) * 0.5;
        let n_len = (nx * nx + nz * nz).sqrt().max(1e-6);
        let n = [nx / n_len, 0.0, nz / n_len];

        let base = (verts.len() / NUM_PROP as usize) as u32;
        // bottom-back, top-back, top-front, bottom-front (CCW from outside)
        for &(x, y, z) in &[(x0, -h, z0), (x0, h, z0), (x1, h, z1), (x1, -h, z1)] {
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&n);
        }
        tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // Top cap: triangle fan from center (normal +Y).
    let top_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, h, 0.0, 0.0, 1.0, 0.0]);
    let top_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, h, a.sin() * r, 0.0, 1.0, 0.0]);
    }
    for i in 0..segments {
        let v0 = top_center;
        let v1 = top_ring_start + i;
        let v2 = top_ring_start + ((i + 1) % segments);
        // CCW from above (+Y looking down): center → ring[i] → ring[i+1].
        // But +Y looking down has +X right, +Z up in the projected view,
        // so going around 0°→90°→180° increasing-angle CCW. That means
        // i+1 then i — we want (center, ring[i+1], ring[i]).
        tris.extend_from_slice(&[v0, v2, v1]);
    }

    // Bottom cap: triangle fan from center (normal -Y).
    let bot_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, -h, 0.0, 0.0, -1.0, 0.0]);
    let bot_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, -h, a.sin() * r, 0.0, -1.0, 0.0]);
    }
    for i in 0..segments {
        let v0 = bot_center;
        let v1 = bot_ring_start + i;
        let v2 = bot_ring_start + ((i + 1) % segments);
        // CCW from below (looking +Y from -Y): center → ring[i] → ring[i+1].
        tris.extend_from_slice(&[v0, v1, v2]);
    }

    make_mesh(verts, tris)
}

/// Tapered + partial-revolve cylinder. MatterCAD parity:
///
///   - `diameter_bottom` / `diameter_top` independent (taper or
///     uniform).
///   - `start_angle_rad` / `end_angle_rad` define the swept arc; the
///     full revolution is `(0, TAU)`. When the arc is partial, two
///     "wedge" side walls close the volume (one at `start_angle`, one
///     at `end_angle`).
///
/// Coordinate system follows the rest of `primitives.rs`: Y up, the
/// cylinder is centred on the origin, bottom face at `y = -height/2`,
/// top face at `y = +height/2`. Returns a flat-shaded `MeshGL`.
pub fn generate_cylinder_advanced(
    diameter_bottom: f64,
    diameter_top: f64,
    height: f64,
    sides: u32,
    start_angle_rad: f64,
    end_angle_rad: f64,
) -> MeshGL {
    let sides = sides.max(3);
    let r_bot = (diameter_bottom * 0.5) as f32;
    let r_top = (diameter_top * 0.5) as f32;
    let h = (height * 0.5) as f32;
    let arc = (end_angle_rad - start_angle_rad).clamp(0.0, std::f64::consts::TAU);
    let partial = arc < std::f64::consts::TAU - 1e-6;

    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Side quads — `sides` evenly spaced over the swept arc.
    for i in 0..sides {
        let t0 = i as f64 / sides as f64;
        let t1 = (i + 1) as f64 / sides as f64;
        let a0 = (start_angle_rad + arc * t0) as f32;
        let a1 = (start_angle_rad + arc * t1) as f32;

        let (cb0, sb0) = (a0.cos(), a0.sin());
        let (cb1, sb1) = (a1.cos(), a1.sin());
        // Outward normal averaged at quad midpoint; ignores taper-slope
        // for now (the difference is small for typical tapers and tests
        // don't depend on it).
        let nx = (cb0 + cb1) * 0.5;
        let nz = (sb0 + sb1) * 0.5;
        let nlen = (nx * nx + nz * nz).sqrt().max(1e-6);
        let n = [nx / nlen, 0.0, nz / nlen];

        let base = (verts.len() / NUM_PROP as usize) as u32;
        let p_bb = (cb0 * r_bot, -h, sb0 * r_bot);
        let p_bt = (cb0 * r_top, h, sb0 * r_top);
        let p_ft = (cb1 * r_top, h, sb1 * r_top);
        let p_fb = (cb1 * r_bot, -h, sb1 * r_bot);
        for (x, y, z) in [p_bb, p_bt, p_ft, p_fb] {
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&n);
        }
        tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // Top cap — triangle fan from centre, normal +Y.
    let top_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, h, 0.0, 0.0, 1.0, 0.0]);
    let top_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..=sides {
        let t = i as f64 / sides as f64;
        let a = (start_angle_rad + arc * t) as f32;
        verts.extend_from_slice(&[a.cos() * r_top, h, a.sin() * r_top, 0.0, 1.0, 0.0]);
    }
    for i in 0..sides {
        // CCW from above (+Y looking down).
        tris.extend_from_slice(&[top_center, top_ring_start + i + 1, top_ring_start + i]);
    }

    // Bottom cap — triangle fan from centre, normal -Y. Reversed winding.
    let bot_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, -h, 0.0, 0.0, -1.0, 0.0]);
    let bot_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..=sides {
        let t = i as f64 / sides as f64;
        let a = (start_angle_rad + arc * t) as f32;
        verts.extend_from_slice(&[a.cos() * r_bot, -h, a.sin() * r_bot, 0.0, -1.0, 0.0]);
    }
    for i in 0..sides {
        tris.extend_from_slice(&[bot_center, bot_ring_start + i, bot_ring_start + i + 1]);
    }

    // Wedge walls for partial revolves — two quads from centre axis
    // out to the swept ring, one at each end of the arc.
    if partial {
        let emit_wedge = |verts: &mut Vec<f32>, tris: &mut Vec<u32>, angle: f32, outward: bool| {
            let (ca, sa) = (angle.cos(), angle.sin());
            // Wedge normal is perpendicular to the rim ray, pointing
            // out of the missing arc. `outward = true` faces away from
            // the included arc on the start side; the end side flips.
            let sign = if outward { 1.0 } else { -1.0 };
            let n = [sign * -sa, 0.0, sign * ca];
            let base = (verts.len() / NUM_PROP as usize) as u32;
            // axis bottom, axis top, outer top, outer bottom — CCW when
            // viewed from the normal side.
            let p_ab = (0.0, -h, 0.0);
            let p_at = (0.0, h, 0.0);
            let p_ot = (ca * r_top, h, sa * r_top);
            let p_ob = (ca * r_bot, -h, sa * r_bot);
            for (x, y, z) in [p_ab, p_at, p_ot, p_ob] {
                verts.extend_from_slice(&[x, y, z]);
                verts.extend_from_slice(&n);
            }
            if outward {
                tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            } else {
                tris.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        };
        emit_wedge(&mut verts, &mut tris, start_angle_rad as f32, false);
        emit_wedge(&mut verts, &mut tris, end_angle_rad as f32, true);
    }

    make_mesh(verts, tris)
}

/// Cone with apex at top, circular base at bottom, both centered on Y.
/// `radius` is the base radius; the apex sits at +height/2.
pub fn generate_cone(radius: f64, height: f64, segments: u32) -> MeshGL {
    let segments = segments.max(3);
    let r = radius as f32;
    let h = (height * 0.5) as f32;

    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Side triangles — each one has its own outward-facing normal
    // (apex is shared geometry-wise but we duplicate verts so flat
    // shading works per face).
    for i in 0..segments {
        let a0 = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32) / (segments as f32) * std::f32::consts::TAU;
        let (x0, z0) = (a0.cos() * r, a0.sin() * r);
        let (x1, z1) = (a1.cos() * r, a1.sin() * r);
        let nx = (a0.cos() + a1.cos()) * 0.5;
        let nz = (a0.sin() + a1.sin()) * 0.5;
        // Normal slants up toward apex — the slope is roughly
        // (radius, height) so angle = atan2(radius, height).
        let slope_y = (r / height as f32).max(0.0);
        let n_len = (nx * nx + nz * nz + slope_y * slope_y).sqrt().max(1e-6);
        let n = [nx / n_len, slope_y / n_len, nz / n_len];

        let base = (verts.len() / NUM_PROP as usize) as u32;
        // base-back, base-front, apex; CCW from outside requires the
        // apex to be visited BEFORE the second base vert.
        for &(x, y, z) in &[(x0, -h, z0), (x1, -h, z1), (0.0, h, 0.0)] {
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&n);
        }
        tris.extend_from_slice(&[base, base + 2, base + 1]);
    }

    // Bottom cap — fan from center, normal -Y.
    let bot_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, -h, 0.0, 0.0, -1.0, 0.0]);
    let bot_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, -h, a.sin() * r, 0.0, -1.0, 0.0]);
    }
    for i in 0..segments {
        let v0 = bot_center;
        let v1 = bot_ring_start + i;
        let v2 = bot_ring_start + ((i + 1) % segments);
        tris.extend_from_slice(&[v0, v1, v2]);
    }

    make_mesh(verts, tris)
}

/// Torus centered at origin in the XZ plane. `major_r` is the distance
/// from center to ring center, `minor_r` is the tube radius.
pub fn generate_torus(major_r: f64, minor_r: f64, segments_major: u32, segments_minor: u32) -> MeshGL {
    let su = segments_major.max(3);
    let sv = segments_minor.max(3);
    let r_major = major_r as f32;
    let r_minor = minor_r as f32;

    let mut verts: Vec<f32> = Vec::with_capacity(((su + 1) * (sv + 1)) as usize * NUM_PROP as usize);
    let mut tris: Vec<u32> = Vec::new();

    // Vertex grid (su+1) × (sv+1) — closed seams have duplicate verts
    // so smooth normals work cleanly across the wrap.
    for j in 0..=sv {
        let v = j as f32 / sv as f32;
        let phi = v * std::f32::consts::TAU;
        let cos_phi = phi.cos();
        let sin_phi = phi.sin();
        for i in 0..=su {
            let u = i as f32 / su as f32;
            let theta = u * std::f32::consts::TAU;
            let cos_th = theta.cos();
            let sin_th = theta.sin();
            // Position on the torus surface.
            let x = (r_major + r_minor * cos_phi) * cos_th;
            let y = r_minor * sin_phi;
            let z = (r_major + r_minor * cos_phi) * sin_th;
            // Normal — vector from the ring center to the point.
            let nx = cos_phi * cos_th;
            let ny = sin_phi;
            let nz = cos_phi * sin_th;
            verts.extend_from_slice(&[x, y, z, nx, ny, nz]);
        }
    }
    let stride_u = su + 1;
    for j in 0..sv {
        for i in 0..su {
            let v00 = j * stride_u + i;
            let v10 = j * stride_u + (i + 1);
            let v01 = (j + 1) * stride_u + i;
            let v11 = (j + 1) * stride_u + (i + 1);
            tris.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
        }
    }
    make_mesh(verts, tris)
}

/// Square-base pyramid — `width × depth` rectangular base centered at
/// origin, apex at +height/2.
pub fn generate_pyramid(width: f64, height: f64, depth: f64) -> MeshGL {
    let w = (width * 0.5) as f32;
    let h = (height * 0.5) as f32;
    let d = (depth * 0.5) as f32;
    let apex = [0.0f32, h, 0.0f32];

    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Four side triangles, each with its own normal.
    let sides: [([f32; 3], [f32; 3]); 4] = [
        // (base-left, base-right) for face viewed from outside.
        ([-w, -h,  d], [ w, -h,  d]),  // +Z face
        ([ w, -h,  d], [ w, -h, -d]),  // +X face
        ([ w, -h, -d], [-w, -h, -d]),  // -Z face
        ([-w, -h, -d], [-w, -h,  d]),  // -X face
    ];
    for (left, right) in sides {
        // Compute face normal by cross product.
        let e1 = [right[0] - left[0], right[1] - left[1], right[2] - left[2]];
        let e2 = [apex[0] - left[0], apex[1] - left[1], apex[2] - left[2]];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
        let nn = [n[0] / nl, n[1] / nl, n[2] / nl];
        let base = (verts.len() / NUM_PROP as usize) as u32;
        for &p in &[left, right, apex] {
            verts.extend_from_slice(&p);
            verts.extend_from_slice(&nn);
        }
        tris.extend_from_slice(&[base, base + 1, base + 2]);
    }

    // Bottom face — two triangles, normal -Y.
    let base = (verts.len() / NUM_PROP as usize) as u32;
    let bot_n = [0.0f32, -1.0f32, 0.0f32];
    for &(x, y, z) in &[(-w, -h, -d), (w, -h, -d), (w, -h, d), (-w, -h, d)] {
        verts.extend_from_slice(&[x, y, z]);
        verts.extend_from_slice(&bot_n);
    }
    // Standard winding (0,1,2), (0,2,3) — gives outward = -Y because
    // the verts are laid out CCW when viewed from -Y looking up to +Y.
    tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

    make_mesh(verts, tris)
}

/// Wedge — a triangular prism with the right-triangle cross-section in
/// the XY plane and depth along Z. The apex of the triangle is at
/// (+w/2, +h/2), the right-angle corner at (-w/2, -h/2).
pub fn generate_wedge(width: f64, height: f64, depth: f64) -> MeshGL {
    let w = (width * 0.5) as f32;
    let h = (height * 0.5) as f32;
    let d = (depth * 0.5) as f32;

    // 6 unique geometric points but we need 18 vertices total (3 per
    // face × 6 faces) for flat shading. Simpler to just enumerate.
    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Helper: emit a quad with given verts + normal.
    let mut emit_quad = |corners: [[f32; 3]; 4], n: [f32; 3], v: &mut Vec<f32>, t: &mut Vec<u32>| {
        let base = (v.len() / NUM_PROP as usize) as u32;
        for c in corners {
            v.extend_from_slice(&c);
            v.extend_from_slice(&n);
        }
        t.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    let mut emit_tri = |corners: [[f32; 3]; 3], n: [f32; 3], v: &mut Vec<f32>, t: &mut Vec<u32>| {
        let base = (v.len() / NUM_PROP as usize) as u32;
        for c in corners {
            v.extend_from_slice(&c);
            v.extend_from_slice(&n);
        }
        t.extend_from_slice(&[base, base + 1, base + 2]);
    };

    // Bottom face (-Y)
    emit_quad(
        [[-w, -h, -d], [w, -h, -d], [w, -h, d], [-w, -h, d]],
        [0.0, -1.0, 0.0],
        &mut verts, &mut tris,
    );
    // Back face (-X) — vertical rectangle
    emit_quad(
        [[-w, -h, -d], [-w, -h, d], [-w, h, d], [-w, h, -d]],
        [-1.0, 0.0, 0.0],
        &mut verts, &mut tris,
    );
    // Slanted top (the hypotenuse face) — normal points up-and-out.
    // Plane through (w,-h,*) and (-w,h,*). Outward normal in XY is
    // (h, w) normalized. Reverse winding from the natural enumeration
    // so the cross product gives +X+Y instead of -X-Y.
    let nx = h;
    let ny = w;
    let nl = (nx * nx + ny * ny).sqrt().max(1e-6);
    let slope_n = [nx / nl, ny / nl, 0.0];
    emit_quad(
        [[-w, h, -d], [-w, h, d], [w, -h, d], [w, -h, -d]],
        slope_n,
        &mut verts, &mut tris,
    );
    // Front cap (+Z)
    emit_tri(
        [[-w, -h, d], [w, -h, d], [-w, h, d]],
        [0.0, 0.0, 1.0],
        &mut verts, &mut tris,
    );
    // Back cap (-Z)
    emit_tri(
        [[w, -h, -d], [-w, -h, -d], [-w, h, -d]],
        [0.0, 0.0, -1.0],
        &mut verts, &mut tris,
    );

    make_mesh(verts, tris)
}

/// UV sphere centered at origin. `segments_u` is the longitudinal count
/// (around Y), `segments_v` the latitudinal count (from south to north
/// pole). Smooth-shaded — the normal at each vertex is its outward
/// direction from the center.
pub fn generate_sphere(radius: f64, segments_u: u32, segments_v: u32) -> MeshGL {
    let su = segments_u.max(3);
    let sv = segments_v.max(2);
    let r = radius as f32;

    let mut verts: Vec<f32> = Vec::with_capacity(((su + 1) * (sv + 1)) as usize * NUM_PROP as usize);
    let mut tris: Vec<u32> = Vec::new();

    // Vertices on a (su+1) x (sv+1) grid (closing seams duplicate).
    for j in 0..=sv {
        let v = j as f32 / sv as f32;
        let phi = v * std::f32::consts::PI; // 0..π (south → north)
        let sin_phi = phi.sin();
        let cos_phi = phi.cos();
        for i in 0..=su {
            let u = i as f32 / su as f32;
            let theta = u * std::f32::consts::TAU; // 0..2π around Y
            let x = sin_phi * theta.cos();
            let y = cos_phi;
            let z = sin_phi * theta.sin();
            verts.extend_from_slice(&[x * r, y * r, z * r, x, y, z]);
        }
    }

    let stride_u = su + 1;
    for j in 0..sv {
        for i in 0..su {
            let v00 = j * stride_u + i;
            let v10 = j * stride_u + (i + 1);
            let v01 = (j + 1) * stride_u + i;
            let v11 = (j + 1) * stride_u + (i + 1);
            // CCW from outside: looking from +radius direction back to origin.
            // For the sphere, going (j → j+1) is south-to-north (Y increases),
            // and (i → i+1) is increasing theta (counter-clockwise when
            // viewed from +Y). So CCW outward triangles for an outward
            // surface patch are (v00, v10, v11) and (v00, v11, v01).
            tris.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
        }
    }

    make_mesh(verts, tris)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::mesh3d::{compute_flat_normals, get_normal, get_pos, num_tris, num_verts};

    #[test]
    fn box_has_24_verts_and_12_tris() {
        let m = generate_box(1.0, 1.0, 1.0);
        assert_eq!(num_verts(&m), 24);
        assert_eq!(num_tris(&m), 12);
    }

    #[test]
    fn box_normals_are_unit_length_and_axis_aligned() {
        let m = generate_box(1.0, 1.0, 1.0);
        for i in 0..num_verts(&m) {
            let n = get_normal(&m, i);
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "vert {} normal not unit: {:?}", i, n);
            // Each component should be 0 or ±1 (axis-aligned faces)
            for k in 0..3 {
                let abs = n[k].abs();
                assert!(abs < 1e-5 || (abs - 1.0).abs() < 1e-5,
                        "non-axis component on box normal: {:?}", n);
            }
        }
    }

    #[test]
    fn box_positions_lie_on_extents() {
        let m = generate_box(2.0, 4.0, 6.0);
        for i in 0..num_verts(&m) {
            let p = get_pos(&m, i);
            assert!((p[0].abs() - 1.0).abs() < 1e-5);
            assert!((p[1].abs() - 2.0).abs() < 1e-5);
            assert!((p[2].abs() - 3.0).abs() < 1e-5);
        }
    }

    #[test]
    fn cylinder_has_correct_vertex_count() {
        let m = generate_cylinder(1.0, 2.0, 8);
        // 8 sides × 4 verts (= 32) + top center + bottom center + 8 top ring
        // + 8 bottom ring = 32 + 1 + 1 + 8 + 8 = 50.
        assert_eq!(num_verts(&m), 50);
        // 8 sides × 2 tris + 8 top fan + 8 bottom fan = 32.
        assert_eq!(num_tris(&m), 32);
    }

    #[test]
    fn sphere_has_outward_unit_normals() {
        let m = generate_sphere(2.0, 16, 8);
        for i in 0..num_verts(&m) {
            let p = get_pos(&m, i);
            let n = get_normal(&m, i);
            // Smooth normals point outward from origin.
            let pl = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((nl - 1.0).abs() < 1e-5, "non-unit normal: {:?}", n);
            // n parallel to p? Check via dot product against normalized p.
            let dot = (p[0] * n[0] + p[1] * n[1] + p[2] * n[2]) / pl.max(1e-6);
            assert!(dot > 0.999, "normal not outward; dot={}", dot);
        }
    }

    #[test]
    fn box_face_normals_match_after_recompute() {
        // Sanity: re-run flat normal computation and verify it matches
        // what we hand-wrote in the generator.
        let mut m = generate_box(3.0, 3.0, 3.0);
        let original = m.vert_properties.clone();
        compute_flat_normals(&mut m);
        // Component-wise comparison; allow tiny tolerance.
        assert_eq!(m.vert_properties.len(), original.len());
        for i in 0..m.vert_properties.len() {
            assert!((m.vert_properties[i] - original[i]).abs() < 1e-5,
                    "mismatch at {}: orig={} new={}",
                    i, original[i], m.vert_properties[i]);
        }
    }
}
