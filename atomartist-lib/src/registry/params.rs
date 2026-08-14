//! Declarative parameter schema for node types — the single source of
//! truth from which a node's input sockets, property rows, and
//! `evaluate` accessors are all derived.
//!
//! ## Why this exists
//!
//! Historically every node hand-wrote three parallel things that had to
//! agree by hand:
//!
//!   1. `instantiate()` minted optional input sockets
//!      (`input_with_label(name, label, ty, true)`);
//!   2. `properties()` returned the matching [`PropDef`]s, each
//!      `.bind_input(socket)` paired to its socket; and
//!   3. `evaluate()` read each value with the "connected socket wins,
//!      else stored property, else default" fallback, spelled out
//!      per-field.
//!
//! Drift between those three lists caused real bugs (an editor range on
//! one side that didn't match the other). [`ParamSet`] collapses them
//! into one ordered declaration. From it a node gets:
//!
//!   - [`ParamSet::mint_sockets`] — appends one optional input socket per
//!     socketable param (declaration order) to a [`TemplateBuilder`];
//!   - [`ParamSet::prop_defs`] — the matching [`PropDef`]s, editor chosen
//!     by the param's value type unless overridden, `bind_input` wired to
//!     the socket;
//!   - [`ParamSet::reader`] — typed accessors (`r.number("width")`,
//!     `r.bool_("uniform")`, `r.color("color")`, `r.matrix("matrix")`)
//!     implementing the exact socket-else-property-else-default chain the
//!     nodes used to hand-write.
//!
//! This extends the existing `PropDef` / `EditorKind` vocabulary rather
//! than introducing a reflection dependency: the schema types here are
//! plain data.

use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::geometry::{primitive_color, DEFAULT_GEOMETRY_COLOR, INHERIT_COLOR};
use crate::graph::node::{identity_matrix, PortValue};

use super::{EditorKind, EvalCtx, NumberAttrs, PropDef, TemplateBuilder, VisibleWhen};

/// One declared parameter: a value that lives on the node (as a stored
/// property) and, by default, can also be driven by an upstream
/// connection over an optional input socket.
#[derive(Clone, Debug)]
struct Param {
    /// Property key — the stable serialization name and the key used to
    /// read the stored value in `evaluate`.
    name: Arc<str>,
    /// Human-readable label for the socket row and property panel.
    label: Arc<str>,
    /// Input-socket name. Defaults to `name` (the lowercase property
    /// key). Nodes override it with [`ParamSet::socket_named`] for two
    /// reasons: to give the socket the capitalized display convention
    /// (Box's `Width`, geometry nodes' `Color` / `Matrix`), or to match a
    /// pre-existing socket name a saved graph already references (Extrude,
    /// Cylinder, whose param sockets predate this schema).
    socket_name: Arc<str>,
    /// Default value; its `PortValue` variant also selects the socket
    /// type and the by-type editor.
    default: PortValue,
    /// Editor hint — defaulted by the value's type, override with
    /// [`ParamSet::editor`].
    editor: EditorKind,
    /// Advanced / conditional visibility gate.
    visible_when: VisibleWhen,
    /// Optional tooltip description.
    description: Option<Arc<str>>,
    /// Whether this param gets an input socket. `true` by default;
    /// [`ParamSet::no_socket`] opts out (property-only, no `bind_input`).
    socketable: bool,
}

impl Param {
    fn new(
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: PortValue,
        editor: EditorKind,
    ) -> Self {
        let name: Arc<str> = name.into();
        Self {
            socket_name: name.clone(),
            name,
            label: label.into(),
            default,
            editor,
            visible_when: VisibleWhen::Always,
            description: None,
            socketable: true,
        }
    }
}

