//! Every ported constant, asserted against a number **read out of the legacy source at
//! test time**.
//!
//! A conformance test that compares this crate's defaults against literals typed into the
//! test is a test of somebody's memory. It passes when both copies drift together, which
//! is the failure it exists to catch. So this file opens
//! `legacy/scms_sim_ref/mock_pipeline/run.py` and
//! `legacy/reference/jvm/ScmsBeaconApp.java`, extracts the values, and compares those.
//!
//! If a legacy file is moved or rewritten, these tests fail loudly rather than silently
//! passing — which is the point: the port's claim is "these are the legacy numbers", and a
//! claim nobody can check is not a claim.

use std::path::PathBuf;

use v2xw_threat::attack::AttackKind;
use v2xw_threat::attack_legacy::{LegacyAttackerParams, Magnitudes};
use v2xw_threat::detect::DetectorParams;
use v2xw_threat::ma::MaParams;

fn legacy_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/v2xw-threat.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("legacy")
}

fn read(rel: &str) -> String {
    let p = legacy_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!(
            "the legacy reference at {} must be readable: {e}",
            p.display()
        )
    })
}

fn py() -> String {
    read("scms_sim_ref/mock_pipeline/run.py")
}

fn jvm() -> String {
    read("reference/jvm/ScmsBeaconApp.java")
}

/// The first float literal in `text`, ignoring anything after a `#` comment marker.
fn first_float(text: &str) -> Option<f64> {
    let code = text.split('#').next().unwrap_or(text);
    let bytes: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.') {
                i += 1;
            }
            let s: String = bytes[start..i].iter().collect();
            return s.parse().ok();
        }
        i += 1;
    }
    None
}

/// Every float literal in `text`, ignoring `#` comments, in source order.
fn floats_in(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for line in text.lines() {
        let code = line.split('#').next().unwrap_or(line);
        let chars: Vec<char> = code.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                if let Ok(v) = s.parse::<f64>() {
                    out.push(v);
                }
            } else {
                i += 1;
            }
        }
    }
    out
}

/// A `PipelineConfig` field default: the line `    name: type = value` inside the
/// dataclass.
fn config_default(src: &str, field: &str) -> f64 {
    let needle = format!("\n    {field}: ");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("PipelineConfig.{field} is gone from the legacy source"));
    let line = src[start + 1..].lines().next().unwrap();
    let rhs = line
        .split_once('=')
        .unwrap_or_else(|| panic!("PipelineConfig.{field} has no default"))
        .1;
    // A default may be a module constant rather than a literal.
    if let Some(v) = first_float(rhs) {
        return v;
    }
    let name = rhs.split('#').next().unwrap().trim();
    module_const(src, name)
}

/// A module-level constant: the line `NAME = value` at column zero.
fn module_const(src: &str, name: &str) -> f64 {
    // `_SYBIL_MIN` and friends sit at column zero too.
    let needle = format!("\n{name} = ");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("the legacy constant {name} is gone"));
    let line = src[start + 1..].lines().next().unwrap();
    let rhs = line.split_once('=').unwrap().1;
    let expr = rhs.split('#').next().unwrap().trim();
    // A derived constant such as DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS = X + 0.5 must be
    // resolved as a sum, not by grabbing the first literal in it.
    if let Some((lhs, add)) = expr.split_once('+') {
        return module_const(src, lhs.trim()) + add.trim().parse::<f64>().unwrap();
    }
    first_float(expr).unwrap_or_else(|| panic!("cannot read the legacy constant {name}: {expr}"))
}

/// A Java `private static final` constant, whether a literal or an `envD`/`envI` default.
fn java_const(src: &str, name: &str) -> f64 {
    let needle = format!("{name} = ");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("the JVM reference constant {name} is gone"));
    let rest = &src[start + needle.len()..];
    let stop = rest.find(';').unwrap_or(rest.len());
    let expr = &rest[..stop];
    // `envD("SCMS_KF_THRESH", 4.0)` carries its default last; a bare literal, possibly
    // followed by a second declaration on the same line (`KF_ALPHA = 0.5, KF_BETA = 0.3`),
    // carries it first.
    if expr.contains("env") {
        let mut fs = floats_in(expr);
        return fs
            .pop()
            .unwrap_or_else(|| panic!("cannot read the JVM constant {name} from {expr:?}"));
    }
    first_float(expr).unwrap_or_else(|| panic!("cannot read the JVM constant {name} from {expr:?}"))
}

