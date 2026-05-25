//! Per-body dynamic uniform buffer used by every per-body pipeline
//! (opaque scene, depth-only, dual-peel init, dual-peel colour).
//!
//! ## Why dynamic offsets
//!
//! Multi-body rendering requires a separate `model` matrix + body
//! colour + flags per draw call. Three viable patterns:
//!
//! 1. **Separate uniform buffer per body.** Simple but costs an
//!    allocation + bind-group rebuild per body each time the list
//!    changes. Doesn't scale to 100+ bodies.
//! 2. **`queue.write_buffer` between draws.** Breaks: wgpu coalesces
//!    `write_buffer` calls to before any GPU work in the submit, so
//!    every draw would see the *last* upload.
//! 3. **Single buffer with dynamic offsets.** Standard wgpu / WebGPU
//!    pattern. Write all N body slots once per frame; per draw, bind
//!    the same bind group with the body's slot offset. One alloc,
//!    one bind group, N draw calls.
//!
//! We use pattern (3). The buffer grows when `bodies.len()` exceeds
//! capacity; the bind group is rebuilt only when the buffer
//! reallocates (host-side cheap, GPU resident moves are infrequent).
//!
//! ## Slot alignment
//!
//! WebGPU `min_uniform_buffer_offset_alignment` is 256 bytes (the
//! pessimistic floor required by every backend). `BodyUniform` is
//! 96 bytes; we pad to 256 so each dynamic offset is `body_index * 256`.

use bytemuck::{Pod, Zeroable};

/// Per-body draw handle shared by every per-body pipeline (opaque,
/// dual-peel, shadow caster, future outline). The `body_index` indexes
/// the dynamic uniform buffer — the slot at that index holds the
/// matching [`BodyUniform`] written during
/// [`crate::scene_renderer::WgpuSceneRenderer::ensure_body_buffers`].
///
/// `cbuf` is the per-vertex colour buffer bound at vertex-buffer
/// slot 1. Always populated by the renderer — either with the source
/// body's `vertex_colors` overlay or a fill of its uniform colour
/// repeated per vertex. See
/// [`crate::scene_renderer::BodyGpu`] for the build-time branch.
#[derive(Clone, Copy)]
pub struct BodyDrawHandle<'a> {
    pub vbuf: &'a wgpu::Buffer,
    pub ibuf: &'a wgpu::Buffer,
    pub cbuf: &'a wgpu::Buffer,
    pub index_count: u32,
    pub body_index: u32,
}

/// Per-body uniform consumed by every per-body pipeline.
///
/// Layout must match the WGSL `B` struct used in `opaque_shaders.rs`
/// and `depth_peel/shaders.rs`.
///
/// * `model` — column-major body transform. The vertex shader applies
///   it before the camera view so per-body translation / rotation /
///   scale work without re-uploading vertices.
/// * `color` — RGBA body tint. Multiplied with the per-vertex colour
///   attribute when `flags.x != 0`; used alone otherwise.
/// * `flags` — packed booleans + spare slots. `flags[0]` is
///   `use_vertex_colors` (0 = uniform colour path, 1 = vertex-colour
///   multiply path); `flags[1..4]` reserved.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct BodyUniform {
    pub model: [f32; 16],
    pub color: [f32; 4],
    pub flags: [u32; 4],
}

impl BodyUniform {
    pub fn identity() -> Self {
        let mut m = [0.0_f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        Self {
            model: m,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [0, 0, 0, 0],
        }
    }
}

/// Minimum dynamic-offset alignment required by WebGPU. The wgpu
/// spec exposes this through `Limits::min_uniform_buffer_offset_alignment`
/// but the universally-safe value is 256 bytes — every browser
/// implementation honours this floor.
pub const DYN_OFFSET_ALIGN: u32 = 256;

/// Compute the byte offset for the `body_index`'th slot in the
/// dynamic uniform buffer.
#[inline]
pub fn slot_offset(body_index: u32) -> u32 {
    body_index * DYN_OFFSET_ALIGN
}

/// Number of bytes required to hold `capacity` body slots.
#[inline]
pub fn buffer_size(capacity: u32) -> u64 {
    (capacity.max(1) as u64) * DYN_OFFSET_ALIGN as u64
}

/// Helper that owns the dynamic uniform buffer + its bind-group
/// layout entry. The bind group itself is rebuilt by the pipeline
/// module that consumes this (so the bind group can include other
/// bindings like textures alongside the body uniform).
pub struct BodyUniformBuffer {
    /// GPU buffer. `None` until the first `ensure_capacity` call.
    pub buffer: Option<wgpu::Buffer>,
    /// Number of slots currently allocated.
    pub capacity: u32,
}

impl BodyUniformBuffer {
    pub fn new() -> Self {
        Self {
            buffer: None,
            capacity: 0,
        }
    }

