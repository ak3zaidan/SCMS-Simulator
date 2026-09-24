//! The plug-in conformance suite (03-interfaces.md §17).
//!
//! A contributor writes a model, registers its card and expects the engine to run it. This
//! module is what they run first. It is a library rather than a test file because its users
//! are outside this repository: a propagation model in someone else's crate cannot be
//! reached by a `#[test]` that lives in ours.
//!
//! ```no_run
//! use v2xw_conformance::plugin::{PluginUnderTest, ProbeCtx, run_suite};
//! # struct MyModel;
//! # impl v2xw_core::model::Model for MyModel {
//! #     fn card(&self) -> &v2xw_core::card::ModelCard { unimplemented!() }
//! # }
//! # struct Subject(MyModel);
//! # impl PluginUnderTest for Subject {
//! #     fn model(&self) -> &dyn v2xw_core::model::Model { &self.0 }
//! #     fn entities(&self) -> Vec<v2xw_core::rng::EntityRef> { Vec::new() }
//! #     fn exercise(&self, _: &mut ProbeCtx, _: v2xw_core::rng::EntityRef) -> Vec<f64> { Vec::new() }
//! # }
//! # let subject = Subject(MyModel);
//! let report = run_suite(&subject, 0x5EED);
//! assert!(report.passed(), "{}", report);
//! ```
//!
//! # The five properties §17 asks for, and how each is actually established
//!
//! | Property | How |
//! |---|---|
//! | owns no random number generator | a source scan for `rand::`, `thread_rng`, a concrete generator type, or a second [`RngRegistry`] |
//! | reads no wall clock | a source scan for the needles [`crate::firewall::WALL_CLOCK_RULES`] carries |
//! | reaches no ground truth | a source scan for the needles [`crate::firewall::GROUND_TRUTH_RULES`] carries, plus the emitted-record visibility check |
//! | registers a valid card with a source for every default | [`v2xw_core::card::ModelCard::validate`], plus the stronger rule that every parameter cites something |
//! | is deterministic under reordering | the same entities exercised forwards, backwards, and across eight threads, compared value for value |
//!
//! The source scans are textual and therefore blunt, which is the same trade the node
//! sentinel makes and for the same reason: the failure they guard against is a
//! plausible-looking line nobody notices, and a textual rule is one a reviewer can check by
//! eye. A plug-in that supplies no source paths gets no scan, and the report says so rather
//! than reporting a pass — an absent check must not read as a passed one.

use std::cell::RefCell;
use std::path::PathBuf;

use v2xw_core::card::{ModelCard, SourceKind, Tier};
use v2xw_core::ctx::{Ctx, ErasedRecord, Visibility};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::ids::ActorId;
use v2xw_core::math::is_on_grid;
use v2xw_core::model::Model;
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::SimTime;

use crate::firewall::{GROUND_TRUTH_RULES, Rule, WALL_CLOCK_RULES, scan};

/// A model must not own randomness: every draw comes from the engine's keyed streams.
pub const OWNED_RNG_RULES: &[Rule] = &[
    Rule {
        name: "no-owned-rng",
        needle: "thread_rng",
        because: "a thread-local generator makes a draw depend on which thread ran it, \
                  which is the nondeterminism the keyed streams exist to remove",
    },
    Rule {
        name: "no-owned-rng",
        needle: "rand::",
        because: "a plug-in draws through Ctx::rng(domain, entity); reaching for `rand` \
                  directly bypasses the (RngDomain, EntityRef) key the digest depends on",
    },
    Rule {
        name: "no-owned-rng",
        needle: "OsRng",
        because: "operating-system entropy makes a run irreproducible by construction",
    },
    Rule {
        name: "no-owned-rng",
        needle: "StdRng",
        because: "a generator the plug-in owns is a generator the engine cannot key or seed",
    },
    Rule {
        name: "no-owned-rng",
        needle: "SmallRng",
        because: "as StdRng",
    },
    Rule {
        name: "no-owned-rng",
        needle: "ChaCha",
        because: "as StdRng: the engine derives the ChaCha stream, the plug-in asks for it",
    },
    Rule {
        name: "no-owned-rng",
        needle: "RngRegistry::new",
        because: "a second registry has a second master seed, so the plug-in's draws stop \
                  being a function of the run's seed",
    },
    Rule {
        name: "no-owned-rng",
        needle: "RngStream::derive",
        because: "deriving a stream restarts it at word zero on every call; ask the \
                  registry through Ctx::rng instead",
    },
];

