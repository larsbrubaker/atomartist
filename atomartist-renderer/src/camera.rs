//! Orbit camera for the 3D viewport.
//!
//! The camera orbits around `center` at distance `radius`. Orientation
//! is stored as a unit `Quat` (camera-to-world rotation) rather than
//! Euler `(azimuth, elevation)` so we can:
//!
//!   1. implement a true MatterCAD-style trackball — the trackball
//!      math wants a free rotation axis in the screen plane, which
//!      Euler angles cannot represent without gimbal-lock workarounds;
//!   2. drive top/bottom orientations without the `look_at` singularity
//!      the old Euler path had to clamp away at `el = ±π/2`;
//!   3. `slerp` orientations cleanly for click-to-orient and the Home
//!      tween — matches MatterCAD's `Quaternion.Slerp` in
//!      `TrackballTumbleWidgetExtended.AnimateRotation`.
//!
//! Conventions (match MatterCAD exactly — this is the printer / CAD
//! convention and the bed sits in the XY plane):
//!   - Right-handed, **Z-up** world space. **`Vec3::Z` is world up.**
//!     The print bed lies in the XY plane at Z=0; +Y is "back" of the
//!     bed (away from the user), -Y is "front" (toward the user), +X
//!     is to the user's right.
//!   - Camera local frame: +X right, +Y up-on-screen, +Z out of the
//!     screen (toward the viewer; the camera looks down its own −Z).
//!   - `orientation: Quat` takes camera-local vectors into world
//!     space, so `orientation * Vec3::Z` is the eye-from-center
//!     direction in world units, `orientation * Vec3::Y` is the
//!     camera up vector in world (typically near world +Z when the
//!     horizon is level), etc.
//!   - Clip space matches wgpu/OpenGL: X right, Y up, Z forward into
//!     the screen with depth in `[-1, 1]` post-perspective-divide.

use std::f32::consts::PI;

use glam::{Mat3, Mat4, Quat, Vec3};

/// Projection mode toggled by the perspective button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    Perspective,
    Orthographic,
}

/// Orbit-drag behaviour toggled by the turntable button.
///
/// `Turntable`: drag-X yaws around world-up (+Y), drag-Y pitches
/// around the camera's local right axis with the horizon constrained
/// (camera never flips upside down). Equivalent to MatterCAD's
/// `TurntableEnabled = true` branch in
/// `TrackballTumbleWidgetExtended.DoRotateAroundOrigin`.
///
/// `Trackball`: a true virtual-trackball — the drag vector picks a
/// rotation axis in the screen plane perpendicular to it, and the
/// camera rotates around that axis by an angle proportional to the
/// drag length. Allows roll and lets the camera tumble freely over
/// the poles. Equivalent to MatterCAD's `TurntableEnabled = false`
/// branch using `TrackBallController.GetRotationForMove`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrbitMode {
    Turntable,
    Trackball,
}

#[derive(Clone, Debug)]
pub struct OrbitCamera {
    pub center: [f32; 3],
    pub radius: f32,
    /// Camera-to-world rotation. `orientation * Vec3::Z` is the
    /// eye-from-center direction; `orientation * Vec3::Y` is the
    /// camera's up vector in world space.
    pub orientation: Quat,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
    pub projection: Projection,
    pub orbit_mode: OrbitMode,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0, 0.0],
            radius: 60.0,
            orientation: default_orientation(),
            fov_y: PI * 0.22,
            near: 0.1,
            far: 5000.0,
            projection: Projection::Perspective,
            orbit_mode: OrbitMode::Turntable,
        }
    }
}

/// Default home orientation — direct port of MatterCAD's
/// `View3DWidget.ResetView()`: `world.Rotate` around world-Z by
/// `-Tau/16`, then around world-X by `Tau * 0.19`. In a Z-up world
/// this produces a 3/4 CAD view — camera at front-left, slightly
/// elevated, looking down at the bed.
fn default_orientation() -> Quat {
    let tau = 2.0 * PI;
    Quat::from_rotation_z(-tau / 16.0) * Quat::from_rotation_x(tau * 0.19)
}

impl OrbitCamera {
    /// World-space position of the camera (eye).
    ///
    /// Equivalent to `center + (orientation * Vec3::Z) * radius`
    /// — i.e. the eye sits along the camera's local +Z axis (the
    /// "back" / out-of-screen direction) projected through the
    /// current orientation, at the orbit distance.
    pub fn eye(&self) -> [f32; 3] {
        let back = self.orientation * Vec3::Z;
        let c = Vec3::from(self.center);
        (c + back * self.radius).to_array()
    }

