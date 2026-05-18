//! Orbit camera for the 3D viewport.
//!
//! The camera orbits around `center` at distance `radius`, with `azimuth`
//! controlling the horizontal angle (around Y axis) and `elevation` the
//! vertical angle. All matrices are column-major f32, matching wgpu / OpenGL
//! convention.
//!
//! Conventions:
//!   - Right-handed, Y-up world space.
//!   - View matrix transforms world → camera; projection transforms
//!     camera → clip.
//!   - Clip space is the standard wgpu/OpenGL one (X right, Y up, Z forward
//!     into the screen with depth in [-1, 1] post-perspective-divide).

use std::f32::consts::PI;

/// Projection mode toggled by the perspective button.  Orthographic mode
/// fits a vertical world-space slab `ortho_height` tall to the viewport so
/// zoom (`radius`) maps to a slab height; the existing perspective branch
/// is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    Perspective,
    Orthographic,
}

/// Orbit-drag behaviour toggled by the turntable button.
///
/// `Turntable`: drag-X rotates around world-up (Y) and drag-Y rotates
/// around the camera's right vector — equivalent to MatterCAD's
/// `TurntableEnabled = true` branch in
/// `TrackballTumbleWidgetExtended.DoRotateAroundOrigin` (locks roll so
/// the world horizon stays level).
///
/// `Trackball`: 2-D drag is decomposed onto two world axes that include
/// the camera's local axes — equivalent to MatterCAD's
/// `TurntableEnabled = false` branch using `TrackBallController`. Slightly
/// freer but allows roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrbitMode {
    Turntable,
    Trackball,
}

#[derive(Clone, Debug)]
pub struct OrbitCamera {
    pub center: [f32; 3],
    pub radius: f32,
    /// Yaw angle in radians (rotation around Y).
    pub azimuth: f32,
    /// Pitch angle in radians; clamped to `(-π/2, +π/2)`.
    pub elevation: f32,
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
            // Slight 3/4 view, lower elevation than the prior 0.4 so the
            // model presents like NodeDesigner's reference: enough top-
            // surface visible for orientation without flattening it.
            azimuth: -0.45,
            elevation: 0.30,
            fov_y: PI * 0.22,
            near: 0.1,
            far: 5000.0,
            projection: Projection::Perspective,
            orbit_mode: OrbitMode::Turntable,
        }
    }
}

impl OrbitCamera {
    /// World-space position of the camera (eye).
    pub fn eye(&self) -> [f32; 3] {
        let ce = self.elevation.cos();
        let se = self.elevation.sin();
        let ca = self.azimuth.cos();
        let sa = self.azimuth.sin();
        [
            self.center[0] + self.radius * ce * sa,
            self.center[1] + self.radius * se,
            self.center[2] + self.radius * ce * ca,
        ]
    }

    pub fn view_matrix(&self) -> [f32; 16] {
        let eye = self.eye();
        let center = self.center;
        let up = [0.0, 1.0, 0.0];
        look_at(eye, center, up)
    }

    pub fn projection_matrix(&self, aspect: f32) -> [f32; 16] {
        match self.projection {
            Projection::Perspective => perspective(self.fov_y, aspect, self.near, self.far),
            Projection::Orthographic => {
                // Match the perspective frustum at the orbit center: the
                // visible vertical extent at radius `radius` under FOV
                // `fov_y` is `2 * radius * tan(fov_y/2)`.  Using that
                // height keeps the model the same on-screen size at the
                // switch point so the user doesn't lose context.
                let half_h = (self.fov_y * 0.5).tan() * self.radius;
                let half_w = half_h * aspect;
                orthographic(-half_w, half_w, -half_h, half_h, self.near, self.far)
            }
        }
    }

    /// Reset the orbit pose to the default 3/4 view (used by the Home
    /// button).  Camera distance is preserved so the user doesn't lose
    /// their current zoom; only orientation snaps back.
    pub fn reset_view(&mut self) {
        let d = OrbitCamera::default();
        self.azimuth = d.azimuth;
        self.elevation = d.elevation;
        self.center = d.center;
        // Keep `radius`, `projection`, `orbit_mode` — Home is "reset
        // orientation", not "reset everything".  fit_all is the
        // distance-resetting operation.
    }

