//! Unit tests for [`crate::file_browser::favorites`].
//!
//! Split out of `favorites.rs` to keep both files well under the
//! 800-line cap enforced by `atomartist-lib::tests::file_line_count`.
//! Re-attached via `#[path]`, so `use super::*` still sees the
//! module's private items.
//!
//! The seed-once tests are the load-bearing ones: they pin the design
//! §2 rule that a user who empties the row keeps it empty across
//! restarts, which only works if the flag survives serialization.

use super::*;

use crate::settings::UiSettings;

/// A registry with every shipped node type — the same one the app
/// builds at startup, so the seed ids are checked against production
/// registration rather than a fixture.
fn registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut reg);
    reg
}

fn uri(text: &str) -> StorageUri {
    StorageUri::from_str(text).expect("valid storage URI")
}

#[test]
fn fresh_favorites_seed_the_primitive_palette() {
    let reg = registry();
    let mut favs = Favorites::default();
    assert!(!favs.seeded());
    assert!(favs.is_empty());

    assert!(favs.seed_defaults_once(&reg));
    assert!(favs.seeded());
    assert_eq!(favs.len(), SEED_NODE_TYPES.len());
    let keys: Vec<&str> = favs.list().iter().map(|f| f.stable_key.as_str()).collect();
    assert_eq!(keys, SEED_NODE_TYPES);
    assert!(favs.list().iter().all(|f| f.kind == FavoriteKind::NodeType));
}

/// Every id in the seed list must actually be registered — otherwise
/// the seeding silently ships a shorter row than intended.
#[test]
fn every_seed_id_is_registered() {
    let reg = registry();
    for id in SEED_NODE_TYPES {
        assert!(
            reg.get(id).is_some(),
            "seed node type '{id}' not registered"
        );
    }
}

#[test]
fn seeding_is_a_no_op_once_seeded() {
    let reg = registry();
    let mut favs = Favorites::default();
    assert!(favs.seed_defaults_once(&reg));
    let after_first = favs.list().to_vec();

    assert!(!favs.seed_defaults_once(&reg));
    assert_eq!(favs.list(), after_first.as_slice());
}

#[test]
fn user_emptied_row_stays_empty_across_reload() {
    let reg = registry();
    let mut settings = UiSettings::default();
    settings.favorites.seed_defaults_once(&reg);
    settings.favorites.clear();
    assert!(settings.favorites.is_empty());
    assert!(settings.favorites.seeded());

    // Round-trip through the same text path the shells persist with.
    let mut reloaded = UiSettings::from_text(&settings.to_text());
    assert!(reloaded.favorites.is_empty());
    assert!(reloaded.favorites.seeded());
    // And a startup seed attempt must not refill it.
    assert!(!reloaded.favorites.seed_defaults_once(&reg));
    assert!(reloaded.favorites.is_empty());
}

#[test]
fn add_dedupes_by_kind_and_key() {
    let mut favs = Favorites::default();
    assert!(favs.add(Favorite::node_type("Box", "Box")));
    // Same key, different display name → still a duplicate.
    assert!(!favs.add(Favorite::node_type("Box", "Cube")));
    assert_eq!(favs.len(), 1);
    assert_eq!(favs.list()[0].display_name, "Box");

    // Same *string* key under a different kind is a different entry.
    assert!(favs.add(Favorite {
        kind: FavoriteKind::Project,
        stable_key: "Box".into(),
        display_name: "Box".into(),
    }));
    assert_eq!(favs.len(), 2);
    assert!(!favs.add(Favorite::node_type("", "empty")));
}

#[test]
fn add_stops_at_the_cap() {
    let mut favs = Favorites::default();
    for i in 0..MAX_FAVORITES {
        assert!(favs.add(Favorite::node_type(format!("T{i}"), "t")));
    }
    assert!(!favs.add(Favorite::node_type("overflow", "o")));
    assert_eq!(favs.len(), MAX_FAVORITES);
}

#[test]
fn remove_reports_whether_anything_matched() {
    let mut favs = Favorites::default();
    favs.add(Favorite::node_type("Box", "Box"));
    favs.add(Favorite::node_type("Sphere", "Sphere"));

    assert!(favs.remove(FavoriteKind::NodeType, "Box"));
    assert!(!favs.contains(FavoriteKind::NodeType, "Box"));
    assert_eq!(favs.len(), 1);
    // Already gone, and the wrong kind for the survivor.
    assert!(!favs.remove(FavoriteKind::NodeType, "Box"));
    assert!(!favs.remove(FavoriteKind::Project, "Sphere"));
    assert_eq!(favs.len(), 1);
}

