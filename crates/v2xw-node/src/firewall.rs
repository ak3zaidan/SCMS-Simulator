//! The `NodeView` conformance sentinel (build decision D11, invariants I-C2 and I-T1).
//!
//! # Why a sentinel and not a type
//!
//! `v2xw-core`'s module documentation once claimed that a plug-in reaching for ground
//! truth "fails to compile". An adversarial review on 2026-09-18 established that this is
//! false — an implementor can smuggle the world in through an associated type — and build
//! decision D11 §3 records the correction:
//!
//! > The claim is corrected, and enforcement moves to where it can actually live: the
//! > conformance kit's sentinel test. An honest boundary with a test is worth more than an
//! > overstated one without.
//!
//! This module is that test. It reads this crate's own source and fails if the node
//! runtime has acquired a way to see the truth. It is a blunt instrument — it matches
//! text — and that is the point: the failure mode it guards against is a plausible-looking
//! line of code that nobody notices, and a textual rule is one a reviewer can check by
//! eye against the test's own output.
//!
//! # What it checks, and what each rule is really about
//!
//! | Rule | Forbidden | The defect it prevents |
//! |---|---|---|
//! | `no-ground-truth-kinematics` | [`v2xw_core::kinematics::Kinematics`] by name | the node reading where the vehicle *is* instead of where it believes it is |
//! | `no-world-access` | `.world()`, `.actors()` | the node resolving a sender to the actor that really sent it |
//! | `no-actor-ids` | [`v2xw_core::ids::ActorId`] by name | linking two pseudonyms of one vehicle for free, which is the thing a Sybil detector must work out |
//! | `narrowed-context-has-no-truth` | `fn world`/`fn actors` on [`crate::ctx::NodeCtx`] | re-opening the hole one level up, in the trait the runtime is driven through |
//! | `ground-truth-fields-reach-only-telemetry` | a `gt_`-prefixed field read outside the telemetry path | a value that arrived legitimately and is then used to decide something |
//!
//! The last rule is the one worth the most. Two numbers in vwp-v1 §3.5.2 are marked **GT**
//! and a node genuinely cannot compute them, so they are handed in from outside. That is a
//! legitimate door, and a legitimate door is how the leak arrives: the value is in the
//! struct, it is in scope, and using it makes the model "better".
//!
//! # It has been shown to fail
//!
//! A check nobody has seen go red is a check that reads as evidence while proving
//! nothing. The faults injected to demonstrate each rule are listed in
//! `tests/firewall_sentinel.rs`, which also drives the scan over the real source tree.

/// A rule the sentinel enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rule {
    /// Its name, as a violation reports it.
    pub name: &'static str,
    /// The text that must not appear.
    pub needle: &'static str,
    /// Why.
    pub because: &'static str,
}

/// Every rule, in the order the module documentation tabulates them.
pub const RULES: &[Rule] = &[
    Rule {
        name: "no-ground-truth-kinematics",
        needle: "Kinematics",
        because: "a node reads its PositionEstimate, never the true kinematics \
                  (02-architecture.md §2, ADR 0010)",
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

/// One breach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Which rule.
    pub rule: &'static str,
    /// Which file.
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
            "{}:{}: [{}] {}\n    because {}",
            self.file, self.line, self.rule, self.text, self.because
        )
    }
}