/// The body of one `attack_claim` branch: everything from its `typ ==` guard to the next
/// one.
fn claim_branch(src: &str, kind: &str) -> String {
    let start = src
        .find(&format!("typ == \"{kind}\":"))
        .unwrap_or_else(|| panic!("attack_claim has no branch for {kind}"));
    let rest = &src[start..];
    let end = rest[1..].find("elif typ ==").map_or(rest.len(), |i| i + 1);
    rest[..end].to_string()
}

fn assert_branch_has(src: &str, kind: &str, want: &[f64]) {
    let body = claim_branch(src, kind);
    let got = floats_in(&body);
    for w in want {
        assert!(
            got.iter().any(|g| (g - w).abs() < 1e-12),
            "the legacy {kind} branch no longer contains {w}; it has {got:?}"
        );
    }
}

#[test]
fn the_legacy_sources_are_where_this_crate_says_they_are() {
    assert!(py().contains("def attack_claim("));
    assert!(py().contains("def detectors("));
    assert!(jvm().contains("KF_ALPHA"));
}

#[test]
fn detector_thresholds_match_the_legacy_source() {
    let src = py();
    let p = DetectorParams::default();
    for (field, got) in [
        ("consistency_threshold_m", p.consistency_threshold_m),
        ("heading_threshold_deg", p.heading_threshold_deg),
        ("detector_lag_s", p.detector_lag_s),
        ("detector_z_threshold", p.z_threshold),
        ("art_max_m", p.art_max_m),
        ("offroad_tol_m", p.offroad_tol_m),
        ("max_accel_mps2", p.max_accel_mps2),
        ("freq_max", p.freq_max),
        ("stale_max_s", p.stale_max_s),
        ("sybil_cell_m", p.sybil_cell_m),
        ("gps_outlier_mag_m", p.gps_outlier_mag_m),
        ("vru_max_plausible_speed_mps", p.vru_max_plausible_speed_mps),
        ("denm_implausible_speed_mps", p.denm_implausible_speed_mps),
        ("denm_benign_max_speed_mps", p.denm_benign_max_speed_mps),
    ] {
        let want = config_default(&src, field);
        assert_eq!(got, want, "{field}: port has {got}, legacy has {want}");
    }
    assert_eq!(
        f64::from(p.min_consecutive),
        config_default(&src, "detector_min_consec")
    );
    assert_eq!(
        f64::from(p.sybil_min_certs),
        config_default(&src, "sybil_min_certs")
    );
    // The brake bound is derived in the legacy source too, and must stay derived: a
    // separately configured bound could sit below the benign trigger and flag a genuine
    // emergency brake.
    assert_eq!(
        p.denm_brake_implausible_speed_mps(),
        module_const(&src, "DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS")
    );
    assert_eq!(
        p.denm_benign_max_speed_mps,
        module_const(&src, "DENM_BENIGN_MAX_SPEED_MPS")
    );
    assert_eq!(
        p.vru_max_plausible_speed_mps,
        module_const(&src, "VRU_MAX_PLAUSIBLE_SPEED_MPS")
    );
    assert_eq!(p.sybil_min_certs, 4, "_SYBIL_MIN");
    assert_eq!(
        f64::from(p.sybil_min_certs),
        module_const(&src, "_SYBIL_MIN")
    );
    assert_eq!(p.sybil_cell_m, module_const(&src, "_CELL_M"));
}

