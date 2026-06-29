//! Gizmo render pass — solid + overlay line drawing for the 3-D
//! viewport's gizmos (bounds, Z control, XY control, rotate corner,
//! measurement overlay).
//!
//! Behavioural port of NodeDesigner's `gizmo-render-pass.js` and the
//! per-gizmo `*.js` files under
//! `static/js/node-editor/rendering/`. NodeDesigner runs each gizmo
//! through Three.js's standard mesh draw with `depthTest: true` for
//! the solid pass and `depthTest: false + transparent + opacity:
//! 0.25-0.35` for the overlay pass. AtomArtist keeps the same
//! two-variant idea but uses wgpu line pipelines (see
//! [`pipelines`]).
//!
//! ## Rendering order
//!
//! Gizmos run into the HDR scene composite (`scene_fb`) after the
//! dual-peel resolve and the post-process outline, before the final
//! 3×3 box downsample. That ordering means:
//!
//! * Gizmos supersample along with the rest of the scene → a smoothly
//!   anti-aliased rim, same as the outline.
//! * The solid variant depth-tests against `scene_depth`, which the
//!   opaque pass populated, so gizmos hide behind closer geometry.
//! * The overlay variant has no depth test so the occluded portion
//!   of the gizmo is still visible at the gizmo's `occluded_alpha`
//!   (`0.25` per NodeDesigner `bounds-gizmo.js`).
//!
//! ## Data model
//!
//! The host populates [`WgpuSceneRenderer::gizmo_lines`] each frame
//! with one or more [`GizmoLineSet`]s. Each set carries:
//!
//! * `vertices` — pairs of `[x, y, z]` defining `LineList` segments.
//! * `color` — RGBA. The overlay variant multiplies `color.a` by
//!   `occluded_alpha`; the solid variant uses `color` verbatim.
//! * `matrix` — optional 4×4 model matrix (column-major) applied
//!   before view × projection. Identity when `None`.
//! * `draw_solid` / `draw_overlay` — which variants to draw.
//! * `occluded_alpha` — overlay-only alpha multiplier (NodeDesigner
//!   defaults: 0.25 for bounds, 0.35 for control gizmos).
//!
//! The renderer rebuilds the line vbuf each frame from the current
//! `gizmo_lines` content. That's cheap (≤ a few hundred vertices per
//! gizmo) and avoids per-frame cache invalidation logic; if a real
//! perf problem shows up later we can add a hash-based cache.

pub mod handles;
pub mod pipelines;
pub mod shaders;

pub use handles::{cone_handle, cube_handle, oriented_cube_handle, sphere_handle};
pub use pipelines::{GizmoLinePipelines, GizmoLineUniforms, GizmoLineVertex};

/// Host-side description of one filled-triangle gizmo — used by
/// gizmo handle meshes (the small spheres/cubes the user clicks and
/// drags). Same shader and uniform layout as [`GizmoLineSet`]; only
/// the pipeline's primitive topology differs (TriangleList instead
/// of LineList) and we cull back-faces so the handles read as solid
/// 3-D shapes.
///
/// Vertex layout: every consecutive triplet defines one triangle.
/// CCW from outside (matches the rest of the renderer). No indices
/// — keeping symmetry with `GizmoLineSet`; the handle meshes are
/// tiny (≤ a few hundred triangles) so the redundancy is cheap.
#[derive(Clone, Debug)]
pub struct GizmoTriangleSet {
    /// Triangles flat-packed as triplets of `[x, y, z]`. CCW outward.
    pub vertices: Vec<[f32; 3]>,
    /// RGBA tint (same convention as `GizmoLineSet`).
    pub color: [f32; 4],
    /// Optional model matrix (column-major). Identity when `None`.
    pub matrix: Option<[f32; 16]>,
    /// Draw the depth-tested solid variant.
    pub draw_solid: bool,
    /// Draw the no-depth overlay variant.
    pub draw_overlay: bool,
    /// Overlay alpha multiplier (matches `GizmoLineSet`).
    pub occluded_alpha: f32,
}

/// Host-side description of one gizmo's line segments. Pushed into
/// [`super::WgpuSceneRenderer::gizmo_lines`] each frame.
#[derive(Clone, Debug)]
pub struct GizmoLineSet {
    /// Pairs of vertices — every two consecutive entries define one
    /// `LineList` segment. NodeDesigner's bounds gizmo emits 24
    /// entries (12 edges × 2 vertices).
    pub vertices: Vec<[f32; 3]>,
    /// RGBA colour. The solid variant uses this verbatim; the
    /// overlay variant multiplies `a` by `occluded_alpha`.
    pub color: [f32; 4],
    /// Optional 4×4 column-major model matrix. Identity when
    /// `None`.
    pub matrix: Option<[f32; 16]>,
    /// Draw the depth-tested solid variant.
    pub draw_solid: bool,
    /// Draw the no-depth overlay variant.
    pub draw_overlay: bool,
    /// Overlay alpha multiplier — NodeDesigner uses 0.25 for the
    /// bounds gizmo and 0.35 for the control gizmos. Ignored when
    /// `draw_overlay = false`.
    pub occluded_alpha: f32,
}

