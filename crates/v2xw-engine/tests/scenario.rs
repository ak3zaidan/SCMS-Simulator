//! Scenario loading, merging, validating and saving.
//!
//! Each `Conflict` test names the field it expects, not just "an error": an error whose
//! message is right by accident is exactly what 03-interfaces.md §13's requirement is
//! there to prevent.

use std::path::{Path, PathBuf};

use v2xw_engine::error::ScenarioError;
use v2xw_engine::scenario::{Scenario, validate};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

/// Load, save, load: the scenario a tool writes is one the loader reads, with the same
/// content hash. A "save as" that quietly changed a run would be invisible otherwise.
#[test]
fn a_scenario_round_trips_through_load_and_save() {
    let loaded = Scenario::load(scenarios().join("grid-traffic.yaml")).expect("loads");

    let yaml = loaded.to_yaml().expect("serialises to yaml");
    let from_yaml = Scenario::parse(&yaml, None).expect("re-parses");
    assert_eq!(from_yaml, loaded, "yaml round trip changed the scenario");

    let json = loaded.to_json().expect("serialises to json");
    let from_json = Scenario::parse(&json, None).expect("re-parses json");
    assert_eq!(from_json, loaded, "json round trip changed the scenario");

    // The same scenario through two syntaxes hashes the same, because the hash is over
    // canonical JSON and not over the file's bytes.
    assert_eq!(
        from_yaml.content_hash().expect("hash"),
        from_json.content_hash().expect("hash")
    );
}

/// The `meta.base` overlay: inherited values survive, overridden ones win, and the base
/// reference is consumed.
#[test]
fn an_overlay_inherits_from_its_base_and_overrides_what_it_names() {
    let base = Scenario::load(scenarios().join("grid-base.yaml")).expect("base loads");
    let overlay = Scenario::load(scenarios().join("grid-traffic.yaml")).expect("overlay loads");

    // Inherited, and not a schema default: the base sets these and the overlay is silent.
    assert_eq!(overlay.seed, base.seed);
    assert_eq!(overlay.time.duration_s, base.time.duration_s);
    assert_eq!(overlay.nodes.default_obu, base.nodes.default_obu);
    assert_eq!(overlay.security.verification_policy, "prioritized");

    // Overridden.
    assert_eq!(overlay.meta.name, "grid-traffic");
    assert_eq!(
        overlay.actors.vehicles.demand.kind,
        "mobility/demand/poisson"
    );
    assert_eq!(base.actors.vehicles.demand.kind, "mobility/demand/none");

    // Consumed: the merged scenario does not still point at a base.
    assert_eq!(overlay.meta.base, None);
    assert_eq!(overlay.events.len(), 1);
}

/// 03-interfaces.md §13's own example, verbatim in shape: the error names the field and
/// states the conflicting value.
#[test]
fn an_invalid_scenario_produces_the_actionable_error_naming_the_field() {
    let err = Scenario::load(scenarios().join("invalid-tiers.yaml")).expect_err("must not load");
    let v2xw_engine::EngineError::Scenario(scenario_error) = err else {
        panic!("expected a scenario error, got {err}");
    };
    assert_eq!(scenario_error.field(), Some("radio.tiers.phy"));
    let message = scenario_error.to_string();
    assert!(
        message.starts_with("radio.tiers.phy: 'high' requires mac 'high' (mac is 'medium')"),
        "the message does not name the field, the rule and the conflicting value: {message}"
    );
}

