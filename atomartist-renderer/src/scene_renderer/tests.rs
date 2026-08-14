//! Unit tests for [`super::WgpuSceneRenderer`] — extracted from
//! `scene_renderer/mod.rs` to keep that file under the 800-line
//! guardrail.

use super::WgpuSceneRenderer;

#[test]
fn renderer_is_constructible() {
    let r = WgpuSceneRenderer::new();
    assert!(r.bodies.is_empty());
}

/// Opaque/translucent classification drives the render split: opaque
/// bodies take the shaded depth-tested pass, translucent ones the
/// dual-peel chain. A fully-opaque tint and the un-tinted
/// inherit-sentinel (which resolves to the opaque default) must both
/// read opaque; anything with real alpha < 1 must read translucent.
#[test]
fn opaque_classification_matches_resolved_alpha() {
    use super::is_opaque_color;
    use atomartist_lib::geometry::{DEFAULT_GEOMETRY_COLOR, INHERIT_COLOR};

    assert!(is_opaque_color([0.6, 0.6, 0.6, 1.0]), "alpha 1 is opaque");
    assert!(
        is_opaque_color(DEFAULT_GEOMETRY_COLOR),
        "the default geometry colour is opaque",
    );
    assert!(
        is_opaque_color(INHERIT_COLOR),
        "the inherit sentinel resolves to the opaque default, so it is opaque",
    );
    assert!(
        !is_opaque_color([0.6, 0.6, 0.6, 0.5]),
        "alpha 0.5 is translucent → dual-peel",
    );
    assert!(
        !is_opaque_color([0.6, 0.6, 0.6, 0.9]),
        "alpha 0.9 is still translucent",
    );
}

/// Regression: opaque and translucent bodies take disjoint paths.
///
/// * Opaque bodies render through the shaded, depth-tested opaque pass
///   and are EXCLUDED from the dual-peel chain — forcing an opaque,
///   self-overlapping mesh through the peel produced black/white resolve
///   splotches.
/// * Translucent bodies are NOT drawn in the opaque pass at all, so they
///   never write the opaque depth. Depth-pre-passing them made the
///   peel's reject-behind-opaque collapse each translucent body to its
///   front layer (splotches / "only front faces"). MatterCAD likewise
///   keeps transparent geometry out of the opaque depth.
///
/// Verified statically because exercising the render path needs a wgpu
/// device.
#[test]
fn opaque_and_translucent_take_disjoint_paths() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/scene_renderer/render_impl.rs",
    ))
    .expect("read render_impl.rs");
    assert!(
        src.contains("s.opaque.draw_body(") && src.contains("!body.opaque"),
        "the opaque pass must shade opaque bodies via `draw_body` and skip \
         translucent ones (`!body.opaque`)",
    );
    assert!(
        !src.contains("draw_body_depth_only"),
        "translucent bodies must not be depth-pre-passed — writing the \
         opaque depth makes the peel reject their own back layers",
    );
    assert!(
        src.contains("b.vertex_count > 0 && !b.opaque"),
        "the dual-peel body list must exclude opaque bodies (`!b.opaque`)",
    );
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

// ---------------------------------------------------------------------------
// Adaptive supersampling
// ---------------------------------------------------------------------------
//
// The scene's offscreen targets are sized `scale × device pixels`, so a
// fixed 3× multiplies an already-high device-pixel ratio. These pin the
// behaviour that keeps that from producing an unallocatable frame (which
// renders as a black canvas, with the error only reaching the console).

use super::{choose_ssaa_scale, scene_bytes_per_pixel, MAX_SSAA_SCALE};

/// WebGPU's default limit tier — what a browser guarantees.
const WEBGPU_MAX_DIM: u32 = 8192;

/// Bytes/px with the 32-bit blendable-float dual-depth slab (the
/// expensive, common case).
fn bpp() -> u32 {
    scene_bytes_per_pixel(16)
}

