//! Binary math operation on two Numbers — Add, Subtract, Multiply, Divide.
//! One node type per operation so the user picks from the menu rather
//! than wading through a property enum.

use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties,
    NodeRegistry,
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

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("a", SocketType::Number)
            .input("b", SocketType::Number)
            .output("out", SocketType::Number)
            .build()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let a = pull_number(ctx.input_named("a"), 0.0);
        let b = pull_number(ctx.input_named("b"), 0.0);
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
        // Build a NodeInstance reflecting what `instantiate` would produce,
        // then build an EvalCtx with values keyed by socket uid.
        let mut alloc = SocketUidAlloc::new();
        let tpl = node.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), node.type_id().to_string(), [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let uid_a = inst.input_by_name("a").unwrap().uid;
        let uid_b = inst.input_by_name("b").unwrap().uid;
        let mut inputs = NodeInputs::default();
        inputs.insert(uid_a, PortValue::Number(a));
        inputs.insert(uid_b, PortValue::Number(b));
        let props = NodeProperties::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = node.evaluate(&ctx).unwrap();
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