#[test]
fn the_in_function_detector_constants_match_the_legacy_source() {
    let src = py();
    let p = DetectorParams::default();
    let det = {
        let start = src.find("    def detectors(").unwrap();
        let rest = &src[start..];
        let end = rest.find("    DET_KEYS = ").unwrap();
        rest[..end].to_string()
    };
    let got = floats_in(&det);
    for (name, want) in [
        ("jerk_slack_factor", p.jerk_slack_factor),
        ("tolerance_floor_factor", p.tolerance_floor_factor),
        ("frozen_score", p.frozen_score),
        ("frozen_stale_score", p.frozen_stale_score),
        ("frozen_min_speed_mps", p.frozen_min_speed_mps),
    ] {
        assert!(
            got.iter().any(|g| (g - want).abs() < 1e-12),
            "detectors() no longer contains {name} = {want}; it has {got:?}"
        );
    }
    // The heading gate and the hard-fail score live in the detection pass.
    assert!(
        src.contains("max(5.0, 2.5 * conf)"),
        "the heading gate moved"
    );
    assert_eq!(p.heading_min_disp_m, 5.0);
    assert_eq!(p.heading_conf_factor, 2.5);
    assert!(
        src.contains("if cs > 3.0 and"),
        "the heading speed gate moved"
    );
    assert_eq!(p.heading_min_speed_mps, 3.0);
    assert!(
        src.contains("det[\"signatureVerification\"] = 1.5"),
        "the signature-failure score moved"
    );
    assert_eq!(p.hard_fail_score, 1.5);
    assert!(
        src.contains("1.5 if (t > b[\"cvt\"] + 1.0 or t < b[\"cvf\"] - 1.0) else 0.0"),
        "the certificate-validity check moved"
    );
    assert_eq!(p.cert_slack_s, 1.0);
    // The soft feature's normaliser.
    assert!(
        src.contains("(2 * cfg.consistency_threshold_m + conf)"),
        "the alpha-beta normaliser moved"
    );
}

#[test]
fn the_tracker_gains_come_from_the_jvm_reference() {
    let src = jvm();
    let p = DetectorParams::default();
    assert_eq!(p.kalman_alpha, java_const(&src, "KF_ALPHA"));
    assert_eq!(p.kalman_beta, java_const(&src, "KF_BETA"));
    // The JVM reference gates the tracker at a normalised residual of 4 and carries the
    // score as `kfNorm / KF_THRESH`. This port keeps the residual itself soft — it never
    // fires — so the gate is not a parameter here; the constant is checked so that a
    // change in the reference is visible rather than silent.
    assert_eq!(java_const(&src, "KF_THRESH"), 4.0);
    assert!(
        src.contains("det.put(\"kalmanConsistency\", kfNorm / KF_THRESH)"),
        "the reference no longer normalises the tracker residual by its gate"
    );
}

#[test]
fn the_ma_operating_point_matches_the_legacy_source() {
    let src = py();
    let p = MaParams::default();
    assert_eq!(
        p.report_threshold_k as f64,
        config_default(&src, "report_threshold_k")
    );
    assert_eq!(
        p.revoke_min_seconds as f64,
        config_default(&src, "revoke_min_seconds")
    );
    assert_eq!(p.revoke_persist_s, config_default(&src, "revoke_persist_s"));
    assert_eq!(p.revoke_window_s, config_default(&src, "revoke_window_s"));
    assert_eq!(
        f64::from(p.reputation_max),
        config_default(&src, "reputation_max")
    );
    assert_eq!(
        f64::from(p.report_budget),
        config_default(&src, "report_budget")
    );
}

#[test]
fn attacker_parameters_match_the_legacy_source() {
    let src = py();
    let p = LegacyAttackerParams::default();
    assert_eq!(p.intensity, config_default(&src, "attack_intensity"));
    assert_eq!(p.dt_s, config_default(&src, "dt"));
    assert_eq!(f64::from(p.dos_burst), config_default(&src, "dos_burst"));
    assert_eq!(p.delay_s, config_default(&src, "delay_s"));
    assert_eq!(
        f64::from(p.sybil_ghosts),
        config_default(&src, "sybil_ghosts")
    );
    assert_eq!(
        p.denm_fake_rate,
        module_const(&src, "DENM_FAKE_FALLBACK_RATE")
    );
    assert_eq!(
        p.denm_rate_window_s,
        module_const(&src, "DENM_RATE_WINDOW_S")
    );
    assert!(src.contains("cvt = t - 5.0"), "the ExpiredCert edit moved");
    assert_eq!(p.expired_cert_lag_s, 5.0);
    assert!(src.contains("cvf = t + 5.0"), "the NotYetValid edit moved");
    assert_eq!(p.not_yet_valid_lead_s, 5.0);
    assert!(
        src.contains("sr.uniform(-1, 1)"),
        "the Sybil ghost jitter moved"
    );
    assert_eq!(p.sybil_jitter_m, 1.0);
}

