//! The radio access path: generation timing, the technology the scenario selects, and
//! the channel models it composes.
//!
//! Each property is shown against its counterexample in the same file: a check that
//! passes against the defect it guards is not a check.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use v2xw_engine::{Engine, MemoryRecorder, RunReport, Scenario};
use v2xw_metrics::channels::NodeTxView;

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

fn rooted(mut scenario: Scenario) -> Scenario {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    if let v2xw_world::WorldSourceSpec::OsmXml { path, .. } = &mut scenario.world.source
        && Path::new(path).is_relative()
    {
        *path = root.join(&*path).to_string_lossy().into_owned();
    }
    scenario
}

/// The scaling ladder's Manhattan base with a bulk-spawned fleet of `n`, for `secs`.
fn manhattan_fleet(n: u32, secs: f64) -> Scenario {
    let mut s = rooted(
        Scenario::load(scenarios().join("scale/base.yaml")).expect("the shipped scenario loads"),
    );
    s.time.duration_s = secs;
    s.actors.vehicles.demand.params = serde_json::json!({ "max_total_vehicles": n });
    s
}

fn run_recorded(scenario: Scenario) -> (RunReport, MemoryRecorder) {
    let mut engine = Engine::build(scenario, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (report, recorder)
}

fn views<V: v2xw_metrics::channels::ChannelView + serde::de::DeserializeOwned>(
    recorder: &MemoryRecorder,
) -> Vec<V> {
    recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == V::CHANNEL)
        .map(|(_, r)| v2xw_metrics::channels::decode(r).expect("decodes"))
        .collect()
}

/// The 10 ms bins of `t_generated mod 100 ms` that any transmission fell in.
fn phase_bins(recorder: &MemoryRecorder) -> BTreeSet<u64> {
    views::<NodeTxView>(recorder)
        .iter()
        .filter_map(|v| v.t_generated)
        .map(|t| (t % 100_000_000) / 10_000_000)
        .collect()
}

fn loss_fraction(report: &RunReport, cause: &str) -> f64 {
    report.rx_losses.get(cause).copied().unwrap_or(0) as f64
        / report.reception_attempts.max(1) as f64
}

// -----------------------------------------------------------------------------------------
// Generation timing
// -----------------------------------------------------------------------------------------

/// Each node generates at its own phase, so the fleet is not synchronised to the engine's
/// step grid, and the synchronised contention that grid caused is gone.
///
/// The counterexample is the same fleet with `phase_window_ms: 0, max_jitter_ms: 0` —
/// the engine's behaviour before the timing model — which puts every generation instant
/// at phase 0 and loses a large share of receptions to collision and half duplex.
#[test]
fn generation_is_desynchronised_and_that_removes_the_synchronised_collisions() {
    let fleet = open_air(manhattan_fleet(20, 3.0));
    let (desync, desync_rec) = run_recorded(fleet.clone());

    let mut synced = fleet;
    synced.messages.generator = Some(v2xw_engine::scenario::schema::ModelChoice {
        id: v2xw_msg::GENERATION_TIMING_ID.to_string(),
        params: serde_json::json!({ "phase_window_ms": 0, "max_jitter_ms": 0 }),
    });
    let (sync, sync_rec) = run_recorded(synced);

    let desync_bins = phase_bins(&desync_rec);
    let sync_bins = phase_bins(&sync_rec);
    assert_eq!(
        sync_bins.len(),
        1,
        "the synchronised counterexample should put every generation at one phase"
    );
    assert!(
        desync_bins.len() >= 6,
        "generation instants fall in only {} of ten 10 ms phase bins",
        desync_bins.len()
    );
    assert!(desync.reception_attempts > 500 && sync.reception_attempts > 500);

    let contention =
        |r: &RunReport| loss_fraction(r, "collision") + loss_fraction(r, "half-duplex");
    assert!(
        contention(&sync) > 0.10,
        "the synchronised fleet lost only {:.3} to contention, so this fleet cannot show \
         the difference",
        contention(&sync)
    );
    assert!(
        contention(&desync) < 0.03,
        "the desynchronised fleet still loses {:.3} of attempts to contention",
        contention(&desync)
    );
}

