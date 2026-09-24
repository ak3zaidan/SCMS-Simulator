//! **Validation against the frozen legacy engine.**
//!
//! ADR 0004 §1 retires the legacy fixed digests and says why: exactly one field in the
//! whole legacy dataset — `st_bbox` in `ma/ma_reports.jsonl` — escapes `round(x, 3)`, its
//! coordinates descend from `sin`/`cos`, and no libm rounds those correctly, so the same
//! seed differs by 1 to 16 ULP between Windows x86-64 and macOS arm64 and the aggregate
//! SHA-256 moves. **Byte equality against the legacy dataset is therefore not the test.**
//! What is compared here is *decisions and rates*, at two levels:
//!
//! | Level | What is held equal | What a difference means |
//! |---|---|---|
//! | **A — the detection pass** | the claim trace, message for message | a porting error, or a divergence this crate declares |
//! | **B — the engine** | the scenario shape: geometry, fleet, attacker fraction, operating point | what the two engines can *see* |
//!
//! Level A is the sharp one. The legacy detection pass is **extracted from
//! `legacy/scms_sim_ref/mock_pipeline/run.py` at test time and executed** — the six-check
//! motion closure plus the per-message block that carries the lagged reference, the heading
//! baseline, the six radio and envelope checks, the alpha-beta tracker, the bad-signature
//! and vulnerable-road-user gates, the streak gate and the reason ordering. Both
//! implementations are then handed the identical claim trace and their fingerprints are
//! diffed. A comparison against formulas typed into a Rust test would be a test of
//! somebody's memory; this one fails if either side drifts.
//!
//! Level B runs the legacy pipeline for real, once per attack type, and compares the
//! per-attack recall, false-positive rate and leading reason against the Rust harness on
//! the same geometry and the same operating point.
//!
//! # Skipping
//!
//! Both levels need `legacy/.venv` (`just legacy-setup`). Without it the tests print what
//! they would have run and pass, because a missing interpreter is an environment fact and
//! not a regression — and the message says so rather than staying silent.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use common::sim::{self, SimOptions};
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};
use v2xw_core::time::{SimTime, secs_to_ns};
use v2xw_threat::attack::{AttackKind, Attacker, AttackerView, Emission, HonestClaim};
use v2xw_threat::attack_legacy::{LegacyAttacker, LegacyAttackerParams};
use v2xw_threat::capability::{AttackSchedule, Capabilities};
use v2xw_threat::ctx::CollectingCtx;
use v2xw_threat::detect::{Detector, DetectorId, DetectorParams, Legacy12};
use v2xw_threat::obs::{
    LocalEnvironment, ObservedKind, ObservedMessage, SelfBelief, VerificationState,
};

/// The repository root.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The frozen reference's interpreter, or `None`.
fn python() -> Option<PathBuf> {
    let p = root()
        .join("legacy")
        .join(".venv")
        .join("bin")
        .join("python");
    p.exists().then_some(p)
}

/// Where this test's intermediate JSON goes.
fn workdir() -> PathBuf {
    let d = root().join("target").join("legacy-compare");
    std::fs::create_dir_all(&d).expect("the work directory must be creatable");
    d
}

// ======================================================================================
// Level A — the detection pass, on one claim trace
// ======================================================================================

/// The world's only road: the east-west line `y = 0`, which is also what the trace hands
/// the legacy `_offroad`.
struct OneRoad;

impl LocalEnvironment for OneRoad {
    fn distance_to_road_m(&self, _x_m: f64, y_m: f64) -> f64 {
        y_m.abs()
    }
}

/// The receiver, at the origin with a 500 m range — the legacy `radio_range_m`.
const RX_X: f64 = 0.0;
/// The receiver's north coordinate.
const RX_Y: f64 = 0.0;
/// The receiver's configured range, metres.
const RR_M: f64 = 500.0;
/// The claim trace's beacon interval, seconds — the legacy `dt`.
const DT_S: f64 = 1.0;
/// How many beacons per attack type.
const STEPS: u64 = 40;
/// The vehicle's constant speed, m/s — the legacy `nominal_speed`.
const SPEED_MPS: f64 = 15.0;
/// The broadcast position confidence, metres. Nonzero so the residual's tolerance is the
/// broadcast one on both sides rather than each side's own floor.
const CONF_M: f64 = 2.5;

