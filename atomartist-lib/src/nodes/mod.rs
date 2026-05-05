//! Built-in node types organized by category.
//!
//! `register_all` populates a `NodeRegistry` with every shipped node. Apps
//! that want to add custom nodes should call this first, then register their
//! own types on top.

pub mod io;
pub mod math;
pub mod mesh;
pub mod ops_2d;
pub mod ops_3d;
pub mod output_node;
pub mod primitives_2d;
pub mod primitives_3d;

use crate::registry::NodeRegistry;

/// Register every built-in node type. Idempotent at the registry level only
/// so far as `register` panics on duplicate type ids — call this once at
/// startup.
pub fn register_all(reg: &mut NodeRegistry) {
    primitives_2d::register_all(reg);
    primitives_3d::register_all(reg);
    ops_2d::register_all(reg);
    ops_3d::register_all(reg);
    mesh::register_all(reg);
    math::register_all(reg);
    io::register_all(reg);
    output_node::register(reg);
}
