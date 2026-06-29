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
//! Anti-aliasing on the main viewport comes from **spatial 3×3 (9×)
//! supersampling**: every offscreen scene target is single-sample and
//! allocated at [`scene_renderer::SSAA_SCALE`]× the on-screen pixel
//! size; the whole scene renders once into that oversized
//! `Rgba16Float` buffer, and the final composite uses
//! `SsaaFramebuffer::blit_downsample_3x_to` (a 9-tap box filter) to
//! resolve it down to the widget rect — one pass, fully anti-aliased.
//! (The targets stay single-sample because depth peeling samples
//! per-pixel scene depth in-shader, which a per-sample depth
//! attachment would make ambiguous.)
//!
//! Other 3-D widgets that don't need OIT (the tumble cube, bed
//! composite chain) follow the same SSAA pattern: render into an
//! oversized offscreen backbuffer and pick the matching composite
//! kernel from `demo_wgpu::SsaaFramebuffer`. Pairing the scale with
//! the right kernel matters — the wrong one silently throws SSAA work
//! away:
//!
//! - `2×` linear scaling (4× pixel cost) → `SsaaFramebuffer::blit_to`. A
//!   single bilinear tap reads an exact 2×2 box per output pixel.
//! - `3×` linear scaling (9× pixel cost) → `SsaaFramebuffer::blit_downsample_3x_to`.
//!   A 9-tap 3×3 box; a single bilinear tap at this scale reads only the
//!   corner 2×2 block (4 of 9 texels) and degrades to roughly 2× quality.
//!
//! Reference SSAA implementation: the tumble-cube renderer
//! ([`tumble_cube::renderer`]) renders into a `3×` SSAA offscreen
//! framebuffer and composites at widget size through the 3×3 box blit.

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