/// The event payload a probe context carries: a label, which is all a probe needs.
pub type ProbePayload = u64;

/// A [`Ctx`] that records what the plug-in asked it for.
///
/// Its shape is `v2xw_sec::testctx::TestCtx`'s — a clock, a seeded [`RngRegistry`], a
/// scheduler, a provenance log, a parameter set and a record sink — with one addition the
/// existing helpers cannot provide: every `(domain, entity)` key the plug-in checked out is
/// logged, which is what turns "the card declares its RNG domains" from a claim into a
/// comparison.
///
/// There is no world and there are no actors, and that is the point: a plug-in that needs
/// either is reaching past the interface it was handed.
#[derive(Debug)]
pub struct ProbeCtx {
    now: SimTime,
    scheduler: Scheduler<ProbePayload>,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    world: (),
    actors: Vec<ActorId>,
    draws: RefCell<Vec<(RngDomain, EntityRef)>>,
    /// Every record the plug-in emitted, as `(channel, visibility, json)`.
    pub emitted: Vec<(&'static str, Visibility, String)>,
}

impl ProbeCtx {
    /// A probe at simulated time zero with `master_seed` and no parameters.
    #[must_use]
    pub fn new(master_seed: u64) -> ProbeCtx {
        ProbeCtx {
            now: 0,
            scheduler: Scheduler::new(),
            rng: RngRegistry::new(master_seed),
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            world: (),
            actors: Vec::new(),
            draws: RefCell::new(Vec::new()),
            emitted: Vec::new(),
        }
    }

    /// A probe whose parameters are the card's declared defaults.
    ///
    /// This is the set the engine would hand a plug-in that the scenario says nothing
    /// about — the common case, and the one a card's defaults have to survive.
    ///
    /// # Errors
    /// Whatever [`ParamSet::resolve`] returns for a card whose defaults contradict its own
    /// declarations.
    pub fn with_card_defaults(master_seed: u64, card: &ModelCard) -> v2xw_core::Result<ProbeCtx> {
        let mut ctx = ProbeCtx::new(master_seed);
        ctx.params = ParamSet::resolve(card, &serde_json::Value::Null)?;
        Ok(ctx)
    }

    /// Moves the clock to `t`.
    pub fn set_now(&mut self, t: SimTime) {
        self.now = t;
    }

    /// Every `(domain, entity)` key checked out so far, in call order.
    #[must_use]
    pub fn draws(&self) -> Vec<(RngDomain, EntityRef)> {
        self.draws.borrow().clone()
    }

