//! Camera tweening primitives — extracted from `camera.rs` so the
//! main camera module stays under the repository line-count
//! guardrail. All three structs here drive the same
//! `Animation::request_draw` redraw loop in `Viewport3dWidget` and
//! are stepped from the viewport's per-frame `tick_*_animation`
//! helpers.
//!
//! Orientation interpolation uses `Quat::slerp`, matching MatterCAD's
//! `Quaternion.Slerp(rotationStart, rotationEnd, t)` in
//! `TrackballTumbleWidgetExtended.AnimateRotation`. The old
//! `(azimuth, elevation)` Euler lerp had subtle wrong-way-around
//! glitches whenever the target azimuth crossed ±π; with `slerp` the
//! interpolation always takes the short path on the unit sphere
//! by construction.

use glam::{Quat, Vec3};

use super::camera::{OrbitCamera, Projection};

// ---------------------------------------------------------------------------
// Orientation animation — drives click-to-orient on the tumble cube and
// any other camera-snap operation. Stepping by elapsed seconds avoids the
// frame-rate-coupled "10 updates" approach MatterCAD used and produces
// identical motion regardless of refresh rate.
// ---------------------------------------------------------------------------

/// Smooth interpolation between two camera orientations.
///
/// Holds start and target quaternions and lerps the orbit centre
/// alongside; each `step` advances progress by `dt / duration` and
/// applies an eased blend via `Quat::slerp`, writing the result back
/// to the camera.  Callers ping `is_done` to know when the cube
/// widget should drop the animation handle.
#[derive(Clone, Debug)]
pub struct OrientAnimation {
    start_orientation: Quat,
    target_orientation: Quat,
    start_center: Vec3,
    target_center: Vec3,
    /// 0.0 → not started; 1.0 → finished.
    progress: f32,
    duration: f32,
}

/// Smooth interpolation between two complete orbit-camera poses.
/// Used by viewport chrome actions such as Home and Zoom-to-selection
/// so those transitions tween from the current transform instead of
/// jumping.
#[derive(Clone, Debug)]
pub struct CameraPoseAnimation {
    start: OrbitCamera,
    target: OrbitCamera,
    progress: f32,
    duration: f32,
}

impl CameraPoseAnimation {
    pub fn new(start: &OrbitCamera, target: OrbitCamera, duration: f32) -> Self {
        Self {
            start: start.clone(),
            target,
            progress: 0.0,
            duration: duration.max(1e-3),
        }
    }

    pub fn step(&mut self, camera: &mut OrbitCamera, dt: f32) -> bool {
        self.progress = (self.progress + dt / self.duration).min(1.0);
        let s = smoothstep01(self.progress);
        camera.center = lerp3(self.start.center, self.target.center, s);
        camera.radius = lerp(self.start.radius, self.target.radius, s);
        camera.orientation = self
            .start
            .orientation
            .slerp(self.target.orientation, s)
            .normalize();
        camera.fov_y = lerp(self.start.fov_y, self.target.fov_y, s);
        camera.near = lerp(self.start.near, self.target.near, s);
        camera.far = lerp(self.start.far, self.target.far, s);
        camera.projection = self.target.projection;
        camera.orbit_mode = self.target.orbit_mode;
        self.progress >= 1.0
    }
}

pub(super) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

pub(super) fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

/// Smooth interpolation between perspective and orthographic
/// projection. Direct port of MatterCAD's
/// `TrackballTumbleWidgetExtended.DoSwitchToProjectionMode` — the
/// animation shrinks the vertical FOV toward zero (toward ortho) or
/// grows it from zero (toward perspective), while simultaneously
/// pulling the camera back so the visible vertical extent at the
/// orbit centre stays constant on screen. Without the radius
/// compensation, foreshortening would shrink the model toward a dot
/// during the tween. Duration matches MatterCAD's
/// `Animation.Run(this, 0.25, 10, …)`.
///
/// At `progress = 1.0` the animation snaps the camera to a clean
/// final state:
///   - When targeting orthographic: `projection = Orthographic`,
///     `fov_y = ref_fov`, `radius = ref_radius`. The ortho
///     `projection_matrix` derives `half_h = tan(fov/2) * radius`,
///     so the final on-screen size matches the last perspective
///     frame.
///   - When targeting perspective: `projection = Perspective`,
///     `fov_y = ref_fov`, `radius = ref_radius`. Symmetric inverse.
#[derive(Clone, Debug)]
pub struct ProjectionAnimation {
    /// FOV at toggle time — restored at the end so toggling back
    /// has a well-defined target. Held constant across the tween
    /// as the "canonical" perspective FOV.
    ref_fov: f32,
    /// Radius at toggle time — restored at the end for the same
    /// reason. Held constant so the ortho mode's `half_h` matches
    /// the perspective view's visible height at the orbit centre.
    ref_radius: f32,
    /// `far` clip plane at toggle time. Restored at the end of
    /// the tween — see `step` for why we need to dilate it during
    /// the tween.
    ref_far: f32,
    /// Projection mode the camera will land in at `progress = 1`.
    target: Projection,
    /// Progress in `[0, 1]`. `step` advances this by `dt / duration`.
    progress: f32,
    /// Total tween duration in seconds.
    duration: f32,
}

