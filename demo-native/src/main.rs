//! AtomArtist native shell.
//!
//! Wires up the OS window (winit + wgpu surface), event loop, and
//! disk-backed state persistence. Contains no application logic —
//! the widget tree lives in `atomartist-ui` and the 3D viewport renderer
//! in `atomartist-renderer`. Mirrors the platform-split policy used by
//! `agg-gui/demo-native`.
//!
//! Phase 0 stub. Real implementation begins in Phase 3 (canvas) /
//! Phase 5 (viewport) / Phase 6 (full layout).

fn main() {
    // Touch each crate so the workspace path-dep graph is exercised by
    // `cargo check`. These calls go away as the real wiring lands.
    atomartist_lib::placeholder();
    atomartist_renderer::placeholder();
    atomartist_ui::placeholder();

    println!("AtomArtist native shell — Phase 0 skeleton");
}
