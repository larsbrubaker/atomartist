//! Progressive accumulation anti-aliasing for [`super::WgpuSceneRenderer`].
//!
//! Port of NodeDesigner's [`accumulation-aa.js`](../../../../../FDS/NodeDesigner/static/js/node-editor/rendering/accumulation-aa.js)
//! (which itself ports MatterCAD's `ProgressiveAccumulator`). Each
//! frame renders one sample of the scene with a sub-pixel Halton(2,3)
//! jitter on the projection matrix, then blends that sample into a
//! HDR ping-pong accumulator using `mix(prev, curr, 1 / (N + 1))`.
//! After [`MAX_SAMPLES`] (= 16) samples accumulate the running
//! average is a 16-tap anti-aliased image.
//!
//! ## Scoping
//!
//! Only the 3-D viewport widget participates. The tumble cube,
//! bed-composite chain, and the 2-D `agg-gui` widgets render
//! single-shot; they would gain nothing from temporal accumulation
//! (and it would visibly lag interactive controls).
//!
//! ## Format choices
//!
//! Sample target and both accumulator slots are [`SAMPLE_FORMAT`]
//! (`Rgba16Float`) — matches NodeDesigner's `THREE.HalfFloatType` so
//! the running average has the extra precision needed to avoid
//! visible banding after 16 averages, even when the underlying
//! samples come from an 8-bit dual-peel resolve.
//!
//! ## Sample-count state
//!
//! The driver (`WgpuSceneRenderer`) owns `sample_count` + `accum_read`
//! so the cache module ([`super::cache`]) can reset them when the
//! scene fingerprint changes. This module exposes only the pure
//! Halton helpers + the GPU passes.

use bytemuck::{Pod, Zeroable};

/// Convergence target — matches NodeDesigner's `MAX_SAMPLES`. After
/// 16 samples the running average is visually indistinguishable from
/// an offline 16x supersampled render, while staying cheap enough
/// that the chain converges in <300 ms at 60 Hz.
pub const MAX_SAMPLES: u32 = 16;

/// Linear HDR format for both the per-sample dual-peel resolve and
/// the ping-pong accumulators. Matches NodeDesigner's
/// `THREE.HalfFloatType` choice — the extra precision over 8-bit
/// sRGB is needed because `mix(prev, curr, 1/(N+1))` operations
/// stack quantisation error in the LSBs.
pub const SAMPLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

const BLEND_SHADER: &str = r#"
struct U {
    /// `x` carries the blend weight `1 / (N + 1)`; `yzw` are pad.
    /// Packed in a `vec4<f32>` for std140 alignment.
    blend: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var prev_accum: texture_2d<f32>;
@group(0) @binding(2) var new_sample: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var o: VOut;
    o.clip = vec4<f32>(positions[idx], 0.0, 1.0);
    o.uv = uvs[idx];
    return o;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let prev = textureSample(prev_accum, samp, in.uv);
    let curr = textureSample(new_sample, samp, in.uv);
    return mix(prev, curr, u.blend.x);
}
"#;

const COPY_SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var o: VOut;
    o.clip = vec4<f32>(positions[idx], 0.0, 1.0);
    o.uv = uvs[idx];
    return o;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(src, samp, in.uv);
}
"#;

/// Low-discrepancy sequence used to pick sub-pixel jitter offsets.
/// Mirrors NodeDesigner's `halton(i, base)`. For our usage `base` is
/// always 2 or 3 (the first two coprime primes — gives uncorrelated
/// x / y sequences); `i` is the 1-indexed sample number (`getJitterOffset`
/// skips index 0 to keep the first frame on the pixel centre).
pub fn halton(mut i: u32, base: u32) -> f32 {
    debug_assert!(base >= 2, "Halton base must be >= 2");
    let base_f = base as f32;
    let mut result = 0.0_f32;
    let mut f = 1.0_f32 / base_f;
    while i > 0 {
        result += f * (i % base) as f32;
        i /= base;
        f /= base_f;
    }
    result
}

/// Sub-pixel jitter offset in `[-0.5, 0.5]` pixels. Sample 0 returns
/// `(0, 0)` so the first frame after a scene change shows the un-jittered
/// image immediately; subsequent samples refine via Halton(2,3).
pub fn jitter_offset(sample_idx: u32) -> (f32, f32) {
    if sample_idx == 0 {
        return (0.0, 0.0);
    }
    (halton(sample_idx, 2) - 0.5, halton(sample_idx, 3) - 0.5)
}