#[test]
fn every_attack_magnitude_matches_the_legacy_attack_claim_branch() {
    let src = py();
    let m = Magnitudes::default();
    assert_branch_has(&src, "ConstPosOffset", &[m.const_pos_offset_m]);
    assert_branch_has(&src, "RandomPos", &[m.random_pos_half_range_m]);
    assert_branch_has(
        &src,
        "Teleport",
        &[m.teleport_dx_m, m.teleport_dy_m, m.teleport_period_s as f64],
    );
    assert_branch_has(
        &src,
        "SineWavePos",
        &[m.sine_pos_amplitude_m, m.sine_pos_omega_rad_s],
    );
    assert_branch_has(&src, "ConstSpeedOffset", &[m.const_speed_offset_mps]);
    assert_branch_has(&src, "RandomSpeed", &[m.random_speed_max_mps]);
    assert_branch_has(&src, "StopAndGo", &[m.stop_and_go_speed_mps]);
    assert_branch_has(&src, "HeadingOffset", &[m.heading_offset_deg]);
    assert_branch_has(&src, "DataReplay", &[m.replay_lag_samples as f64]);
    assert_branch_has(
        &src,
        "SlowDrift",
        &[m.slow_drift_ramp_mps, m.slow_drift_max_rate_mps],
    );
    assert_branch_has(&src, "AlongRoadOffset", &[m.along_road_offset_m]);
    assert_branch_has(&src, "DoSRandom", &[m.random_pos_half_range_m]);
    assert_branch_has(
        &src,
        "Disruptive",
        &[
            m.disruptive_pos_half_range_m,
            m.disruptive_speed_up_mps,
            m.disruptive_speed_down_mps,
            m.disruptive_heading_deg,
        ],
    );
    assert_branch_has(
        &src,
        "PosSpeedInconsistent",
        &[m.pos_speed_inconsistent_drop_mps],
    );
    assert_branch_has(
        &src,
        "PosHeadingInconsistent",
        &[m.pos_heading_swing_m, m.pos_heading_omega_rad_s],
    );
    assert_branch_has(
        &src,
        "EventualStop",
        &[
            m.eventual_stop_delay_s,
            m.eventual_stop_base_speed_mps,
            m.eventual_stop_scale_mps,
        ],
    );
    assert_branch_has(
        &src,
        "VruPositionSpoof",
        &[
            m.vru_spoof_speed_mps,
            m.vru_spoof_radius_m,
            m.vru_spoof_omega_rad_s,
        ],
    );
    // `ConstPos` and `ReversedHeading` have no magnitude at all, which is what
    // `is_magnitude_scalable` says; check the legacy source still agrees.
    assert!(src.contains("ch = (mheading + 180.0) % 360.0"));
    assert!(!AttackKind::ReversedHeading.is_magnitude_scalable());
}

