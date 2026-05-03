//! Operations on 3D geometry: Transform, Combine.
//! Boolean and mesh-repair operations land in this module in later phases.

pub mod boolean_node;
pub mod combine_node;
pub mod extrude_node;
pub mod transform_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    transform_node::register(reg);
    combine_node::register(reg);
    extrude_node::register(reg);
    boolean_node::register(reg);
}
