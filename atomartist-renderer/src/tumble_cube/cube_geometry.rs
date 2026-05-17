//! Hand-built cube mesh for the tumble cube widget — 24 vertices (4 per
//! face) so each face carries its own UV `[0, 1]` square and an outward
//! normal, plus 36 indices forming 12 triangles.
//!
//! Face ordering matches MatterCAD's `TextureFace(0..10 step 2, ...)` in
//! `TumbleCubeControl` so the hit-test table (which uses indices 0..6)
//! and the texture upload list line up.  The face enum below is the
//! single source of truth.
//!
//! Coordinate convention: cube lives in `[-1, +1]^3` around the origin
//! (MatterCAD uses `[-2, 2]` from `CreateCube(4, 4, 4)`; we keep the
//! semantics with a smaller, normalised size since the widget renders
//! in its own miniature scene).

/// Logical face indices.  Match the order of MatterCAD's
/// `connections.Add(new ConnectedFaces(...))` calls in
/// `TumbleCubeControl` so the hit-test logic ports verbatim.
///
/// (See `TumbleCubeControl.cs` lines 182–189.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Face {
    /// `+Z`  — labelled "Top" in MatterCAD's coordinate system. Because
    /// AtomArtist is Y-up while MatterCAD is Z-up, the labels are
    /// reinterpreted by the orient mapping — the geometry / hit-test
    /// table here matches MatterCAD verbatim so the ported algorithm
    /// stays correct.
    Top = 0,
    /// `+X` — labelled "Left".
    Left = 1,
    /// `-X` — labelled "Right".
    Right = 2,
    /// `-Z` — labelled "Bottom".
    Bottom = 3,
    /// `+Y` — labelled "Back".
    Back = 4,
    /// `-Y` — labelled "Front".
    Front = 5,
}

impl Face {
    /// Iteration order matching the `connections` table.  Index `i` of
    /// the returned slice corresponds to the entry built by
    /// `connections.Add(...)` at call-site `i`.
    pub const ALL: [Face; 6] = [
        Face::Top,
        Face::Left,
        Face::Right,
        Face::Bottom,
        Face::Back,
        Face::Front,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Face::Top => "Top",
            Face::Bottom => "Bottom",
            Face::Left => "Left",
            Face::Right => "Right",
            Face::Front => "Front",
            Face::Back => "Back",
        }
    }

    /// World-space outward normal of the face on the unit cube.
    pub fn normal(self) -> [f32; 3] {
        match self {
            Face::Top => [0.0, 0.0, 1.0],
            Face::Bottom => [0.0, 0.0, -1.0],
            Face::Left => [1.0, 0.0, 0.0],
            Face::Right => [-1.0, 0.0, 0.0],
            Face::Back => [0.0, 1.0, 0.0],
            Face::Front => [0.0, -1.0, 0.0],
        }
    }
}

/// A single cube vertex pushed to the GPU.  Position + normal + UV
/// matches the WGSL `Vertex` definition in [`super::renderer`].
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CubeVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

/// Build the 24 vertices / 36 indices.  Vertex layout: each face owns
/// four consecutive vertices in the order
/// `[bottom-left, bottom-right, top-right, top-left]` of the face's UV
/// square, then two triangles `[0, 1, 2]` and `[0, 2, 3]`.
pub fn build_cube() -> (Vec<CubeVertex>, Vec<u32>) {
    let mut verts: Vec<CubeVertex> = Vec::with_capacity(24);
    let mut idx: Vec<u32> = Vec::with_capacity(36);

    // Face vertex tables — four corners in world space for each face,
    // counter-clockwise when viewed from outside the cube so the normal
    // matches `Face::normal`. The order matches Face::ALL.
    let faces_corners: [(Face, [[f32; 3]; 4]); 6] = [
        // Top (+Z)
        (
            Face::Top,
            [
                [-1.0, -1.0, 1.0],
                [1.0, -1.0, 1.0],
                [1.0, 1.0, 1.0],
                [-1.0, 1.0, 1.0],
            ],
        ),
        // Left (+X)
        (
            Face::Left,
            [
                [1.0, -1.0, -1.0],
                [1.0, 1.0, -1.0],
                [1.0, 1.0, 1.0],
                [1.0, -1.0, 1.0],
            ],
        ),
        // Right (-X)
        (
            Face::Right,
            [
                [-1.0, 1.0, -1.0],
                [-1.0, -1.0, -1.0],
                [-1.0, -1.0, 1.0],
                [-1.0, 1.0, 1.0],
            ],
        ),
        // Bottom (-Z)
        (
            Face::Bottom,
            [
                [-1.0, 1.0, -1.0],
                [1.0, 1.0, -1.0],
                [1.0, -1.0, -1.0],
                [-1.0, -1.0, -1.0],
            ],
        ),
        // Back (+Y)
        (
            Face::Back,
            [
                [1.0, 1.0, -1.0],
                [-1.0, 1.0, -1.0],
                [-1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
            ],
        ),
        // Front (-Y)
        (
            Face::Front,
            [
                [-1.0, -1.0, -1.0],
                [1.0, -1.0, -1.0],
                [1.0, -1.0, 1.0],
                [-1.0, -1.0, 1.0],
            ],
        ),
    ];

    // UV order — `(0,0)` at bottom-left, `(1,1)` at top-right, matching
    // the face_textures painter which writes labels with the origin at
    // the bottom-left.
    let uvs: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

    for (face, corners) in faces_corners.iter() {
        let n = face.normal();
        let base = verts.len() as u32;
        for k in 0..4 {
            verts.push(CubeVertex {
                pos: corners[k],
                normal: n,
                uv: uvs[k],
            });
        }
        // Two triangles: 0-1-2, 0-2-3.
        idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    (verts, idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_has_24_verts_and_36_indices() {
        let (v, i) = build_cube();
        assert_eq!(v.len(), 24);
        assert_eq!(i.len(), 36);
    }

    #[test]
    fn each_face_normal_points_outward() {
        let (v, _) = build_cube();
        // Pick one vertex per face; its position projected onto the
        // normal should be positive (we're at +1 on the face's axis).
        for f in Face::ALL.iter() {
            let face_idx = *f as usize;
            let v0 = &v[face_idx * 4];
            let dot = v0.pos[0] * v0.normal[0]
                + v0.pos[1] * v0.normal[1]
                + v0.pos[2] * v0.normal[2];
            assert!(dot > 0.0, "face {:?} normal points inward", f);
        }
    }
}
