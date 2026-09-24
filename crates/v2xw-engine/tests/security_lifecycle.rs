//! The credential lifecycle, pseudonym rotation and backend connectivity, on the procedural
//! grid (fast enough to run every check in seconds rather than the Manhattan path's
//! minutes).
//!
//! Each check is paired with the same scenario with one thing removed or broken, and
//! asserts the difference — a check that could only pass is not a check.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;
use v2xw_engine::scenario::ModelChoice;
use v2xw_engine::{Engine, MemoryRecorder, RunReport, Scenario};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

/// The Phase 1 grid with a fleet and the SCMS backend turned on.
fn grid(duration_s: f64, rate: f64) -> Scenario {
    let mut s =
        Scenario::load(scenarios().join("phase1-grid.yaml")).expect("the shipped grid loads");
    s.time.duration_s = duration_s;
    s.actors.vehicles.demand.rate_veh_per_h = Some(rate);
    // Five avenues by eight streets, about 1.1 km by 0.6 km: small enough that a fleet
    // spawned over a minute is within radio range of itself.
    if let v2xw_world::WorldSourceSpec::Procedural { params, .. } = &mut s.world.source {
        params["cols"] = json!(5);
        params["rows"] = json!(8);
    }
    s.metrics = vec!["all".to_string()];
    s.actors.backend.protocol = Some(v2xw_engine::phase2::CAMP_SCMS.to_string());
    s
}

fn lifecycle(s: &mut Scenario, params: serde_json::Value) {
    s.security.protocol = Some(ModelChoice {
        id: v2xw_engine::phase2::CAMP_SCMS.to_string(),
        params,
    });
}

fn cellular(s: &mut Scenario) {
    s.net.uu = Some(ModelChoice {
        id: "cellular/uu/fixed-latency".to_string(),
        params: json!({"preset": "4g-east-coast"}),
    });
}

fn run(s: Scenario) -> (RunReport, MemoryRecorder) {
    assert!(
        v2xw_engine::scenario::validate::validate(&s).is_empty(),
        "the test scenario must load: {:?}",
        v2xw_engine::scenario::validate::validate(&s)
    );
    let mut engine = Engine::build(s, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (report, recorder)
}

fn records<'a>(r: &'a MemoryRecorder, channel: &str) -> Vec<serde_json::Value> {
    r.records()
        .iter()
        .filter(|(_, rec)| rec.channel == channel)
        .map(|(_, rec)| serde_json::from_slice(&rec.json).expect("json"))
        .collect()
}

/// A compressed calendar: a 20 s i-period, three certificates a period, two periods held
/// at the start, and a top-up as soon as only the current period is left.
fn compressed() -> serde_json::Value {
    json!({
        "i_period_s": 20,
        "cert_lifetime_s": 21,
        "certs_per_period": 3,
        "pool_periods": 2,
        "topup_below_periods": 1,
        "cert_shuffle_window_s": 1,
        "first_batch_delay_s": 1,
        "download_poll_interval_s": 1
    })
}

/// Certificates expire, a vehicle with a backend link tops its pool up before it runs
/// dry, and a vehicle with none runs out and stops signing — which is counted.
#[test]
fn a_pool_is_topped_up_over_the_link_and_runs_dry_without_one() {
    let mut connected = grid(70.0, 400.0);
    lifecycle(&mut connected, compressed());
    cellular(&mut connected);
    let (with_link, rec) = run(connected);
    let p = &with_link.phase2;
    println!(
        "with a link: {} top-ups started, {} completed, {} certificates, {} starved \
         vehicles, {} starved node-steps",
        p.topups_started,
        p.topups_completed,
        p.certs_topped_up,
        p.vehicles_starved,
        p.starved_node_steps
    );
    assert!(
        p.topups_started > 0,
        "no pool ever ran low in a 70 s run of 20 s periods"
    );
    assert!(p.topups_completed > 0, "no top-up batch was installed");
    assert!(p.certs_topped_up >= 3, "a top-up installs a whole period");
    assert!(
        records(&rec, "sec.cert")
            .iter()
            .any(|r| r["event"] == "top-up"),
        "a top-up must be on sec.cert"
    );
    // The bytes of a top-up cross the cellular link, both ways.
    assert!(p.access.cellular_vehicles > 0);
    let buckets: BTreeSet<String> = records(&rec, "net.bytes")
        .iter()
        .filter_map(|r| r["bucket"].as_str().map(str::to_string))
        .collect();
    assert!(buckets.contains("cellular-ul"), "{buckets:?}");
    assert!(buckets.contains("cellular-dl"), "{buckets:?}");
    assert!(buckets.contains("backend"), "{buckets:?}");

    // The same fleet with no modem and no roadside unit: nothing can top up.
    let mut isolated = grid(70.0, 400.0);
    lifecycle(&mut isolated, compressed());
    let (offline, _) = run(isolated);
    let q = &offline.phase2;
    println!(
        "offline: {} top-ups, {} starved vehicles, {} starved node-steps",
        q.topups_started, q.vehicles_starved, q.starved_node_steps
    );
    assert_eq!(
        q.topups_started, 0,
        "a vehicle with no link started a top-up"
    );
    assert!(
        q.vehicles_starved > 0,
        "the pools expired and no vehicle was left unable to sign"
    );
    assert!(q.starved_node_steps > p.starved_node_steps);
}