/// An ordered, declarative set of node parameters. Build one with the
/// fluent constructors (`.number`, `.bool_`, `.color`, …) plus per-param
/// modifiers (`.integer()`, `.advanced()`, `.socket_named(..)`, …) that
/// apply to the most recently added param.
///
/// ## Registration-time panics (author errors)
///
/// The per-param modifiers panic at registration time on misuse, so a
/// mis-declared schema fails loudly the first time the node type is
/// registered rather than silently mis-rendering:
///
///   - a modifier (`.integer()`, `.socket_named(..)`, …) called before
///     any param has been added (empty set) panics; and
///   - a numeric modifier (`.integer()`, `.step(..)`, `.range(..)`, …)
///     applied to a non-numeric param (bool / color / string / enum)
///     panics.
///
/// These are programmer errors in `params()` definitions, caught by the
/// registry-wide sweeps the first time `register_all` runs.
#[derive(Clone, Debug, Default)]
pub struct ParamSet {
    params: Vec<Param>,
}

impl ParamSet {
    pub fn new() -> Self {
        Self { params: Vec::new() }
    }

    /// Preseed the standard `color` + `matrix` params every geometry-
    /// *producing* node carries, with the exact defaults and capitalized
    /// `"Color"` / `"Matrix"` socket names that [`wrap_mesh`] resolves.
    /// The default colour is [`DEFAULT_GEOMETRY_COLOR`] (opaque). Follow
    /// this with the node's own dimension params; they trail `color` +
    /// `matrix` so those render as the first two property rows.
    ///
    /// [`wrap_mesh`]: super::wrap_mesh
    pub fn geometry() -> Self {
        Self::new()
            .color("color", "Color", DEFAULT_GEOMETRY_COLOR)
            .socket_named("Color")
            .matrix("matrix", "Matrix", identity_matrix())
            .socket_named("Matrix")
    }

    /// Like [`ParamSet::geometry`], but seeded the way MatterCAD seeds a
    /// freshly created primitive:
    ///
    ///   * `color` is the primitive's own hue from MatterCAD's
    ///     `Object3DExtensions.PrimitiveColors` table (see
    ///     [`primitive_color`]) rather than the generic grey-blue, so a
    ///     scene of mixed shapes reads at a glance; and
    ///   * `matrix` lifts the shape by `bed_offset_z` so it *sits on
    ///     the bed* instead of being half-buried. Our generators build
    ///     Z-centred meshes exactly like MatterCAD's
    ///     `PlatonicSolids.CreateCube`, and MatterCAD likewise carries
    ///     the drop-to-bed translation on the item's own `Matrix`.
    ///
    /// `name` is MatterCAD's primitive name (`"Cube"`, `"Torus"`, …),
    /// which is not always our node's `type_id` — Box is MatterCAD's
    /// Cube. Pass half the shape's default height as `bed_offset_z`.
    pub fn primitive(name: &str, bed_offset_z: f32) -> Self {
        let mut m = identity_matrix();
        m[14] = bed_offset_z;
        Self::new()
            .color("color", "Color", primitive_color(name))
            .socket_named("Color")
            .matrix("matrix", "Matrix", m)
            .socket_named("Matrix")
    }

    /// Preseed the standard `color` + `matrix` params for *operation*
    /// nodes (Transform, Align, …) that act on an upstream `Geometry3d`.
    /// Identical to [`ParamSet::geometry`] except the default colour is
    /// the [`INHERIT_COLOR`] sentinel (alpha 0 = "use upstream's colour"),
    /// matching the inherit/compose semantics of
    /// [`compose_with_upstream`].
    ///
    /// [`compose_with_upstream`]: super::compose_with_upstream
    pub fn op() -> Self {
        Self::new()
            .color("color", "Color", INHERIT_COLOR)
            .socket_named("Color")
            .matrix("matrix", "Matrix", identity_matrix())
            .socket_named("Matrix")
    }

    // ---- value constructors (each appends a new param) ----

    /// A numeric param rendered as a drag-value editor over `range`.
    pub fn number(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: f64,
        range: RangeInclusive<f64>,
    ) -> Self {
        let attrs = NumberAttrs::with_range(*range.start(), *range.end());
        self.params.push(Param::new(
            name,
            label,
            PortValue::Number(default),
            EditorKind::NumberDrag(attrs),
        ));
        self
    }

    /// A numeric param with an explicit drag `step`.
    pub fn number_stepped(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: f64,
        range: RangeInclusive<f64>,
        step: f64,
    ) -> Self {
        let attrs = NumberAttrs::with_range(*range.start(), *range.end()).with_step(step);
        self.params.push(Param::new(
            name,
            label,
            PortValue::Number(default),
            EditorKind::NumberDrag(attrs),
        ));
        self
    }

