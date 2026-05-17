//! Hand-built cube mesh for the tumble cube widget — 24 vertices (4 per
//! face) so each face carries its own UV `[0, 1]` square and an outward
//! normal, plus 36 indices forming 12 triangles.
//!
//! Face indices match the order used by [`super::hit_test`] so the
//! ported MatterCAD `ConnectedFaces` table lines up.
//!
//! Coordinate convention: cube lives in `[-1, +1]^3` around the origin
//! in AtomArtist's **Y-up** world (MatterCAD itself is Z-up; we
//! reinterpret the face labels into Y-up here so the cube's orientation
//! matches the main viewport's `OrbitCamera`).
//!
//! Label / world mapping:
//!
//! | Label  | Outward normal | Camera that "shows" this face |
//! |--------|---------------|-------------------------------|
//! | Top    | (0, +1, 0)    | camera above (+Y)             |
//! | Bottom | (0, -1, 0)    | camera below (-Y)             |
//! | Front  | (0, 0, +1)    | camera in front (+Z)          |
//! | Back   | (0, 0, -1)    | camera behind (-Z)            |
//! | Right  | (+1, 0, 0)    | camera to the right (+X)      |
//! | Left   | (-1, 0, 0)    | camera to the left (-X)       |
//!
//! Per-face vertex order is strictly `[BL, BR, TR, TL]` from the
//! *external viewer*'s perspective (viewer outside the cube looking
//! at the face from along its outward normal). UVs `[(0,1), (1,1),
//! (1,0), (0,0)]` map BL → texture-bottom-left etc., so a top-down
//! stored label appears right-side-up on the face.
//!
//! Each face's "up" direction for the viewer:
//! - Top / Bottom: `+/-Z` (since +Y is the face normal, we need a
//!   horizontal world axis as label-up — `-Z` for Top so "top of label"
//!   points toward Back; `+Z` for Bottom).
//! - All four side faces (Front / Back / Left / Right): `+Y` (label
//!   reads upright when the cube is at default view).

