//! The "Part(s) to Subtract" **rows** — the per-instance property schema
//! behind the selection semantics `boolean_nary_tests` already covers
//! (plan step B-3b of `docs/boolean-node-plan.md`).
//!
//! MatterCAD shows a titled group with one toggle per child
//! (`BooleanObject3D.cs:153-161`); ours is a read-only title row plus one
//! checkbox per operand *socket*, minted by
//! [`boolean_selection::rows`] through `NodeDef::instance_properties` and
//! committed back through `NodeDef::translate_property_commit`. What is
//! pinned here is the contract between those two halves: what the rows
//! say, and what a click on one stores.
//!
//! The canvas end of the same feature — a real click landing on a real
//! checkbox row — lives in `atomartist-ui-test/tests/boolean_part_rows.rs`.

use std::sync::Arc;

use super::boolean_selection::{self, HEADER_ROW, HEADER_TEXT, ROW_PREFIX, SUBTRACT_PARTS};
use super::BooleanNode;
use crate::graph::graph::{Graph, Noodle};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::graph::socket::SocketUid;
use crate::nodes;
use crate::registry::{CommitTranslation, EditorKind, NodeDef, NodeRegistry, PropDef};

fn registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    nodes::register_all(&mut r);
    r
}

/// A Boolean node with `count` Box nodes wired into it, plus the operand
/// socket uids in display order.
fn boolean_with_operands(count: usize) -> (Graph, NodeRegistry, NodeId, Vec<SocketUid>) {
    let reg = registry();
    let mut g = Graph::new();
    let b = g.add_new_node("Boolean", [200.0, 0.0], &reg).unwrap();
    let mut uids = Vec::new();
    for i in 0..count {
        let bx = g
            .add_new_node("Box", [0.0, 100.0 * i as f64], &reg)
            .unwrap();
        let out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let slot = g.get(b).unwrap().inputs.last().unwrap().uid;
        g.connect(Noodle::new(bx, out, b, slot), &reg).unwrap();
        uids.push(slot);
    }
    (g, reg, b, uids)
}

fn node(g: &Graph, id: NodeId) -> &NodeInstance {
    g.get(id).expect("the Boolean node is in the graph")
}

fn set_operation(g: &mut Graph, id: NodeId, operation: &str) {
    g.set_property(
        id,
        "operation",
        PortValue::StringVal(Arc::new(operation.to_string())),
    )
    .unwrap();
}

fn set_selection(g: &mut Graph, id: NodeId, value: &str) {
    g.set_property(
        id,
        SUBTRACT_PARTS,
        PortValue::StringVal(Arc::new(value.to_string())),
    )
    .unwrap();
}

/// The instance rows the canvas would actually mount: the per-instance
/// schema, filtered through the same visibility hook the projection uses.
fn visible_rows(g: &Graph, id: NodeId) -> Vec<PropDef> {
    let n = node(g, id);
    let props = boolean_selection::properties_of(n);
    BooleanNode
        .resolved_properties(n)
        .into_iter()
        .filter(|p| boolean_selection::is_row(&p.name))
        .filter(|p| BooleanNode.row_visible(&p.name, &props))
        .collect()
}

/// The checked state of each checkbox row, in row order.
fn checkboxes(g: &Graph, id: NodeId) -> Vec<(String, bool)> {
    visible_rows(g, id)
        .into_iter()
        .filter(|p| p.name.starts_with(ROW_PREFIX))
        .map(|p| {
            (
                p.label.as_ref().map(|l| l.to_string()).unwrap_or_default(),
                matches!(p.default, PortValue::Bool(true)),
            )
        })
        .collect()
}

/// Click a checkbox row the way the canvas does: it sends the *flipped*
/// value under the row's own name, and the def translates that into the
/// write the graph stores.
fn click_row(g: &mut Graph, id: NodeId, uid: SocketUid) {
    let name = format!("{ROW_PREFIX}{}", uid.0);
    let now = visible_rows(g, id)
        .into_iter()
        .find(|p| p.name.as_ref() == name)
        .map(|p| matches!(p.default, PortValue::Bool(true)))
        .unwrap_or_else(|| panic!("no row named {name}"));
    match BooleanNode.translate_property_commit(node(g, id), &name, &PortValue::Bool(!now)) {
        CommitTranslation::Store(real_name, real_value) => {
            g.set_property(id, real_name, real_value).unwrap();
        }
        other => panic!("a live checkbox row must translate into a write, got {other:?}"),
    }
}

