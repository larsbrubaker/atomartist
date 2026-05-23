//! Small free-function helpers around the AtomArtist graph that don't
//! belong inside any specific widget.  The canvas's add-node menu, the
//! top menu bar's "Add Node" action, and a handful of tests all use
//! [`add_node_with_defaults`] to insert a node with property defaults
//! filled from the registry.

use atomartist_lib::graph::node::NodeId;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;

/// Add a node of `type_id` at `position` with all properties seeded
/// from the type's `PropDef` defaults and sockets minted via
/// `NodeDef::instantiate`. Returns the new node's id, or `None` if the
/// type isn't registered.
///
/// Thin wrapper around [`Graph::add_new_node`]. Kept as a free function
/// so callers (file loaders, the top menu's add-node action,
/// integration tests) can stay independent of the canvas widget.
pub fn add_node_with_defaults(
    graph: &mut Graph,
    registry: &NodeRegistry,
    type_id: &str,
    position: [f64; 2],
) -> Option<NodeId> {
    graph.add_new_node(type_id, position, registry).ok()
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
        assert_eq!(n.type_id.as_ref(), "Box");
        assert!(n.properties.contains_key("width"));
        // Sockets minted via instantiate(): Box has one Geometry3d output.
        assert_eq!(n.outputs.len(), 1);
        assert_eq!(n.outputs[0].name.as_ref(), "out");
    }
}
