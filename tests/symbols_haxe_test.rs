//! Integration tests for the Haxe symbol indexing adapter.
//!
//! Unlike the C# adapter, there is no offline pipeline to test without a
//! live helper — the Haxe adapter has no batch index (see
//! `src/symbols/haxe.rs` module docs), so every meaningful test here needs
//! a real `haxe` compiler on PATH. All tests are gated behind the
//! `haxe_helper_integration` cargo feature.

use std::path::PathBuf;

use codesearch::symbols::haxe::HaxeSymbolIndexer;
use codesearch::symbols::{RebuildScope, SymbolIndexer};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/haxe/SmallProject")
}

/// Full pipeline integration test: `haxe --display @usage` subprocess →
/// parsed references, for both the position-based and name-based lookup
/// paths.
///
/// Requires the `haxe_helper_integration` feature flag AND a `haxe`
/// compiler on `$PATH` (or `CODESEARCH_HAXE` pointing to one).
#[test]
#[cfg_attr(not(feature = "haxe_helper_integration"), ignore)]
fn test_haxe_pipeline_smallproject_roundtrip() {
    let fixture_root = fixture_root();
    assert!(
        fixture_root.join("build.hxml").exists(),
        "Fixture not found at {}",
        fixture_root.display()
    );

    let indexer = HaxeSymbolIndexer::new();
    assert!(
        indexer.is_available(),
        "haxe compiler not found on PATH — install the Haxe SDK to run this test"
    );
    assert!(indexer.applies_to(&fixture_root));

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path();

    let summary = indexer
        .rebuild(&fixture_root, db_path, RebuildScope::Full)
        .expect("rebuild failed");
    // No batch index — rebuild just verifies the helper + .hxml are present.
    assert_eq!(summary.symbols_indexed, 0);
    assert!(indexer.has_index(db_path));
    assert!(indexer.index_age(db_path) < 60);

    // Position-based: Helper.hx line 2 is `function validate(...)`.
    let refs_by_position = indexer
        .find_references_by_position(db_path, &PathBuf::from("src/Helper.hx"), 2)
        .expect("find_references_by_position failed");
    assert_eq!(
        refs_by_position.len(),
        2,
        "expected 2 call sites of Helper.validate, got {:?}",
        refs_by_position
    );
    for r in &refs_by_position {
        assert_eq!(r.file, PathBuf::from("src/Main.hx"));
        assert_eq!(r.kind, "reference");
    }
    let mut lines: Vec<u32> = refs_by_position.iter().map(|r| r.start_line).collect();
    lines.sort();
    assert_eq!(lines, vec![3, 4]);

    // Name-based: same result via the tree-sitter name-scan fallback.
    let refs_by_name = indexer
        .find_references(db_path, "Helper.validate")
        .expect("find_references failed");
    assert_eq!(refs_by_name.len(), 2);

    // A line with no declaration returns empty, not an error.
    let empty = indexer
        .find_references_by_position(db_path, &PathBuf::from("src/Main.hx"), 3)
        .expect("should not error on a non-declaration line");
    assert!(empty.is_empty());

    // An unknown symbol name returns empty, not an error.
    let unknown = indexer
        .find_references(db_path, "NoSuchSymbolAnywhere")
        .expect("should not error on an unresolvable name");
    assert!(unknown.is_empty());
}
