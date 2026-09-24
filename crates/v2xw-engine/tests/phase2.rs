//! The Phase 2 path: a report becomes a vehicle that cannot sign.
//!
//! 10-roadmap.md Phase 2 acceptance item 2 is "every stage timestamp in 05-protocols §8 is
//! emitted and the latency decomposition renders". These tests assert on the path and on
//! the decomposition, and — because the whole point of this file is a check that could
//! pass while doing nothing — each assertion is paired with the same scenario with one
//! piece removed.
//!
//! # The scenario the tests run
//!
//! `phase2-manhattan.yaml` places a roadside unit at every signalised intersection of 5th
//! and 6th Avenues (55 units) with building obstruction on. Every unit is a receiver of
//! every frame in range, and a debug build spends most of a run's time in the reception
//! phase, so the tests thin the deployment to every third unit on each avenue (the path is
//! the same one; only the number of masts differs) and shorten nothing else.
//! [`the_shipped_scenario_builds_with_its_whole_deployment`] builds the shipped file as it
//! is, so the thinning cannot hide a scenario that does not load.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::json;
use v2xw_engine::scenario::ModelChoice;
use v2xw_engine::{DigestRecorder, Engine, RunReport, Scenario};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

/// The shipped scenario, rooted.
fn shipped() -> Scenario {
    rooted(
        Scenario::load(scenarios().join("phase2-manhattan.yaml"))
            .expect("the shipped scenario loads"),
    )
}

/// The shipped scenario with every third roadside unit kept (see the module docs).
fn phase2() -> Scenario {
    let mut s = shipped();
    s.actors.rsus = s
        .actors
        .rsus
        .into_iter()
        .enumerate()
        .filter(|(i, _)| i % 3 == 0)
        .map(|(_, r)| r)
        .collect();
    s
}

/// Makes a scenario's world source path absolute.
///
/// A scenario's paths are relative to the repository root, because that is where a user
/// runs `v2xw` from; a test's working directory is its crate. Rewriting the path here
/// rather than changing the process's directory keeps the tests runnable in parallel,
/// which `cargo test` does by default.
fn rooted(mut scenario: Scenario) -> Scenario {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    if let v2xw_world::WorldSourceSpec::OsmXml { path, .. } = &mut scenario.world.source
        && Path::new(path).is_relative()
    {
        *path = root.join(&*path).to_string_lossy().into_owned();
    }
    scenario
}

fn run(scenario: Scenario) -> (RunReport, String) {
    let mut engine = Engine::build(scenario, "").expect("builds");
    let mut recorder = DigestRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (report, recorder.digest_hex())
}

/// The path run, once for every test in this binary that reads it.
fn path_run() -> &'static (RunReport, String) {
    static RUN: OnceLock<(RunReport, String)> = OnceLock::new();
    RUN.get_or_init(|| run(phase2()))
}

/// Sets one lifecycle parameter over the shipped ones.
fn lifecycle(s: &mut Scenario, key: &str, value: f64) {
    let choice = s.security.protocol.get_or_insert_with(|| ModelChoice {
        id: v2xw_engine::phase2::CAMP_SCMS.to_string(),
        params: json!({}),
    });
    if !choice.params.is_object() {
        choice.params = json!({});
    }
    choice.params[key] = json!(value);
}

/// The shipped file loads and builds with its whole deployment.
#[test]
fn the_shipped_scenario_builds_with_its_whole_deployment() {
    let s = shipped();
    assert!(v2xw_engine::scenario::validate::validate(&s).is_empty());
    assert_eq!(s.actors.rsus.len(), 55);
    assert!(s.world.buildings.enabled, "building obstruction must be on");
    let engine = Engine::build(s, "").expect("builds");
    let p = engine.phase2().expect("the Phase 2 path is declared");
    assert_eq!(p.rsu_nodes().len(), 55);
}

