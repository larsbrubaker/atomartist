//! Favorites data model (`docs/file-browser-design.md` §2, §5 step 6d-1).
//!
//! The favorites row is AtomArtist's descendant of MatterCAD's
//! `FavoritesService`: an ordered list of `{kind, stable_key,
//! display_name}` records that the left rail paints as buttons. This
//! module is the *data* half — no widgets. The rail (6d-2) renders
//! [`Favorites::list`] and the drag-insert controller (6e) consumes
//! [`Favorite::resolve`].
//!
//! Two kinds ship in v1 (design §7.3):
//!
//! * [`FavoriteKind::NodeType`] — a primitive from the node palette.
//!   The stable key is the node type's registry id
//!   ([`NodeDef::type_id`]), which is the same string the serializer
//!   writes into `.atmr` graphs, so it is stable across renames of
//!   the *display* name.
//! * [`FavoriteKind::Project`] — a pinned project file. The stable
//!   key is a [`StorageUri`] string.
//!
//! **Seeding.** A fresh install gets the 3-D primitive palette
//! ([`SEED_NODE_TYPES`]). A user who deletes every favorite must keep
//! an empty row across restarts, so "user emptied" is distinguished
//! from "never seeded" by the persisted [`Favorites::seeded`] flag —
//! [`Favorites::seed_defaults_once`] is a no-op once that flag is set.
//! Both live in [`UiSettings`](crate::settings::UiSettings), which both
//! shells persist (native file / web localStorage).
//!
//! **Display names.** The stored `display_name` is a *fallback*.
//! Lookup by key wins: [`Favorite::resolve`] asks the live
//! [`NodeRegistry`] for the current display name so a renamed node type
//! immediately renames its favorite. The stored copy is what the rail
//! shows when the registry has no such type (unregistered node type,
//! or a `Project` favorite, which has no registry at all).
//!
//! **Dead entries are not pruned.** A favorite whose provider is
//! offline (or whose node type is temporarily unregistered) resolves to
//! [`FavoriteResolution::Dead`] so the rail can grey it out, but it
//! stays in the settings file — the provider may come back. That is
//! MatterCAD's behaviour and the design's explicit rule.

use std::str::FromStr;
use std::sync::Arc;

use atomartist_lib::registry::{NodeDef, NodeRegistry};
use atomartist_storage::StorageUri;

/// Node-type registry ids seeded into an empty, never-seeded favorites
/// list. These are the `Primitives 3D` category in registration order
/// (`atomartist_lib::nodes::primitives_3d::register_all`), which is the
/// palette MatterCAD's favorites bar seeds with.
pub const SEED_NODE_TYPES: &[&str] = &[
    "Box", "Cylinder", "Sphere", "Cone", "Torus", "Pyramid", "Wedge",
];

/// What a favorite points at. The kind keeps the palette entries and
/// the pinned storage entries cleanly separated (design §7.3) — they
/// are activated differently (add a node vs open a project) and
/// resolved against different authorities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FavoriteKind {
    /// A node type from the palette; `stable_key` is its registry id.
    NodeType,
    /// A project file; `stable_key` is a [`StorageUri`] string.
    Project,
}

impl FavoriteKind {
    /// Token written to the settings file. Stable — changing it
    /// orphans existing favorites.
    pub fn key(self) -> &'static str {
        match self {
            FavoriteKind::NodeType => "node_type",
            FavoriteKind::Project => "project",
        }
    }

    /// Parse a persisted token. Unknown tokens return `None` so the
    /// whole line is dropped rather than mis-typed (a future kind
    /// written by a newer build must not load as a `NodeType`).
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "node_type" => Some(FavoriteKind::NodeType),
            "project" => Some(FavoriteKind::Project),
            _ => None,
        }
    }
}

/// One favorites-row entry. Identity is `(kind, stable_key)`;
/// `display_name` is presentation-only (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Favorite {
    pub kind: FavoriteKind,
    pub stable_key: String,
    pub display_name: String,
}

