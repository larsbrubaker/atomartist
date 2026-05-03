//! AtomArtist core library.
//!
//! Contains the visual node-graph engine, typed sockets, node-type registry,
//! 2D / 3D geometry primitives, and serialization. Platform-agnostic — no
//! windowing, no rendering, no file dialogs. The `atomartist-renderer` crate
//! consumes geometry from here for the 3D viewport, and `atomartist-ui`
//! consumes the graph + registry to drive the canvas widget.
//!
//! Phase 0 stub. Real implementation begins in Phase 1.

/// Crate-level placeholder so `cargo check` has something to type-check.
/// Removed when Phase 1 adds the graph module.
pub fn placeholder() {}
