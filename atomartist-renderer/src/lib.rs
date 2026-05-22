//! AtomArtist 3D viewport renderer.
//!
//! Currently provides a software-projected wireframe viewport that uses the
//! existing `agg-gui` 2D `DrawCtx`. Triangles are projected by an
//! `OrbitCamera`'s view + projection matrices, backface-culled, and stroked
//! with normal-modulated colors. This works on every platform agg-gui runs
//! on without needing a custom wgpu render-pass injection.
//!
//! A future revision will replace the wireframe path with a wgpu fragment
//! shader once agg-gui exposes a generic custom-render hook (analogous to
//! the old `GlPaint` trait or the in-progress `WgpuPaint`). The widget
//! interface (`Viewport3dWidget`) and camera API (`OrbitCamera`) are
//! designed to be stable across that swap.
//!
//! # Anti-aliasing policy
//!
//! The 3-D scene is moving toward a **depth-peeled** transparency solution
//! (the same approach NodeDesigner / MatterCAD use). Depth peeling relies on
//! the stencil buffer and per-pass attachment swaps, so **full-screen
//! hardware MSAA is disabled** — every offscreen 3-D framebuffer in this
//! crate is built with `sample_count = 1`.
//!
//! When a specific 3-D object needs anti-aliasing, render it into an
//! oversized offscreen backbuffer (SSAA) and pick the matching composite
//! kernel from `demo_wgpu::MsaaFramebuffer`. Choosing the right downsample
//! kernel for the scale is critical — the wrong one silently throws SSAA
//! work away:
//!
//! - `2×` linear scaling (4× pixel cost) → `MsaaFramebuffer::blit_to`. A
//!   single bilinear tap reads an exact 2×2 box per output pixel.
//! - `4×` linear scaling (16× pixel cost) → `MsaaFramebuffer::blit_downsample_4x_to`.
//!   Runs 4 bilinear taps in a 2×2 quadrant grid for an exact 4×4 box
//!   average. `blit_to` at this scale only sees 4 of the 16 source texels
//!   per output pixel and degrades to roughly 2× quality — always pair the
//!   scale and the kernel.
//!
//! Reference implementation: the tumble-cube renderer
//! ([`tumble_cube::renderer`]) keeps `SAMPLE_COUNT = 1` and renders into a
//! `2×` SSAA offscreen framebuffer, compositing at widget size through the
//! shared bilinear blit pipeline.

pub mod bed;
pub mod camera;
pub mod camera_animations;
pub mod picking;
pub mod scene_renderer;
mod scene_shaders;
pub mod tumble_cube;
pub mod viewport_widget;

pub use camera::{OrbitCamera, OrbitMode, Projection};
pub use camera_animations::{CameraPoseAnimation, OrientAnimation, ProjectionAnimation};
pub use picking::{
    project_to_view_plane, raycast_mesh, resolve_pivot_or_fallback, HitPlane, PivotResolution,
};
pub use scene_renderer::{RenderStyle, WgpuSceneRenderer};
pub use tumble_cube::{TumbleCubeInputs, TumbleCubeWidget};
pub use viewport_widget::{Viewport3dWidget, ViewportInputs, ViewportTool};

/// Phase 0 placeholder kept so `demo-native` / `demo-wasm` stubs continue
/// to compile while real wiring lands. Removed in Phase 6.
pub fn placeholder() {}
