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
/// `Turntable`: drag-X yaws around world-up (+Z), drag-Y pitches
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

    /// World units one screen pixel covers at the given world point.
    /// Camera-distance-proportional gizmos use this so the on-screen
    /// size stays constant regardless of zoom — MatterCAD's
    /// `GetWorldUnitsPerScreenPixelAtPosition` pattern.
    ///
    /// `viewport_height` is the framebuffer pixel height (the camera
    /// only needs vertical extent since the projection uses
    /// `fov_y`).
    ///
    /// * Perspective: distance from eye to `world_pos` along the
    ///   view's forward axis × `tan(fov_y/2)` × `2 / viewport_height`.
    /// * Orthographic: depth doesn't matter — the visible vertical
    ///   extent is `2 * radius * tan(fov_y/2)` (matched at radius in
    ///   `projection_matrix`).
    pub fn world_units_per_pixel_at(&self, world_pos: [f32; 3], viewport_height: f32) -> f32 {
        if viewport_height <= 0.5 {
            return 1.0;
        }
        let half_h = match self.projection {
            Projection::Perspective => {
                let forward = (self.orientation * Vec3::Z).normalize_or_zero(); // points TOWARD eye
                let eye = Vec3::from(self.eye());
                let depth = (eye - Vec3::from(world_pos)).dot(forward).abs().max(1e-3);
                depth * (self.fov_y * 0.5).tan()
            }
            Projection::Orthographic => self.radius * (self.fov_y * 0.5).tan(),
        };
        (half_h * 2.0) / viewport_height
    }

    pub fn projection_matrix(&self, aspect: f32) -> [f32; 16] {
        // Critical: use the **wgpu / Vulkan / Metal**-style matrices
        // (no `_gl` suffix) so NDC z lands in [0, 1] rather than
        // OpenGL's [-1, 1]. wgpu clips anything outside [0, 1],
        // which is invisible under perspective (the model lands at
        // NDC z ≈ 0.997 — well above 0) but catastrophic under
        // orthographic where depth is linear and a 60-unit-away
        // model maps to NDC z ≈ -0.976 → entire scene clipped
        // (the "white screen on ortho" symptom). `screen_to_ray`
        // below matches: it unprojects NDC z = 0 (near) and z = 1
        // (far) instead of the GL-style ±1.
        match self.projection {
            Projection::Perspective => Mat4::perspective_rh(
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
                Mat4::orthographic_rh(-half_w, half_w, -half_h, half_h, self.near, self.far)
                    .to_cols_array()
            }
        }
    }

    /// Apply the **minimum-angle** rotation that aligns the
    /// camera-up vector with world +Z, then pre-multiply the
    /// orientation by it. After the call, screen-up corresponds
    /// to world +Z and turntable yaw (which goes around world +Z)
    /// feels natural again.
    ///
    /// Used by the Turntable HUD button when toggling back from
    /// Trackball: trackball rotation can leave the camera rolled
    /// or upside-down, so before turntable's yaw-around-+Z kicks
    /// in we re-align "up." Using `Quat::from_rotation_arc` gives
    /// the **smallest** possible quaternion rotation that does
    /// the alignment — the camera moves the minimum amount
    /// necessary to be turntable-friendly again, which is exactly
    /// what the user asked for. The full yaw-pitch decomposition
    /// approach (which always rebuilt orientation from scratch)
    /// changed the view far more than necessary when the camera
    /// was nearly aligned.
    pub fn snap_to_turntable_alignment(&mut self) {
        let cam_up = (self.orientation * Vec3::Y).normalize_or_zero();
        if cam_up == Vec3::ZERO {
            return;
        }
        // Already aligned (within ~0.06°) — no-op.
        if (cam_up - Vec3::Z).length() < 1e-3 {
            return;
        }
        let snap = Quat::from_rotation_arc(cam_up, Vec3::Z);
        self.orientation = (snap * self.orientation).normalize();
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
    /// camera under the active [`OrbitMode`]. Rotates around the
    /// existing orbit `center`. Use [`Self::orbit_drag_around`] to
    /// rotate around an arbitrary world pivot (MatterCAD's
    /// `world.RotateAroundPosition`).
    ///
    /// `Turntable` mode yaws around world-up (+Z) and pitches around
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
        let q = self.delta_quat_for_drag(dx, dy);
        if q != Quat::IDENTITY {
            self.orientation = (q * self.orientation).normalize();
        }
    }

    /// Rotate the camera around an arbitrary world-space pivot —
    /// port of MatterCAD's `world.RotateAroundPosition(pivot, q)`.
    /// Eye and orbit centre both swing around `pivot`, so the
    /// pivot's screen projection stays put while the rest of the
    /// view orbits it. `radius` is invariant under rotation.
    ///
    /// Crucially, *nothing* changes on the first frame when `dx ==
    /// dy == 0` — that's what keeps the scene from jumping the
    /// moment a rotate drag starts, even if the cursor is far from
    /// the existing orbit centre.
    pub fn orbit_drag_around(&mut self, pivot: [f32; 3], dx: f32, dy: f32) {
        let q = self.delta_quat_for_drag(dx, dy);
        if q == Quat::IDENTITY {
            return;
        }
        let p = Vec3::from(pivot);
        let c = Vec3::from(self.center);
        let new_center = p + q * (c - p);
        self.center = new_center.to_array();
        self.orientation = (q * self.orientation).normalize();
    }

    /// Add `(d_az, d_el)` to the camera orientation in the turntable
    /// convention. Kept for backward compatibility with keyboard
    /// shortcuts; HUD drags go through `orbit_drag` instead.
    pub fn orbit(&mut self, d_az: f32, d_el: f32) {
        self.orbit_drag(d_az, d_el);
    }

    /// Build the delta quaternion the active `orbit_mode` would
    /// apply for a drag of `(dx, dy)` radians. Pulled out so both
    /// `orbit_drag` (rotate around centre) and
    /// `orbit_drag_around` (rotate around an external pivot) can
    /// share the same per-mode math, including the turntable pitch
    /// clamp.
    fn delta_quat_for_drag(&self, dx: f32, dy: f32) -> Quat {
        if dx == 0.0 && dy == 0.0 {
            return Quat::IDENTITY;
        }
        match self.orbit_mode {
            OrbitMode::Turntable => self.turntable_delta_quat(dx, dy),
            OrbitMode::Trackball => self.trackball_delta_quat(dx, dy),
        }
    }

    /// Yaw around world-up (+Z) then pitch around the camera's
    /// right axis AFTER the yaw. The pitch is clamped so the
    /// camera can't tip past the world-Z pole — matches MatterCAD's
    /// `TurntableEnabled = true` branch in `DoRotateAroundOrigin`.
    ///
    /// ## Sign convention (important)
    ///
    /// `pitch_angle > 0` means the user dragged the cursor UP and
    /// expects the camera to TILT DOWN (eye moves below the bed
    /// plane, looking up at the model — the "see-from-below" feel
    /// of MatterCAD turntable drag). Geometrically:
    ///
    ///   * The pitch rotation is `Quat::from_axis_angle(cam_right,
    ///     applied)` around the **horizontal** post-yaw cam-right
    ///     axis. Rodrigues with `cam_right ⊥ back` and the right-
    ///     hand rule gives `back.z' = back.z·cos(applied) -
    ///     |back_h|·sin(applied)`, so `applied > 0` **decreases**
    ///     `back.z` (camera elevation drops).
    ///   * Therefore `applied = current_elevation - new_elevation`.
    ///     We want `new_elevation = current_elevation - pitch_angle`,
    ///     so `applied = pitch_angle` (subject to the clamp below).
    ///
    /// The previous implementation had `target_pitch = (current +
    /// pitch_angle).clamp(...)` and `applied = target - current`,
    /// which treated `pitch_angle > 0` as "increase elevation" —
    /// the opposite of the rotation's true direction. The clamp
    /// then bounded the wrong side of the motion and the camera
    /// could freely spin past the poles into gimbal lock; tests
    /// missed it because they drove in the direction the math
    /// happened to agree with.
    fn turntable_delta_quat(&self, yaw_angle: f32, pitch_angle: f32) -> Quat {
        let mut q = Quat::IDENTITY;
        if yaw_angle != 0.0 {
            q = Quat::from_rotation_z(yaw_angle);
        }
        if pitch_angle == 0.0 {
            return q;
        }
        // Apply yaw first to find where camera-right ends up; the
        // pitch is around that post-yaw axis.
        let after_yaw = q * self.orientation;
        let back = after_yaw * Vec3::Z;
        // `current_elevation` is the angle `back` makes above the
        // XY plane (positive when the eye is above the bed).
        let current_elevation = back.z.clamp(-1.0, 1.0).asin();
        // Stop just shy of ±π/2 so the post-clamp orientation
        // never sits exactly at the pole (where the rotation axis
        // becomes ambiguous under floating-point error).
        let limit = PI * 0.499;
        let raw_target = current_elevation - pitch_angle;
        let target_elevation = raw_target.clamp(-limit, limit);
        let applied = current_elevation - target_elevation;
        if applied.abs() < 1e-6 {
            return q;
        }
        let cam_right = after_yaw * Vec3::X;
        Quat::from_axis_angle(cam_right, applied) * q
    }

    /// MatterCAD-style trackball: the drag vector picks a rotation
    /// axis perpendicular to it in the screen plane, with rotation
    /// magnitude equal to the drag length.
    ///
    /// Sign matches turntable: cursor-up (`dy > 0` once the HUD has
    /// pre-flipped screen Y) tips the camera DOWN so the user sees
    /// more of the model's underside. We achieve that by negating
    /// the rotation magnitude relative to the MatterCAD convention
    /// — MatterCAD rotates the **world**, we rotate the **camera**,
    /// so the rotation we apply is the inverse of the world
    /// rotation, hence the sign flip.
    fn trackball_delta_quat(&self, dx: f32, dy: f32) -> Quat {
        let length = (dx * dx + dy * dy).sqrt();
        if length < 1e-6 {
            return Quat::IDENTITY;
        }
        let axis_x = -dy / length;
        let axis_y = dx / length;
        let cam_right = self.orientation * Vec3::X;
        let cam_up = self.orientation * Vec3::Y;
        let axis_world = (cam_right * axis_x + cam_up * axis_y).normalize_or_zero();
        if axis_world == Vec3::ZERO {
            return Quat::IDENTITY;
        }
        // Negative magnitude → rotate the camera the opposite way
        // from MatterCAD's world rotation, matching the inverse
        // relationship and aligning the visible result with the
        // turntable convention above.
        Quat::from_axis_angle(axis_world, -length)
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

        // Unproject NDC z = 0 (near) and z = 1 (far) — wgpu / Vulkan
        // depth convention, matching the `_rh` (not `_rh_gl`)
        // projection matrices built by `projection_matrix`.
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
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

// Tests live in the sibling `camera_tests.rs` module so this file
// stays under the repository line-count guardrail.
#[cfg(test)]
#[path = "camera_tests.rs"]
mod camera_tests_extracted;

// Old inline tests removed; see `camera_tests.rs` for the full set.