#[test]
fn move_favorite_reorders_and_rejects_out_of_bounds() {
    let mut favs = Favorites::default();
    for id in ["A", "B", "C"] {
        favs.add(Favorite::node_type(id, id));
    }

    // Drag the last entry to the front.
    assert!(favs.move_favorite(2, 0));
    let keys: Vec<&str> = favs.list().iter().map(|f| f.stable_key.as_str()).collect();
    assert_eq!(keys, ["C", "A", "B"]);

    // Middle to the end.
    assert!(favs.move_favorite(1, 2));
    let keys: Vec<&str> = favs.list().iter().map(|f| f.stable_key.as_str()).collect();
    assert_eq!(keys, ["C", "B", "A"]);

    // No-ops and out-of-range indices leave the row untouched.
    assert!(!favs.move_favorite(1, 1));
    assert!(!favs.move_favorite(3, 0));
    assert!(!favs.move_favorite(0, 3));
    let keys: Vec<&str> = favs.list().iter().map(|f| f.stable_key.as_str()).collect();
    assert_eq!(keys, ["C", "B", "A"]);
}

#[test]
fn project_favorite_names_itself_after_the_file_stem() {
    let fav = Favorite::project(&uri("file:///C:/users/bob/projects/widget.atmr"));
    assert_eq!(fav.kind, FavoriteKind::Project);
    assert_eq!(fav.display_name, "widget");
    assert_eq!(
        fav.stable_key,
        "file:///C:/users/bob/projects/widget.atmr".to_string()
    );
}

#[test]
fn favorites_round_trip_through_settings_text() {
    let reg = registry();
    let mut settings = UiSettings::default();
    settings.favorites.seed_defaults_once(&reg);
    settings
        .favorites
        .add(Favorite::project(&uri("mem:///projects/pinned.atmr")));

    let reloaded = UiSettings::from_text(&settings.to_text());
    assert_eq!(reloaded.favorites, settings.favorites);
    assert_eq!(reloaded, settings);
}

/// Display names may contain the field separator (a project stem can
/// be anything the user typed).
#[test]
fn display_name_with_separator_survives_the_field_encoding() {
    let fav = Favorite::node_type("Box", "Box | Cube");
    let parsed = Favorite::from_field(&fav.to_field()).expect("parses");
    assert_eq!(parsed, fav);
}

/// Regression: the *key* can contain the separator too. `StorageUri`
/// only validates the scheme and rejects traversal, so a provider
/// path may legitimately hold a `|` — and a truncated key still
/// parses as a valid URI, which would resolve Alive while pointing at
/// a **different** project.
#[test]
fn stable_key_with_separator_round_trips_to_the_same_uri() {
    let original = uri("mem:///projects/a|b.atmr");
    let fav = Favorite::project(&original);
    assert_eq!(fav.stable_key, original.to_string());

    let parsed = Favorite::from_field(&fav.to_field()).expect("parses");
    assert_eq!(parsed, fav);
    match parsed.resolve(&registry()) {
        FavoriteResolution::Project { uri, .. } => assert_eq!(uri, original),
        other => panic!("expected the same project, got {other:?}"),
    }
}

/// Regression: a newline in a name or key must not write a second
/// physical line into the settings blob — a file named
/// `evil\nfavorites_seeded=true` would otherwise inject settings.
#[test]
fn newlines_in_a_favorite_cannot_inject_settings_lines() {
    let mut settings = UiSettings::default();
    // Not seeded, and the injected line tries to flip that.
    assert!(!settings.favorites.seeded());
    settings.favorites.add(Favorite::node_type(
        "Box",
        "evil\nfavorites_seeded=true\r\nperspective=false",
    ));

    let text = settings.to_text();
    assert_eq!(
        text.lines().filter(|l| l.starts_with("favorite_")).count(),
        1,
        "the favorite must occupy exactly one physical line: {text}"
    );

    let reloaded = UiSettings::from_text(&text);
    assert_eq!(reloaded.favorites.len(), 1);
    assert_eq!(reloaded.favorites, settings.favorites);
    assert!(!reloaded.favorites.seeded(), "injected a settings line");
    assert!(reloaded.perspective, "injected a settings line");
}

/// Regression: leading / trailing whitespace is legal in OPFS and
/// local file names, so trimming the encoded parts silently rewrites
/// the key to a different one.
#[test]
fn surrounding_whitespace_in_keys_and_names_survives() {
    let original = uri("mem:///projects/ padded .atmr");
    let fav = Favorite {
        kind: FavoriteKind::Project,
        stable_key: original.to_string(),
        display_name: " padded ".to_string(),
    };
    let parsed = Favorite::from_field(&fav.to_field()).expect("parses");
    assert_eq!(parsed, fav);

    let mut settings = UiSettings::default();
    settings.favorites.add(fav.clone());
    let reloaded = UiSettings::from_text(&settings.to_text());
    assert_eq!(reloaded.favorites.list(), &[fav]);
}

