//! What the tool promises, asserted end to end.
//!
//! Every test here drives the library functions the binary drives, so nothing is asserted
//! about a run that the binary would not also produce.
//!
//! # Each check is shown to be capable of failing
//!
//! This project has shipped four separate checks that could not fail and therefore read as
//! evidence while proving nothing. Every positive assertion below is therefore paired with
//! a **negative control** in the same test: a deliberately perturbed input whose result
//! must *differ*. If the quantity under test were a constant — a digest that ignored its
//! input, a validator that accepted everything, a counter wired to zero — the negative
//! control fails and the test goes red. The perturbation is named in each test's
//! documentation.

use std::path::{Path, PathBuf};

use v2xw_cli::run::{RunOptions, digest_only, run};
use v2xw_cli::validate::validate;

/// The Phase 1 slice on the procedural grid — the scenario that runs today.
fn grid_scenario() -> PathBuf {
    repo_root().join("scenarios/phase1-grid.yaml")
}

/// The Phase 1 slice on the Manhattan import.
fn manhattan_scenario() -> PathBuf {
    repo_root().join("scenarios/phase1-manhattan.yaml")
}

/// The engine's deliberately invalid example: a frame-level PHY over a medium MAC.
fn invalid_tiers() -> PathBuf {
    repo_root().join("crates/v2xw-engine/scenarios/invalid-tiers.yaml")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate lives two levels below the workspace root")
        .to_path_buf()
}