    /// A boolean param rendered as a toggle.
    pub fn bool_(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: bool,
    ) -> Self {
        self.params.push(Param::new(
            name,
            label,
            PortValue::Bool(default),
            EditorKind::Toggle,
        ));
        self
    }

    /// An RGBA colour param rendered as a colour picker.
    pub fn color(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: [f32; 4],
    ) -> Self {
        self.params.push(Param::new(
            name,
            label,
            PortValue::Color(default),
            EditorKind::ColorPicker,
        ));
        self
    }

    /// A 4×4 matrix param rendered with the matrix editor.
    pub fn matrix(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: [f32; 16],
    ) -> Self {
        self.params.push(Param::new(
            name,
            label,
            PortValue::Matrix4x4(default),
            EditorKind::Matrix,
        ));
        self
    }

    /// A single-line string param.
    pub fn string(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: impl Into<String>,
    ) -> Self {
        self.params.push(Param::new(
            name,
            label,
            PortValue::StringVal(Arc::new(default.into())),
            EditorKind::StringSingleLine,
        ));
        self
    }

    /// A numeric param with **no** min/max bound — an unbounded drag
    /// editor. Used by params (e.g. Transform's translation offsets)
    /// that accept any value; contrast with [`ParamSet::number`], which
    /// always clamps to a declared range.
    pub fn number_unbounded(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default: f64,
    ) -> Self {
        self.params.push(Param::new(
            name,
            label,
            PortValue::Number(default),
            EditorKind::NumberDrag(NumberAttrs::default()),
        ));
        self
    }

    /// A first-class enum param: a string value constrained to one of
    /// `variants`, stored as [`PortValue::StringVal`] and rendered (by
    /// default) as an [`EditorKind::EnumDropdown`]. Override the widget
    /// with [`ParamSet::editor`] (`EnumButtons` / `EnumTabs`).
    ///
    /// Enum params mint **no input socket** (there is no socket type for
    /// an enum) — they are property-only by construction. Read them with
    /// [`ParamReader::enum_`], which validates the stored value against
    /// `variants` and falls back to `default_variant` for any illegal or
    /// legacy value (never panics).
    pub fn enum_(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        default_variant: impl Into<Arc<str>>,
        variants: &[&str],
    ) -> Self {
        let default: Arc<str> = default_variant.into();
        let variant_list: Vec<Arc<str>> = variants.iter().map(|v| Arc::from(*v)).collect();
        debug_assert!(
            variant_list.iter().any(|v| v.as_ref() == default.as_ref()),
            "enum default '{}' is not one of the declared variants",
            default,
        );
        let mut p = Param::new(
            name,
            label,
            PortValue::StringVal(Arc::new(default.to_string())),
            EditorKind::EnumDropdown { variants: variant_list },
        );
        // No socket type exists for enums — property-only by construction.
        p.socketable = false;
        self.params.push(p);
        self
    }

    // ---- per-param modifiers (apply to the last added param) ----

    fn last_mut(&mut self) -> &mut Param {
        self.params
            .last_mut()
            .expect("ParamSet modifier called before any param was added")
    }

    fn number_attrs_mut(&mut self) -> &mut NumberAttrs {
        match &mut self.last_mut().editor {
            EditorKind::NumberDrag(a) | EditorKind::Slider(a) => a,
            _ => panic!("numeric modifier applied to a non-numeric param"),
        }
    }

    /// Mark the last numeric param as integer-valued.
    pub fn integer(mut self) -> Self {
        self.number_attrs_mut().integer = true;
        self
    }

    /// Set the drag step of the last numeric param.
    pub fn step(mut self, step: f64) -> Self {
        self.number_attrs_mut().step = Some(step);
        self
    }

    /// Set the ease-in exponent of the last numeric param's drag.
    pub fn ease_in(mut self, e: f64) -> Self {
        self.number_attrs_mut().ease_in = Some(e);
        self
    }

