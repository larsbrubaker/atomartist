//! Registry-wide policy sweep: every editable number row uses the
//! DragValue-style control (`EditorKind::NumberDrag`), not a slider.
//!
//! The owner's product decision is that number properties render as the
//! drag-value pill (left/right arrows + centred value + click-to-edit),
//! never as a thermometer-fill slider. A slider whose value sits at its
//! range minimum paints as an empty box with no affordance (the "Radius
//! renders as a plain box" bug), so the whole app standardises on
//! `NumberDrag`.
//!
//! The single sanctioned exception is `NumberConst` (the Input > Number
//! node), which keeps an explicit slider for NodeDesigner parity — its
//! entire purpose is to be a bounded slider whose min/max/step the user
//! configures. If the owner wants to fold that in too, drop the
//! exemption below.
//!
//! Modelled on `tests/socket_property_bindings.rs`: build the full
//! production registry and sweep every registered def's `properties()`.

use atomartist_lib::graph::node::PortValue;
use atomartist_lib::nodes::register_all;
use atomartist_lib::registry::{EditorKind, NodeRegistry};

/// Node `type_id`s allowed to surface a non-`NumberDrag` numeric editor
/// (i.e. a `Slider`) on a `Number` property. `NumberConst` is the single
/// sanctioned slider for NodeDesigner parity.
const SLIDER_EXEMPT: &[&str] = &["NumberConst"];

fn production_registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    register_all(&mut reg);
    reg
}

#[test]
fn no_node_surfaces_slider_editor_except_number_const() {
    let reg = production_registry();
    let mut violations: Vec<String> = Vec::new();

    for def in reg.iter() {
        if SLIDER_EXEMPT.contains(&def.type_id()) {
            continue;
        }
        for prop in def.properties() {
            if matches!(prop.editor, EditorKind::Slider(_)) {
                violations.push(format!(
                    "  {}: property '{}' uses EditorKind::Slider — number rows \
                     must use EditorKind::NumberDrag",
                    def.type_id(),
                    prop.name,
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "number rows must use the DragValue-style NumberDrag editor, not Slider:\n{}",
        violations.join("\n"),
    );
}

/// Stronger sweep: every `Number`-typed property must surface the
/// `NumberDrag` editor — never `Display` (a plain, arrow-less box), a
/// slider, or any non-numeric editor. This is the "every editable number
/// row shows the arrows + centred-value control" guarantee: a number row
/// that fell back to `Display` would render as a dead box with no drag
/// affordance (the class of bug the Radius pill exhibited). `NumberConst`
/// keeps its explicit slider.
#[test]
fn every_number_property_uses_the_numberdrag_editor() {
    let reg = production_registry();
    let mut violations: Vec<String> = Vec::new();

    for def in reg.iter() {
        if SLIDER_EXEMPT.contains(&def.type_id()) {
            continue;
        }
        for prop in def.properties() {
            if !matches!(prop.default, PortValue::Number(_)) {
                continue;
            }
            if !matches!(prop.editor, EditorKind::NumberDrag(_)) {
                violations.push(format!(
                    "  {}: Number property '{}' uses {:?} — every editable \
                     number row must use EditorKind::NumberDrag",
                    def.type_id(),
                    prop.name,
                    prop.editor,
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Number-typed properties must render as the DragValue-style NumberDrag \
         control (arrows + centred value), not Display / slider / non-numeric:\n{}",
        violations.join("\n"),
    );
}