/// Every identifier a passive observer reads changes together: the certificate, the BSM
/// temporary ID the node really encodes, and the link-layer address.
#[test]
fn every_identifier_changes_together_at_a_pseudonym_change() {
    let mut s = grid(45.0, 300.0);
    s.security.pseudonym_change.period_s = Some(10.0);
    let (report, rec) = run(s);
    assert!(report.phase2.pseudonym_changes > 0, "no pseudonym changed");
    let changes = records(&rec, "sec.pseudonym");
    assert_eq!(changes.len() as u64, report.phase2.pseudonym_changes);
    let mut with_old = 0;
    for c in &changes {
        if c["old_digest"].is_null() {
            continue;
        }
        with_old += 1;
        for (a, b) in [
            ("old_digest", "new_digest"),
            ("old_temp_id", "new_temp_id"),
            ("old_l2", "new_l2"),
        ] {
            assert_ne!(c[a], c[b], "{a} did not change with the certificate: {c}");
        }
    }
    assert!(with_old > 0);

    // What the node really put in its BSMs: one temporary ID per pseudonym, and a new
    // one with each new pseudonym.
    let tx = records(&rec, "node.tx");
    let pairs: Vec<(String, String)> = tx
        .iter()
        .filter_map(|r| {
            Some((
                r["pseudonym"].as_str()?.to_string(),
                r["content"]["temp_id"].as_str()?.to_string(),
            ))
        })
        .collect();
    assert!(!pairs.is_empty());
    assert_eq!(identifier_leaks(&pairs), Vec::<String>::new());
    // The checker itself can go red: a temporary ID kept across a certificate change.
    let leaked = vec![
        ("aa11".to_string(), "t1".to_string()),
        ("bb22".to_string(), "t1".to_string()),
    ];
    assert_eq!(identifier_leaks(&leaked).len(), 1);
}

/// Temporary IDs that two different pseudonyms were sent under.
fn identifier_leaks(pairs: &[(String, String)]) -> Vec<String> {
    let mut by_temp: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (pseudonym, temp) in pairs {
        by_temp.entry(temp).or_default().insert(pseudonym);
    }
    by_temp
        .into_iter()
        .filter(|(_, ps)| ps.len() > 1)
        .map(|(t, _)| t.to_string())
        .collect()
}

/// The pseudonym-privacy study's cells differ: the change period reaches the run, and the
/// linkability metric has changes to measure.
#[test]
fn the_change_period_reaches_the_run_and_the_observer_measures_it() {
    let mut per = BTreeMap::new();
    for period in [10.0, 30.0] {
        let mut s = grid(45.0, 300.0);
        s.security.pseudonym_change.period_s = Some(period);
        let (report, rec) = run(s);
        let links = records(&rec, "privacy.link").len();
        println!(
            "period {period} s: {} changes, {} observer claims ({} linked, {} correct)",
            report.phase2.pseudonym_changes,
            links,
            report.phase2.privacy_links_claimed,
            report.phase2.privacy_links_correct
        );
        per.insert(
            period as u32,
            (report.phase2.pseudonym_changes, rec.digest_hex()),
        );
    }
    let (fast, fast_digest) = &per[&10];
    let (slow, slow_digest) = &per[&30];
    assert!(
        fast > slow,
        "a 10 s period changed no more often than a 30 s one"
    );
    assert_ne!(
        fast_digest, slow_digest,
        "two periods produced the same run"
    );
}

