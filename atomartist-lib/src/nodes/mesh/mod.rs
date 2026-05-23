//! Mesh-related nodes: import, repair, etc.

pub mod library_mesh_node;
pub mod mesh_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    library_mesh_node::register(reg);
    mesh_node::register(reg);
}
