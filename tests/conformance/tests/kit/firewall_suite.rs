//! The interface firewall, over the whole workspace — and the blind spot it closes.
//!
//! `crates/v2xw-node/tests/firewall_sentinel.rs` scans with one `std::fs::read_dir` of
//! `src/`, which is one directory deep. The first test below builds a tree with a violation
//! in a subdirectory and shows the flat read walking straight past it while the recursive
//! walk finds it. Everything after that runs the general version over the real crates.

use std::path::{Path, PathBuf};

use v2xw_conformance::firewall::{
    CHECKS, GROUND_TRUTH_RULES, HASH_ORDER_RULES, Rule, TRANSCENDENTAL_RULES, WALL_CLOCK_RULES,
    files_for, flat_rust_sources, report, run_check, scan, scan_ground_truth_fields,
    walk_rust_sources,
};
use v2xw_conformance::{MIN_CRATES, crates, repo_root};

/// A probe rule, so the blind-spot demonstration does not depend on any real rule's needle.
const PROBE: &[Rule] = &[Rule {
    name: "probe",
    needle: "FORBIDDEN_BY_THE_PROBE",
    because: "a synthetic needle used only to demonstrate the scan's reach",
}];

/// A throwaway source tree: one file at the top, one inside a subdirectory, both offending.
fn build_probe_tree(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("v2xw-conformance-{}-{tag}", std::process::id()));
    let nested = root.join("wire").join("inner");
    std::fs::create_dir_all(&nested).expect("a scratch tree");
    std::fs::write(
        root.join("top.rs"),
        "fn top() { let _ = FORBIDDEN_BY_THE_PROBE; }\n",
    )
    .expect("writes");
    std::fs::write(
        root.join("wire").join("mid.rs"),
        "fn mid() { let _ = FORBIDDEN_BY_THE_PROBE; }\n",
    )
    .expect("writes");
    std::fs::write(
        nested.join("deep.rs"),
        "fn deep() { let _ = FORBIDDEN_BY_THE_PROBE; }\n",
    )
    .expect("writes");
    // Not a Rust file: neither scan may read it.
    std::fs::write(root.join("notes.md"), "FORBIDDEN_BY_THE_PROBE\n").expect("writes");
    root
}