impl ProjectionAnimation {
    /// Build a tween that takes `camera`'s projection to `target`
    /// over `duration` seconds. The `camera` is snapshotted: changes
    /// to `fov_y` / `radius` made by the user *during* the tween
    /// will be overwritten on the final frame. That mirrors
    /// MatterCAD's behaviour (it locks input during the 0.25 s
    /// animation).
    pub fn new(camera: &OrbitCamera, target: Projection, duration: f32) -> Self {
        Self {
            ref_fov: camera.fov_y,
            ref_radius: camera.radius,
            ref_far: camera.far,
            target,
            progress: 0.0,
            duration: duration.max(1e-3),
        }
    }

    /// Advance the animation by `dt` seconds and write the
    /// intermediate (or final) state to `camera`. Returns `true` if
    /// the animation finished on this step.
    pub fn step(&mut self, camera: &mut OrbitCamera, dt: f32) -> bool {
        self.progress = (self.progress + dt / self.duration).min(1.0);
        let s = smoothstep01(self.progress);
        // Avoid `tan(0)` and a div-by-zero radius. 1e-3 rad ~ 0.06°
        // — small enough to read as ortho on screen.
        let fov_min = 1e-3_f32;
        let (fov_start, fov_end) = match self.target {
            Projection::Orthographic => (self.ref_fov, fov_min),
            Projection::Perspective => (fov_min, self.ref_fov),
        };
        let fov = lerp(fov_start, fov_end, s);

        // Reference visible half-height at the orbit centre, held
        // constant. tan(ref_fov/2) * ref_radius is the
        // pre-tween value; equating that to tan(fov/2) * new_radius
        // gives the radius needed to keep the model the same size
        // on screen as the foreshortening fades.
        let ref_half_h = (self.ref_fov * 0.5).tan() * self.ref_radius;
        let new_radius = ref_half_h / (fov * 0.5).tan().max(1e-6);

        camera.fov_y = fov;
        camera.radius = new_radius;
        // CRITICAL: as `fov` approaches `fov_min`, `new_radius`
        // explodes (≈ `ref_half_h / fov_min` — for a default scene
        // that's ~43,000 units while `OrbitCamera::default().far`
        // is only 5,000). Without the dilation below, the model
        // ends up beyond the far clip plane for the last few
        // frames of the tween → wgpu clips everything → a single
        // white frame, in both directions. Dilate `far` to safely
        // contain the puffed-up radius; restored to `ref_far` on
        // the final frame.
        camera.far = self.ref_far.max(new_radius * 2.0);
        // The tween itself ALWAYS runs through perspective math
        // (matches MatterCAD's `CalculatePerspectiveMatrixOffCenter`
        // call in the per-step update). The final-frame block
        // below swaps in true ortho.
        camera.projection = Projection::Perspective;

        if self.progress >= 1.0 {
            camera.fov_y = self.ref_fov;
            camera.radius = self.ref_radius;
            camera.far = self.ref_far;
            camera.projection = self.target;
            return true;
        }
        false
    }

    pub fn is_done(&self) -> bool {
        self.progress >= 1.0
    }
}

fn smoothstep01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl OrientAnimation {
    /// Build an animation that takes the camera from its current
    /// pose to `target_orientation` (centre held). `duration` is in
    /// seconds — 0.25 s matches MatterCAD's `AnimateRotation` length.
    /// Mirrors `Quaternion.Slerp(rotationStart, rotationEnd, t)`.
    pub fn to_orientation(camera: &OrbitCamera, target_orientation: Quat, duration: f32) -> Self {
        Self {
            start_orientation: camera.orientation,
            target_orientation: target_orientation.normalize(),
            start_center: Vec3::from(camera.center),
            target_center: Vec3::from(camera.center),
            progress: 0.0,
            duration: duration.max(1e-3),
        }
    }

    pub fn is_done(&self) -> bool {
        self.progress >= 1.0
    }

    /// Advance the animation by `dt` seconds, write the eased
    /// orientation into `camera`, and return `true` if the animation
    /// finished on this step (so callers can drop the handle).
    pub fn step(&mut self, camera: &mut OrbitCamera, dt: f32) -> bool {
        if self.progress >= 1.0 {
            return false;
        }
        self.progress = (self.progress + dt / self.duration).min(1.0);
        let s = smoothstep01(self.progress);
        camera.orientation = self
            .start_orientation
            .slerp(self.target_orientation, s)
            .normalize();
        let c = self.start_center.lerp(self.target_center, s);
        camera.center = c.to_array();
        self.progress >= 1.0
    }
}
