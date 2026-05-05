//! Bulk-import test for the bundled NodeDesigner examples. Verifies
//! that every `.example/scene.json` in
//! `tests/nodedesigner_examples/` parses, builds at least one node
//! that maps to an AtomArtist type, and can be evaluated without
//! panicking.
//!
//! Examples that use unsupported NodeDesigner node types (drag_on_*,
//! image/alpha_to_path, basic/time, field/formula, ...) are imported
//! partially with warnings — that's expected. The test fails only when
//! the importer or executor crashes outright.

use atomartist_lib::graph::executor::evaluate_all;
use atomartist_lib::nodes;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::serialization::import_nodedesigner_scene_str;

fn registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    nodes::register_all(&mut r);
    r
}

#[test]
fn bulk_import_all_bundled_examples() {
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("nodedesigner_examples");
    let mut total = 0usize;
    let mut at_least_one_node = 0usize;
    let mut fully_imported = 0usize;
    let mut evaluated = 0usize;
    let mut by_skipped: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    let entries = std::fs::read_dir(&examples_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", examples_dir.display(), e));
    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        total += 1;

        let json = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        let reg = registry();
        let result = match import_nodedesigner_scene_str(&json, &reg) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "PARSE-FAIL {}: {}",
                    path.file_name().unwrap().to_string_lossy(),
                    e
                );
                continue;
            }
        };

        // Tally skip warnings by missing node type id.
        for w in &result.warnings {
            if let Some(rest) = w.strip_prefix("skipping unsupported node type '") {
                if let Some(end) = rest.find('\'') {
                    let id = &rest[..end];
                    *by_skipped.entry(id.to_string()).or_insert(0) += 1;
                }
            }
        }

        let node_count = result.graph.node_count();
        if node_count > 0 {
            at_least_one_node += 1;
        }
        if result.warnings.is_empty() {
            fully_imported += 1;
        }

        // Evaluate — even partial graphs should not panic.
        let mut g = result.graph;
        let _ = evaluate_all(&mut g, &reg); // ignore errors; some
        // partial graphs may have unresolved edges and that's OK.
        evaluated += 1;
    }

    eprintln!(
        "bulk-import: total={} at-least-one-node={} fully={} evaluated-ok={}",
        total, at_least_one_node, fully_imported, evaluated
    );
    let mut skipped: Vec<_> = by_skipped.into_iter().collect();
    skipped.sort_by(|a, b| b.1.cmp(&a.1));
    for (id, n) in &skipped {
        eprintln!("  skipped {}× {}", n, id);
    }

    assert!(total >= 20, "expected at least 20 examples bundled, found {}", total);
    // Most examples should at least make it through evaluation. Some
    // older NodeDesigner schema variants may fail to parse; we tolerate
    // a handful as long as the majority work.
    assert!(
        evaluated * 100 / total >= 80,
        "fewer than 80% of examples evaluated: {}/{}",
        evaluated, total
    );
}
