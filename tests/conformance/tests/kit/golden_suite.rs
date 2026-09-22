//! The golden determinism harness, against the committed record.
//!
//! One scenario is run once per test session. The run is the expensive part of this whole
//! kit, so the single run's digest is used for every assertion rather than re-running for
//! each — and the assertions are ordered so the most informative failure comes first: a run
//! that produced nothing is a different problem from a run that produced the wrong thing.

use v2xw_conformance::golden::{
    BLESS_ENV, CASES, Comparison, RunDigest, blessing, compare_against_golden, digest_of,
    golden_path, load_golden,
};

/// **The golden case reproduces its committed digest.**
///
/// If no record has been committed, this fails and says how to write one. That is
/// deliberate: a harness that created its own oracle on first run and then agreed with it
/// for ever would be a check that cannot fail. The first failure is where a human looks at
/// the number and decides it is right.
#[test]
fn the_golden_case_reproduces_its_committed_digest() {
    let case = CASES.first().expect("at least one golden case");
    let digest = digest_of(case).unwrap_or_else(|e| panic!("{}: {e}", case.name));

    // First: the run did something. Every comparison below is satisfied by two identical
    // nothings, so this is the assertion that gives the rest their meaning.
    assert!(
        digest.records > 0,
        "the run wrote no records at all, so its digest is the digest of nothing"
    );
    assert!(digest.end_ns > 0, "the run did not advance");
    assert_eq!(digest.scenario_hash.len(), 64, "no scenario hash");
    assert_eq!(digest.world_hash.len(), 64, "no world hash");
    assert_eq!(digest.record_digest.len(), 64, "no record digest");
    assert!(
        digest.per_channel.len() >= 2,
        "the run wrote {} channel(s): {:?}",
        digest.per_channel.len(),
        digest.per_channel.keys().collect::<Vec<_>>()
    );

    match compare_against_golden(&digest).expect("the golden record is readable") {
        Comparison::Match => {}
        Comparison::Blessed { path } => {
            assert!(
                blessing(),
                "a record was written without {BLESS_ENV} being set"
            );
            eprintln!("blessed {}", path.display());
        }
        Comparison::Missing { path } => panic!(
            "no golden record for `{}` at {}.\n\
             Run the case, read the numbers, and if they are right commit the record:\n  \
             {BLESS_ENV}=1 cargo test -p v2xw-conformance --test kit golden\n\
             This fails rather than writing the file itself, because a harness that \
             invents its own oracle on the first run agrees with it for ever.",
            case.name,
            path.display()
        ),
        Comparison::Differs { lines } => panic!(
            "`{}` no longer reproduces its committed digest:\n  {}\n\n\
             This is the determinism contract of ADR 0004 failing, or a deliberate change \
             to the engine's output. If it is the latter, re-bless with {BLESS_ENV}=1 and \
             say in the commit message what changed and why.",
            case.name,
            lines.join("\n  ")
        ),
    }
}

/// The committed record, if there is one, is not a record of nothing.
///
/// Reads the file rather than running the engine, so it costs nothing and still catches the
/// failure mode a blessed-on-first-run harness produces: a golden file full of zeroes that
/// every future run reproduces exactly.
#[test]
fn a_committed_golden_record_is_not_a_record_of_nothing() {
    for case in CASES {
        let Some(golden) = load_golden(case.name).expect("the record is readable") else {
            // Absent is reported by the test above; this one is about what a present
            // record contains.
            continue;
        };
        assert_eq!(golden.case, case.name);
        assert!(
            golden.records > 0,
            "{} is a golden record of a run that wrote nothing: {}",
            golden_path(case.name).display(),
            golden.to_key_values()
        );
        assert!(golden.end_ns > 0, "{} never advanced", case.name);
        assert_ne!(
            golden.record_digest,
            "0".repeat(64),
            "{} has a placeholder digest",
            case.name
        );
        assert!(
            !golden.per_channel.is_empty(),
            "{} names no channel",
            case.name
        );
    }
}

/// The case list is usable: names are unique, files exist, reasons are given.
///
/// Costs no engine time, and stops the harness failing halfway through a run with a
/// file-not-found that the scenario loader words unhelpfully.
#[test]
fn the_case_list_is_well_formed() {
    assert!(!CASES.is_empty(), "the golden set is empty");
    let mut names: Vec<&str> = CASES.iter().map(|c| c.name).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "two cases share a name");

    for case in CASES {
        let path = v2xw_conformance::repo_root().join(case.scenario);
        assert!(
            path.is_file(),
            "case `{}` names {}, which does not exist",
            case.name,
            path.display()
        );
        assert!(
            case.because.len() > 30,
            "case `{}` does not say why it is in the set",
            case.name
        );
    }
}

/// The record's key/value form round-trips through JSON, which is what the CI job and the
/// committed file each rely on.
#[test]
fn a_record_survives_the_form_ci_and_the_repository_store_it_in() {
    let original = RunDigest {
        case: "probe".to_string(),
        scenario_hash: "11".repeat(32),
        world_hash: "22".repeat(32),
        master_seed: 42,
        end_ns: 1,
        records: 7,
        record_digest: "33".repeat(32),
        per_channel: std::collections::BTreeMap::from([("node.tx".to_string(), (7u64, 70u64))]),
    };
    let json = serde_json::to_string(&original).expect("serialises");
    let back: RunDigest = serde_json::from_str(&json).expect("deserialises");
    assert_eq!(back, original);
    assert_eq!(back.to_key_values(), original.to_key_values());
    assert!(original.differences(&back).is_empty());
}
