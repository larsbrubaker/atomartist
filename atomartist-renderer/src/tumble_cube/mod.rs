//! Tumble-cube navigation widget — port of MatterCAD's
//! `TumbleCubeControl`.
//!
//! Anchored to the top-right corner of `Viewport3dWidget`, this widget
//! shows a small 3-D cube labelled `Top` / `Bottom` / `Left` / `Right` /
//! `Front` / `Back` whose orientation mirrors the main camera. Hovering
//! highlights the face / edge / corner tile under the cursor; clicking
//! animates the camera to look at that face / edge / corner; dragging
//! rotates the camera as if the user grabbed the world cube.
//!
//! Module layout:
//!   - [`widget`] — `TumbleCubeWidget`, the `agg-gui` `Widget` shell.
//!   - [`cube_geometry`] — hand-built 24-vertex / 12-triangle cube + UVs.
//!   - [`face_textures`] — CPU-rasterized RGBA face labels and the
//!     per-tile hover-overlay paint.
//!   - [`hit_test`] — face / tile resolution (corner / edge / centre).
//!   - [`orient`] — `(face, tile)` → `(azimuth, elevation)` mapping +
//!     animation glue.
//!   - [`renderer`] — `WgpuCustomRender` impl with MSAA framebuffer and
//!     six per-face texture bind groups.

pub mod cube_geometry;
pub mod face_textures;
pub mod hit_test;
pub mod orient;
pub mod renderer;
pub mod widget;

pub use widget::{TumbleCubeInputs, TumbleCubeWidget};
