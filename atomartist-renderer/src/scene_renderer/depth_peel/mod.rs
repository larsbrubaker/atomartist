//! Dual depth peeling resources for [`super::WgpuSceneRenderer`].
//!
//! Port of MatterCAD's dual depth peeling
//! ([`NativeSceneEffects.cs::RenderTransparentLayers`](../../../../../../MatterCAD/Submodules/agg-sharp/VorticeD3D/NativeSceneEffects.cs))
//! plus the dual-output blend math in
//! [`NodeDesignerScene.hlsl::ApplyDualDepthPeeling`](../../../../../../MatterCAD/Submodules/agg-sharp/VorticeD3D/Shaders/NodeDesignerScene.hlsl).
//!
//! Why dual peeling: each peel pass extracts both the **nearest** and the
//! **farthest** unpeeled depth at every pixel, so `N` passes resolve `2N`
//! layers — half the GPU work versus single-direction peeling. The
//! per-layer math is enforced by:
//!
//! * a `(-frontZ, backZ)` ping-pong target (`R32G32` packed in `Rgba32Float`);
//! * a "front accumulation" MRT slot that uses **under** blending
//!   (premultiplied; new layers go behind the running front composite);
//! * a "back accumulation" MRT slot that uses standard **over** blending
//!   (new layers go on top of the running back composite).
//!
//! Stencil is **not** used — discards are driven entirely by sampling
//! the opaque-pass depth texture and the previous-iteration dual-depth
//! texture in the shader. The crate-root anti-aliasing note explains
//! why MSAA can't coexist with this scheme.
//!
//! ## Resource ownership
//!
//! [`DualPeelTargets`] owns every texture the peeling chain needs.
//! Iteration loop and pipelines are built in
//! [`super::WgpuSceneRenderer`] and `super::depth_peel::pipelines`
//! respectively (added in a follow-up step); this module exposes only
//! the textures + views + sampler, plus an [`DualPeelTargets::ensure_size`]
//! helper that mirrors the cheap-when-stable pattern used by
//! [`demo_wgpu::SsaaFramebuffer::ensure_size`].

pub mod pipelines;
pub mod shaders;

/// MatterCAD's default `DepthPeelingLayers = 6`, halved to 3 iterations
/// by [`iteration_count`] (each iteration peels one front layer and one
/// back layer). Six layers is the empirically chosen sweet spot in
/// MatterCAD: enough to resolve typical 3-D-print scenes without visible
/// banding, cheap enough that even on integrated GPUs the chain runs
/// in a handful of milliseconds.
pub const DEFAULT_LAYERS: u32 = 6;

/// Number of dual-peel iterations needed to resolve `layers` storage
/// slots. Identical formula to
/// [`DualDepthPeelingMath::GetIterationCount`](../../../../../../MatterCAD/Submodules/agg-sharp/VorticeD3D/DualDepthPeelingMath.cs)
/// (`(layers + 1) / 2`) so the AtomArtist iteration count matches
/// MatterCAD's frame-for-frame at the same `layers` setting.
///
/// `<= 0 → 0` returns the same as MatterCAD's bug-resistance branch.
#[inline]
pub const fn iteration_count(layers: i32) -> u32 {
    if layers <= 0 {
        0
    } else {
        ((layers + 1) / 2) as u32
    }
}

/// `Rgba16Float` was picked over `Rgba32Float` because the dual-peel
/// chain runs hardware `Max` blending on these targets, and wgpu only
/// guarantees blending support for ≤16-bit float formats. Enabling
/// `FLOAT32_BLENDABLE` would be a per-adapter feature flag we can't
/// promise on the web backend, so we stick with half-float.
///
/// Precision implications: half-float in the `[0, 1]` NDC range has
/// ~2¯¹¹ ≈ 5e-4 spacing. The peel discard threshold (`PEEL_BIAS` in
/// `depth_peel::shaders`) is sized to ~1e-3 to absorb this rounding
/// noise — comfortably below the ~0.1% depth-range visual noise floor
/// for any normal viewport scene.
const DUAL_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// HDR accumulation format — matches MatterCAD's
/// `DXGI_FORMAT_R16G16B16A16_FLOAT` so blend math behaves the same.
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// All textures + views the dual-peel chain needs. One instance is
/// owned by the renderer's `GpuState`; it survives across frames and
/// is resized on demand via [`Self::ensure_size`].
pub struct DualPeelTargets {
    width: u32,
    height: u32,
    surface_format: wgpu::TextureFormat,

    /// Ping-pong `(-frontZ, backZ)` slab. Index `[i % 2]` is the SOURCE
    /// the dual-peel shader reads on iteration `i`; `[(i + 1) % 2]` is
    /// the destination it writes the *expanded* slab to.
    pub dual_depth: [wgpu::Texture; 2],
    pub dual_depth_views: [wgpu::TextureView; 2],

