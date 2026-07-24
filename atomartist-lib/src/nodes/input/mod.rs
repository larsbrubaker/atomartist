//! Input nodes — constant value sources with no graph inputs.
//!
//! Each node emits a single typed output driven by an editable property:
//! Number, Boolean, String, and Color. They act as single sources of truth
//! feeding same-typed inputs across the graph. NumberConst previously lived
//! under `Math`; it moved here so all constant sources share one category.

pub mod bool_const_node;
pub mod color_const_node;
pub mod number_const_node;
pub mod string_const_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    number_const_node::register(reg);
    bool_const_node::register(reg);
    string_const_node::register(reg);
    color_const_node::register(reg);
}
