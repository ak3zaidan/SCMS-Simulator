//! The coverage ledger: every §10 item has an owner, and every owner still exists.
//!
//! This is the file that makes the rest of the suite mean something. Without it, "the kit
//! mechanises the checklist" is a claim about a directory; with it, a renamed test, a
//! deleted test, a new checklist item or a silently widened gap is a failure that names
//! itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use v2xw_conformance::checklist::{
    COVERAGE, Coverage, GAP_COUNT, ITEM_COUNT, ITEMS, Side, UNMET_COUNT, parse, spec_path,
};

/// The specification text.
fn spec() -> String {
    let path = spec_path();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// This suite's own source files, by name.
fn suite_sources() -> BTreeMap<String, String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vwp");
    let mut out = BTreeMap::new();
    for entry in
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
    {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            let name = path
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            out.insert(
                name,
                std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("reading {}: {e}", path.display())),
            );
        }
    }
    assert!(out.len() >= 8, "the suite has only {} files", out.len());
    out
}

/// **The checklist this kit believes in is the checklist the specification states.**
///
/// Ids, sides and order, all three. A specification edit that adds an item, renames one or
/// moves one between sections fails here and names the difference, which is the only way a
/// kit can stay honest about a document it does not own.
#[test]
fn the_ledger_matches_section_ten_of_the_specification() {
    let parsed = parse(&spec());
    assert_eq!(
        parsed.len(),
        ITEM_COUNT,
        "§10 states {} items and this build expects {ITEM_COUNT}; parsed ids {:?}",
        parsed.len(),
        parsed.iter().map(|i| i.id.as_str()).collect::<Vec<_>>()
    );

    let stated: Vec<(String, Side)> = parsed.iter().map(|i| (i.id.clone(), i.side)).collect();
    let expected: Vec<(String, Side)> = ITEMS
        .iter()
        .map(|(id, side)| ((*id).to_string(), *side))
        .collect();
    assert_eq!(
        stated, expected,
        "the specification and this build's item list differ"
    );

    // Each item's text survived the parse, so the ledger is keyed to real clauses rather
    // than to empty rows.
    for item in &parsed {
        assert!(
            item.text.len() > 10,
            "item {} parsed with almost no text: {:?}",
            item.id,
            item.text
        );
        assert!(
            item.section.starts_with("10."),
            "item {} landed under section {:?}",
            item.id,
            item.section
        );
    }
}

/// **Every item has exactly one owner.**
#[test]
fn every_item_has_exactly_one_owner() {
    assert_eq!(COVERAGE.len(), ITEM_COUNT);
    for (id, _) in ITEMS {
        let rows: Vec<&&str> = COVERAGE
            .iter()
            .filter(|e| e.id == *id)
            .map(|e| &e.id)
            .collect();
        assert_eq!(rows.len(), 1, "{id} has {} owners", rows.len());
    }
    for entry in COVERAGE {
        assert!(
            ITEMS.iter().any(|(id, _)| *id == entry.id),
            "the ledger names `{}`, which §10 does not",
            entry.id
        );
    }
}

/// **Every owner exists.**
///
/// A test named in the ledger is opened and looked for. This is the assertion that stops
/// the ledger decaying into a list of aspirations: rename
/// `a_slot_is_not_reused_until_a_keyframe_period_after_its_despawn` and Q5 becomes an
/// orphan here rather than silently uncovered.
#[test]
fn every_owner_named_by_the_ledger_still_exists() {
    let suite = suite_sources();
    let root = v2xw_conformance::repo_root();
    let mut checked_here = 0usize;
    let mut checked_there = 0usize;

    for entry in COVERAGE {
        match entry.coverage {
            Coverage::Here { test } => {
                let needle = format!("fn {test}(");
                assert!(
                    suite.values().any(|text| text.contains(&needle)),
                    "{}: this kit claims `{test}`, which no file in tests/vwp/ defines",
                    entry.id
                );
                checked_here += 1;
            }
            Coverage::Delegated { path, test } => {
                let file = root.join(path);
                assert!(
                    file.is_file(),
                    "{}: the ledger points at {}, which does not exist",
                    entry.id,
                    file.display()
                );
                let text = std::fs::read_to_string(&file)
                    .unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
                let found = if is_rust(&file) {
                    text.contains(&format!("fn {test}("))
                } else {
                    // vitest names tests with strings, so the item id is what a delegated
                    // TypeScript test is searched for.
                    text.contains(test)
                };
                assert!(
                    found,
                    "{}: {} no longer contains `{test}`",
                    entry.id,
                    file.display()
                );
                checked_there += 1;
            }
            Coverage::Gap { reason } => {
                assert!(
                    reason.len() > 20,
                    "{}: a gap must say what would have to exist, not `{reason}`",
                    entry.id
                );
            }
        }
    }

    assert!(
        checked_here > 20 && checked_there > 20,
        "the ledger resolved {checked_here} local and {checked_there} delegated owners, \
         which is too few for the check to have done anything"
    );
}

/// **The gaps are the ones we know about, and there are no more of them.**
///
/// The number is pinned rather than merely reported. An item quietly moved from a test to a
/// gap is how a suite shrinks without anyone deciding to shrink it.
#[test]
fn the_gaps_and_the_unmet_items_are_the_ones_we_know_about() {
    let gaps: Vec<&str> = COVERAGE
        .iter()
        .filter(|e| matches!(e.coverage, Coverage::Gap { .. }))
        .map(|e| e.id)
        .collect();
    assert_eq!(
        gaps.len(),
        GAP_COUNT,
        "the gap set changed: {gaps:?} (pinned at {GAP_COUNT})"
    );

    let unmet: Vec<&str> = COVERAGE.iter().filter(|e| e.unmet).map(|e| e.id).collect();
    assert_eq!(
        unmet.len(),
        UNMET_COUNT,
        "the set of items this build does not satisfy changed: {unmet:?}"
    );
    assert_eq!(
        unmet,
        vec!["F8"],
        "F8 is the one item this build does not satisfy: compression is not implemented"
    );

    // Every gap is on the server or the client side of a live connection, a benchmark, or
    // a browser. None of them is a property of a pure function that somebody could have
    // tested and did not.
    for id in &gaps {
        assert!(
            [
                "H1", "H8", "H11", "Q7", "C6", "W2", "W6", "R4", "R5", "R6", "R8", "P2"
            ]
            .contains(id),
            "`{id}` became a gap without being written down as one"
        );
    }
}

fn is_rust(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "rs")
}