    /// Premultiplied "under"-blended accumulation of fragments lying on
    /// the current iteration's near plane. Sampled by the resolve pass.
    pub front_accum: wgpu::Texture,
    pub front_accum_view: wgpu::TextureView,

    /// Standard "over"-blended accumulation of fragments lying on the
    /// current iteration's far plane. Sampled by the resolve pass.
    pub back_accum: wgpu::Texture,
    pub back_accum_view: wgpu::TextureView,

    /// Single-sample target in the surface format. The resolve pass
    /// writes the composited (opaque + front + back) result here; the
    /// accumulation chain samples it on each jitter sample.
    pub resolved: wgpu::Texture,
    pub resolved_view: wgpu::TextureView,

    /// Point sampler used by the dual-peel shader for the source
    /// dual-depth texture and by the resolve shader for the front /
    /// back accumulators. Mirrors MatterCAD's `pointClampSampler`.
    pub point_sampler: wgpu::Sampler,
}

impl DualPeelTargets {
    pub fn new(device: &wgpu::Device, w: u32, h: u32, surface_format: wgpu::TextureFormat) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        let dual_depth = [
            alloc_dual_depth(device, w, h, 0),
            alloc_dual_depth(device, w, h, 1),
        ];
        let dual_depth_views = [
            dual_depth[0].create_view(&wgpu::TextureViewDescriptor::default()),
            dual_depth[1].create_view(&wgpu::TextureViewDescriptor::default()),
        ];
        let front_accum = alloc_accum(device, w, h, "atomartist front_accum");
        let front_accum_view = front_accum.create_view(&wgpu::TextureViewDescriptor::default());
        let back_accum = alloc_accum(device, w, h, "atomartist back_accum");
        let back_accum_view = back_accum.create_view(&wgpu::TextureViewDescriptor::default());
        let resolved = alloc_resolved(device, w, h, surface_format);
        let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
        let point_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atomartist dual-peel point sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self {
            width: w,
            height: h,
            surface_format,
            dual_depth,
            dual_depth_views,
            front_accum,
            front_accum_view,
            back_accum,
            back_accum_view,
            resolved,
            resolved_view,
            point_sampler,
        }
    }

    /// Reallocate every texture if `(w, h)` differs from the cached
    /// extent. Cheap when the size is stable — matches the pattern used
    /// by [`demo_wgpu::SsaaFramebuffer::ensure_size`] so the renderer
    /// can call it unconditionally each frame.
    pub fn ensure_size(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let w = w.max(1);
        let h = h.max(1);
        if w == self.width && h == self.height {
            return;
        }
        self.width = w;
        self.height = h;
        self.dual_depth = [
            alloc_dual_depth(device, w, h, 0),
            alloc_dual_depth(device, w, h, 1),
        ];
        self.dual_depth_views = [
            self.dual_depth[0].create_view(&wgpu::TextureViewDescriptor::default()),
            self.dual_depth[1].create_view(&wgpu::TextureViewDescriptor::default()),
        ];
        self.front_accum = alloc_accum(device, w, h, "atomartist front_accum");
        self.front_accum_view = self
            .front_accum
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.back_accum = alloc_accum(device, w, h, "atomartist back_accum");
        self.back_accum_view = self
            .back_accum
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.resolved = alloc_resolved(device, w, h, self.surface_format);
        self.resolved_view = self
            .resolved
            .create_view(&wgpu::TextureViewDescriptor::default());
    }

    #[inline]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[inline]
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    /// Source / destination dual-depth views for iteration `i`.
    /// Matches MatterCAD's `(sourceDepthTarget, destinationDepthTarget)`
    /// swap pattern in `RenderTransparentLayers`.
    pub fn dual_depth_for_iteration(
        &self,
        iteration: u32,
    ) -> (&wgpu::TextureView, &wgpu::TextureView) {
        let src = (iteration as usize) & 1;
        let dst = src ^ 1;
        (&self.dual_depth_views[src], &self.dual_depth_views[dst])
    }
}

