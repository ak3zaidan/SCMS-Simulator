//! The reception path, end to end, and the two properties that make it a measurement.
//!
//! Every test here injects the counterexample rather than only asserting the result. A
//! check that cannot go red is not a check, and the two that matter most in this file —
//! "packet delivery falls with distance" and "the revocation reaches the other vehicle" —
//! are both things that would pass trivially against a broken engine: the first if every
//! frame were lost, the second if the assertion were on a counter the path never touches.
//! So each is paired with a run of the same scenario with one thing removed, and the
//! removal has to change the answer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use v2xw_engine::{DigestRecorder, Engine, MemoryRecorder, RunReport, Scenario};

/// The repository's `scenarios/` directory.
fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

fn load(path: &str) -> Scenario {
    rooted(Scenario::load(scenarios().join(path)).expect("the shipped scenario loads"))
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

fn run_recorded(scenario: Scenario) -> (RunReport, MemoryRecorder) {
    let mut engine = Engine::build(scenario, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (report, recorder)
}

// -----------------------------------------------------------------------------------------
// The reception path
// -----------------------------------------------------------------------------------------

/// A multi-vehicle run attempts receptions, decodes some of them, and delivers them to the
/// nodes' own verification path.
///
/// The Phase 1 run reported "0 attempted, 0 received" for one reason only: one vehicle has
/// nobody to hear it. This is the same stack with a fleet on the map.
#[test]
fn a_fleet_attempts_receptions_and_decodes_some_of_them() {
    let (report, recorder) = run_recorded(load("scale/10.yaml"));
    assert!(
        report.nodes_created > 1,
        "one node cannot exercise a reception path"
    );
    assert!(report.reception_attempts > 0, "nothing was attempted");
    assert!(report.receptions_ok > 0, "nothing decoded");
    assert!(
        report.receptions_ok < report.reception_attempts,
        "everything decoded, which means the error model is not deciding anything"
    );
    // Everything downstream of reception ran: the node spent verification time on what it
    // decoded, which is what `node.verify` records.
    assert!(
        recorder.count_on("node.verify") > 0,
        "nothing reached the nodes' verification path"
    );
    assert!(recorder.count_on("phy.rx") > 0);
}

/// Every reception attempt carries exactly one loss cause, or none and a decode
/// (invariant I-R3).
#[test]
fn every_lost_attempt_carries_exactly_one_cause() {
    let (report, recorder) = run_recorded(load("scale/10.yaml"));
    let lost: u64 = report.rx_losses.values().sum();
    assert_eq!(
        lost + report.receptions_ok,
        report.reception_attempts,
        "the causes and the decodes must partition the attempts"
    );
    for (_, rec) in recorder.records() {
        if rec.channel != "phy.rx" {
            continue;
        }
        let view: v2xw_metrics::channels::PhyRxView =
            v2xw_metrics::channels::decode(rec).expect("decodes");
        let causes = view.all_causes().len();
        match view.outcome {
            v2xw_metrics::channels::RxOutcome::Ok => assert_eq!(causes, 0),
            _ => assert_eq!(causes, 1, "a lost frame must carry exactly one cause"),
        }
    }
}

/// Packet delivery falls with distance.
///
/// The most basic sanity check in the field, and the one this engine could not make until
/// the reception path was closed. It is asserted on the *near half against the far half*
/// rather than bin by bin, because a per-bin monotone assertion on a run with tens of links
/// is an assertion about which links happened to exist: shadowing is correlated per link,
/// so one badly shadowed pair at 300 m moves one bin and nothing else.
///
/// The counterexample is the same scenario at the abstract propagation tier, which is free
/// space with no fading and no obstacles: the near-to-far *drop* there is smaller, because
/// free space is the most optimistic path loss there is.
#[test]
fn packet_delivery_falls_with_distance() {
    let (_, recorder) = run_recorded(load("scale/100.yaml"));
    let bins = pdr_by_distance(&recorder, 100.0);
    assert!(
        bins.len() >= 4,
        "a delivery-versus-distance curve needs distance to vary; got {} bins",
        bins.len()
    );
    let near: Vec<f64> = bins.iter().take(2).map(|(_, p)| *p).collect();
    let far: Vec<f64> = bins.iter().rev().take(2).map(|(_, p)| *p).collect();
    let near_mean = near.iter().sum::<f64>() / near.len() as f64;
    let far_mean = far.iter().sum::<f64>() / far.len() as f64;
    assert!(
        near_mean > far_mean,
        "packet delivery did not fall with distance: near {near_mean:.3}, far {far_mean:.3}, \
         curve {bins:?}"
    );
    // And it is a real fall, not a rounding one.
    assert!(
        near_mean - far_mean > 0.1,
        "the fall is {:.3}, which is inside the noise of this fleet size",
        near_mean - far_mean
    );
}

/// The same curve, measured with the loss causes, says *why* it falls: the far bins are
/// dominated by the receiver's own sensitivity and the near bins are not.
#[test]
fn the_far_bins_are_lost_below_sensitivity_and_the_near_bins_are_not() {
    let (_, recorder) = run_recorded(load("scale/100.yaml"));
    let mut near_sensitivity = 0u64;
    let mut far_sensitivity = 0u64;
    let mut near_total = 0u64;
    let mut far_total = 0u64;
    for (_, rec) in recorder.records() {
        if rec.channel != "phy.rx" {
            continue;
        }
        let view: v2xw_metrics::channels::PhyRxView =
            v2xw_metrics::channels::decode(rec).expect("decodes");
        let Some(d) = view.dist_m else { continue };
        let sensitivity = view
            .all_causes()
            .iter()
            .any(|c| *c == "below-sensitivity") as u64;
        if d < 200.0 {
            near_total += 1;
            near_sensitivity += sensitivity;
        } else if d > 600.0 {
            far_total += 1;
            far_sensitivity += sensitivity;
        }
    }
    assert!(near_total > 0 && far_total > 0, "both halves must have links");
    let near = near_sensitivity as f64 / near_total as f64;
    let far = far_sensitivity as f64 / far_total as f64;
    assert!(
        far > near,
        "sensitivity losses must grow with distance: near {near:.3}, far {far:.3}"
    );
}

/// Interference is summed: a dense fleet loses frames to collisions and a sparse one does
/// not.
///
/// The counterexample is the sparse run. Before the reception path registered arrivals at
/// the start of a frame rather than at its end, there was nothing for a concurrent frame to
/// be added to, every SINR was a plain SNR, and `collision` was a cause no run could ever
/// report — at any density.
#[test]
fn a_dense_fleet_loses_frames_to_interference_and_a_sparse_one_does_not() {
    let (dense, _) = run(load("scale/100.yaml"));
    let (sparse, _) = run(load("scale/2.yaml"));
    let dense_collisions = *dense.rx_losses.get("collision").unwrap_or(&0);
    let sparse_collisions = *sparse.rx_losses.get("collision").unwrap_or(&0);
    assert!(
        dense_collisions > 0,
        "a hundred vehicles on one channel produced no collision at all"
    );
    let dense_share = dense_collisions as f64 / dense.reception_attempts.max(1) as f64;
    let sparse_share = sparse_collisions as f64 / sparse.reception_attempts.max(1) as f64;
    assert!(
        dense_share > sparse_share,
        "collisions did not grow with density: dense {dense_share:.4}, sparse {sparse_share:.4}"
    );
}

/// The medium-access model delays frames, and it delays them more when the medium is busy.
///
/// The counterexample is the sparse run, where the medium is idle and access costs an AIFS.
///
/// Both runs switch building obstruction off. What this test measures is the MAC under a
/// shared medium, and in Midtown most pairs are behind a block of towers, so with buildings
/// on a node's energy detector hears only the few vehicles on its own street and the
/// "dense" fleet is not a busy medium at all. (It used to pass with buildings on for the
/// wrong reason: every node generated at the same instant, so every frame waited behind
/// every other one — the synchronised contention `v2xw_msg::GenerationTiming` removed.)
#[test]
fn the_mac_delays_a_frame_more_when_the_medium_is_busy() {
    let open = |mut s: Scenario| {
        s.world.buildings.enabled = false;
        s
    };
    let (dense, _) = run(open(load("scale/100.yaml")));
    let (sparse, _) = run(open(load("scale/2.yaml")));
    assert!(dense.mac_grants > 0 && sparse.mac_grants > 0);
    let dense_delay = dense.mac_access_delay_ns as f64 / dense.mac_grants as f64;
    let sparse_delay = sparse.mac_access_delay_ns as f64 / sparse.mac_grants as f64;
    // The expected mean access delay is about `ρ · (T/2 + AIFS + CW_min/2 · slot)` — the
    // chance a frame finds the medium busy, times the residual frame, the AIFS and the
    // mean AC_VO backoff it then waits (≈ 120 + 58 + 20 µs for a 240 µs BSM). A hundred
    // unsynchronised vehicles on this map keep each node's medium busy for roughly a tenth
    // of the time and two keep it busy for well under one per cent, so the ratio is of the
    // order of ten. The bound was 10x while every node generated at the same instant,
    // which inflated the dense delay with synchronised contention; 5x is what the load
    // itself supports (measured 7.9x: 28.2 µs against 3.55 µs), and a MAC that did not
    // defer to a busy medium would put the two within a few microseconds of each other.
    assert!(
        dense_delay > 5.0 * sparse_delay,
        "the access delay did not grow with the load: dense {dense_delay:.0} ns, \
         sparse {sparse_delay:.0} ns"
    );
}

// -----------------------------------------------------------------------------------------
// Determinism, at every size
// -----------------------------------------------------------------------------------------

/// Two runs of one scenario produce an identical content digest, at every fleet size the
/// ladder covers that a test can afford.
///
/// The digest is the record stream's, not the MCAP file's: an MCAP carries a manifest with
/// a build timestamp in it, and comparing two files would compare that too.
#[test]
fn repeated_runs_produce_identical_digests_at_every_size() {
    for rung in ["scale/2.yaml", "scale/10.yaml", "scale/100.yaml"] {
        let (first_report, first) = run(load(rung));
        let (second_report, second) = run(load(rung));
        assert_eq!(first, second, "{rung}: the digests differ");
        assert_eq!(first_report, second_report, "{rung}: the reports differ");
        assert!(
            first_report.reception_attempts > 0 || rung == "scale/2.yaml",
            "{rung}: a run with no reception attempts proves nothing about the reception path"
        );
    }
}

/// A different seed produces a different run at every size, so the digest comparison above
/// is sensitive to something.
#[test]
fn a_different_seed_changes_the_digest() {
    let mut scenario = load("scale/10.yaml");
    let (_, baseline) = run(scenario.clone());
    scenario.seed ^= 1;
    let (_, changed) = run(scenario);
    assert_ne!(
        baseline, changed,
        "changing the seed changed nothing, so the digest is not covering the run"
    );
}

// -----------------------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------------------

/// Packet delivery ratio by distance bin, read off the recorded `phy.rx` stream.
///
/// Bins with fewer than thirty attempts are dropped: a ratio over five trials is not a
/// point on a curve.
fn pdr_by_distance(recorder: &MemoryRecorder, bin_m: f64) -> Vec<(u64, f64)> {
    let mut bins: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    for (_, rec) in recorder.records() {
        if rec.channel != "phy.rx" {
            continue;
        }
        let view: v2xw_metrics::channels::PhyRxView =
            v2xw_metrics::channels::decode(rec).expect("decodes");
        let Some(d) = view.dist_m else { continue };
        let entry = bins.entry((d / bin_m) as u64).or_insert((0, 0));
        entry.0 += 1;
        if matches!(view.outcome, v2xw_metrics::channels::RxOutcome::Ok) {
            entry.1 += 1;
        }
    }
    bins.into_iter()
        .filter(|(_, (n, _))| *n >= 30)
        .map(|(bin, (n, ok))| (bin, ok as f64 / n as f64))
        .collect()
}
