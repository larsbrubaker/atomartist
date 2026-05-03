//! Primitive 2D nodes: Rectangle, Circle, Ring, Star.

pub mod circle_node;
pub mod rectangle_node;
pub mod ring_node;
pub mod star_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    rectangle_node::register(reg);
    circle_node::register(reg);
    ring_node::register(reg);
    star_node::register(reg);
}