// -----------------------------------------------------------------------------------------
// Shared helpers for the access-layer tests
// -----------------------------------------------------------------------------------------

/// Delivery over the attempts whose distance falls in `[lo, hi)` metres, or `None` when
/// there were none.
fn pdr_between(recorder: &MemoryRecorder, lo: f64, hi: f64) -> Option<f64> {
    let rx = views::<v2xw_metrics::channels::PhyRxView>(recorder);
    let mut n = 0u64;
    let mut ok = 0u64;
    for v in rx {
        let Some(d) = v.dist_m else { continue };
        if d >= lo && d < hi {
            n += 1;
            if v.outcome == v2xw_metrics::channels::RxOutcome::Ok {
                ok += 1;
            }
        }
    }
    (n > 0).then(|| ok as f64 / n as f64)
}

fn with_rat(mut s: Scenario, rat: &str) -> Scenario {
    s.radio.rat = serde_json::from_value(serde_json::json!(rat)).expect("a rat");
    s
}

/// Buildings off: every link line of sight, so the medium is genuinely shared.
fn open_air(mut s: Scenario) -> Scenario {
    s.world.buildings.enabled = false;
    s
}

fn mean_access_delay_ns(r: &RunReport) -> f64 {
    r.mac_access_delay_ns as f64 / r.mac_grants.max(1) as f64
}

// -----------------------------------------------------------------------------------------
// radio.rat
// -----------------------------------------------------------------------------------------

/// `radio.rat` selects a genuinely different access layer, and each one leaves the
/// fingerprint of its own standard in the recording.
///
/// * 802.11p: channel 172, air time set by the frame's length (a 150-250 B BSM at
///   6 Mbit/s is 200-400 µs), CSMA/CA access in microseconds, no sidelink report.
/// * LTE-V2X Mode 4: channel 183, every transport block exactly one 1 ms subframe, an SPS
///   reservation 1-100 subframes after the packet arrives — so tens of milliseconds of
///   access delay — and selections recorded by trigger.
/// * NR-V2X Mode 2: the same channel, a 0.5 ms slot at 30 kHz, and a selection window
///   bounded by T2 = 33 slots, so markedly less access delay than LTE's.
///
/// The counterexample is the engine before `radio.rat` was wired: all three would be the
/// 802.11p run, and every assertion that separates them would fail.
#[test]
fn each_radio_technology_runs_its_own_access_layer() {
    let fleet = open_air(manhattan_fleet(30, 3.0));
    let (dsrc, dsrc_rec) = run_recorded(with_rat(fleet.clone(), "dsrc-80211p"));
    let (lte, lte_rec) = run_recorded(with_rat(fleet.clone(), "lte-v2x-pc5"));
    let (nr, nr_rec) = run_recorded(with_rat(fleet, "nr-v2x-pc5"));

    let channels = |r: &MemoryRecorder| -> BTreeSet<u16> {
        views::<NodeTxView>(r)
            .iter()
            .filter_map(|v| v.channel)
            .collect()
    };
    let airtimes = |r: &MemoryRecorder| -> BTreeSet<u64> {
        views::<NodeTxView>(r)
            .iter()
            .filter_map(|v| v.airtime_us)
            .collect()
    };
    assert_eq!(channels(&dsrc_rec), BTreeSet::from([172]));
    assert_eq!(channels(&lte_rec), BTreeSet::from([183]));
    assert_eq!(channels(&nr_rec), BTreeSet::from([183]));
    let dsrc_air = airtimes(&dsrc_rec);
    assert!(
        dsrc_air.iter().all(|a| (150..600).contains(a)) && dsrc_air.len() > 1,
        "802.11p air time follows the frame length: {dsrc_air:?}"
    );
    assert_eq!(
        airtimes(&lte_rec),
        BTreeSet::from([1000]),
        "one LTE subframe"
    );
    assert_eq!(
        airtimes(&nr_rec),
        BTreeSet::from([500]),
        "one NR slot at 30 kHz"
    );

    assert!(dsrc.sidelink.is_none());
    let lte_sl = lte
        .sidelink
        .as_ref()
        .expect("an LTE run reports its sidelink");
    let nr_sl = nr
        .sidelink
        .as_ref()
        .expect("an NR run reports its sidelink");
    assert_eq!(lte_sl.rat, "lte-v2x-mode4");
    assert_eq!(nr_sl.rat, "nr-v2x-mode2");
    assert!(lte_sl.selections.values().sum::<u64>() > 0 && lte_sl.grants > 0);
    // A BSM that attaches its certificate once a second outgrows a one-sub-channel
    // reservation, which TS 36.321 §5.14.1.1 answers with a reselection.
    assert!(lte_sl.selections.get("size-change").copied().unwrap_or(0) > 0);
    // Rel-16 only: LTE has no pre-emption and no re-evaluation.
    assert!(!lte_sl.selections.contains_key("preemption"));
    assert!(!lte_sl.selections.contains_key("reevaluation"));

    // Access delay: microseconds of CSMA against a reservation tens of milliseconds out.
    let (d, l, n) = (
        mean_access_delay_ns(&dsrc),
        mean_access_delay_ns(&lte),
        mean_access_delay_ns(&nr),
    );
    assert!(d < 1.0e6, "802.11p access delay {d} ns");
    assert!(
        (10.0e6..100.0e6).contains(&l),
        "LTE SPS access delay {l} ns"
    );
    assert!(
        n < l,
        "NR's 16.5 ms selection window gives less delay than LTE's: {n} vs {l}"
    );

    // The sidelinks lose frames to causes 802.11p has no word for.
    let sl_causes: BTreeSet<&String> = lte.rx_losses.keys().chain(nr.rx_losses.keys()).collect();
    assert!(
        sl_causes
            .iter()
            .any(|c| *c == "resource-collision" || *c == "in-band-emission"),
        "no SPS-specific loss in either sidelink run: {sl_causes:?}"
    );
    assert!(!dsrc.rx_losses.contains_key("resource-collision"));
    assert!(!dsrc.rx_losses.contains_key("in-band-emission"));
}