    /// The distinct RNG domains drawn from, sorted, as their card spellings.
    #[must_use]
    pub fn domains_drawn(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .draws
            .borrow()
            .iter()
            .map(|(d, _)| d.as_str())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The distinct cached — that is, not single-use — keys drawn from.
    ///
    /// The number of these is what [`EntityRef::is_single_use`] exists to bound: a plug-in
    /// that interns a stream per transmission grows the registry without limit, and the
    /// symptom is this set growing when the same entity is exercised twice.
    #[must_use]
    pub fn cached_keys(&self) -> Vec<(RngDomain, EntityRef)> {
        let mut v: Vec<(RngDomain, EntityRef)> = self
            .draws
            .borrow()
            .iter()
            .filter(|(_, e)| !e.is_single_use())
            .copied()
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

impl Ctx for ProbeCtx {
    type World = ();
    type Actors = Vec<ActorId>;
    type Payload = ProbePayload;

    fn now(&self) -> SimTime {
        self.now
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.draws.borrow_mut().push((domain, entity));
        self.rng.checkout(domain, entity)
    }

    fn schedule(&mut self, at: SimTime, class: EventClass, payload: Self::Payload) -> EventHandle {
        self.scheduler.schedule(at, class, payload)
    }

    fn cancel(&mut self, handle: EventHandle) -> bool {
        self.scheduler.cancel(handle)
    }

    fn world(&self) -> &Self::World {
        &self.world
    }

    fn actors(&self) -> &Self::Actors {
        &self.actors
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        let mut bytes = Vec::new();
        record.write_json(&mut bytes).expect("record serialises");
        self.emitted.push((
            record.channel(),
            record.visibility(),
            String::from_utf8(bytes).expect("json is utf-8"),
        ));
    }

    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
        self.provenance.record(subject, model, params);
    }

    fn params(&self) -> &ParamSet {
        &self.params
    }
}

/// What a contributor implements to run their model through the suite.
///
/// The three required methods are the smallest description of a plug-in the suite can act
/// on: what it is, which entities it acts for, and one unit of work that produces numbers.
/// Everything else is defaulted, and every default is the conservative answer — no source
/// files means the source scans report "not checked" rather than "passed".
pub trait PluginUnderTest: Sync {
    /// The model, as the registry would hold it.
    fn model(&self) -> &dyn Model;

    /// The entities to exercise it for. Two or more make the reordering check meaningful.
    fn entities(&self) -> Vec<EntityRef>;

    /// One deterministic unit of work for `entity`, returning the numbers it produced.
    ///
    /// "Deterministic" is the property under test, so the implementation must not do
    /// anything to *make* it deterministic that the real call path would not do: draw from
    /// `ctx.rng`, read `ctx.params()`, emit records through `ctx.emit`, and return the
    /// values a caller would use.
    fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64>;

    /// The source files to scan. Empty means the source checks are skipped and reported as
    /// unchecked.
    fn sources(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// The grid every number [`PluginUnderTest::exercise`] returns must sit on
    /// (ADR 0004 decision 7, build decision D9).
    ///
    /// The default is the legacy three-decimal convention, which is the grid most exported
    /// quantities use. A model whose outputs are dB, degrees or seconds overrides it.
    fn quantum(&self) -> f64 {
        1e-3
    }

    /// True when this plug-in runs inside a node, so no record it emits may be
    /// ground-truth tainted (invariant I-C2, [`Visibility::allowed_on_node_channel`]).
    fn runs_as_node(&self) -> bool {
        false
    }
}

/// Whether one check passed, failed, or could not be run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The property holds.
    Pass,
    /// The property does not hold.
    Fail,
    /// The suite could not establish it, and says so rather than implying a pass.
    NotChecked,
}

/// One check's outcome.
#[derive(Debug, Clone)]
pub struct CheckOutcome {
    /// The check's name, as §17 names the property.
    pub name: &'static str,
    /// What happened.
    pub verdict: Verdict,
    /// The evidence, or the reason.
    pub detail: String,
}

impl CheckOutcome {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        CheckOutcome {
            name,
            verdict: Verdict::Pass,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        CheckOutcome {
            name,
            verdict: Verdict::Fail,
            detail: detail.into(),
        }
    }

    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        CheckOutcome {
            name,
            verdict: Verdict::NotChecked,
            detail: detail.into(),
        }
    }
}

/// Everything the suite established about one plug-in.
#[derive(Debug, Clone)]
pub struct Report {
    /// The model's `id@version`.
    pub model: String,
    /// One outcome per check, in a fixed order.
    pub checks: Vec<CheckOutcome>,
}

impl Report {
    /// True when no check failed. A [`Verdict::NotChecked`] does not fail the suite, but it
    /// is printed, and a caller that requires every property uses [`Report::complete`].
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.verdict != Verdict::Fail)
    }

    /// True when every check ran and passed.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.checks.iter().all(|c| c.verdict == Verdict::Pass)
    }

    /// The failing checks.
    #[must_use]
    pub fn failures(&self) -> Vec<&CheckOutcome> {
        self.checks
            .iter()
            .filter(|c| c.verdict == Verdict::Fail)
            .collect()
    }
}

impl core::fmt::Display for Report {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "plug-in conformance: {}", self.model)?;
        for c in &self.checks {
            let mark = match c.verdict {
                Verdict::Pass => "pass",
                Verdict::Fail => "FAIL",
                Verdict::NotChecked => "----",
            };
            writeln!(f, "  [{mark}] {:<24} {}", c.name, c.detail)?;
        }
        Ok(())
    }
}

