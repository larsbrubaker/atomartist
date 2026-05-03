//! AtomArtist WASM shell.
//!
//! `wasm-bindgen` entry point that mounts the shared widget tree from
//! `atomartist-ui` onto a browser HTMLCanvasElement, with the wgpu device
//! using WebGPU (with WebGL2 fallback). Mirrors the platform-split policy
//! used by `agg-gui/demo-wasm` — no application logic lives here.
//!
//! Phase 0 stub. Real implementation begins in Phase 10.

use wasm_bindgen::prelude::*;

/// Entry point invoked from `index.html`. The browser calls this once
/// the WASM module finishes initializing.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();

    // Touch each crate so the workspace path-dep graph is exercised by
    // `wasm-pack build`. These calls go away as the real wiring lands.
    atomartist_lib::placeholder();
    atomartist_renderer::placeholder();
    atomartist_ui::placeholder();
}
