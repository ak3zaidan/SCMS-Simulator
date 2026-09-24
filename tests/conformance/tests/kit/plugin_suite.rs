//! The plug-in conformance suite, run against a correct plug-in and against four broken
//! ones.
//!
//! A reference plug-in is defined here — small, correct, and written the way a contributor
//! is told to write one — and each broken variant differs from it in exactly one way. That
//! is what makes the suite evidence: every check below has been watched going red on a
//! subject that breaks it and staying green on one that does not.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use v2xw_conformance::plugin::{
    CheckOutcome, PluginUnderTest, ProbeCtx, Report, Verdict, assert_registry_shaped,
    behaviour_checks, card_checks, run_suite, source_checks,
};
use v2xw_core::card::{Determinism, Family, ModelCard, Parameter, Source, SourceKind, Tier};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::ActorId;
use v2xw_core::math::quantize_to;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};

const SEED: u64 = 0xC0FF_EE5E_ED15_EA5E;
const QUANTUM: f64 = 1e-3;

/// The card a correct plug-in ships: a cited default, declared tiers, declared `ignores`,
/// and a determinism block that names the one domain it draws from.
fn reference_card() -> ModelCard {
    let mut card = ModelCard::new(
        "conformance/reference/log-normal-shadowing",
        Family::Propagation,
        "1.0.0",
        "A reference plug-in for the conformance suite: one shadowing draw per entity.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium];
    card.ignores = vec!["small-scale fading, which the next tier up models".to_string()];
    card.parameters = vec![Parameter::new(
        "sigma_db",
        "dB",
        json!(4.0),
        Source::new(
            SourceKind::Standard,
            "ETSI TR 103 257-1 V1.1.1 §4.2, urban shadowing standard deviation",
        ),
    )];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Shadow.as_str().to_string()],
    };
    card
}

/// Four actors, which is enough for the reordering comparison to mean something.
fn entities() -> Vec<EntityRef> {
    (0..4u32)
        .map(|i| EntityRef::Actor(ActorId::new(i)))
        .collect()
}

/// One shadowing draw, quantised at the writer, keyed by the entity it is for.
fn draw(ctx: &ProbeCtx, entity: EntityRef, domain: RngDomain) -> f64 {
    let sigma = ctx.params().get_f64("sigma_db").unwrap_or(4.0);
    let raw = ctx.rng(domain, entity).normal(0.0, sigma);
    quantize_to(raw, QUANTUM)
}

/// The plug-in a contributor is told to write.
struct Reference {
    card: ModelCard,
}

impl Reference {
    fn new() -> Self {
        Reference {
            card: reference_card(),
        }
    }
}

impl Model for Reference {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl PluginUnderTest for Reference {
    fn model(&self) -> &dyn Model {
        self
    }

    fn entities(&self) -> Vec<EntityRef> {
        entities()
    }

    fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64> {
        vec![draw(ctx, entity, RngDomain::Shadow)]
    }

    fn quantum(&self) -> f64 {
        QUANTUM
    }
}

/// Carries state between entities, so the answer depends on the order they ran in.
struct CarriesState {
    card: ModelCard,
    seen: AtomicU64,
}

impl Model for CarriesState {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl PluginUnderTest for CarriesState {
    fn model(&self) -> &dyn Model {
        self
    }

    fn entities(&self) -> Vec<EntityRef> {
        entities()
    }

    fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64> {
        let n = self.seen.fetch_add(1, Ordering::SeqCst);
        // The bug: the position in the sweep leaks into the value.
        vec![draw(ctx, entity, RngDomain::Shadow) + n as f64]
    }

    fn quantum(&self) -> f64 {
        QUANTUM
    }
}

/// Draws from a domain its card does not declare.
struct UndeclaredDomain {
    card: ModelCard,
}

impl Model for UndeclaredDomain {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl PluginUnderTest for UndeclaredDomain {
    fn model(&self) -> &dyn Model {
        self
    }

    fn entities(&self) -> Vec<EntityRef> {
        entities()
    }

    fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64> {
        vec![draw(ctx, entity, RngDomain::Fading)]
    }

    fn quantum(&self) -> f64 {
        QUANTUM
    }
}

/// Returns a raw double, skipping the writer-side quantisation of ADR 0004 decision 7.
struct OffGrid {
    card: ModelCard,
}

impl Model for OffGrid {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl PluginUnderTest for OffGrid {
    fn model(&self) -> &dyn Model {
        self
    }

    fn entities(&self) -> Vec<EntityRef> {
        entities()
    }

    fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64> {
        let sigma = ctx.params().get_f64("sigma_db").unwrap_or(4.0);
        // The bug: the raw IEEE-754 value, not rounded to its field's quantum.
        vec![ctx.rng(RngDomain::Shadow, entity).normal(0.0, sigma)]
    }

    fn quantum(&self) -> f64 {
        QUANTUM
    }
}

/// The verdict of one named check.
fn verdict(report: &Report, name: &str) -> Verdict {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check named `{name}` in:\n{report}"))
        .verdict
}

/// **The correct plug-in passes every behavioural and card check.**
#[test]
fn the_reference_plugin_passes_the_suite() {
    let subject = Reference::new();
    let report = run_suite(&subject, SEED);
    assert!(report.passed(), "{report}");

    for name in [
        "card-validates",
        "card-api-version",
        "card-sources",
        "card-tier-contract",
        "determinism",
        "reordering",
        "thread-independence",
        "quantisation",
        "card-declares-rng",
        "stream-hygiene",
    ] {
        assert_eq!(verdict(&report, name), Verdict::Pass, "{name}:\n{report}");
    }

    // The three source checks report "not checked" rather than "passed", because the
    // reference plug-in names no source files. An absent check must never read as a
    // passed one.
    for name in ["no-owned-rng", "no-wall-clock", "no-ground-truth"] {
        assert_eq!(
            verdict(&report, name),
            Verdict::NotChecked,
            "an unnamed source tree must be reported as unchecked:\n{report}"
        );
    }
    assert!(
        !report.complete(),
        "`complete` must be false while three checks were skipped"
    );
    assert_eq!(
        report.model,
        "conformance/reference/log-normal-shadowing@1.0.0"
    );
}

/// **Injected fault: state carried between entities.**
///
/// The reordering check is the one §17 asks for by name, and it is the one that would
/// otherwise pass on any plug-in whose entities happen to be visited in the same order
/// twice.
#[test]
fn a_plugin_that_carries_state_between_entities_fails_the_reordering_check() {
    let subject = CarriesState {
        card: reference_card(),
        seen: AtomicU64::new(0),
    };
    let report = run_suite(&subject, SEED);
    assert_eq!(
        verdict(&report, "reordering"),
        Verdict::Fail,
        "the sweep position leaks into the value and the suite did not notice:\n{report}"
    );
    assert!(!report.passed());
}

/// **Injected fault: a draw from an undeclared RNG domain.**
#[test]
fn a_plugin_that_draws_from_an_undeclared_domain_fails() {
    let subject = UndeclaredDomain {
        card: reference_card(),
    };
    let report = run_suite(&subject, SEED);
    assert_eq!(
        verdict(&report, "card-declares-rng"),
        Verdict::Fail,
        "{report}"
    );
    let detail = &report
        .checks
        .iter()
        .find(|c| c.name == "card-declares-rng")
        .expect("the check")
        .detail;
    assert!(
        detail.contains("fading"),
        "the failure must name the domain: {detail}"
    );
    // The reordering and determinism checks still pass: the fault is in the declaration,
    // not in the behaviour, and a suite that failed everything at once would be useless
    // for locating it.
    assert_eq!(verdict(&report, "determinism"), Verdict::Pass, "{report}");
    assert_eq!(verdict(&report, "reordering"), Verdict::Pass, "{report}");
}

/// **Injected fault: an unquantised output.**
#[test]
fn a_plugin_that_emits_a_raw_double_fails_the_quantisation_check() {
    let subject = OffGrid {
        card: reference_card(),
    };
    let report = run_suite(&subject, SEED);
    assert_eq!(verdict(&report, "quantisation"), Verdict::Fail, "{report}");
    assert!(!report.passed());
}

/// **Injected fault: a plug-in that owns a generator.**
///
/// The source scan is textual, so the fault is injected as text: a file that reaches for
/// `rand::thread_rng` the way a contributor used to writing ordinary Rust would.
#[test]
fn a_plugin_whose_source_owns_a_generator_fails_the_source_scan() {
    let dir = std::env::temp_dir().join(format!("v2xw-conformance-{}-rng", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let clean = dir.join("clean.rs");
    let dirty = dir.join("dirty.rs");
    std::fs::write(
        &clean,
        "fn draw(ctx: &dyn Ctx, e: EntityRef) -> f64 {\n    \
         ctx.rng(RngDomain::Shadow, e).f64()\n}\n",
    )
    .expect("writes");
    std::fs::write(
        &dirty,
        "fn draw() -> f64 {\n    let mut g = rand::thread_rng();\n    g.r#gen()\n}\n",
    )
    .expect("writes");

    struct WithSources {
        card: ModelCard,
        files: Vec<PathBuf>,
    }
    impl Model for WithSources {
        fn card(&self) -> &ModelCard {
            &self.card
        }
    }
    impl PluginUnderTest for WithSources {
        fn model(&self) -> &dyn Model {
            self
        }
        fn entities(&self) -> Vec<EntityRef> {
            entities()
        }
        fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64> {
            vec![draw(ctx, entity, RngDomain::Shadow)]
        }
        fn sources(&self) -> Vec<PathBuf> {
            self.files.clone()
        }
        fn quantum(&self) -> f64 {
            QUANTUM
        }
    }

    // The control: the honest file alone passes the scan, so the failure below is about
    // the content and not about scanning at all.
    let honest = WithSources {
        card: reference_card(),
        files: vec![clean.clone()],
    };
    let honest_report = run_suite(&honest, SEED);
    assert_eq!(
        verdict(&honest_report, "no-owned-rng"),
        Verdict::Pass,
        "{honest_report}"
    );
    assert!(honest_report.passed(), "{honest_report}");
    for name in ["no-owned-rng", "no-wall-clock", "no-ground-truth"] {
        assert_eq!(
            verdict(&honest_report, name),
            Verdict::Pass,
            "naming source files must turn the three scans from unchecked into \
             checked:\n{honest_report}"
        );
    }

    let broken = WithSources {
        card: reference_card(),
        files: vec![clean, dirty],
    };
    let broken_report = run_suite(&broken, SEED);
    assert_eq!(
        verdict(&broken_report, "no-owned-rng"),
        Verdict::Fail,
        "{broken_report}"
    );
    assert_eq!(
        verdict(&broken_report, "no-wall-clock"),
        Verdict::Pass,
        "only the generator rule may fire:\n{broken_report}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A card that cites nothing fails, and the failure names the parameter.**
///
/// "Every model card cites a source for every default; no source means `todo-calibrate`
/// with a plan." `ModelCard::validate` enforces the plan; the suite enforces the citation,
/// which is the half a card can otherwise slip past by leaving `ref` empty.
#[test]
fn a_card_with_an_uncited_default_fails_and_names_it() {
    let mut card = reference_card();
    card.parameters.push(Parameter::new(
        "guessed_gain_db",
        "dB",
        json!(2.0),
        Source::new(SourceKind::Standard, "   "),
    ));
    let outcomes = card_checks(&card);
    let sources = outcomes
        .iter()
        .find(|c| c.name == "card-sources")
        .expect("the card-sources check");
    assert_eq!(sources.verdict, Verdict::Fail, "{}", sources.detail);
    assert!(
        sources.detail.contains("guessed_gain_db"),
        "the failure must name the parameter: {}",
        sources.detail
    );

    // A `todo-calibrate` default with a plan is allowed, and is *not* an uncited default:
    // it is an honest one with a route to a number.
    let mut honest = reference_card();
    let mut todo = Parameter::new(
        "guessed_gain_db",
        "dB",
        json!(2.0),
        Source::todo_calibrate("no measurement for this antenna yet"),
    );
    todo.calibration = Some("fit against the anechoic-chamber sweep in issue #142".to_string());
    honest.parameters.push(todo);
    let outcomes = card_checks(&honest);
    assert!(
        outcomes
            .iter()
            .all(|c: &CheckOutcome| c.verdict != Verdict::Fail),
        "a todo-calibrate default with a plan must pass: {outcomes:?}"
    );
}

/// **A card that implements no top tier and lists no `ignores` fails the tier contract.**
#[test]
fn a_card_that_says_nothing_about_what_it_leaves_out_fails_the_tier_contract() {
    let mut card = reference_card();
    card.ignores.clear();
    let outcomes = card_checks(&card);
    assert_eq!(
        outcomes
            .iter()
            .find(|c| c.name == "card-tier-contract")
            .expect("the check")
            .verdict,
        Verdict::Fail
    );

    // A model that implements the highest tier has nothing above it to leave out.
    let mut top = reference_card();
    top.ignores.clear();
    top.tier = vec![Tier::High];
    assert!(
        card_checks(&top).iter().all(|c| c.verdict != Verdict::Fail),
        "a high-tier model need not list ignores"
    );
}

/// A plug-in that draws nothing is reported as unchecked rather than as deterministic.
///
/// Two identical nothings compare equal, and a suite that called that a pass would give
/// every empty plug-in a clean bill of health.
#[test]
fn a_plugin_that_exercises_nothing_is_reported_as_unchecked() {
    struct Empty {
        card: ModelCard,
    }
    impl Model for Empty {
        fn card(&self) -> &ModelCard {
            &self.card
        }
    }
    impl PluginUnderTest for Empty {
        fn model(&self) -> &dyn Model {
            self
        }
        fn entities(&self) -> Vec<EntityRef> {
            Vec::new()
        }
        fn exercise(&self, _: &mut ProbeCtx, _: EntityRef) -> Vec<f64> {
            Vec::new()
        }
    }
    let outcomes = behaviour_checks(
        &Empty {
            card: reference_card(),
        },
        SEED,
    );
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].name, "determinism");
    assert_eq!(outcomes[0].verdict, Verdict::NotChecked);
}

/// The suite runs against a card the repository actually ships, not only against its own.
///
/// `world/source/procedural-grid` is a registered model with a real card; if the kit's card
/// rules were stricter than the project's own models can satisfy, they would be rules
/// nobody could follow.
#[test]
fn a_card_this_repository_ships_passes_the_card_checks() {
    let card = v2xw_world::procedural::card();
    let outcomes = card_checks(&card);
    let failures: Vec<&CheckOutcome> = outcomes
        .iter()
        .filter(|c| c.verdict == Verdict::Fail)
        .collect();
    assert!(
        failures.is_empty(),
        "the world generator's own card fails the kit's rules: {failures:?}"
    );
}

/// The registry shape §17 asks for: the model behind `Arc<dyn Model + Send + Sync>`.
#[test]
fn a_model_is_usable_in_the_shape_the_registry_stores_it_in() {
    let handle: std::sync::Arc<dyn Model + Send + Sync> = std::sync::Arc::new(Reference::new());
    assert_eq!(
        assert_registry_shaped(handle),
        "conformance/reference/log-normal-shadowing@1.0.0"
    );
}

/// Naming no source files is reported as three unchecked results and nothing else.
#[test]
fn an_unnamed_source_tree_is_three_unchecked_results() {
    let outcomes = source_checks(&Reference::new());
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(|c| c.verdict == Verdict::NotChecked));
    assert!(
        outcomes
            .iter()
            .all(|c| c.detail.contains("PluginUnderTest::sources")),
        "the report must say how to have the files scanned: {outcomes:?}"
    );
}