// -----------------------------------------------------------------------------------------
// world.buildings.enabled
// -----------------------------------------------------------------------------------------

/// Buildings obstruct: in Midtown, a pair a few hundred metres apart is almost always
/// behind a block of towers, and delivery there collapses; with obstruction switched off
/// the same pairs are line of sight and nearly all deliver. Pairs within 100 m — mostly the
/// same street — are unaffected either way.
#[test]
fn buildings_obstruct_links_and_the_switch_turns_them_off() {
    // Sixty trips, of which the lane-insertion gap realises about 39 vehicles. With thirty
    // (18 realised) the traffic track's corrected Manhattan world put no two vehicles
    // within 100 m of each other, so the near band had no links to measure at all.
    let fleet = manhattan_fleet(60, 2.0);
    let (_, city) = run_recorded(fleet.clone());
    let (_, open) = run_recorded(open_air(fleet));
    let city_mid = pdr_between(&city, 200.0, 500.0).expect("links at 200-500 m");
    let open_mid = pdr_between(&open, 200.0, 500.0).expect("links at 200-500 m");
    assert!(
        open_mid > 0.9,
        "open-air delivery at 200-500 m is {open_mid:.3}"
    );
    assert!(
        city_mid < 0.5 * open_mid,
        "buildings changed 200-500 m delivery only from {open_mid:.3} to {city_mid:.3}"
    );
    let city_near = pdr_between(&city, 0.0, 100.0).expect("links within 100 m");
    assert!(
        city_near > 0.7,
        "same-street delivery within 100 m is {city_near:.3}"
    );
}

// -----------------------------------------------------------------------------------------
// radio.models
// -----------------------------------------------------------------------------------------