impl Favorite {
    /// Favorite for a node type. `display_name` is the fallback label;
    /// pass the registry's current one.
    pub fn node_type(type_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            kind: FavoriteKind::NodeType,
            stable_key: type_id.into(),
            display_name: display_name.into(),
        }
    }

    /// Favorite for a project. The label defaults to the file stem,
    /// matching how the browser grid names project entries.
    pub fn project(uri: &StorageUri) -> Self {
        let display_name =
            crate::app_state_storage::uri_file_stem(uri).unwrap_or_else(|| uri.to_string());
        Self {
            kind: FavoriteKind::Project,
            stable_key: uri.to_string(),
            display_name,
        }
    }

    /// `true` when this entry has the given identity.
    pub fn matches(&self, kind: FavoriteKind, stable_key: &str) -> bool {
        self.kind == kind && self.stable_key == stable_key
    }

    /// Resolve against the live authorities so the rail can decide
    /// between painting the entry live or greyed out. Never mutates
    /// and never prunes.
    pub fn resolve<'r>(&self, registry: &'r NodeRegistry) -> FavoriteResolution<'r> {
        match self.kind {
            FavoriteKind::NodeType => match registry.get(&self.stable_key) {
                Some(def) => FavoriteResolution::NodeType {
                    def,
                    display_name: def.display_name().to_string(),
                },
                None => FavoriteResolution::Dead,
            },
            FavoriteKind::Project => match StorageUri::from_str(&self.stable_key) {
                Ok(uri) => {
                    let display_name = crate::app_state_storage::uri_file_stem(&uri)
                        .unwrap_or_else(|| self.display_name.clone());
                    FavoriteResolution::Project { uri, display_name }
                }
                Err(_) => FavoriteResolution::Dead,
            },
        }
    }

    /// Label to paint: the resolved (live) name when the entry
    /// resolves, else the stored fallback.
    pub fn effective_display_name(&self, registry: &NodeRegistry) -> String {
        match self.resolve(registry) {
            FavoriteResolution::NodeType { display_name, .. }
            | FavoriteResolution::Project { display_name, .. } => display_name,
            FavoriteResolution::Dead => self.display_name.clone(),
        }
    }

    /// Render to the single-line settings-file field
    /// `kind|stable_key|display_name`, with both payloads escaped by
    /// [`escape_field`]. The kind token is a fixed ASCII word and
    /// needs no escaping.
    ///
    /// Escaping is not cosmetic: a [`StorageUri`] may legitimately
    /// contain `|` (the URI type validates the scheme and rejects
    /// traversal — it does not restrict file-name characters), and a
    /// raw newline in a name would otherwise write a second physical
    /// line into the settings file, i.e. let a file name inject
    /// arbitrary `key=value` settings.
    pub fn to_field(&self) -> String {
        format!(
            "{}|{}|{}",
            self.kind.key(),
            escape_field(&self.stable_key),
            escape_field(&self.display_name)
        )
    }

    /// Inverse of [`Favorite::to_field`]. Returns `None` for an
    /// unknown kind, an empty key, a malformed escape, or a missing
    /// separator — the caller drops the entry rather than loading a
    /// half-formed (or, worse, silently truncated) favorite.
    pub fn from_field(field: &str) -> Option<Self> {
        // Splitting honours escapes, so a `\|` inside a key or name
        // is payload rather than a separator.
        let (kind, rest) = split_escaped_once(field)?;
        let kind = FavoriteKind::from_key(kind.trim())?;
        let (stable_key, display_name) = match split_escaped_once(rest) {
            Some((key, name)) => (key, name),
            // A field with no display name is accepted; the resolver
            // supplies a label.
            None => (rest, ""),
        };
        let stable_key = unescape_field(stable_key)?;
        if stable_key.is_empty() {
            return None;
        }
        let display_name = unescape_field(display_name)?;
        Some(Self {
            kind,
            stable_key,
            display_name,
        })
    }
}

/// Escape one payload of the `kind|stable_key|display_name` field
/// encoding so it survives a round trip through the line-oriented
/// settings file.
///
/// Backslash escapes, all of them two ASCII characters:
///
/// | char | escape | why |
/// |---|---|---|
/// | `\` | `\\` | the escape character itself |
/// | `|` | `\|` | the field separator |
/// | LF  | `\n` | ends the settings line (injection) |
/// | CR  | `\r` | ends the settings line on CRLF readers |
/// | TAB | `\t` | stripped by the settings parser's `trim` |
/// | ` ` | `\s` | ditto, at either end of the value |
///
/// Space and tab are escaped everywhere rather than only at the
/// edges: `UiSettings::from_text` trims each value before dispatch,
/// so an unescaped edge space would silently rewrite a key into a
/// *different*, still-valid one. Escaping uniformly keeps the encoder
/// stateless and the decoder exact.
pub fn escape_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ' ' => out.push_str("\\s"),
            _ => out.push(ch),
        }
    }
    out
}

/// Inverse of [`escape_field`]. `None` for a malformed escape (an
/// unknown escape letter or a trailing lone backslash) so the caller
/// drops the whole entry instead of guessing at the payload.
pub fn unescape_field(field: &str) -> Option<String> {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            '|' => out.push('|'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            's' => out.push(' '),
            _ => return None,
        }
    }
    Some(out)
}

/// Split at the first **unescaped** `|`. Returns `None` when the
/// field holds no separator at all.
fn split_escaped_once(field: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (i, ch) in field.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => return Some((&field[..i], &field[i + 1..])),
            _ => {}
        }
    }
    None
}

/// Outcome of resolving a [`Favorite`] against the live registry /
/// URI parser. `Dead` entries stay in settings; the rail greys them.
pub enum FavoriteResolution<'r> {
    NodeType {
        def: &'r Arc<dyn NodeDef>,
        display_name: String,
    },
    Project {
        uri: StorageUri,
        display_name: String,
    },
    /// The node type is not registered, or the stored URI no longer
    /// parses. Not an error and not a reason to forget the entry.
    Dead,
}