#[test]
fn the_catalog_and_its_order_match_the_legacy_source() {
    let src = py();
    let list = |name: &str| -> Vec<String> {
        let start = src.find(&format!("\n{name} = (")).unwrap();
        let rest = &src[start..];
        let end = rest.find(")\n").unwrap();
        rest[..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    };
    let catalog = list("ATTACK_CATALOG");
    let ours: Vec<String> = AttackKind::LEGACY_CATALOG
        .iter()
        .map(|k| k.as_str().to_string())
        .collect();
    assert_eq!(ours, catalog, "ATTACK_CATALOG order is frozen");
    for (name, ours) in [
        ("COMBINED_ATTACKS", &AttackKind::COMBINED[..]),
        ("IDENTITY_SPOOF_ATTACKS", &AttackKind::IDENTITY_SPOOF[..]),
        ("DENM_ATTACKS", &AttackKind::DENM[..]),
    ] {
        let want = list(name);
        let got: Vec<String> = ours.iter().map(|k| k.as_str().to_string()).collect();
        assert_eq!(got, want, "{name}");
    }
    // The port adds exactly one type the legacy engine never had.
    let legacy_total = catalog.len()
        + AttackKind::COMBINED.len()
        + AttackKind::IDENTITY_SPOOF.len()
        + AttackKind::DENM.len();
    assert_eq!(legacy_total, 28);
    assert_eq!(AttackKind::ALL.len(), legacy_total + 1);
    assert!(AttackKind::parse("SelectiveDrop").is_some());
}

#[test]
fn the_unscalable_set_matches_the_legacy_source() {
    let src = py();
    let start = src
        .find("_UNSCALABLE_MAGNITUDE_TYPES = frozenset({")
        .unwrap();
    let rest = &src[start..];
    let end = rest.find("})").unwrap();
    let mut want: Vec<String> = rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    want.sort();
    let mut got: Vec<String> = AttackKind::ALL
        .iter()
        .filter(|k| !k.is_magnitude_scalable())
        .map(|k| k.as_str().to_string())
        .collect();
    got.sort();
    assert_eq!(got, want);
}

#[test]
fn the_detector_key_order_matches_the_legacy_column_order() {
    let src = py();
    let start = src.find("    DET_KEYS = (").unwrap();
    let rest = &src[start..];
    let end = rest.find(")\n").unwrap();
    let want: Vec<String> = rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let got: Vec<String> = v2xw_threat::detect::DetectorId::LEGACY_12
        .iter()
        .map(|d| d.as_str().to_string())
        .collect();
    assert_eq!(
        got, want,
        "the detnorm_* column order is the legacy on-disk order"
    );
    // The two gated checks are appended after the twelve, and the soft feature is last.
    assert!(src.contains("DET_KEYS + (\"vruImpersonation\",)"));
    assert!(src.contains("DET_KEYS + (\"denmPlausibility\",)"));
    assert!(src.contains("SOFT_KEYS = (\"kalmanConsistency\",)"));
    let all: Vec<&str> = v2xw_threat::detect::DetectorId::ALL
        .iter()
        .map(|d| d.as_str())
        .collect();
    assert_eq!(
        &all[12..],
        &["vruImpersonation", "denmPlausibility", "kalmanConsistency"]
    );
}

#[test]
fn the_motion_key_set_matches_the_legacy_source() {
    let src = py();
    let start = src.find("    MOTION_KEYS = (").unwrap();
    let rest = &src[start..];
    let end = rest.find(")\n").unwrap();
    let want: Vec<String> = rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let got: Vec<String> = v2xw_threat::detect::DetectorId::MOTION
        .iter()
        .map(|d| d.as_str().to_string())
        .collect();
    assert_eq!(got, want);
}

#[test]
fn the_forged_report_distributions_match_the_legacy_collusion_pass() {
    let src = py();
    let p = v2xw_threat::report::ForgeryProfile::default();
    assert!(
        src.contains("cfab.uniform(1.05, 4.0)"),
        "the fabricated leading score moved"
    );
    assert_eq!(p.leading_score, (1.05, 4.0));
    assert!(src.contains("cfab.randint(1, 2) / cfg.sybil_min_certs"));
    assert_eq!(p.sybil_count, (1, 2));
    assert!(src.contains("cfab.randint(1, 3) / cfg.freq_max"));
    assert_eq!(p.beacon_count, (1, 3));
    assert!(src.contains("cfab.uniform(0.0, 0.15)"));
    assert_eq!(p.stale_score, (0.0, 0.15));
    assert!(src.contains("cfab.uniform(2.0, 9.0)"));
    assert_eq!(p.pos_confidence_m, (2.0, 9.0));
}

#[test]
fn the_falsified_label_rule_matches_the_legacy_source() {
    let src = py();
    // run.py, the broadcast pre-pass: position > 1 m, speed > 1 m/s, heading > 5 deg,
    // extra messages, a stale generation time, a bad signature, a bad certificate, or a
    // false station type.
    assert!(src.contains("math.hypot(cx - mx, cy - my) > 1.0"));
    assert!(src.contains("abs(cs - tspeed) > 1.0"));
    assert!(src.contains("_ang_diff(ch, theading) > 5.0"));
    assert!(src.contains("msg_count > 1"));
    assert!(src.contains("or not sig_ok or cert_bad"));
    assert!(src.contains("(declared_station == \"vru\" and not tx.is_vru)"));
}
