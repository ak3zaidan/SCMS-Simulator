//! Prints a golden run's digest, for the three-platform determinism comparison.
//!
//! Running the same tests on three platforms does **not** prove the three agree: each job
//! only checks its own assertions. `.github/workflows/ci.yml` says so in its own header and
//! solves it by having each platform publish digests and a fourth job compare them. This
//! binary is the engine-level version of the value those jobs publish, and it calls
//! [`v2xw_conformance::golden::digest_of_scenario`] — the same function
//! `tests/kit/golden_suite.rs` asserts on — so the number CI compares and the number the
//! test checks cannot drift apart.
//!
//! ```text
//! cargo run --release -p v2xw-conformance --bin v2xw-golden-digest                 # every case
//! cargo run --release -p v2xw-conformance --bin v2xw-golden-digest -- grid-traffic # one case
//! cargo run --release -p v2xw-conformance --bin v2xw-golden-digest -- --json       # as JSON
//! ```
//!
//! It exits non-zero on failure and prints to stderr, so a CI step fails rather than
//! publishing an empty artefact — the failure mode the existing determinism job guards
//! against with its `test -n "$world"` lines.

use std::process::ExitCode;

use v2xw_conformance::golden::{CASES, Case, RunDigest, digest_of_scenario};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let as_json = args.iter().any(|a| a == "--json");
    let wanted: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();

    let cases: Vec<&Case> = if wanted.is_empty() {
        CASES.iter().collect()
    } else {
        CASES.iter().filter(|c| wanted.contains(&c.name)).collect()
    };

    if cases.is_empty() {
        eprintln!(
            "no such case: {wanted:?}; known cases are {:?}",
            CASES.iter().map(|c| c.name).collect::<Vec<_>>()
        );
        return ExitCode::FAILURE;
    }

    let mut digests: Vec<RunDigest> = Vec::with_capacity(cases.len());
    for case in cases {
        let path = v2xw_conformance::repo_root().join(case.scenario);
        match digest_of_scenario(case.name, &path) {
            Ok(d) => digests.push(d),
            Err(e) => {
                eprintln!("{}: {e}", case.name);
                return ExitCode::FAILURE;
            }
        }
    }

    if as_json {
        match serde_json::to_string_pretty(&digests) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("serialising the digests: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        for d in &digests {
            println!("{}", d.to_key_values());
        }
    }
    ExitCode::SUCCESS
}
