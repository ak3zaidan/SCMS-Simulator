//! The interface firewall: the textual invariants, checked over the whole source tree.
//!
//! `v2xw-node` already has a sentinel ([`v2xw_node::firewall`]) that reads its own source
//! and fails if the node runtime has acquired a way to see the truth. It works, it has been
//! shown to go red, and it has one hole.
//!
//! # The hole
//!
//! `crates/v2xw-node/tests/firewall_sentinel.rs::rust_sources` collects the files to scan
//! with a single `std::fs::read_dir` of `src/`, filtered to `extension == "rs"`. A
//! `read_dir` is one directory deep. The moment any policed crate grows a subdirectory —
//! and nine of the seventeen crates already have one, `v2xw-record/src/wire/`,
//! `v2xw-proto/src/scms/`, `v2xw-msg/src/j2735/` and the rest — every file inside it is
//! invisible to the scan while the sentinel still reports success. The check would not
//! *fail*; it would pass while looking at less than it claims to. That is the same defect
//! class as the four checks this project has already found to be incapable of failing
//! (`docs/design/findings/slice-verification.md`).
//!
//! A second, smaller hole is in
//! [`v2xw_node::firewall::scan_ground_truth_fields`]: its `in_test` flag is set by the
//! first `#[cfg(test)]` it meets and is never cleared, so every line after the first test
//! module in a file is skipped — in a file whose test module is not last, the rule stops
//! looking part-way through.
//!
//! [`walk_rust_sources`] is the version without either hole: it recurses, it sorts so a
//! failure reads the same on every machine, and [`scan`] closes the test-module bracket
//! rather than latching it. [`flat_rust_sources`] reproduces the old behaviour on purpose,
//! so `tests/kit/firewall_suite.rs` can demonstrate the difference on a tree built for the
//! occasion instead of asserting it from memory.
//!
//! # What it polices
//!
//! One [`Check`] per non-negotiable rule of the build brief, each with the crates it
//! applies to and the files that are exempt *with a reason*:
//!
//! | Check | Rule | Exemptions |
//! |---|---|---|
//! | `ground-truth` | a node reads beliefs, never truth (I-C2) | the rule table itself |
//! | `wall-clock` | no engine-facing code reads a real clock | the CLI's `wall.rs` and the server's HTTP task |
//! | `hash-order` | no `std` hash container, whose iteration order is unspecified | two Arrow metadata maps |
//! | `transcendental` | no `std` transcendental; always `v2xw_core::math` | none |
//!
//! An exemption is data, not a special case in the scanner, so the list of things allowed
//! to break a rule is one grep away and shrinks when someone deletes a line from it.

use std::io;
use std::path::{Path, PathBuf};

/// A forbidden string and the defect it prevents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rule {
    /// The rule's name, as a violation reports it.
    pub name: &'static str,
    /// The text that must not appear in non-comment, non-test code.
    pub needle: &'static str,
    /// Why the rule exists.
    pub because: &'static str,
}

/// One breach, with enough context for a reader to judge it without opening the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Which check found it.
    pub check: &'static str,
    /// Which rule.
    pub rule: &'static str,
    /// The repository-relative path.
    pub file: String,
    /// One-based line number.
    pub line: usize,
    /// The offending line, trimmed.
    pub text: String,
    /// Why the rule exists.
    pub because: &'static str,
}

impl core::fmt::Display for Violation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}:{}: [{}/{}] {}\n    because {}",
            self.file, self.line, self.check, self.rule, self.text, self.because
        )
    }
}

/// A source file the scanner has read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceFile {
    /// Repository-relative path, with `/` separators on every platform.
    pub rel: String,
    /// The absolute path it was read from.
    pub path: PathBuf,
    /// Its contents.
    pub text: String,
}

/// A named rule set, the crates it applies to, and the files exempt from it.
#[derive(Debug, Clone, Copy)]
pub struct Check {
    /// The check's name, as a violation reports it.
    pub name: &'static str,
    /// The rules it enforces.
    pub rules: &'static [Rule],
    /// The crate directory names it scans; empty means every crate [`crate::crates`] finds.
    pub crates: &'static [&'static str],
    /// Repository-relative files that may break the rule, each with its reason.
    pub exempt: &'static [(&'static str, &'static str)],
}

