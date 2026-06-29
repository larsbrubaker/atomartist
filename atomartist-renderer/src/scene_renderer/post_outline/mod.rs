//! Blender-style post-process selection outline for the 3-D viewport.
//!
//! Port of NodeDesigner's
//! [`selection-outline.js`](../../../../../FDS/NodeDesigner/static/js/node-editor/rendering/selection-outline.js).
//! See [`shaders`] for the GLSL → WGSL behavioural notes and
//! [`pipelines`] for the wgpu wiring details.
//!
//! The outline runs after the dual-peel resolve, drawing on top of
//! the HDR scene composite (`scene_fb`) before the final 3×3 box
//! downsample. It therefore supersamples along with the rest of the
//! scene — the oversized buffer keeps the rim crisp, and the
//! downsample anti-aliases it together with the geometry in one pass.
//!
//! ## Single-mesh assumption
//!
//! NodeDesigner supports multi-mesh selection by drawing each selected
//! mesh with the same ID into the prepass. AtomArtist's renderer
//! currently only tracks one mesh (`WgpuSceneRenderer::mesh`), so the
//! `OutlinePass::render` call rasterises *that* mesh into the ID mask.
//! Multi-mesh selection is a future extension — the prepass shader
//! already writes a constant `1.0`, so additional selected meshes
//! would just need additional draw calls into the same target before
//! the edge-detect quad runs.

pub mod pipelines;
pub mod shaders;

pub use pipelines::{OutlinePipelines, OutlineUniforms};

/// Mesh-handle bundle the renderer hands to
/// [`OutlinePipelines::execute`]. Lives in its own module so the
/// pipeline module can import it without depending on `mod.rs`.
pub mod pipelines_mesh {
    #[derive(Clone, Copy)]
    pub struct Mesh<'a> {
        pub vbuf: &'a wgpu::Buffer,
        pub ibuf: &'a wgpu::Buffer,
        pub index_count: u32,
    }
}

/// Wgpu textures + views the outline chain needs. Held inside the
/// renderer's GpuState; reallocated on resize via
/// [`OutlineTargets::ensure_size`].
pub struct OutlineTargets {
    width: u32,
    height: u32,

    /// `R8Unorm` mask: `1.0` where the selected mesh is rasterised,
    /// `0.0` elsewhere. Sampled by the edge-detect shader.
    pub id_mask: wgpu::Texture,
    pub id_mask_view: wgpu::TextureView,

    /// Hardware depth attachment for the ID prepass — drives
    /// `LessEqual` so the prepass mask reflects the front-most
    /// selected fragment. Not sampled.
    pub id_depth: wgpu::Texture,
    pub id_depth_view: wgpu::TextureView,

    /// `R32Float` mirror of the selected mesh's clip-space depth.
    /// Sampled by the edge-detect shader for the occlusion test.
    pub selected_depth: wgpu::Texture,
    pub selected_depth_view: wgpu::TextureView,
}

impl OutlineTargets {
    pub fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        let (id_mask, id_mask_view) = alloc_color_tex(
            device,
            "atomartist outline id_mask",
            w,
            h,
            pipelines::ID_MASK_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let (id_depth, id_depth_view) = alloc_color_tex(
            device,
            "atomartist outline id_depth",
            w,
            h,
            pipelines::ID_DEPTH_FORMAT,
            // Depth attachment only — never sampled. Same wiring as
            // `util::ensure_scene_depth`.
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let (selected_depth, selected_depth_view) = alloc_color_tex(
            device,
            "atomartist outline selected_depth",
            w,
            h,
            pipelines::SELECTED_DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        Self {
            width: w,
            height: h,
            id_mask,
            id_mask_view,
            id_depth,
            id_depth_view,
            selected_depth,
            selected_depth_view,
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

    #[inline]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

fn alloc_color_tex(
    device: &wgpu::Device,
    label: &'static str,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

#[cfg(test)]
mod tests {
    use super::shaders::{EDGE_DETECT_SHADER, ID_PREPASS_SHADER};

    /// Regression test mirroring the dual-peel / scene-shader GLSL ES
    /// 300 emit tests. Both outline shaders must survive naga's WebGL2
    /// GLSL backend, AND must not bind a shadow sampler (the sentinel
    /// for the "naga treated my depth texture as a shadow sampler"
    /// failure mode).
    #[test]
    fn outline_shaders_emit_glsl_es_300() {
        for (label, wgsl, stage) in [
            ("id fs", ID_PREPASS_SHADER, naga::ShaderStage::Fragment),
            ("id vs", ID_PREPASS_SHADER, naga::ShaderStage::Vertex),
            ("edge fs", EDGE_DETECT_SHADER, naga::ShaderStage::Fragment),
            ("edge vs", EDGE_DETECT_SHADER, naga::ShaderStage::Vertex),
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
            assert!(
                !out.contains("sampler2DShadow"),
                "[{label}] emitted GLSL bound a shadow sampler: {out}"
            );
        }
    }
}
