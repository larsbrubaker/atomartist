//! Per-body GPU buffer construction — extracted from
//! `scene_renderer/mod.rs` so that file stays under the 800-line
//! guardrail (`atomartist-lib/tests/file_line_count.rs`).
//!
//! The single entry point is [`WgpuSceneRenderer::ensure_body_buffers`],
//! which de-indexes each body's mesh into the non-indexed
//! position/normal, colour, and edge-hint buffers the surface pipelines
//! consume, and repacks the per-body uniform slots. It leans on the
//! small cache-key helpers (`is_opaque_color`, `variant_key`,
//! `pack_color_q`) that live next to [`super::BodyGpu`] in `mod.rs`.

use bytemuck::cast_slice;
use wgpu::util::DeviceExt;

use atomartist_lib::geometry::{is_inherit_color, DEFAULT_GEOMETRY_COLOR};

use super::opaque_pass::Vertex;
use super::{
    body_uniform, is_opaque_color, pack_color_q, render_modes, variant_key, BodyGpu,
    WgpuSceneRenderer,
};

impl WgpuSceneRenderer {
    /// Refresh the per-body GPU cache + the dynamic body-uniform
    /// buffer so they reflect `self.bodies`.
    ///
    /// Strategy: for each body in declaration order, reuse the
    /// existing `bodies_gpu` entry when its `(mesh_ptr, vertex_colors_ptr,
    /// body_color_q, variant_key)` matches; rebuild otherwise. Surplus
    /// entries are dropped.
    ///
    /// Per-body uniforms (model + colour + flags) are repacked into
    /// the dynamic uniform buffer every frame — the body Vec is small
    /// (typically ≤ 16) and the slot write is one `queue.write_buffer`
    /// call, so amortising further isn't worth the bookkeeping.
    ///
    /// Returns `true` when the underlying uniform buffer reallocated
    /// (capacity grew). Callers rebuild any bind group that resolves
    /// against the buffer's identity on a `true` return.
    pub(crate) fn ensure_body_buffers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> bool {
        let bodies = self.bodies.clone();
        let render_style = self.render_style;
        let s = match &mut self.state {
            Some(s) => s,
            None => return false,
        };

        let mut new_cache: Vec<BodyGpu> = Vec::with_capacity(bodies.len());
        let mut taken = vec![false; s.bodies_gpu.len()];

        for body in bodies.iter() {
            let mesh = &body.mesh;
            if mesh.num_prop < 6 || mesh.vert_properties.is_empty() {
                // Skip — degenerate body. Slot still consumes a
                // `BodyUniform` entry below for index parity.
                continue;
            }
            let mesh_ptr = mesh.vert_properties.as_ptr() as usize;
            let vc_ptr = body
                .vertex_colors
                .as_ref()
                .map(|v| v.as_ptr() as usize)
                .unwrap_or(0);
            let color_q = pack_color_q(body.color);
            let variant = variant_key(render_style);

            // Reuse an existing cache entry with matching pointers,
            // tint AND render-mode fingerprint (the tint participates
            // in the cbuf fill when there's no per-vertex overlay; the
            // variant guards the mode-dependent hint buffer).
            let mut reused = false;
            for (i, prev) in s.bodies_gpu.iter().enumerate() {
                if !taken[i]
                    && prev.mesh_ptr == mesh_ptr
                    && prev.vertex_colors_ptr == vc_ptr
                    && prev.body_color_q == color_q
                    && prev.variant_key == variant
                {
                    taken[i] = true;
                    let clone = BodyGpu {
                        mesh_ptr: prev.mesh_ptr,
                        vertex_colors_ptr: prev.vertex_colors_ptr,
                        body_color_q: prev.body_color_q,
                        vbuf: prev.vbuf.clone(),
                        cbuf: prev.cbuf.clone(),
                        hbuf: prev.hbuf.clone(),
                        vertex_count: prev.vertex_count,
                        variant_key: prev.variant_key,
                        opaque: prev.opaque,
                    };
                    new_cache.push(clone);
                    reused = true;
                    break;
                }
            }
            if reused {
                continue;
            }

            // Build fresh — everything de-indexed to 3 verts/triangle
            // (non-indexed draw). This is what lets the surface shaders
            // fold the wireframe into the polygon pass: barycentric comes
            // from `vertex_index % 3`, correct per-triangle even where the
            // source mesh shares vertices between triangles.
            let stride = mesh.num_prop as usize;
            let n_verts = mesh.vert_properties.len() / stride;
            let tri_count = mesh.tri_verts.len() / 3;
            let vertex_count = (tri_count * 3) as u32;

            // Per-source-vertex colour lookup (overlay or uniform tint),
            // read while de-indexing so the colour follows its vertex.
            let src_color = |v: usize| -> [f32; 4] {
                match body.vertex_colors.as_ref() {
                    Some(colors) if colors.len() == n_verts * 4 => {
                        [colors[v * 4], colors[v * 4 + 1], colors[v * 4 + 2], colors[v * 4 + 3]]
                    }
                    _ => body.color,
                }
            };

            // Per-triangle edge hints for the active mode (empty for
            // Shaded / Overhang → zero hints → no wireframe).
            let tri_hints = render_style
                .edge_kind()
                .map(|kind| render_modes::edge_hints(mesh, kind));

            let mut verts: Vec<Vertex> = Vec::with_capacity(tri_count * 3);
            let mut cbuf_data: Vec<f32> = Vec::with_capacity(tri_count * 3 * 4);
            let mut hbuf_data: Vec<f32> = Vec::with_capacity(tri_count * 3 * 3);
            for t in 0..tri_count {
                let hint = tri_hints
                    .as_ref()
                    .and_then(|h| h.get(t).copied())
                    .unwrap_or([0.0, 0.0, 0.0]);
                for k in 0..3 {
                    let v = mesh.tri_verts[t * 3 + k] as usize;
                    let o = v * stride;
                    verts.push(Vertex {
                        pos: [
                            mesh.vert_properties[o],
                            mesh.vert_properties[o + 1],
                            mesh.vert_properties[o + 2],
                        ],
                        normal: [
                            mesh.vert_properties[o + 3],
                            mesh.vert_properties[o + 4],
                            mesh.vert_properties[o + 5],
                        ],
                    });
                    cbuf_data.extend_from_slice(&src_color(v));
                    hbuf_data.extend_from_slice(&hint);
                }
            }

            let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist body vb"),
                contents: cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let cbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist body cb"),
                contents: cast_slice(&cbuf_data),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let hbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist body hb"),
                contents: cast_slice(&hbuf_data),
                usage: wgpu::BufferUsages::VERTEX,
            });