/// **I-C2.** The node runtime reads its own beliefs and never the ground truth.
///
/// The same four needles [`v2xw_node::firewall::RULES`] carries; the kit checks that it is
/// still a superset of them, so a rule added there and not here is a visible difference.
pub const GROUND_TRUTH_RULES: &[Rule] = &[
    Rule {
        name: "no-ground-truth-kinematics",
        needle: "Kinematics",
        because: "a node reads its PositionEstimate, never the true kinematics \
                  (02-architecture.md §2)",
    },
    Rule {
        name: "no-world-access",
        needle: ".world()",
        because: "NodeView has no world(); a lane graph reaches a node through its own map \
                  store, not through the engine's world",
    },
    Rule {
        name: "no-world-access",
        needle: ".actors()",
        because: "the true set of actors with their true positions is the leak in its \
                  purest form (invariant I-C2)",
    },
    Rule {
        name: "no-actor-ids",
        needle: "ActorId",
        because: "an ActorId names the body behind a node, which a receiver cannot know; \
                  a detector holding one could tell two pseudonyms of one vehicle apart \
                  for free",
    },
];

/// **ADR 0004.** Nothing engine-facing reads a wall clock; time is [`v2xw_core::SimTime`].
pub const WALL_CLOCK_RULES: &[Rule] = &[
    Rule {
        name: "no-wall-clock",
        needle: "SystemTime::now",
        because: "a run's output must be a function of the scenario, the seed and the \
                  build; a real clock makes it a function of when it ran",
    },
    Rule {
        name: "no-wall-clock",
        needle: "Instant::now",
        because: "a duration measured against a real clock differs between two identical \
                  runs, which is how a wall-clock read reaches a digest",
    },
    Rule {
        name: "no-wall-clock",
        needle: "UNIX_EPOCH",
        because: "the only calendar an engine knows is the scenario's t0 \
                  (03-interfaces.md §13)",
    },
];

/// **02-architecture.md §6.4.** No `std` hash container reaches an ordering.
///
/// The needle is the type name rather than an iteration site, because the ordering escapes
/// through so many shapes — `for`, `.iter()`, `.keys()`, `collect()` into a `Vec` — that a
/// rule about iteration would be a rule about nothing. A `BTreeMap`, an `IndexMap` or an
/// explicitly sorted `Vec` costs nothing here and removes the question.
pub const HASH_ORDER_RULES: &[Rule] = &[
    Rule {
        name: "no-std-hash-container",
        needle: "HashMap",
        because: "std's hash iteration order is randomised per process, so anything it \
                  reaches — an ordering, an id assignment, a digest — differs run to run",
    },
    Rule {
        name: "no-std-hash-container",
        needle: "HashSet",
        because: "as HashMap: use BTreeSet, or an explicitly sorted Vec",
    },
];

/// **ADR 0003, ADR 0004 §4.** Every transcendental goes through [`v2xw_core::math`].
///
/// `sqrt` is absent on purpose: IEEE-754 makes it exact, and `math::sqrt` says so.
pub const TRANSCENDENTAL_RULES: &[Rule] = &[
    Rule {
        name: "no-std-transcendental",
        needle: ".sin(",
        because: "std's transcendentals call the platform libm, whose precision the Rust \
                  standard library documents as varying by platform and version; use \
                  v2xw_core::math",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".cos(",
        because: "as .sin(): use v2xw_core::math::cos",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".tan(",
        because: "as .sin(): use v2xw_core::math::tan",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".exp(",
        because: "as .sin(): use v2xw_core::math::exp",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".exp2(",
        because: "as .sin(): use v2xw_core::math::exp2",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".ln(",
        because: "as .sin(): use v2xw_core::math::ln",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".log10(",
        because: "as .sin(): use v2xw_core::math::log10",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".log2(",
        because: "as .sin(): use v2xw_core::math::log2",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".powf(",
        because: "as .sin(): use v2xw_core::math::pow",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".atan2(",
        because: "as .sin(): use v2xw_core::math::atan2",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".asin(",
        because: "as .sin(): use v2xw_core::math::asin",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".acos(",
        because: "as .sin(): use v2xw_core::math::acos",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".atan(",
        because: "as .sin(): use v2xw_core::math::atan",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".hypot(",
        because: "as .sin(): use v2xw_core::math::hypot",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".cbrt(",
        because: "as .sin(): use v2xw_core::math::cbrt",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".sinh(",
        because: "as .sin(): use v2xw_core::math::sinh",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".cosh(",
        because: "as .sin(): use v2xw_core::math::cosh",
    },
    Rule {
        name: "no-std-transcendental",
        needle: ".tanh(",
        because: "as .sin(): use v2xw_core::math::tanh",
    },
];

