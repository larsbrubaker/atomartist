//! Per-frame `WgpuSceneRenderer` state population — extracted from
//! `viewport_widget.rs`'s `paint()` so the parent file stays under
//! the 800-line guardrail. Side-effect-only helpers; everything they
//! touch already lives on the parent `Viewport3dWidget` struct.

use super::*;

impl Viewport3dWidget {
    /// Snapshot the current frame's host state into the scene
    /// renderer's mutable inputs — bodies, camera, gizmos, style
    /// flags. Per-frame side effects only; no allocations beyond the
    /// gizmo set pushes.
    pub(super) fn populate_scene_state(
        &self,
        bodies: Vec<atomartist_lib::geometry::Body>,
        first_body: Option<&atomartist_lib::geometry::Body>,
        selection_active: bool,
        outline_width: f32,
        outline_color: [f32; 4],
    ) {
        let sel_id = *self.inputs.selection.lock().unwrap();
        // Index of the body whose `origin` NodeId matches the active
        // selection. The outline pass uses this so the silhouette
        // rims the body the user actually clicked on — not just the
        // first body in the group. `None` falls back to body 0 in
        // the renderer (e.g. selection-active but origin un-matched
        // somehow).
        let outline_body_index = sel_id.and_then(|id| {
            bodies.iter().position(|b| b.origin == Some(id))
        });
        let mut s = self.scene.borrow_mut();
        s.bodies = bodies;
        if let Some(b) = first_body {
            s.base_color = b.color;
        }
        s.camera = self.cam();
        s.outline_enabled = selection_active;
        s.outline_width = outline_width;
        s.outline_body_index = outline_body_index;
        s.gizmo_lines.clear();
        s.gizmo_triangles.clear();
        // Bounds-box gizmo dropped — the selection-outline rim
        // already gives a visible "this is selected" cue. The bounds
        // box added noise without orientation context.
        // Z control gizmo — anchored above the selected body's world
        // AABB. Camera-distance-proportional sizing keeps the gizmo
        // a constant pixel-size at any zoom; idle colour mirrors the
        // active theme's text colour (MatterCAD parity).
        if let Some(sel_id) = *self.inputs.selection.lock().unwrap() {
            let geom = self.current_geometry();
            if let Some(world_aabb) = selected_body_world_aabb(geom.as_deref(), sel_id) {
                let visuals = agg_gui::theme::current_visuals();
                let idle = [
                    visuals.text_color.r,
                    visuals.text_color.g,
                    visuals.text_color.b,
                    1.0,
                ];
                let accent = [
                    visuals.accent.r,
                    visuals.accent.g,
                    visuals.accent.b,
                    1.0,
                ];
                let cam = self.cam();
                let vh = self.bounds.height.max(1.0) as f32;
                // Universal "hide the others while a control is
                // engaged" (MatterCAD turns off the selection
                // indicators during a drag): each control draws only
                // while idle — or while ITS drag is active, anchored
                // to the drag state so it rides the cursor instead of
                // the async-rebuilt geometry (which lags and made the
                // controls bounce against the body).
                let drag_idle = matches!(
                    self.drag,
                    CameraDrag::None | CameraDrag::Selecting { .. }
                );
                // Z-translate cone (MatterCAD `MoveInZControl`):
                // hover-accented while idle, accent + drag-anchored
                // while moving in Z, hidden during any other control's
                // drag.
                match &self.drag {
                    _ if drag_idle => {
                        let z_color = if self.hovered_z_control { accent } else { idle };
                        s.gizmo_triangles.push(z_control_gizmo::z_control_for_aabb(
                            world_aabb, &cam, vh, z_color,
                        ));
                    }
                    CameraDrag::DragBodyZ {
                        anchor_xy,
                        start_top_z,
                        live_dz,
                        ..
                    } => {
                        let top = [anchor_xy[0], anchor_xy[1], start_top_z + live_dz];
                        let (center, size) =
                            z_control_gizmo::z_control_layout_at(top, &cam, vh);
                        s.gizmo_triangles
                            .push(z_control_gizmo::z_cone(center, size, accent));
                    }
                    _ => {}
                }
                // Height / scale-Z box (MatterCAD `ScaleHeightControl`).
                // Idle: mode-aware placement (object top when a height
                // parameter exists, else AABB top), hover-accented.
                // While ITS drag is in flight: anchored to the drag
                // axis + live length. Hidden during other drags.
                match &self.drag {
                    _ if drag_idle => {
                        let hbox_color =
                            if self.hovered_height_control { accent } else { idle };
                        let (hbox_center, hbox_size, hbox_axes) =
                            self.height_box_layout(sel_id, world_aabb, &cam, vh);
                        s.gizmo_triangles.push(z_control_gizmo::height_control(
                            hbox_center,
                            hbox_size,
                            hbox_axes,
                            hbox_color,
                        ));
                    }
                    CameraDrag::DragBodyHeight {
                        axis_origin,
                        axis_dir,
                        live_len,
                        start_body_matrix,
                        ..
                    } => {
                        let top = [
                            axis_origin[0] + axis_dir[0] * live_len,
                            axis_origin[1] + axis_dir[1] * live_len,
                            axis_origin[2] + axis_dir[2] * live_len,
                        ];
                        let upp = cam.world_units_per_pixel_at(top, vh);
                        let size = z_control_gizmo::HEIGHT_BOX_PX * upp;
                        let half = size * 0.5;
                        let center = [
                            top[0] + axis_dir[0] * half,
                            top[1] + axis_dir[1] * half,
                            top[2] + axis_dir[2] * half,
                        ];
                        let m = start_body_matrix;
                        let xa = normalize3([m[0], m[1], m[2]]);
                        let ya = normalize3([m[4], m[5], m[6]]);
                        let axes = [
                            if xa[0].is_finite() { xa } else { [1.0, 0.0, 0.0] },
                            if ya[0].is_finite() { ya } else { [0.0, 1.0, 0.0] },
                            *axis_dir,
                        ];
                        s.gizmo_triangles.push(z_control_gizmo::height_control(
                            center, size, axes, accent,
                        ));
                    }
                    _ => {}
                }
                // Rotate gizmo — three per-axis corner handles (MatterCAD
                // RotateCornerControl). Two display modes:
                //
                // * While a rotation is in flight, hide the arrow handles
                //   (they'd jitter with the spinning AABB) and draw only
                //   the compass, anchored to the mouse-down capture so it
                //   stays put — MatterCAD hides the arrows while a control
                //   is engaged.
                // * Idle: draw all three handles (accent the hovered one),
                //   and show the ring + ticks for the hovered axis.
                match &self.drag {
                    CameraDrag::RotateBodyAxis {
                        axis,
                        center,
                        anchor_angle,
                        snapped,
                        radius,
                        ..
                    } => {
                        let (band, ticks) = rotate_gizmo::ring_and_ticks(
                            *center, *axis, *radius, &cam, vh, accent, idle,
                        );
                        s.gizmo_triangles.push(band);
                        s.gizmo_lines.push(ticks);
                        let (otris, olines) = rotate_gizmo::drag_overlay(
                            *center,
                            *axis,
                            *radius,
                            *anchor_angle,
                            *snapped,
                            &cam,
                            vh,
                            accent,
                            idle,
                        );
                        for t in otris {
                            s.gizmo_triangles.push(t);
                        }
                        for l in olines {
                            s.gizmo_lines.push(l);
                        }
                    }
                    CameraDrag::DragBodyZ {
                        anchor_xy,
                        start_bottom_z,
                        live_dz,
                        ..
                    } => {
                        // Moving in Z: hide the rotate handles and show
                        // measure bars from the bed up to the body's
                        // bottom (the 2-D value label is drawn by the
                        // viewport's overlay pass). Anchored to the
                        // drag state so the bars track the cursor.
                        // MatterCAD's `MoveInZControl` measurement.
                        let (bars, _, _) = z_control_gizmo::measure_bars(
                            [anchor_xy[0], anchor_xy[1], 0.0],
                            [anchor_xy[0], anchor_xy[1], start_bottom_z + live_dz],
                            &cam,
                            vh,
                            idle,
                        );
                        s.gizmo_lines.push(bars);
                    }
                    CameraDrag::DragBodyHeight {
                        axis_origin,
                        axis_dir,
                        live_len,
                        ..
                    } => {
                        // Scaling in Z: hide the rotate handles and show
                        // measure bars spanning the body's base → top
                        // along the drag axis (MatterCAD's
                        // `ScaleHeightControl` measurement). Anchors
                        // come from the drag state, so the bars track
                        // the cursor without async-rebuild lag.
                        let top = [
                            axis_origin[0] + axis_dir[0] * live_len,
                            axis_origin[1] + axis_dir[1] * live_len,
                            axis_origin[2] + axis_dir[2] * live_len,
                        ];
                        let (bars, _, _) = z_control_gizmo::measure_bars(
                            *axis_origin,
                            top,
                            &cam,
                            vh,
                            idle,
                        );
                        s.gizmo_lines.push(bars);
                    }
                    _ if !drag_idle => {
                        // Some other drag (e.g. bed-plane XY) is in
                        // flight — hide the rotate handles too; only
                        // the active control may draw.
                    }
                    _ => {
                        // Idle: hovering a Z/height control previews its
                        // measure bars, exactly like hovering a rotate
                        // handle previews its compass ring (MatterCAD
                        // shows the measurement when `MouseIsOver`).
                        if self.hovered_z_control {
                            let (mn, mx) = world_aabb;
                            let cx = (mn[0] + mx[0]) * 0.5;
                            let cyc = (mn[1] + mx[1]) * 0.5;
                            let (bars, _, _) = z_control_gizmo::measure_bars(
                                [cx, cyc, 0.0],
                                [cx, cyc, mn[2]],
                                &cam,
                                vh,
                                idle,
                            );
                            s.gizmo_lines.push(bars);
                        } else if self.hovered_height_control {
                            if let Some((base, top)) = self.height_measure_anchors(sel_id) {
                                let (bars, _, _) =
                                    z_control_gizmo::measure_bars(base, top, &cam, vh, idle);
                                s.gizmo_lines.push(bars);
                            }
                        }
                        let layouts = rotate_gizmo::rotate_axis_layouts(world_aabb, &cam, vh);
                        for handle in rotate_gizmo::rotate_handles(
                            &layouts,
                            self.hovered_rotate_axis,
                            idle,
                            accent,
                        ) {
                            s.gizmo_triangles.push(handle);
                        }
                        if let Some(axis) = self.hovered_rotate_axis {
                            let l = layouts[axis as usize];
                            let rc = l.rotation_center;
                            let cc = l.control_center;
                            let radius = ((cc[0] - rc[0]).powi(2)
                                + (cc[1] - rc[1]).powi(2)
                                + (cc[2] - rc[2]).powi(2))
                            .sqrt();
                            let (band, ticks) = rotate_gizmo::ring_and_ticks(
                                rc, axis, radius, &cam, vh, accent, idle,
                            );
                            s.gizmo_triangles.push(band);
                            s.gizmo_lines.push(ticks);
                        }
                    }
                }
            }
        }
        s.outline_color = outline_color;
        s.render_style = *self.inputs.render_style.lock().unwrap();
        s.draw_grid = *self.inputs.show_bed.lock().unwrap();
    }
}
