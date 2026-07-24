//! Registry-wide integrity check for `PropDef::bound_input` ↔ optional
//! input socket bindings.
//!
//! `bind_input("SocketName")` is what makes a property's inline editor
//! render on a node-row's input socket (and hide itself once that socket
//! is wired). If the string passed to `bind_input` doesn't match any real
//! socket name, the editor silently detaches — no compile error, no panic,
//! just a missing widget. Conversely, an optional input socket with no
//! property bound to it has no inline editor at all, which is usually a
//! forgotten `bind_input` rather than a deliberate choice.
//!
//! This test builds the full production registry (`nodes::register_all`)
//! and, for every registered def, instantiates it and asserts both
//! directions:
//!
//!   (a) every `PropDef` whose `bound_input` is `Some` names a socket that
//!       actually exists in the instantiate template's inputs (exact name
//!       match); and
//!   (b) every OPTIONAL input socket in the template has some `PropDef`
//!       bound to it — except sockets covered by [`is_exempt_optional`].
//!
//! Direction (a) guards against `bind_input` typos; direction (b) guards
//! against a new optional socket landing without its inline editor.

use atomartist_lib::graph::socket::{Socket, SocketUidAlloc};
use atomartist_lib::nodes::register_all;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::SocketType;

/// Optional input sockets that legitimately have no bound `PropDef`.
///
/// Documented exemptions:
///
///   - **Dynamic trailing placeholder slot** (Output, Combine): an
///     always-present empty input the user drags a wire onto. Output mints
///     it as `("", SocketType::None)`; Combine as `("", SocketType::Geometry3d)`.
///     It carries no inline editor — its whole purpose is to accept a
///     connection and then spawn a concretely-named slot. Identified by an
///     empty socket name, which no editor-backed socket ever uses.
///   - **`SocketType::None` placeholder**: the untyped "accept any drop"
///     slot. Never editable inline (there is no editor for an unknown
///     type). Covered separately in case a future dynamic node names its
///     placeholder non-empty.
fn is_exempt_optional(socket: &Socket) -> bool {
    socket.name.as_ref().is_empty() || socket.socket_type == SocketType::None
}

fn production_registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    register_all(&mut reg);
    reg
}

#[test]
fn bound_inputs_name_real_sockets() {
    let reg = production_registry();
    let mut violations: Vec<String> = Vec::new();

    for def in reg.iter() {
        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        for prop in def.properties() {
            let Some(bound) = prop.bound_input.as_ref() else {
                continue;
            };
            let exists = tpl
                .inputs
                .iter()
                .any(|s| s.name.as_ref() == bound.as_ref());
            if !exists {
                violations.push(format!(
                    "  {}: property '{}' binds input '{}' which is not a socket \
                     (sockets: {:?})",
                    def.type_id(),
                    prop.name,
                    bound,
                    tpl.inputs.iter().map(|s| s.name.as_ref()).collect::<Vec<_>>(),
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "bind_input names a nonexistent socket (typo detaches the inline editor):\n{}",
        violations.join("\n"),
    );
}

#[test]
fn optional_inputs_have_bound_properties() {
    let reg = production_registry();
    let mut violations: Vec<String> = Vec::new();

    for def in reg.iter() {
        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        let props = def.properties();
        for input in tpl.inputs.iter().filter(|s| s.optional) {
            if is_exempt_optional(input) {
                continue;
            }
            let bound = props.iter().any(|p| {
                p.bound_input.as_ref().map(|b| b.as_ref()) == Some(input.name.as_ref())
            });
            if !bound {
                violations.push(format!(
                    "  {}: optional input '{}' ({:?}) has no PropDef bound to it",
                    def.type_id(),
                    input.name,
                    input.socket_type,
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "optional input socket has no inline editor (missing bind_input?):\n{}",
        violations.join("\n"),
    );
}
