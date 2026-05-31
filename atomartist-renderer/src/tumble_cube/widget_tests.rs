//! Unit tests for [`super::widget::TumbleCubeWidget`].
//!
//! Split out of `widget.rs` so the main source file stays under the
//! repository line-count guardrail (`atomartist-lib/tests/file_line_count.rs`).
//! Pulled in via `#[path]` so it shares the parent module's private
//! surface (e.g. `raycast_rendered_cube`).

use super::*;
use agg_gui::Modifiers;
use crate::camera::OrbitMode;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Drive a small drag through `Widget::on_event` and verify the
/// camera orientation moves by the small expected amount and does
/// NOT snap toward a pole (the user-reported regression where the
/// cube would jump to the bottom view after a few pixels of drag).
#[test]
fn small_drag_produces_small_rotation_not_a_snap() {
    let camera = Arc::new(Mutex::new(OrbitCamera::default()));
    camera.lock().unwrap().orbit_mode = OrbitMode::Turntable;
    let before_back = camera.lock().unwrap().orientation * glam::Vec3::Z;
    let before_elevation = before_back.z.clamp(-1.0, 1.0).asin();

    let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
        camera: camera.clone(),
        animation_completed: None,
    });
    widget.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));

    let down = Point::new(50.0, 50.0);
    widget.on_event(&Event::MouseDown {
        pos: down,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    // Drag 10 px to the right and 5 px up — past the 2-px threshold,
    // so this should enter Rotating and apply a delta of roughly
    // `dx*scale` yaw + `dy*scale` pitch where scale = 0.01 rad/px.
    widget.on_event(&Event::MouseMove { pos: Point::new(60.0, 55.0) });
    widget.on_event(&Event::MouseUp {
        pos: Point::new(60.0, 55.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });

    let after_back = camera.lock().unwrap().orientation * glam::Vec3::Z;
    let after_elevation = after_back.z.clamp(-1.0, 1.0).asin();
    let delta = (after_elevation - before_elevation).abs();
    // Expected: dy=5 px * 0.01 = 0.05 rad ≈ 2.86° of pitch.
    // Allow a generous bound up to 0.2 rad (≈11°). Anything bigger
    // means we snapped toward a pole.
    assert!(
        delta < 0.2,
        "small drag should rotate the camera by a small angle; \
         elevation moved {delta} rad ({} → {})",
        before_elevation, after_elevation
    );
    // And the cube must not have snapped down to looking-from-below.
    assert!(
        after_back.z > -0.5,
        "small drag should NOT land near the lower pole; \
         after back.z = {} (expected near {before:.3})",
        after_back.z, before = before_back.z
    );
}

/// User-reported regression reproducer: click on the front face at
/// default 3/4 view and drag the cursor DOWN (decreasing pos.y in
/// agg-gui's Y-up screen coords). The user expects the camera to
/// tilt UP (front face moves down on screen → top of bed comes into
/// view), which is what the turntable math does for `pitch_angle <
/// 0`. If instead the camera tilts DOWN and snaps to the lower
/// pole (back.z ≈ -1, eye below the bed looking up), something has
/// reversed the dy sign.
#[test]
fn drag_cursor_down_tilts_toward_top_view_not_bottom() {
    let camera = Arc::new(Mutex::new(OrbitCamera::default()));
    camera.lock().unwrap().orbit_mode = OrbitMode::Turntable;
    let before_back = camera.lock().unwrap().orientation * glam::Vec3::Z;

    let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
        camera: camera.clone(),
        animation_completed: None,
    });
    widget.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));

    // Mouse-down on the front face of the cube (lower-centre of the
    // widget rect at default 3/4 view).
    widget.on_event(&Event::MouseDown {
        pos: Point::new(50.0, 75.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    // Drag the cursor DOWN in Y-up coords (pos.y decreases).
    for step in 1..=10 {
        widget.on_event(&Event::MouseMove {
            pos: Point::new(50.0, 75.0 - 5.0 * step as f64),
        });
    }
    widget.on_event(&Event::MouseUp {
        pos: Point::new(50.0, 25.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });

    let after_back = camera.lock().unwrap().orientation * glam::Vec3::Z;
    // Cursor-down in Y-up should INCREASE camera elevation (eye
    // rises further above the bed), not crash into the lower pole.
    assert!(
        after_back.z >= before_back.z - 1e-3,
        "drag cursor-down should not lower camera elevation; back.z {} → {}",
        before_back.z, after_back.z
    );
    // And it must absolutely not snap to the lower pole (bottom view).
    assert!(
        after_back.z > -0.5,
        "drag cursor-down must NOT snap to bottom view; back = {after_back:?}"
    );
}

/// User-reported regression: clicking on the front face and
/// dragging mid-drag is supposed to feel like a "standard
/// trackball/turntable" rotation. The cube widget must call the
/// same `orbit_drag_around` primitive the main viewport uses, so
/// dragging on the cube and dragging on the viewport with the
/// same gesture produce the SAME orientation delta.
#[test]
fn cube_drag_matches_viewport_drag_for_same_gesture() {
    let cam_a = Arc::new(Mutex::new(OrbitCamera::default()));
    let cam_b = Arc::new(Mutex::new(OrbitCamera::default()));

    let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
        camera: cam_a.clone(),
        animation_completed: None,
    });
    widget.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));

    // Drive the cube widget with a 25-px down + 15-px right drag.
    widget.on_event(&Event::MouseDown {
        pos: Point::new(50.0, 75.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    widget.on_event(&Event::MouseMove { pos: Point::new(65.0, 50.0) });

    // Replicate the viewport's `Orbit` branch by hand on the
    // independent camera with the SAME deltas and scale.
    let dx = 15.0_f32; // 65 - 50
    let dy = -25.0_f32; // 50 - 75 (cursor moved DOWN in Y-up)
    let scale = 0.005_f32;
    {
        let mut c = cam_b.lock().unwrap();
        let pivot = c.center;
        c.orbit_drag_around(pivot, -dx * scale, dy * scale);
    }

    let qa = cam_a.lock().unwrap().orientation;
    let qb = cam_b.lock().unwrap().orientation;
    let cosine = qa.dot(qb).abs();
    assert!(
        cosine > 0.9999,
        "cube drag must apply the same orientation delta as the viewport's orbit_drag_around; \
         cube q = {qa:?}, viewport-equivalent q = {qb:?}, |dot| = {cosine}"
    );
}

/// Multi-step drag that simulates a real user dragging across the
/// cube. Each `MouseMove` step is small (5 px) and the total drag
/// is ~50 px. With scale = 0.005 rad/px (matching the viewport)
/// that's ~0.25 rad of pitch — far from snapping to a pole.
#[test]
fn multi_step_drag_in_turntable_mode_does_not_snap() {
    let camera = Arc::new(Mutex::new(OrbitCamera::default()));
    camera.lock().unwrap().orbit_mode = OrbitMode::Turntable;
    let before_back = camera.lock().unwrap().orientation * glam::Vec3::Z;
    let before_elevation = before_back.z.clamp(-1.0, 1.0).asin();

    let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
        camera: camera.clone(),
        animation_completed: None,
    });
    widget.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));

    widget.on_event(&Event::MouseDown {
        pos: Point::new(50.0, 25.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    // Drag upward 5 px at a time for 10 steps = 50 px total.
    for step in 1..=10 {
        widget.on_event(&Event::MouseMove {
            pos: Point::new(50.0, 25.0 + 5.0 * step as f64),
        });
    }
    widget.on_event(&Event::MouseUp {
        pos: Point::new(50.0, 75.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });

    let after_back = camera.lock().unwrap().orientation * glam::Vec3::Z;
    let after_elevation = after_back.z.clamp(-1.0, 1.0).asin();
    // 50 px * 0.005 rad/px = 0.25 rad of pitch. From the default
    // ~0.38 rad elevation, dragging cursor-up lowers elevation by
    // ~0.25 rad → ends near +0.13 rad. Still well above the lower
    // pole of -π·0.499.
    let delta = before_elevation - after_elevation;
    assert!(
        (delta - 0.25).abs() < 0.05,
        "10×5 px upward drags should pitch by ~0.25 rad; got {delta} rad ({} → {})",
        before_elevation, after_elevation
    );
}

/// Click without drag (cursor barely moves) should land in the
/// click-to-orient path. With the cursor in the middle of the
/// cube at default 3/4 view, the picked tile is the centre of the
/// front-most face and the resulting animation must NOT pull the
/// camera toward the bottom face. This pins down the symptom the
/// user described as "small rotation snaps to bottom" in case the
/// drag threshold was being missed.
#[test]
fn click_in_centre_of_default_view_does_not_orient_to_bottom() {
    let camera = Arc::new(Mutex::new(OrbitCamera::default()));
    let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
        camera: camera.clone(),
        animation_completed: None,
    });
    widget.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));

    let pos = Point::new(50.0, 50.0);
    widget.on_event(&Event::MouseDown {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    widget.on_event(&Event::MouseUp {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    // Run any click-to-orient animation to completion.
    for _ in 0..40 {
        widget.step_animation_for_test(0.016);
        if !widget.animation_active() { break; }
    }
    let after_back = camera.lock().unwrap().orientation * glam::Vec3::Z;
    // "Bottom view" = back ≈ -Z. Anything close to that here means
    // the centre-of-cube click was wrongly resolved to the bottom
    // tile.
    assert!(
        after_back.z > -0.5,
        "centre click on default view must not snap to the bottom; \
         back = {after_back:?}"
    );
}

/// The cube must NOT swallow a mouse-up that wasn't part of a cube
/// gesture. It is stacked ABOVE the 3-D viewport in `ViewportOverlay`,
/// whose `on_event` forwards every MouseUp to each child top-down and
/// stops at the first that consumes. If the idle cube consumed the
/// release that ends a viewport body-drag, the body would stay glued
/// to the cursor (the reported "doesn't release the object" bug). An
/// idle MouseUp must therefore return `Ignored` so it falls through to
/// the viewport; a MouseUp that ends a real cube gesture must still be
/// `Consumed`.
#[test]
fn idle_mouse_up_falls_through_but_gesture_release_is_consumed() {
    let camera = Arc::new(Mutex::new(OrbitCamera::default()));
    let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
        camera: camera.clone(),
        animation_completed: None,
    });
    widget.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));

    // No prior MouseDown → CubeDrag::None. A stray release here belongs
    // to whatever the user was actually dragging (the viewport beneath).
    let idle = widget.on_event(&Event::MouseUp {
        pos: Point::new(50.0, 50.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    assert_eq!(
        idle,
        EventResult::Ignored,
        "an idle cube must not swallow a mouse-up meant for the viewport",
    );

    // A real cube gesture (down on the cube, then up) is the cube's own
    // and must stay consumed so it doesn't double-dispatch.
    widget.on_event(&Event::MouseDown {
        pos: Point::new(50.0, 50.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    let release = widget.on_event(&Event::MouseUp {
        pos: Point::new(50.0, 50.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    assert_eq!(
        release,
        EventResult::Consumed,
        "the cube's own click release must remain consumed",
    );
}

/// Cube drags must respect `OrbitMode`. With identical drag inputs,
/// turntable and trackball produce different orientation deltas
/// (matches `orbit_drag_honours_orbit_mode` in `camera_tests`).
/// This test confirms the cube widget isn't bypassing the mode by,
/// for example, calling some explicit yaw/pitch code path.
#[test]
fn cube_drag_respects_orbit_mode() {
    let make = |mode: OrbitMode| -> Arc<Mutex<OrbitCamera>> {
        let cam = Arc::new(Mutex::new(OrbitCamera::default()));
        {
            let mut c = cam.lock().unwrap();
            c.orientation = Quat::from_rotation_y(0.4) * Quat::from_rotation_x(-0.7);
            c.orbit_mode = mode;
        }
        cam
    };
    let drive = |cam: Arc<Mutex<OrbitCamera>>| {
        let mut w = TumbleCubeWidget::new(TumbleCubeInputs {
            camera: cam.clone(),
            animation_completed: None,
        });
        w.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));
        w.on_event(&Event::MouseDown {
            pos: Point::new(20.0, 20.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        w.on_event(&Event::MouseMove { pos: Point::new(60.0, 60.0) });
        w.on_event(&Event::MouseUp {
            pos: Point::new(60.0, 60.0),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        cam.lock().unwrap().orientation
    };
    let tt = drive(make(OrbitMode::Turntable));
    let tb = drive(make(OrbitMode::Trackball));
    // Same drag, different modes → different ending orientations.
    let cosine = tt.dot(tb).abs();
    assert!(
        cosine < 0.999,
        "turntable and trackball cube drags should disagree at a tilted start; |dot| = {cosine}"
    );
}

#[test]
fn orient_to_face_runs_to_completion_and_fires_hook_once() {
    let camera = Arc::new(Mutex::new(OrbitCamera::default()));
    {
        let mut c = camera.lock().unwrap();
        // Start away from Front so the test verifies the
        // animation actually moves the orientation.
        c.orientation = Quat::from_rotation_y(1.25) * Quat::from_rotation_x(-0.65);
    }

    let completed = Arc::new(AtomicUsize::new(0));
    let completed_cb = completed.clone();
    let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
        camera: camera.clone(),
        animation_completed: Some(Arc::new(move || {
            completed_cb.fetch_add(1, Ordering::SeqCst);
        })),
    });

    widget.orient_to_face(Face::Front);
    assert!(widget.animation_active(), "orient_to_face should start an animation");

    // Step past the 0.25s duration in several smaller increments
    // to exercise interpolation rather than only the final clamp.
    for _ in 0..20 {
        widget.step_animation_for_test(0.016);
        if !widget.animation_active() {
            break;
        }
    }

    assert!(!widget.animation_active(), "animation should run to completion");
    assert_eq!(completed.load(Ordering::SeqCst), 1, "completion hook should fire once");

    // Z-up Front view: camera at -Y looking +Y. The orientation's
    // back vector (eye-from-centre) lands on -Y.
    let c = camera.lock().unwrap();
    let back = c.orientation * glam::Vec3::Z;
    assert!(
        (back - glam::Vec3::NEG_Y).length() < 1e-3,
        "Front view should land at back = -Y, got {back:?}"
    );
}

#[test]
fn raycast_uses_rendered_scaled_cube_bounds() {
    // X=0.9 would hit the old unscaled [-1, 1] cube, but should
    // miss the rendered cube because it is scaled to [-0.8, 0.8].
    let miss = raycast_rendered_cube([0.9, 0.0, 5.0], [0.0, 0.0, -1.0]);
    assert!(miss.is_none(), "ray outside the scaled cube should miss");

    let hit = raycast_rendered_cube([0.7, 0.0, 5.0], [0.0, 0.0, -1.0])
        .expect("ray inside the scaled cube should hit");
    // Returned hit is model-space, so the front face is still z=1.
    assert!((hit[2] - 1.0).abs() < 1e-4, "hit z = {}", hit[2]);
}
