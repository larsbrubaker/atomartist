//! Unit tests for `OrbitCamera`. Split out of `camera.rs` so the
//! main camera module stays under the repository file-line-count
//! guardrail. Pulled in via `#[path]` so it sits next to its sibling
//! rather than nesting under `camera/`.

use std::f32::consts::PI;

use glam::{Quat, Vec3};

use super::{orientation_for_view_direction, OrbitCamera, OrbitMode, Projection};
use crate::camera_animations::{CameraPoseAnimation, ProjectionAnimation};

/// Default home pose: MatterCAD's `ResetView()` rotates world
/// around +Z by -Tau/16 and then around +X by Tau*0.19. Starting
/// from the identity (Top view, camera straight above looking
/// down) this puts the camera in a 3/4 view in front-left of
/// the bed, slightly elevated above the XY plane.
#[test]
fn default_orientation_matches_mattercad_resetview() {
    let c = OrbitCamera::default();
    let back = c.orientation * Vec3::Z;
    // back ≈ (-0.36, -0.86, 0.37) — front-left of the bed, just
    // above horizontal. Check the signs and order of magnitude.
    assert!(back.x < 0.0, "camera should be on the -X side; back = {back:?}");
    assert!(back.y < 0.0, "camera should be in front of the bed (-Y); back = {back:?}");
    assert!(back.z > 0.0, "camera should be slightly elevated (+Z); back = {back:?}");
    assert!(back.y.abs() > back.z.abs(), "should be a 3/4 view, not top-down; back = {back:?}");
}

/// Turntable cursor-up (`pitch_angle > 0`) tilts the camera DOWN
/// (eye drops below the bed plane, looking up at the model — the
/// MatterCAD "see-from-below" feel). Hammering that direction
/// must clamp at the lower pitch limit and never let `back.z`
/// drop below `sin(-π·0.499)`.
#[test]
fn turntable_pitch_clamp_stops_at_lower_pole() {
    let mut c = OrbitCamera::default();
    c.orbit_mode = OrbitMode::Turntable;
    c.orientation = Quat::IDENTITY;
    for _ in 0..200 {
        c.orbit_drag(0.0, 0.1);
    }
    let back = c.orientation * Vec3::Z;
    let lower_pole = -(PI * 0.499).sin();
    assert!(
        back.z >= lower_pole - 1e-3,
        "turntable pitch should clamp at lower pole; back.z = {} (limit {})",
        back.z,
        lower_pole
    );
}

/// Regression for "I can rotate past the top." From Top view,
/// `pitch_angle < 0` (cursor-down) wants to tilt the camera
/// further UP — but the camera is already at the +Z pole. The
/// clamp must hold it there; the orientation must NOT wrap around
/// to the other side.
#[test]
fn turntable_cannot_rotate_past_the_top() {
    let mut c = OrbitCamera::default();
    c.orbit_mode = OrbitMode::Turntable;
    // Top view exactly.
    c.orientation = Quat::IDENTITY;
    // Drive the "want camera further up" direction hard.
    for _ in 0..200 {
        c.orbit_drag(0.0, -0.1);
    }
    let back = c.orientation * Vec3::Z;
    let upper_pole = (PI * 0.499).sin();
    // back.z must stay near the upper pole — definitely never
    // flip below the equator (which is what "rotating past the top"
    // looks like to the user).
    assert!(
        back.z >= upper_pole - 1e-3,
        "should still be near the +Z pole; back.z = {} (clamp {})",
        back.z,
        upper_pole
    );
    assert!(back.z <= 1.0 + 1e-3, "physical bound");
}

/// Trackball and turntable must agree on the cursor-up direction:
/// both should tilt the camera DOWN (eye Z decreases, model's
/// underside becomes visible) so the modes feel like alternative
/// interpretations of the SAME gesture, not opposite ones.
#[test]
fn trackball_and_turntable_match_on_cursor_up_direction() {
    let mut tt = OrbitCamera::default();
    tt.orbit_mode = OrbitMode::Turntable;
    let mut tb = tt.clone();
    tb.orbit_mode = OrbitMode::Trackball;
    // The HUD layer feeds `orbit_drag(_, +scale)` for cursor-up;
    // emulate that here.
    let pitch = 0.2;
    let tt_z_before = (tt.orientation * Vec3::Z).z;
    let tb_z_before = (tb.orientation * Vec3::Z).z;
    tt.orbit_drag(0.0, pitch);
    tb.orbit_drag(0.0, pitch);
    let tt_z_after = (tt.orientation * Vec3::Z).z;
    let tb_z_after = (tb.orientation * Vec3::Z).z;
    assert!(
        tt_z_after < tt_z_before,
        "turntable cursor-up must lower back.z; {tt_z_before} → {tt_z_after}"
    );
    assert!(
        tb_z_after < tb_z_before,
        "trackball cursor-up must also lower back.z; {tb_z_before} → {tb_z_after}"
    );
}