/// `radio.models` reaches the run: free-space propagation with no fading delivers further
/// than the default dual-slope law with Nakagami fading, and an unknown model is refused
/// at load rather than silently replaced by the default.
#[test]
fn radio_models_select_the_propagation_and_fading_and_refuse_unknown_ids() {
    let base = open_air(manhattan_fleet(20, 2.0));
    let mut free = base.clone();
    free.radio.models.insert(
        "propagation".to_string(),
        v2xw_engine::scenario::schema::ModelChoice::new("propagation/free-space"),
    );
    free.radio.models.insert(
        "fading".to_string(),
        v2xw_engine::scenario::schema::ModelChoice::new("fading/none"),
    );
    let (_, default_rec) = run_recorded(base.clone());
    let (_, free_rec) = run_recorded(free);
    let far_default = pdr_between(&default_rec, 700.0, 1000.0).expect("far links");
    let far_free = pdr_between(&free_rec, 700.0, 1000.0).expect("far links");
    assert!(
        far_free > far_default + 0.05,
        "free space at 700-1000 m delivered {far_free:.3} against {far_default:.3}"
    );

    let mut bad = base;
    bad.radio.models.insert(
        "propagation".to_string(),
        v2xw_engine::scenario::schema::ModelChoice::new("propagation/winner-plus-b1"),
    );
    let errors = v2xw_engine::scenario::validate(&bad);
    assert!(
        errors
            .iter()
            .any(|e| e.to_string().contains("radio.models.propagation")),
        "an unshipped propagation model loaded: {errors:?}"
    );
}

// -----------------------------------------------------------------------------------------
// threats.jammers
// -----------------------------------------------------------------------------------------

fn jammer(
    id: &str,
    at: [f64; 2],
    extra: serde_json::Value,
) -> v2xw_engine::scenario::schema::ModelChoice {
    let mut params = serde_json::json!({ "position_m": at, "power_dbm": 23.0 });
    if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            p.insert(k.clone(), v.clone());
        }
    }
    v2xw_engine::scenario::schema::ModelChoice {
        id: id.to_string(),
        params,
    }
}

/// A constant jammer in the middle of the fleet costs frames, and the frames it costs are
/// reported as `jammed` rather than as collisions; a jammer on the sidelink does the same,
/// and a reactive one too. The counterexample is the same fleet with no jammer, which
/// reports none.
#[test]
fn a_jammer_costs_frames_and_they_are_reported_as_jammed() {
    let base = open_air(manhattan_fleet(20, 2.0));
    let world = v2xw_engine::wiring::build_world(&base).expect("the world builds");
    let centre = [
        (world.bbox.min.x + world.bbox.max.x) * 0.5,
        (world.bbox.min.y + world.bbox.max.y) * 0.5,
    ];
    let (quiet, _) = run_recorded(base.clone());
    assert_eq!(quiet.rx_losses.get("jammed"), None);

    let mut jammed = base.clone();
    jammed.threats.jammers = vec![jammer(
        "attacker/jammer/constant",
        centre,
        serde_json::json!({}),
    )];
    assert!(v2xw_engine::scenario::validate(&jammed).is_empty());
    let (dsrc, _) = run_recorded(jammed.clone());
    assert!(
        dsrc.rx_losses.get("jammed").copied().unwrap_or(0) > 0,
        "a constant jammer killed nothing: {:?}",
        dsrc.rx_losses
    );
    assert!(
        dsrc.receptions_ok < quiet.receptions_ok,
        "the jammer did not reduce delivery: {} against {}",
        dsrc.receptions_ok,
        quiet.receptions_ok
    );

    let (lte, _) = run_recorded(with_rat(jammed, "lte-v2x-pc5"));
    assert!(
        lte.rx_losses.get("jammed").copied().unwrap_or(0) > 0,
        "the sidelink run attributed nothing to the jammer: {:?}",
        lte.rx_losses
    );

    let mut reactive = base;
    reactive.threats.jammers = vec![jammer(
        "attacker/jammer/reactive",
        centre,
        serde_json::json!({}),
    )];
    let (r, _) = run_recorded(reactive);
    assert!(
        r.rx_losses.get("jammed").copied().unwrap_or(0) > 0,
        "a reactive jammer in the middle of the fleet killed nothing: {:?}",
        r.rx_losses
    );

    // And a jammer the loader cannot place is refused, not ignored.
    let mut nowhere = manhattan_fleet(2, 1.0);
    nowhere.threats.jammers = vec![v2xw_engine::scenario::schema::ModelChoice::new(
        "attacker/jammer/constant",
    )];
    assert!(!v2xw_engine::scenario::validate(&nowhere).is_empty());
}

