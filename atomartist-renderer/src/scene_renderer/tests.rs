//! Unit tests for [`super::WgpuSceneRenderer`] — extracted from
//! `scene_renderer/mod.rs` to keep that file under the 800-line
//! guardrail.

use super::WgpuSceneRenderer;

#[test]
fn renderer_is_constructible() {
    let r = WgpuSceneRenderer::new();
    assert!(r.bodies.is_empty());
}

/// Bed is currently hard-locked to `0.0` (ignoring `grid_z` too) so
/// no codepath can drift it while the bed-Z offset is reworked.
#[test]
fn bed_render_z_locked_to_zero() {
    let mut r = WgpuSceneRenderer::new();
    r.grid_z = 0.0;
    assert_eq!(r.bed_render_z(), 0.0);
    r.grid_z = 1.5;
    assert_eq!(r.bed_render_z(), 0.0);
}

#[test]
fn bed_toggle_default_is_on() {
    let r = WgpuSceneRenderer::new();
    assert!(r.draw_grid);
}

/// Regression: the opaque scene shader must survive naga's GLSL ES
/// 3.00 backend so the WASM (WebGL2) build can use it. Same failure
/// shape as the dual-peel `peel_shaders_emit_glsl_es_300` test: a
/// shadow sampler binding silently appearing in the emitted GLSL
/// means a future change has drifted into a path naga can't handle
/// on WebGL2. We also catch the more general "shader fails to
/// validate" by surfacing naga's error message verbatim.
///
/// New shader features added by the NodeDesigner port (`dpdx` /
/// `dpdy` flat normals, sRGB->linear conversion, dual-light
/// Blinn-Phong with shininess) are the most likely future regression
/// vector here — GLSL ES 3.00 supports them as core but the boundary
/// is thinner than for vertex / fragment basics.
#[test]
fn scene_shaders_emit_glsl_es_300() {
    use super::opaque_shaders::SCENE_SHADER;
    for (label, wgsl, stage) in [
        ("scene fs", SCENE_SHADER, naga::ShaderStage::Fragment),
        ("scene vs", SCENE_SHADER, naga::ShaderStage::Vertex),
    ] {
        let module = naga::front::wgsl::parse_str(wgsl)
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
            .unwrap_or_else(|| panic!("[{label}] no entry point for stage {stage:?}"));
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
        // sampler2DShadow sentinel — see the peel-shader test for the
        // full rationale. We don't sample depth textures here either,
        // so this must never appear.
        assert!(
            !out.contains("sampler2DShadow"),
            "[{label}] emitted GLSL bound a shadow sampler: {out}"
        );
    }
}

/// Regression: every per-body draw call in the dual-peel chain must
/// bind BOTH vertex-buffer slots (slot 0 = pos+normal, slot 1 =
/// per-vertex colour). The peel pipelines declare a 2-slot vertex
/// layout via `opaque_pass::vertex_layouts()`, and wgpu validates
/// the bind on every `draw_indexed` — missing slot 1 surfaces as a
/// runtime "requires vertex buffer 1 to be set" validation panic.
///
/// Caught the hard way: an earlier refactor added slot 1 to the
/// pipeline layout but only updated the init-pass draw loop; the
/// peel-iteration loop kept its single-slot bind and crashed on
/// the first frame with a body present. Static-grep test prevents
/// the same drift on the next pipeline that grows a vertex slot.
#[test]
fn dual_peel_draw_loops_bind_both_vertex_slots() {
    // Read the source file directly — checks current state of the
    // code, not a snapshot baked into the test binary.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/scene_renderer/depth_peel/pipelines.rs",
    ))
    .expect("read pipelines.rs");

    // Every occurrence of `set_vertex_buffer(0,` inside execute_chain
    // must be followed within 5 lines by a `set_vertex_buffer(1,`
    // before the next `draw_indexed`. The window catches the typical
    // bind → bind → set_index_buffer → draw_indexed shape without
    // false-positives from across-function distance.
    let lines: Vec<&str> = src.lines().collect();
    let mut slot0_lines: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.contains("set_vertex_buffer(0,") {
            slot0_lines.push(i);
        }
    }
    assert!(
        !slot0_lines.is_empty(),
        "expected at least one set_vertex_buffer(0, ...) in dual-peel pipelines",
    );

    for &i in &slot0_lines {
        let window_end = (i + 6).min(lines.len());
        let window = &lines[i..window_end];
        let has_slot1 = window.iter().any(|l| l.contains("set_vertex_buffer(1,"));
        let has_draw = window
            .iter()
            .any(|l| l.contains("draw_indexed") || l.contains("draw(") );
        assert!(
            has_slot1,
            "line {}: set_vertex_buffer(0,) without a paired set_vertex_buffer(1,) within \
             5 lines — dual-peel pipelines declare a 2-slot vertex layout so both must \
             bind before draw. Window:\n{}",
            i + 1,
            window.join("\n"),
        );
        let _ = has_draw;
    }
}
