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
//! The main 3-D viewport ([`scene_renderer::WgpuSceneRenderer`]) uses
//! **dual depth peeling** for order-independent transparency, ported from
//! MatterCAD's `RenderTransparentLayers` chain (see
//! [`scene_renderer::depth_peel`]). Despite the name, depth peeling does
//! NOT use the stencil buffer — both reference implementations
//! (MatterCAD's dual peeling and NodeDesigner's single-direction
//! peeling) drive their per-pass discards by sampling the previous
//! iteration's depth attachment in the fragment shader. Stencil ops
//! never enter the chain.
//!
//! The reason **hardware MSAA is disabled** on every offscreen 3-D
//! framebuffer in this crate (`sample_count = 1`) is more subtle: the
//! peeling fragment shader has to ask "what is the opaque-pass depth at
//! this pixel?" via a `texture_depth_2d` sample. An MSAA depth texture
//! has a separate depth value per sample-slot, so that question doesn't
//! have a single answer — the discard test would be incoherent. Single-
//! sample depth keeps the lookup well-defined.
//!
//! Anti-aliasing on the main viewport comes from a **16-sample
//! progressive jitter accumulator** ([`scene_renderer::accumulation`]),
//! also ported from NodeDesigner (`accumulation-aa.js`). Each frame
//! that the scene is static, the renderer adds one more Halton(2,3)
//! sub-pixel-jittered sample to a `Rgba16Float` ping-pong accumulator;
//! after 16 samples the running average is a 16x supersampled image.
//! A [`scene_renderer::cache::SceneFingerprint`] gate ensures the chain
//! only runs when the scene actually changed — dragging a 2-D node
//! canvas next to the viewport doesn't invalidate the cache, but
//! rotating the camera or the tumble cube does.
//!
//! Other 3-D widgets that don't need OIT (the tumble cube, bed
//! composite chain) keep their existing single-shot pipelines and can
//! choose **SSAA** if they need anti-aliasing: render into an oversized
//! offscreen backbuffer and pick the matching composite kernel from
//! `demo_wgpu::MsaaFramebuffer`. Choosing the right downsample kernel
//! for the scale is critical — the wrong one silently throws SSAA
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
//! Reference SSAA implementation: the tumble-cube renderer
//! ([`tumble_cube::renderer`]) keeps `SAMPLE_COUNT = 1` and renders into a
//! `2×` SSAA offscreen framebuffer, compositing at widget size through the
//! shared bilinear blit pipeline.

pub mod bed;
pub mod camera;
pub mod camera_animations;
pub mod picking;
pub mod scene_renderer;
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