/// **The blind spot, demonstrated rather than asserted from memory.**
///
/// The flat read finds one of the three offending files. The recursive walk finds all
/// three. This is the hole in the node sentinel: nine crates already have a `src/`
/// subdirectory, so a rule that moved into one would stop being enforced while the
/// sentinel went on reporting success.
#[test]
fn the_recursive_walk_finds_what_a_flat_read_dir_misses() {
    let root = build_probe_tree("blindspot");

    let flat = flat_rust_sources(&root, &root).expect("the flat read succeeds");
    let deep = walk_rust_sources(&root, &root).expect("the recursive walk succeeds");

    let names = |files: &[v2xw_conformance::firewall::SourceFile]| {
        files.iter().map(|f| f.rel.clone()).collect::<Vec<_>>()
    };
    assert_eq!(
        names(&flat),
        vec!["top.rs".to_string()],
        "the flat read is one directory deep, by construction"
    );
    assert_eq!(
        names(&deep),
        vec![
            "top.rs".to_string(),
            "wire/inner/deep.rs".to_string(),
            "wire/mid.rs".to_string(),
        ],
        "the recursive walk must find every .rs file and sort them"
    );

    let count = |files: &[v2xw_conformance::firewall::SourceFile]| {
        files
            .iter()
            .map(|f| scan("probe", &f.rel, &f.text, PROBE).len())
            .sum::<usize>()
    };
    assert_eq!(count(&flat), 1, "the old scan sees one breach");
    assert_eq!(
        count(&deep),
        3,
        "the new scan sees all three; the other two are the ones that were invisible"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The kit's ground-truth rules are a superset of the sentinel's.
///
/// A rule added to `v2xw-node` and not here would mean the general scan was weaker than the
/// one it generalises, which is the opposite of the point.
#[test]
fn the_node_sentinels_rules_are_a_subset_of_the_kits() {
    let ours: Vec<&str> = GROUND_TRUTH_RULES.iter().map(|r| r.needle).collect();
    for rule in v2xw_node::firewall::RULES {
        assert!(
            ours.contains(&rule.needle),
            "v2xw-node enforces `{}` ({}), which this kit does not",
            rule.needle,
            rule.name
        );
    }
    assert!(!ours.is_empty());
}

/// **I-C2, over every file in the node crate, subdirectories included.**
#[test]
fn no_node_source_can_reach_ground_truth() {
    let check = CHECKS
        .iter()
        .find(|c| c.name == "ground-truth")
        .expect("the ground-truth check");
    let violations = run_check(check).expect("the scan reads the crate");
    assert!(
        violations.is_empty(),
        "the node runtime can reach ground truth:\n{}",
        report(&violations)
    );
}

/// **ADR 0004: no engine-facing code reads a wall clock.**
#[test]
fn no_engine_facing_code_reads_a_wall_clock() {
    let check = CHECKS
        .iter()
        .find(|c| c.name == "wall-clock")
        .expect("the wall-clock check");
    let violations = run_check(check).expect("the scan reads the crates");
    assert!(
        violations.is_empty(),
        "a wall clock is being read outside the two documented exemptions:\n{}",
        report(&violations)
    );
}

/// **02-architecture.md §6.4: no `std` hash container.**
#[test]
fn no_std_hash_container_reaches_an_ordering() {
    let check = CHECKS
        .iter()
        .find(|c| c.name == "hash-order")
        .expect("the hash-order check");
    let violations = run_check(check).expect("the scan reads the crates");
    assert!(
        violations.is_empty(),
        "a std hash container is in use outside the two Arrow metadata maps:\n{}",
        report(&violations)
    );
}

/// **ADR 0003: every transcendental goes through `v2xw_core::math`.**
#[test]
fn no_std_transcendental_is_called() {
    let check = CHECKS
        .iter()
        .find(|c| c.name == "transcendental")
        .expect("the transcendental check");
    let violations = run_check(check).expect("the scan reads the crates");
    assert!(
        violations.is_empty(),
        "the platform libm is being called, so a digest can differ between platforms:\n{}",
        report(&violations)
    );
}

/// A ground-truth field reaches the telemetry record and nothing else, in every file of the
/// node crate rather than in the ones a flat read happened to see.
#[test]
fn a_ground_truth_field_reaches_only_the_telemetry_record() {
    let root = repo_root();
    let files =
        walk_rust_sources(&root.join("crates/v2xw-node/src"), &root).expect("the node crate reads");
    let mut violations = Vec::new();
    for file in &files {
        if file.rel == "crates/v2xw-node/src/firewall.rs" {
            continue;
        }
        violations.extend(scan_ground_truth_fields(&file.rel, &file.text));
    }
    assert!(
        violations.is_empty(),
        "a ground-truth value is being used to decide something:\n{}",
        report(&violations)
    );
    assert!(
        files.iter().any(|f| f.text.contains("gt_")),
        "no file in the node crate holds a gt_-prefixed field, so the rule polices nothing"
    );
}

/// **The scans are actually reading the tree.**
///
/// Every assertion above would pass over an empty file list, which is the exact shape of a
/// check that cannot fail. This one insists the walk found a plausible number of crates, a
/// plausible number of files, non-empty ones, and — the part the flat read could never
/// satisfy — files inside subdirectories.
#[test]
fn the_scans_are_actually_reading_the_tree() {
    let found = crates();
    assert!(
        found.len() >= MIN_CRATES,
        "only {} crates found: {found:?}",
        found.len()
    );

    for check in CHECKS {
        let files = files_for(check).expect("the scan reads");
        assert!(
            !files.is_empty(),
            "the `{}` check scanned no files at all",
            check.name
        );
        assert!(
            files.iter().all(|f| !f.text.is_empty()),
            "the `{}` check read a file as empty",
            check.name
        );
    }

    let all = files_for(
        CHECKS
            .iter()
            .find(|c| c.name == "wall-clock")
            .expect("the workspace-wide check"),
    )
    .expect("the scan reads");
    assert!(
        all.len() > 100,
        "the workspace-wide scan found only {} files",
        all.len()
    );
    let nested: Vec<&str> = all
        .iter()
        .map(|f| f.rel.as_str())
        .filter(|rel| rel.matches('/').count() > 3)
        .collect();
    assert!(
        nested.len() >= 10,
        "the scan found {} files inside a src/ subdirectory; those are the files the node \
         sentinel's flat read cannot see, and the kit exists to see them",
        nested.len()
    );
}

/// Every exemption names a file that exists and says why.
///
/// An exemption whose file has been deleted or renamed is a hole that nobody notices,
/// because a missing path simply never matches.
#[test]
fn every_exemption_names_a_file_that_exists_and_a_reason() {
    let root = repo_root();
    let mut total = 0usize;
    for check in CHECKS {
        for (path, reason) in check.exempt {
            let file = root.join(path);
            assert!(
                file.is_file(),
                "the `{}` check exempts {}, which does not exist",
                check.name,
                file.display()
            );
            assert!(
                reason.len() > 30,
                "the `{}` check exempts {path} with the reason `{reason}`, which does not \
                 explain anything",
                check.name
            );
            total += 1;
        }
    }
    assert_eq!(
        total, 5,
        "the exemption list changed; it should only shrink"
    );
}

/// The rule tables are non-empty and free of duplicate needles.
///
/// A duplicated needle reports the same line twice and makes a failure harder to read; an
/// empty table is a check that cannot fail.
#[test]
fn the_rule_tables_are_populated_and_free_of_duplicates() {
    for rules in [
        GROUND_TRUTH_RULES,
        WALL_CLOCK_RULES,
        HASH_ORDER_RULES,
        TRANSCENDENTAL_RULES,
    ] {
        assert!(!rules.is_empty());
        let mut needles: Vec<&str> = rules.iter().map(|r| r.needle).collect();
        let before = needles.len();
        needles.sort_unstable();
        needles.dedup();
        assert_eq!(needles.len(), before, "a duplicate needle in {needles:?}");
        for rule in rules {
            assert!(!rule.name.is_empty());
            assert!(
                rule.because.len() > 20,
                "rule `{}` does not say why it exists",
                rule.name
            );
        }
    }
}

/// The walk skips what it says it skips.
#[test]
fn the_walk_skips_generated_and_vendored_directories() {
    let root = std::env::temp_dir().join(format!("v2xw-conformance-{}-skip", std::process::id()));
    for dir in ["target", "node_modules", ".hidden"] {
        std::fs::create_dir_all(root.join(dir)).expect("a scratch tree");
        std::fs::write(root.join(dir).join("x.rs"), "fn x() {}\n").expect("writes");
    }
    std::fs::write(root.join("real.rs"), "fn real() {}\n").expect("writes");

    let files = walk_rust_sources(&root, &root).expect("the walk succeeds");
    assert_eq!(
        files.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(),
        vec!["real.rs"],
        "the walk must skip target/, node_modules/ and dot-directories"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A missing directory is an empty result rather than a panic, so a check scoped to a crate
/// that has not been created yet does not take the suite down with it.
#[test]
fn a_missing_directory_scans_to_nothing() {
    let missing = repo_root().join("crates/v2xw-does-not-exist/src");
    assert!(!Path::new(&missing).exists());
    assert!(
        walk_rust_sources(&missing, &repo_root())
            .expect("a missing directory is not an error")
            .is_empty()
    );
}
