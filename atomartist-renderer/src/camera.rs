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
        perspective(self.fov_y, aspect, self.near, self.far)
    }

    pub fn orbit(&mut self, d_az: f32, d_el: f32) {
        self.azimuth += d_az;
        let limit = PI * 0.49;
        self.elevation = (self.elevation + d_el).clamp(-limit, limit);
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
}