/// Honest traffic is never revoked at the default thresholds, and it is the authority's
/// persistence gate that prevents it: with the gate opened to a single report, the same
/// honest fleet's false positives do revoke a device.
#[test]
fn honest_traffic_is_not_revoked_and_the_gate_is_why() {
    let base = || {
        let mut s = grid(90.0, 900.0);
        s.security.verification_policy = "verify-all".to_string();
        s.detection.local = vec![ModelChoice::new(v2xw_engine::phase2::LEGACY_12)];
        cellular(&mut s);
        lifecycle(
            &mut s,
            json!({"report_shuffle_window_s": 1, "crl_cadence_s": 0, "crl_fetch_interval_s": 5}),
        );
        s
    };
    let (honest, _) = run(base());
    let p = &honest.phase2;
    println!(
        "default gate: {} verdicts over {} messages, {} reports, {} at the authority, {} \
         revoke decisions, {} issued",
        p.verdicts_fired,
        p.messages_checked,
        p.reports_sent,
        p.reports_at_ma,
        p.ma_revoke_decisions,
        p.crls_issued
    );
    assert!(p.messages_checked > 0, "the detectors must run");
    assert_eq!(
        p.ma_revoke_decisions, 0,
        "the authority decided to revoke an honest device"
    );
    assert_eq!(p.crls_issued, 0);

    // The control: the same fleet with the gate opened to one report from one reporter.
    let mut open = base();
    open.detection.ma = Some(ModelChoice {
        id: v2xw_engine::phase2::MA_LEGACY_WINDOW.to_string(),
        params: json!({"report_threshold_k": 1, "revoke_min_seconds": 1, "revoke_persist_s": 0.0}),
    });
    let (opened, _) = run(open);
    let q = &opened.phase2;
    println!(
        "open gate: {} reports at the authority, {} revoke decisions, {} issued",
        q.reports_at_ma, q.ma_revoke_decisions, q.crls_issued
    );
    assert!(
        q.reports_sent > 0,
        "the honest fleet filed no report at all, so this control proves nothing"
    );
    assert!(
        q.ma_revoke_decisions > 0,
        "with no persistence gate the honest fleet's false reports should revoke someone"
    );
    assert!(q.revoked_honest > 0, "the control revoked no honest device");
    assert_eq!(p.revoked_honest, 0);
}

/// A report is backend traffic: with a modem it goes over the cellular uplink and never
/// on the sidelink; the counts and the byte buckets say so.
#[test]
fn a_report_goes_over_the_cellular_uplink_not_the_sidelink() {
    let mut s = grid(60.0, 900.0);
    s.security.verification_policy = "verify-all".to_string();
    s.detection.local = vec![ModelChoice::new(v2xw_engine::phase2::LEGACY_12)];
    s.threats.attackers = vec![v2xw_engine::scenario::schema::Attacker {
        id: "threat/attacker/legacy/ConstPos".to_string(),
        count: Some(1),
        schedule: Some(v2xw_engine::scenario::schema::DilationWindow {
            from_s: 5.0,
            to_s: 60.0,
        }),
        ..Default::default()
    }];
    cellular(&mut s);
    lifecycle(&mut s, json!({"report_shuffle_window_s": 1}));
    let (report, rec) = run(s);
    let p = &report.phase2;
    println!(
        "{} reports: {} over cellular, {} relayed, {} at the proxy",
        p.reports_sent, p.reports_uploaded_cellular, p.reports_uploaded_relay, p.reports_received
    );
    assert!(p.reports_sent > 0, "the attacker was never reported");
    assert_eq!(p.reports_uploaded_relay, 0);
    assert!(p.reports_uploaded_cellular > 0);
    assert!(p.reports_received > 0, "no report reached the proxy");
    // No report frame went on the air.
    assert!(
        !records(&rec, "node.tx")
            .iter()
            .any(|r| r["msg_type"] == "mbr"),
        "a report went on the sidelink although the vehicle had a modem"
    );
    assert!(p.access.uu_ul_bytes > 0);
}

