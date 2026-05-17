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

pub mod camera;
pub mod picking;
pub mod scene_renderer;
pub mod tumble_cube;
pub mod viewport_widget;

pub use camera::{CameraPoseAnimation, OrbitCamera, OrbitMode, OrientAnimation, Projection};
pub use picking::{project_to_view_plane, raycast_mesh};
pub use scene_renderer::{RenderStyle, WgpuSceneRenderer};
pub use tumble_cube::{TumbleCubeInputs, TumbleCubeWidget};
pub use viewport_widget::{Viewport3dWidget, ViewportInputs, ViewportTool};

/// Phase 0 placeholder kept so `demo-native` / `demo-wasm` stubs continue
/// to compile while real wiring lands. Removed in Phase 6.
pub fn placeholder() {}