/// Every rule names a dotted field path, so a UI can highlight it and a test can assert on
/// it. A rule that returned a bare sentence would pass a "returns an error" test and fail
/// the requirement.
#[test]
fn every_validation_error_names_a_field() {
    let mut s = Scenario::minimal();
    s.time.duration_s = 0.0;
    s.time.mobility_step_ms = 5;
    s.actors.vehicles.equipped_fraction = 2.0;
    s.radio.tiers.phy = v2xw_core::card::Tier::High;
    s.radio.tiers.mac = v2xw_core::card::Tier::Medium;
    s.messages.sets = vec!["bsm".into(), "bsm".into()];
    s.metrics = vec!["all".into(), "pdr".into()];
    s.nodes.default_obu = String::new();
    s.security.crypto_mode = v2xw_engine::scenario::CryptoModeSpec::Real;
    s.security.signature = "ml-dsa-65".into();

    let errors = validate(&s);
    assert!(
        errors.len() >= 8,
        "expected every rule to fire: {errors:#?}"
    );
    for e in &errors {
        let field = e.field().unwrap_or_else(|| panic!("{e} names no field"));
        assert!(!field.is_empty());
        assert!(
            e.to_string().starts_with(field),
            "the message should lead with the field: {e}"
        );
    }
    let fields: Vec<&str> = errors.iter().filter_map(ScenarioError::field).collect();
    for expected in [
        "time.duration_s",
        "time.mobility_step_ms",
        "actors.vehicles.equipped_fraction",
        "radio.tiers.phy",
        "metrics",
        "nodes.default_obu",
        "security.signature",
    ] {
        assert!(fields.contains(&expected), "no error for {expected}");
    }
}

/// The fault injection for the tier rule: with the MAC raised to `high` the same scenario
/// validates, so the check is discriminating and not merely loud.
#[test]
fn the_tier_rule_passes_when_the_conflict_is_removed() {
    let mut s = Scenario::minimal();
    s.radio.tiers.phy = v2xw_core::card::Tier::High;
    s.radio.tiers.mac = v2xw_core::card::Tier::Medium;
    assert!(
        validate(&s)
            .iter()
            .any(|e| e.field() == Some("radio.tiers.phy")),
        "the rule did not fire on the faulty scenario"
    );
    s.radio.tiers.mac = v2xw_core::card::Tier::High;
    assert!(
        !validate(&s)
            .iter()
            .any(|e| e.field() == Some("radio.tiers.phy")),
        "the rule still fires after the conflict was removed"
    );
}

/// A `param.change` whose path names nothing is caught at load, not at the instant it
/// would have fired. An unresolvable path that ran would change nothing and say nothing.
#[test]
fn a_timeline_path_that_names_nothing_is_refused_at_load() {
    let text = r#"
schema: v2xw/scenario/1
world:
  source: {kind: procedural, generator: world/source/procedural-grid, params: {}}
events:
  - t: 1.0
    type: param.change
    path: security.pseudonym_change.params.period_s
    value: 120
"#;
    let err = Scenario::parse(text, None).expect_err("must not load");
    assert!(err.to_string().contains("events[0].path"), "{err}");

    // The same scenario with the real path — the schema spells it `period_s` directly —
    // loads. This is the injected-fault counterpart: the rule distinguishes the two.
    let fixed = text.replace(
        "security.pseudonym_change.params.period_s",
        "security.pseudonym_change.period_s",
    );
    Scenario::parse(&fixed, None).expect("the corrected path loads");
}

/// A base that does not exist is a named error, not a panic and not a silent default.
#[test]
fn an_unresolvable_base_is_reported_with_the_reference_it_could_not_find() {
    let text = r#"
schema: v2xw/scenario/1
meta: {base: no-such-preset.yaml}
world:
  source: {kind: procedural, generator: world/source/procedural-grid, params: {}}
"#;
    let err = Scenario::parse(text, Some(&scenarios())).expect_err("must not load");
    assert!(err.to_string().contains("meta.base"), "{err}");
    assert!(err.to_string().contains("no-such-preset.yaml"), "{err}");
}