/// Every check, with its scope and its exemptions.
pub const CHECKS: &[Check] = &[
    Check {
        name: "ground-truth",
        rules: GROUND_TRUTH_RULES,
        crates: &["v2xw-node"],
        exempt: &[(
            "crates/v2xw-node/src/firewall.rs",
            "the rule table: it holds every forbidden string as a literal by construction, \
             and v2xw-node's own tests check that it pulls in no engine type",
        )],
    },
    Check {
        name: "wall-clock",
        rules: WALL_CLOCK_RULES,
        crates: &[],
        exempt: &[
            (
                "crates/v2xw-cli/src/wall.rs",
                "the one module allowed to read a real clock, behind named helpers; nothing \
                 it returns reaches a digest (ADR 0004, and the vertical-slice audit \
                 confirmed it by injection)",
            ),
            (
                "crates/v2xw-server/src/http.rs",
                "the transport: §1.5's ping/pong liveness and §7.4's seek budget are \
                 wall-clock durations by definition, and the instant is handed down rather \
                 than read again below this file (see rpc::Context::received_at)",
            ),
        ],
    },
    Check {
        name: "hash-order",
        rules: HASH_ORDER_RULES,
        crates: &[],
        exempt: &[
            (
                "crates/v2xw-metrics/src/arrow_out.rs",
                "Arrow's schema metadata is typed as a std HashMap by the arrow crate; it \
                 holds one entry and nothing iterates it",
            ),
            (
                "crates/v2xw-record/src/export/schema.rs",
                "as arrow_out.rs: Arrow field metadata, built and handed straight to arrow",
            ),
        ],
    },
    Check {
        name: "transcendental",
        rules: TRANSCENDENTAL_RULES,
        crates: &[],
        exempt: &[],
    },
];

/// Reads every `.rs` file under `root`, **recursively**, sorted by relative path.
///
/// `target`, `node_modules`, `.git` and any other dot-directory are skipped: they hold
/// generated or vendored code that no rule of ours governs, and a scan that walked into
/// `target/` would take minutes and report on code nobody wrote.
///
/// `rel_to` is the directory relative paths are reported against — the repository root, so
/// a violation names a path a reader can paste into an editor.
///
/// # Errors
/// Any I/O failure reading a directory or a file. A file that is not valid UTF-8 is an
/// error rather than a skip: silently ignoring an unreadable file is how a scan comes to
/// cover less than it says.
pub fn walk_rust_sources(root: &Path, rel_to: &Path) -> io::Result<Vec<SourceFile>> {
    let mut out = Vec::new();
    collect(root, rel_to, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect(dir: &Path, rel_to: &Path, out: &mut Vec<SourceFile>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    // `read_dir` order is whatever the file system gives; every caller sorts afterwards.
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect(&path, rel_to, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path)?;
            out.push(SourceFile {
                rel: relative(&path, rel_to),
                path,
                text,
            });
        }
    }
    Ok(())
}

/// The **flat** read the `v2xw-node` sentinel uses: one directory deep, nothing else.
///
/// Public so that `tests/kit/firewall_suite.rs` can show what it misses against a tree it
/// built itself. Nothing in the kit scans with it.
///
/// # Errors
/// As [`walk_rust_sources`].
pub fn flat_rust_sources(root: &Path, rel_to: &Path) -> io::Result<Vec<SourceFile>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() || !path.extension().is_some_and(|e| e == "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        out.push(SourceFile {
            rel: relative(&path, rel_to),
            path,
            text,
        });
    }
    out.sort();
    Ok(out)
}