/// Trackball still lets the camera tumble past the +Z pole when
/// driven hard — that's the headline difference from turntable
/// (no clamp). Drive a big drag from a Front-view start and verify
/// the camera goes upside down (camera-up Z component flips).
#[test]
fn trackball_can_tumble_past_the_pole() {
    let mut c = OrbitCamera::default();
    c.orbit_mode = OrbitMode::Trackball;
    c.orientation = orientation_for_view_direction([0.0, -1.0, 0.0]);
    c.orbit_drag(0.0, PI);
    let cam_up = c.orientation * Vec3::Y;
    assert!(
        cam_up.z < 0.0,
        "trackball should still be able to tumble past the pole; up = {cam_up:?}"
    );
}

/// Regression: drag-right should rotate the world so it appears
/// to follow the cursor right. Starting from Front view (camera
/// at -Y looking +Y), a rightward drag yaws the camera around
/// world +Z to the camera-left, which is the -X side. The
/// camera's back vector picks up a negative X component.
#[test]
fn drag_right_from_front_moves_camera_to_negative_x() {
    let mut c = OrbitCamera::default();
    c.orientation = orientation_for_view_direction([0.0, -1.0, 0.0]);
    c.orbit_mode = OrbitMode::Turntable;
    let dx = 10.0f32;
    let scale = 0.005f32;
    // HUD pre-flips: rightward drag → orbit_drag(-dx * scale).
    c.orbit_drag(-dx * scale, 0.0);
    let back = c.orientation * Vec3::Z;
    assert!(
        back.x < 0.0,
        "right-drag from Front should move camera to -X; back = {back:?}"
    );
    assert!(
        back.y < 0.0,
        "still mostly in front of the bed after a small yaw; back = {back:?}"
    );
}

#[test]
fn orthographic_projection_has_no_perspective_divide() {
    let mut c = OrbitCamera::default();
    c.projection = Projection::Orthographic;
    let m = c.projection_matrix(1.0);
    // Ortho's last column is (0, 0, 0, 1) — no w-divide.
    assert!((m[3]).abs() < 1e-5);
    assert!((m[7]).abs() < 1e-5);
    assert!((m[11]).abs() < 1e-5);
    assert!((m[15] - 1.0).abs() < 1e-5);
}

#[test]
fn projection_animation_to_ortho_lands_in_ortho() {
    let mut cam = OrbitCamera::default();
    cam.projection = Projection::Perspective;
    let fov_before = cam.fov_y;
    let radius_before = cam.radius;
    let mut anim = ProjectionAnimation::new(&cam, Projection::Orthographic, 0.25);
    let done = anim.step(&mut cam, 1.0);
    assert!(done);
    assert_eq!(cam.projection, Projection::Orthographic);
    assert!((cam.fov_y - fov_before).abs() < 1e-5);
    assert!((cam.radius - radius_before).abs() < 1e-5);
}

#[test]
fn projection_animation_to_perspective_lands_in_perspective() {
    let mut cam = OrbitCamera::default();
    cam.projection = Projection::Orthographic;
    let mut anim = ProjectionAnimation::new(&cam, Projection::Perspective, 0.25);
    let done = anim.step(&mut cam, 1.0);
    assert!(done);
    assert_eq!(cam.projection, Projection::Perspective);
}

/// Regression for the "single white frame at the end of the
/// ortho tween" bug: mid-tween the camera is pushed back to a
/// huge radius (`ref_half_h / tan(fov_min/2)`) so the
/// perspective view still shows the same on-screen height as
/// the upcoming ortho frame. Without dilating `far` to match,
/// the model exits the far frustum and wgpu clips the entire
/// scene — a single-frame flash to background colour. The
/// animation must keep `radius < far` throughout the tween.
#[test]
fn projection_animation_keeps_radius_inside_far_plane() {
    let mut cam = OrbitCamera::default();
    cam.projection = Projection::Perspective;
    let mut anim = ProjectionAnimation::new(&cam, Projection::Orthographic, 0.25);
    // Step in small increments — the worst frames are right
    // before progress = 1.0, when `fov` ≈ `fov_min`.
    for _ in 0..50 {
        anim.step(&mut cam, 0.006);
        assert!(
            cam.radius < cam.far,
            "radius {} must stay inside far {}",
            cam.radius,
            cam.far
        );
    }
}