fn stored(g: &Graph, id: NodeId) -> String {
    boolean_selection::stored(&boolean_selection::properties_of(node(g, id)))
}

// ------------------------------------------------------------- the rows

/// One checkbox per connected operand, labelled the way the socket is —
/// the name it adopted from its source, so a row and its noodle read as
/// the same thing. (Identical labels also pick up a position suffix; see
/// `operands_that_would_share_a_label_are_numbered`.)
#[test]
fn every_connected_operand_gets_a_row_labelled_like_its_socket() {
    let (mut g, _reg, b, uids) = boolean_with_operands(3);
    set_operation(&mut g, b, "Subtract");

    let rows = checkboxes(&g, b);
    assert_eq!(rows.len(), uids.len(), "one row per operand: {:?}", rows);
    for (i, (label, _)) in rows.iter().enumerate() {
        let socket = &node(&g, b).inputs[i];
        let shown = socket.display_label.as_ref().unwrap().as_ref();
        assert!(
            label.starts_with(shown),
            "row {i} reads {label:?}, not the socket's display name {shown:?}"
        );
    }
    // The trailing empty placeholder is not an operand and gets no row.
    assert_eq!(node(&g, b).inputs.len(), 4);
}

/// Two operands from the same kind of source adopt the *same* display
/// label ("Box - out"), and two identical checkboxes name nothing. The
/// repeats carry their 1-based operand position; a node whose parts
/// already read distinctly is left alone.
#[test]
fn operands_that_would_share_a_label_are_numbered() {
    let (mut g, _reg, b, _uids) = boolean_with_operands(2);
    set_operation(&mut g, b, "Subtract");

    let labels: Vec<String> = checkboxes(&g, b).into_iter().map(|(l, _)| l).collect();
    assert_eq!(
        labels,
        vec!["Box - out (1)".to_string(), "Box - out (2)".to_string()],
        "two Box operands must not draw two rows reading the same thing"
    );

    // A distinct source keeps its label bare.
    let cyl = g.add_new_node("Cylinder", [0.0, 400.0], &_reg).unwrap();
    let out = g.get(cyl).unwrap().output_by_name("out").unwrap().uid;
    let slot = g.get(b).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(cyl, out, b, slot), &_reg).unwrap();
    let labels: Vec<String> = checkboxes(&g, b).into_iter().map(|(l, _)| l).collect();
    assert_eq!(
        labels[2], "Cylinder - out",
        "an unambiguous label stays bare"
    );
}

/// The group is titled, as MatterCAD titles it. The header is a
/// read-only row with no label, which is how agg-gui's row renderer
/// paints a plain line of text across the node body.
#[test]
fn the_group_carries_matter_cads_title() {
    let (mut g, _reg, b, _uids) = boolean_with_operands(2);
    set_operation(&mut g, b, "Subtract");

    let header = visible_rows(&g, b)
        .into_iter()
        .find(|p| p.name.as_ref() == HEADER_ROW)
        .expect("the selection group has a title row");
    assert!(matches!(header.editor, EditorKind::StringReadOnly));
    assert_eq!(header.label.as_deref(), Some(""));
    match &header.default {
        PortValue::StringVal(s) => assert_eq!(s.as_str(), HEADER_TEXT),
        other => panic!("the title row carries {:?}", other),
    }
}

/// Rows only for the operations that cut (`UpdateControls`,
/// `BooleanObject3D.cs:397-408`) — the same gate the hidden raw row has
/// always had.
#[test]
fn the_rows_appear_only_for_the_operations_that_cut() {
    let (mut g, _reg, b, _uids) = boolean_with_operands(2);
    for operation in ["Subtract", "Subtract & Replace"] {
        set_operation(&mut g, b, operation);
        assert_eq!(
            checkboxes(&g, b).len(),
            2,
            "{operation} must offer the selection rows"
        );
    }
    for operation in ["Combine", "Intersect"] {
        set_operation(&mut g, b, operation);
        assert!(
            visible_rows(&g, b).is_empty(),
            "{operation} has nothing to subtract, so it offers no rows"
        );
    }
}