    pub fn view_matrix(&self) -> [f32; 16] {
        // Camera world-space transform = T(eye) * R(orientation).
        // View matrix is its inverse. Using glam avoids the
        // `look_at(eye, center, up)` singularity at the poles.
        let eye = Vec3::from(self.eye());
        let camera_world = Mat4::from_rotation_translation(self.orientation, eye);
        camera_world.inverse().to_cols_array()
    }

    pub fn projection_matrix(&self, aspect: f32) -> [f32; 16] {
        match self.projection {
            Projection::Perspective => Mat4::perspective_rh_gl(
                self.fov_y,
                aspect.max(1e-6),
                self.near,
                self.far,
            )
            .to_cols_array(),
            Projection::Orthographic => {
                // Match the perspective frustum at the orbit center:
                // the visible vertical extent at radius `radius`
                // under FOV `fov_y` is `2 * radius * tan(fov_y/2)`.
                // Using that height keeps the model the same
                // on-screen size at the switch point so the user
                // doesn't lose context — and lets `ProjectionAnimation`
                // restore `(fov_y, radius)` at the end of its tween
                // without producing a visual pop.
                let half_h = (self.fov_y * 0.5).tan() * self.radius;
                let half_w = half_h * aspect;
                Mat4::orthographic_rh_gl(-half_w, half_w, -half_h, half_h, self.near, self.far)
                    .to_cols_array()
            }
        }
    }

    /// Reset the orbit pose to the default 3/4 view (used by the Home
    /// button).  Camera distance is preserved so the user doesn't lose
    /// their current zoom; only orientation snaps back.
    pub fn reset_view(&mut self) {
        let d = OrbitCamera::default();
        self.orientation = d.orientation;
        self.center = d.center;
        // Keep `radius`, `projection`, `orbit_mode` — Home is "reset
        // orientation", not "reset everything".  fit_all is the
        // distance-resetting operation.
    }

    /// Apply a screen-space drag (`dx`, `dy` in radians) to the
    /// camera under the active [`OrbitMode`].
    ///
    /// `Turntable` mode yaws around world-up (+Y) and pitches around
    /// the camera's right axis, with a strict pitch clamp that keeps
    /// the horizon level — matches MatterCAD's `TurntableEnabled`
    /// branch.
    ///
    /// `Trackball` mode treats the drag vector as a tangent on a
    /// virtual sphere: the rotation axis is in the screen plane,
    /// perpendicular to the drag, and the rotation magnitude is the
    /// drag length. No pitch clamp — the camera can tumble freely
    /// over the poles. Direct port of MatterCAD's
    /// `TrackBallController.GetRotationForMove` reduced for a
    /// fixed-distance orbit camera.
    pub fn orbit_drag(&mut self, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        match self.orbit_mode {
            OrbitMode::Turntable => self.turntable_drag(dx, dy),
            OrbitMode::Trackball => self.trackball_drag(dx, dy),
        }
    }

    /// Add `(d_az, d_el)` to the camera orientation in the turntable
    /// convention. Kept for backward compatibility with keyboard
    /// shortcuts; HUD drags go through `orbit_drag` instead.
    pub fn orbit(&mut self, d_az: f32, d_el: f32) {
        self.turntable_drag(d_az, d_el);
    }

    fn turntable_drag(&mut self, yaw_angle: f32, pitch_angle: f32) {
        // Yaw around world-up (+Z) — the rotation lives in world
        // frame, so it pre-multiplies the orientation. Drag-right
        // (+dx delivered as a negative yaw by the caller, since the
        // HUD negates screen-dx) makes the camera orbit
        // counter-clockwise around world-Z. Matches MatterCAD's
        // `TrackballTumbleWidgetExtended.DoRotateAroundOrigin` which
        // builds yaw around `Vector3.UnitZ.Transform(world.RotationMatrix)`
        // (i.e. world up).
        if yaw_angle != 0.0 {
            self.orientation = (Quat::from_rotation_z(yaw_angle) * self.orientation).normalize();
        }
        if pitch_angle == 0.0 {
            return;
        }
        // Pitch around camera-local right axis. Clamp so the camera
        // never tips past horizontal — this is what keeps the horizon
        // level in turntable mode. We clamp based on the asin-of-Z
        // component of the camera back vector (the camera's
        // elevation above the bed: back.z = sin(elevation)).
        let back = self.orientation * Vec3::Z;
        let current_pitch = back.z.clamp(-1.0, 1.0).asin();
        let limit = PI * 0.49;
        let target_pitch = (current_pitch + pitch_angle).clamp(-limit, limit);
        let applied = target_pitch - current_pitch;
        if applied == 0.0 {
            return;
        }
        let cam_right = self.orientation * Vec3::X;
        let pitch = Quat::from_axis_angle(cam_right, applied);
        self.orientation = (pitch * self.orientation).normalize();
    }

