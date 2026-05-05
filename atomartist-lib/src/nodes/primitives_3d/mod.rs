//! Primitive 3D nodes: Box, Cylinder, Sphere, Cone, Torus, Pyramid, Wedge.

pub mod box_node;
pub mod cone_node;
pub mod cylinder_node;
pub mod pyramid_node;
pub mod sphere_node;
pub mod torus_node;
pub mod wedge_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    box_node::register(reg);
    cylinder_node::register(reg);
    sphere_node::register(reg);
    cone_node::register(reg);
    torus_node::register(reg);
    pyramid_node::register(reg);
    wedge_node::register(reg);
}
