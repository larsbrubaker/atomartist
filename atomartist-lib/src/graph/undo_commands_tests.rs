//! Unit tests for the graph undo/redo commands ([`super`]).
//!
//! Split out of `undo_commands.rs` to keep that file under the
//! project-wide 800-line cap. Included as a child module via
//! `#[path]` so `use super::*` still resolves to the command types.

use super::*;
use crate::graph::graph::Noodle;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, PropertyChangedCtx,
};
use crate::socket_types::SocketType;
use agg_gui::undo::UndoBuffer;

struct ConstNode;
impl NodeDef for ConstNode {
    fn type_id(&self) -> &'static str { "Const" }
    fn category(&self) -> &'static str { "Math" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Number)
            .build()
    }
    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.properties.number("value", 0.0);
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Number(v));
        Ok(o)
    }
}
struct TwoIn;
impl NodeDef for TwoIn {
    fn type_id(&self) -> &'static str { "TwoIn" }
    fn category(&self) -> &'static str { "Math" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("a", SocketType::Number)
            .output("out", SocketType::Number)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

fn setup() -> (Arc<Mutex<Graph>>, Arc<NodeRegistry>) {
    let mut r = NodeRegistry::new();
    r.register(ConstNode);
    r.register(TwoIn);
    (Arc::new(Mutex::new(Graph::new())), Arc::new(r))
}

#[test]
fn add_then_undo_leaves_graph_empty() {
    let (g, _reg) = setup();
    let id = g.lock().unwrap().allocate_id();
    let node = NodeInstance::new(id, "Const", [0.0, 0.0]);
    let mut cmd = AddNodeCmd::new(g.clone(), node);
    cmd.do_it();
    assert_eq!(g.lock().unwrap().node_count(), 1);
    cmd.undo_it();
    assert_eq!(g.lock().unwrap().node_count(), 0);
    cmd.do_it();
    assert_eq!(g.lock().unwrap().node_count(), 1, "redo restores");
}

/// The height drag's paired write: `height` + `matrix` land as ONE
/// command — mid-stroke samples coalesce into it, and a single
/// undo restores both pre-stroke values (MatterCAD's one "Scale"
/// undo entry).
#[test]
fn change_props_cmd_coalesces_and_undoes_both_values() {
    let (g, reg) = setup();
    let id = {
        let mut graph = g.lock().unwrap();
        let id = graph.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let _ = graph.set_property(id, "height", PortValue::Number(20.0));
        let _ = graph.set_property(id, "matrix", PortValue::Matrix4x4([1.0; 16]));
        id
    };
    let mut buf = UndoBuffer::new();
    let mk = |h: f64, m: f32| -> Vec<(Arc<str>, PortValue)> {
        vec![
            (Arc::from("matrix"), PortValue::Matrix4x4([m; 16])),
            (Arc::from("height"), PortValue::Number(h)),
        ]
    };
    buf.add_and_do(Box::new(ChangePropsCmd::new(g.clone(), id, mk(25.0, 2.0))));

    // Mid-stroke sample coalesces — still one undo entry.
    let coalesced = buf.try_coalesce_last(|top| {
        if let Some(cmd) = top.as_any_mut().downcast_mut::<ChangePropsCmd>() {
            if cmd.matches(id, &["matrix", "height"]) {
                cmd.extend_into(&[
                    PortValue::Matrix4x4([3.0; 16]),
                    PortValue::Number(30.0),
                ]);
                return true;
            }
        }
        false
    });
    assert!(coalesced, "same node + names must coalesce");

    let read = |name: &str| g.lock().unwrap().get(id).unwrap().properties.get(name).cloned();
    assert_eq!(read("height"), Some(PortValue::Number(30.0)));
    assert_eq!(read("matrix"), Some(PortValue::Matrix4x4([3.0; 16])));

    // ONE undo restores both pre-stroke values.
    assert!(buf.can_undo());
    buf.undo();
    assert_eq!(read("height"), Some(PortValue::Number(20.0)), "undo restores height");
    assert_eq!(read("matrix"), Some(PortValue::Matrix4x4([1.0; 16])), "undo restores matrix");
    assert!(!buf.can_undo(), "the whole stroke was a single undo entry");

    // Redo replays the final coalesced pair.
    assert!(buf.can_redo());
    buf.redo();
    assert_eq!(read("height"), Some(PortValue::Number(30.0)));
    assert_eq!(read("matrix"), Some(PortValue::Matrix4x4([3.0; 16])));
}

#[test]
fn undo_buffer_full_round_trip() {
    let (g, reg) = setup();
    let mut buf = UndoBuffer::new();
    let (a, b) = {
        let mut graph = g.lock().unwrap();
        let a = graph.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = graph.add_new_node("TwoIn", [100.0, 0.0], &reg).unwrap();
        (a, b)
    };

    let (out_a, in_a_b) = {
        let graph = g.lock().unwrap();
        let out_a = graph.get(a).unwrap().output_by_name("out").unwrap().uid;
        let in_a_b = graph.get(b).unwrap().input_by_name("a").unwrap().uid;
        (out_a, in_a_b)
    };

    buf.add_and_do(Box::new(ConnectCmd::new(
        g.clone(),
        reg.clone(),
        Noodle::new(a, out_a, b, in_a_b),
    )));

    assert_eq!(g.lock().unwrap().node_count(), 2);
    assert_eq!(g.lock().unwrap().noodle_count(), 1);

    buf.undo();
    assert_eq!(g.lock().unwrap().noodle_count(), 0);
    buf.redo();
    assert_eq!(g.lock().unwrap().noodle_count(), 1);
}

#[test]
fn change_property_undo_redo() {
    let (g, _reg) = setup();
    let id = g.lock().unwrap().allocate_id();
    let mut node = NodeInstance::new(id, "Const", [0.0, 0.0]);
    node.properties.insert(Arc::from("value"), PortValue::Number(2.0));
    g.lock().unwrap().add_node(node).unwrap();

    let mut cmd = ChangePropertyCmd::new(g.clone(), id, "value", PortValue::Number(7.0));
    cmd.do_it();
    let cur = g.lock().unwrap().get(id).unwrap().properties.get("value").cloned().unwrap();
    assert_eq!(cur, PortValue::Number(7.0));
    cmd.undo_it();
    let cur = g.lock().unwrap().get(id).unwrap().properties.get("value").cloned().unwrap();
    assert_eq!(cur, PortValue::Number(2.0));
}

// --- on_property_changed hook via the undo-command path -------------

/// A typed input node whose single output socket adopts the type
/// named by its `port_type` property. Mirrors the eventual typed
/// GraphInput node: changing "port type" retypes the output.
struct TypedInput;
impl NodeDef for TypedInput {
    fn type_id(&self) -> &'static str { "TypedInput" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Number)
            .property("port_type", PortValue::StringVal(Arc::new("Number".into())))
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
    fn on_property_changed(&self, ctx: &mut PropertyChangedCtx) {
        if ctx.property != "port_type" {
            return;
        }
        let ty = match ctx.property_value("port_type") {
            Some(PortValue::StringVal(s)) => match s.as_str() {
                "Geometry3d" => SocketType::Geometry3d,
                _ => SocketType::Number,
            },
            _ => SocketType::Number,
        };
        let out_uid = ctx
            .graph
            .get(ctx.this_node)
            .and_then(|n| n.outputs.first().map(|s| s.uid));
        if let Some(uid) = out_uid {
            let _ = ctx.graph.retype_socket(ctx.this_node, uid, ty);
        }
    }
}

/// Consumes a `Number` on its single input.
struct NumberSink;
impl NodeDef for NumberSink {
    fn type_id(&self) -> &'static str { "NumberSink" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("n", SocketType::Number)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

/// A wildcard sink whose input is typed `None` — accepts any source
/// and stays compatible across retypes (like Output's trailing slot).
struct WildcardSink;
impl NodeDef for WildcardSink {
    fn type_id(&self) -> &'static str { "WildcardSink" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("any", SocketType::None)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

fn typed_setup() -> (Arc<Mutex<Graph>>, Arc<NodeRegistry>) {
    let mut r = NodeRegistry::new();
    r.register(TypedInput);
    r.register(NumberSink);
    r.register(WildcardSink);
    (Arc::new(Mutex::new(Graph::new())), Arc::new(r))
}

/// Changing `port_type` retypes the output (hook fires on do), which
/// disconnects the now-incompatible Number noodle while the wildcard
/// noodle survives. Undo fires the hook again (retype back) and
/// restores the disconnected noodle.
#[test]
fn property_hook_retypes_and_revalidates_via_command() {
    let (g, reg) = typed_setup();
    let (src, num_sink, wild_sink, out_uid, num_in, wild_in) = {
        let mut graph = g.lock().unwrap();
        let src = graph.add_new_node("TypedInput", [0.0, 0.0], &reg).unwrap();
        let num_sink = graph.add_new_node("NumberSink", [100.0, 0.0], &reg).unwrap();
        let wild_sink = graph.add_new_node("WildcardSink", [100.0, 50.0], &reg).unwrap();
        let out_uid = graph.get(src).unwrap().output_by_name("out").unwrap().uid;
        let num_in = graph.get(num_sink).unwrap().input_by_name("n").unwrap().uid;
        let wild_in = graph.get(wild_sink).unwrap().input_by_name("any").unwrap().uid;
        (src, num_sink, wild_sink, out_uid, num_in, wild_in)
    };

    // out(Number) → NumberSink.n(Number) and → WildcardSink.any(None).
    let num_noodle = Noodle::new(src, out_uid, num_sink, num_in);
    let wild_noodle = Noodle::new(src, out_uid, wild_sink, wild_in);
    {
        let mut graph = g.lock().unwrap();
        graph.connect(num_noodle, &reg).unwrap();
        graph.connect(wild_noodle, &reg).unwrap();
        assert_eq!(graph.noodle_count(), 2);
    }

    let out_type = |graph: &Graph| {
        graph.get(src).unwrap().output_by_uid(out_uid).unwrap().socket_type
    };
    assert_eq!(out_type(&g.lock().unwrap()), SocketType::Number);

    // Flip port_type → Geometry3d through the undoable command path.
    let mut cmd = ChangePropertyCmd::new(
        g.clone(),
        src,
        "port_type",
        PortValue::StringVal(Arc::new("Geometry3d".into())),
    )
    .with_registry(reg.clone());
    cmd.do_it();

    {
        let graph = g.lock().unwrap();
        // (a) hook fired on do → output retyped.
        assert_eq!(out_type(&graph), SocketType::Geometry3d, "hook retyped output on do");
        // (c) incompatible Number noodle dropped; (d) wildcard survives.
        assert!(
            !graph.noodles().contains(&num_noodle),
            "incompatible noodle should be disconnected",
        );
        assert!(
            graph.noodles().contains(&wild_noodle),
            "wildcard noodle should survive the retype",
        );
        assert_eq!(graph.noodle_count(), 1);
    }

    // Undo: (b) hook fires again (retype back) + disconnected noodle restored.
    cmd.undo_it();
    {
        let graph = g.lock().unwrap();
        assert_eq!(out_type(&graph), SocketType::Number, "hook retyped output back on undo");
        assert!(
            graph.noodles().contains(&num_noodle),
            "undo restores the hook-disconnected noodle",
        );
        assert!(graph.noodles().contains(&wild_noodle));
        assert_eq!(graph.noodle_count(), 2);
    }

    // Redo re-applies the retype + re-disconnects.
    cmd.do_it();
    {
        let graph = g.lock().unwrap();
        assert_eq!(out_type(&graph), SocketType::Geometry3d);
        assert_eq!(graph.noodle_count(), 1);
    }
}

/// A node with two independently-typed outputs. Changing `type_a`
/// retypes `out_a`; changing `type_b` retypes `out_b`. Used to prove a
/// [`ChangePropsCmd`] batch that retypes BOTH sockets — each dropping a
/// different noodle — restores every dropped noodle on undo.
struct DualTypedInput;
impl NodeDef for DualTypedInput {
    fn type_id(&self) -> &'static str { "DualTypedInput" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out_a", SocketType::Number)
            .output("out_b", SocketType::Number)
            .property("type_a", PortValue::StringVal(Arc::new("Number".into())))
            .property("type_b", PortValue::StringVal(Arc::new("Number".into())))
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
    fn on_property_changed(&self, ctx: &mut PropertyChangedCtx) {
        // Map property → the output socket it drives.
        let out_name = match ctx.property {
            "type_a" => "out_a",
            "type_b" => "out_b",
            _ => return,
        };
        let ty = match ctx.property_value(ctx.property) {
            Some(PortValue::StringVal(s)) if s.as_str() == "Geometry3d" => SocketType::Geometry3d,
            _ => SocketType::Number,
        };
        let out_uid = ctx
            .graph
            .get(ctx.this_node)
            .and_then(|n| n.output_by_name(out_name).map(|s| s.uid));
        if let Some(uid) = out_uid {
            let _ = ctx.graph.retype_socket(ctx.this_node, uid, ty);
        }
    }
}

/// A `ChangePropsCmd` batch that retypes two different sockets — each
/// dropping a different Number noodle — must restore BOTH noodles on
/// undo. The old first-non-empty-wins capture only restored the first
/// slot's drop; accumulating across the batch fixes it.
#[test]
fn change_props_cmd_restores_all_batch_disconnections_on_undo() {
    let mut r = NodeRegistry::new();
    r.register(DualTypedInput);
    r.register(NumberSink);
    let reg = Arc::new(r);
    let g = Arc::new(Mutex::new(Graph::new()));

    let (src, sink_a, sink_b, out_a_uid, out_b_uid, in_a, in_b) = {
        let mut graph = g.lock().unwrap();
        let src = graph.add_new_node("DualTypedInput", [0.0, 0.0], &reg).unwrap();
        let sink_a = graph.add_new_node("NumberSink", [100.0, 0.0], &reg).unwrap();
        let sink_b = graph.add_new_node("NumberSink", [100.0, 50.0], &reg).unwrap();
        let out_a_uid = graph.get(src).unwrap().output_by_name("out_a").unwrap().uid;
        let out_b_uid = graph.get(src).unwrap().output_by_name("out_b").unwrap().uid;
        let in_a = graph.get(sink_a).unwrap().input_by_name("n").unwrap().uid;
        let in_b = graph.get(sink_b).unwrap().input_by_name("n").unwrap().uid;
        (src, sink_a, sink_b, out_a_uid, out_b_uid, in_a, in_b)
    };

    let noodle_a = Noodle::new(src, out_a_uid, sink_a, in_a);
    let noodle_b = Noodle::new(src, out_b_uid, sink_b, in_b);
    {
        let mut graph = g.lock().unwrap();
        graph.connect(noodle_a, &reg).unwrap();
        graph.connect(noodle_b, &reg).unwrap();
        assert_eq!(graph.noodle_count(), 2);
    }

    // One batch retypes BOTH outputs to Geometry3d — both Number noodles
    // become incompatible and are dropped by the hook.
    let mut cmd = ChangePropsCmd::new(
        g.clone(),
        src,
        vec![
            (Arc::from("type_a"), PortValue::StringVal(Arc::new("Geometry3d".into()))),
            (Arc::from("type_b"), PortValue::StringVal(Arc::new("Geometry3d".into()))),
        ],
    )
    .with_registry(reg.clone());
    cmd.do_it();

    {
        let graph = g.lock().unwrap();
        assert!(!graph.noodles().contains(&noodle_a), "noodle_a dropped on retype");
        assert!(!graph.noodles().contains(&noodle_b), "noodle_b dropped on retype");
        assert_eq!(graph.noodle_count(), 0);
    }

    // Undo must restore BOTH dropped noodles, not just the first slot's.
    cmd.undo_it();
    {
        let graph = g.lock().unwrap();
        assert!(graph.noodles().contains(&noodle_a), "undo restores noodle_a");
        assert!(graph.noodles().contains(&noodle_b), "undo restores noodle_b");
        assert_eq!(graph.noodle_count(), 2);
    }
}
