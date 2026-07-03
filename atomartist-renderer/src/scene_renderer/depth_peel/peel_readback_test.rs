//! GPU read-back validation for the dual-peel chain.
//!
//! These tests drive the REAL [`DualPeelPipelines::execute_chain`] on a
//! headless device with a controlled stack of full-screen transparent
//! layers, then copy the resolved output back to the CPU and assert on
//! the composited pixel. This is how we validate transparency instead
//! of eyeballing screenshots — a mis-composited peel shows up as a
//! black (`combined_alpha≈0`) or blown-out (small-alpha divide) pixel.
//!
//! Skips gracefully when no wgpu adapter is present (CI without a GPU).

use super::pipelines::{DualPeelPipelines, PeelUniforms};
use super::DualPeelTargets;
use crate::scene_renderer::body_uniform::{BodyDrawHandle, BodyUniform, DYN_OFFSET_ALIGN};
use wgpu::util::DeviceExt;

const OUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::PRIMARY;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    eprintln!(
        "adapter FLOAT32_BLENDABLE supported = {}",
        adapter.features().contains(wgpu::Features::FLOAT32_BLENDABLE)
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("atomartist-peel-readback-test"),
        required_features: wgpu::Features::FLOAT32_BLENDABLE,
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        experimental_features: wgpu::ExperimentalFeatures::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue))
}