/// One message of the trace, in the shape both engines read.
#[derive(serde::Serialize)]
struct TraceMsg {
    i: usize,
    t: f64,
    step: u64,
    digest: String,
    cx: f64,
    cy: f64,
    cs: f64,
    /// Heading in **degrees**, because that is the unit the legacy engine carries it in.
    ch: f64,
    conf: f64,
    msg_count: u32,
    cg: f64,
    sig_ok: bool,
    cvf: f64,
    cvt: f64,
    station_type: String,
    rx_x: f64,
    rx_y: f64,
    rr: f64,
    offroad_m: f64,
    /// The co-location census over this step's broadcasts, keyed `"cellx,celly,octant"`.
    cells: BTreeMap<String, u32>,
    /// The attack the sender was running, for the report.
    attack: String,
}

/// What the Rust suite made of one message.
#[derive(serde::Serialize)]
struct RustRow {
    i: usize,
    det: BTreeMap<String, f64>,
    fired: Vec<String>,
}

/// The legacy `DET_KEYS` order, which is also [`DetectorId::LEGACY_12`] plus the two gated
/// checks. Both gates are on in this trace, because the trace contains a
/// vulnerable-road-user declaration and an event message.
fn det_keys() -> Vec<String> {
    let mut v: Vec<String> = DetectorId::LEGACY_12
        .iter()
        .map(|d| d.as_str().to_string())
        .collect();
    v.push(DetectorId::VruImpersonation.as_str().to_string());
    v.push(DetectorId::DenmPlausibility.as_str().to_string());
    v
}

/// Seconds, for the legacy side.
fn secs(t: SimTime) -> f64 {
    if t == SimTime::MAX {
        1.0e9
    } else {
        t as f64 / 1e9
    }
}

