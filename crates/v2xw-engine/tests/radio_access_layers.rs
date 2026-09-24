//! The access layers end to end: 802.11p medium access on an idle channel, and the
//! LTE-V2X / NR-V2X sidelink's congestion control, blind retransmissions, SCI-based
//! sensing, jammers that move and are sensed, the focus region, and what `node.tx` reports.
//!
//! Each property is shown against its counterexample in the same test: a check that
//! passes against the defect it guards is not a check.

use std::path::{Path, PathBuf};

use v2xw_engine::scenario::schema::ModelChoice;
use v2xw_engine::{Engine, MemoryRecorder, RunReport, Scenario};
use v2xw_metrics::channels::{NodeTxView, PhyRxView, RxOutcome};

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

/// The scaling ladder's Manhattan base with a bulk-spawned fleet of `n`, for `secs`, with
/// building obstruction off so the medium is genuinely shared.
fn fleet(n: u32, secs: f64) -> Scenario {
    let mut s = rooted(
        Scenario::load(scenarios().join("scale/base.yaml")).expect("the shipped scenario loads"),
    );
    s.time.duration_s = secs;
    s.actors.vehicles.demand.params = serde_json::json!({ "max_total_vehicles": n });
    s.world.buildings.enabled = false;
    s
}

fn with_rat(mut s: Scenario, rat: &str) -> Scenario {
    s.radio.rat = serde_json::from_value(serde_json::json!(rat)).expect("a rat");
    s
}

fn with_sidelink(mut s: Scenario, params: serde_json::Value) -> Scenario {
    s.radio.models.insert(
        "sidelink".to_string(),
        ModelChoice {
            id: "access/sidelink/engine-coupling".to_string(),
            params,
        },
    );
    s
}