#[test]
fn projection_animation_preserves_visible_height_mid_tween() {
    let mut cam = OrbitCamera::default();
    cam.projection = Projection::Perspective;
    let ref_half_h = (cam.fov_y * 0.5).tan() * cam.radius;
    let mut anim = ProjectionAnimation::new(&cam, Projection::Orthographic, 0.25);
    anim.step(&mut cam, 0.125);
    let mid_half_h = (cam.fov_y * 0.5).tan() * cam.radius;
    assert!(
        (mid_half_h - ref_half_h).abs() < 1e-3,
        "tween should hold half-height; ref={} mid={}",
        ref_half_h,
        mid_half_h
    );
}

#[test]
fn fit_to_bounds_centers_and_sets_radius() {
    let mut c = OrbitCamera::default();
    c.fit_to_bounds([-1.0, -1.0, -1.0], [3.0, 5.0, 7.0]);
    assert!((c.center[0] - 1.0).abs() < 1e-5);
    assert!((c.center[1] - 2.0).abs() < 1e-5);
    assert!((c.center[2] - 3.0).abs() < 1e-5);
    assert!(c.radius > 0.0);
}

#[test]
fn reset_view_restores_default_orientation() {
    let mut c = OrbitCamera::default();
    c.orientation = Quat::from_rotation_y(1.0);
    c.center = [10.0, 20.0, 30.0];
    let r_before = c.radius;
    c.reset_view();
    let d = OrbitCamera::default();
    let cosine = c.orientation.dot(d.orientation).abs();
    assert!(cosine > 0.999, "orientation should match default (|dot| = {})", cosine);
    assert_eq!(c.center, d.center);
    assert_eq!(c.radius, r_before, "Home preserves zoom");
}

/// Identity orientation in Z-up = Top view (camera straight
/// above looking down). Camera local Y axis (screen up) maps to
/// world +Y. So a "drag-down" (agg-gui dy < 0 in Y-up screen
/// coords) should slide the model down on screen — which means
/// the camera centre's world +Y component should INCREASE
/// (camera looks at a point further "back" on the bed, so the
/// origin appears further toward the bottom of the screen).
#[test]
fn pan_down_drag_raises_look_at() {
    let mut c = OrbitCamera::default();
    c.orientation = Quat::IDENTITY;
    let y0 = c.center[1];
    c.pan(0.0, -10.0);
    assert!(
        c.center[1] > y0,
        "drag-down should raise the look-at's world-Y (screen-up axis at top view); was {} → {}",
        y0,
        c.center[1]
    );
}

/// At identity (top view), camera-right = world +X. A rightward
/// drag shifts the camera's centre to its left — so world X
/// decreases.
#[test]
fn pan_right_drag_shifts_center_left_in_eye_frame() {
    let mut c = OrbitCamera::default();
    c.orientation = Quat::IDENTITY;
    let x0 = c.center[0];
    c.pan(10.0, 0.0);
    assert!(
        c.center[0] < x0,
        "pan with positive dx (rightward drag) should shift centre to the camera's left; was {} → {}",
        x0,
        c.center[0]
    );
}

#[test]
fn pan_ignores_nan_inputs() {
    let mut c = OrbitCamera::default();
    let center0 = c.center;
    c.pan(f32::NAN, 0.0);
    c.pan(0.0, f32::NAN);
    c.pan(f32::INFINITY, 0.0);
    assert_eq!(c.center, center0);
}

#[test]
fn zoom_ignores_nan_factor() {
    let mut c = OrbitCamera::default();
    let r0 = c.radius;
    c.zoom(f32::NAN);
    assert_eq!(c.radius, r0);
}

/// Regression: the old Euler `(az, el)` lerp could take the LONG
/// way around when `target_az - start_az` crossed ±π — Home from
/// an "almost rear" view would spin a full 270° instead of going
/// the 90° short way. `Quat::slerp` always picks the geodesic on
/// the unit sphere (glam negates one operand when `dot < 0`), so
/// the mid-point of the animation must sit closer to the target
/// than to the start measured in quaternion-angle.
#[test]
fn camera_pose_animation_takes_short_path_when_orientation_wraps() {
    let mut start = OrbitCamera::default();
    // Yaw a touch under +π — almost looking at the back of the model.
    start.orientation = Quat::from_rotation_y(PI - 0.1);
    let mut target = start.clone();
    // Target yaw a touch under -π — same model orientation,
    // wraps the wrong way under naive Euler lerp.
    target.orientation = Quat::from_rotation_y(-PI + 0.1);

    let mut cam = start.clone();
    let mut anim = CameraPoseAnimation::new(&start, target.clone(), 0.25);
    anim.step(&mut cam, 0.125); // halfway through

    let to_target = cam.orientation.angle_between(target.orientation);
    assert!(
        to_target < 0.15,
        "slerp must take the short path; half-way still {to_target} rad from target",
    );
}