/// A hand-edited file can't grow the row past the cap either.
#[test]
fn parsed_favorites_are_truncated_to_the_cap() {
    let mut text = String::from("favorites_seeded=true\n");
    for i in 0..(MAX_FAVORITES + 20) {
        text.push_str(&format!("favorite_{i}=node_type|T{i}|T{i}\n"));
    }
    let parsed = UiSettings::from_text(&text);
    assert_eq!(parsed.favorites.len(), MAX_FAVORITES);
    // The cap keeps the *first* entries, in index order.
    assert_eq!(parsed.favorites.list()[0].stable_key, "T0");
    assert_eq!(
        parsed.favorites.list()[MAX_FAVORITES - 1].stable_key,
        format!("T{}", MAX_FAVORITES - 1)
    );
}

#[test]
fn malformed_favorite_fields_are_dropped() {
    assert!(Favorite::from_field("").is_none());
    assert!(Favorite::from_field("node_type").is_none());
    // Unknown kind (e.g. written by a newer build) is not coerced.
    assert!(Favorite::from_field("folder|mem:///a|A").is_none());
    // Empty stable key carries no identity.
    assert!(Favorite::from_field("node_type||A").is_none());
    // A missing display name is fine — the resolver supplies one.
    let parsed = Favorite::from_field("node_type|Box").expect("parses");
    assert_eq!(parsed.display_name, "");
    // Malformed escapes are ambiguous payload; drop the line rather
    // than guess (unknown escape letter, trailing lone backslash).
    assert!(Favorite::from_field("node_type|Bo\\x|Box").is_none());
    assert!(Favorite::from_field("node_type|Box|Box\\").is_none());
}

/// Forward tolerance: a settings blob written before favorites
/// existed must still parse, and must land "never seeded" so the next
/// launch fills the row.
#[test]
fn old_settings_without_favorites_still_parse() {
    let old = "\
# AtomArtist UI settings
perspective=false
turntable=true
show_bed=true
render_style=Shaded
snap_amount=1
theme=dark
";
    let parsed = UiSettings::from_text(old);
    assert!(!parsed.perspective);
    assert!(parsed.favorites.is_empty());
    assert!(!parsed.favorites.seeded());
}

/// A hand-edited file with duplicate / shuffled indices still loads a
/// deduped, index-ordered row.
#[test]
fn hand_edited_favorites_are_ordered_and_deduped() {
    let text = "\
favorites_seeded=true
favorite_2=node_type|Sphere|Sphere
favorite_0=node_type|Box|Box
favorite_1=node_type|Box|Box duplicate
favorite_3=bogus-line
";
    let parsed = UiSettings::from_text(text);
    assert!(parsed.favorites.seeded());
    let keys: Vec<&str> = parsed
        .favorites
        .list()
        .iter()
        .map(|f| f.stable_key.as_str())
        .collect();
    assert_eq!(keys, ["Box", "Sphere"]);
}

#[test]
fn node_type_favorites_resolve_against_the_real_registry() {
    let reg = registry();
    let live = Favorite::node_type("Box", "stale label");
    match live.resolve(&reg) {
        FavoriteResolution::NodeType { def, display_name } => {
            assert_eq!(def.type_id(), "Box");
            // Registry wins over the stored fallback.
            assert_eq!(display_name, "Box");
        }
        other => panic!("expected a live node type, got {other:?}"),
    }
    assert_eq!(live.effective_display_name(&reg), "Box");

    let dead = Favorite::node_type("NoSuchNodeType", "Ghost");
    assert!(!dead.resolve(&reg).is_alive());
    // Dead entries fall back to the stored label and are *not* pruned.
    assert_eq!(dead.effective_display_name(&reg), "Ghost");
}

#[test]
fn project_favorites_resolve_by_parsing_their_uri() {
    let reg = registry();
    let live = Favorite::project(&uri("mem:///projects/pinned.atmr"));
    match live.resolve(&reg) {
        FavoriteResolution::Project { uri, display_name } => {
            assert_eq!(uri.scheme(), "mem");
            assert_eq!(display_name, "pinned");
        }
        other => panic!("expected a project, got {other:?}"),
    }

    let broken = Favorite {
        kind: FavoriteKind::Project,
        stable_key: "not a uri".into(),
        display_name: "Pinned".into(),
    };
    assert!(!broken.resolve(&reg).is_alive());
    assert_eq!(broken.effective_display_name(&reg), "Pinned");
}

/// Dead entries survive a save/load cycle — a provider that is
/// offline today may be back tomorrow (design §7.3).
#[test]
fn dead_favorites_are_not_pruned_on_reload() {
    let reg = registry();
    let mut settings = UiSettings::default();
    settings.favorites.mark_seeded();
    settings
        .favorites
        .add(Favorite::node_type("NoSuchNodeType", "Ghost"));

    let reloaded = UiSettings::from_text(&settings.to_text());
    assert_eq!(reloaded.favorites.len(), 1);
    let fav = &reloaded.favorites.list()[0];
    assert!(!fav.resolve(&reg).is_alive());
    assert_eq!(fav.display_name, "Ghost");
}