    fn trackball_drag(&mut self, dx: f32, dy: f32) {
        let length = (dx * dx + dy * dy).sqrt();
        if length < 1e-6 {
            return;
        }
        // Drag direction in CAMERA screen-plane coords:
        //   axis_screen = perpendicular to (dx, dy, 0) in the screen
        //                  plane = normalize((-dy, dx, 0)).
        // Convert to world by mixing the camera's right / up axes.
        let axis_x = -dy / length; // camera-local X component
        let axis_y = dx / length;  // camera-local Y component
        let cam_right = self.orientation * Vec3::X;
        let cam_up = self.orientation * Vec3::Y;
        let axis_world = (cam_right * axis_x + cam_up * axis_y).normalize_or_zero();
        if axis_world == Vec3::ZERO {
            return;
        }
        let q = Quat::from_axis_angle(axis_world, length);
        // World-frame rotation pre-multiplies orientation.
        self.orientation = (q * self.orientation).normalize();
    }

    /// Pan in screen-aligned axes — drag-to-pan semantics.
    ///
    /// `dx` / `dy` are cursor deltas in agg-gui's Y-up logical pixel space
    /// (so a downward drag has *negative* `dy`). The camera's `center`
    /// shifts opposite to the drag so the world point under the cursor
    /// stays roughly under it: drag right → world appears to follow the
    /// finger right (camera looks left-of-center).
    pub fn pan(&mut self, dx: f32, dy: f32) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        let (right, up, _fwd) = self.basis();
        let right = Vec3::from(right);
        let up = Vec3::from(up);
        let c = Vec3::from(self.center);
        self.center = (c - right * dx - up * dy).to_array();
    }

    pub fn zoom(&mut self, factor: f32) {
        if !factor.is_finite() {
            return;
        }
        self.radius = (self.radius * factor).clamp(0.05, 10_000.0);
    }

    pub fn fit_to_bounds(&mut self, min: [f32; 3], max: [f32; 3]) {
        self.center = [
            0.5 * (min[0] + max[0]),
            0.5 * (min[1] + max[1]),
            0.5 * (min[2] + max[2]),
        ];
        let dx = max[0] - min[0];
        let dy = max[1] - min[1];
        let dz = max[2] - min[2];
        let extent = dx.max(dy).max(dz).max(0.1);
        // Place the camera so the bounding sphere fits comfortably inside
        // the viewport with generous headroom. The 1.4 multiplier keeps the
        // model occupying ~60-70% of the viewport extent, matching the
        // NodeDesigner reference layout.
        let half_fov = self.fov_y * 0.5;
        let dist = (extent * 1.4) / half_fov.tan().max(1e-3);
        self.radius = dist.max(0.1);
    }

    /// Right, up, forward unit vectors in world space.
    pub fn basis(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let right = (self.orientation * Vec3::X).to_array();
        let up = (self.orientation * Vec3::Y).to_array();
        // Camera looks down its local -Z; forward in world = -Z mapped through orientation.
        let fwd = (self.orientation * Vec3::NEG_Z).to_array();
        (right, up, fwd)
    }
}

impl OrbitCamera {
    /// Build a world-space ray from a cursor position inside the viewport
    /// widget.
    ///
    /// `cursor_local` is in widget-local pixels with **origin at top-left**
    /// (typical screen convention used by event coordinates). `viewport_size`
    /// is the widget's pixel size. Returns `(origin, direction)` — both in
    /// world space, with `direction` unit-length.
    ///
    /// Implementation: convert the cursor to NDC `[-1, 1]` (with the Y axis
    /// flipped to match the standard top-down screen → bottom-up clip-space
    /// convention), unproject through the inverse view-projection at two
    /// depths, and take the difference as the ray direction.
    pub fn screen_to_ray(
        &self,
        cursor_local: (f64, f64),
        viewport_size: (f64, f64),
    ) -> ([f32; 3], [f32; 3]) {
        let (cx, cy) = cursor_local;
        let (vw, vh) = viewport_size;
        let ndc_x = ((cx / vw.max(1.0)) * 2.0 - 1.0) as f32;
        // Cursor is top-down (origin top-left). NDC's Y axis points up, so
        // flip.
        let ndc_y = (1.0 - (cy / vh.max(1.0)) * 2.0) as f32;
        let aspect = (vw / vh.max(1.0)) as f32;
        let view = Mat4::from_cols_array(&self.view_matrix());
        let proj = Mat4::from_cols_array(&self.projection_matrix(aspect));
        let inv = (proj * view).inverse();

        // Unproject near (z = -1) and far (z = +1) NDC points into world.
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, -1.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
        let dir = (far - near).normalize_or_zero();
        (near.to_array(), dir.to_array())
    }
}

