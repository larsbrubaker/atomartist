//! Math nodes — binary arithmetic ops.
//!
//! The Number constant source moved to the `input` category (see
//! `super::input`); this module now hosts only the binary operators.

pub mod binary_op_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    binary_op_node::register(reg);
}