/// Builds the trace and the Rust fingerprints for one attack type.
fn trace_one(kind: Option<AttackKind>, i0: usize, sender: u8) -> (Vec<TraceMsg>, Vec<RustRow>) {
    let mut ctx = CollectingCtx::new(20_260_922);
    let params = DetectorParams {
        generation_interval_s: DT_S,
        station_types_in_play: true,
        event_messages_in_play: true,
        ..DetectorParams::default()
    };
    let mut det = Legacy12::new(params);
    let mut attacker = kind.map(|k| {
        let mut p = LegacyAttackerParams::new(k);
        p.dt_s = DT_S;
        LegacyAttacker::new(
            v2xw_core::ids::NodeId::new(2),
            p,
            Capabilities::insider(20),
            AttackSchedule {
                from: 5 * v2xw_core::time::NS_PER_S,
                to: u64::MAX,
                ..AttackSchedule::default()
            },
            (0..6u8)
                .map(|i| [0xB0 | i, sender, 2, 3, 4, 5, 6, 7])
                .collect(),
        )
    });
    let _ = RngRegistry::new(1);
    let _ = (RngDomain::Attack, EntityRef::Global);

    let mut msgs = Vec::new();
    let mut rows = Vec::new();
    let mut i = i0;

    for s in 0..STEPS {
        let t: SimTime = secs_to_ns(s as f64 * DT_S);
        ctx.set_now(t);
        let honest = HonestClaim {
            x_m: SPEED_MPS * (s as f64) * DT_S,
            y_m: 0.0,
            speed_mps: SPEED_MPS,
            heading_rad: 0.0,
        };
        let me = SelfBelief {
            node: v2xw_core::ids::NodeId::new(2),
            believed_time: t,
            x_m: honest.x_m,
            y_m: honest.y_m,
            radio_range_m: RR_M,
        };
        // A distinct signer per sender. The legacy detection pass keys its per-subject
        // history on the digest and runs one `last_claimed` across the whole trace, so a
        // shared digest would splice every attack's history onto the previous attack's —
        // which is what an earlier draft of this test did, and it produced an
        // `implausibleAcceleration` of 1 944 675 at the seam where `t` jumped backwards.
        let mut em = Emission::honest([0xAA, sender, 0, 0, 0, 0, 0, 1], honest, t, 0, u64::MAX);
        if let Some(a) = attacker.as_mut() {
            let view = AttackerView {
                own_rx: &[],
                own_credentials: &[],
                crl_revocations_seen: None,
                own_belief: me,
                honest,
                believed_time: t,
            };
            a.observe(&mut ctx, &view);
            a.act(&mut ctx, &view, &mut em);
        }
        if em.suppressed {
            continue;
        }

        // Every message this emission puts on the air, beacon first.
        let mut on_air: Vec<ObservedMessage> = vec![observed(&em, t, CONF_M)];
        for g in &em.ghosts {
            on_air.push(observed(g, t, CONF_M));
        }
        for e in &em.events {
            let mut m = observed(&em, t, CONF_M);
            m.kind = ObservedKind::Denm(e.event_type.clone());
            m.claimed_x_m = e.x_m;
            m.claimed_y_m = e.y_m;
            m.claimed_speed_mps = e.claimed_speed_mps;
            on_air.push(m);
        }

        // The co-location census, built the legacy way: distinct certificates binned to a
        // `sybil_cell_m` grid and a 45° heading octant, over this step's broadcasts.
        // `run.py`: `cells = Counter((round(cx/cell), round(cy/cell), int(ch//45) % 8) …)`.
        let mut cells: BTreeMap<String, u32> = BTreeMap::new();
        for m in &on_air {
            if matches!(m.kind, ObservedKind::Denm(_)) {
                continue;
            }
            let key = format!(
                "{},{},{}",
                py_round(m.claimed_x_m / 3.0),
                py_round(m.claimed_y_m / 3.0),
                ((m.claimed_heading_rad.to_degrees().rem_euclid(360.0) / 45.0) as i64) % 8
            );
            *cells.entry(key).or_insert(0) += 1;
        }

        let rx = SelfBelief {
            node: v2xw_core::ids::NodeId::new(1),
            believed_time: t,
            x_m: RX_X,
            y_m: RX_Y,
            radio_range_m: RR_M,
        };
        for m in &on_air {
            let v = det.on_message(&mut ctx, &rx, m, &OneRoad);
            let mut d: BTreeMap<String, f64> = BTreeMap::new();
            for id in DetectorId::ALL {
                d.insert(
                    id.as_str().to_string(),
                    (v.fingerprint.get(id) * 1000.0).round() / 1000.0,
                );
            }
            rows.push(RustRow {
                i,
                det: d,
                fired: v
                    .fired
                    .iter()
                    .map(|o| o.detector.as_str().to_string())
                    .collect(),
            });
            msgs.push(TraceMsg {
                i,
                t: secs(t),
                step: s,
                digest: m.signer_hex(),
                cx: m.claimed_x_m,
                cy: m.claimed_y_m,
                cs: m.claimed_speed_mps,
                ch: m.claimed_heading_rad.to_degrees().rem_euclid(360.0),
                conf: m.claimed_pos_confidence_m,
                msg_count: m.repetitions,
                cg: secs(m.claimed_generation_time),
                sig_ok: m.verification.is_valid(),
                cvf: secs(m.cert_valid_from),
                cvt: secs(m.cert_valid_to),
                station_type: m.station_type.as_str().to_string(),
                rx_x: RX_X,
                rx_y: RX_Y,
                rr: RR_M,
                offroad_m: OneRoad.distance_to_road_m(m.claimed_x_m, m.claimed_y_m),
                cells: cells.clone(),
                attack: kind.map_or("none".to_string(), |k| k.as_str().to_string()),
            });
            i += 1;
        }
    }
    (msgs, rows)
}

/// Python's `round`: banker's rounding to even, which is what `round(cx / cell)` does and
/// what Rust's `f64::round` does not.
fn py_round(x: f64) -> i64 {
    let f = x.floor();
    let frac = x - f;
    let r = if (frac - 0.5).abs() < f64::EPSILON {
        let fi = f as i64;
        if fi % 2 == 0 { fi } else { fi + 1 }
    } else {
        x.round() as i64
    };
    r
}

fn observed(em: &Emission, t: SimTime, conf: f64) -> ObservedMessage {
    ObservedMessage {
        signer: em.signer,
        kind: ObservedKind::Beacon,
        received_at: t,
        claimed_generation_time: em.generation_time,
        claimed_x_m: em.x_m,
        claimed_y_m: em.y_m,
        claimed_speed_mps: em.speed_mps,
        claimed_heading_rad: em.heading_rad,
        claimed_pos_confidence_m: conf,
        repetitions: em.repetitions,
        cert_valid_from: em.cert_valid_from,
        cert_valid_to: em.cert_valid_to,
        station_type: em.station_type,
        verification: if em.signature_valid {
            VerificationState::Valid
        } else {
            VerificationState::BadSignature
        },
    }
}

