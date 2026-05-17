//! Cube ray-hit → face/tile resolution.
//!
//! Port of MatterCAD's `ConnectedFaces` / `HitData` / `GetHitData`
//! helpers (`TumbleCubeControl.cs` lines 45-146, 497-594).  Given a
//! world-space hit point on the unit cube, returns up to three
//! `(face, tile)` pairs identifying the face plus (for corner / edge
//! hits) the neighbouring faces and tiles sharing the picked corner /
//! edge.
//!
//! The cube lives in `[-1, +1]^3`. Tiles are numbered 0..9 within each
//! face (3×3 grid, see [`super::face_textures`] for the layout); tile 4
//! is the face centre, edges are 1/3/5/7, corners 0/2/6/8.
//!
//! Coordinate convention for hit positions: components are clamped to
//! the unit cube, so hit thresholds against `±0.5` decide which third
//! of the face was struck. MatterCAD's code uses `> 1` against a `±2`
//! cube; we scale to `[-1, 1]` here so the threshold becomes `> 0.5`.

#[cfg(test)]
use super::cube_geometry::Face;

/// MatterCAD's connected-faces table.  Each face records:
///   - `axis` — which world axis it is normal to (0 = X, 1 = Y, 2 = Z).
///   - `direction` — +1 for the positive side of that axis, -1 for the
///     negative.
///   - `left`/`right`/`top`/`bottom` — indices into the `connections`
///     vector of the four faces that share an edge with this one.
///
/// Verbatim from `TumbleCubeControl.cs` lines 182-189.
#[derive(Clone, Copy, Debug)]
pub struct ConnectedFaces {
    pub axis: usize,
    pub direction: f32,
    pub left: usize,
    pub bottom: usize,
    pub right: usize,
    pub top: usize,
}

impl ConnectedFaces {
    /// Tile shared with the face on the given edge — port of
    /// `ConnectedFaces.Tile(int faceSharingEdge)`.
    fn tile_for_edge(&self, face_sharing_edge: usize) -> u32 {
        if face_sharing_edge == self.left {
            3
        } else if face_sharing_edge == self.bottom {
            1
        } else if face_sharing_edge == self.right {
            5
        } else if face_sharing_edge == self.top {
            7
        } else {
            4
        }
    }

    /// Tile shared with two faces at a corner — port of
    /// `ConnectedFaces.Tile(int faceCornerA, int faceCornerB)`.
    fn tile_for_corner(&self, a: usize, b: usize) -> u32 {
        if a == self.left {
            if b == self.top {
                6
            } else {
                0
            }
        } else if a == self.bottom {
            if b == self.left {
                0
            } else {
                2
            }
        } else if a == self.right {
            if b == self.top {
                8
            } else {
                2
            }
        } else if a == self.top {
            if b == self.left {
                6
            } else {
                8
            }
        } else {
            4
        }
    }
}

/// Result of [`get_hit_data`].  Up to three `(face, tile)` pairs — a
/// face hit fills slot 0, an edge hit fills slots 0+1, a corner hit
/// fills all three.  Unused slots store `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitData {
    pub face_tile: [Option<(u8, u32)>; 3],
}

impl HitData {
    pub fn empty() -> Self {
        Self {
            face_tile: [None, None, None],
        }
    }

    pub fn single(face: u8, tile: u32) -> Self {
        let mut h = Self::empty();
        h.face_tile[0] = Some((face, tile));
        h
    }

    pub fn double(a: (u8, u32), b: (u8, u32)) -> Self {
        let mut h = Self::empty();
        h.face_tile[0] = Some(a);
        h.face_tile[1] = Some(b);
        h
    }

    pub fn triple(a: (u8, u32), b: (u8, u32), c: (u8, u32)) -> Self {
        let mut h = Self::empty();
        h.face_tile[0] = Some(a);
        h.face_tile[1] = Some(b);
        h.face_tile[2] = Some(c);
        h
    }
}