impl FavoriteResolution<'_> {
    pub fn is_alive(&self) -> bool {
        !matches!(self, FavoriteResolution::Dead)
    }
}

/// Hand-written because `dyn NodeDef` is not `Debug`; the type id is
/// the only part of the def worth printing anyway.
impl std::fmt::Debug for FavoriteResolution<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FavoriteResolution::NodeType { def, display_name } => f
                .debug_struct("NodeType")
                .field("type_id", &def.type_id())
                .field("display_name", display_name)
                .finish(),
            FavoriteResolution::Project { uri, display_name } => f
                .debug_struct("Project")
                .field("uri", &uri.to_string())
                .field("display_name", display_name)
                .finish(),
            FavoriteResolution::Dead => f.write_str("Dead"),
        }
    }
}

/// The ordered favorites list plus the seed flag, persisted verbatim
/// in [`UiSettings`](crate::settings::UiSettings).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Favorites {
    items: Vec<Favorite>,
    seeded: bool,
}

/// Upper bound on persisted favorites, so a hand-edited settings file
/// can't grow the rail unboundedly. Mirrors
/// [`MAX_RECENT_PROJECTS`](crate::settings::MAX_RECENT_PROJECTS)'s intent.
pub const MAX_FAVORITES: usize = 64;

impl Favorites {
    /// Build from already-persisted parts. Used by the settings
    /// parser; application code adds through [`Favorites::add`].
    pub fn from_parts(items: Vec<Favorite>, seeded: bool) -> Self {
        let mut out = Self {
            items: Vec::new(),
            seeded,
        };
        for item in items {
            out.add(item);
        }
        out
    }

    /// The row, in display order.
    pub fn list(&self) -> &[Favorite] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// `true` once the defaults have been seeded (or the flag was
    /// restored from settings). An empty list with `seeded == true`
    /// is a row the user deliberately cleared.
    pub fn seeded(&self) -> bool {
        self.seeded
    }

    pub fn contains(&self, kind: FavoriteKind, stable_key: &str) -> bool {
        self.items.iter().any(|f| f.matches(kind, stable_key))
    }

    pub fn position(&self, kind: FavoriteKind, stable_key: &str) -> Option<usize> {
        self.items.iter().position(|f| f.matches(kind, stable_key))
    }

    /// Append a favorite. Deduped by `(kind, stable_key)`: adding an
    /// entry that is already pinned is a no-op that returns `false`
    /// (it does *not* move the existing entry — the user's ordering
    /// is theirs). Also a no-op once [`MAX_FAVORITES`] is reached.
    pub fn add(&mut self, favorite: Favorite) -> bool {
        if favorite.stable_key.is_empty()
            || self.contains(favorite.kind, &favorite.stable_key)
            || self.items.len() >= MAX_FAVORITES
        {
            return false;
        }
        self.items.push(favorite);
        true
    }

    /// Unpin by identity. Returns `false` when nothing matched.
    pub fn remove(&mut self, kind: FavoriteKind, stable_key: &str) -> bool {
        match self.position(kind, stable_key) {
            Some(i) => {
                self.items.remove(i);
                true
            }
            None => false,
        }
    }

    /// Move the entry at `from` so it lands at index `to` (drag-
    /// reorder semantics: remove then insert). Out-of-range indices
    /// and no-op moves return `false`.
    pub fn move_favorite(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return false;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        true
    }

    /// Remove every favorite, leaving the seed flag alone — the row
    /// stays empty across restarts (design §2: "user emptied" ≠
    /// "never seeded").
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Seed the primitive palette exactly once in the lifetime of a
    /// settings file. Returns `true` when this call did the seeding.
    ///
    /// `display_name`s come from the live registry when the type is
    /// there; a seed id missing from the registry is skipped entirely
    /// (we never pin an entry that can't resolve on the very run that
    /// created it). The flag is set either way, so the seeding never
    /// runs twice.
    pub fn seed_defaults_once(&mut self, registry: &NodeRegistry) -> bool {
        if self.seeded {
            return false;
        }
        self.seeded = true;
        for type_id in SEED_NODE_TYPES {
            if let Some(def) = registry.get(type_id) {
                self.add(Favorite::node_type(*type_id, def.display_name()));
            }
        }
        true
    }

    /// Mark as seeded without adding anything — for shells/tests that
    /// want to opt a settings file out of seeding.
    pub fn mark_seeded(&mut self) {
        self.seeded = true;
    }
}

// Tests live in `favorites_tests.rs` so this file stays under the
// 800-line cap enforced by `atomartist-lib::tests::file_line_count`.
#[cfg(test)]
#[path = "favorites_tests.rs"]
mod tests;