/// The seed accepts the three spellings §13 shows, and writes back as hex.
#[test]
fn the_seed_accepts_hex_and_decimal_and_writes_back_as_hex() {
    for (text, expected) in [
        ("seed: \"0xdead_beef\"", 0xdead_beefu64),
        ("seed: 3735928559", 0xdead_beefu64),
        ("seed: \"3735928559\"", 0xdead_beefu64),
    ] {
        let doc = format!(
            "schema: v2xw/scenario/1\n{text}\nworld:\n  source: {{kind: procedural, \
             generator: world/source/procedural-grid, params: {{}}}}\n"
        );
        let s = Scenario::parse(&doc, None).expect("loads");
        assert_eq!(s.seed, expected, "from {text}");
        assert!(s.to_yaml().expect("yaml").contains("0x00000000deadbeef"));
    }
}

/// An unknown key is refused rather than ignored: a typo in a scenario is a run that did
/// not do what its author wrote, and `deny_unknown_fields` is what turns that into a load
/// error.
#[test]
fn a_misspelled_key_is_refused_rather_than_ignored() {
    let text = r#"
schema: v2xw/scenario/1
world:
  source: {kind: procedural, generator: world/source/procedural-grid, params: {}}
time: {duration_seconds: 30}
"#;
    let err = Scenario::parse(text, None).expect_err("must not load");
    assert!(err.to_string().contains("duration_seconds"), "{err}");
}

/// A key this build cannot act on is refused, not accepted and ignored.
///
/// The vertical-slice audit's finding: six keys were validated, documented and hashed and
/// never reached the engine, so the scenario file overstated what it controlled. Those six
/// are now wired; the ones that remain genuinely unimplemented are refused here, each with
/// the field named, so that an author finds out at load rather than by reading a result
/// the file appears to explain and does not.
#[test]
fn a_key_the_engine_cannot_act_on_is_refused_and_names_itself() {
    // Each case sets exactly one thing and expects exactly that field back. A case that
    // set two would pass while one of the rules was missing.
    let cases: Vec<(&str, Box<dyn Fn(&mut Scenario)>)> = vec![
        // `radio.tiers.focus` used to be here; it is wired now (a focus region runs its
        // receivers at the focus tier), and `radio_access::a_focus_region_runs_its_
        // receivers_at_the_focus_tier` is its test. `hybrid` is the radio value that
        // remains refused: it needs a per-message policy no key states.
        (
            "radio.rat",
            Box::new(|s: &mut Scenario| {
                s.radio.rat = v2xw_engine::scenario::schema::Rat::Hybrid;
            }),
        ),
        // Pedestrians and cyclists now walk and ride; what the kernel still cannot host is
        // their device, so that is the one field refused.
        (
            "actors.vru.device_fraction",
            Box::new(|s: &mut Scenario| {
                s.actors.vru.device_fraction = 0.5;
            }),
        ),
        (
            "actors.vehicles.demand.kind",
            Box::new(|s: &mut Scenario| {
                s.actors.vehicles.demand.kind = "mobility/demand/activity-based".to_string();
            }),
        ),
        (
            "actors.vehicles.classes.pedestrian",
            Box::new(|s: &mut Scenario| {
                s.actors.vehicles.classes.insert(
                    "pedestrian".to_string(),
                    v2xw_engine::scenario::schema::VehicleClassSpec {
                        fraction: 1.0,
                        obu: None,
                    },
                );
            }),
        ),
        (
            // The engine now has an exporter stage (`v2xw_engine::export`), so the list is no
            // longer refused as a whole: an id it does not implement is, and the error names
            // the element.
            "exporters[0].id",
            Box::new(|s: &mut Scenario| {
                s.exporters = vec![v2xw_engine::scenario::ExporterSpec {
                    id: "ma-dataset-v2".to_string(),
                    opts: serde_json::Value::Null,
                }];
            }),
        ),
        // `net.layer: gn-btp` used to be here; the GeoNetworking/BTP header is now composed
        // into every frame, and `gn_btp_and_a_generator_override_validate` holds that it
        // loads. What is still refused is a fragmenter that would split, and a generator
        // this build does not run or cannot honour.
        (
            "net.fragmenter",
            Box::new(|s: &mut Scenario| {
                s.net.fragmenter = Some(v2xw_engine::scenario::ModelChoice::new(
                    v2xw_net::FRAGMENTER_GENERIC_ID,
                ));
            }),
        ),
        (
            "messages.generator.id",
            Box::new(|s: &mut Scenario| {
                s.messages.generator = Some(v2xw_engine::scenario::ModelChoice::new(
                    "generator/nonexistent",
                ));
            }),
        ),
        (
            "messages.generator.params.nominal_itt_ms",
            Box::new(|s: &mut Scenario| {
                let mut g =
                    v2xw_engine::scenario::ModelChoice::new(v2xw_msg::generator::BSM_GENERATOR_ID);
                // Faster than the nodes are stepped: no node could send that often.
                g.params = serde_json::json!({"nominal_itt_ms": 20.0});
                s.messages.generator = Some(g);
            }),
        ),
        (
            "messages.sets[0]",
            Box::new(|s: &mut Scenario| s.messages.sets = vec!["denm".to_string()]),
        ),
        // `messages.codec_tier: size-model` is acted on now (the generator sizes the
        // message instead of encoding it), so it left this list. A collective perception
        // message has no perception model to fill it and is refused by name.
        (
            "messages.sets[0]",
            Box::new(|s: &mut Scenario| s.messages.sets = vec!["cpm".to_string()]),
        ),
    ];

    for (field, apply) in cases {
        let mut s = Scenario::minimal();
        apply(&mut s);
        let errors = validate(&s);
        let fields: Vec<&str> = errors.iter().filter_map(ScenarioError::field).collect();
        assert!(
            fields.contains(&field),
            "setting {field} produced no error naming it: {errors:#?}"
        );
        for e in &errors {
            let f = e.field().unwrap_or_else(|| panic!("{e} names no field"));
            assert!(
                e.to_string().starts_with(f),
                "the message should lead with the field: {e}"
            );
        }
    }
}