/// The whole path, in one run.
#[test]
fn a_report_becomes_a_vehicle_that_cannot_sign() {
    let (report, _) = path_run();
    let p = &report.phase2;
    println!("{}", serde_json::to_string_pretty(p).unwrap_or_default());

    assert_eq!(p.backend_errors, 0, "the backend refused: {}", p.first_backend_error);
    assert!(p.rsus > 1, "the roadside units were not created");
    assert_eq!(p.attackers, 1, "the attacker was not armed");
    assert!(p.falsified_claims > 0, "the attacker never falsified anything");
    assert!(p.messages_checked > 0, "the detector suite never saw a message");
    assert!(p.verdicts_fired > 0, "no detector fired");
    assert!(p.reports_sent > 0, "no report was filed");
    assert!(
        p.reports_uploaded_cellular > 0,
        "no report crossed a cellular uplink, which is how this fleet reaches the backend"
    );
    assert!(p.reports_received > 0, "no report reached the privacy proxy");
    assert!(p.reports_at_ma > 0, "no report came through the RA's shuffle to the authority");
    assert!(p.ma_revoke_decisions > 0, "the authority never decided");
    assert!(p.cases_opened > 0, "the backend never opened a case");
    assert!(p.crls_issued >= 1, "the CRL Generator did not issue");
    assert!(p.crl_versions_published >= 1, "the CRL Store never published");
    assert!(
        p.crl_broadcasts > 0 || p.crl_downloads > 0,
        "the list was published and neither path distributed it"
    );
    assert!(p.crls_installed > 0, "no vehicle installed the entry");
    assert!(
        p.revoked_receptions > 0,
        "the entry was installed and no reception was ever refused by it"
    );
    assert!(p.revocation_latency_ns > 0);
}

/// The decomposition is the revocation's own: every stage of 05-protocols §8 the path
/// passes through is present and in order, the RA's report shuffle is on it, and the CRL
/// was published on the Generator's cadence.
#[test]
fn the_revocation_latency_is_decomposed_by_stage() {
    let scenario = phase2();
    let horizon_ns = (scenario.time.duration_s * 1e9).round() as u64;
    let (report, _) = path_run();
    let p = &report.phase2;
    let stages: Vec<(&str, u64)> =
        p.revocation_stages.iter().map(|(s, t)| (s.as_str(), *t)).collect();
    println!("{stages:?}");
    let at = |name: &str| stages.iter().find(|(s, _)| *s == name).map(|(_, t)| *t);
    for name in [
        "detect",
        "report_sent",
        "shuffled",
        "report_received",
        "decision",
        "resolved",
        "blocklisted",
        "issued",
        "published",
        "enforced",
    ] {
        assert!(at(name).is_some(), "stage {name} missing from {stages:?}");
    }
    for w in stages.windows(2) {
        assert!(w[0].1 <= w[1].1, "stages out of order: {stages:?}");
    }
    // The report waited in the RA's shuffle: the window is a minute in this scenario.
    let shuffled = at("shuffled").unwrap() - at("report_sent").unwrap();
    assert!(shuffled > 0, "the report skipped the RA's shuffle");
    assert!(shuffled <= 61_000_000_000, "the shuffle took longer than its window");
    // The entry waited for the Generator's 60 s cadence boundary.
    assert!(at("published").unwrap() >= at("issued").unwrap());
    assert!(p.revocation_latency_ns < horizon_ns);
    assert_eq!(p.crl_past_horizon, 0);
}

/// With no attacker, nothing is revoked, at the legacy suite's default thresholds and the
/// legacy authority's default gate. The suite still fires on honest traffic — that is
/// printed, not hidden — and the gate is what holds.
#[test]
fn with_no_attacker_nothing_is_revoked() {
    let mut scenario = phase2();
    scenario.threats.attackers.clear();
    let (report, _) = run(scenario);
    let p = &report.phase2;
    let rate = p.verdicts_fired as f64 / p.messages_checked.max(1) as f64;
    println!(
        "legacy-12 on honest traffic: {} verdicts over {} messages ({:.2} %), {} reports, \
         {} at the authority, {} revoke decisions",
        p.verdicts_fired,
        p.messages_checked,
        100.0 * rate,
        p.reports_sent,
        p.reports_at_ma,
        p.ma_revoke_decisions
    );
    let mut by: Vec<(&String, &u64)> = p.verdicts_by_detector.iter().collect();
    by.sort_by(|a, b| b.1.cmp(a.1));
    for (name, n) in by {
        println!(
            "  {name:30} {n:7} ({:.2} % of messages)",
            100.0 * *n as f64 / p.messages_checked.max(1) as f64
        );
    }
    assert_eq!(p.attackers, 0);
    assert_eq!(p.falsified_claims, 0);
    assert!(p.messages_checked > 0, "the detectors must still be running");
    assert_eq!(
        p.ma_revoke_decisions, 0,
        "the authority decided to revoke a device in a run with no attacker in it"
    );
    assert_eq!(p.crls_issued, 0, "a device was revoked in a run with no attacker in it");
}