/// The two protocols revoke differently, and the run shows it. Under the CAMP SCMS the
/// authority's decision becomes a linkage-seed CRL entry that receivers enforce; under the
/// ETSI ITS PKI there is no per-vehicle list (TS 102 941 §6.1.4 NOTE 4): the EA blocklists
/// the enrolment credential, the vehicle keeps signing with the tickets it holds, no
/// reception is refused, and its pool runs dry because the AA stops issuing.
#[test]
fn the_etsi_pki_revokes_passively_and_the_scms_actively() {
    let base = |protocol: &str| {
        let mut s = grid(90.0, 900.0);
        s.security.verification_policy = "verify-all".to_string();
        s.detection.local = vec![ModelChoice::new(v2xw_engine::phase2::LEGACY_12)];
        s.threats.attackers = vec![v2xw_engine::scenario::schema::Attacker {
            id: "threat/attacker/legacy/ConstPos".to_string(),
            count: Some(1),
            schedule: Some(v2xw_engine::scenario::schema::DilationWindow {
                from_s: 5.0,
                to_s: 90.0,
            }),
            ..Default::default()
        }];
        cellular(&mut s);
        s.actors.backend.protocol = Some(protocol.to_string());
        s.security.protocol = Some(ModelChoice {
            id: protocol.to_string(),
            params: json!({
                "i_period_s": 30, "cert_lifetime_s": 31, "certs_per_period": 3,
                "cert_shuffle_window_s": 1, "first_batch_delay_s": 1,
                "download_poll_interval_s": 1, "report_shuffle_window_s": 2,
                "crl_cadence_s": 0, "crl_fetch_interval_s": 2
            }),
        });
        if protocol == v2xw_engine::phase2::ETSI_PKI {
            s.security.envelope = "etsi103097".to_string();
        }
        s
    };
    let (scms, _) = run(base(v2xw_engine::phase2::CAMP_SCMS));
    let (etsi, rec) = run(base(v2xw_engine::phase2::ETSI_PKI));
    let (a, b) = (&scms.phase2, &etsi.phase2);
    println!(
        "SCMS: {} decisions, {} issued, {} installed, {} refused receptions; \
         ETSI: {} decisions, {} issued, {} refused receptions, {} top-ups started, {} \
         completed, {} starved vehicles",
        a.ma_revoke_decisions,
        a.crls_issued,
        a.crls_installed,
        a.revoked_receptions,
        b.ma_revoke_decisions,
        b.crls_issued,
        b.revoked_receptions,
        b.topups_started,
        b.topups_completed,
        b.vehicles_starved
    );
    assert_eq!(
        a.backend_errors + b.backend_errors,
        0,
        "{} {}",
        a.first_backend_error,
        b.first_backend_error
    );
    // On this small grid the attacker has usually driven out of the map by the time the
    // list is installed, so refused receptions are asserted by the Manhattan path test;
    // what distinguishes the protocols here is that the SCMS issues a list the fleet
    // installs and the ETSI PKI issues none.
    assert!(
        a.revoked_attackers >= 1 && a.crls_installed > 0,
        "the SCMS path must issue a list the fleet installs"
    );
    assert_eq!(
        b.crls_installed, 0,
        "the ETSI PKI installed a revocation list"
    );
    assert!(
        b.revoked_attackers >= 1,
        "the ETSI authority must decide on the attacker"
    );
    assert_eq!(
        b.crls_issued, 0,
        "the ETSI PKI has no per-vehicle revocation list"
    );
    assert_eq!(
        b.revoked_receptions, 0,
        "a passive revocation refused a reception"
    );
    let stages: BTreeSet<String> = records(&rec, "proto.revocation")
        .iter()
        .filter_map(|r| r["stage"].as_str().map(str::to_string))
        .collect();
    for s in [
        "detect",
        "decision",
        "blocklisted",
        "last_valid_credential_expiry",
    ] {
        assert!(stages.contains(s), "ETSI stage {s} missing: {stages:?}");
    }
    assert!(
        b.topups_completed > 0,
        "honest ETSI vehicles must top their tickets up"
    );
}