#[test]
fn camera_pose_animation_reaches_target_orientation_and_radius() {
    let mut start = OrbitCamera::default();
    start.center = [1.0, 2.0, 3.0];
    start.radius = 20.0;
    start.orientation = Quat::from_rotation_y(1.0);

    let mut target = start.clone();
    target.center = [-2.0, 0.5, 4.0];
    target.radius = 42.0;
    target.orientation = Quat::from_rotation_y(-0.25);

    let mut cam = start.clone();
    let mut anim = CameraPoseAnimation::new(&start, target.clone(), 0.25);
    let done = anim.step(&mut cam, 0.25);
    assert!(done);
    assert_eq!(cam.center, target.center);
    assert!((cam.radius - target.radius).abs() < 1e-5);
    let cosine = cam.orientation.dot(target.orientation).abs();
    assert!(cosine > 0.999, "orientation should reach target (|dot| = {})", cosine);
}

/// `orbit_drag_around` with a zero delta is a strict no-op —
/// even when the pivot is far from the orbit centre. This is
/// the property that keeps the scene from "jumping" the
/// instant a rotate drag starts.
#[test]
fn orbit_drag_around_zero_delta_does_not_move_eye() {
    let mut c = OrbitCamera::default();
    let pivot = [5.0, -3.0, 1.0]; // arbitrary world point far from centre
    let eye_before = c.eye();
    let center_before = c.center;
    let orientation_before = c.orientation;
    c.orbit_drag_around(pivot, 0.0, 0.0);
    for k in 0..3 {
        assert!(
            (c.eye()[k] - eye_before[k]).abs() < 1e-5,
            "eye[{}] shifted: {} → {}",
            k, eye_before[k], c.eye()[k]
        );
        assert_eq!(c.center[k], center_before[k]);
    }
    let cosine = c.orientation.dot(orientation_before).abs();
    assert!(cosine > 1.0 - 1e-5, "orientation rotated unexpectedly");
}

/// MatterCAD parity: rotating around a pivot keeps that pivot's
/// **world** position fixed — the camera orbits *around* it.
/// The pivot's screen projection is what the rotate cursor
/// circle is anchored to, so this is what makes the circle
/// track the cursor cleanly.
#[test]
fn orbit_drag_around_holds_pivot_world_position() {
    let mut c = OrbitCamera::default();
    let pivot = Vec3::new(2.0, -4.0, 0.5);
    let eye_before = Vec3::from(c.eye());
    let dist_before = (eye_before - pivot).length();
    c.orbit_drag_around(pivot.to_array(), 0.5, 0.2);
    let eye_after = Vec3::from(c.eye());
    let dist_after = (eye_after - pivot).length();
    assert!(
        (dist_before - dist_after).abs() < 1e-3,
        "eye distance to pivot drifted: {dist_before} → {dist_after}"
    );
    assert!(
        (eye_after - eye_before).length() > 1e-3,
        "non-zero drag should have moved the eye"
    );
}

/// `orbit_drag` honours `orbit_mode`. Apply the same drag in
/// turntable and trackball — at a tilted orientation the
/// quaternion rotations land the camera in different places.
#[test]
fn orbit_drag_diverges_between_turntable_and_trackball_at_elevated_view() {
    let mut tt = OrbitCamera::default();
    tt.orientation = Quat::from_rotation_y(0.5) * Quat::from_rotation_x(-0.8);
    tt.orbit_mode = OrbitMode::Turntable;
    let mut tb = tt.clone();
    tb.orbit_mode = OrbitMode::Trackball;
    let dx = 0.5;
    tt.orbit_drag(dx, 0.0);
    tb.orbit_drag(dx, 0.0);
    let cosine = tt.orientation.dot(tb.orientation).abs();
    assert!(
        cosine < 0.999,
        "turntable and trackball should diverge for a non-equatorial X drag; |dot| = {cosine}"
    );
}

/// Z-up Front view: eye at -Y looking +Y. The back vector
/// (eye-from-centre) is -Y; screen-up is +Z (world up); world +X
/// reads as screen-right.
#[test]
fn orientation_for_front_face_lands_on_negative_y() {
    let q = orientation_for_view_direction([0.0, -1.0, 0.0]);
    let back = q * Vec3::Z;
    let up = q * Vec3::Y;
    let right = q * Vec3::X;
    assert!((back - Vec3::NEG_Y).length() < 1e-4, "front back = -Y; got {back:?}");
    assert!((up - Vec3::Z).length() < 1e-4, "front up = +Z; got {up:?}");
    assert!((right - Vec3::X).length() < 1e-4, "front right = +X; got {right:?}");
}