#[cfg(test)]
mod tests {
    use super::shaders::GIZMO_LINE_SHADER;
    use super::*;

    /// Same WebGL2 / GLSL ES 300 regression guard the peel and
    /// outline shaders use. Catches future shader edits that drift
    /// into a path naga can't emit cleanly.
    #[test]
    fn gizmo_shader_emits_glsl_es_300() {
        for (label, stage) in [
            ("gizmo fs", naga::ShaderStage::Fragment),
            ("gizmo vs", naga::ShaderStage::Vertex),
        ] {
            let module = naga::front::wgsl::parse_str(GIZMO_LINE_SHADER)
                .unwrap_or_else(|e| panic!("[{label}] WGSL parse failed: {e:?}"));
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("[{label}] validation failed: {e:?}"));
            let options = naga::back::glsl::Options {
                version: naga::back::glsl::Version::Embedded {
                    version: 300,
                    is_webgl: true,
                },
                writer_flags: naga::back::glsl::WriterFlags::empty(),
                binding_map: Default::default(),
                zero_initialize_workgroup_memory: false,
            };
            let entry_point = module
                .entry_points
                .iter()
                .find(|ep| ep.stage == stage)
                .unwrap_or_else(|| panic!("[{label}] no entry point for {stage:?}"));
            let pipeline_options = naga::back::glsl::PipelineOptions {
                shader_stage: stage,
                entry_point: entry_point.name.clone(),
                multiview: None,
            };
            let mut out = String::new();
            let mut writer = naga::back::glsl::Writer::new(
                &mut out,
                &module,
                &info,
                &options,
                &pipeline_options,
                naga::proc::BoundsCheckPolicies::default(),
            )
            .unwrap_or_else(|e| panic!("[{label}] glsl writer construct failed: {e:?}"));
            writer
                .write()
                .unwrap_or_else(|e| panic!("[{label}] glsl emit failed: {e:?}"));
            assert!(
                out.contains("void main()"),
                "[{label}] emitted GLSL missing entry point: {out}"
            );
            assert!(
                !out.contains("sampler2DShadow"),
                "[{label}] emitted GLSL bound a shadow sampler: {out}"
            );
        }
    }

    /// `bounds_box` produces exactly 24 vertices (12 edges × 2) with
    /// the NodeDesigner red colour and the `bounds-gizmo.js`-default
    /// occluded alpha of 0.25.
    #[test]
    fn bounds_box_layout_matches_node_designer() {
        let set = GizmoLineSet::bounds_box([0.0, 0.0, 0.0], [2.0, 4.0, 6.0], None);
        assert_eq!(set.vertices.len(), 24);
        assert_eq!(set.color, [1.0, 0.267, 0.267, 1.0]);
        assert_eq!(set.occluded_alpha, 0.25);
        assert!(set.draw_solid && set.draw_overlay);
        // Bottom face vertices should sit at z = -3 (cz - hh) and
        // top at z = +3. Spot-check the first segment which is the
        // first edge of the bottom face.
        assert_eq!(set.vertices[0][2], -3.0);
        assert_eq!(set.vertices[1][2], -3.0);
    }
}

impl GizmoLineSet {
    /// Build a wireframe box matching NodeDesigner's
    /// `bounds-gizmo.js::updateBounds`: 12 edges around a centred
    /// AABB. `center` and `size` are world-space; an optional
    /// `matrix` transforms the corners (used by NodeDesigner when a
    /// FitToBounds node has downstream transforms applied).
    ///
    /// Default colour matches NodeDesigner's `0xff4444` red with
    /// alpha 1.0. Both variants are enabled (`occluded_alpha = 0.25`
    /// matches NodeDesigner).
    pub fn bounds_box(
        center: [f32; 3],
        size: [f32; 3],
        matrix: Option<[f32; 16]>,
    ) -> Self {
        let hw = size[0] * 0.5;
        let hd = size[1] * 0.5;
        let hh = size[2] * 0.5;
        let cx = center[0];
        let cy = center[1];
        let cz = center[2];
        // 8 corners — same order as NodeDesigner bounds-gizmo.js:120.
        let corners = [
            [cx - hw, cy - hd, cz - hh],
            [cx + hw, cy - hd, cz - hh],
            [cx + hw, cy + hd, cz - hh],
            [cx - hw, cy + hd, cz - hh],
            [cx - hw, cy - hd, cz + hh],
            [cx + hw, cy - hd, cz + hh],
            [cx + hw, cy + hd, cz + hh],
            [cx - hw, cy + hd, cz + hh],
        ];
        // 12 edges — bottom face → top face → vertical edges.
        const EDGES: [(usize, usize); 12] = [
            (0, 1), (1, 2), (2, 3), (3, 0),
            (4, 5), (5, 6), (6, 7), (7, 4),
            (0, 4), (1, 5), (2, 6), (3, 7),
        ];
        let mut vertices = Vec::with_capacity(EDGES.len() * 2);
        for (a, b) in EDGES {
            vertices.push(corners[a]);
            vertices.push(corners[b]);
        }
        Self {
            vertices,
            color: [1.0, 0.267, 0.267, 1.0], // 0xff4444
            matrix,
            draw_solid: true,
            draw_overlay: true,
            occluded_alpha: 0.25,
        }
    }
}
