//! The crate's non-negotiable rules, checked by scanning its own sources.
//!
//! Four rules bind every crate in this workspace, and three of them are properties of the
//! *source* rather than of any value a test can compute:
//!
//! 1. **No `std` transcendental.** `f64::sin` and friends call the platform libm, whose
//!    precision the Rust standard library documents as varying "by platform, Rust version,
//!    and can even differ within the same execution" (ADR 0003, ADR 0004 §4). Every
//!    transcendental goes through `v2xw_core::math`, which routes to the `libm` crate.
//! 2. **No `std` `HashMap`/`HashSet` iteration may reach an output ordering or a hash.**
//! 3. **No wall-clock read in engine-facing code.** Times come from `SimTime`, and this
//!    crate's runtime diagnostics take their wall-clock measurements as arguments.
//! 4. **No `unsafe`.** `#![forbid(unsafe_code)]` already enforces this; it is scanned too,
//!    because a `#[allow]` somewhere would silently reopen it.
//!
//! A rule nothing checks is a comment, so this test is the check. It reads the crate's own
//! `src/*.rs`, strips comments, and looks for the forbidden forms.
//!
//! # How the scan handles comments
//!
//! Several modules *discuss* the forbidden forms in their documentation — `runtime.rs` says
//! in prose that `Instant` appears nowhere in the crate — so a scan over raw text would
//! flag the very sentences that state the rule. Each line is therefore truncated at its
//! first `//`, which removes line and doc comments. That is deliberately conservative: a
//! string literal containing `//` would also be truncated, which can only hide a violation
//! and never invent one, and no string literal in this crate contains `//`.

use std::path::{Path, PathBuf};

/// The crate's `src` directory.
fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file directly under `src`, with its contents, sorted by name.
fn sources() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(src_dir())
        .expect("the crate's src directory is readable")
        .map(|e| e.expect("a readable directory entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&p).expect("a readable source file");
            (name, text)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        out.len() >= 15,
        "the scan found only {} source files, so it is probably looking in the wrong place",
        out.len()
    );
    out
}

/// `text` with every line truncated at its first `//`, so comments cannot trip the scan.
fn without_comments(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `(file, line number, line)` in the crate's code whose text contains `needle`.
fn hits(needle: &str) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for (name, text) in sources() {
        for (i, line) in without_comments(&text).lines().enumerate() {
            if line.contains(needle) {
                out.push((name.clone(), i + 1, line.trim().to_string()));
            }
        }
    }
    out
}

/// Fails with the file and line of every hit.
fn forbid(needle: &str, why: &str) {
    let found = hits(needle);
    assert!(
        found.is_empty(),
        "`{needle}` is forbidden in this crate ({why}), found at:\n{}",
        found
            .iter()
            .map(|(f, l, t)| format!("  src/{f}:{l}: {t}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Rule 1: every transcendental goes through `v2xw_core::math`, never `std`.
///
/// `sqrt` is the documented exemption — IEEE-754 requires it to be correctly rounded, so
/// every platform agrees on it — and this crate calls `v2xw_core::math::sqrt` for it anyway,
/// so the method form is forbidden here too and the scan is one rule with no special case.
#[test]
fn no_std_transcendental_is_called() {
    for method in [
        ".sin()",
        ".cos()",
        ".tan()",
        ".asin()",
        ".acos()",
        ".atan()",
        ".atan2(",
        ".sin_cos()",
        ".sinh()",
        ".cosh()",
        ".tanh()",
        ".exp()",
        ".exp2()",
        ".exp_m1()",
        ".ln()",
        ".ln_1p()",
        ".log10()",
        ".log2()",
        ".log(",
        ".powf(",
        ".powi(",
        ".cbrt()",
        ".hypot(",
        ".sqrt()",
        "f64::sin",
        "f64::cos",
        "f64::exp",
        "f64::ln",
        "f64::powf",
        "f64::sqrt",
    ] {
        forbid(
            method,
            "std delegates to the platform libm, which ADR 0003 documents as \
             platform-dependent; use v2xw_core::math",
        );
    }
    // …and the one square root the crate does take is the routed one.
    let routed = hits("math::sqrt");
    assert_eq!(
        routed.len(),
        1,
        "expected exactly one v2xw_core::math::sqrt call (the Wilson interval), found {}: {:?}",
        routed.len(),
        routed
    );
    assert_eq!(routed[0].0, "stats.rs");
}

/// Rule 2: no `std` hash container, except the single-entry Arrow schema metadata.
///
/// Arrow's `Schema::new_with_metadata` takes a `std::collections::HashMap` and no other
/// type, so `arrow_out.rs` holds one. It holds **one entry**, so it has no iteration order
/// to get wrong — a separate unit test in that module asserts the entry count, which is the
/// property that actually matters. Every other file must be free of them.
#[test]
fn no_hash_container_outside_the_single_entry_arrow_metadata() {
    for (name, text) in sources() {
        let code = without_comments(&text);
        for needle in ["HashMap", "HashSet", "hashbrown"] {
            let found: Vec<&str> = code
                .lines()
                .filter(|l| l.contains(needle))
                .map(str::trim)
                .collect();
            if name == "arrow_out.rs" && needle == "HashMap" {
                assert!(
                    found.len() <= 4,
                    "arrow_out.rs may hold only the single-entry schema metadata map, found \
                     {} mentions: {found:?}",
                    found.len()
                );
                continue;
            }
            assert!(
                found.is_empty(),
                "src/{name} uses `{needle}`; a hash container's iteration order must not \
                 reach an output ordering or a hash — use BTreeMap or IndexMap. Found: \
                 {found:?}"
            );
        }
    }
}

/// Rule 3: no wall clock. The runtime diagnostics take their measurements as arguments.
#[test]
fn no_wall_clock_is_read() {
    for needle in [
        "Instant::now",
        "SystemTime",
        "std::time::",
        "UNIX_EPOCH",
        "chrono",
        "Local::now",
        "Utc::now",
    ] {
        forbid(
            needle,
            "times come from SimTime; a wall-clock read makes a run irreproducible",
        );
    }
}

/// Rule 4: no `unsafe`, and no `#[allow]` that would let one in.
#[test]
fn there_is_no_unsafe_and_no_blanket_allow() {
    forbid("unsafe ", "the crate forbids unsafe_code");
    forbid("allow(unsafe", "the crate forbids unsafe_code");
    // The forbid attribute itself is present, and in the crate root.
    let (_, root) = sources()
        .into_iter()
        .find(|(n, _)| n == "lib.rs")
        .expect("lib.rs");
    assert!(root.contains("#![forbid(unsafe_code)]"));
    assert!(root.contains("#![deny(missing_docs)]"));
}

/// A run's own randomness: a metric provider draws none, and every card says so.
///
/// 03-interfaces.md §17's card-completeness item requires every RNG domain a plug-in draws
/// from to be declared. A metric provider reduces recorded events and has nothing to draw
/// for, so the honest declaration is `uses_rng: false` with no domains — and the scan
/// confirms there is no generator in the crate to contradict it.
#[test]
fn no_provider_draws_random_numbers() {
    for needle in ["RngStream", "rng(", "rand::", "thread_rng", "ChaCha"] {
        forbid(
            needle,
            "a metric provider reduces recorded events and draws no random numbers; its \
             card declares uses_rng: false",
        );
    }
}
