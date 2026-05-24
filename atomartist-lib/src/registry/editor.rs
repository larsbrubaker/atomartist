//! Editor metadata for node properties.
//!
//! ## Phase 1 of the dynamic-row migration
//!
//! As of the dynamic-node-row pivot the shared widget-vocabulary types
//! ([`EditorKind`], [`NumberAttrs`], [`NodeFieldAttrs`]) live in
//! `agg-gui/widgets/property_row`. This module re-exports them so
//! every downstream caller in atomartist continues to refer to
//! `atomartist_lib::registry::EditorKind` exactly as before. The
//! local-only type that remains here is [`PropDef`] — the node-side
//! binding that pairs an [`EditorKind`] with a default
//! [`PortValue`](crate::graph::node::PortValue) and a property name.
//!
//! Phase 2 will introduce a row factory at the agg-gui layer that
//! takes an `EditorKind` + a value + an on-change callback and emits
//! the concrete widget. Phase 3 will move the property-panel widget
//! tree itself to agg-gui, leaving atomartist responsible only for
//! "what is the schema for this node?" — not "how do we render it?"

use std::sync::Arc;

pub use agg_gui::widgets::{EditorKind, NodeFieldAttrs, NumberAttrs};

use crate::graph::node::PortValue;

/// Description of one settable property on a node type.
///
/// Properties are values stored on the node itself (as opposed to flowing
/// in over a socket connection). They appear as widgets on the node box on
/// the canvas, and as rows in the right-side property panel when the node
/// is selected.
///
/// This type embeds a [`PortValue`] for its default and so cannot move
/// to the agg-gui layer with the rest of the editor vocabulary. The
/// fields it does carry mirror [`NodeFieldAttrs`] one-for-one so the
/// agg-gui-side row factory can read everything it needs off either
/// type interchangeably.
#[derive(Clone, Debug)]
pub struct PropDef {
    pub name: Arc<str>,
    pub default: PortValue,
    /// Inclusive minimum for numeric properties. Mirrored from
    /// `editor.numeric_range()` for backwards compatibility.
    pub min: Option<f64>,
    /// Inclusive maximum.
    pub max: Option<f64>,
    /// Display label override. Falls back to `name` when `None`.
    pub label: Option<Arc<str>>,
    /// Editor hint — the UI layer picks the widget; the schema
    /// describes the intent + numeric range / integer-ness.
    pub editor: EditorKind,
    /// When `Some(socket_name)`, the property is rendered inline on
    /// that input socket's row. The editor hides itself when the
    /// socket is connected.
    pub bound_input: Option<Arc<str>>,
    /// Free-text description shown in tooltips. MatterCAD's
    /// `[Description]`.
    pub description: Option<Arc<str>>,
    /// Whether this property belongs to the "Advanced" section — only
    /// shown when the node's `Advanced` toggle is on.
    pub advanced: bool,
    /// Hidden from the property panel entirely. MatterCAD's
    /// `[HideFromEditor]`.
    pub hidden: bool,
}

impl PropDef {
    pub fn new(name: impl Into<Arc<str>>, default: PortValue) -> Self {
        Self {
            name: name.into(),
            default,
            min: None,
            max: None,
            label: None,
            editor: EditorKind::default(),
            bound_input: None,
            description: None,
            advanced: false,
            hidden: false,
        }
    }

    /// Set an inclusive numeric range. Updates both the legacy
    /// `min`/`max` fields and the `editor`'s numeric attrs so callers
    /// using either API see consistent values.
    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        match &mut self.editor {
            EditorKind::NumberDrag(a) | EditorKind::Slider(a) => {
                a.min = Some(min);
                a.max = Some(max);
            }
            other => {
                *other = EditorKind::NumberDrag(NumberAttrs {
                    min: Some(min),
                    max: Some(max),
                    ..Default::default()
                });
            }
        }
        self
    }

    /// Override the editor hint. Numeric ranges on the hint are
    /// mirrored onto `min`/`max` so legacy callers keep working.
    pub fn with_editor(mut self, editor: EditorKind) -> Self {
        let (mn, mx) = editor.numeric_range();
        if mn.is_some() {
            self.min = mn;
        }
        if mx.is_some() {
            self.max = mx;
        }
        self.editor = editor;
        self
    }

    /// Set the human-readable display label.
    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the description (tooltip text).
    pub fn with_description(mut self, text: impl Into<Arc<str>>) -> Self {
        self.description = Some(text.into());
        self
    }

    /// Mark this property as belonging to the Advanced section.
    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }

    /// Hide from the property panel.
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Bind the property to an input socket: the canvas will render
    /// the inline editor on that socket's row, and hide it once the
    /// socket is connected.
    pub fn bind_input(mut self, socket_name: impl Into<Arc<str>>) -> Self {
        self.bound_input = Some(socket_name.into());
        self
    }

    /// Construct a `PropDef` from a [`NodeFieldAttrs`] + default value.
    /// Used by reflected property structs to mint their `PropDef`s.
    pub fn from_attrs(
        name: impl Into<Arc<str>>,
        default: PortValue,
        attrs: &NodeFieldAttrs,
    ) -> Self {
        let mut p = PropDef::new(name, default).with_editor(attrs.editor.clone());
        if let Some(l) = &attrs.label {
            p = p.with_label(l.clone());
        }
        if let Some(d) = &attrs.description {
            p = p.with_description(d.clone());
        }
        if let Some(s) = &attrs.bound_input {
            p = p.bind_input(s.clone());
        }
        if attrs.advanced {
            p = p.advanced();
        }
        if attrs.hidden {
            p = p.hidden();
        }
        p
    }
}