/// A lone operand is always a keep, so there is no honest checkbox to
/// draw: one that could never be honoured would be a lie.
#[test]
fn a_lone_operand_has_no_rows_at_all() {
    let (mut g, _reg, b, _uids) = boolean_with_operands(1);
    set_operation(&mut g, b, "Subtract");
    assert!(visible_rows(&g, b).is_empty());
    assert!(
        BooleanNode.instance_properties(node(&g, b)).is_none(),
        "with nothing to show, the def falls back to its static schema"
    );
}

/// Nobody has chosen yet: the row for the default remover — the last
/// connected input — shows checked, matching what the node actually does.
#[test]
fn the_unset_selection_shows_the_last_operand_checked() {
    let (mut g, _reg, b, _uids) = boolean_with_operands(3);
    set_operation(&mut g, b, "Subtract");
    assert_eq!(stored(&g, b), boolean_selection::AUTO);
    let checked: Vec<bool> = checkboxes(&g, b).into_iter().map(|(_, c)| c).collect();
    assert_eq!(checked, vec![false, false, true]);
}

/// An explicit selection shows exactly those rows checked.
#[test]
fn an_explicit_selection_is_what_the_rows_show() {
    let (mut g, _reg, b, uids) = boolean_with_operands(3);
    set_operation(&mut g, b, "Subtract");
    set_selection(&mut g, b, &boolean_selection::encode(&[uids[0], uids[1]]));
    let checked: Vec<bool> = checkboxes(&g, b).into_iter().map(|(_, c)| c).collect();
    assert_eq!(checked, vec![true, true, false]);
}

// ---------------------------------------------------------- the commits

/// The first click on an unset node **materializes** what the rows were
/// showing, then applies the flip — so nothing the user did not touch
/// changes underneath them. From then on the selection is explicit, as in
/// MatterCAD once a part is picked.
#[test]
fn the_first_toggle_materializes_the_effective_selection() {
    let (mut g, _reg, b, uids) = boolean_with_operands(3);
    set_operation(&mut g, b, "Subtract");

    click_row(&mut g, b, uids[0]);

    assert_eq!(
        stored(&g, b),
        boolean_selection::encode(&[uids[0], uids[2]]),
        "checking the first row must keep the auto-chosen last one checked"
    );
    let checked: Vec<bool> = checkboxes(&g, b).into_iter().map(|(_, c)| c).collect();
    assert_eq!(checked, vec![true, false, true]);
}

/// …including when the flip is the auto-chosen row itself: unchecking it
/// leaves an explicitly empty selection, not a fall back to the default.
#[test]
fn unchecking_the_auto_default_stores_the_explicit_empty_choice() {
    let (mut g, _reg, b, uids) = boolean_with_operands(2);
    set_operation(&mut g, b, "Subtract");

    click_row(&mut g, b, uids[1]);

    assert_eq!(stored(&g, b), "", "an empty choice is stored, not `auto`");
    let checked: Vec<bool> = checkboxes(&g, b).into_iter().map(|(_, c)| c).collect();
    assert_eq!(checked, vec![false, false], "every part is a keep");
}

/// Checking every row is a state the user can reach — and one the node
/// refuses at evaluate with a named error (`boolean_nary_tests`'s
/// `selecting_every_part_as_a_remover_is_a_named_error`). Configurable
/// and honest beats un-clickable.
#[test]
fn every_row_can_be_checked_even_though_evaluating_that_is_an_error() {
    let (mut g, _reg, b, uids) = boolean_with_operands(2);
    set_operation(&mut g, b, "Subtract");

    click_row(&mut g, b, uids[0]);

    assert_eq!(
        stored(&g, b),
        boolean_selection::encode(&[uids[0], uids[1]])
    );
    let checked: Vec<bool> = checkboxes(&g, b).into_iter().map(|(_, c)| c).collect();
    assert_eq!(checked, vec![true, true]);
}

