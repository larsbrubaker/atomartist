//! Binary math operation on two Numbers — Add, Subtract, Multiply, Divide.
//! One node type per operation so the user picks from the menu rather
//! than wading through a property enum.

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, SocketDef,
};
use crate::socket_types::SocketType;

fn pull_number(v: &PortValue, default: f64) -> f64 {
    match v { PortValue::Number(n) => *n, _ => default }
}

fn make_node(
    type_id: &'static str,
    display_name: &'static str,
    op: fn(f64, f64) -> f64,
) -> impl NodeDef {
    BinaryOpNode { type_id, display_name, op }
}

struct BinaryOpNode {
    type_id: &'static str,
    display_name: &'static str,
    op: fn(f64, f64) -> f64,
}

impl NodeDef for BinaryOpNode {
    fn type_id(&self) -> &'static str { self.type_id }
    fn display_name(&self) -> &'static str { self.display_name }
    fn category(&self) -> &'static str { "Math" }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![
            SocketDef::required("a", SocketType::Number),
            SocketDef::required("b", SocketType::Number),
        ]
    }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Number)]
    }

    fn evaluate(&self, inputs: &NodeInputs, _props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let a = pull_number(inputs.get("a"), 0.0);
        let b = pull_number(inputs.get("b"), 0.0);
        let r = (self.op)(a, b);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Number(r));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(make_node("Add",      "Add",      |a, b| a + b));
    reg.register(make_node("Subtract", "Subtract", |a, b| a - b));
    reg.register(make_node("Multiply", "Multiply", |a, b| a * b));
    reg.register(make_node("Divide",   "Divide",   |a, b| if b.abs() < 1e-12 { 0.0 } else { a / b }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(node: &impl NodeDef, a: f64, b: f64) -> f64 {
        let mut inputs = NodeInputs::default();
        inputs.insert("a", PortValue::Number(a));
        inputs.insert("b", PortValue::Number(b));
        let outs = node.evaluate(&inputs, &NodeProperties::default()).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Number(n) => *n,
            _ => panic!(),
        }
    }

    #[test]
    fn add_two_numbers() {
        let n = make_node("Add", "Add", |a, b| a + b);
        assert!((run(&n, 2.0, 3.0) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn divide_by_zero_returns_zero() {
        let n = make_node("Divide", "Divide", |a, b| if b.abs() < 1e-12 { 0.0 } else { a / b });
        assert_eq!(run(&n, 5.0, 0.0), 0.0);
    }
}
