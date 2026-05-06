//! Small free-function helpers around the AtomArtist graph that don't
//! belong inside any specific widget.  The canvas's add-node menu, the
//! top menu bar's "Add Node" action, and a handful of tests all use
//! [`add_node_with_defaults`] to insert a node with property defaults
//! filled from the registry.

use atomartist_lib::graph::node::{NodeId, NodeInstance};
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;

/// Add a node of `type_id` at `position` with all properties seeded
/// from the type's `PropDef` defaults.  Returns the new node's id, or
/// `None` if the type isn't registered.
///
/// Lives here (rather than inside the canvas widget) so callers that
/// don't depend on the widget tree — file loaders, the top menu's
/// add-node action, integration tests — can still use it.
pub fn add_node_with_defaults(
    graph: &mut Graph,
    registry: &NodeRegistry,
    type_id: &'static str,
    position: [f64; 2],
) -> Option<NodeId> {
    let def = registry.get(type_id)?;
    let id = graph.allocate_id();
    let mut node = NodeInstance::new(id, type_id, position);
    for prop in def.properties() {
        node.properties.insert(prop.name, prop.default);
    }
    graph.add_node(node).ok()?;
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomartist_lib::nodes;

    #[test]
    fn add_node_helper_inserts_with_defaults() {
        let mut reg = NodeRegistry::new();
        nodes::register_all(&mut reg);
        let mut g = Graph::new();
        let id = add_node_with_defaults(&mut g, &reg, "Box", [10.0, 20.0]).unwrap();
        let n = g.get(id).unwrap();
        assert_eq!(n.type_id, "Box");
        assert!(n.properties.contains_key("width"));
    }
}
