//! AtomArtist 3D viewport renderer.
//!
//! Builds on wgpu for cross-platform 3D rendering (Windows/macOS/Linux native
//! via Vulkan/DX12/Metal, browsers via WebGPU/WebGL2). Renders the graph's
//! output mesh into an offscreen `wgpu::Texture` that the agg-gui compositor
//! samples as a 2D image, isolating 3D state from the 2D UI render pass.
//!
//! Phase 0 stub. Real implementation begins in Phase 5.

/// Crate-level placeholder so `cargo check` has something to type-check.
/// Removed when Phase 5 adds the camera + scene_renderer modules.
pub fn placeholder() {}
