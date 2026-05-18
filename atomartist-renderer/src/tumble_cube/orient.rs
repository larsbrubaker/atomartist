//! Map a `(face, tile)` (or combination of two/three for edges/corners)
//! to a target orbit-camera orientation as a `Quat`, suitable for the
//! `OrientAnimation` slerp that snaps the main view there.
//!
//! Direct port of MatterCAD's `TumbleCubeControl.GetDirectionForFace`
//! (`TumbleCubeControl.cs` lines 416-490). The face → view direction
//! table below mirrors the C# `case 0..5` block exactly, using the
//! Z-up world AtomArtist now shares with MatterCAD.
//!
//! ## Face label → camera back direction (Z-up)
//!
//! The "back" direction is `orientation * Vec3::Z` — the world vector
//! pointing from the orbit centre to the eye. So clicking "Top" puts
//! the camera at +Z above the bed; clicking "Front" puts it in front
//! of the bed at -Y looking toward +Y.
//!
//! ```text
//!   "Top"    → camera back = +Z   (camera above the bed, looking -Z)
//!   "Bottom" → camera back = -Z   (camera below the bed, looking +Z)
//!   "Right"  → camera back = +X   (-X forward)
//!   "Left"   → camera back = -X
//!   "Front"  → camera back = -Y   (camera at -Y looking toward +Y)
//!   "Back"   → camera back = +Y
//! ```
//!
//! `orientation_for_view_direction` in `camera.rs` converts a view
//! direction into a clean `Quat`, picking a sensible up-hint at the
//! singular top / bottom cases so the resulting camera basis has
//! world +X on screen-right.

use glam::{Quat, Vec3};

use super::cube_geometry::Face;
use super::hit_test::HitData;
use crate::camera::orientation_for_view_direction;

/// Target orientation for the orbit camera — the rotation that
/// visually "centres" the cube face / edge / corner the user
/// clicked on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetPose {
    pub orientation: Quat,
}

/// Map a clicked `HitData` to a target camera orientation.
///
/// Strategy:
///   1. Map each hit `(face, tile)` to a "view direction" — the unit
///      world vector pointing from the orbit centre toward the camera
///      when that face is centred (i.e. the face's outward normal in
///      world space, in AtomArtist's Y-up coords).
///   2. Average the per-face directions to handle corner/edge hits —
///      e.g. a Top+Front+Left corner click averages three orthogonal
///      directions into a body-diagonal view.
///   3. Convert the averaged direction into a `Quat` via
///      `orientation_for_view_direction`, which handles the
///      previously-singular Top / Bottom cases.
pub fn target_for_hit(hit: HitData) -> Option<TargetPose> {
    let mut sum = Vec3::ZERO;
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
        sum += face_to_view_direction(face);
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let d = sum / n as f32;
    let d = d.normalize_or_zero();
    if d == Vec3::ZERO {
        return None;
    }
    Some(TargetPose {
        orientation: orientation_for_view_direction(d.to_array()),
    })
}

/// World-space view direction (eye-from-orbit-centre) for a
/// face-centred click. Direct port of MatterCAD's switch in
/// `GetDirectionForFace` (Z-up).
///
/// Note: MatterCAD stores the *camera-forward* normal (the
/// negation of what we want) in its switch — e.g. `Top → -Vector3.UnitZ`.
/// AtomArtist's `target_for_hit` then feeds the eye-from-centre
/// direction into `orientation_for_view_direction`, so we return the
/// negation here ("Top" → `+Z`).
fn face_to_view_direction(face: Face) -> Vec3 {
    match face {
        Face::Top => Vec3::Z,        // camera at +Z above bed
        Face::Bottom => Vec3::NEG_Z, // camera below the bed
        Face::Right => Vec3::X,
        Face::Left => Vec3::NEG_X,
        Face::Front => Vec3::NEG_Y,  // CAD convention: front of bed at -Y
        Face::Back => Vec3::Y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn front_face_targets_camera_at_negative_y() {
        // CAD convention (Z-up): "Front" view puts the camera at -Y
        // looking toward +Y, with world +Z as screen-up. The
        // resulting orientation's back vector is -Y.
        let hit = HitData::single(Face::Front as u8, 4);
        let t = target_for_hit(hit).unwrap();
        let back = t.orientation * Vec3::Z;
        assert!(
            (back - Vec3::NEG_Y).length() < 1e-4,
            "Front face back vector should be -Y, got {back:?}"
        );
    }

    #[test]
    fn top_face_targets_clean_orientation() {
        // Z-up: Top view back = +Z. The quaternion path picks an
        // up-hint of +Y at this singular case so world +X reads as
        // screen-right.
        let hit = HitData::single(Face::Top as u8, 4);
        let t = target_for_hit(hit).unwrap();
        let back = t.orientation * Vec3::Z;
        assert!(
            (back - Vec3::Z).length() < 1e-4,
            "Top face back vector should be +Z, got {back:?}"
        );
    }

    #[test]
    fn right_face_targets_camera_at_positive_x() {
        let hit = HitData::single(Face::Right as u8, 4);
        let t = target_for_hit(hit).unwrap();
        let back = t.orientation * Vec3::Z;
        assert!(
            (back - Vec3::X).length() < 1e-4,
            "Right face back vector should be +X, got {back:?}"
        );
    }

    #[test]
    fn front_top_edge_lands_between_face_and_top_targets() {
        let hit = HitData::double((Face::Top as u8, 7), (Face::Front as u8, 1));
        let t = target_for_hit(hit).unwrap();
        // Averaged direction: (0,0,1) + (0,-1,0) normalized
        // = (0, -1/√2, 1/√2).
        let back = t.orientation * Vec3::Z;
        let expected = Vec3::new(0.0, -0.5f32.sqrt(), 0.5f32.sqrt());
        assert!(
            (back - expected).length() < 1e-3,
            "Front+Top edge back vector should split the diagonal; got {back:?}, expected {expected:?}"
        );
    }
}
