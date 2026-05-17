//! Map a `(face, tile)` (or combination of two/three for edges/corners)
//! to a target `(azimuth, elevation)` orbit-camera pose, and drive the
//! orientation animation that snaps the main view there.
//!
//! Port of `TumbleCubeControl.GetDirectionForFace` (`TumbleCubeControl.cs`
//! lines 416-495), reinterpreted for AtomArtist's Y-up orbit camera.
//!
//! ## Coordinate translation
//!
//! MatterCAD's world is Z-up; AtomArtist's world is Y-up.  The labels on
//! the cube ("Top", "Front", "Back", "Left", "Right", "Bottom") refer to
//! the user's mental model of the *world*, so we re-map the geometry
//! face indices to camera orientations such that:
//!
//! ```text
//!   "Top"    → look down (+Y up, camera above looking -Y)   el = +π/2
//!   "Bottom" → look up                                      el = -π/2
//!   "Front"  → look at world from +Z toward origin          az = 0    el = 0
//!   "Back"   → look from -Z                                 az = π    el = 0
//!   "Left"   → look from -X                                 az = -π/2 el = 0
//!   "Right"  → look from +X                                 az = +π/2 el = 0
//! ```
//!
//! These match the conventions the AtomArtist viewport already uses for
//! orbit (`azimuth` = yaw around +Y, `elevation` = pitch where `+π/2`
//! puts the camera directly above the centre).

use std::f32::consts::PI;

use super::cube_geometry::Face;
use super::hit_test::HitData;

/// Target pose for the orbit camera — the orientation that visually
/// "centres" the cube face / edge / corner the user clicked on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetPose {
    pub azimuth: f32,
    pub elevation: f32,
}

/// Map a clicked `HitData` to a target camera pose.
///
/// Strategy:
///   1. Map each hit `(face, tile)` to a "view direction" — the unit
///      world vector pointing from the orbit centre toward the camera
///      when that face is centred (i.e. the face's outward normal in
///      world space, in AtomArtist's Y-up coords).
///   2. Average the per-face directions to handle corner/edge hits —
///      e.g. a Top+Front+Left corner click averages three orthogonal
///      directions into a body-diagonal view.
///   3. Convert the averaged direction back to `(az, el)` via
///      `el = asin(dir.y)`, `az = atan2(dir.x, dir.z)`.
pub fn target_for_hit(hit: HitData) -> Option<TargetPose> {
    let mut sum = [0.0f32; 3];
    let mut n = 0;
    for slot in hit.face_tile.iter() {
        let Some((face_idx, _tile)) = slot else { continue };
        let face = match face_idx {
            0 => Face::Top,
            1 => Face::Left,
            2 => Face::Right,
            3 => Face::Bottom,
            4 => Face::Back,
            5 => Face::Front,
            _ => continue,
        };
        let v = face_to_view_direction(face);
        sum[0] += v[0];
        sum[1] += v[1];
        sum[2] += v[2];
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let inv = 1.0 / n as f32;
    let mut d = [sum[0] * inv, sum[1] * inv, sum[2] * inv];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
    d = [d[0] / len, d[1] / len, d[2] / len];
    // Convert direction to azimuth / elevation.
    let elevation = d[1].clamp(-1.0, 1.0).asin();
    let azimuth = d[0].atan2(d[2]);
    // Clamp elevation to the orbit camera's working range so the
    // animation doesn't try to drive past the singularity at ±π/2.
    let limit = PI * 0.499;
    let elevation = elevation.clamp(-limit, limit);
    Some(TargetPose { azimuth, elevation })
}

/// World-space view direction (from the orbit centre toward the camera)
/// for a face-centred click.  AtomArtist convention:
/// `eye = center + radius * (cos(el) * sin(az), sin(el), cos(el) * cos(az))`,
/// so a "view direction" of `[sin(az), 0, cos(az)]` when `el = 0`
/// puts the camera at +Z when `az = 0` (i.e. "Front" — looking at the
/// scene from in front).
fn face_to_view_direction(face: Face) -> [f32; 3] {
    match face {
        Face::Top => [0.0, 1.0, 0.0],     // camera above, looking down
        Face::Bottom => [0.0, -1.0, 0.0], // camera below, looking up
        // Front view: camera in front of the model (+Z), az = 0, el = 0.
        Face::Front => [0.0, 0.0, 1.0],
        Face::Back => [0.0, 0.0, -1.0],
        // Right view: camera to the model's right (+X), az = +π/2.
        Face::Right => [1.0, 0.0, 0.0],
        // Left view: camera to the model's left (-X), az = -π/2.
        Face::Left => [-1.0, 0.0, 0.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_face_targets_az_zero_el_zero() {
        let hit = HitData::single(Face::Front as u8, 4);
        let t = target_for_hit(hit).unwrap();
        assert!(t.azimuth.abs() < 1e-4);
        assert!(t.elevation.abs() < 1e-4);
    }

    #[test]
    fn top_face_targets_high_elevation() {
        let hit = HitData::single(Face::Top as u8, 4);
        let t = target_for_hit(hit).unwrap();
        assert!(
            t.elevation > 1.4,
            "Top should target elevation near +π/2, got {}",
            t.elevation
        );
    }

    #[test]
    fn right_face_targets_az_quarter_turn() {
        let hit = HitData::single(Face::Right as u8, 4);
        let t = target_for_hit(hit).unwrap();
        assert!(
            (t.azimuth - PI * 0.5).abs() < 1e-3,
            "Right should target az = +π/2, got {}",
            t.azimuth
        );
    }

    #[test]
    fn front_top_edge_lands_between_face_and_top_targets() {
        let hit = HitData::double((Face::Top as u8, 7), (Face::Front as u8, 1));
        let t = target_for_hit(hit).unwrap();
        // Front: el=0, Top: el=π/2. Average direction should sit at
        // some positive elevation but well below π/2.
        assert!(t.elevation > 0.4);
        assert!(t.elevation < 1.0);
    }
}