/// The two keys this build now acts on load: the European network stack, and a BSM
/// generator slowed to 5 Hz. And the generator override reaches the node configuration.
#[test]
fn gn_btp_and_a_generator_override_validate() {
    let mut s = Scenario::minimal();
    s.net.layer = "gn-btp".to_string();
    let mut g = v2xw_engine::scenario::ModelChoice::new(v2xw_msg::generator::BSM_GENERATOR_ID);
    g.params = serde_json::json!({"nominal_itt_ms": 200.0, "max_itt_ms": 600.0});
    s.messages.generator = Some(g);
    let errors = validate(&s);
    assert!(errors.is_empty(), "{errors:#?}");
    let (bsm, cam) = v2xw_engine::wiring::generator_params(&s);
    assert_eq!(bsm.nominal_itt.as_nanos(), 200_000_000);
    assert_eq!(bsm.max_itt.as_nanos(), 600_000_000);
    assert_eq!(
        bsm.min_itt,
        v2xw_msg::generator::BsmGenParams::j2945_1().min_itt
    );
    assert_eq!(cam, v2xw_msg::generator::CamGenParams::en302637_2());
}

/// And the control: a scenario that sets none of them validates, so the rules above are
/// refusing the keys rather than refusing everything.
///
/// The shipped scenarios are the fixture, because they are what the rules must not break.
#[test]
fn the_shipped_scenarios_still_validate() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate is two levels below the workspace root")
        .join("scenarios");
    for name in [
        "phase1-grid.yaml",
        "phase1-manhattan.yaml",
        "phase2-manhattan.yaml",
    ] {
        let path = repo.join(name);
        if !path.exists() {
            continue;
        }
        let s = Scenario::load(&path).unwrap_or_else(|e| panic!("{name} does not load: {e}"));
        let errors = validate(&s);
        assert!(errors.is_empty(), "{name} no longer validates: {errors:#?}");
    }
    assert!(validate(&Scenario::minimal()).is_empty());
}
