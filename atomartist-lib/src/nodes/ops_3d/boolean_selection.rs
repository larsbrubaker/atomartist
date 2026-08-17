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
//! The stored string is what the *node* reads. What the **user** sees is
//! one checkbox per operand socket, minted per instance by [`rows`] and
//! folded back into this one property by [`commit`] — see "The rows"
//! below.
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

use std::sync::Arc;

use crate::graph::node::{NodeInstance, PortValue};
use crate::graph::socket::SocketUid;
use crate::registry::{CommitTranslation, EditorKind, NodeProperties, PropDef};

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

// ---------------------------------------------------------------------
// The rows (plan step B-3b)
// ---------------------------------------------------------------------
//
// MatterCAD renders this selection as a titled group with one toggle per
// child (`BooleanObject3D.cs:153-161`, `SelectedChildren`). Ours is one
// **instance** property row per operand *socket*: the row list depends on
// what the user has wired up, which `NodeDef::properties()` — a per-type
// schema — cannot express, so the rows are minted through
// `NodeDef::instance_properties` and their commits folded back into the
// single stored `subtract_parts` string by
// `NodeDef::translate_property_commit`. Nothing named `subtract_part:*`
// is ever written to the instance: the row's *default* carries its
// checked state (the projection falls back to the default when a property
// is absent), and its commit becomes a write to `subtract_parts`.

/// Name prefix of a per-operand checkbox row: `subtract_part:<uid>`.
pub const ROW_PREFIX: &str = "subtract_part:";

/// Name of the read-only row that titles the group.
pub const HEADER_ROW: &str = "subtract_parts_group";

/// The group title, MatterCAD's label for the same control.
pub const HEADER_TEXT: &str = "Part(s) to Subtract";

/// True for a name minted by [`rows`] — the synthetic rows the node
/// interprets rather than stores.
pub fn is_row(name: &str) -> bool {
    name == HEADER_ROW || name.starts_with(ROW_PREFIX)
}

fn row_name(uid: SocketUid) -> String {
    format!("{ROW_PREFIX}{}", uid.0)
}

fn row_uid(name: &str) -> Option<SocketUid> {
    name.strip_prefix(ROW_PREFIX)?
        .trim()
        .parse::<u64>()
        .ok()
        .map(SocketUid)
}

/// A [`NodeProperties`] view of a live instance's property map, so the
/// selection helpers read the same way outside an evaluation as in one.
pub fn properties_of(node: &NodeInstance) -> NodeProperties {
    let mut props = NodeProperties::default();
    for (k, v) in &node.properties {
        props.insert(k.clone(), v.clone());
    }
    props
}

/// The operand slots, paired with the label to show for each: whatever
/// the socket shows on the canvas (the name it adopted from its source),
/// so a row and its noodle read as the same thing.
///
/// Two operands can wear the *same* display label — the adopted label is
/// `"<source type> - <source socket>"`, so two Boxes both read
/// "Box - out" — and two identical checkboxes name nothing. A repeated
/// label therefore gains its operand's 1-based position ("Box - out (2)").
/// Only the repeats are numbered: a node whose parts already read
/// distinctly stays clean.
fn operand_rows(node: &NodeInstance) -> Vec<(SocketUid, String)> {
    let uids = super::boolean_ops::operand_sockets_of(node);
    let mut rows: Vec<(SocketUid, String)> = node
        .inputs
        .iter()
        .filter(|s| uids.contains(&s.uid))
        .map(|s| {
            let label = s
                .display_label
                .as_ref()
                .map(|l| l.to_string())
                .unwrap_or_else(|| s.name.to_string());
            (s.uid, label)
        })
        .collect();
    let repeated: Vec<String> = rows
        .iter()
        .filter(|(_, l)| rows.iter().filter(|(_, o)| o == l).count() > 1)
        .map(|(_, l)| l.clone())
        .collect();
    for (i, (_, label)) in rows.iter_mut().enumerate() {
        if repeated.contains(label) {
            *label = format!("{} ({})", label, i + 1);
        }
    }
    rows
}

/// The group header plus one checkbox row per operand, in socket order.
///
/// Empty below two operands: with a single part wired in there is nothing
/// to subtract *from* (a lone operand is always a keep — see
/// [`removers`]), so every checkbox would be one the node refuses to
/// honour.
pub fn rows(node: &NodeInstance) -> Vec<PropDef> {
    let operands = operand_rows(node);
    if operands.len() < 2 {
        return Vec::new();
    }
    let uids: Vec<SocketUid> = operands.iter().map(|(u, _)| *u).collect();
    let chosen = removers(&uids, &properties_of(node));
    let mut out = vec![
        // An empty label hands the whole row width to the renderer, which
        // is how a read-only string paints as a plain line of text —
        // MatterCAD's group title, with the widgets agg-gui already has.
        PropDef::new(
            HEADER_ROW,
            PortValue::StringVal(Arc::new(HEADER_TEXT.to_string())),
        )
        .with_editor(EditorKind::StringReadOnly)
        .with_label(""),
    ];
    out.extend(operands.iter().map(|(uid, label)| {
        PropDef::new(row_name(*uid), PortValue::Bool(chosen.contains(uid)))
            .with_editor(EditorKind::Toggle)
            .with_label(label.as_str())
            .with_description("Checked: this part is cut out of the others. Unchecked: it is kept.")
    }));
    out
}

/// Fold a checkbox commit back into the stored selection.
///
/// The effective selection is **materialized first**: while the value is
/// still [`AUTO`] the rows show the default (the last operand) checked,
/// and the first click has to keep every other row exactly as it was
/// drawn — so the click writes the explicit list it was showing, with
/// this one entry flipped. From then on the choice is the user's, as in
/// MatterCAD, where picking a part replaces the auto-chosen default.
///
/// Materialization reads the wiring **at commit time**, not the wiring
/// the row was painted against. A connect landing between the two moves
/// the auto default onto the new last operand, so a click on a row drawn
/// a frame earlier materializes the *new* default rather than the one
/// the user was looking at. Accepted: the event loop is single-threaded,
/// so this needs a connection to complete inside the same click, and the
/// alternative — carrying the painted state through the commit — would
/// let a stale frame overwrite live wiring, which is worse.
///
/// A name this module does not own passes through untouched (the header
/// row commits nothing, and so does every ordinary property). A checkbox
/// naming a socket that is **no longer an operand** is *rejected*: the
/// row was drawn against a wiring that has since changed, and the two
/// alternatives are both wrong — storing it verbatim would leave a
/// `subtract_part:<uid>` property on the node (a phantom checkbox state
/// that outlives the socket), and folding it in would act on a part the
/// user can no longer see.
pub fn commit(node: &NodeInstance, name: &str, value: &PortValue) -> CommitTranslation {
    let uid = match row_uid(name) {
        Some(u) => u,
        None => return CommitTranslation::Passthrough,
    };
    let checked = matches!(value, PortValue::Bool(true));
    let operands: Vec<SocketUid> = operand_rows(node).into_iter().map(|(u, _)| u).collect();
    if !operands.contains(&uid) {
        return CommitTranslation::Reject;
    }
    let effective = removers(&operands, &properties_of(node));
    // Rebuilt in operand order rather than by pushing onto `effective`,
    // so the stored list always reads in the order the rows are drawn.
    let next: Vec<SocketUid> = operands
        .into_iter()
        .filter(|u| {
            if *u == uid {
                checked
            } else {
                effective.contains(u)
            }
        })
        .collect();
    CommitTranslation::Store(
        Arc::from(SUBTRACT_PARTS),
        PortValue::StringVal(Arc::new(encode(&next))),
    )
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