    /// Enable snap-to-grid on the last numeric param's drag.
    pub fn snap_grid(mut self) -> Self {
        self.number_attrs_mut().snap_grid = true;
        self
    }

    /// Set the max decimal places rendered for the last numeric param.
    pub fn decimals(mut self, n: u8) -> Self {
        self.number_attrs_mut().max_decimal_places = Some(n);
        self
    }

    /// Override the last param's numeric range.
    pub fn range(mut self, min: f64, max: f64) -> Self {
        let a = self.number_attrs_mut();
        a.min = Some(min);
        a.max = Some(max);
        self
    }

    /// Override the last param's editor hint outright.
    pub fn editor(mut self, editor: EditorKind) -> Self {
        self.last_mut().editor = editor;
        self
    }

    /// Override the last param's input-socket name (for nodes preserving a
    /// legacy capitalized socket name distinct from the property key).
    pub fn socket_named(mut self, socket: impl Into<Arc<str>>) -> Self {
        self.last_mut().socket_name = socket.into();
        self
    }

    /// Drop the last param's input socket: it becomes a property-only
    /// param (no socket minted, no `bind_input`).
    pub fn no_socket(mut self) -> Self {
        self.last_mut().socketable = false;
        self
    }

    /// Gate the last param behind the node's "advanced" toggle.
    pub fn advanced(mut self) -> Self {
        self.last_mut().visible_when = VisibleWhen::AdvancedOn;
        self
    }

    /// Show the last param only in easy mode (hidden once "advanced" is
    /// on) — mirrors [`PropDef::easy_only`]. Used by the cylinder's
    /// read-only easy-mode hint row.
    pub fn easy_only(mut self) -> Self {
        self.last_mut().visible_when = VisibleWhen::AdvancedOff;
        self
    }

    /// Set the last param's visibility gate explicitly.
    pub fn visible_when(mut self, when: VisibleWhen) -> Self {
        self.last_mut().visible_when = when;
        self
    }

    /// Set the last param's tooltip description.
    pub fn description(mut self, text: impl Into<Arc<str>>) -> Self {
        self.last_mut().description = Some(text.into());
        self
    }

    // ---- derivations ----

    /// Append one optional input socket per socketable param, in
    /// declaration order, to `builder`. Call this after any
    /// node-specific geometry inputs so params trail them.
    pub fn mint_sockets<'a>(&self, mut builder: TemplateBuilder<'a>) -> TemplateBuilder<'a> {
        for p in &self.params {
            if !p.socketable {
                continue;
            }
            builder = builder.input_with_label(
                p.socket_name.clone(),
                p.label.clone(),
                p.default.socket_type(),
                true,
            );
        }
        builder
    }

    /// The matching [`PropDef`] list, one per param in declaration order.
    /// The editor defaults by value type (set at construction) unless
    /// overridden; socketable params get `bind_input` to their socket.
    pub fn prop_defs(&self) -> Vec<PropDef> {
        self.params
            .iter()
            .map(|p| {
                let mut d = PropDef::new(p.name.clone(), p.default.clone())
                    .with_editor(p.editor.clone())
                    .with_label(p.label.clone())
                    .visible_when(p.visible_when);
                if let Some(desc) = &p.description {
                    d = d.with_description(desc.clone());
                }
                if p.socketable {
                    d = d.bind_input(p.socket_name.clone());
                }
                d
            })
            .collect()
    }

    /// A reader bound to an evaluation context, exposing typed accessors
    /// that resolve socket-else-property-else-default.
    pub fn reader<'a>(&'a self, ctx: &'a EvalCtx<'a>) -> ParamReader<'a> {
        ParamReader { params: self, ctx }
    }

    fn param(&self, name: &str) -> &Param {
        self.params
            .iter()
            .find(|p| p.name.as_ref() == name)
            .unwrap_or_else(|| panic!("no param named '{name}' in this ParamSet"))
    }
}

/// Typed value accessors over a [`ParamSet`] and an [`EvalCtx`],
/// implementing the resolution chain every node evaluate used to
/// hand-write: a connected input socket carrying the matching
/// `PortValue` variant wins; otherwise the stored property; otherwise the
/// param's declared default.
///
/// Every accessor takes the param `name` (the property key, not the
/// socket name) and **panics if no param by that name was declared** on
/// the [`ParamSet`]. That is an author error — reading a param the schema
/// never declared — and is caught the first time the node evaluates.
pub struct ParamReader<'a> {
    params: &'a ParamSet,
    ctx: &'a EvalCtx<'a>,
}

