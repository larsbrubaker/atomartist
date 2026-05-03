//! Operations on 2D paths.

pub mod inflate_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    inflate_node::register(reg);
}