/// A vehicle with no modem reaches the backend through a roadside unit: the report goes on
/// the air to the unit, crosses its backhaul, and the bytes land in the backhaul bucket and
/// not in a cellular one. The same fleet with the unit's backhaul cut reaches nothing.
#[test]
fn a_vehicle_without_a_modem_relays_through_a_roadside_unit() {
    let base = |backhaul: Option<&str>| {
        let mut s = grid(60.0, 900.0);
        s.security.verification_policy = "verify-all".to_string();
        s.detection.local = vec![ModelChoice::new(v2xw_engine::phase2::LEGACY_12)];
        s.threats.attackers = vec![v2xw_engine::scenario::schema::Attacker {
            id: "threat/attacker/legacy/ConstPos".to_string(),
            count: Some(1),
            schedule: Some(v2xw_engine::scenario::schema::DilationWindow {
                from_s: 5.0,
                to_s: 60.0,
            }),
            ..Default::default()
        }];
        lifecycle(&mut s, json!({"report_shuffle_window_s": 1}));
        // Units on a 300 m lattice over the grid, so most of it is within relay range.
        for x in [150.0, 450.0, 750.0, 1050.0] {
            for y in [150.0, 450.0] {
                s.actors.rsus.push(v2xw_engine::scenario::Rsu {
                    site: None,
                    position_m: Some([x, y, 0.0]),
                    roles: vec!["report-forward".to_string()],
                    profile: None,
                    backhaul: backhaul.map(str::to_string),
                });
            }
        }
        s
    };
    let (connected, rec) = run(base(None));
    let p = &connected.phase2;
    println!(
        "relay: {} reports, {} relayed, {} at the proxy, {} backhaul bytes, {} held",
        p.reports_sent,
        p.reports_uploaded_relay,
        p.reports_received,
        p.access.backhaul_bytes,
        p.reports_unsent
    );
    assert_eq!(p.access.cellular_vehicles, 0);
    assert!(p.reports_uploaded_relay > 0, "no report was relayed");
    assert!(
        p.reports_received > 0,
        "no relayed report reached the proxy"
    );
    assert!(p.access.backhaul_bytes > 0, "the backhaul carried nothing");
    assert_eq!(
        p.access.uu_ul_bytes, 0,
        "a vehicle with no modem used the uplink"
    );
    assert!(
        records(&rec, "node.tx")
            .iter()
            .any(|r| r["msg_type"] == "mbr"),
        "a relayed report is a frame on the air"
    );

    let (cut, _) = run(base(Some("backhaul/none")));
    let q = &cut.phase2;
    assert_eq!(
        q.reports_received, 0,
        "a report crossed a backhaul that is not there"
    );
    assert!(
        q.reports_sent > 0,
        "the detectors must still fire, or this proves nothing"
    );
}

/// A roadside unit verifies what it hears and checks it with the detector suite. The unit
/// runs on the default roadside profile; on the Cohda MK5 RSU profile, whose brief
/// publishes no verification rate, a unit could price no verification and dropped every
/// frame as a verification-queue overflow — the control shows exactly that.
#[test]
fn a_roadside_unit_verifies_and_checks_what_it_hears() {
    let build = |profile: Option<&str>| {
        let mut s = grid(30.0, 900.0);
        s.security.verification_policy = "verify-all".to_string();
        s.detection.local = vec![ModelChoice::new(v2xw_engine::phase2::LEGACY_12)];
        cellular(&mut s);
        for x in [150.0, 450.0, 750.0, 1050.0] {
            s.actors.rsus.push(v2xw_engine::scenario::Rsu {
                site: None,
                position_m: Some([x, 300.0, 0.0]),
                roles: vec!["report-forward".to_string()],
                profile: profile.map(str::to_string),
                backhaul: None,
            });
        }
        s
    };
    let delivered_at_units = |rec: &MemoryRecorder| {
        records(rec, "node.rx")
            .iter()
            .filter(|r| r["rx"].as_u64().is_some_and(|n| n < 4))
            .filter(|r| r["outcome"] == "delivered")
            .count()
    };
    let (with_default, rec) = run(build(None));
    let (with_mk5, rec_mk5) = run(build(Some("rsu/cohda-mk5-rsu")));
    let (a, b) = (delivered_at_units(&rec), delivered_at_units(&rec_mk5));
    println!(
        "units delivered {a} frames on the default profile, {b} on the MK5 RSU; the suite \
         checked {} and {} messages",
        with_default.phase2.messages_checked, with_mk5.phase2.messages_checked
    );
    assert!(
        a > 0,
        "no roadside unit delivered a frame to its applications"
    );
    assert_eq!(
        b, 0,
        "the control: the MK5 RSU profile prices no verification"
    );
    assert!(with_default.phase2.messages_checked > with_mk5.phase2.messages_checked);
}
