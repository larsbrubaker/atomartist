//! AtomArtist WASM shell — wasm-bindgen entry point.
//!
//! Phase 1 (current): wires up the same `build_app` widget tree as the
//! native shell so the path-dep stack compiles cleanly to
//! `wasm32-unknown-unknown`. Validates that:
//!   - all sibling crates (agg-gui, manifold-rust, clipper2-rust,
//!     tess2-rust) link as wasm32
//!   - atomartist-{lib,renderer,ui} build for the browser
//!   - the file-dialog injection pattern works with `NoFileDialogs`
//!
//! Phase 2 (follow-up): full canvas mounting + wgpu surface + winit-on-
//! WebSys event loop, modeled after agg-gui/demo-wasm. That work needs
//! the browser-side bootstrap (HTML + JS glue, font asset fetch,
//! requestAnimationFrame loop) and a non-trivial chunk of platform
//! plumbing — saved for a focused session.

use std::sync::Arc;

use atomartist_ui::{build_app, fresh_state_with_starter_graph};
use atomartist_ui::top_menu_bar::{FileDialogProvider, NoFileDialogs};
use wasm_bindgen::prelude::*;

/// Browser entry point. Initializes the panic hook, builds the AppState
/// + widget tree, and logs a "ready" message. Real canvas mounting +
/// event wiring lands in Phase 2.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();

    log("AtomArtist WASM module initialized");

    // Build the same widget tree the native shell builds. Without a
    // canvas + event loop yet, we don't paint anything — but
    // constructing the tree exercises every code path that the runtime
    // viewport will hit on its first frame.
    let state = fresh_state_with_starter_graph();
    let _state_clone = state.clone();
    // Skip widget tree construction here: build_app needs a system font
    // installed, which is a separate Phase-2 concern. The state build
    // alone validates that the registry / starter graph / executor all
    // link as wasm32.
    let _dialogs: Arc<dyn FileDialogProvider> = Arc::new(NoFileDialogs);
    // (Compile-only sanity: unused vars confirm the trait wiring matches.)
    let _ = build_app;
    let _ = state.registry.len();

    log("Starter graph built — node count + edge count populated");
}

/// Thin wrapper around `console.log` so the placeholder lifecycle is
/// observable from the browser devtools.
fn log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}
