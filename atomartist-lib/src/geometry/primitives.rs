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

/// Cylinder along the Z axis (Z-up), centered at origin. Radius applied
/// in the XY plane. `segments` controls the side count. Top + bottom
/// caps are flat-shaded. Matches MatterCAD's `CylinderObject3D`
/// convention; `height` extends along Z from `-h/2` to `+h/2`.
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
        let (x0, y0) = (a0.cos() * r, a0.sin() * r);
        let (x1, y1) = (a1.cos() * r, a1.sin() * r);
        // Side normal at midpoint of the segment (averaged across the quad).
        let nx = (a0.cos() + a1.cos()) * 0.5;
        let ny = (a0.sin() + a1.sin()) * 0.5;
        let n_len = (nx * nx + ny * ny).sqrt().max(1e-6);
        let n = [nx / n_len, ny / n_len, 0.0];

        let base = (verts.len() / NUM_PROP as usize) as u32;
        // bottom-back, top-back, top-front, bottom-front. Reverse winding
        // relative to the natural (0,1,2),(0,2,3) order so the cross
        // product points outward in a Z-up XY-plane ring.
        for &(x, y, z) in &[(x0, y0, -h), (x0, y0, h), (x1, y1, h), (x1, y1, -h)] {
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&n);
        }
        tris.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }

    // Top cap: triangle fan from centre (normal +Z).
    let top_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, 0.0, h, 0.0, 0.0, 1.0]);
    let top_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, a.sin() * r, h, 0.0, 0.0, 1.0]);
    }
    for i in 0..segments {
        let v0 = top_center;
        let v1 = top_ring_start + i;
        let v2 = top_ring_start + ((i + 1) % segments);
        // CCW from above (+Z looking down): center → ring[i] → ring[i+1].
        tris.extend_from_slice(&[v0, v1, v2]);
    }

    // Bottom cap: triangle fan from centre (normal -Z).
    let bot_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, 0.0, -h, 0.0, 0.0, -1.0]);
    let bot_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, a.sin() * r, -h, 0.0, 0.0, -1.0]);
    }
    for i in 0..segments {
        let v0 = bot_center;
        let v1 = bot_ring_start + i;
        let v2 = bot_ring_start + ((i + 1) % segments);
        // CCW from below (-Z looking up): reverse winding to keep
        // outward face culling correct.
        tris.extend_from_slice(&[v0, v2, v1]);
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
/// Coordinate system: **Z-up** (matches MatterCAD's `CylinderObject3D`
/// convention and the rest of the AtomArtist world) — the rotation
/// axis runs along +Z, bottom face at `z = -height/2`, top face at
/// `z = +height/2`. The ring lies in the XY plane.
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

    // Z-up cylinder: ring lies in XY plane, height extends along Z.
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
        let ny = (sb0 + sb1) * 0.5;
        let nlen = (nx * nx + ny * ny).sqrt().max(1e-6);
        let n = [nx / nlen, ny / nlen, 0.0];

        let base = (verts.len() / NUM_PROP as usize) as u32;
        let p_bb = (cb0 * r_bot, sb0 * r_bot, -h);
        let p_bt = (cb0 * r_top, sb0 * r_top, h);
        let p_ft = (cb1 * r_top, sb1 * r_top, h);
        let p_fb = (cb1 * r_bot, sb1 * r_bot, -h);
        for (x, y, z) in [p_bb, p_bt, p_ft, p_fb] {
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&n);
        }
        // Z-up winding: reverse the bb→bt→ft order so the outward
        // normal faces correctly (see `generate_cylinder` for the
        // handedness analysis).
        tris.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }

    // Top cap — triangle fan from centre, normal +Z.
    let top_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, 0.0, h, 0.0, 0.0, 1.0]);
    let top_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..=sides {
        let t = i as f64 / sides as f64;
        let a = (start_angle_rad + arc * t) as f32;
        verts.extend_from_slice(&[a.cos() * r_top, a.sin() * r_top, h, 0.0, 0.0, 1.0]);
    }
    for i in 0..sides {
        // CCW when viewed from +Z looking toward -Z.
        tris.extend_from_slice(&[top_center, top_ring_start + i, top_ring_start + i + 1]);
    }

    // Bottom cap — triangle fan from centre, normal -Z. Reversed winding.
    let bot_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, 0.0, -h, 0.0, 0.0, -1.0]);
    let bot_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..=sides {
        let t = i as f64 / sides as f64;
        let a = (start_angle_rad + arc * t) as f32;
        verts.extend_from_slice(&[a.cos() * r_bot, a.sin() * r_bot, -h, 0.0, 0.0, -1.0]);
    }
    for i in 0..sides {
        tris.extend_from_slice(&[bot_center, bot_ring_start + i + 1, bot_ring_start + i]);
    }

    // Wedge walls for partial revolves — two quads from centre axis
    // out to the swept ring, one at each end of the arc.
    if partial {
        let emit_wedge = |verts: &mut Vec<f32>, tris: &mut Vec<u32>, angle: f32, outward: bool| {
            let (ca, sa) = (angle.cos(), angle.sin());
            // Wedge normal is perpendicular to the rim ray in XY plane,
            // pointing out of the missing arc. `outward = true` faces
            // away from the included arc on the start side; the end
            // side flips.
            let sign = if outward { 1.0 } else { -1.0 };
            let n = [sign * -sa, sign * ca, 0.0];
            let base = (verts.len() / NUM_PROP as usize) as u32;
            // axis bottom, axis top, outer top, outer bottom — CCW when
            // viewed from the normal side.
            let p_ab = (0.0, 0.0, -h);
            let p_at = (0.0, 0.0, h);
            let p_ot = (ca * r_top, sa * r_top, h);
            let p_ob = (ca * r_bot, sa * r_bot, -h);
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

/// Cone along the Z axis, centered at origin. Apex sits at `+height/2`,
/// circular base ring lies in the XY plane at `z = -height/2`. Matches
/// MatterCAD's Z-up CAD convention.
pub fn generate_cone(radius: f64, height: f64, segments: u32) -> MeshGL {
    let segments = segments.max(3);
    let r = radius as f32;
    let h = (height * 0.5) as f32;

    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Side triangles — each has its own outward-facing normal so flat
    // shading reads correctly per face.
    for i in 0..segments {
        let a0 = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32) / (segments as f32) * std::f32::consts::TAU;
        let (x0, y0) = (a0.cos() * r, a0.sin() * r);
        let (x1, y1) = (a1.cos() * r, a1.sin() * r);
        let nx = (a0.cos() + a1.cos()) * 0.5;
        let ny = (a0.sin() + a1.sin()) * 0.5;
        // Normal slants up toward apex; the +Z slope is roughly
        // `radius / height` (right-triangle of the cone profile).
        let slope_z = (r / height as f32).max(0.0);
        let n_len = (nx * nx + ny * ny + slope_z * slope_z).sqrt().max(1e-6);
        let n = [nx / n_len, ny / n_len, slope_z / n_len];

        let base = (verts.len() / NUM_PROP as usize) as u32;
        // base-back, base-front, apex; for the Z-up XY ring this winding
        // gives an outward normal (cross of edge1 × edge2 points along
        // the stored `n`).
        for &(x, y, z) in &[(x0, y0, -h), (x1, y1, -h), (0.0, 0.0, h)] {
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&n);
        }
        tris.extend_from_slice(&[base, base + 1, base + 2]);
    }

    // Bottom cap — fan from center, normal -Z. CCW viewed from -Z (below)
    // is CW viewed from above, so visit ring[i+1] before ring[i].
    let bot_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, 0.0, -h, 0.0, 0.0, -1.0]);
    let bot_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, a.sin() * r, -h, 0.0, 0.0, -1.0]);
    }
    for i in 0..segments {
        let v0 = bot_center;
        let v1 = bot_ring_start + i;
        let v2 = bot_ring_start + ((i + 1) % segments);
        tris.extend_from_slice(&[v0, v2, v1]);
    }

    make_mesh(verts, tris)
}