// -----------------------------------------------------------------------------------------
// radio.tiers.focus
// -----------------------------------------------------------------------------------------

/// A focus region at the high tier puts its receivers under the high-tier PHY rule, which
/// is the only rule that can lose a frame to a missed preamble. The counterexample is the
/// same run without the region, which cannot.
#[test]
fn a_focus_region_runs_its_receivers_at_the_focus_tier() {
    let base = open_air(manhattan_fleet(30, 2.0));
    let world = v2xw_engine::wiring::build_world(&base).expect("the world builds");
    let (plain, _) = run_recorded(base.clone());
    assert_eq!(plain.rx_losses.get("preamble-missed"), None);

    let mut focused = base;
    let origin: v2xw_core::geo::GeoOrigin = world.origin.into();
    let (lat0, lon0, _) = origin.to_geodetic(world.bbox.min);
    let (lat1, lon1, _) = origin.to_geodetic(world.bbox.max);
    focused.radio.tiers.focus = Some(v2xw_engine::scenario::schema::Focus {
        region: v2xw_engine::scenario::schema::FocusRegion::Bbox {
            bbox: v2xw_world::GeoBbox::new(lat0, lon0, lat1, lon1),
        },
        tier: v2xw_core::card::Tier::High,
    });
    let errors = v2xw_engine::scenario::validate(&focused);
    assert!(
        errors.is_empty(),
        "a focus region no longer loads: {errors:?}"
    );
    let (hi, _) = run_recorded(focused);
    assert!(
        hi.rx_losses.get("preamble-missed").copied().unwrap_or(0) > 0,
        "the focus region's receivers were not decided at the high tier: {:?}",
        hi.rx_losses
    );
}

// -----------------------------------------------------------------------------------------
// nodes.compute_tier
// -----------------------------------------------------------------------------------------

/// At the abstract compute tier a node's signature costs a microsecond, so the time from
/// generation to the air is the hand-off jitter and the access delay alone; at the medium
/// tier it carries the profile's ECDSA signing time (about 9.5 ms on the CRATON2 HSM).
#[test]
fn the_abstract_compute_tier_removes_the_signing_time() {
    let base = manhattan_fleet(10, 2.0);
    let mut abstract_ = base.clone();
    abstract_.nodes.compute_tier = v2xw_core::card::Tier::Abstract;
    let gen_to_air = |r: &MemoryRecorder| {
        let d: Vec<f64> = views::<NodeTxView>(r)
            .iter()
            .filter_map(|t| t.t_generated.map(|g| (t.t - g) as f64 / 1e6))
            .collect();
        d.iter().sum::<f64>() / d.len().max(1) as f64
    };
    let (_, medium) = run_recorded(base);
    let (_, fast) = run_recorded(abstract_);
    let (m, f) = (gen_to_air(&medium), gen_to_air(&fast));
    assert!(
        m - f > 5.0,
        "the abstract tier saved only {:.2} ms of generation-to-air time ({m:.2} -> {f:.2})",
        m - f
    );
}

// -----------------------------------------------------------------------------------------
// world.terrain
// -----------------------------------------------------------------------------------------