#[test]
fn desktop_at_dpr_1_keeps_full_quality() {
    // The case the fixed 3× was tuned for must not regress.
    assert_eq!(
        choose_ssaa_scale(1.0, 1280, 400, WEBGPU_MAX_DIM, bpp()),
        MAX_SSAA_SCALE,
    );
}

#[test]
fn high_dpi_screens_supersample_less() {
    // A display that already has the samples doesn't need us to add
    // them: the factor is the quality target divided by the ratio.
    let small = (600u32, 400u32); // small enough that only the DPR term binds
    assert_eq!(choose_ssaa_scale(1.5, small.0, small.1, WEBGPU_MAX_DIM, bpp()), 2);
    assert_eq!(choose_ssaa_scale(2.0, small.0, small.1, WEBGPU_MAX_DIM, bpp()), 2);
    assert_eq!(choose_ssaa_scale(3.0, small.0, small.1, WEBGPU_MAX_DIM, bpp()), 1);
}

/// The regression this whole change exists for. A phone viewport in
/// device pixels at DPR 3: the old fixed 3× asked for targets past the
/// texture limit and ~1.5 GB of VRAM, and the frame silently failed.
#[test]
fn phone_viewport_stays_within_the_texture_limit() {
    // 390×932 CSS px at DPR 3 — a current large phone, full-height pane.
    let (w, h) = (1170u32, 2796u32);
    // What the old constant would have asked for:
    assert!(
        h * MAX_SSAA_SCALE > WEBGPU_MAX_DIM,
        "precondition: fixed 3× must overrun the limit for this to be a regression test",
    );
    let scale = choose_ssaa_scale(3.0, w, h, WEBGPU_MAX_DIM, bpp());
    assert!(w * scale <= WEBGPU_MAX_DIM && h * scale <= WEBGPU_MAX_DIM);
}

#[test]
fn oversized_viewport_steps_down_to_fit_memory() {
    // 4K-wide viewport at DPR 1: the DPR term alone would allow 3×,
    // which is ~2.4 GiB of offscreen targets. The budget must bite.
    let (w, h) = (3840u32, 2160u32);
    let scale = choose_ssaa_scale(1.0, w, h, WEBGPU_MAX_DIM, bpp());
    assert!(scale < MAX_SSAA_SCALE, "expected a step down, got {scale}");
    let bytes = (w * scale) as u64 * (h * scale) as u64 * bpp() as u64;
    assert!(
        bytes <= super::SSAA_MEMORY_BUDGET_BYTES,
        "chose {scale}× = {} MiB, over budget",
        bytes / (1024 * 1024),
    );
}

#[test]
fn scale_is_never_zero_even_in_absurd_cases() {
    // Nothing left to trade away — must still return a usable factor
    // rather than 0, which would produce a zero-sized target.
    let scale = choose_ssaa_scale(4.0, 16_000, 16_000, WEBGPU_MAX_DIM, bpp());
    assert_eq!(scale, 1);
}

#[test]
fn nonsense_device_scale_falls_back_to_full_quality() {
    // A zero / NaN ratio from a shell that never set one must not
    // produce a divide-by-zero or a 0 factor.
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let s = choose_ssaa_scale(bad, 800, 600, WEBGPU_MAX_DIM, bpp());
        assert!((1..=MAX_SSAA_SCALE).contains(&s), "ratio {bad} gave {s}");
    }
}

/// The byte-per-pixel figure drives the budget decision, so it must
/// track the real target list. Cross-checked against the measured
/// allocation report: a 3840×1179 frame reported 328.1 MiB.
#[test]
fn bytes_per_pixel_matches_the_measured_allocation() {
    let measured_bytes = 328.1_f64 * 1024.0 * 1024.0;
    let px = 3840.0_f64 * 1179.0;
    let implied = measured_bytes / px;
    let ours = bpp() as f64;
    assert!(
        (implied - ours).abs() < 1.0,
        "bytes/px drifted from the measured report: implied {implied:.1}, ours {ours:.1}",
    );
}