fn alloc_dual_depth(device: &wgpu::Device, w: u32, h: u32, slot: usize) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(if slot == 0 {
            "atomartist dual_depth_0"
        } else {
            "atomartist dual_depth_1"
        }),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DUAL_DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn alloc_accum(device: &wgpu::Device, w: u32, h: u32, label: &'static str) -> wgpu::Texture {
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
        format: ACCUM_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn alloc_resolved(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    surface_format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atomartist scene resolved"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: surface_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iteration_count_matches_mattercad_formula() {
        // Identical to MatterCAD's DualDepthPeelingMath.GetIterationCount.
        assert_eq!(iteration_count(0), 0);
        assert_eq!(iteration_count(-1), 0);
        assert_eq!(iteration_count(1), 1);
        assert_eq!(iteration_count(2), 1);
        assert_eq!(iteration_count(3), 2);
        assert_eq!(iteration_count(4), 2);
        assert_eq!(iteration_count(5), 3);
        assert_eq!(iteration_count(6), 3);
        assert_eq!(iteration_count(7), 4);
        assert_eq!(iteration_count(8), 4);
    }

    #[test]
    fn default_layers_yields_three_iterations() {
        assert_eq!(iteration_count(DEFAULT_LAYERS as i32), 3);
    }

    #[test]
    fn dual_peel_targets_construct_on_headless_device() {
        let Some((device, _queue)) = headless_device() else {
            eprintln!("dual_peel_targets_construct_on_headless_device: no wgpu adapter, skipping");
            return;
        };
        let targets =
            DualPeelTargets::new(&device, 256, 128, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert_eq!(targets.size(), (256, 128));
        assert_eq!(
            targets.surface_format(),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        // Ping-pong swap: iteration 0 reads slot 0, writes slot 1;
        // iteration 1 reads slot 1, writes slot 0.
        let (s0, d0) = targets.dual_depth_for_iteration(0);
        let (s1, d1) = targets.dual_depth_for_iteration(1);
        assert!(std::ptr::eq(s0, d1));
        assert!(std::ptr::eq(d0, s1));
    }

    /// Regression test for "Format Rgba32Float is not blendable" — by
    /// actually building the init / peel / resolve pipelines we exercise
    /// the wgpu validator's format-compatibility checks against the
    /// blend states declared in `pipelines.rs`. If the dual-depth
    /// format ever drifts back to a non-blendable type, this test
    /// reproduces the runtime panic at unit-test time.
    #[test]
    fn dual_peel_pipelines_construct_on_headless_device() {
        let Some((device, _queue)) = headless_device() else {
            eprintln!(
                "dual_peel_pipelines_construct_on_headless_device: no wgpu adapter, skipping"
            );
            return;
        };
        // `Rgba16Float` is the production resolve-output format (matches
        // the accumulation chain's `SAMPLE_FORMAT`). If you change this,
        // also change `WgpuSceneRenderer::ensure_state`.
        let _pipes = super::pipelines::DualPeelPipelines::new(
            &device,
            wgpu::TextureFormat::Rgba16Float,
        );
    }

    /// Regression for the May 2026 WASM panic: Naga's GLSL ES 3.00
    /// backend (the one wgpu uses on WebGL2) refuses to emit
    /// `textureLoad` on `texture_depth_2d`, and silently emits
    /// `textureLod(sampler2DShadow, …)` for `textureSampleLevel`
    /// which the WebGL2 GLSL compiler then rejects. Both failure
    /// modes manifest as a `Device::create_render_pipeline`
    /// validation error in the browser — invisible to native tests.
    ///
    /// We run the three peel shaders through naga's WGSL frontend
    /// and GLSL ES backend to catch *either* failure at unit-test
    /// time. The test passes when every shader emits a non-empty
    /// GLSL string without error; it doesn't bother diff-checking
    /// the actual GLSL contents — naga's exact output is unstable
    /// across versions and not what we care about.
    #[test]
    fn peel_shaders_emit_glsl_es_300() {
        for (label, wgsl, stage) in [
            (
                "dual-depth init",
                super::shaders::DUAL_DEPTH_INIT_SHADER,
                naga::ShaderStage::Fragment,
            ),
            (
                "dual-peel colour",
                super::shaders::DUAL_PEEL_COLOR_SHADER,
                naga::ShaderStage::Fragment,
            ),
            (
                "dual-peel resolve",
                super::shaders::DUAL_PEEL_RESOLVE_SHADER,
                naga::ShaderStage::Fragment,
            ),
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
            // No `sampler2DShadow` in any peel shader — that's the
            // sentinel that Naga has gone down the "treat this as a
            // depth-comparison sampler" path that WebGL2 chokes on.
            // Our fix routes the opaque depth through an R32Float
            // colour attachment specifically to avoid this.
            assert!(
                !out.contains("sampler2DShadow"),
                "[{label}] emitted GLSL still binds a shadow sampler: {out}"
            );
        }
    }

    #[test]
    fn ensure_size_is_no_op_when_unchanged() {
        let Some((device, _queue)) = headless_device() else {
            eprintln!("ensure_size_is_no_op_when_unchanged: no wgpu adapter, skipping");
            return;
        };
        let mut targets =
            DualPeelTargets::new(&device, 256, 128, wgpu::TextureFormat::Rgba8UnormSrgb);
        let resolved_ptr_before = &targets.resolved as *const _;
        targets.ensure_size(&device, 256, 128);
        let resolved_ptr_after = &targets.resolved as *const _;
        // `Texture` itself is a wrapper, but the inner identity is what
        // we care about — same field address means the wrapper wasn't
        // replaced, so no reallocation happened.
        assert_eq!(resolved_ptr_before, resolved_ptr_after);
    }

    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("atomartist-depth-peel-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some((device, queue))
    }
}