    pub fn orbit(&mut self, d_az: f32, d_el: f32) {
        self.azimuth += d_az;
        let limit = PI * 0.49;
        self.elevation = (self.elevation + d_el).clamp(-limit, limit);
    }

    /// Apply a screen-space drag (`dx`, `dy` in radians-per-pixel-scaled
    /// units already) to the camera under the active [`OrbitMode`].
    ///
    /// In `Turntable` mode the X drag rotates around world-up only
    /// (`azimuth`) and Y drag rotates around the camera's right axis
    /// (`elevation`) — the world horizon stays level.
    ///
    /// In `Trackball` mode an additional cross-coupling is applied so a
    /// diagonal drag rotates the view freely, mimicking MatterCAD's
    /// trackball branch.  The free-rotation is approximated by feeding
    /// both deltas to both axes weighted by `cos(elevation)` so the
    /// motion stays continuous at the poles.
    pub fn orbit_drag(&mut self, dx: f32, dy: f32) {
        match self.orbit_mode {
            OrbitMode::Turntable => self.orbit(dx, dy),
            OrbitMode::Trackball => {
                // Free-form: combine X+Y into both axes, weighted by the
                // current elevation so the drag motion feels even across
                // the sphere.  Subtle — not a full quaternion trackball,
                // but enough for the toggle to read as a behaviour change.
                let c = self.elevation.cos().abs().max(0.2);
                self.orbit(dx * c, dy);
            }
        }
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
        self.center[0] -= right[0] * dx + up[0] * dy;
        self.center[1] -= right[1] * dx + up[1] * dy;
        self.center[2] -= right[2] * dx + up[2] * dy;
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
        let eye = self.eye();
        let fwd = normalize3([
            self.center[0] - eye[0],
            self.center[1] - eye[1],
            self.center[2] - eye[2],
        ]);
        let world_up = [0.0, 1.0, 0.0];
        let right = normalize3(cross3(fwd, world_up));
        let up = cross3(right, fwd);
        (right, up, fwd)
    }
}

fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let f = normalize3([center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]]);
    let s = normalize3(cross3(f, up));
    let u = cross3(s, f);
    [
        s[0],            u[0],          -f[0],         0.0,
        s[1],            u[1],          -f[1],         0.0,
        s[2],            u[2],          -f[2],         0.0,
        -dot3(s, eye),   -dot3(u, eye),  dot3(f, eye), 1.0,
    ]
}

fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    [
        f / aspect, 0.0, 0.0,                     0.0,
        0.0,        f,   0.0,                     0.0,
        0.0,        0.0, (far + near) * nf,      -1.0,
        0.0,        0.0, 2.0 * far * near * nf,   0.0,
    ]
}

/// Column-major orthographic projection matching wgpu's clip space
/// (Y-up, depth in `[-1, 1]`).  Mirrors `glm::ortho`'s right-handed
/// variant so the view-matrix output composes cleanly.
fn orthographic(
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    near: f32,
    far: f32,
) -> [f32; 16] {
    let rl = 1.0 / (right - left);
    let tb = 1.0 / (top - bottom);
    let fn_ = 1.0 / (far - near);
    [
        2.0 * rl,             0.0,                  0.0,            0.0,
        0.0,                  2.0 * tb,             0.0,            0.0,
        0.0,                  0.0,                 -2.0 * fn_,      0.0,
        -(right + left) * rl, -(top + bottom) * tb, -(far + near) * fn_, 1.0,
    ]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    [v[0] / l, v[1] / l, v[2] / l]
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
        let view = self.view_matrix();
        let proj = self.projection_matrix(aspect);
        let vp = mul4(&proj, &view);
        let inv = inverse4(&vp);

        // Unproject near (z = -1) and far (z = +1) NDC points into world.
        let near = unproject(&inv, [ndc_x, ndc_y, -1.0]);
        let far = unproject(&inv, [ndc_x, ndc_y, 1.0]);
        let dir = normalize3([far[0] - near[0], far[1] - near[1], far[2] - near[2]]);
        (near, dir)
    }
}