/// MatterCAD's connection table, indexed by face index `0..6`.  Order
/// matches `Face::ALL` and the `connections.Add(...)` calls in the
/// original C# code.
pub fn connections() -> [ConnectedFaces; 6] {
    [
        // 0: Top   axis=Z dir=+1, left=Left(1), bottom=Front(5), right=Right(2), top=Back(4)
        ConnectedFaces {
            axis: 2,
            direction: 1.0,
            left: 1,
            bottom: 5,
            right: 2,
            top: 4,
        },
        // 1: Left  axis=X dir=-1, left=Back(4), bottom=Bottom(3), right=Front(5), top=Top(0)
        ConnectedFaces {
            axis: 0,
            direction: -1.0,
            left: 4,
            bottom: 3,
            right: 5,
            top: 0,
        },
        // 2: Right axis=X dir=+1, left=Front(5), bottom=Bottom(3), right=Back(4), top=Top(0)
        ConnectedFaces {
            axis: 0,
            direction: 1.0,
            left: 5,
            bottom: 3,
            right: 4,
            top: 0,
        },
        // 3: Bottom axis=Z dir=-1, left=Left(1), bottom=Back(4), right=Right(2), top=Front(5)
        ConnectedFaces {
            axis: 2,
            direction: -1.0,
            left: 1,
            bottom: 4,
            right: 2,
            top: 5,
        },
        // 4: Back  axis=Y dir=+1, left=Right(2), bottom=Bottom(3), right=Left(1), top=Top(0)
        ConnectedFaces {
            axis: 1,
            direction: 1.0,
            left: 2,
            bottom: 3,
            right: 1,
            top: 0,
        },
        // 5: Front axis=Y dir=-1, left=Left(1), bottom=Bottom(3), right=Right(2), top=Top(0)
        ConnectedFaces {
            axis: 1,
            direction: -1.0,
            left: 1,
            bottom: 3,
            right: 2,
            top: 0,
        },
    ]
}

/// Border threshold beyond which a coordinate is considered "off-centre"
/// enough to belong to an edge / corner tile rather than the centre tile.
/// MatterCAD uses `> 1.0` for a `[-2, 2]` cube; on our `[-1, 1]` cube
/// the equivalent threshold is `> 0.5`.
const EDGE_THRESHOLD: f32 = 0.5;

/// Resolve a hit point on the unit cube to the corresponding face / tile
/// configuration.  Port of `TumbleCubeControl.GetHitData`.
///
/// `hit_pos` is in `[-1, 1]^3` world space — the caller is expected to
/// have raycast against the cube already.
pub fn get_hit_data(hit_pos: [f32; 3]) -> HitData {
    let connections = connections();
    for i in 0..6 {
        let face = &connections[i];
        // The picked component on the face's axis should be ≈ ±1.
        let on_face = (hit_pos[face.axis] - face.direction).abs() < 1e-3;
        if !on_face {
            continue;
        }

        // Helper: how far the hit is along the neighbour face's normal
        // axis (positive = "on the neighbour's side").  Mirrors
        // MatterCAD's `hitPosition[connections[X].axis] * connections[X].direction`.
        let along = |neighbour_idx: usize| -> f32 {
            let n = &connections[neighbour_idx];
            hit_pos[n.axis] * n.direction
        };

        let on_left = along(face.left) > EDGE_THRESHOLD;
        let on_right = along(face.right) > EDGE_THRESHOLD;
        let on_top = along(face.top) > EDGE_THRESHOLD;
        let on_bottom = along(face.bottom) > EDGE_THRESHOLD;

        // Order of checks matches MatterCAD's nested if/else exactly so
        // ports of the test table line up.

        if on_left {
            if on_bottom {
                // Corner: face + left + bottom.
                return HitData::triple(
                    (i as u8, 0),
                    (
                        face.left as u8,
                        connections[face.left].tile_for_corner(i, face.bottom),
                    ),
                    (
                        face.bottom as u8,
                        connections[face.bottom].tile_for_corner(i, face.left),
                    ),
                );
            }
            if on_top {
                // Corner: face + left + top.
                return HitData::triple(
                    (i as u8, 6),
                    (
                        face.left as u8,
                        connections[face.left].tile_for_corner(i, face.top),
                    ),
                    (
                        face.top as u8,
                        connections[face.top].tile_for_corner(i, face.left),
                    ),
                );
            }
            // Edge: face left + left neighbour.
            return HitData::double(
                (i as u8, 3),
                (face.left as u8, connections[face.left].tile_for_edge(i)),
            );
        }

        if on_right {
            if on_bottom {
                return HitData::triple(
                    (i as u8, 2),
                    (
                        face.right as u8,
                        connections[face.right].tile_for_corner(i, face.bottom),
                    ),
                    (
                        face.bottom as u8,
                        connections[face.bottom].tile_for_corner(i, face.right),
                    ),
                );
            }
            if on_top {
                return HitData::triple(
                    (i as u8, 8),
                    (
                        face.right as u8,
                        connections[face.right].tile_for_corner(i, face.top),
                    ),
                    (
                        face.top as u8,
                        connections[face.top].tile_for_corner(i, face.right),
                    ),
                );
            }
            return HitData::double(
                (i as u8, 5),
                (face.right as u8, connections[face.right].tile_for_edge(i)),
            );
        }

        if on_bottom {
            return HitData::double(
                (i as u8, 1),
                (face.bottom as u8, connections[face.bottom].tile_for_edge(i)),
            );
        }
        if on_top {
            return HitData::double(
                (i as u8, 7),
                (face.top as u8, connections[face.top].tile_for_edge(i)),
            );
        }

        // Centre tile of the face.
        return HitData::single(i as u8, 4);
    }
    HitData::empty()
}

