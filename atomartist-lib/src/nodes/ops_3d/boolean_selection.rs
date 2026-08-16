//! "Part(s) to Subtract" — which of the Boolean node's n-ary operands are
//! removers rather than keeps (MatterCAD's `SelectedChildren`,
//! `BooleanObject3D.cs:153-161`).
//!
//! ## What identifies a part
//!
//! MatterCAD selects children by their `ID`. Our operands are input
//! *sockets*, so the equivalent stable key is the socket's
//! [`SocketUid`]: it is minted once, preserved when the slot adopts a
//! source's name on connect, referenced by every noodle, and written to
//! (and restored from) the project file by
//! [`crate::serialization::graph_json`]. The socket *name* is not stable —
//! it is adopted from whatever output happens to feed the slot, and two
//! rewires can swap two names — so the selection is keyed by uid and
//! stored as a comma-separated list of uids in the `subtract_parts`
//! property.
//!
//! ## Three states, not two
//!
//! The stored string distinguishes "the user has not chosen" from "the
//! user chose nothing", which a plain list cannot:
//!
//! | Stored | Meaning |
//! |---|---|
//! | [`AUTO`] (the schema default) | nobody has chosen — the **last** connected input is the remover (`ChooseDefaultsForWrappedChildren`, L418-429) |
//! | `""` | chosen, and empty — every operand is a keep |
//! | `"7,9"` | those sockets are the removers |
//!
//! The distinction has to survive a save/load, and a loaded graph always
//! carries every declared property (absent keys are seeded with the
//! schema default), so "absent" cannot be the unset signal — hence the
//! explicit sentinel.
//!
//! ## Pruning, and what it does *not* cover
//!
//! Stale uids are dropped from the stored value in the Boolean node's
//! **disconnect hook** (MatterCAD's `CleanUpSelectedChildrenIDs`,
//! L435-450) — that is the one path that rewrites the property.
//!
//! Other paths never reach it. Deleting the *source* node detaches its
//! noodles through `Graph::remove_node`, which does not fire the target's
//! disconnect hook, so the Boolean is left holding an orphan input slot
//! (named, wired to nothing) and, if that slot was selected, a selection
//! entry pointing at it. Neither is harmful: [`removers`] filters against
//! the live operand list every time it reads, and an orphan slot simply
//! contributes no geometry. The orphan slot itself is a pre-existing wart
//! shared with Combine — both leave the collapsed-slot bookkeeping to the
//! disconnect hook — and cleaning it up belongs in a graph-model pass, not
//! here.

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUid;
use crate::registry::NodeProperties;

/// Property key holding the selection.
pub const SUBTRACT_PARTS: &str = "subtract_parts";

/// Sentinel value meaning "no explicit choice yet" — see the module docs.
pub const AUTO: &str = "auto";

/// Encode a selection for storage.
pub fn encode(uids: &[SocketUid]) -> String {
    uids.iter()
        .map(|u| u.0.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a stored selection. `None` means the value is the [`AUTO`]
/// sentinel (or a variant this build does not understand), i.e. "nobody
/// has chosen".
pub fn decode(stored: &str) -> Option<Vec<SocketUid>> {
    let s = stored.trim();
    if s.eq_ignore_ascii_case(AUTO) {
        return None;
    }
    if s.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        match part.trim().parse::<u64>() {
            Ok(n) => out.push(SocketUid(n)),
            // A token that is not a uid is dropped rather than turning
            // the whole selection into "nobody has chosen" — the rest of
            // the user's choice is still honourable.
            Err(_) => continue,
        }
    }
    Some(out)
}

/// The raw stored string for a node, or [`AUTO`] when the property is
/// missing or wrong-typed.
pub fn stored(props: &NodeProperties) -> String {
    match props.get(SUBTRACT_PARTS) {
        PortValue::StringVal(s) => s.as_str().to_string(),
        _ => AUTO.to_string(),
    }
}

/// Which of `operands` (input socket uids, in display order) are removers.
///
/// A **lone** operand is always a keep — MatterCAD's `GetSubtractItems`
/// returns `(source, null)` for a single child (BooleanMeshBuilder.cs:550-553),
/// so a Subtract with one thing wired in passes it through instead of
/// erasing it.
pub fn removers(operands: &[SocketUid], props: &NodeProperties) -> Vec<SocketUid> {
    if operands.len() < 2 {
        return Vec::new();
    }
    match decode(&stored(props)) {
        Some(chosen) => chosen
            .into_iter()
            .filter(|u| operands.contains(u))
            .collect(),
        None => operands.last().copied().into_iter().collect(),
    }
}

/// Drop uids that no longer name a live input socket. Returns `None` when
/// nothing changed (including the [`AUTO`] case, which names nothing and
/// so can never go stale).
pub fn prune(stored_value: &str, live: &[SocketUid]) -> Option<String> {
    let chosen = decode(stored_value)?;
    let kept: Vec<SocketUid> = chosen
        .iter()
        .copied()
        .filter(|u| live.contains(u))
        .collect();
    if kept.len() == chosen.len() {
        return None;
    }
    Some(encode(&kept))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props_with(value: &str) -> NodeProperties {
        let mut p = NodeProperties::default();
        p.insert(
            SUBTRACT_PARTS,
            PortValue::StringVal(std::sync::Arc::new(value.to_string())),
        );
        p
    }

    #[test]
    fn the_unset_sentinel_makes_the_last_input_the_remover() {
        let ops = [SocketUid(3), SocketUid(5), SocketUid(9)];
        assert_eq!(removers(&ops, &props_with(AUTO)), vec![SocketUid(9)]);
        // A node whose property was never written at all reads the same.
        assert_eq!(
            removers(&ops, &NodeProperties::default()),
            vec![SocketUid(9)]
        );
    }

    #[test]
    fn an_explicitly_empty_selection_is_not_the_default() {
        let ops = [SocketUid(3), SocketUid(5)];
        assert!(
            removers(&ops, &props_with("")).is_empty(),
            "an empty choice must stay empty, not fall back to the last input"
        );
    }

    #[test]
    fn a_lone_operand_is_always_a_keep() {
        let ops = [SocketUid(3)];
        assert!(removers(&ops, &props_with(AUTO)).is_empty());
        assert!(
            removers(&ops, &props_with("3")).is_empty(),
            "even an explicit choice cannot subtract the only part there is"
        );
    }

    #[test]
    fn stale_uids_are_ignored_and_prunable() {
        let live = [SocketUid(3), SocketUid(5)];
        assert_eq!(removers(&live, &props_with("5,42")), vec![SocketUid(5)]);
        assert_eq!(prune("5,42", &live).as_deref(), Some("5"));
        assert_eq!(prune("5", &live), None, "nothing stale → no rewrite");
        assert_eq!(prune(AUTO, &live), None, "the sentinel names nothing");
    }

    #[test]
    fn a_selection_round_trips_through_the_stored_string() {
        let sel = [SocketUid(11), SocketUid(2)];
        assert_eq!(encode(&sel), "11,2");
        assert_eq!(decode("11,2"), Some(sel.to_vec()));
    }
}