fn run_recorded(scenario: Scenario) -> (RunReport, MemoryRecorder) {
    let errors = v2xw_engine::scenario::validate(&scenario);
    assert!(errors.is_empty(), "the scenario does not load: {errors:?}");
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

fn pdr_between(recorder: &MemoryRecorder, lo: f64, hi: f64) -> Option<(f64, u64)> {
    let mut n = 0u64;
    let mut ok = 0u64;
    for v in views::<PhyRxView>(recorder) {
        let Some(d) = v.dist_m else { continue };
        if d >= lo && d < hi {
            n += 1;
            if v.outcome == RxOutcome::Ok {
                ok += 1;
            }
        }
    }
    (n > 0).then(|| (ok as f64 / n as f64, n))
}

fn centre(s: &Scenario) -> [f64; 2] {
    let world = v2xw_engine::wiring::build_world(s).expect("the world builds");
    [
        (world.bbox.min.x + world.bbox.max.x) * 0.5,
        (world.bbox.min.y + world.bbox.max.y) * 0.5,
    ]
}

fn jammer(params: serde_json::Value) -> ModelChoice {
    ModelChoice {
        id: "attacker/jammer/constant".to_string(),
        params,
    }
}

// -----------------------------------------------------------------------------------------
// 802.11p: the access delay on an idle channel
// -----------------------------------------------------------------------------------------

/// On a nearly idle 802.11p channel every frame's access delay (air start − signed) is
/// exactly the AIFS and backoff its `node.tx` record reports, with no deferral left over,
/// and a frame that went out the instant it was ready reports no AIFS at all.
///
/// The counterexample is the engine before this was fixed: every record claimed a whole
/// 58 µs AIFS, so a frame with a zero access delay reported more AIFS than delay.
#[test]
fn an_idle_80211p_channel_costs_the_aifs_and_backoff_the_mac_reports() {
    let (_, rec) = run_recorded(fleet(5, 3.0));
    let tx = views::<NodeTxView>(&rec);
    assert!(tx.len() > 30, "only {} frames", tx.len());
    let mut immediate = 0;
    let mut deferred = 0;
    for v in &tx {
        let signed = v.t_signed.expect("t_signed");
        let aifs = v.mac_aifs_ns.expect("mac_aifs_ns");
        let backoff = v.mac_backoff_ns.expect("mac_backoff_ns");
        let access = v.t - signed;
        assert!(
            aifs + backoff <= access,
            "frame {:?}: {aifs} ns AIFS + {backoff} ns backoff inside a {access} ns access \
             delay",
            v.msg
        );
        if access == 0 {
            immediate += 1;
        }
        if access > aifs + backoff {
            deferred += 1;
        }
    }
    assert!(
        immediate > tx.len() / 2,
        "{immediate} of {} immediate",
        tx.len()
    );
    assert!(
        deferred * 50 <= tx.len(),
        "{deferred} of {} frames deferred on an idle channel",
        tx.len()
    );
    // And the MAC reports `dsrc-80211p` at the 6 Mbit/s rate, 802.11 OFDM rate index 2.
    let radio = tx[0]
        .radio
        .as_ref()
        .expect("node.tx carries the access layer");
    assert_eq!(radio.rat, "dsrc-80211p");
    assert_eq!(radio.mcs, "6mbps-qpsk-1/2");
    assert_eq!(tx[0].mcs, Some(2));
}

// -----------------------------------------------------------------------------------------
// The sidelink: what node.tx reports
// -----------------------------------------------------------------------------------------

/// A sidelink frame's `node.tx` carries the sidelink's own MCS and resource: the LTE MCS
/// of the J3161/1 profile (7) and a two-or-more-sub-channel allocation in the ten-channel
/// pool, or the NR MCS (9) in the two-channel one — not the 802.11p rate.
#[test]
fn sidelink_tx_records_carry_the_sidelink_mcs_and_resource() {
    let base = fleet(10, 2.0);
    let (lte, lte_rec) = run_recorded(with_rat(base.clone(), "lte-v2x-pc5"));
    let (_, nr_rec) = run_recorded(with_rat(base, "nr-v2x-pc5"));
    let lte_sl = lte
        .sidelink
        .as_ref()
        .expect("an LTE run reports its sidelink");
    assert_eq!(lte_sl.profile, "sae-j3161");
    assert_eq!(lte_sl.subchannels, 10);
    assert_eq!(lte_sl.congestion_control, "sae-j3161");
    let tx = views::<NodeTxView>(&lte_rec);
    assert!(!tx.is_empty());
    for v in &tx {
        let r = v.radio.as_ref().expect("radio view");
        assert_eq!(r.rat, "lte-v2x-mode4");
        assert_eq!(r.mcs, "lte-mcs7-j3161");
        assert_eq!(v.mcs, Some(7));
        assert_eq!(r.subchannels, Some(10));
        let (subch, len) = (r.subch.expect("subch"), r.subch_len.expect("len"));
        assert!(len >= 2 && subch + len <= 10, "resource {subch}+{len}");
        assert_eq!(
            r.slot,
            Some(v.t / 1_000_000),
            "the slot is the subframe it went out in"
        );
        assert_eq!(r.priority, Some(5), "a BSM is PPPP 5");
    }
    for v in views::<NodeTxView>(&nr_rec) {
        let r = v.radio.as_ref().expect("radio view");
        assert_eq!(
            (r.rat.as_str(), r.mcs.as_str()),
            ("nr-v2x-mode2", "nr-mcs9")
        );
        assert_eq!(v.mcs, Some(9));
        assert_eq!(r.slot, Some(v.t / 500_000), "a 0.5 ms NR slot");
    }
}

// -----------------------------------------------------------------------------------------
// Congestion control
// -----------------------------------------------------------------------------------------

/// With the channel loaded — a jammer in the middle of the fleet holds every sub-channel
/// above −94 dBm, so the CBR is near one — ETSI TS 103 574's CR limit for PPPP 5 (0.003)
/// is held at every transmission, by dropping blind retransmissions first. The
/// counterexample is the same run with congestion control off, which exceeds the limit.
#[test]
fn congestion_control_holds_the_cr_limit_under_load() {
    let mut base = with_rat(fleet(30, 3.0), "lte-v2x-pc5");
    let c = centre(&base);
    base.threats.jammers = vec![jammer(serde_json::json!({
        "position_m": c,
        "power_dbm": 23.0,
    }))];
    let cc = with_sidelink(
        base.clone(),
        serde_json::json!({ "congestion_control": "etsi-ts-103-574", "max_transmissions": 2 }),
    );
    let off = with_sidelink(
        base,
        serde_json::json!({ "congestion_control": "off", "max_transmissions": 2 }),
    );
    let (r_cc, rec_cc) = run_recorded(cc);
    let (r_off, rec_off) = run_recorded(off);
    let sl = r_cc.sidelink.as_ref().expect("sidelink report");
    let sl_off = r_off.sidelink.as_ref().expect("sidelink report");
    assert!(
        sl.max_cbr_at_grant_pm > 800,
        "the jammer did not load the channel: max CBR {} ‰",
        sl.max_cbr_at_grant_pm
    );
    // Held at every transmission.
    let mut limited = 0;
    for v in views::<NodeTxView>(&rec_cc) {
        let r = v.radio.expect("radio");
        if let (Some(cr), Some(l)) = (r.cr, r.cr_limit) {
            limited += 1;
            assert!(
                cr <= l + 1e-4,
                "CR {cr} over its limit {l} at CBR {:?}",
                r.cbr
            );
        }
    }
    assert!(limited > 0, "no transmission was under a CR limit");
    assert!(
        sl.cc_dropped_retx > 0,
        "no retransmission was given up: {sl:?}"
    );
    // The counterexample: nothing given up, and the limit is exceeded where it applied.
    assert_eq!((sl_off.cc_dropped_retx, sl_off.cc_dropped), (0, 0));
    let over = views::<NodeTxView>(&rec_off)
        .into_iter()
        .filter_map(|v| v.radio)
        .filter(|r| r.cbr.unwrap_or(0.0) > 0.8 && r.cr.unwrap_or(0.0) > 0.003 + 1e-4)
        .count();
    assert!(
        over > 0,
        "without congestion control the limit was never exceeded"
    );
    assert!(sl_off.retransmissions > sl.retransmissions);
}

// -----------------------------------------------------------------------------------------
// Blind retransmissions
// -----------------------------------------------------------------------------------------

/// A blind retransmission, chase-combined at the receiver, delivers at ranges where one
/// copy often fails; each retransmission is a `node.tx` record of its own (attempt 2),
/// and each transport block is still one reception per receiver.
#[test]
fn blind_retransmissions_raise_delivery_at_range() {
    let base = with_rat(fleet(30, 3.0), "lte-v2x-pc5");
    let once = with_sidelink(
        base.clone(),
        serde_json::json!({ "congestion_control": "off" }),
    );
    let twice = with_sidelink(
        base,
        serde_json::json!({ "congestion_control": "off", "max_transmissions": 2 }),
    );
    let (r1, rec1) = run_recorded(once);
    let (r2, rec2) = run_recorded(twice);
    let sl1 = r1.sidelink.as_ref().expect("report");
    let sl2 = r2.sidelink.as_ref().expect("report");
    assert_eq!((sl1.retransmissions, sl1.retx_decodes), (0, 0));
    // In an open-air fleet the second copy mostly helps by diversity — its own fade — and
    // combining shows under interference (`congestion_control_holds_the_cr_limit_under_load`
    // runs one with a jammer); either way the receptions it rescues are counted.
    assert!(sl2.retransmissions > 0 && sl2.retx_decodes > 0, "{sl2:?}");
    let attempts: std::collections::BTreeSet<u32> = views::<NodeTxView>(&rec2)
        .iter()
        .filter_map(|v| v.radio.as_ref().and_then(|r| r.attempt))
        .collect();
    assert_eq!(attempts, [1, 2].into_iter().collect());
    // One reception per (block, receiver): the phy.rx count equals the attempts the run
    // reports, and no (msg, rx) pair repeats.
    let rx = views::<PhyRxView>(&rec2);
    let pairs: std::collections::BTreeSet<(Option<u64>, u32)> =
        rx.iter().map(|v| (v.msg, v.rx.index())).collect();
    assert_eq!(
        pairs.len(),
        rx.len(),
        "a block was recorded twice at one receiver"
    );
    assert_eq!(rx.len() as u64, r2.reception_attempts);
    for (lo, hi) in [
        (0.0, 300.0),
        (300.0, 700.0),
        (700.0, 850.0),
        (850.0, 1000.0),
    ] {
        let a = pdr_between(&rec1, lo, hi);
        let b = pdr_between(&rec2, lo, hi);
        eprintln!("PDR {lo}-{hi} m: one copy {a:?}, two {b:?}");
    }
    // Over every distance: the losses one copy suffers — half duplex and collisions near,
    // fading at the edge of range — at least halve with a second.
    let (all1, _) = pdr_between(&rec1, 0.0, 1001.0).expect("links");
    let (all2, _) = pdr_between(&rec2, 0.0, 1001.0).expect("links");
    let (loss1, loss2) = (1.0 - all1, 1.0 - all2);
    assert!(
        loss1 > 0.01,
        "one copy loses only {loss1:.4}: nothing for a second to rescue"
    );
    assert!(
        loss2 < 0.5 * loss1,
        "a second copy cut the loss only from {loss1:.4} to {loss2:.4}"
    );
}

// -----------------------------------------------------------------------------------------
// Sensing from decoded SCIs
// -----------------------------------------------------------------------------------------

/// Sensing is fed by decoded SCIs only, and whether an SCI decodes depends on its SINR,
/// not only on how strong it is: the same fleet with a jammer in its middle misses far
/// more of the SCIs in range, though every one of them arrives at the same power. (With
/// "heard above the threshold is decoded" the two runs would sense the same.)
#[test]
fn sensing_records_only_the_scis_a_ue_decoded() {
    let base = with_rat(fleet(30, 2.0), "lte-v2x-pc5");
    let c = centre(&base);
    let (quiet, _) = run_recorded(base.clone());
    let mut jammed = base;
    jammed.threats.jammers = vec![jammer(serde_json::json!({ "position_m": c }))];
    let (loud, _) = run_recorded(jammed);
    let missed = |r: &RunReport| {
        let sl = r.sidelink.as_ref().expect("report");
        assert!(sl.sci_decoded > 0 && sl.sci_missed > 0, "{sl:?}");
        sl.sci_missed as f64 / (sl.sci_decoded + sl.sci_missed) as f64
    };
    let (q, l) = (missed(&quiet), missed(&loud));
    eprintln!("SCIs missed: quiet {q:.3}, jammed {l:.3}");
    assert!(
        l > q + 0.05,
        "a jammer changed the SCIs missed only from {q:.3} to {l:.3}"
    );
}

// -----------------------------------------------------------------------------------------
// Jammers that move, and that the sidelink senses
// -----------------------------------------------------------------------------------------

/// A jammer can ride a vehicle or drive a path. One driving from 5 km outside the map to
/// its centre jams frames once it arrives; the same jammer standing at its starting point
/// jams none. One riding a vehicle of the fleet jams frames too.
#[test]
fn a_jammer_can_ride_a_vehicle_or_drive_a_path() {
    let base = fleet(20, 3.0);
    let c = centre(&base);
    let far = [c[0] + 5000.0, c[1]];
    let mut parked = base.clone();
    parked.threats.jammers = vec![jammer(serde_json::json!({ "position_m": far }))];
    let (p, _) = run_recorded(parked);
    assert_eq!(
        p.rx_losses.get("jammed"),
        None,
        "a jammer 5 km out jammed: {p:?}"
    );

    let mut driving = base.clone();
    driving.threats.jammers = vec![jammer(serde_json::json!({
        "path_m": [far, c],
        "speed_mps": 5000.0,
    }))];
    let (d, _) = run_recorded(driving);
    assert!(
        d.rx_losses.get("jammed").copied().unwrap_or(0) > 0,
        "the jammer that drove in jammed nothing: {:?}",
        d.rx_losses
    );

    // Ride the first vehicle that transmits.
    let (_, plain) = run_recorded(base.clone());
    let rider = views::<NodeTxView>(&plain)[0].node.index();
    let mut riding = base.clone();
    riding.threats.jammers = vec![jammer(serde_json::json!({ "follow_node": rider }))];
    let (rd, _) = run_recorded(riding);
    assert!(
        rd.rx_losses.get("jammed").copied().unwrap_or(0) > 0,
        "the jammer riding node {rider} jammed nothing"
    );

    // Placed two ways at once, or on a path without a speed: refused at load.
    let mut both = base.clone();
    both.threats.jammers = vec![jammer(serde_json::json!({
        "position_m": c,
        "follow_node": 1,
    }))];
    assert!(!v2xw_engine::scenario::validate(&both).is_empty());
    let mut slow = base;
    slow.threats.jammers = vec![jammer(serde_json::json!({ "path_m": [far, c] }))];
    assert!(!v2xw_engine::scenario::validate(&slow).is_empty());
}

/// On a sidelink the jammer's energy is measured: it raises every nearby UE's CBR. The
/// counterexample is the same run without it.
#[test]
fn the_sidelink_senses_a_jammer_in_its_cbr() {
    let base = with_rat(fleet(20, 2.0), "lte-v2x-pc5");
    let c = centre(&base);
    let (quiet, _) = run_recorded(base.clone());
    let mut jammed = base;
    jammed.threats.jammers = vec![jammer(serde_json::json!({ "position_m": c }))];
    let (loud, _) = run_recorded(jammed);
    let q = quiet
        .sidelink
        .as_ref()
        .expect("report")
        .mean_cbr_at_grant_pm;
    let l = loud.sidelink.as_ref().expect("report").mean_cbr_at_grant_pm;
    assert!(l > q + 200, "mean CBR at grant: {q} ‰ quiet, {l} ‰ jammed");
}

// -----------------------------------------------------------------------------------------
// The focus region on a sidelink
// -----------------------------------------------------------------------------------------

/// A high focus region over the whole map puts every sidelink receiver under the high-tier
/// PHY rule — the SCI must decode before the transport block counts — so it delivers no
/// more than the medium tier and, at range, less; and every `phy.rx` record carries its
/// link's place against the region.
#[test]
fn a_focus_region_decides_sidelink_receivers_at_its_tier_and_tags_the_links() {
    let mut base = with_rat(fleet(30, 2.0), "lte-v2x-pc5");
    // The region raises the propagation tier to its own as well as the PHY's, and since
    // the radioprop track the high propagation tier is the geometric city-street law,
    // which on this building-free extract is line of sight all the way (TR 37.885's
    // shallow LOS exponent) and so delivers *more* than the medium law (5,803 against
    // 5,426 receptions). That compares two path-loss laws, not the SCI stage. Both runs
    // therefore price links with the medium tier's law, named in `radio.models`, and the
    // region raises the PHY rule and the fading.
    base.radio.models.insert(
        "propagation".to_string(),
        ModelChoice::new("propagation/log-distance-shadowing"),
    );
    let world = v2xw_engine::wiring::build_world(&base).expect("the world builds");
    let origin: v2xw_core::geo::GeoOrigin = world.origin.into();
    let (lat0, lon0, _) = origin.to_geodetic(world.bbox.min);
    let (lat1, lon1, _) = origin.to_geodetic(world.bbox.max);
    let mut focused = base.clone();
    focused.radio.tiers.focus = Some(v2xw_engine::scenario::schema::Focus {
        region: v2xw_engine::scenario::schema::FocusRegion::Bbox {
            bbox: v2xw_world::GeoBbox::new(lat0, lon0, lat1, lon1),
        },
        tier: v2xw_core::card::Tier::High,
    });
    let (plain, plain_rec) = run_recorded(base);
    let (hi, hi_rec) = run_recorded(focused);
    let tags: std::collections::BTreeSet<Option<String>> = views::<PhyRxView>(&hi_rec)
        .into_iter()
        .map(|v| v.focus)
        .collect();
    assert!(tags.contains(&Some("inside".to_string())), "tags {tags:?}");
    assert!(
        views::<PhyRxView>(&plain_rec)
            .iter()
            .all(|v| v.focus.is_none()),
        "a run with no focus region tagged its links"
    );
    assert!(
        hi.receptions_ok < plain.receptions_ok,
        "the high-tier SCI stage changed nothing: {} vs {}",
        hi.receptions_ok,
        plain.receptions_ok
    );
}

// -----------------------------------------------------------------------------------------
// radio.models.sidelink
// -----------------------------------------------------------------------------------------

/// `radio.models.sidelink` is checked against the RAT: a sidelink configuration on an
/// 802.11p run, an MCS the profile has no allocation for, and more transmissions than
/// LTE-V2X allows are each refused at load.
#[test]
fn radio_models_sidelink_is_checked_against_the_rat() {
    let base = fleet(2, 1.0);
    let lte = with_rat(base.clone(), "lte-v2x-pc5");
    let nr = with_rat(base.clone(), "nr-v2x-pc5");
    let ok = |s: Scenario| v2xw_engine::scenario::validate(&s).is_empty();
    assert!(ok(with_sidelink(
        lte.clone(),
        serde_json::json!({ "mcs": 11 })
    )));
    assert!(ok(with_sidelink(
        lte.clone(),
        serde_json::json!({ "profile": "molina-masegosa-2017" })
    )));
    assert!(ok(with_sidelink(
        nr.clone(),
        serde_json::json!({ "max_transmissions": 3 })
    )));
    assert!(!ok(with_sidelink(base, serde_json::json!({}))));
    assert!(!ok(with_sidelink(
        lte.clone(),
        serde_json::json!({ "mcs": 6 })
    )));
    assert!(!ok(with_sidelink(
        lte.clone(),
        serde_json::json!({ "max_transmissions": 3 })
    )));
    assert!(!ok(with_sidelink(
        lte,
        serde_json::json!({ "congestion_control": "fcc" })
    )));
    assert!(!ok(with_sidelink(
        nr,
        serde_json::json!({ "profile": "sae-j3161" })
    )));
}

// -----------------------------------------------------------------------------------------
// nodes.compute_tier
// -----------------------------------------------------------------------------------------

/// `high` runs a node's CPU as its profile's cores and `medium` as one server
/// (06-node-models §2.1). On a profile with no HSM, whose ECDSA verification runs on the
/// CPU (the generic automotive SoC: 645 µs a verify, four cores), a fleet's verifications
/// wait less at `high`. The counterexample is the same pair of runs on the reference OBU,
/// whose cryptography runs on its HSM and accelerator: the tiers do not differ there.
#[test]
fn the_high_compute_tier_serves_cpu_work_on_every_core() {
    let mean_wait_us = |rec: &MemoryRecorder| {
        let w: Vec<f64> = views::<v2xw_metrics::channels::NodeVerifyView>(rec)
            .iter()
            .filter_map(|v| v.t_start.map(|s| (s - v.t_enqueue) as f64 / 1e3))
            .collect();
        assert!(!w.is_empty(), "no verification was recorded");
        w.iter().sum::<f64>() / w.len() as f64
    };
    let tiers = |obu: &str| {
        let mut medium = fleet(60, 3.0);
        medium.nodes.default_obu = obu.to_string();
        let mut high = medium.clone();
        high.nodes.compute_tier = v2xw_core::card::Tier::High;
        let (_, m) = run_recorded(medium);
        let (_, h) = run_recorded(high);
        (mean_wait_us(&m), mean_wait_us(&h))
    };
    let (m, h) = tiers("obu/generic-automotive-soc-no-hsm");
    eprintln!("software crypto: mean verify wait {m:.1} us medium, {h:.1} us high");
    assert!(m > 2.0 * h && m > 10.0, "medium {m:.1} us, high {h:.1} us");
    let (rm, rh) = tiers("obu/unex-obu-301-craton2");
    eprintln!("HSM crypto: mean verify wait {rm:.1} us medium, {rh:.1} us high");
    assert!(
        (rm - rh).abs() < 1e-9,
        "the HSM profile changed: {rm} vs {rh}"
    );
}

// -----------------------------------------------------------------------------------------
// The three technologies side by side (a measurement, not a unit test)
// -----------------------------------------------------------------------------------------

/// Delivery against distance and latency for 802.11p, LTE-V2X and NR-V2X at a low and a
/// high density, on Midtown with buildings off (line of sight, as the published
/// comparisons are). Run with `--ignored --nocapture`; it prints the table the build
/// status quotes.
#[test]
#[ignore = "measurement: prints the technology comparison"]
fn technology_comparison_table() {
    let secs: f64 = std::env::var("CMP_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5.0);
    let densities: Vec<u32> = std::env::var("CMP_TRIPS")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| vec![20, 200]);
    let pct = |mut v: Vec<f64>, q: f64| -> f64 {
        if v.is_empty() {
            return f64::NAN;
        }
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * q).round() as usize]
    };
    for &trips in &densities {
        for rat in ["dsrc-80211p", "lte-v2x-pc5", "nr-v2x-pc5"] {
            let (r, rec) = run_recorded(with_rat(fleet(trips, secs), rat));
            let mut bins = String::new();
            for lo in (0..1000).step_by(100) {
                let p = pdr_between(&rec, f64::from(lo), f64::from(lo + 100));
                bins.push_str(&match p {
                    Some((p, _)) => format!(" {p:.3}"),
                    None => "   -  ".to_string(),
                });
            }
            let e2e: Vec<f64> = views::<v2xw_metrics::channels::NodeRxView>(&rec)
                .iter()
                .filter_map(|v| v.e2e_ns().map(|ns| ns as f64 / 1e6))
                .collect();
            let access: Vec<f64> = views::<NodeTxView>(&rec)
                .iter()
                .filter(|v| v.radio.as_ref().and_then(|r| r.attempt).unwrap_or(1) == 1)
                .filter_map(|v| v.t_signed.map(|s| (v.t - s) as f64 / 1e6))
                .collect();
            let cbr = r.sidelink.as_ref().map_or(String::from("-"), |s| {
                format!("{} pm", s.mean_cbr_at_grant_pm)
            });
            println!(
                "trips {trips:>4} nodes {:>4} {rat:<12} PDR/100m:{bins} | access ms p50 {:.3} \
                 p95 {:.3} | e2e ms p50 {:.2} p95 {:.2} | attempts {} ok {} | cbr {cbr}",
                r.nodes_created,
                pct(access.clone(), 0.5),
                pct(access, 0.95),
                pct(e2e.clone(), 0.5),
                pct(e2e, 0.95),
                r.reception_attempts,
                r.receptions_ok,
            );
        }
    }
}