            new_cache.push(BodyGpu {
                mesh_ptr,
                vertex_colors_ptr: vc_ptr,
                body_color_q: color_q,
                vbuf,
                cbuf,
                hbuf,
                vertex_count,
                variant_key: variant,
                opaque: is_opaque_color(body.color),
            });
        }

        s.bodies_gpu = new_cache;

        // Resize + repopulate the dynamic uniform buffer. One slot
        // per body — the slot order matches `bodies_gpu` so a draw
        // call's body index doubles as the uniform-slot index.
        let needed = bodies.len() as u32;
        let realloc = s.body_uniforms.ensure_capacity(device, needed);
        let mut slots: Vec<body_uniform::BodyUniform> = Vec::with_capacity(bodies.len());
        for body in bodies.iter() {
            // Renderer-side fallback for the `INHERIT_COLOR` sentinel:
            // if a body reaches the renderer with alpha = 0, no node
            // along its chain set an explicit colour, so substitute
            // `DEFAULT_GEOMETRY_COLOR` to keep the body visible.
            let color = if is_inherit_color(&body.color) {
                DEFAULT_GEOMETRY_COLOR
            } else {
                body.color
            };
            slots.push(body_uniform::BodyUniform {
                model: body.matrix,
                color,
                // flags.y = Overhang: the shader recolours the surface
                // by world-space slope instead of the per-vertex colour
                // (hardware port of `render_modes::overhang_colors`).
                flags: [
                    body.has_vertex_colors() as u32,
                    render_style.is_overhang() as u32,
                    0,
                    0,
                ],
            });
        }
        if !slots.is_empty() {
            s.body_uniforms.write_slots(queue, &slots);
        }
        realloc
    }
}