/// Shift a column-major 4x4 projection matrix by a sub-pixel amount.
/// Modifies `proj[8]` (column 2, row 0 — `m02`) and `proj[9]` (column
/// 2, row 1 — `m12`) so the perspective divide turns the world-space
/// view-ray shift into a screen-space pixel shift of `(jx, jy)`.
/// Mirrors NodeDesigner's `applyJitter` element edits exactly.
pub fn apply_jitter_to_proj(proj: &mut [f32; 16], jx: f32, jy: f32, w: f32, h: f32) {
    let dx = 2.0 * jx / w.max(1.0);
    let dy = 2.0 * jy / h.max(1.0);
    proj[8] += dx;
    proj[9] += dy;
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlendUniforms {
    blend: [f32; 4],
}

/// Ping-pong accumulators + the per-sample target. All three share
/// [`SAMPLE_FORMAT`] (`Rgba16Float`) so the blend shader reads / writes
/// the same precision.
pub struct AccumulationTargets {
    width: u32,
    height: u32,

    /// Per-sample dual-peel output. Written by the resolve pass, read
    /// by the blend pass.
    pub sample_target: wgpu::Texture,
    pub sample_view: wgpu::TextureView,

    /// `accum[i]` is the i-th ping-pong accumulation slot. On
    /// iteration `N` the driver reads `accum[accum_read]` and writes
    /// `accum[1 - accum_read]`, then swaps. Matches NodeDesigner's
    /// `accumTargets[0/1]`.
    pub accum: [wgpu::Texture; 2],
    pub accum_views: [wgpu::TextureView; 2],
}

impl AccumulationTargets {
    pub fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        let sample_target = alloc(device, w, h, "atomartist accum sample");
        let sample_view = sample_target.create_view(&wgpu::TextureViewDescriptor::default());
        let accum = [
            alloc(device, w, h, "atomartist accum 0"),
            alloc(device, w, h, "atomartist accum 1"),
        ];
        let accum_views = [
            accum[0].create_view(&wgpu::TextureViewDescriptor::default()),
            accum[1].create_view(&wgpu::TextureViewDescriptor::default()),
        ];
        Self {
            width: w,
            height: h,
            sample_target,
            sample_view,
            accum,
            accum_views,
        }
    }

    pub fn ensure_size(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let w = w.max(1);
        let h = h.max(1);
        if w == self.width && h == self.height {
            return;
        }
        *self = Self::new(device, w, h);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Both helper pipelines: a blend pass that runs `mix(prev, curr, w)`
/// from `(accum[read], sample)` into `accum[write]`, and a copy pass
/// that downsamples the final HDR accumulator into the surface-format
/// output texture for the final blit.
pub struct AccumulationPipelines {
    surface_format: wgpu::TextureFormat,

    blend_pipeline: wgpu::RenderPipeline,
    blend_bgl: wgpu::BindGroupLayout,
    blend_ub: wgpu::Buffer,

    copy_pipeline: wgpu::RenderPipeline,
    copy_bgl: wgpu::BindGroupLayout,

    sampler: wgpu::Sampler,
}

impl AccumulationPipelines {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atomartist accum sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let (blend_pipeline, blend_bgl) = build_blend_pipeline(device);
        let blend_ub = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atomartist accum blend ub"),
            size: std::mem::size_of::<BlendUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let (copy_pipeline, copy_bgl) = build_copy_pipeline(device, surface_format);
        Self {
            surface_format,
            blend_pipeline,
            blend_bgl,
            blend_ub,
            copy_pipeline,
            copy_bgl,
            sampler,
        }
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    /// Blend `sample` into the accumulator. Writes the result into
    /// `accum[1 - read_index]` and returns the new `read_index` for
    /// the caller to store. `sample_count` is the count of samples
    /// already in the accumulator *before* this call, so the first
    /// sample passes `0` here (yielding `weight = 1`, i.e. mix picks
    /// `sample` verbatim).
    pub fn execute_blend(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        targets: &AccumulationTargets,
        sample_count_before: u32,
        read_index: u8,
    ) -> u8 {
        let weight = 1.0 / (sample_count_before as f32 + 1.0);
        let u = BlendUniforms {
            blend: [weight, 0.0, 0.0, 0.0],
        };
        queue.write_buffer(&self.blend_ub, 0, bytemuck::bytes_of(&u));

        let write_index = (read_index ^ 1) as usize;
        let read = read_index as usize;
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist accum blend bg"),
            layout: &self.blend_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.blend_ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.accum_views[read]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&targets.sample_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atomartist accum blend"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.accum_views[write_index],
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Always clear — when `weight == 1` (first sample) the
                    // blend ignores `prev` anyway, so loading vs clearing
                    // is equivalent. When `weight < 1` the shader produces
                    // the full output so the clear is also irrelevant; it
                    // just keeps the GPU pipeline predictable.
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let (w, h) = targets.size();
        pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        pass.set_scissor_rect(0, 0, w, h);
        pass.set_pipeline(&self.blend_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);

        write_index as u8
    }

    /// Copy `accum[read_index]` (HDR `Rgba16Float`) into the
    /// surface-format `output_view`, converting linear-to-sRGB
    /// implicitly when the target view's format is sRGB.
    pub fn execute_copy_to_surface(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        targets: &AccumulationTargets,
        read_index: u8,
        output_view: &wgpu::TextureView,
        output_size: (u32, u32),
    ) {
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist accum copy bg"),
            layout: &self.copy_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &targets.accum_views[read_index as usize],
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atomartist accum copy-to-surface"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let (w, h) = output_size;
        pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        pass.set_scissor_rect(0, 0, w, h);
        pass.set_pipeline(&self.copy_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn alloc(device: &wgpu::Device, w: u32, h: u32, label: &'static str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SAMPLE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn build_blend_pipeline(
    device: &wgpu::Device,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("atomartist accum blend bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("atomartist accum blend pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist accum blend shader"),
        source: wgpu::ShaderSource::Wgsl(BLEND_SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist accum blend pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: SAMPLE_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
}

fn build_copy_pipeline(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("atomartist accum copy bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("atomartist accum copy pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist accum copy shader"),
        source: wgpu::ShaderSource::Wgsl(COPY_SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist accum copy pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halton_matches_node_designer() {
        // Reference values produced by running NodeDesigner's
        // `halton(i, 2)` and `halton(i, 3)` in a browser console.
        let h2: [f32; 8] = [0.5, 0.25, 0.75, 0.125, 0.625, 0.375, 0.875, 0.0625];
        let h3: [f32; 8] = [
            1.0 / 3.0,
            2.0 / 3.0,
            1.0 / 9.0,
            4.0 / 9.0,
            7.0 / 9.0,
            2.0 / 9.0,
            5.0 / 9.0,
            8.0 / 9.0,
        ];
        for (i, expected) in h2.iter().enumerate() {
            let got = halton((i as u32) + 1, 2);
            assert!(
                (got - expected).abs() < 1e-6,
                "halton({}, 2): expected {}, got {}",
                i + 1,
                expected,
                got
            );
        }
        for (i, expected) in h3.iter().enumerate() {
            let got = halton((i as u32) + 1, 3);
            assert!(
                (got - expected).abs() < 1e-5,
                "halton({}, 3): expected {}, got {}",
                i + 1,
                expected,
                got
            );
        }
    }

    #[test]
    fn jitter_offset_zero_for_first_sample() {
        assert_eq!(jitter_offset(0), (0.0, 0.0));
    }

    #[test]
    fn jitter_offset_is_centered_in_pixel_for_subsequent_samples() {
        for i in 1..=16 {
            let (x, y) = jitter_offset(i);
            assert!(
                x >= -0.5 && x <= 0.5,
                "jitter x out of [-0.5, 0.5] at i={}: {}",
                i,
                x
            );
            assert!(
                y >= -0.5 && y <= 0.5,
                "jitter y out of [-0.5, 0.5] at i={}: {}",
                i,
                y
            );
        }
    }

    #[test]
    fn apply_jitter_only_touches_m02_and_m12() {
        let mut proj = [0.0_f32; 16];
        for v in proj.iter_mut().enumerate() {
            *v.1 = v.0 as f32; // unique sentinel per slot
        }
        let original = proj;
        apply_jitter_to_proj(&mut proj, 0.25, -0.125, 100.0, 50.0);
        for i in 0..16 {
            if i == 8 || i == 9 {
                continue;
            }
            assert_eq!(
                proj[i], original[i],
                "apply_jitter modified slot {} (expected only 8 and 9 to change)",
                i
            );
        }
        let dx_expected = 2.0 * 0.25 / 100.0;
        let dy_expected = 2.0 * (-0.125) / 50.0;
        assert!(
            (proj[8] - (original[8] + dx_expected)).abs() < 1e-7,
            "m02 mismatch: expected {} + {}, got {}",
            original[8],
            dx_expected,
            proj[8]
        );
        assert!(
            (proj[9] - (original[9] + dy_expected)).abs() < 1e-7,
            "m12 mismatch: expected {} + {}, got {}",
            original[9],
            dy_expected,
            proj[9]
        );
    }

    #[test]
    fn max_samples_matches_node_designer() {
        assert_eq!(MAX_SAMPLES, 16);
    }
}