/// Scans one file's source for breaches.
///
/// Lines inside a `#[cfg(test)]` module, doc comments and ordinary comments are skipped:
/// the rules are about what the *runtime* can reach, and a test that constructs ground
/// truth in order to prove the node cannot see it is doing the right thing. The test-module
/// boundary is found by the `#[cfg(test)]` attribute and brace depth rather than by parsing,
/// which is coarse but conservative — a mis-tracked brace makes the scan check *more*
/// lines, never fewer.
pub fn scan_source(file: &str, src: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut test_mod_depth: Option<i32> = None;
    let mut pending_test_attr = false;

    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();

        if line.contains("#[cfg(test)]") {
            pending_test_attr = true;
        }
        let opens = raw.matches('{').count() as i32;
        let closes = raw.matches('}').count() as i32;

        let in_test = test_mod_depth.is_some();
        let is_comment = line.starts_with("//") || line.starts_with("*") || line.starts_with("/*");

        if !in_test && !is_comment {
            for rule in RULES {
                if line.contains(rule.needle) {
                    out.push(Violation {
                        rule: rule.name,
                        file: file.to_string(),
                        line: i + 1,
                        text: line.to_string(),
                        because: rule.because,
                    });
                }
            }
        }

        if pending_test_attr && line.starts_with("mod ") && opens > 0 {
            test_mod_depth = Some(depth);
            pending_test_attr = false;
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

/// Scans the narrowed context trait's declaration for a ground-truth accessor.
///
/// Separate from [`scan_source`] because the needle is different — a trait *declaring*
/// `fn world` is the breach, not a caller invoking one — and because this is the rule that
/// closes the hole one level above the view.
pub fn scan_context_trait(src: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut in_trait = false;
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with("pub trait NodeCtx") {
            in_trait = true;
            continue;
        }
        if in_trait && line == "}" {
            in_trait = false;
        }
        if in_trait && (line.starts_with("fn world") || line.starts_with("fn actors")) {
            out.push(Violation {
                rule: "narrowed-context-has-no-truth",
                file: "src/ctx.rs".to_string(),
                line: i + 1,
                text: line.to_string(),
                because: "the narrowed node context is where a runtime would re-open the \
                          hole the view closes (build decision D12.2, invariant I-C2)",
            });
        }
    }
    out
}

/// Scans for a ground-truth field being read outside the telemetry path.
///
/// A field whose name starts with `gt_` is one the engine handed in from outside the
/// firewall. Reading it is legal exactly twice: where it is written, and where the
/// telemetry record is built. Anywhere else, a decision is being made on the truth.
pub fn scan_ground_truth_fields(file: &str, src: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut in_test = false;
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if line.contains("#[cfg(test)]") {
            in_test = true;
        }
        if in_test || line.starts_with("//") || line.starts_with("*") {
            continue;
        }
        let Some(at) = line.find("self.gt_") else {
            continue;
        };
        // The write (`self.gt_x = …`) and the declaration are fine; so is the one read
        // that builds the telemetry input struct, which names the §3.5.2 field it feeds.
        let rest = &line[at..];
        let is_write = rest
            .split_once('=')
            .is_some_and(|(lhs, rhs)| !lhs.contains('(') && !rhs.starts_with('='));
        let is_telemetry_field =
            line.starts_with("pos_error_m:") || line.starts_with("clock_offset_ns:");
        if is_write || is_telemetry_field {
            continue;
        }
        out.push(Violation {
            rule: "ground-truth-fields-reach-only-telemetry",
            file: file.to_string(),
            line: i + 1,
            text: line.to_string(),
            because: "a ground-truth value handed in for the §3.5.2 GT fields must reach \
                      the telemetry record and nothing else; using it to decide anything \
                      is the leak the firewall exists to stop",
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each rule fires on the line it is about.
    #[test]
    fn the_scanner_finds_each_kind_of_breach() {
        let src = "fn leak(ctx: &dyn Ctx) {\n    \
                   let truth = ctx.world();\n    \
                   let who: ActorId = ctx.actors().first();\n    \
                   let k: Kinematics = truth.of(who);\n}\n";
        let v = scan_source("src/leak.rs", src);
        let names: Vec<&str> = v.iter().map(|x| x.rule).collect();
        assert!(names.contains(&"no-world-access"));
        assert!(names.contains(&"no-actor-ids"));
        assert!(names.contains(&"no-ground-truth-kinematics"));
        assert_eq!(v[0].line, 2);
    }

    /// A clean file produces nothing.
    #[test]
    fn honest_source_is_clean() {
        let src = "fn honest(view: &dyn NodeView) -> Vec3 {\n    \
                   view.position().pos\n}\n";
        assert_eq!(scan_source("src/honest.rs", src), Vec::new());
    }

    /// Test modules are exempt, because a test that builds ground truth in order to show
    /// the node cannot see it is doing exactly the right thing.
    #[test]
    fn test_modules_are_exempt_but_the_exemption_ends_with_them() {
        let src = "fn real() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n    \
                   fn t() { let k: Kinematics = truth(); }\n\
                   }\n\
                   fn after() { let k: Kinematics = truth(); }\n";
        let v = scan_source("src/x.rs", src);
        assert_eq!(v.len(), 1, "only the line after the test module: {v:?}");
        assert_eq!(v[0].line, 6);
    }

    /// Comments are exempt, so the rules can be *documented* in the crate they police.
    #[test]
    fn comments_are_exempt() {
        let src = "// A node never reads Kinematics or calls .world().\n\
                   fn f() {}\n";
        assert_eq!(scan_source("src/c.rs", src), Vec::new());
    }

    /// The context-trait rule fires on a declaration, not on a call.
    #[test]
    fn the_context_trait_rule_catches_a_reopened_hole() {
        let clean = "pub trait NodeCtx {\n    fn now(&self) -> SimTime;\n}\n";
        assert_eq!(scan_context_trait(clean), Vec::new());
        let leaky = "pub trait NodeCtx {\n    fn now(&self) -> SimTime;\n    \
                     fn world(&self) -> &World;\n}\n";
        let v = scan_context_trait(leaky);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "narrowed-context-has-no-truth");
    }

    /// The ground-truth field rule lets the write and the telemetry read through and
    /// catches the third use.
    #[test]
    fn a_ground_truth_field_may_only_reach_the_telemetry_record() {
        let ok = "fn set(&mut self, e: f32) {\n    self.gt_pos_error_m = e;\n}\n\
                  fn build(&self) -> T {\n    T {\n\
                  pos_error_m: self.gt_pos_error_m,\n    }\n}\n";
        assert_eq!(scan_ground_truth_fields("src/r.rs", ok), Vec::new());

        let leak = "fn decide(&self) -> bool {\n    self.gt_pos_error_m < 1.0\n}\n";
        let v = scan_ground_truth_fields("src/r.rs", leak);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "ground-truth-fields-reach-only-telemetry");
    }
}