/// The stored list reads in row order however the user clicked, so a
/// saved file's selection matches what the panel shows.
#[test]
fn the_stored_list_follows_row_order() {
    let (mut g, _reg, b, uids) = boolean_with_operands(3);
    set_operation(&mut g, b, "Subtract");
    set_selection(&mut g, b, "");

    click_row(&mut g, b, uids[2]);
    click_row(&mut g, b, uids[0]);

    assert_eq!(
        stored(&g, b),
        boolean_selection::encode(&[uids[0], uids[2]])
    );
}

/// A synthetic row name is never stored on the instance: it is folded
/// into `subtract_parts` before the write. Anything else would end up in
/// the project file and outlive the socket it names.
#[test]
fn a_row_commit_never_writes_a_synthetic_property() {
    let (mut g, _reg, b, uids) = boolean_with_operands(2);
    set_operation(&mut g, b, "Subtract");
    click_row(&mut g, b, uids[0]);

    let synthetic: Vec<&str> = node(&g, b)
        .properties
        .keys()
        .map(|k| k.as_ref())
        .filter(|k| boolean_selection::is_row(k))
        .collect();
    assert!(synthetic.is_empty(), "leaked rows: {:?}", synthetic);
}

/// Names this node does not own pass through untouched; a checkbox for
/// a part that is no longer wired is **refused**.
///
/// A stale row can be clicked for real: the canvas paints from one
/// projection and dispatches against the next, so a disconnect between
/// the two leaves a checkbox on screen whose socket is gone. Falling
/// through would store `subtract_part:<uid>` on the node — a phantom
/// checkbox state that survives until the file is reloaded.
#[test]
fn a_stale_checkbox_row_is_refused_rather_than_stored() {
    let (mut g, _reg, b, _uids) = boolean_with_operands(2);
    set_operation(&mut g, b, "Subtract");
    let n = node(&g, b);
    assert_eq!(
        BooleanNode.translate_property_commit(n, HEADER_ROW, &PortValue::Bool(true)),
        CommitTranslation::Passthrough,
        "the header is not a checkbox row — the canvas never commits it anyway"
    );
    assert_eq!(
        BooleanNode.translate_property_commit(n, "operation", &PortValue::Bool(true)),
        CommitTranslation::Passthrough,
        "an ordinary property is stored as it comes"
    );
    assert_eq!(
        BooleanNode.translate_property_commit(
            n,
            &format!("{ROW_PREFIX}9999"),
            &PortValue::Bool(true)
        ),
        CommitTranslation::Reject,
        "a checkbox naming a socket this node no longer has must be dropped"
    );
}

/// …and the UI-side end of that: the refused write leaves the property
/// map exactly as it was — no synthetic key, no change to the selection.
#[test]
fn a_refused_commit_writes_nothing_at_all() {
    let (mut g, _reg, b, _uids) = boolean_with_operands(2);
    set_operation(&mut g, b, "Subtract");
    let stale = format!("{ROW_PREFIX}9999");
    let before = stored(&g, b);

    if let CommitTranslation::Store(name, value) =
        BooleanNode.translate_property_commit(node(&g, b), &stale, &PortValue::Bool(true))
    {
        g.set_property(b, name, value).unwrap();
    }

    assert_eq!(stored(&g, b), before, "the selection must not move");
    assert!(
        node(&g, b).properties.get(stale.as_str()).is_none(),
        "the stale row name must not become a property"
    );
}

// --------------------------------------------------- rows follow the wiring

/// Rows derive from the live inputs, so disconnecting one takes its row
/// with it — and the disconnect hook's pruning keeps the rest checked as
/// they were.
#[test]
fn disconnecting_an_input_removes_its_row() {
    let (mut g, reg, b, uids) = boolean_with_operands(3);
    set_operation(&mut g, b, "Subtract");
    set_selection(&mut g, b, &boolean_selection::encode(&[uids[0], uids[2]]));

    let noodle = *g
        .noodles()
        .iter()
        .find(|n| n.to.node == b && n.to.socket == uids[0])
        .expect("the first operand is wired");
    g.disconnect(&noodle, &reg).unwrap();

    let rows = checkboxes(&g, b);
    assert_eq!(rows.len(), 2, "the disconnected operand's row is gone");
    let checked: Vec<bool> = rows.into_iter().map(|(_, c)| c).collect();
    assert_eq!(checked, vec![false, true], "the surviving choice is intact");
    assert_eq!(stored(&g, b), boolean_selection::encode(&[uids[2]]));
}