/// Invert a column-major 4x4 matrix.  Public so callers crossing the
/// wgpu boundary (and a few tests) can keep using `[f32; 16]`.
pub fn inverse4(m: &[f32; 16]) -> [f32; 16] {
    Mat4::from_cols_array(m).inverse().to_cols_array()
}

/// Multiply a column-major 4x4 matrix by a 3D point with implicit w = 1.
/// Returns the homogeneous (x, y, z, w) — caller can perform perspective
/// divide.
pub fn transform_point4(m: &[f32; 16], p: [f32; 3]) -> [f32; 4] {
    let v = Mat4::from_cols_array(m) * glam::Vec4::new(p[0], p[1], p[2], 1.0);
    [v.x, v.y, v.z, v.w]
}

/// Multiply two column-major 4x4 matrices.
pub fn mul4(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    (Mat4::from_cols_array(a) * Mat4::from_cols_array(b)).to_cols_array()
}

/// Build a camera-to-world `Quat` whose +Z axis (eye-from-center
/// direction) matches `view_direction`. The chosen up-hint is world
/// +Z for non-vertical views (so the camera horizon stays level
/// with the bed plane). At the singular Top / Bottom orientations
/// the world-up axis is parallel to `back`, so we fall back to a
/// Y-aligned hint chosen to put camera-right on world +X — that
/// matches MatterCAD's Top view where world +X reads as screen-right
/// when looking down at the bed.
///
/// Used by the tumble-cube click-to-orient logic which produces a
/// face normal and wants a quaternion target for `OrientAnimation`.
pub fn orientation_for_view_direction(view_direction: [f32; 3]) -> Quat {
    let back = Vec3::from(view_direction).normalize_or_zero();
    if back == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    // Pick an up-hint that produces a non-degenerate basis.
    let up_hint = if back.z > 0.99 {
        // Top: camera above the bed looking down (+Z). World-up is
        // parallel to back, so use +Y as the up hint — the cross
        // product `up_hint × back = (+Y) × (+Z) = +X` puts world
        // +X on screen-right.
        Vec3::Y
    } else if back.z < -0.99 {
        // Bottom: camera below the bed looking up. Use -Y so the
        // cross `(-Y) × (-Z) = +X` keeps world +X on screen-right.
        Vec3::NEG_Y
    } else {
        Vec3::Z
    };
    let right = up_hint.cross(back).normalize_or_zero();
    if right == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    let up = back.cross(right).normalize();
    // Columns of the rotation matrix are the camera-frame basis
    // vectors expressed in world space: (right, up, back).
    let m = Mat3::from_cols(right, up, back);
    Quat::from_mat3(&m)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// Turntable mode keeps the horizon level in the new Z-up world:
    /// after a long sustained pitch drag, the back vector's +Z
    /// component must saturate (not flip the camera upside down).
    #[test]
    fn turntable_yaw_clamp_keeps_horizon_level() {
        let mut c = OrbitCamera::default();
        c.orbit_mode = OrbitMode::Turntable;
        c.orientation = Quat::IDENTITY;
        // Drive pitch hard past the pole.
        for _ in 0..200 {
            c.orbit_drag(0.0, 0.1);
        }
        let back = c.orientation * Vec3::Z;
        assert!(
            back.z <= (PI * 0.49).sin() + 1e-3,
            "turntable pitch should clamp; back.z = {}",
            back.z
        );
    }

    #[test]
    fn trackball_can_tumble_past_the_pole() {
        let mut c = OrbitCamera::default();
        c.orbit_mode = OrbitMode::Trackball;
        // Start from Front view so a vertical drag tilts the camera
        // through the world-Z pole. Camera-local +Y is screen-up
        // (initially aligned with world +Z); after a 180° drag its
        // world-Z component flips below zero (camera "up" now
        // points into the bed).
        c.orientation = orientation_for_view_direction([0.0, -1.0, 0.0]);
        c.orbit_drag(0.0, PI);
        let cam_up = c.orientation * Vec3::Y;
        assert!(
            cam_up.z < 0.0,
            "trackball should be able to tumble past the pole; up = {cam_up:?}"
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

        // The half-way orientation should be on the short geodesic
        // between start and target — angular distance to TARGET at
        // t=0.5 should be at most ~half the short-arc length (which
        // is 0.2 rad), i.e. < 0.15 rad. The long way would put us
        // ~3π/2 from target — orders of magnitude worse.
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
}