/// Brute-force ray vs. axis-aligned `[-1, 1]^3` cube.  Returns the
/// closest entry hit point in world space (slabs test) or `None` if the
/// ray misses.  Used by the cube widget's mouse-move and click paths.
pub fn raycast_unit_cube(origin: [f32; 3], dir: [f32; 3]) -> Option<[f32; 3]> {
    let mut tmin = f32::NEG_INFINITY;
    let mut tmax = f32::INFINITY;
    for k in 0..3 {
        if dir[k].abs() < 1e-7 {
            // Ray parallel to slab — must be inside or miss entirely.
            if origin[k] < -1.0 || origin[k] > 1.0 {
                return None;
            }
        } else {
            let inv = 1.0 / dir[k];
            let t1 = (-1.0 - origin[k]) * inv;
            let t2 = (1.0 - origin[k]) * inv;
            let (lo, hi) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
            if lo > tmin {
                tmin = lo;
            }
            if hi < tmax {
                tmax = hi;
            }
            if tmin > tmax {
                return None;
            }
        }
    }
    if tmax < 0.0 {
        return None;
    }
    let t = if tmin >= 0.0 { tmin } else { tmax };
    Some([origin[0] + dir[0] * t, origin[1] + dir[1] * t, origin[2] + dir[2] * t])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face_idx(f: Face) -> u8 {
        f as u8
    }

    #[test]
    fn centre_hit_on_each_face_resolves_to_tile_4() {
        // Six face-centre hits — pick a small offset to ensure the
        // "near 1" axis check matches but the tile decision lands in
        // the centre (tile 4).
        let cases = [
            (Face::Top, [0.0, 0.0, 1.0]),
            (Face::Bottom, [0.0, 0.0, -1.0]),
            (Face::Left, [1.0, 0.0, 0.0]),
            (Face::Right, [-1.0, 0.0, 0.0]),
            (Face::Back, [0.0, 1.0, 0.0]),
            (Face::Front, [0.0, -1.0, 0.0]),
        ];
        for (face, pos) in cases.iter() {
            let hit = get_hit_data(*pos);
            assert_eq!(hit.face_tile[0], Some((face_idx(*face), 4)),
                "face {:?} centre hit should be tile 4, got {:?}", face, hit);
            assert_eq!(hit.face_tile[1], None, "centre hit should be single-face");
        }
    }

    #[test]
    fn corner_hit_returns_three_faces() {
        // Hit near the corner where Top + Left + Front meet.
        let hit = get_hit_data([0.95, -0.95, 1.0]);
        let face_a = hit.face_tile[0].expect("at least one face");
        let face_b = hit.face_tile[1].expect("corner = three faces");
        let face_c = hit.face_tile[2].expect("corner = three faces");
        // Faces involved should be Top + Left + Front in some order.
        let set: std::collections::HashSet<u8> = [face_a.0, face_b.0, face_c.0]
            .iter()
            .copied()
            .collect();
        assert!(set.contains(&face_idx(Face::Top)));
        assert!(set.contains(&face_idx(Face::Left)));
        assert!(set.contains(&face_idx(Face::Front)));
    }

    #[test]
    fn edge_hit_returns_two_faces() {
        // Top + Front edge centre: x ≈ 0, y = -1, z = 1.
        let hit = get_hit_data([0.0, -0.95, 1.0]);
        let a = hit.face_tile[0].unwrap();
        let b = hit.face_tile[1].expect("edge hit = two faces");
        assert!(hit.face_tile[2].is_none());
        let set: std::collections::HashSet<u8> = [a.0, b.0].iter().copied().collect();
        assert!(set.contains(&face_idx(Face::Top)));
        assert!(set.contains(&face_idx(Face::Front)));
    }

    #[test]
    fn ray_misses_cube_when_pointed_away() {
        let r = raycast_unit_cube([0.0, 0.0, 5.0], [0.0, 0.0, 1.0]);
        assert!(r.is_none());
    }

    #[test]
    fn ray_hits_top_face_when_pointed_down_at_origin() {
        let r = raycast_unit_cube([0.0, 0.0, 5.0], [0.0, 0.0, -1.0]).unwrap();
        // Should hit z = 1 first.
        assert!((r[2] - 1.0).abs() < 1e-4, "z = {}", r[2]);
    }
}
