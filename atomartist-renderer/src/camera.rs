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

    /// Pan in screen-aligned axes. `dx` moves the camera along its right
    /// vector, `dy` along its up vector.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let (right, up, _fwd) = self.basis();
        self.center[0] += right[0] * dx + up[0] * dy;
        self.center[1] += right[1] * dx + up[1] * dy;
        self.center[2] += right[2] * dx + up[2] * dy;
    }

    pub fn zoom(&mut self, factor: f32) {
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
}