/// Runs every check against `subject` and returns what it found.
///
/// `seed` is the master seed every probe context is built with; two calls with the same
/// seed must produce the same report.
#[must_use]
pub fn run_suite(subject: &dyn PluginUnderTest, seed: u64) -> Report {
    let card = subject.model().card();
    let mut checks = Vec::new();
    checks.extend(card_checks(card));
    checks.extend(behaviour_checks(subject, seed));
    checks.extend(source_checks(subject));
    Report {
        model: card.id_at_version(),
        checks,
    }
}

/// The card half of the suite: validity, sources, tiers and the determinism declaration.
#[must_use]
pub fn card_checks(card: &ModelCard) -> Vec<CheckOutcome> {
    let mut out = Vec::new();

    out.push(match card.validate() {
        Ok(()) => CheckOutcome::pass("card-validates", "id, tiers, parameters and rule R1"),
        Err(e) => CheckOutcome::fail("card-validates", e.to_string()),
    });

    out.push(match card.check_api_version() {
        Ok(()) => CheckOutcome::pass(
            "card-api-version",
            format!("targets {} and this build is compatible", card.api_version),
        ),
        Err(e) => CheckOutcome::fail("card-api-version", e.to_string()),
    });

    // "Every model card cites a source for every default; no source means todo-calibrate
    // with a plan." `validate` enforces the plan; this enforces the citation.
    let uncited: Vec<String> = card
        .parameters
        .iter()
        .filter(|p| {
            p.source.kind != SourceKind::TodoCalibrate && p.source.reference.trim().is_empty()
        })
        .map(|p| p.name.clone())
        .collect();
    out.push(if uncited.is_empty() {
        CheckOutcome::pass(
            "card-sources",
            format!(
                "{} parameter(s), {} still to calibrate",
                card.parameters.len(),
                card.todo_calibrate().count()
            ),
        )
    } else {
        CheckOutcome::fail(
            "card-sources",
            format!("no source cited for: {}", uncited.join(", ")),
        )
    });

    // §17's tier contract: a model that does not implement the top tier says what it leaves
    // out relative to the tier above it.
    out.push(
        if card.implements_tier(Tier::High) || !card.ignores.is_empty() {
            CheckOutcome::pass(
                "card-tier-contract",
                format!("tiers {:?}, {} ignores", card.tier, card.ignores.len()),
            )
        } else {
            CheckOutcome::fail(
                "card-tier-contract",
                "the card implements no high tier and lists nothing under `ignores`, so a \
             reader cannot tell what it leaves out relative to the tier above",
            )
        },
    );

    out
}