fn relative(path: &Path, rel_to: &Path) -> String {
    let p = path.strip_prefix(rel_to).unwrap_or(path);
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Scans one file for breaches of `rules`.
///
/// Comments and `#[cfg(test)]` modules are skipped, for the reason the node sentinel gives:
/// the rules are about what the *runtime* can reach, and a test that constructs ground
/// truth in order to prove the node cannot see it is doing the right thing.
///
/// The test-module bracket is tracked by brace depth and **closed** when the depth returns,
/// so a file whose test module is followed by more code is still scanned to the end. A
/// mis-tracked brace makes the scan check more lines, never fewer.
///
/// A `#[cfg(test)]` that is not immediately followed by a module — on a function, a `use`
/// or a field — is discarded rather than left pending, so it cannot silently exempt the
/// next module in the file.
#[must_use]
pub fn scan(check: &'static str, file: &str, src: &str, rules: &'static [Rule]) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut test_mod_depth: Option<i32> = None;
    let mut pending_test_attr = false;

    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        let opens = i32::try_from(raw.matches('{').count()).unwrap_or(i32::MAX);
        let closes = i32::try_from(raw.matches('}').count()).unwrap_or(i32::MAX);
        let is_comment =
            line.starts_with("//") || line.starts_with('*') || line.starts_with("/*");

        // Guarded by `!is_comment` on purpose: a doc comment that *documents* the
        // attribute — as this module's own header does — must not arm the exemption for
        // whatever module happens to follow it.
        if !is_comment && line.contains("#[cfg(test)]") {
            pending_test_attr = true;
        } else if pending_test_attr && !line.is_empty() && !is_comment && !line.starts_with('#') {
            // The attribute applied to something that is not a module, so it exempts
            // nothing from here on.
            if line.starts_with("mod ") && opens > 0 {
                test_mod_depth = Some(depth);
            }
            pending_test_attr = false;
        }

        if test_mod_depth.is_none() && !is_comment {
            for rule in rules {
                if line.contains(rule.needle) {
                    out.push(Violation {
                        check,
                        rule: rule.name,
                        file: file.to_string(),
                        line: i + 1,
                        text: line.to_string(),
                        because: rule.because,
                    });
                }
            }
        }

        depth += opens - closes;
        if let Some(d) = test_mod_depth
            && depth <= d
        {
            test_mod_depth = None;
        }
    }
    out
}

/// Scans for a ground-truth field being read outside the telemetry path.
///
/// The same rule as [`v2xw_node::firewall::scan_ground_truth_fields`], with its latch
/// removed: that version sets `in_test` at the first `#[cfg(test)]` and never clears it, so
/// everything after the first test module in a file goes unchecked. This one uses
/// [`scan`]'s bracket tracking, so a file whose test module sits in the middle is still
/// checked to the end.
///
/// A field whose name starts with `gt_` is one the engine handed in from outside the
/// firewall. Reading it is legal exactly twice: where it is written, and where the
/// telemetry record of vwp-v1 §3.5.2 is built.
#[must_use]
pub fn scan_ground_truth_fields(file: &str, src: &str) -> Vec<Violation> {
    const BECAUSE: &str = "a ground-truth value handed in for the §3.5.2 GT fields must \
                           reach the telemetry record and nothing else; using it to decide \
                           anything is the leak the firewall exists to stop";
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut test_mod_depth: Option<i32> = None;
    let mut pending_test_attr = false;

    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        let opens = i32::try_from(raw.matches('{').count()).unwrap_or(i32::MAX);
        let closes = i32::try_from(raw.matches('}').count()).unwrap_or(i32::MAX);
        let is_comment =
            line.starts_with("//") || line.starts_with('*') || line.starts_with("/*");

        // Guarded by `!is_comment` on purpose: a doc comment that *documents* the
        // attribute — as this module's own header does — must not arm the exemption for
        // whatever module happens to follow it.
        if !is_comment && line.contains("#[cfg(test)]") {
            pending_test_attr = true;
        } else if pending_test_attr && !line.is_empty() && !is_comment && !line.starts_with('#') {
            if line.starts_with("mod ") && opens > 0 {
                test_mod_depth = Some(depth);
            }
            pending_test_attr = false;
        }

        if test_mod_depth.is_none()
            && !is_comment
            && let Some(at) = line.find("self.gt_")
        {
            let rest = &line[at..];
            let is_write = rest
                .split_once('=')
                .is_some_and(|(lhs, rhs)| !lhs.contains('(') && !rhs.starts_with('='));
            let is_telemetry_field =
                line.starts_with("pos_error_m:") || line.starts_with("clock_offset_ns:");
            if !is_write && !is_telemetry_field {
                out.push(Violation {
                    check: "ground-truth",
                    rule: "ground-truth-fields-reach-only-telemetry",
                    file: file.to_string(),
                    line: i + 1,
                    text: line.to_string(),
                    because: BECAUSE,
                });
            }
        }

        depth += opens - closes;
        if let Some(d) = test_mod_depth
            && depth <= d
        {
            test_mod_depth = None;
        }
    }
    out
}

/// The files one [`Check`] scans, read from disk.
///
/// # Errors
/// As [`walk_rust_sources`].
pub fn files_for(check: &Check) -> io::Result<Vec<SourceFile>> {
    let root = crate::repo_root();
    let names: Vec<String> = if check.crates.is_empty() {
        crate::crates()
    } else {
        check.crates.iter().map(|s| (*s).to_string()).collect()
    };
    let mut out = Vec::new();
    for name in names {
        let dir = root.join("crates").join(&name).join("src");
        out.extend(walk_rust_sources(&dir, &root)?);
    }
    out.sort();
    Ok(out)
}

