//! Primitive 3D nodes: Box, Cylinder, Sphere.

pub mod box_node;
pub mod cylinder_node;
pub mod sphere_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    box_node::register(reg);
    cylinder_node::register(reg);
    sphere_node::register(reg);
}