/// The behavioural half: determinism, reordering, thread independence, quantisation,
/// declared RNG domains, stream hygiene and recorder discipline.
#[must_use]
pub fn behaviour_checks(subject: &dyn PluginUnderTest, seed: u64) -> Vec<CheckOutcome> {
    let card = subject.model().card();
    let entities = subject.entities();
    let mut out = Vec::new();

    if entities.is_empty() {
        out.push(CheckOutcome::skip(
            "determinism",
            "the plug-in exercised no entity, so two identical nothings would compare equal",
        ));
        return out;
    }

    let forward = exercise_all(subject, seed, &entities);
    let again = exercise_all(subject, seed, &entities);
    out.push(if forward.values == again.values {
        CheckOutcome::pass(
            "determinism",
            format!(
                "{} value(s) identical across two runs",
                count(&forward.values)
            ),
        )
    } else {
        CheckOutcome::fail(
            "determinism",
            first_difference(&forward.values, &again.values),
        )
    });

    let mut reversed_entities = entities.clone();
    reversed_entities.reverse();
    let reversed = exercise_all(subject, seed, &reversed_entities);
    out.push(if same_per_entity(&forward.values, &reversed.values) {
        CheckOutcome::pass(
            "reordering",
            "each entity's values are the same whichever order the entities ran in",
        )
    } else {
        CheckOutcome::fail(
            "reordering",
            "an entity's values changed when the entities ran in the other order, so the \
             plug-in is carrying state between entities",
        )
    });

    let threaded = exercise_threaded(subject, seed, &entities, 8);
    out.push(if same_per_entity(&forward.values, &threaded) {
        CheckOutcome::pass(
            "thread-independence",
            "eight threads draw the same values as one",
        )
    } else {
        CheckOutcome::fail(
            "thread-independence",
            "the values depend on the thread count, which is what the per-entity stream \
             keying exists to prevent (03-interfaces.md §1.1)",
        )
    });

    let quantum = subject.quantum();
    let off_grid: Vec<f64> = forward
        .values
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .filter(|x| !is_on_grid(*x, quantum))
        .collect();
    out.push(if off_grid.is_empty() {
        CheckOutcome::pass(
            "quantisation",
            format!("every value sits on the {quantum} grid"),
        )
    } else {
        CheckOutcome::fail(
            "quantisation",
            format!(
                "{} value(s) off the {quantum} grid, first {:?} (build decision D9)",
                off_grid.len(),
                off_grid[0]
            ),
        )
    });

    let drawn = forward.domains;
    let declared: Vec<&str> = card
        .determinism
        .rng_domains
        .iter()
        .map(String::as_str)
        .collect();
    let undeclared: Vec<&str> = drawn
        .iter()
        .copied()
        .filter(|d| !declared.contains(d))
        .collect();
    out.push(if !undeclared.is_empty() {
        CheckOutcome::fail(
            "card-declares-rng",
            format!(
                "drew from {undeclared:?}, which the card's determinism.rng_domains does \
                 not list"
            ),
        )
    } else if drawn.is_empty() && card.determinism.uses_rng {
        CheckOutcome::fail(
            "card-declares-rng",
            "the card says uses_rng, and the plug-in drew nothing: one of the two is wrong",
        )
    } else if !drawn.is_empty() && !card.determinism.uses_rng {
        CheckOutcome::fail(
            "card-declares-rng",
            format!("drew from {drawn:?} with determinism.uses_rng = false"),
        )
    } else {
        CheckOutcome::pass("card-declares-rng", format!("drew from {drawn:?}"))
    });

    out.push(if forward.cached_keys <= entities.len() {
        CheckOutcome::pass(
            "stream-hygiene",
            format!(
                "{} cached stream(s) for {} entities",
                forward.cached_keys,
                entities.len()
            ),
        )
    } else {
        CheckOutcome::fail(
            "stream-hygiene",
            format!(
                "{} cached streams for {} entities: a per-call scope must use \
                 EntityRef::LinkFrame, which the registry derives and drops rather than \
                 interning",
                forward.cached_keys,
                entities.len()
            ),
        )
    });

    out.push(if !subject.runs_as_node() {
        CheckOutcome::skip(
            "recorder-discipline",
            "the plug-in does not declare itself node-hosted, so no channel restriction applies",
        )
    } else if let Some(bad) = forward
        .records
        .iter()
        .find(|(_, v)| !v.allowed_on_node_channel())
    {
        CheckOutcome::fail(
            "recorder-discipline",
            format!(
                "a node-hosted plug-in emitted a {:?} record on `{}` (invariant I-C2)",
                bad.1, bad.0
            ),
        )
    } else {
        CheckOutcome::pass(
            "recorder-discipline",
            format!(
                "{} record(s), none ground-truth tainted",
                forward.records.len()
            ),
        )
    });

    out
}

/// The source half: the three textual rules, over whatever files the plug-in names.
#[must_use]
pub fn source_checks(subject: &dyn PluginUnderTest) -> Vec<CheckOutcome> {
    let files = subject.sources();
    const NAMES: [(&str, &[Rule]); 3] = [
        ("no-owned-rng", OWNED_RNG_RULES),
        ("no-wall-clock", WALL_CLOCK_RULES),
        ("no-ground-truth", GROUND_TRUTH_RULES),
    ];
    if files.is_empty() {
        return NAMES
            .iter()
            .map(|&(name, _)| {
                CheckOutcome::skip(
                    name,
                    "the plug-in named no source files; implement PluginUnderTest::sources \
                     to have them scanned",
                )
            })
            .collect();
    }

    let mut read = Vec::new();
    for path in &files {
        match std::fs::read_to_string(path) {
            Ok(text) => read.push((path.display().to_string(), text)),
            Err(e) => {
                return vec![CheckOutcome::fail(
                    "no-owned-rng",
                    format!("could not read {}: {e}", path.display()),
                )];
            }
        }
    }

    NAMES
        .iter()
        .map(|&(name, rules)| {
            let mut found = Vec::new();
            for (file, text) in &read {
                found.extend(scan(name, file, text, rules));
            }
            if found.is_empty() {
                CheckOutcome::pass(name, format!("{} file(s) clean", read.len()))
            } else {
                CheckOutcome::fail(name, crate::firewall::report(&found))
            }
        })
        .collect()
}