/// Runs one check over the repository and returns every breach, in path order.
///
/// # Errors
/// As [`walk_rust_sources`].
pub fn run_check(check: &'static Check) -> io::Result<Vec<Violation>> {
    let mut out = Vec::new();
    for file in files_for(check)? {
        if check.exempt.iter().any(|(p, _)| *p == file.rel) {
            continue;
        }
        out.extend(scan(check.name, &file.rel, &file.text, check.rules));
    }
    Ok(out)
}

/// Runs every check in [`CHECKS`].
///
/// # Errors
/// As [`walk_rust_sources`].
pub fn run_all() -> io::Result<Vec<Violation>> {
    let mut out = Vec::new();
    for check in CHECKS {
        out.extend(run_check(check)?);
    }
    Ok(out)
}

/// Renders violations one per line, for an assertion message.
#[must_use]
pub fn report(violations: &[Violation]) -> String {
    violations
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE: &[Rule] = &[Rule {
        name: "probe",
        needle: "FORBIDDEN",
        because: "a synthetic needle, so these tests do not depend on any real rule",
    }];

    /// Each rule fires on the line it is about, and a clean file produces nothing.
    #[test]
    fn the_scanner_finds_a_breach_and_leaves_honest_code_alone() {
        let hit = scan("c", "src/x.rs", "fn f() {\n    let _ = FORBIDDEN;\n}\n", PROBE);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].line, 2);
        assert!(scan("c", "src/y.rs", "fn f() {}\n", PROBE).is_empty());
    }

    /// Comments are exempt, so a rule can be documented in the crate it polices.
    #[test]
    fn comments_are_exempt() {
        let src = "// never write FORBIDDEN here\n/// nor FORBIDDEN here\nfn f() {}\n";
        assert!(scan("c", "src/c.rs", src, PROBE).is_empty());
    }

    /// The exemption for a test module **ends with the module**.
    ///
    /// This is the second hole in the node sentinel's scan, and the reason this test exists
    /// rather than being taken on trust: run the same input through a latching
    /// implementation and the third breach is missed.
    #[test]
    fn a_test_module_is_exempt_but_the_code_after_it_is_not() {
        let src = "fn before() { let _ = FORBIDDEN; }\n\
                   #[cfg(test)]\n\
                   mod tests {\n    \
                   fn t() { let _ = FORBIDDEN; }\n\
                   }\n\
                   fn after() { let _ = FORBIDDEN; }\n";
        let v = scan("c", "src/x.rs", src, PROBE);
        let lines: Vec<usize> = v.iter().map(|x| x.line).collect();
        assert_eq!(lines, vec![1, 6], "found {v:?}");
    }

    /// A `#[cfg(test)]` on something that is not a module exempts nothing.
    #[test]
    fn a_cfg_test_attribute_on_a_function_does_not_exempt_the_next_module() {
        let src = "#[cfg(test)]\n\
                   fn helper() {}\n\
                   mod real {\n    \
                   fn f() { let _ = FORBIDDEN; }\n\
                   }\n";
        let v = scan("c", "src/x.rs", src, PROBE);
        assert_eq!(v.len(), 1, "found {v:?}");
        assert_eq!(v[0].line, 4);
    }

    /// The ground-truth field rule lets the write and the telemetry read through, catches
    /// the third use, and keeps looking after a test module.
    #[test]
    fn a_ground_truth_field_may_only_reach_the_telemetry_record() {
        let ok = "fn set(&mut self, e: f32) {\n    self.gt_pos_error_m = e;\n}\n\
                  fn build(&self) -> T {\n    T {\n\
                  pos_error_m: self.gt_pos_error_m,\n    }\n}\n";
        assert!(scan_ground_truth_fields("src/r.rs", ok).is_empty());

        let leak = "#[cfg(test)]\n\
                    mod tests {\n    \
                    fn t() { let _ = self.gt_pos_error_m; }\n\
                    }\n\
                    fn decide(&self) -> bool {\n    self.gt_pos_error_m < 1.0\n}\n";
        let v = scan_ground_truth_fields("src/r.rs", leak);
        assert_eq!(v.len(), 1, "found {v:?}");
        assert_eq!(v[0].line, 6, "the read after the test module must be caught");
    }
}