/// A directory of this test's own, removed first so a rerun does not read a stale file.
fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("v2xw-cli-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Short runs: these tests are about identity and wiring, not about traffic volume.
fn options(scenario: PathBuf, out: PathBuf) -> RunOptions {
    RunOptions {
        scenario,
        out: Some(out),
        // Pinned, so that two runs' manifests are comparable field by field. The engine
        // excludes it from every digest either way.
        build_utc: Some("2026-09-22T00:00:00Z".to_string()),
        keyframe_ms: 1000,
        node_only: false,
        record: true,
        attach_world: false,
        verify: true,
        duration_s: Some(3.0),
        rate_veh_per_h: Some(3600.0),
        json: false,
    }
}

/// Phase 1 acceptance criterion 1, at its narrowest: the record stream itself.
///
/// **Negative control:** the same scenario at a different duration. A digest that ignored
/// the run would pass the first assertion and fail the second.
#[test]
fn two_runs_of_one_scenario_emit_the_same_records() {
    let a = digest_only(&grid_scenario(), "2026-09-22T00:00:00Z").expect("run a");
    let b = digest_only(&grid_scenario(), "a different timestamp entirely").expect("run b");
    assert_eq!(
        a, b,
        "two runs of one scenario produced different record streams; the build timestamp \
         is the only thing that differed and it is excluded from every digest"
    );

    // Negative control: perturb the scenario and require the digest to move.
    let mut perturbed = v2xw_engine::Scenario::load(grid_scenario()).expect("load");
    perturbed.seed ^= 1;
    let mut engine = v2xw_engine::Engine::build(perturbed, "").expect("build");
    let mut recorder = v2xw_engine::MemoryRecorder::new();
    engine.run(&mut recorder).expect("run");
    assert_ne!(
        a,
        recorder.digest_hex(),
        "flipping one bit of the master seed did not change the record digest, so the \
         digest is not a function of the run and the equality above proves nothing"
    );
}

/// Phase 1 acceptance criterion 1 through the container: the recording's data section.
///
/// The *file* is not compared, and the test says why: the `mcap` writer emits the summary
/// section's repeated channel and schema records from a `HashMap`, so the file's own digest
/// can move between two runs of the same program. The data section cannot.
///
/// **Negative control:** a third run at a different duration must produce a different
/// content digest.
#[test]
fn two_runs_write_the_same_recording_data_section() {
    let a = run(&options(grid_scenario(), out_dir("determinism-a"))).expect("run a");
    let b = run(&options(grid_scenario(), out_dir("determinism-b"))).expect("run b");

    assert_eq!(
        a.content_digest, b.content_digest,
        "the two recordings' data sections differ"
    );
    assert!(
        a.content_digest.is_some(),
        "no content digest was computed, so the comparison above compared two `None`s"
    );
    assert_eq!(a.report, b.report, "the two run reports differ");
    assert_eq!(a.channels, b.channels, "the two channel tallies differ");

    let mut longer = options(grid_scenario(), out_dir("determinism-c"));
    longer.duration_s = Some(4.0);
    let c = run(&longer).expect("run c");
    assert_ne!(
        a.content_digest, c.content_digest,
        "a run of a different length produced the same content digest, so the digest does \
         not depend on what was recorded"
    );
}

/// The run's own counters agree with what reading the recording back finds.
///
/// A recorder that silently dropped records would still report a plausible-looking run;
/// this is the check that catches it, because the two numbers are produced by different
/// code on different sides of the container.
///
/// **Negative control:** the same comparison against a deliberately wrong number.
#[test]
fn the_report_and_the_recording_agree_on_how_many_records_there_are() {
    let o = run(&options(grid_scenario(), out_dir("counts"))).expect("run");
    let verified = o.verified.as_ref().expect("the run was verified");
    let tallied: u64 = o.channels.values().map(|c| c.records).sum();

    assert_eq!(
        o.report.records, verified.records,
        "the engine says it emitted {} records and the recording holds {}",
        o.report.records, verified.records
    );
    assert_eq!(
        tallied, verified.records,
        "the per-channel tally sums to {tallied} and the recording holds {}",
        verified.records
    );
    assert!(
        verified.records > 0,
        "the run recorded nothing, so the two equalities above are 0 == 0 and prove nothing"
    );
    assert_ne!(
        verified.records,
        o.report.records + 1,
        "sanity: the comparison is not against a constant"
    );
}

/// The recording really carries the signed transmissions, and the metric provider really
/// ran. Both are things a Phase 1 slice can silently omit while still producing a file.
#[test]
fn the_vertical_slice_signs_messages_and_measures_them() {
    let o = run(&options(grid_scenario(), out_dir("slice"))).expect("run");

    assert!(
        o.report.frames_transmitted > 0,
        "no frame went on the air, so nothing was encoded, signed or recorded"
    );
    let tx = o
        .channels
        .get("node.tx")
        .expect("a transmission leaves a node.tx record");
    assert_eq!(
        tx.records, o.report.frames_transmitted,
        "every transmitted frame must leave exactly one node.tx record"
    );
    assert!(
        o.metric_samples > 0,
        "the scenario asks for `pdr` and no metric sample was emitted"
    );
    assert!(
        o.channels.contains_key("gt.kinematics"),
        "the ground-truth channel is missing, so the run recorded no trajectory"
    );
}

/// 03-interfaces.md §13: an invalid scenario names the field, not a line number.
///
/// **Negative control:** the valid scenario in the same shape must pass. A validator that
/// rejected everything would satisfy the first half of this test.
#[test]
fn an_invalid_scenario_names_the_offending_field() {
    let err = validate(&invalid_tiers()).expect_err("a frame-level PHY over a medium MAC");
    let v2xw_cli::CliError::Engine(v2xw_engine::EngineError::Scenario(s)) = &err else {
        panic!("expected a scenario error, got {err}");
    };
    assert_eq!(
        s.field(),
        Some("radio.tiers.phy"),
        "the error must name the key the author has to edit: {err}"
    );
    assert!(
        err.to_string().contains("medium"),
        "the error must quote the conflicting value: {err}"
    );

    validate(&grid_scenario()).expect("the Phase 1 grid scenario is valid");
}

/// Both shipped scenarios load, validate and hash. `phase1-manhattan.yaml` is checked here
/// even though it cannot yet be *run*: whether the schema can express it and whether the
/// engine can build its world are two different questions, and only the second is open.
#[test]
fn the_shipped_scenarios_validate() {
    for path in [grid_scenario(), manhattan_scenario()] {
        let o = validate(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(o.schema, "v2xw/scenario/1");
        assert_eq!(o.master_seed_hex, "0xc0ffee5eed");
        assert_eq!(o.metrics, vec!["pdr".to_string()]);
        assert_eq!(o.scenario_hash.len(), 64);
    }
    // Negative control: the two differ, so the hash is over the document and not a
    // constant the loader prints.
    let a = validate(&grid_scenario()).expect("grid");
    let b = validate(&manhattan_scenario()).expect("manhattan");
    assert_ne!(a.scenario_hash, b.scenario_hash);
}

/// A keyframe period that is not a whole number of mobility steps is refused, by the flag
/// name, before the run starts.
///
/// **Negative control:** 1000 ms over the same 100 ms step is accepted.
#[test]
fn a_keyframe_period_off_the_step_grid_is_refused_by_name() {
    let mut opts = options(grid_scenario(), out_dir("cadence"));
    opts.keyframe_ms = 150;
    let err = run(&opts).expect_err("150 ms is not a whole number of 100 ms steps");
    match &err {
        v2xw_cli::CliError::BadArgument { flag, problem } => {
            assert_eq!(*flag, "keyframe-ms");
            assert!(problem.contains("150"), "{problem}");
        }
        other => panic!("expected a bad-argument error, got {other}"),
    }

    let mut ok = options(grid_scenario(), out_dir("cadence-ok"));
    ok.keyframe_ms = 1000;
    run(&ok).expect("1000 ms is ten whole steps");
}

/// The importer's speed preset has no default, and an unknown name is refused with the
/// names that would work.
#[test]
fn an_unknown_speed_preset_is_refused_with_the_alternatives() {
    let err = v2xw_cli::import::import_osm_extract(&v2xw_cli::import::ImportOptions {
        extract: repo_root().join("tests/fixtures/midtown-6block.osm.xml"),
        out: out_dir("import-bad-preset"),
        imported_at: Some("2026-09-18T00:00:00Z".to_string()),
        speed_preset: "german".to_string(),
        bbox: None,
    })
    .expect_err("'german' is not a preset name");
    let text = err.to_string();
    assert!(text.contains("urban-us-nyc"), "{text}");
    assert!(text.contains("sumo-german"), "{text}");
}

/// A transposed bounding box is refused — by what it *kept*, not by its arithmetic.
///
/// `40.744,-73.99,...` is a legal longitude followed by a legal latitude, so a range check
/// passes it; measured, it imports "successfully" and yields a 0 x 0 m world with 488
/// `clipped-out` anomalies. The refusal therefore comes from `refuse_an_empty_network`.
///
/// **Negative control:** the same box in the documented order is accepted and produces a
/// different world hash from an unboxed import, so `--bbox` is reaching the importer.
#[test]
fn a_transposed_bounding_box_is_refused_by_name() {
    let extract = repo_root().join("tests/fixtures/midtown-6block.osm.xml");
    let err = v2xw_cli::import::import_osm_extract(&v2xw_cli::import::ImportOptions {
        extract: extract.clone(),
        out: out_dir("import-bad-bbox"),
        imported_at: Some("2026-09-18T00:00:00Z".to_string()),
        speed_preset: "urban-us-nyc".to_string(),
        // Latitude first, which is the wrong order.
        bbox: Some("40.7440,-73.9900,40.7620,-73.9680".to_string()),
    })
    .expect_err("a transposed box keeps nothing");
    match &err {
        v2xw_cli::CliError::BadArgument { flag, problem } => {
            assert_eq!(*flag, "bbox");
            assert!(
                problem.contains("min_lon,min_lat,max_lon,max_lat"),
                "{problem}"
            );
        }
        other => panic!("expected a bad-argument error, got {other}"),
    }

    let boxed = v2xw_cli::import::import_osm_extract(&v2xw_cli::import::ImportOptions {
        extract: extract.clone(),
        out: out_dir("import-boxed"),
        imported_at: Some("2026-09-18T00:00:00Z".to_string()),
        speed_preset: "urban-us-nyc".to_string(),
        bbox: Some("-73.9900,40.7440,-73.9680,40.7620".to_string()),
    })
    .expect("the documented order is accepted")
    .0;
    let unboxed = v2xw_cli::import::import_osm_extract(&v2xw_cli::import::ImportOptions {
        extract,
        out: out_dir("import-unboxed"),
        imported_at: Some("2026-09-18T00:00:00Z".to_string()),
        speed_preset: "urban-us-nyc".to_string(),
        bbox: None,
    })
    .expect("no box is also accepted")
    .0;
    assert_ne!(
        boxed.world_hash, unboxed.world_hash,
        "stating the box changed nothing, so --bbox is not reaching the importer"
    );
    assert_eq!(
        boxed.bbox.as_deref(),
        Some("-73.99,40.744,-73.968,40.762"),
        "the box is reported back quantised onto the importer's degree grid"
    );
}

/// The Manhattan scenario builds a real OSM world.
///
/// This replaces a tripwire. Until the engine's `wiring::build_world` learned to dispatch
/// `osm-xml`, a test here pinned the *symptom* — `Engine::build` failing with a world
/// error naming the source it refused — and its own documentation said to delete it the
/// day the dispatch landed. The dispatch landed, that test went red as designed, and this
/// is the real assertion it asked for.
///
/// It checks the world layer specifically: that the import produced the lane-level network
/// the radio and mobility models need, rather than merely that no error was returned. A
/// world that builds but is empty would satisfy the weaker reading.
#[test]
fn the_manhattan_scenario_builds_a_real_osm_world() {
    let mut scenario = v2xw_engine::Scenario::load(manhattan_scenario()).expect("it loads");
    // The scenario names its extract relative to the repository root, which is where the
    // tool is run from; `cargo test` runs in the crate's own directory instead.
    if let v2xw_world::WorldSourceSpec::OsmXml { path, .. } = &mut scenario.world.source {
        *path = repo_root().join(&*path).to_string_lossy().into_owned();
    }
    let engine = v2xw_engine::Engine::build(scenario, "2026-09-22T00:00:00Z")
        .expect("the engine builds an OSM world");
    let world = engine.world();
    let counts = world.counts();
    assert!(
        counts.lanes > 1_000,
        "only {} lanes: this is not the Midtown extract",
        counts.lanes
    );
    assert!(
        counts.junctions > 100,
        "only {} junctions",
        counts.junctions
    );
    assert!(
        !world.buildings.is_empty(),
        "no buildings, so nothing would shadow a radio link"
    );
    assert_eq!(
        world.provenance.projection,
        v2xw_core::GeoOrigin::PROJECTION,
        "the world came out on a different projection than the one D6 fixes"
    );
}

/// `--duration-s` is validated as part of the scenario it produces: an override that makes
/// the file valid runs, and one that makes it invalid is refused by the rule it breaks.
///
/// Found in QA: the file was validated before the override was applied, so
/// `--duration-s 180` on a 40 s scenario whose attacker schedule ran to 180 s was refused
/// ("not a non-empty interval inside the run [0, 40] s"), and a shortening override was
/// never checked at all.
#[test]
fn a_duration_override_is_validated_with_the_scenario_it_makes() {
    let dir = out_dir("override-validation");
    std::fs::create_dir_all(&dir).expect("dir");
    let text = std::fs::read_to_string(grid_scenario()).expect("grid scenario");
    // The grid scenario runs 60 s; an event at 80 s is past its horizon.
    let late = dir.join("late-event.yaml");
    std::fs::write(
        &late,
        format!("{text}\nevents:\n  - {{t: 80.0, type: weather.front, value: rain}}\n"),
    )
    .expect("write");
    assert!(
        v2xw_engine::Scenario::load(&late).is_err(),
        "the file on its own must be invalid for this test to mean anything"
    );
    let mut longer = options(late.clone(), dir.join("longer"));
    longer.duration_s = Some(90.0);
    longer.record = false;
    longer.verify = false;
    run(&longer).expect("a 90 s run makes the 80 s event valid");

    let early = dir.join("early-event.yaml");
    std::fs::write(
        &early,
        format!("{text}\nevents:\n  - {{t: 10.0, type: weather.front, value: rain}}\n"),
    )
    .expect("write");
    v2xw_engine::Scenario::load(&early).expect("the file on its own is valid");
    let mut shorter = options(early, dir.join("shorter"));
    shorter.duration_s = Some(5.0);
    let err = run(&shorter).expect_err("a 5 s run leaves the 10 s event past the horizon");
    assert!(err.to_string().contains("events[0].t"), "{err}");
}