/// The detector-pass comparison: identical claim trace, both implementations, diffed.
#[test]
fn the_ported_detection_pass_agrees_with_the_legacy_one_on_one_trace() {
    let Some(py) = python() else {
        eprintln!(
            "SKIPPED: no legacy interpreter at legacy/.venv/bin/python; run `just \
             legacy-setup`. The comparison would have executed the detection pass \
             extracted from legacy/scms_sim_ref/mock_pipeline/run.py over the trace this \
             test builds."
        );
        return;
    };

    let mut msgs: Vec<TraceMsg> = Vec::new();
    let mut rust: Vec<RustRow> = Vec::new();
    let mut kinds: Vec<Option<AttackKind>> = vec![None];
    for k in AttackKind::LEGACY_CATALOG {
        kinds.push(Some(k));
    }
    for k in AttackKind::COMBINED {
        kinds.push(Some(k));
    }
    for k in AttackKind::IDENTITY_SPOOF {
        kinds.push(Some(k));
    }
    let senders = kinds.len();
    for (n, k) in kinds.into_iter().enumerate() {
        let (m, r) = trace_one(k, msgs.len(), n as u8);
        msgs.extend(m);
        rust.extend(r);
    }
    eprintln!("trace: {} messages over {senders} senders", msgs.len());

    let trace_path = workdir().join("trace.json");
    let cfg = serde_json::json!({
        "seed": 1001,
        "dt": DT_S,
        "consistency_threshold_m": 5.0,
        "heading_threshold_deg": 35.0,
        "detector_lag_s": 1.5,
        "detector_z_threshold": 3.0,
        "detector_min_consec": 2,
        "sybil_min_certs": 4,
        "sybil_cell_m": 3.0,
        "art_max_m": 150.0,
        "offroad_tol_m": 15.0,
        "max_accel_mps2": 12.0,
        "freq_max": 6.0,
        "stale_max_s": 5.0,
        "report_prob": 1.0,
        "vru_max_plausible_speed_mps": 10.0,
        "gps_outlier_mag_m": 12.0,
        "denm_implausible_speed_mps": 6.0,
        "denm_benign_max_speed_mps": 4.0,
    });
    std::fs::write(
        &trace_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "cfg": cfg,
            "det_keys": det_keys(),
            "messages": msgs,
        }))
        .unwrap(),
    )
    .expect("the trace must be writable");

    let legacy_path = workdir().join("legacy_detect.json");
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("legacy")
        .join("detect_oracle.py");
    let status = Command::new(&py)
        .arg(&script)
        .arg(&trace_path)
        .arg(&legacy_path)
        .status()
        .expect("the legacy interpreter must be runnable");
    assert!(
        status.success(),
        "the legacy detection pass must run; see the output above"
    );

    let legacy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&legacy_path).unwrap()).unwrap();
    let legacy_rows = legacy["rows"].as_array().unwrap();
    assert_eq!(
        legacy_rows.len(),
        rust.len(),
        "both sides must judge the same number of messages"
    );

    // The comparison. `sybilCoLocation` is excluded from the score diff and reported
    // separately: this crate's own documentation states that the legacy census was global
    // over every broadcast in the step while this port counts only what the receiver heard
    // in a window, so a disagreement there is a declared divergence and not a defect.
    let mut mismatched: BTreeMap<String, u64> = BTreeMap::new();
    let mut worst: BTreeMap<String, (f64, usize, String)> = BTreeMap::new();
    let mut fired_diff: u64 = 0;
    let mut fired_examples: Vec<String> = Vec::new();
    let keys = det_keys();

    for (row, lrow) in rust.iter().zip(legacy_rows) {
        let ldet = lrow["det"].as_object().unwrap();
        for k in &keys {
            let a = row.det.get(k).copied().unwrap_or(0.0);
            let b = ldet
                .get(k)
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            let d = (a - b).abs();
            // The legacy writes `round(x, 3)` and so does this crate's `SCORE_Q`, so the
            // tolerance is half a quantum and not an epsilon.
            if d > 5e-4 {
                *mismatched.entry(k.clone()).or_insert(0) += 1;
                let slot = worst.entry(k.clone()).or_insert((0.0, 0, String::new()));
                if d > slot.0 {
                    *slot = (
                        d,
                        row.i,
                        format!(
                            "{} rust {a} legacy {b} (msg #{} of attack {})",
                            k, row.i, msgs[row.i].attack
                        ),
                    );
                }
            }
        }
        let a: BTreeSet<String> = row
            .fired
            .iter()
            .filter(|s| s.as_str() != "sybilCoLocation")
            .cloned()
            .collect();
        let b: BTreeSet<String> = lrow["fired"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| *s != "sybilCoLocation")
            .map(str::to_string)
            .collect();
        if a != b {
            fired_diff += 1;
            if fired_examples.len() < 12 {
                fired_examples.push(format!(
                    "#{} attack {}: rust {a:?} legacy {b:?}",
                    row.i, msgs[row.i].attack
                ));
            }
        }
    }

    eprintln!(
        "--- level A: per-message score diff over {} messages ---",
        rust.len()
    );
    for k in &keys {
        let n = mismatched.get(k).copied().unwrap_or(0);
        let w = worst
            .get(k)
            .map(|(d, _, s)| format!("max |Δ| {d:.6}  e.g. {s}"))
            .unwrap_or_else(|| "identical".to_string());
        eprintln!("  {k:28} mismatches {n:6}  {w}");
    }
    // The one declared divergence, characterised rather than waved at.
    let mut sybil_rust_lower = 0u64;
    let mut sybil_rust_higher = 0u64;
    for (row, lrow) in rust.iter().zip(legacy_rows) {
        let a = row.det.get("sybilCoLocation").copied().unwrap_or(0.0);
        let b = lrow["det"]["sybilCoLocation"].as_f64().unwrap_or(0.0);
        if (a - b).abs() > 5e-4 {
            if a < b {
                sybil_rust_lower += 1;
            } else {
                sybil_rust_higher += 1;
            }
        }
    }
    eprintln!(
        "sybilCoLocation: {sybil_rust_lower} messages where this port scores LOWER than          the legacy census, {sybil_rust_higher} where it scores higher"
    );
    eprintln!("fired-set disagreements (sybilCoLocation excluded): {fired_diff}");
    for e in &fired_examples {
        eprintln!("  {e}");
    }

    // The twelve checks other than `sybilCoLocation` must agree exactly: identical inputs,
    // ported formulas, the same quantum.
    let mut offenders: Vec<String> = Vec::new();
    for k in &keys {
        if k == "sybilCoLocation" {
            continue;
        }
        if let Some(n) = mismatched.get(k)
            && *n > 0
        {
            offenders.push(format!("{k}: {n} of {}", rust.len()));
        }
    }
    assert!(
        offenders.is_empty(),
        "the ported checks must reproduce the legacy scores on an identical trace; \
         disagreements: {offenders:?}"
    );
    assert_eq!(
        fired_diff, 0,
        "the ported streak gate and reason ordering must reproduce the legacy fired set"
    );
}