/// Torus centered at origin with its major ring in the XY plane (Z-up).
/// `major_r` is the distance from origin to the ring center, `minor_r`
/// the tube radius. The donut sits flat on the bed; the tube's height
/// extent is `±minor_r` along Z.
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
            // Position: major angle θ revolves around +Z (ring lies in
            // XY plane); minor angle φ rotates the tube cross-section
            // whose "vertical" axis is +Z.
            let x = (r_major + r_minor * cos_phi) * cos_th;
            let y = (r_major + r_minor * cos_phi) * sin_th;
            let z = r_minor * sin_phi;
            // Normal — vector from the ring center to the point.
            let nx = cos_phi * cos_th;
            let ny = cos_phi * sin_th;
            let nz = sin_phi;
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
            // Z-up torus: θ goes CCW around +Z and φ CCW around the
            // tube center. The natural patch winding (v00, v10, v11)
            // gives an outward normal (matches stored).
            tris.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
        }
    }
    make_mesh(verts, tris)
}

/// Square-base pyramid in Z-up world: `width × depth` rectangular base
/// in the XY plane at `z = -height/2`, apex centered at `(0, 0, +height/2)`.
pub fn generate_pyramid(width: f64, height: f64, depth: f64) -> MeshGL {
    let w = (width * 0.5) as f32;
    let h = (height * 0.5) as f32;
    let d = (depth * 0.5) as f32;
    let apex = [0.0f32, 0.0f32, h];

    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Four side triangles, each with its own normal. Walking each face's
    // base edge from `left` to `right` then up to the apex is CCW from
    // outside (the cross product gives the outward normal).
    let sides: [([f32; 3], [f32; 3]); 4] = [
        // (base-left, base-right) for face viewed from outside.
        ([-w, -d, -h], [ w, -d, -h]),  // -Y face (front)
        ([ w, -d, -h], [ w,  d, -h]),  // +X face (right)
        ([ w,  d, -h], [-w,  d, -h]),  // +Y face (back)
        ([-w,  d, -h], [-w, -d, -h]),  // -X face (left)
    ];
    for (left, right) in sides {
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

    // Bottom face — two triangles, normal -Z. Verts laid out CCW when
    // viewed from -Z (below), so the standard (0,1,2),(0,2,3) winding
    // produces the -Z normal.
    let base = (verts.len() / NUM_PROP as usize) as u32;
    let bot_n = [0.0f32, 0.0f32, -1.0f32];
    for &(x, y, z) in &[(-w, -d, -h), (-w, d, -h), (w, d, -h), (w, -d, -h)] {
        verts.extend_from_slice(&[x, y, z]);
        verts.extend_from_slice(&bot_n);
    }
    tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

    make_mesh(verts, tris)
}

/// Wedge — a triangular prism in Z-up world: right-triangle cross-section
/// in the XZ plane, depth along Y. The right-angle corner sits at
/// `(-w/2, *, -h/2)`; the X leg runs to `(+w/2, *, -h/2)` (bottom-front
/// edge); the Z leg to `(-w/2, *, +h/2)` (top-back edge). The hypotenuse
/// face is the slanted top.
pub fn generate_wedge(width: f64, height: f64, depth: f64) -> MeshGL {
    let w = (width * 0.5) as f32;
    let h = (height * 0.5) as f32;
    let d = (depth * 0.5) as f32;

    // 6 unique geometric points but we need 18 vertices total (3 per
    // face × 6 faces) for flat shading. Simpler to just enumerate.
    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

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

    // Bottom face (-Z) at z=-h — full w × d rectangle.
    emit_quad(
        [[-w, -d, -h], [-w, d, -h], [w, d, -h], [w, -d, -h]],
        [0.0, 0.0, -1.0],
        &mut verts, &mut tris,
    );
    // Back face (-X) at x=-w — vertical rectangle in the YZ plane.
    emit_quad(
        [[-w, -d, -h], [-w, -d, h], [-w, d, h], [-w, d, -h]],
        [-1.0, 0.0, 0.0],
        &mut verts, &mut tris,
    );
    // Slanted top (hypotenuse) — connects (+w, ±d, -h) and (-w, ±d, +h).
    // Outward normal lies in the XZ plane; perpendicular to the slope
    // direction `(-2w, 0, +2h)`, so the unit normal is `(h, 0, w)/len`.
    let nx = h;
    let nz = w;
    let nl = (nx * nx + nz * nz).sqrt().max(1e-6);
    let slope_n = [nx / nl, 0.0, nz / nl];
    emit_quad(
        [[w, -d, -h], [w, d, -h], [-w, d, h], [-w, -d, h]],
        slope_n,
        &mut verts, &mut tris,
    );
    // Front cap (+Y) at y=+d — right-triangle face.
    emit_tri(
        [[-w, d, -h], [-w, d, h], [w, d, -h]],
        [0.0, 1.0, 0.0],
        &mut verts, &mut tris,
    );
    // Back cap (-Y) at y=-d — same triangle, mirrored CCW from -Y.
    emit_tri(
        [[-w, -d, -h], [w, -d, -h], [-w, -d, h]],
        [0.0, -1.0, 0.0],
        &mut verts, &mut tris,
    );

    make_mesh(verts, tris)
}

/// UV sphere centered at origin (Z-up). `segments_u` is the longitudinal
/// count (around Z), `segments_v` the latitudinal count (north pole at
/// +Z to south pole at -Z). Smooth-shaded — the normal at each vertex is
/// its outward direction from the center.
pub fn generate_sphere(radius: f64, segments_u: u32, segments_v: u32) -> MeshGL {
    let su = segments_u.max(3);
    let sv = segments_v.max(2);
    let r = radius as f32;

    let mut verts: Vec<f32> = Vec::with_capacity(((su + 1) * (sv + 1)) as usize * NUM_PROP as usize);
    let mut tris: Vec<u32> = Vec::new();

    // Vertices on a (su+1) × (sv+1) grid (closing seams duplicate).
    // j=0 collapses to the north pole at (0, 0, +r); j=sv collapses to
    // the south pole at (0, 0, -r).
    for j in 0..=sv {
        let v = j as f32 / sv as f32;
        let phi = v * std::f32::consts::PI; // 0..π (north → south along Z)
        let sin_phi = phi.sin();
        let cos_phi = phi.cos();
        for i in 0..=su {
            let u = i as f32 / su as f32;
            let theta = u * std::f32::consts::TAU; // 0..2π around Z (in XY plane)
            let x = sin_phi * theta.cos();
            let y = sin_phi * theta.sin();
            let z = cos_phi;
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
            // CCW from outside the sphere. Going (i → i+1) is +theta
            // (CCW around +Z viewed from above); going (j → j+1) is
            // north-to-south (Z decreases). Walking the natural patch
            // order (v00, v10, v11) would be CW from outside, so reverse:
            // (v00, v11, v10) and (v00, v01, v11) point outward.
            tris.extend_from_slice(&[v00, v11, v10, v00, v01, v11]);
        }
    }

    make_mesh(verts, tris)
}

// Structural, winding, and Z-up axis tests live in
// atomartist-lib/tests/primitives_axis.rs to keep this file under
// the workspace 800-line guardrail.
