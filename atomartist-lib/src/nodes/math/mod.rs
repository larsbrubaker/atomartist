//! Math nodes — Number constant, binary arithmetic ops.

pub mod binary_op_node;
pub mod number_const_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    number_const_node::register(reg);
    binary_op_node::register(reg);
}