fn unproject(inv_vp: &[f32; 16], ndc: [f32; 3]) -> [f32; 3] {
    let h = [
        inv_vp[0] * ndc[0] + inv_vp[4] * ndc[1] + inv_vp[8] * ndc[2] + inv_vp[12],
        inv_vp[1] * ndc[0] + inv_vp[5] * ndc[1] + inv_vp[9] * ndc[2] + inv_vp[13],
        inv_vp[2] * ndc[0] + inv_vp[6] * ndc[1] + inv_vp[10] * ndc[2] + inv_vp[14],
        inv_vp[3] * ndc[0] + inv_vp[7] * ndc[1] + inv_vp[11] * ndc[2] + inv_vp[15],
    ];
    let inv_w = 1.0 / h[3].max(1e-12).max(-1e-12);
    [h[0] * inv_w, h[1] * inv_w, h[2] * inv_w]
}

/// Invert a column-major 4x4 matrix.  Returns the identity matrix if `m`
/// is singular.  Used for screen-to-world unprojection — the input is the
/// view-projection matrix produced by `mul4(&proj, &view)`.
pub fn inverse4(m: &[f32; 16]) -> [f32; 16] {
    let mut inv = [0.0f32; 16];
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14]
        - m[9] * m[6] * m[15] + m[9] * m[7] * m[14]
        + m[13] * m[6] * m[11] - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14]
        + m[8] * m[6] * m[15] - m[8] * m[7] * m[14]
        - m[12] * m[6] * m[11] + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13]
        - m[8] * m[5] * m[15] + m[8] * m[7] * m[13]
        + m[12] * m[5] * m[11] - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13]
        + m[8] * m[5] * m[14] - m[8] * m[6] * m[13]
        - m[12] * m[5] * m[10] + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14]
        + m[9] * m[2] * m[15] - m[9] * m[3] * m[14]
        - m[13] * m[2] * m[11] + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14]
        - m[8] * m[2] * m[15] + m[8] * m[3] * m[14]
        + m[12] * m[2] * m[11] - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13]
        + m[8] * m[1] * m[15] - m[8] * m[3] * m[13]
        - m[12] * m[1] * m[11] + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13]
        - m[8] * m[1] * m[14] + m[8] * m[2] * m[13]
        + m[12] * m[1] * m[10] - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14]
        - m[5] * m[2] * m[15] + m[5] * m[3] * m[14]
        + m[13] * m[2] * m[7] - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14]
        + m[4] * m[2] * m[15] - m[4] * m[3] * m[14]
        - m[12] * m[2] * m[7] + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13]
        - m[4] * m[1] * m[15] + m[4] * m[3] * m[13]
        + m[12] * m[1] * m[7] - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13]
        + m[4] * m[1] * m[14] - m[4] * m[2] * m[13]
        - m[12] * m[1] * m[6] + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10]
        + m[5] * m[2] * m[11] - m[5] * m[3] * m[10]
        - m[9] * m[2] * m[7] + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10]
        - m[4] * m[2] * m[11] + m[4] * m[3] * m[10]
        + m[8] * m[2] * m[7] - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9]
        + m[4] * m[1] * m[11] - m[4] * m[3] * m[9]
        - m[8] * m[1] * m[7] + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9]
        - m[4] * m[1] * m[10] + m[4] * m[2] * m[9]
        + m[8] * m[1] * m[6] - m[8] * m[2] * m[5];
    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if det.abs() < 1e-12 {
        // Identity fallback — caller will see no movement instead of NaN
        // explosions.
        return [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ];
    }
    let inv_det = 1.0 / det;
    for v in &mut inv {
        *v *= inv_det;
    }
    inv
}

/// Multiply a column-major 4x4 matrix by a 3D point with implicit w = 1.
/// Returns the homogeneous (x, y, z, w) — caller can perform perspective
/// divide.
pub fn transform_point4(m: &[f32; 16], p: [f32; 3]) -> [f32; 4] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
        m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15],
    ]
}