/// Asserts at compile time that a model is usable the way the registry stores it.
///
/// §17 asks the suite to "build the plug-in behind `Box<dyn Family>` and
/// `Arc<dyn Model + Send + Sync>`". The `Box<dyn Family>` half belongs to the family's own
/// suite; this is the half every family shares, and it is a function rather than a test so
/// that a contributor's crate fails to compile at the seam rather than in the nine crates
/// downstream.
pub fn assert_registry_shaped(model: std::sync::Arc<dyn Model + Send + Sync>) -> String {
    let dynamic: &dyn Model = &*model;
    dynamic.id_at_version()
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// One pass over every entity.
struct Pass {
    /// `(entity, values)` in the order the entities were exercised.
    values: Vec<(EntityRef, Vec<f64>)>,
    /// The distinct RNG domains drawn from, sorted.
    domains: Vec<&'static str>,
    /// How many distinct cached (non single-use) keys were drawn from.
    cached_keys: usize,
    /// The records emitted, as `(channel, visibility)`.
    records: Vec<(&'static str, Visibility)>,
}

fn exercise_all(subject: &dyn PluginUnderTest, seed: u64, entities: &[EntityRef]) -> Pass {
    let card = subject.model().card();
    let mut ctx = ProbeCtx::with_card_defaults(seed, card).unwrap_or_else(|_| ProbeCtx::new(seed));
    let mut values = Vec::with_capacity(entities.len());
    for entity in entities {
        values.push((*entity, subject.exercise(&mut ctx, *entity)));
    }
    Pass {
        values,
        domains: ctx.domains_drawn(),
        cached_keys: ctx.cached_keys().len(),
        records: ctx.emitted.iter().map(|(c, v, _)| (*c, *v)).collect(),
    }
}

/// The same work spread over `threads` threads, each with its own probe.
///
/// Each thread gets its own [`ProbeCtx`] with the same master seed, because that is what
/// the property says: a draw is a function of `(master_seed, domain, entity)` and of
/// nothing else, so where it happened cannot matter. A shared registry would additionally
/// test the registry's locking, which `v2xw-core` already pins.
fn exercise_threaded(
    subject: &dyn PluginUnderTest,
    seed: u64,
    entities: &[EntityRef],
    threads: usize,
) -> Vec<(EntityRef, Vec<f64>)> {
    let chunk = entities.len().div_ceil(threads.max(1));
    let mut out: Vec<(EntityRef, Vec<f64>)> = Vec::with_capacity(entities.len());
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for slice in entities.chunks(chunk.max(1)) {
            handles.push(scope.spawn(move || exercise_all(subject, seed, slice).values));
        }
        for handle in handles {
            out.extend(handle.join().unwrap_or_default());
        }
    });
    out
}

fn count(values: &[(EntityRef, Vec<f64>)]) -> usize {
    values.iter().map(|(_, v)| v.len()).sum()
}

/// True when both passes produced the same values for the same entities, whatever order
/// the pairs are in.
fn same_per_entity(a: &[(EntityRef, Vec<f64>)], b: &[(EntityRef, Vec<f64>)]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().all(|(entity, values)| {
        b.iter()
            .any(|(other, other_values)| other == entity && other_values == values)
    })
}

fn first_difference(a: &[(EntityRef, Vec<f64>)], b: &[(EntityRef, Vec<f64>)]) -> String {
    for (i, (left, right)) in a.iter().zip(b.iter()).enumerate() {
        if left != right {
            return format!("run {i} differs: {left:?} against {right:?}");
        }
    }
    format!("the runs produced {} and {} results", a.len(), b.len())
}