#[test]
fn orientation_for_right_face_puts_camera_at_positive_x() {
    // Z-up Right view: eye on +X side, looking -X, with world
    // +Z as screen-up and world +Y on screen-right.
    let q = orientation_for_view_direction([1.0, 0.0, 0.0]);
    let back = q * Vec3::Z;
    let up = q * Vec3::Y;
    assert!((back - Vec3::X).length() < 1e-4, "right face back = +X; got {back:?}");
    assert!((up - Vec3::Z).length() < 1e-4, "right face up = +Z; got {up:?}");
}

/// After a trackball tumble the camera can be rolled (`cam_up`
/// no longer aligned with world +Z) or even fully upside down.
/// `snap_to_turntable_alignment` applies the **minimum-angle**
/// rotation that lands cam-up on world +Z, so screen-up is world
/// +Z again and turntable yaw (around world +Z) feels natural.
#[test]
fn snap_to_turntable_alignment_aligns_cam_up_with_world_z() {
    let mut c = OrbitCamera::default();
    c.orbit_mode = OrbitMode::Trackball;
    c.orbit_drag(0.4, 2.5);
    c.snap_to_turntable_alignment();
    let cam_up = c.orientation * Vec3::Y;
    assert!(
        (cam_up - Vec3::Z).length() < 1e-3,
        "cam-up should equal world +Z after snap; got {cam_up:?}"
    );
}

/// Snapping when `cam_up` already equals world +Z must be a
/// no-op modulo numerical noise. Front view is the canonical
/// no-roll orientation where this holds exactly.
#[test]
fn snap_to_turntable_alignment_is_noop_when_already_upright() {
    let mut c = OrbitCamera::default();
    // Front view: orientation_for_view_direction picks +Z as the
    // up-hint so the resulting cam-up is exactly world +Z.
    c.orientation = orientation_for_view_direction([0.0, -1.0, 0.0]);
    let cam_up_before = c.orientation * Vec3::Y;
    assert!(
        (cam_up_before - Vec3::Z).length() < 1e-4,
        "test premise: Front view should have cam_up = +Z; got {cam_up_before:?}"
    );
    let before = c.orientation;
    c.snap_to_turntable_alignment();
    let cosine = c.orientation.dot(before).abs();
    assert!(cosine > 0.999, "already-upright → snap should be a no-op, |dot| = {cosine}");
}

/// Minimum-rotation property: when the camera is only slightly
/// rolled, the snap angle equals that roll angle — NOT the larger
/// distance a yaw-pitch rebuild would produce. Start from a
/// no-roll Front view, roll the camera 10° around the back axis
/// (cam_up tilts 10° away from +Z because back ⊥ cam_up at Front
/// view), and verify the snap removes exactly that 10°.
#[test]
fn snap_to_turntable_alignment_makes_a_minimum_rotation() {
    let mut c = OrbitCamera::default();
    c.orientation = orientation_for_view_direction([0.0, -1.0, 0.0]);
    let back = c.orientation * Vec3::Z;
    let roll = Quat::from_axis_angle(back, 10f32.to_radians());
    c.orientation = (roll * c.orientation).normalize();
    let before = c.orientation;
    c.snap_to_turntable_alignment();
    let angle = before.angle_between(c.orientation);
    assert!(
        angle < 15f32.to_radians(),
        "snap should be ~10° (the roll), got {} deg",
        angle.to_degrees()
    );
    assert!(
        angle > 5f32.to_radians(),
        "snap should NOT be a no-op for a 10° roll; got {} deg",
        angle.to_degrees()
    );
}

#[test]
fn orientation_for_top_face_avoids_singularity() {
    // Z-up: Top view eye-from-center direction = +Z. The
    // resulting orientation must take camera-back to +Z without
    // the singularity the old `look_at(up=+Z)` path hit.
    let q = orientation_for_view_direction([0.0, 0.0, 1.0]);
    let back = q * Vec3::Z;
    assert!((back - Vec3::Z).length() < 1e-4, "top face back vector should be +Z; got {back:?}");
    // World +X should read as screen-right in the top view —
    // confirms the up-hint logic picks +Y at the pole.
    let right = q * Vec3::X;
    assert!((right - Vec3::X).length() < 1e-4, "top view should put world +X on screen-right; got {right:?}");
}