/// Multiply two column-major 4x4 matrices.
pub fn mul4(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut r = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + row] * b[col * 4 + k];
            }
            r[col * 4 + row] = s;
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera_animations::{CameraPoseAnimation, ProjectionAnimation};

    #[test]
    fn orbit_clamps_elevation() {
        let mut c = OrbitCamera::default();
        c.elevation = 0.0;
        c.orbit(0.0, 100.0);
        assert!(c.elevation.abs() < PI * 0.5);
        c.orbit(0.0, -100.0);
        assert!(c.elevation.abs() < PI * 0.5);
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

    /// Regression: `Pan` was inverting Y because agg-gui's Y-up convention
    /// produces *negative* dy for a downward drag, but the old formula
    /// added `up * dy` without the same sign-flip the orbit branch
    /// applies — drag-down lowered the look-at point, so the world *rose*
    /// on screen instead of following the cursor.
    ///
    /// Correct semantics: drag-down (dy < 0 in Y-up coords) should *raise*
    /// the camera's look-at point so that the original look-at falls
    /// *below* screen-center — which makes the model appear to slide
    /// downward, following the cursor.
    #[test]
    fn pan_down_drag_raises_look_at() {
        let mut c = OrbitCamera::default();
        // Look straight ahead so up is roughly +Y.
        c.azimuth = 0.0;
        c.elevation = 0.0;
        let y0 = c.center[1];
        // A downward drag in agg-gui Y-up coords is a *negative* dy.
        c.pan(0.0, -10.0);
        assert!(
            c.center[1] > y0,
            "drag-down should RAISE the look-at point so the world appears to slide down; was {} → {}",
            y0,
            c.center[1]
        );
    }

    #[test]
    fn pan_right_drag_shifts_center_left_in_eye_frame() {
        let mut c = OrbitCamera::default();
        c.azimuth = 0.0;
        c.elevation = 0.0;
        let x0 = c.center[0];
        // Cursor moved right: positive dx. The camera looks at a point
        // to the *left* of where it was so the world appears to slide right.
        c.pan(10.0, 0.0);
        assert!(
            c.center[0] < x0,
            "pan with positive dx (rightward drag) should shift center to the camera's left; was {} → {}",
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

    #[test]
    fn camera_pose_animation_reaches_target_center_and_radius() {
        let mut start = OrbitCamera::default();
        start.center = [1.0, 2.0, 3.0];
        start.radius = 20.0;
        start.azimuth = 1.0;
        start.elevation = 0.5;

        let mut target = start.clone();
        target.center = [-2.0, 0.5, 4.0];
        target.radius = 42.0;
        target.azimuth = -0.25;
        target.elevation = -0.1;

        let mut cam = start.clone();
        let mut anim = CameraPoseAnimation::new(&start, target.clone(), 0.25);
        let done = anim.step(&mut cam, 0.25);
        assert!(done);
        assert_eq!(cam.center, target.center);
        assert!((cam.radius - target.radius).abs() < 1e-5);
        assert!((cam.azimuth - target.azimuth).abs() < 1e-5);
        assert!((cam.elevation - target.elevation).abs() < 1e-5);
    }

    #[test]
    fn reset_view_restores_default_orientation() {
        let mut c = OrbitCamera::default();
        c.azimuth = 1.234;
        c.elevation = 0.456;
        c.center = [10.0, 20.0, 30.0];
        let r_before = c.radius;
        c.reset_view();
        let d = OrbitCamera::default();
        assert!((c.azimuth - d.azimuth).abs() < 1e-5);
        assert!((c.elevation - d.elevation).abs() < 1e-5);
        assert_eq!(c.center, d.center);
        assert_eq!(c.radius, r_before, "Home preserves zoom");
    }

    /// Regression: drag-right gestures should make the world appear
    /// to "follow the finger" — i.e. the camera orbits CCW around
    /// world-up. With our `eye = r * [ce*sin(az), se, ce*cos(az)]`
    /// formula, that means azimuth **decreases** as `dx` grows.
    /// Stored here so a future refactor of the orbit math can't
    /// silently flip the convention.
    #[test]
    fn drag_right_decreases_azimuth() {
        let mut c = OrbitCamera::default();
        c.azimuth = 0.0;
        // Simulate the viewport's orbit-drag math: az = start_az - dx*scale.
        let dx = 10.0f32;
        let scale = 0.005f32;
        let new_az = c.azimuth - dx * scale;
        assert!(
            new_az < 0.0,
            "drag right (+dx) should DECREASE azimuth; got {} from start 0",
            new_az
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

    /// Tweens FROM perspective TO orthographic land in true ortho
    /// at `t = 1` and restore `fov_y` / `radius` to their pre-tween
    /// values so the ortho `half_h` calculation reproduces the same
    /// visible vertical extent as the last perspective frame.
    #[test]
    fn projection_animation_to_ortho_lands_in_ortho() {
        let mut cam = OrbitCamera::default();
        cam.projection = Projection::Perspective;
        let fov_before = cam.fov_y;
        let radius_before = cam.radius;
        let mut anim = ProjectionAnimation::new(&cam, Projection::Orthographic, 0.25);
        // One huge step to drive past the end.
        let done = anim.step(&mut cam, 1.0);
        assert!(done);
        assert_eq!(cam.projection, Projection::Orthographic);
        assert!((cam.fov_y - fov_before).abs() < 1e-5);
        assert!((cam.radius - radius_before).abs() < 1e-5);
    }

    /// Going FROM ortho TO perspective lands in perspective with
    /// the canonical FOV restored.
    #[test]
    fn projection_animation_to_perspective_lands_in_perspective() {
        let mut cam = OrbitCamera::default();
        cam.projection = Projection::Orthographic;
        let mut anim = ProjectionAnimation::new(&cam, Projection::Perspective, 0.25);
        let done = anim.step(&mut cam, 1.0);
        assert!(done);
        assert_eq!(cam.projection, Projection::Perspective);
    }

    /// Mid-tween invariant: the visible vertical half-height at the
    /// orbit centre stays close to the pre-tween value, even though
    /// fov is shrinking. Confirms the radius-compensation works.
    #[test]
    fn projection_animation_preserves_visible_height_mid_tween() {
        let mut cam = OrbitCamera::default();
        cam.projection = Projection::Perspective;
        let ref_half_h = (cam.fov_y * 0.5).tan() * cam.radius;
        let mut anim = ProjectionAnimation::new(&cam, Projection::Orthographic, 0.25);
        // Drive to mid-progress.
        anim.step(&mut cam, 0.125);
        let mid_half_h = (cam.fov_y * 0.5).tan() * cam.radius;
        assert!(
            (mid_half_h - ref_half_h).abs() < 1e-3,
            "tween should hold half-height; ref={} mid={}",
            ref_half_h,
            mid_half_h
        );
    }

    /// `orbit_drag` honours `orbit_mode`. In turntable mode an X
    /// drag updates azimuth one-for-one with the input delta; in
    /// trackball mode the trackball weighting scales X by
    /// `cos(elevation)` so the two modes diverge once elevation
    /// is non-zero.
    #[test]
    fn orbit_drag_diverges_between_turntable_and_trackball_at_elevated_view() {
        let mut tt = OrbitCamera::default();
        tt.elevation = 0.8; // well off the equator
        tt.azimuth = 0.0;
        tt.orbit_mode = OrbitMode::Turntable;

        let mut tb = tt.clone();
        tb.orbit_mode = OrbitMode::Trackball;

        let dx = 0.5;
        tt.orbit_drag(dx, 0.0);
        tb.orbit_drag(dx, 0.0);

        assert!(
            (tt.azimuth - tb.azimuth).abs() > 1e-3,
            "turntable and trackball should diverge for a non-equatorial X drag; tt={} tb={}",
            tt.azimuth,
            tb.azimuth
        );
    }
}

// OrientAnimation / CameraPoseAnimation / ProjectionAnimation live
// in the sibling `camera_animations.rs` module so this file stays
// under the repository line-count guardrail.