// ======================================================================================
// Level B — the engine, on the same scenario shape
// ======================================================================================

/// One attack type's measured outcome on one engine.
#[derive(Debug, Clone, Default)]
struct Rates {
    attackers: u64,
    reports: u64,
    tp: u64,
    fp: u64,
    fn_: u64,
    tn: u64,
    recall: Option<f64>,
    fpr: Option<f64>,
    revoked: u64,
    lead: String,
}

fn rust_rates(kind: AttackKind, ideal: bool) -> Rates {
    let mut opts = SimOptions::new(21, 14_400.0, 0.25).with_attack(kind);
    opts.max_vehicles = 60;
    if ideal {
        opts = opts.ideal();
    }
    let out = sim::run(&opts);
    let (report, vehicle) = sim::score(&out);
    let lead = out
        .firings
        .iter()
        .max_by_key(|(_, v)| **v)
        .map(|(k, v)| format!("{k}={v}"))
        .unwrap_or_else(|| "-".to_string());
    Rates {
        attackers: out.attackers,
        reports: out.reports,
        tp: report.tp,
        fp: report.fp,
        fn_: report.fn_,
        tn: report.tn,
        recall: report
            .recall(1, v2xw_metrics::stats::ConfidenceLevel::P95)
            .point(),
        fpr: report
            .fpr(1, v2xw_metrics::stats::ConfidenceLevel::P95)
            .point(),
        revoked: vehicle.tp + vehicle.fp,
        lead,
    }
}