    /// Grow the buffer (and signal a reallocation) so it can hold at
    /// least `needed` slots. Returns `true` when the buffer was
    /// reallocated — callers rebuild any cached bind group on `true`.
    pub fn ensure_capacity(&mut self, device: &wgpu::Device, needed: u32) -> bool {
        let needed = needed.max(1);
        if self.buffer.is_some() && self.capacity >= needed {
            return false;
        }
        // Grow geometrically to amortise reallocs as the body count
        // climbs (e.g. on a multi-body 3MF import).
        let new_capacity = needed.next_power_of_two().max(4);
        self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atomartist body uniforms"),
            size: buffer_size(new_capacity),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.capacity = new_capacity;
        true
    }

    /// Write `bodies` worth of slots into the buffer. Each slot is
    /// padded to `DYN_OFFSET_ALIGN` so dynamic offsets can index by
    /// `body_index * align`.
    pub fn write_slots(&self, queue: &wgpu::Queue, slots: &[BodyUniform]) {
        let buffer = match &self.buffer {
            Some(b) => b,
            None => return,
        };
        // Build a single contiguous byte vec with per-slot padding —
        // one `write_buffer` call beats N individual writes when the
        // body count is high.
        let mut bytes = vec![0_u8; (slots.len() as u64 * DYN_OFFSET_ALIGN as u64) as usize];
        for (i, slot) in slots.iter().enumerate() {
            let off = i * DYN_OFFSET_ALIGN as usize;
            let src = bytemuck::bytes_of(slot);
            bytes[off..off + src.len()].copy_from_slice(src);
        }
        queue.write_buffer(buffer, 0, &bytes);
    }

    /// Build a [`wgpu::BindingResource`] that references the first
    /// slot's worth of bytes — used by pipelines whose bind group
    /// layout entry has `has_dynamic_offset = true`.
    pub fn binding_resource(&self) -> Option<wgpu::BindingResource<'_>> {
        let buffer = self.buffer.as_ref()?;
        Some(wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer,
            offset: 0,
            size: std::num::NonZeroU64::new(std::mem::size_of::<BodyUniform>() as u64),
        }))
    }
}

impl Default for BodyUniformBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_offset_scales_with_index() {
        assert_eq!(slot_offset(0), 0);
        assert_eq!(slot_offset(1), 256);
        assert_eq!(slot_offset(5), 1280);
    }

    #[test]
    fn buffer_size_rounds_up_to_align() {
        assert_eq!(buffer_size(1), 256);
        assert_eq!(buffer_size(4), 1024);
        assert_eq!(buffer_size(0), 256, "zero capacity bumps to one slot");
    }

    #[test]
    fn body_uniform_layout_matches_wgsl() {
        // model 64 + color 16 + flags 16 = 96.
        assert_eq!(std::mem::size_of::<BodyUniform>(), 96);
        // Pod requires 4-byte alignment; bytemuck guarantees no padding.
        assert_eq!(std::mem::align_of::<BodyUniform>(), 4);
    }

    #[test]
    fn identity_uniform_is_white_no_vertex_colors() {
        let u = BodyUniform::identity();
        assert_eq!(u.color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(u.flags[0], 0);
        assert_eq!(u.model[0], 1.0);
        assert_eq!(u.model[5], 1.0);
        assert_eq!(u.model[10], 1.0);
        assert_eq!(u.model[15], 1.0);
        assert_eq!(u.model[1], 0.0);
    }
}