impl<'a> ParamReader<'a> {
    /// Read a numeric param.
    pub fn number(&self, name: &str) -> f64 {
        let p = self.params.param(name);
        if p.socketable {
            if let PortValue::Number(n) = self.ctx.input_named(&p.socket_name) {
                return *n;
            }
        }
        match self.ctx.properties.get(name) {
            PortValue::Number(n) => *n,
            _ => match &p.default {
                PortValue::Number(n) => *n,
                _ => 0.0,
            },
        }
    }

    /// Read a boolean param.
    pub fn bool_(&self, name: &str) -> bool {
        let p = self.params.param(name);
        if p.socketable {
            if let PortValue::Bool(b) = self.ctx.input_named(&p.socket_name) {
                return *b;
            }
        }
        match self.ctx.properties.get(name) {
            PortValue::Bool(b) => *b,
            _ => matches!(&p.default, PortValue::Bool(true)),
        }
    }

    /// Read an RGBA colour param.
    pub fn color(&self, name: &str) -> [f32; 4] {
        let p = self.params.param(name);
        if p.socketable {
            if let PortValue::Color(c) = self.ctx.input_named(&p.socket_name) {
                return *c;
            }
        }
        match self.ctx.properties.get(name) {
            PortValue::Color(c) => *c,
            _ => match &p.default {
                PortValue::Color(c) => *c,
                _ => [0.0, 0.0, 0.0, 1.0],
            },
        }
    }

    /// Read a 4×4 matrix param.
    pub fn matrix(&self, name: &str) -> [f32; 16] {
        let p = self.params.param(name);
        if p.socketable {
            if let PortValue::Matrix4x4(m) = self.ctx.input_named(&p.socket_name) {
                return *m;
            }
        }
        match self.ctx.properties.get(name) {
            PortValue::Matrix4x4(m) => *m,
            _ => match &p.default {
                PortValue::Matrix4x4(m) => *m,
                _ => crate::graph::node::identity_matrix(),
            },
        }
    }

    /// Read an enum param's current variant. Validates the stored value
    /// against the param's declared variants; any illegal or legacy value
    /// resolves to the declared default variant (never panics). Enum
    /// params carry no socket, so only the property store and default
    /// participate in resolution.
    pub fn enum_(&self, name: &str) -> &'a str {
        let p = self.params.param(name);
        let variants: &'a [Arc<str>] = match &p.editor {
            EditorKind::EnumDropdown { variants }
            | EditorKind::EnumButtons { variants }
            | EditorKind::EnumTabs { variants } => variants.as_slice(),
            _ => &[],
        };
        let default: &'a str = match &p.default {
            PortValue::StringVal(s) => s.as_str(),
            _ => "",
        };
        let stored: &'a str = match self.ctx.properties.get(name) {
            PortValue::StringVal(s) => s.as_str(),
            _ => default,
        };
        // A legal stored value wins; borrow the matching variant so the
        // returned &str lives for the reader's lifetime.
        for v in variants {
            if v.as_ref() == stored {
                return v.as_ref();
            }
        }
        // Illegal / legacy value → declared default (return the variant
        // borrow when the default is itself a variant, else the default).
        for v in variants {
            if v.as_ref() == default {
                return v.as_ref();
            }
        }
        default
    }

    /// Read a string param.
    pub fn string(&self, name: &str) -> Arc<String> {
        let p = self.params.param(name);
        if p.socketable {
            if let PortValue::StringVal(s) = self.ctx.input_named(&p.socket_name) {
                return s.clone();
            }
        }
        match self.ctx.properties.get(name) {
            PortValue::StringVal(s) => s.clone(),
            _ => match &p.default {
                PortValue::StringVal(s) => s.clone(),
                _ => Arc::new(String::new()),
            },
        }
    }
}


#[cfg(test)]
#[path = "params/tests.rs"]
mod tests;