fn identity() -> [f32; 16] {
    let mut m = [0.0_f32; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

/// Clear a colour attachment to `c` with an empty render pass.
fn clear(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, c: wgpu::Color) {
    let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(c),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

/// Render `layers` (each `(ndc_z, alpha)`) as full-screen quads through
/// the real peel chain and return the resolved RGBA8 buffer (`w*h*4`,
/// top row first). Ambient is forced to 0.5 and diffuse to 0 so every
/// fragment shades to a flat linear 0.5·white → the composite math is
/// exactly predictable and any splotch is unambiguous.
fn render_layers(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    layers: &[(f32, f32)],
) -> Vec<u8> {
    let colored: Vec<(f32, f32, [f32; 3])> =
        layers.iter().map(|&(z, a)| (z, a, [1.0, 1.0, 1.0])).collect();
    render_layers_proj(device, queue, w, h, identity(), 1.0, [0.0, 0.0, 1.0], true, &colored)
}

/// Like [`render_layers`] but with an explicit projection matrix and
/// screen-covering triangle `extent`, so callers can reproduce the real
/// renderer's perspective depth distribution. Each layer's `z` is the
/// value fed into `proj * view(identity) * [x, y, z, 1]` — NDC z for an
/// identity proj, or a view-space depth for a perspective proj.
#[allow(clippy::too_many_arguments)]
fn render_layers_proj(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    proj: [f32; 16],
    extent: f32,
    normal: [f32; 3],
    ambient_only: bool,
    layers: &[(f32, f32, [f32; 3])],
) -> Vec<u8> {
    let mut pipes = DualPeelPipelines::new(device, OUT_FORMAT);
    let targets = DualPeelTargets::new(device, w, h, OUT_FORMAT);

    // Body uniform buffer: one identity/white slot.
    let body_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test body ub"),
        size: DYN_OFFSET_ALIGN as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let body = BodyUniform {
        model: identity(),
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0, 0, 0, 0],
    };
    queue.write_buffer(&body_buf, 0, bytemuck::bytes_of(&body));
    pipes.rebuild_body_bg(device, &body_buf);

    // Geometry: one oversized screen-covering triangle per layer, at the
    // layer's NDC z, normal facing the camera. Vertex = [x,y,z, nx,ny,nz].
    let mut verts: Vec<f32> = Vec::new();
    let mut cols: Vec<f32> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let e = extent;
    for (i, &(z, alpha, rgb)) in layers.iter().enumerate() {
        let base = (i * 3) as u32;
        for &(x, y) in &[(-e, -e), (3.0 * e, -e), (-e, 3.0 * e)] {
            verts.extend_from_slice(&[x, y, z, normal[0], normal[1], normal[2]]);
            cols.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
        }
        idx.extend_from_slice(&[base, base + 1, base + 2]);
    }
    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("test vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let cbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("test cb"),
        contents: bytemuck::cast_slice(&cols),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("test ib"),
        contents: bytemuck::cast_slice(&idx),
        usage: wgpu::BufferUsages::INDEX,
    });
    let handles = [BodyDrawHandle {
        vbuf: &vbuf,
        ibuf: &ibuf,
        cbuf: &cbuf,
        index_count: idx.len() as u32,
        body_index: 0,
    }];

    // Opaque-depth mirror (R32Float) cleared far so nothing is rejected;
    // opaque colour cleared transparent (no opaque behind); HDR-less
    // Rgba8 output for easy readback.
    let mk = |fmt, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage,
            view_formats: &[],
        })
    };
    let opaque_depth = mk(
        wgpu::TextureFormat::R32Float,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let opaque_color = mk(
        OUT_FORMAT,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let out = mk(
        OUT_FORMAT,
        wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    );
    let opaque_depth_v = opaque_depth.create_view(&Default::default());
    let opaque_color_v = opaque_color.create_view(&Default::default());
    let out_v = out.create_view(&Default::default());

    // `ambient_only` → flat 0.5 grey regardless of normal (isolates the
    // composite math). Otherwise use the renderer's real two-light rig
    // (see `WgpuSceneRenderer::new`) so back-face shading is exercised.
    let u = if ambient_only {
        PeelUniforms {
            proj,
            view: identity(),
            light_dir0: [0.0, 0.0, 1.0, 0.0],
            light_dir1: [0.0, 0.0, 1.0, 0.0],
            light_diffuse0: [0.0; 4],
            light_specular0: [0.0; 4],
            light_ambient0: [0.0; 4],
            light_diffuse1: [0.0; 4],
            light_specular1: [0.0; 4],
            global_ambient: [0.5, 0.5, 0.5, 0.0],
            material_specular: [0.0; 4],
            base_color: [1.0, 1.0, 1.0, 1.0],
            params: [1.0, 0.0, 0.0, 0.0],
            resolution: [w as f32, h as f32, super::peel_bias(device), 0.0],
        }
    } else {
        PeelUniforms {
            proj,
            view: identity(),
            light_dir0: [-0.577_350_3, -0.577_350_3, 0.577_350_3, 0.0],
            light_dir1: [0.577_350_3, 0.577_350_3, 0.577_350_3, 0.0],
            light_diffuse0: [0.7, 0.7, 0.7, 0.0],
            light_specular0: [0.0; 4],
            light_ambient0: [0.0; 4],
            light_diffuse1: [0.5, 0.5, 0.5, 0.0],
            light_specular1: [0.0; 4],
            global_ambient: [0.2, 0.2, 0.2, 0.0],
            material_specular: [0.0; 4],
            base_color: [1.0, 1.0, 1.0, 1.0],
            params: [1.0, 0.0, 0.0, 0.0],
            resolution: [w as f32, h as f32, super::peel_bias(device), 0.0],
        }
    };

    let mut encoder = device.create_command_encoder(&Default::default());
    clear(&mut encoder, &opaque_depth_v, wgpu::Color { r: 1.0, g: 0.0, b: 0.0, a: 0.0 });
    clear(&mut encoder, &opaque_color_v, wgpu::Color::TRANSPARENT);
    let iterations = super::iteration_count(super::DEFAULT_LAYERS as i32);
    pipes.execute_chain(
        device,
        queue,
        &mut encoder,
        &targets,
        &opaque_depth_v,
        &opaque_color_v,
        &out_v,
        &handles,
        &u,
        iterations,
    );
    queue.submit(std::iter::once(encoder.finish()));

    readback_rgba8(device, queue, &out, w, h)
}

fn readback_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let unpadded = w * 4;
    let padded = unpadded.div_ceil(ALIGN) * ALIGN;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded as u64) * (h as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit(std::iter::once(enc.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().unwrap().unwrap();
    let view = slice.get_mapped_range();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h as usize {
        let start = row * padded as usize;
        out.extend_from_slice(&view[start..start + unpadded as usize]);
    }
    drop(view);
    staging.unmap();
    out
}

fn center(px: &[u8], w: u32, h: u32) -> [u8; 4] {
    let (cx, cy) = (w / 2, h / 2);
    let i = ((cy * w + cx) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

/// Two well-separated 50%-alpha layers over a transparent background
/// must composite to a clean grey — NOT a black or blown-out splotch.
///
/// Hand-computed expectation (ambient-only shade → linear 0.5 grey,
/// alpha 0.5 per layer):
///   front_accum = (0.25, .., 0.5)   back_accum = (0.25, .., 0.5)
///   remaining = 0.5·(1-0.5) = 0.25 → combined_alpha = 0.75
///   rgb = (0.25 + 0.5·0.25) / 0.75 = 0.5
/// so the resolved pixel is ≈ (128, 128, 128, 191).
#[test]
fn two_layers_composite_to_clean_grey() {
    let Some((device, queue)) = headless() else {
        eprintln!("two_layers_composite_to_clean_grey: no wgpu adapter, skipping");
        return;
    };
    let (w, h) = (16u32, 16u32);
    let px = render_layers(&device, &queue, w, h, &[(0.3, 0.5), (0.7, 0.5)]);
    let c = center(&px, w, h);
    assert!(
        (c[3] as i32 - 191).abs() <= 6,
        "combined alpha must be ~0.75 (191); got {c:?} — a black/empty \
         result means the peel captured no layer",
    );
    for ch in 0..3 {
        assert!(
            (c[ch] as i32 - 128).abs() <= 10,
            "channel {ch} must be ~0.5 (128); got {c:?} — a 0 (black) or \
             255 (blown out) channel is the splotch failure",
        );
    }
}

/// The elephant fails where MANY overlapping shells stack up. Eight
/// 25%-alpha layers must still composite to a sane, monotonically
/// building grey — never black, never fully saturated white.
#[test]
fn deep_stack_does_not_splotch() {
    let Some((device, queue)) = headless() else {
        eprintln!("deep_stack_does_not_splotch: no wgpu adapter, skipping");
        return;
    };
    let (w, h) = (16u32, 16u32);
    let layers: Vec<(f32, f32)> = (0..8).map(|i| (0.1 + i as f32 * 0.08, 0.25)).collect();
    let px = render_layers(&device, &queue, w, h, &layers);
    let c = center(&px, w, h);
    assert!(
        c[3] > 20 && c[3] < 250,
        "combined alpha must be a real partial-coverage value, got {c:?} \
         (near-0 = black splotch, 255 = fully-opaque blow-out)",
    );
    for ch in 0..3 {
        assert!(
            c[ch] > 5 && c[ch] < 250,
            "channel {ch} must be a mid grey, got {c:?} (0 = black splotch, \
             255 = blown-out white splotch)",
        );
    }
}

/// REPRODUCER for the elephant. A perspective projection compresses
/// depth toward z≈1, where the `Rgba16Float` dual-depth has ~1e-3 ULP —
/// the same order as `PEEL_BIAS`. Overlapping shells then land within a
/// single quantisation step and the peel can't separate them. Six
/// shells packed into `[0.990, 0.995]` (≈1e-3 apart, near z=1) must
/// STILL composite to a sane grey — if this splotches, the fix is depth
/// precision, not the peel logic.
#[test]
fn compressed_depth_layers_do_not_splotch() {
    let Some((device, queue)) = headless() else {
        eprintln!("compressed_depth_layers_do_not_splotch: no wgpu adapter, skipping");
        return;
    };
    let (w, h) = (16u32, 16u32);
    let layers: Vec<(f32, f32)> = (0..6).map(|i| (0.990 + i as f32 * 0.001, 0.5)).collect();
    let px = render_layers(&device, &queue, w, h, &layers);
    let c = center(&px, w, h);
    // Six stacked 50% layers are legitimately near-opaque; only a black
    // (0) or blown-out (255) channel is the splotch failure.
    for ch in 0..3 {
        assert!(
            c[ch] > 5 && c[ch] < 250,
            "channel {ch} of compressed shells must be mid grey, got {c:?}",
        );
    }
    assert!(c[3] > 20, "compressed shells must have real coverage, got {c:?}");
}

/// FAITHFUL elephant reproducer: real perspective projection. The
/// renderer projects with `Mat4::perspective_rh`, which packs the whole
/// scene's depth into a thin sliver near NDC z=1. Eight 50%-alpha shells
/// spread across view-space depth −5..−12 (a solid ~elephant-sized
/// object) land within a couple of half-float ULPs of each other. If the
/// dual-depth precision is the culprit, THIS is where black/white
/// splotches appear.
#[test]
fn perspective_depth_layers_do_not_splotch() {
    let Some((device, queue)) = headless() else {
        eprintln!("perspective_depth_layers_do_not_splotch: no wgpu adapter, skipping");
        return;
    };
    let (w, h) = (16u32, 16u32);
    let proj = glam::Mat4::perspective_rh(45.0_f32.to_radians(), 1.0, 0.1, 100.0)
        .to_cols_array();
    // Eight shells at view-space z −5 .. −12 (camera looks down −z).
    let layers: Vec<(f32, f32, [f32; 3])> =
        (0..8).map(|i| (-5.0 - i as f32, 0.5, [1.0, 1.0, 1.0])).collect();
    let px = render_layers_proj(&device, &queue, w, h, proj, 60.0, [0.0, 0.0, 1.0], true, &layers);
    let c = center(&px, w, h);
    eprintln!("perspective 8-shell centre pixel = {c:?}");
    for ch in 0..3 {
        assert!(
            c[ch] > 5 && c[ch] < 250,
            "channel {ch} must be a mid grey under perspective projection, \
             got {c:?} — 0 (black) / 255 (white) is the elephant splotch",
        );
    }
}

/// A transparent object shows BOTH faces. A back face (normal pointing
/// away from the camera) must still be lit — with two-sided shading its
/// normal is flipped toward the viewer, so it reads about as bright as
/// the equivalent front face. Without that flip a back face collapses to
/// the ambient floor, and a translucent mesh becomes a mess of
/// dark-back / bright-front patches — the elephant's "splotches".
///
/// This is the REPRODUCER: a single back-facing lit layer. It fails
/// while the shader lights the raw (away-facing) normal and passes once
/// two-sided shading is restored.
#[test]
fn back_face_is_lit_like_front() {
    let Some((device, queue)) = headless() else {
        eprintln!("back_face_is_lit_like_front: no wgpu adapter, skipping");
        return;
    };
    let (w, h) = (16u32, 16u32);
    // Real perspective so view-space z is negative (a valid view space);
    // single 50%-alpha layer at z=-5, centre pixel rgb ≈ shaded value.
    let proj = glam::Mat4::perspective_rh(45.0_f32.to_radians(), 1.0, 0.1, 100.0)
        .to_cols_array();
    let front = render_layers_proj(
        &device, &queue, w, h, proj, 60.0, [0.0, 0.0, 1.0], false, &[(-5.0, 0.5, [1.0, 1.0, 1.0])],
    );
    let back = render_layers_proj(
        &device, &queue, w, h, proj, 60.0, [0.0, 0.0, -1.0], false, &[(-5.0, 0.5, [1.0, 1.0, 1.0])],
    );
    let (cf, cb) = (center(&front, w, h), center(&back, w, h));
    eprintln!("front-face lit = {cf:?}   back-face lit = {cb:?}");
    // The front face is clearly lit (well above the ~0.2 ambient floor).
    assert!(cf[0] > 120, "sanity: front face should be lit, got {cf:?}");
    // The back face must be lit to a similar brightness — NOT collapsed
    // to the dark ambient-only floor.
    assert!(
        (cb[0] as i32 - cf[0] as i32).abs() < 40,
        "back face must shade like the front (two-sided lighting); got \
         front {cf:?} vs back {cb:?} — a dark back face is the transparent \
         mesh's dark/bright patch failure",
    );
}

/// THE painter's-algorithm reproducer. A near RED layer and a far BLUE
/// layer, with BLUE drawn first (draw-order ≠ depth-order). Depth-correct
/// peeling always puts the near red in front → the pixel is red-dominant
/// regardless of draw order. If the two layers fall inside one `PEEL_BIAS`
/// band (as a perspective-compressed solid does with our half-float
/// depth), the peel can't separate them and they blend in DRAW order →
/// the first-drawn far blue wins → blue-dominant. That draw-order result
/// is exactly the "painter's algorithm" the elephant shows.
///
/// The well-separated control proves the harness sees red-in-front; the
/// tightly-spaced case is the failure.
#[test]
fn near_layer_wins_regardless_of_draw_order() {
    let Some((device, queue)) = headless() else {
        eprintln!("near_layer_wins_regardless_of_draw_order: no wgpu adapter, skipping");
        return;
    };
    let (w, h) = (16u32, 16u32);
    let red = [1.0, 0.0, 0.0];
    let blue = [0.0, 0.0, 1.0];
    let n = [0.0, 0.0, 1.0];
    // Draw order is [far-blue, near-red]; NDC z: smaller = nearer.
    let separated = render_layers_proj(
        &device, &queue, w, h, identity(), 1.0, n, true,
        &[(0.70, 0.5, blue), (0.30, 0.5, red)],
    );
    let cs = center(&separated, w, h);
    eprintln!("separated (control) = {cs:?}");
    assert!(cs[0] > cs[2], "control: near red must be in front, got {cs:?}");

    // Same, but the two layers are 5e-5 apart — BELOW half-float depth
    // precision near z=0.5 (ULP ≈ 4.9e-4). In `Rgba16Float` they collapse
    // to one bucket and no bias can separate them; only 32-bit depth can.
    let packed = render_layers_proj(
        &device, &queue, w, h, identity(), 1.0, n, true,
        &[(0.50005, 0.5, blue), (0.50000, 0.5, red)],
    );
    let cp = center(&packed, w, h);
    eprintln!("packed (within bias) = {cp:?}");
    assert!(
        cp[0] > cp[2],
        "near red must still win when layers are close in depth; got {cp:?} \
         — blue-dominant means the peel fell back to draw-order (painter's) \
         because the layers landed inside one PEEL_BIAS band",
    );
}

/// Coincident shells (identical NDC z — what a CSG union leaves along
/// internal walls) must not produce a black or blown-out pixel: the
/// peel has to classify them deterministically, not divide by a
/// near-zero alpha.
#[test]
fn coincident_layers_do_not_splotch() {
    let Some((device, queue)) = headless() else {
        eprintln!("coincident_layers_do_not_splotch: no wgpu adapter, skipping");
        return;
    };
    let (w, h) = (16u32, 16u32);
    let px = render_layers(&device, &queue, w, h, &[(0.5, 0.5), (0.5, 0.5)]);
    let c = center(&px, w, h);
    assert!(
        c[3] > 20 && c[3] < 250 && c[0] > 5 && c[0] < 250,
        "coincident layers must resolve to a stable grey, got {c:?}",
    );
}
