//! The Phase 2 path: a report becomes a vehicle that cannot sign.
//!
//! 10-roadmap.md Phase 2 acceptance item 2 is "every stage timestamp in 05-protocols §8 is
//! emitted and the latency decomposition renders". These tests assert on the path and on
//! the decomposition, and — because the whole point of this file is a check that could
//! pass while doing nothing — each assertion is paired with the same scenario with one
//! piece removed.

use std::path::{Path, PathBuf};

use v2xw_engine::{DigestRecorder, Engine, RunReport, Scenario};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

fn phase2() -> Scenario {
    rooted(
        Scenario::load(scenarios().join("phase2-manhattan.yaml"))
            .expect("the shipped scenario loads"),
    )
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

/// The whole path, in one run.
#[test]
fn a_report_becomes_a_vehicle_that_cannot_sign() {
    let (report, _) = run(phase2());
    let p = &report.phase2;

    assert_eq!(p.rsus, 1, "the roadside unit was not created");
    assert_eq!(p.attackers, 1, "the attacker was not armed");
    assert!(
        p.falsified_claims > 0,
        "the attacker was armed and never falsified anything"
    );
    assert!(
        p.messages_checked > 0,
        "the detector suite never saw a message, so the verdict count means nothing"
    );
    assert!(p.verdicts_fired > 0, "no detector fired");
    assert!(p.reports_sent > 0, "no report went on the air");
    assert!(
        p.reports_received > 0,
        "no report crossed the backhaul to the authority"
    );
    assert!(p.cases_opened > 0, "the authority never opened a case");
    assert_eq!(p.crls_issued, 1, "the CRL Generator did not issue");
    assert!(
        p.crl_broadcasts > 0,
        "the roadside never broadcast the CRL: {} revocation(s) issued, {} of them past \
         the run horizon at a modelled latency of {} ns",
        p.crls_issued,
        p.crl_past_horizon,
        p.revocation_latency_ns
    );
    assert!(
        p.crls_installed > 0,
        "no vehicle installed the entry, so nothing enforces it"
    );
    assert!(
        p.revoked_receptions > 0,
        "the entry was installed and no reception was ever refused by it"
    );
    assert!(
        p.revocation_latency_ns > 0,
        "the revocation took no time, which is not a decomposition"
    );
}

/// The counterexample for the whole second half: without the roadside unit's `crl` role
/// nothing is broadcast, nothing is installed, and no reception is refused.
///
/// This is the check that the check can go red. Every assertion on the distribution half
/// of the path above would hold just as well against an engine that revoked the device by
/// writing to its store directly, and this is what distinguishes the two.
#[test]
fn without_the_crl_role_the_revocation_never_reaches_a_vehicle() {
    let mut scenario = phase2();
    for rsu in &mut scenario.actors.rsus {
        rsu.roles.retain(|r| r != "crl");
    }
    let (report, _) = run(scenario);
    let p = &report.phase2;
    // The first half is unchanged: the report still crosses the backhaul and the authority
    // still issues.
    assert!(p.reports_received > 0);
    assert_eq!(p.crls_issued, 1);
    // And the second half is gone.
    assert_eq!(p.crl_broadcasts, 0, "a unit with no `crl` role broadcast one");
    assert_eq!(p.crls_installed, 0);
    assert_eq!(p.revoked_receptions, 0);
}

/// Without the `report-forward` role the report never leaves the air, so the authority
/// hears nothing and nothing is issued.
#[test]
fn without_the_report_forward_role_the_authority_hears_nothing() {
    let mut scenario = phase2();
    for rsu in &mut scenario.actors.rsus {
        rsu.roles.retain(|r| r != "report-forward");
    }
    let (report, _) = run(scenario);
    let p = &report.phase2;
    assert!(
        p.reports_sent > 0,
        "the detectors still have to fire, or this proves nothing"
    );
    assert_eq!(p.reports_received, 0, "a report crossed a backhaul that is not there");
    assert_eq!(p.crls_issued, 0);
}

/// With no attacker, the path produces nothing — which is what says the detectors are
/// responding to the attack and not to the scenario.
///
/// The detectors still run and still see traffic; what changes is that there is no liar.
/// A suite that fired anyway would be reporting its own false-positive rate as a
/// revocation, so the assertion is on the *issuance* and not on the verdict count: the
/// legacy suite does have false positives on honest traffic, which
/// [`the_detector_suite_has_false_positives_on_honest_traffic`] measures rather than hides.
#[test]
fn with_no_attacker_nothing_is_revoked() {
    let mut scenario = phase2();
    scenario.threats.attackers.clear();
    let (report, _) = run(scenario);
    let p = &report.phase2;
    assert_eq!(p.attackers, 0);
    assert_eq!(p.falsified_claims, 0);
    assert!(p.messages_checked > 0, "the detectors must still be running");
    assert_eq!(
        p.crls_issued, 0,
        "a device was revoked in a run with no attacker in it"
    );
}

/// The legacy twelve-detector suite fires on honest traffic too, and this is the number.
///
/// Reported rather than asserted away: a detector suite with no false positives on a fleet
/// whose positions come from a Gauss–Markov GNSS model would be a suite that is not
/// looking at the positions. What the test pins is that the authority's two Linkage
/// Authorities are what stop a false report from revoking an innocent device — `cases_opened`
/// counts every candidate pair the authority spent lookups on, and `crls_issued` counts
/// the ones that resolved.
#[test]
fn the_detector_suite_has_false_positives_on_honest_traffic() {
    let mut scenario = phase2();
    scenario.threats.attackers.clear();
    let (report, _) = run(scenario);
    let p = &report.phase2;
    let rate = p.verdicts_fired as f64 / p.messages_checked.max(1) as f64;
    println!(
        "legacy-12 on honest traffic: {} verdicts over {} messages ({:.2} %), \
         {} reports filed, {} candidate pairs opened, {} refused by the linkage authorities",
        p.verdicts_fired,
        p.messages_checked,
        100.0 * rate,
        p.reports_sent,
        p.cases_opened,
        p.cases_unresolved
    );
    println!(
        "  of the Invalid: {} would not parse, {} parsed but failed the signature",
        p.spdu_parse_failures, p.spdu_signature_failures
    );
    for (state, n) in &p.verification_states {
        println!(
            "  state {state:24} {n:7} ({:.2} % of messages)",
            100.0 * *n as f64 / p.messages_checked.max(1) as f64
        );
    }
    let mut by: Vec<(&String, &u64)> = p.verdicts_by_detector.iter().collect();
    by.sort_by(|a, b| b.1.cmp(a.1));
    for (name, n) in by {
        println!(
            "  {name:30} {n:7} ({:.2} % of messages)",
            100.0 * *n as f64 / p.messages_checked.max(1) as f64
        );
    }
    assert_eq!(
        p.crls_issued, 0,
        "false reports revoked a device, which is what the two-authority resolution exists \
         to prevent"
    );
}

/// The Phase 2 run is deterministic: the backend, the attacker, the detectors and the
/// revocation are all keyed streams and ordered walks.
#[test]
fn the_phase_2_run_is_deterministic() {
    let (first_report, first) = run(phase2());
    let (second_report, second) = run(phase2());
    assert_eq!(first, second, "the content digests differ");
    assert_eq!(first_report, second_report, "the run reports differ");
    assert!(first_report.phase2.crls_issued > 0, "nothing happened to be deterministic about");
}

/// A role belongs to the unit that declares it, not to the scenario.
///
/// Asked of a *built* engine rather than of a run, because what is being checked is the
/// predicate and not the path: the two role decisions on this path — `rsus_with_role` and
/// `rsu_has_role` — used to ask whether **any** declared unit carried the role, which made
/// the role a property of the scenario. With one unit that is indistinguishable from the
/// right answer, and the shipped scenario declares one, so
/// `without_the_crl_role_the_revocation_never_reaches_a_vehicle` could not see it.
///
/// The second mast is the fault injection: it forwards reports and distributes nothing, and
/// the old predicate answered that it distributed.
#[test]
fn a_role_belongs_to_the_unit_that_declares_it_and_not_to_the_scenario() {
    let mut scenario = phase2();
    let profile = scenario.actors.rsus[0].profile.clone();
    scenario.actors.rsus.push(v2xw_engine::scenario::Rsu {
        site: None,
        // 500 m east of the first mast, still inside the D7 box.
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
    // A node that is not a mast carries no roadside role, whatever the scenario declared.
    assert!(!p.rsu_has_role(v2xw_core::ids::NodeId::new(9_999), "crl"));
    assert!(p.rsu_spec_of(v2xw_core::ids::NodeId::new(9_999)).is_none());
}

/// The revocation latency is the revocation's own, not the backend's whole history.
///
/// The backend keeps its own clock and every call into it runs it to quiescence
/// (`v2xw_engine::phase2`, joint 3), so the stage log's instants are contiguous only within
/// one such call: each report submission spends the deployment's report shuffle window, and
/// a vehicle that spawns in between spends a provisioning round trip. The decomposition was
/// assembled from the **first** report the authority ever received, so the number it
/// produced grew with the number of reports filed and with the size of the fleet — and the
/// engine then applied that number on its own timeline, which put the broadcast past the
/// horizon and is why the distribution half of this path never happened.
///
/// The band is what says the number is the right one. One report shuffle window is on the
/// path by construction — CAMP-EE SCMS-765's window, shortened to a minute by
/// `ScmsParams::quick` — and everything else on it is service time and link time measured
/// in milliseconds, so a latency below one window would mean the shuffle was skipped and a
/// latency above two would mean something not on this revocation's path was charged to it.
#[test]
fn the_revocation_latency_is_the_revocations_own_and_not_the_whole_backend_history() {
    let scenario = phase2();
    let horizon_ns = (scenario.time.duration_s * 1e9).round().max(0.0) as u64;
    let (report, _) = run(scenario);
    let p = &report.phase2;

    assert_eq!(
        p.crls_issued, 1,
        "nothing was revoked, so there is no latency to say anything about"
    );
    assert_eq!(
        p.crl_past_horizon, 0,
        "the revocation's latency ({} ns) did not fit inside the {horizon_ns} ns run",
        p.revocation_latency_ns
    );

    let window = v2xw_proto::ScmsParams::default()
        .quick()
        .report_shuffle_window
        .as_nanos();
    assert!(
        p.revocation_latency_ns >= window,
        "the path must include the report shuffle window: {} ns against {window} ns",
        p.revocation_latency_ns
    );
    assert!(
        p.revocation_latency_ns < 2 * window,
        "the decomposition charged this revocation for more than one report shuffle \
         window ({} ns against {window} ns), which is backend work that is not on its \
         path",
        p.revocation_latency_ns
    );
}