/// Logical face indices.  Must match the order of the `connections`
/// table in [`super::hit_test`] so face-index ↔ ConnectedFaces lookup
/// works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Face {
    Top = 0,
    Left = 1,
    Right = 2,
    Bottom = 3,
    Back = 4,
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

    /// World-space outward normal of the face on the unit cube (Y-up
    /// world).  Matches the table at the top of the module.
    pub fn normal(self) -> [f32; 3] {
        match self {
            Face::Top => [0.0, 1.0, 0.0],
            Face::Bottom => [0.0, -1.0, 0.0],
            Face::Front => [0.0, 0.0, 1.0],
            Face::Back => [0.0, 0.0, -1.0],
            Face::Right => [1.0, 0.0, 0.0],
            Face::Left => [-1.0, 0.0, 0.0],
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

/// Build the 24 vertices / 36 indices.  Vertex layout per face:
/// `[BL, BR, TR, TL]` in the external viewer's frame, two triangles
/// `[0, 1, 2]` and `[0, 2, 3]` (CCW when viewed from outside, so
/// back-face culling keeps the outside surface visible).
pub fn build_cube() -> (Vec<CubeVertex>, Vec<u32>) {
    let mut verts: Vec<CubeVertex> = Vec::with_capacity(24);
    let mut idx: Vec<u32> = Vec::with_capacity(36);

    // Corners derived in the module-level header.  Each row is the
    // exact `[BL, BR, TR, TL]` from the face's external viewer.
    let faces_corners: [(Face, [[f32; 3]; 4]); 6] = [
        // Top (+Y); viewer above looking down; label-up = -Z.
        (
            Face::Top,
            [
                [-1.0, 1.0, 1.0],   // BL: -X, +Z (viewer's bottom-left)
                [1.0, 1.0, 1.0],    // BR
                [1.0, 1.0, -1.0],   // TR
                [-1.0, 1.0, -1.0],  // TL
            ],
        ),
        // Left (-X); viewer at -X looking toward +X; label-up = +Y.
        (
            Face::Left,
            [
                [-1.0, -1.0, -1.0], // BL
                [-1.0, -1.0, 1.0],  // BR
                [-1.0, 1.0, 1.0],   // TR
                [-1.0, 1.0, -1.0],  // TL
            ],
        ),
        // Right (+X); viewer at +X looking toward -X; label-up = +Y.
        (
            Face::Right,
            [
                [1.0, -1.0, 1.0],   // BL
                [1.0, -1.0, -1.0],  // BR
                [1.0, 1.0, -1.0],   // TR
                [1.0, 1.0, 1.0],    // TL
            ],
        ),
        // Bottom (-Y); viewer below looking up; label-up = +Z.
        (
            Face::Bottom,
            [
                [-1.0, -1.0, -1.0], // BL
                [1.0, -1.0, -1.0],  // BR
                [1.0, -1.0, 1.0],   // TR
                [-1.0, -1.0, 1.0],  // TL
            ],
        ),
        // Back (-Z); viewer at -Z looking toward +Z; label-up = +Y.
        (
            Face::Back,
            [
                [1.0, -1.0, -1.0],  // BL
                [-1.0, -1.0, -1.0], // BR
                [-1.0, 1.0, -1.0],  // TR
                [1.0, 1.0, -1.0],   // TL
            ],
        ),
        // Front (+Z); viewer at +Z looking toward -Z; label-up = +Y.
        (
            Face::Front,
            [
                [-1.0, -1.0, 1.0],  // BL
                [1.0, -1.0, 1.0],   // BR
                [1.0, 1.0, 1.0],    // TR
                [-1.0, 1.0, 1.0],   // TL
            ],
        ),
    ];

    // UVs map `[BL, BR, TR, TL]` to texture coords. Texture is stored
    // top-down (V=0 at top); BL of the viewer therefore samples V=1.
    let uvs: [[f32; 2]; 4] = [
        [0.0, 1.0], // BL
        [1.0, 1.0], // BR
        [1.0, 0.0], // TR
        [0.0, 0.0], // TL
    ];

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
        for f in Face::ALL.iter() {
            let face_idx = *f as usize;
            let v0 = &v[face_idx * 4];
            let dot = v0.pos[0] * v0.normal[0]
                + v0.pos[1] * v0.normal[1]
                + v0.pos[2] * v0.normal[2];
            assert!(dot > 0.0, "face {:?} normal points inward", f);
        }
    }

    #[test]
    fn first_corner_of_each_face_is_bottom_left_in_viewer_frame() {
        // BL = smallest projection along both viewer's "right" and
        // viewer's "up" axes.  Equivalent: BL has UV = (0, 1).
        let (v, _) = build_cube();
        for f in Face::ALL.iter() {
            let face_idx = *f as usize;
            let bl = &v[face_idx * 4];
            assert_eq!(bl.uv, [0.0, 1.0], "face {:?} BL UV", f);
        }
    }

    #[test]
    fn opposite_corners_have_opposite_uvs() {
        let (v, _) = build_cube();
        for f in Face::ALL.iter() {
            let face_idx = *f as usize;
            let bl = &v[face_idx * 4];
            let tr = &v[face_idx * 4 + 2];
            assert_eq!(bl.uv, [0.0, 1.0], "{:?} BL", f);
            assert_eq!(tr.uv, [1.0, 0.0], "{:?} TR", f);
        }
    }

    #[test]
    fn face_winding_is_ccw_from_outside() {
        // Edge BL->BR and BL->TL should be perpendicular in world
        // space; their cross product should align with the face's
        // outward normal (positive dot).
        let (v, _) = build_cube();
        for f in Face::ALL.iter() {
            let face_idx = *f as usize;
            let bl = v[face_idx * 4].pos;
            let br = v[face_idx * 4 + 1].pos;
            let tl = v[face_idx * 4 + 3].pos;
            let e1 = [br[0] - bl[0], br[1] - bl[1], br[2] - bl[2]];
            let e2 = [tl[0] - bl[0], tl[1] - bl[1], tl[2] - bl[2]];
            let cross = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let n = f.normal();
            let dot = cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2];
            assert!(dot > 0.0, "face {:?} winds CW from outside", f);
        }
    }
}
