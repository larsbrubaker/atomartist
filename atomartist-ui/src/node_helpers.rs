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

/// Canvas position for a node added from the top menu (which, unlike the
/// right-click canvas popup, carries no cursor location). Placing every
/// menu-added node at one fixed point made successive adds stack on top
/// of each other and land behind the starter graph's nodes — invisible
/// to hit-testing, so they couldn't be selected, moved, or wired. Instead
/// we drop the new node just to the right of the right-most node in the
/// current (lowest) row, so it always lands in open space and successive
/// adds cascade rightward.
///
/// To keep the cascade from marching off toward +X forever, a row wraps
/// once it would extend past [`ROW_MAX_EXTENT`] units from the left-most
/// node: the next node starts a fresh row *below* the lowest existing node.
///
/// Y-up canvas convention: `position` is the node's top-left, and "below"
/// means a *smaller* Y. Placement is a pure function of existing node
/// positions, so it stays deterministic and overlap-free.
pub fn next_menu_add_position(graph: &Graph) -> [f64; 2] {
    // Clear enough of any node body (~180 wide) that the new node's title
    // bar and sockets never overlap the previous right-most node.
    const CASCADE_GAP: f64 = 220.0;
    // Wrap to a new row once a row would reach this far from the left-most
    // node (~6 columns of CASCADE_GAP), bounding the graph's X extent.
    const ROW_MAX_EXTENT: f64 = 1400.0;
    // Vertical drop to the next row; ~one node body tall so rows never
    // overlap. Y-up: a lower row has a smaller Y.
    const ROW_GAP: f64 = 200.0;
    // How close in Y a node must be to the lowest node to count as part of
    // the current (bottom) row. Half of ROW_GAP keeps adjacent rows'
    // bands from overlapping.
    const ROW_BAND: f64 = ROW_GAP * 0.5;
    // Fallback drop point for an empty canvas — matches the starter row.
    const DEFAULT_POS: [f64; 2] = [80.0, 240.0];

    // Snapshot the extents we need in a single pass.
    let mut leftmost_x = f64::INFINITY;
    let mut lowest_y = f64::INFINITY;
    let mut any = false;
    for n in graph.nodes() {
        any = true;
        leftmost_x = leftmost_x.min(n.position[0]);
        lowest_y = lowest_y.min(n.position[1]);
    }
    if !any {
        return DEFAULT_POS;
    }

    // The current row is the band of nodes sitting near the lowest Y; find
    // its right-most node and cascade from there.
    let row_right = graph
        .nodes()
        .filter(|n| (n.position[1] - lowest_y).abs() <= ROW_BAND)
        .max_by(|a, b| {
            a.position[0]
                .partial_cmp(&b.position[0])
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    match row_right {
        Some(r) => {
            let candidate_x = r.position[0] + CASCADE_GAP;
            if candidate_x <= leftmost_x + ROW_MAX_EXTENT {
                // Still room in this row: continue the rightward cascade.
                [candidate_x, r.position[1]]
            } else {
                // Row is full: start a fresh one below everything (Y-up:
                // smaller Y), aligned back to the left-most column.
                [leftmost_x, lowest_y - ROW_GAP]
            }
        }
        None => DEFAULT_POS,
    }
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