/// Without the `crl` role, and with the cellular poll pushed past the horizon, the list is
/// published and reaches nobody: the two distribution paths are the only ways in.
#[test]
fn without_either_distribution_path_the_revocation_never_reaches_a_vehicle() {
    let mut scenario = phase2();
    for rsu in &mut scenario.actors.rsus {
        rsu.roles.retain(|r| r != "crl");
    }
    lifecycle(&mut scenario, "crl_fetch_interval_s", 1.0e6);
    let (report, _) = run(scenario);
    let p = &report.phase2;
    // The first half is unchanged.
    assert!(p.reports_received > 0);
    assert!(p.crls_issued >= 1, "the authority must still issue");
    // And the second half is gone.
    assert_eq!(p.crl_broadcasts, 0, "a unit with no `crl` role broadcast one");
    assert_eq!(p.crl_downloads, 0, "a vehicle polled past its interval");
    assert_eq!(p.crls_installed, 0);
    assert_eq!(p.revoked_receptions, 0);
}

/// A fleet with no modem reaches the backend only through a unit that relays reports; with
/// the `report-forward` role removed, the authority hears nothing and the reports wait in
/// the vehicles' outboxes.
#[test]
fn without_a_modem_or_a_relay_the_authority_hears_nothing() {
    let mut scenario = phase2();
    scenario.net.uu = None;
    for rsu in &mut scenario.actors.rsus {
        rsu.roles.retain(|r| r != "report-forward");
    }
    let (report, _) = run(scenario);
    let p = &report.phase2;
    assert!(p.reports_sent > 0, "the detectors still have to fire, or this proves nothing");
    assert_eq!(p.reports_uploaded_cellular, 0);
    assert_eq!(p.reports_received, 0, "a report reached the backend over nothing");
    assert!(p.reports_unsent > 0, "the reports must be held, not dropped");
    assert_eq!(p.crls_issued, 0);
}

/// The Phase 2 run is deterministic.
#[test]
fn the_phase_2_run_is_deterministic() {
    let (first_report, first) = path_run();
    let (second_report, second) = run(phase2());
    assert_eq!(first, &second, "the content digests differ");
    assert_eq!(first_report, &second_report, "the run reports differ");
    assert!(
        first_report.phase2.crls_issued > 0,
        "nothing happened to be deterministic about"
    );
}

/// A role belongs to the unit that declares it, not to the scenario.
#[test]
fn a_role_belongs_to_the_unit_that_declares_it_and_not_to_the_scenario() {
    let mut scenario = phase2();
    scenario.actors.rsus.truncate(1);
    let profile = scenario.actors.rsus[0].profile.clone();
    scenario.actors.rsus.push(v2xw_engine::scenario::Rsu {
        site: None,
        position_m: Some([1400.0, 1000.0, 0.0]),
        roles: vec!["report-forward".to_string()],
        profile,
        backhaul: None,
    });
    let engine = Engine::build(scenario, "").expect("builds");
    let p = engine
        .phase2()
        .expect("a scenario with roadside units declares the Phase 2 path");
    let nodes = p.rsu_nodes().to_vec();
    assert_eq!(nodes.len(), 2, "both masts must have been created as nodes");

    assert_eq!(
        p.rsus_with_role("crl"),
        vec![nodes[0]],
        "only the unit that declares `crl` distributes a revocation list"
    );
    assert_eq!(
        p.rsus_with_role("report-forward"),
        nodes,
        "both units declare `report-forward`"
    );
    assert!(p.rsu_has_role(nodes[1], "report-forward"));
    assert!(
        !p.rsu_has_role(nodes[1], "crl"),
        "the second unit does not declare `crl` and must not be treated as if it did"
    );
    assert!(!p.rsu_has_role(v2xw_core::ids::NodeId::new(9_999), "crl"));
    assert!(p.rsu_spec_of(v2xw_core::ids::NodeId::new(9_999)).is_none());
}

#[test]
#[ignore = "diagnostic"]
fn diag_print_report() {
    let mut s = if std::env::var("FULL").is_ok() { shipped() } else { phase2() };
    if std::env::var("NO_ATTACKER").is_ok() {
        s.threats.attackers.clear();
    }
    let (report, _) = run(s);
    println!("{}", serde_json::to_string_pretty(&report.phase2).unwrap());
}