/// Runs the frozen legacy engine over the catalog and returns its measured rates.
fn legacy_rates(py: &PathBuf, regime: &str) -> serde_json::Value {
    let out = workdir().join(format!("legacy_{regime}.json"));
    if !out.exists() {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("legacy")
            .join("engine_rates.py");
        let status = Command::new(py)
            .arg(&script)
            .arg(&out)
            .arg(regime)
            // 60 s, a 100 ms legacy step, 60 vehicles, a quarter of them attackers, seed
            // 1001. 100 ms rather than the legacy `dt` default of 1 s: `dt` is the legacy
            // engine's step *and* its beacon interval, and the Rust node's `BsmGenerator`
            // runs at the J2945/1 10 Hz cadence, so 0.1 is what puts the two fleets on the
            // same beacon rate. It costs about 18 s per attack type and the result is
            // cached in `target/legacy-compare/`.
            .args(["60", "0.1", "60", "0.25", "1001"])
            .env("V2XW_LEGACY_OUT", workdir().join("runs"))
            .status()
            .expect("the legacy interpreter must be runnable");
        assert!(status.success(), "the legacy engine sweep must run");
    }
    serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap()
}

/// The engine-level comparison: same geometry, same fleet size, same attacker fraction,
/// same operating point, both engines, per attack type.
#[test]
fn the_two_engines_agree_on_which_attacks_are_detectable() {
    let Some(py) = python() else {
        eprintln!(
            "SKIPPED: no legacy interpreter at legacy/.venv/bin/python; run `just \
             legacy-setup`."
        );
        return;
    };
    let legacy = legacy_rates(&py, "ideal");
    let results = legacy["results"].as_object().unwrap();

    eprintln!(
        "legacy: {} s, dt {}, {} vehicles, attacker_pct {}",
        legacy["duration_s"], legacy["dt"], legacy["n_vehicles"], legacy["attacker_pct"]
    );
    eprintln!(
        "{:<20} {:>18} {:>18}   {:>10} {:>10}",
        "attack", "legacy recall/fpr", "rust recall/fpr", "legacy lead", "rust lead"
    );

    let mut agree = 0usize;
    let mut disagree: Vec<String> = Vec::new();
    for kind in AttackKind::LEGACY_CATALOG {
        let name = kind.as_str();
        let Some(l) = results.get(name) else { continue };
        let lr = l["recall"].as_f64();
        let lf = l["fpr"].as_f64();
        let ll = l["leading"]
            .as_object()
            .map(|o| {
                let mut v: Vec<(&String, i64)> = o
                    .iter()
                    .map(|(k, x)| (k, x.as_i64().unwrap_or(0)))
                    .collect();
                v.sort_by_key(|(_, n)| -n);
                v.first()
                    .map(|(k, n)| format!("{k}={n}"))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let r = rust_rates(kind, true);
        eprintln!(
            "{name:<20} {:>8.3}/{:>8.3} {:>8.3}/{:>8.3}   {ll:>10} {:>10}  \
             (legacy att {} rep {} | rust att {} rep {})",
            lr.unwrap_or(f64::NAN),
            lf.unwrap_or(f64::NAN),
            r.recall.unwrap_or(f64::NAN),
            r.fpr.unwrap_or(f64::NAN),
            r.lead,
            l["attackers"],
            l["reports"],
            r.attackers,
            r.reports,
        );
        // "Detectable at all" is the decision both engines have to agree on. A rate
        // difference is expected — the two engines see different traffic — but an attack
        // one engine catches and the other never does is either a porting error or a
        // stated difference in what the engines can see.
        let ld = lr.unwrap_or(0.0) > 0.0;
        let rd = r.recall.unwrap_or(0.0) > 0.0;
        if ld == rd {
            agree += 1;
        } else {
            disagree.push(format!(
                "{name}: legacy recall {:?}, rust recall {:?}",
                lr, r.recall
            ));
        }
    }
    eprintln!(
        "detectability agreement: {agree} of {}",
        AttackKind::LEGACY_CATALOG.len()
    );
    for d in &disagree {
        eprintln!("  DISAGREE {d}");
    }
    assert!(
        agree >= 17,
        "the two engines must agree about which attacks are detectable at all; \
         disagreements: {disagree:?}"
    );
}
