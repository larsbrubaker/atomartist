//! Operations on 2D paths.

pub mod inflate_node;
pub mod smooth_paths_node;
pub mod stroke_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    inflate_node::register(reg);
    smooth_paths_node::register(reg);
    stroke_node::register(reg);
}