/// A DEM named in `world.terrain.dem` is read and attached, and a ridge in it obstructs a
/// link across it by knife-edge diffraction while a link beside it stays clear. The
/// counterexample is the same world without the file, where the ground is flat and the
/// link across the ridge's line is line of sight.
#[test]
fn a_dem_ridge_obstructs_a_link_across_it() {
    let mut scenario = rooted(
        Scenario::load(scenarios().join("phase1-grid.yaml")).expect("the shipped scenario loads"),
    );
    // The ground is what is under test: building obstruction off, so a class is terrain's.
    scenario.world.buildings.enabled = false;
    let flat = v2xw_engine::wiring::build_world(&scenario).expect("the flat world builds");
    assert!(flat.terrain.is_none());
    let origin: v2xw_core::geo::GeoOrigin = flat.origin.into();
    let mid_x = (flat.bbox.min.x + flat.bbox.max.x) * 0.5;
    let mid_y = (flat.bbox.min.y + flat.bbox.max.y) * 0.5;

    // A geographic ESRI ASCII grid over the world with a margin, 0.0005 degrees a cell,
    // flat except for a 150 m ridge running north-south through the middle.
    let (lat0, lon0, _) = origin.to_geodetic(flat.bbox.min);
    let (lat1, lon1, _) = origin.to_geodetic(flat.bbox.max);
    let cell = 0.0005_f64;
    let (lat0, lon0) = (lat0 - 0.01, lon0 - 0.01);
    let ncols = (((lon1 + 0.01) - lon0) / cell).ceil() as usize + 1;
    let nrows = (((lat1 + 0.01) - lat0) / cell).ceil() as usize + 1;
    let (_, ridge_lon, _) = origin.to_geodetic(v2xw_core::geom::Vec3::new(mid_x, mid_y, 0.0));
    let ridge_col = ((ridge_lon - lon0) / cell).round() as usize;
    let mut text = format!(
        "ncols {ncols}\nnrows {nrows}\nxllcorner {lon0}\nyllcorner {lat0}\ncellsize {cell}\nNODATA_value -9999\n"
    );
    for _ in 0..nrows {
        let row: Vec<&str> = (0..ncols)
            .map(|c| {
                if c.abs_diff(ridge_col) <= 1 {
                    "150"
                } else {
                    "0"
                }
            })
            .collect();
        text.push_str(&row.join(" "));
        text.push('\n');
    }
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("radio-access-dem");
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let path = dir.join("ridge.asc");
    std::fs::write(&path, text).expect("the DEM is written");

    scenario.world.terrain.dem = Some(path.to_string_lossy().into_owned());
    let world = v2xw_engine::wiring::build_world(&scenario).expect("the world with a DEM builds");
    assert!(world.terrain.is_some(), "the DEM was not attached");
    let mut stack = v2xw_engine::wiring::build_obstacles(&scenario, &world);
    assert!(
        stack.terrain.is_some(),
        "no terrain model was composed over the DEM"
    );

    let at = |dx: f64, dy: f64| v2xw_core::geom::Vec3::new(mid_x + dx, mid_y + dy, 1.5);
    let across = stack.classify(&world, at(-400.0, 0.0), at(400.0, 0.0));
    assert_eq!(across.class, v2xw_radio::LosClass::NlosT, "{across:?}");
    let loss = v2xw_radio::multi_edge_loss_db(
        &across.knife_edges,
        v2xw_radio::numeric::wavelength_m(5.86e9),
        v2xw_radio::MultiEdgeRule::Deygout,
        false,
    );
    assert!(
        loss > 20.0,
        "a 150 m ridge between the antennas costs only {loss:.1} dB"
    );
    let beside = stack.classify(&world, at(-400.0, -200.0), at(-400.0, 200.0));
    assert_eq!(beside.class, v2xw_radio::LosClass::Los);

    // Without the file the same link is clear.
    let mut flat_stack = v2xw_engine::wiring::build_obstacles(&scenario, &flat);
    let flat_across = flat_stack.classify(&flat, at(-400.0, 0.0), at(400.0, 0.0));
    assert!(flat_across.knife_edges.is_empty());
}
