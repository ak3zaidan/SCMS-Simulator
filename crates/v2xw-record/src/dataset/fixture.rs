//! A deterministic synthetic run that exercises every channel the dataset exporters read.
//!
//! `crate::fixture` produces a run for the *container* — frames, chunks, seeking. This one
//! produces a run for the *dataset*: spawns with labels, pseudonym changes, attacker
//! actions, receptions, verifications, detector observations, reports, cases, decisions and
//! revocation stages, in the shapes 03-interfaces.md §14 declares.
//!
//! It exists because the engine does not yet emit most of these channels — Phase 1 emits
//! `gt.kinematics`, `node.tx` and `metric.sample` — and an exporter whose only test data is
//! the three channels that exist today would be untested where it matters. The fixture is
//! not a stand-in for an engine run: it is a record stream in the declared shapes, which is
//! exactly what the exporter's contract is against.
//!
//! # Determinism without an RNG
//!
//! Nothing here draws a random number. Every value is a closed-form function of the actor
//! index and the step, so the fixture is reproducible without touching an RNG stream and
//! without the exporter's tests depending on the engine's random sequence. Where a value
//! needs to look irregular — a detector score, a position offset — it comes from an
//! integer mix, not from a generator.

use v2xw_core::math::q3;

use crate::reader::RecordedRecord;

/// The shape of a synthetic dataset run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetShape {
    /// How many devices, of which the first [`DatasetShape::attackers`] are attackers.
    pub devices: u32,
    /// How many of the devices attack.
    pub attackers: u32,
    /// How many of the devices are faulty rather than malicious. Taken from the end of
    /// the fleet, so a device is never both.
    pub faulty: u32,
    /// How many infrastructure reporters. Their certificates are not vehicle pseudonyms,
    /// which is the case the frozen audit's `R3` check makes an allowance for.
    pub rsus: u32,
    /// How many 100 ms steps to run.
    pub steps: u32,
    /// How many pseudonyms each device rotates through.
    pub pseudonyms: u32,
    /// Whether the authority revokes the attackers it convicts.
    pub revoke: bool,
    /// Whether one benign device is framed by a colluding attacker, which is what makes
    /// `malicious_false_report` and `false_positive` appear in the labels.
    pub collusion: bool,
}

impl Default for DatasetShape {
    fn default() -> Self {
        DatasetShape {
            devices: 6,
            attackers: 2,
            faulty: 1,
            rsus: 1,
            steps: 40,
            pseudonyms: 2,
            revoke: true,
            collusion: true,
        }
    }
}

impl DatasetShape {
    /// A shape with no attacker and no revocation — the all-benign edge case, where every
    /// report is a false positive and no device should have been revoked.
    #[must_use]
    pub fn all_benign() -> Self {
        DatasetShape {
            attackers: 0,
            faulty: 0,
            revoke: false,
            collusion: false,
            ..Default::default()
        }
    }

    fn is_attacker(&self, actor: u32) -> bool {
        actor < self.attackers
    }

    fn is_faulty(&self, actor: u32) -> bool {
        !self.is_attacker(actor) && actor >= self.devices.saturating_sub(self.faulty)
    }

    fn is_rsu(&self, actor: u32) -> bool {
        actor >= self.devices && actor < self.devices + self.rsus
    }

    fn total_actors(&self) -> u32 {
        self.devices + self.rsus
    }
}

/// A pseudonym digest for one device's `k`th certificate.
///
/// A truncated SHA-256 of a domain-separated string, which is what the legacy pipeline
/// did and what a HashedId8 looks like on the wire: 16 hex characters.
#[must_use]
pub fn digest_of(actor: u32, k: u32) -> String {
    let hex = v2xw_core::hash::sha256_hex(format!("v2xw-fixture-pseudonym|{actor}|{k}").as_bytes());
    hex[..16].to_string()
}

/// A value in `[0, 1)` from an integer mix — irregular without an RNG.
fn mix(a: u64, b: u64) -> f64 {
    let h = v2xw_core::hash::sha256(format!("v2xw-fixture-mix|{a}|{b}").as_bytes());
    let v = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    f64::from(v) / f64::from(u32::MAX)
}

fn rec(channel: &str, t: u64, value: &serde_json::Value) -> RecordedRecord {
    RecordedRecord {
        channel: channel.to_string(),
        sim_time: t,
        json: serde_json::to_vec(value).expect("a fixture record serialises"),
    }
}

const STEP_NS: u64 = 100_000_000;

/// The whole record stream, in emission order.
///
/// Channels covered: `gt.spawn`, `gt.kinematics`, `gt.attack.action`, `sec.cert`,
/// `node.tx`, `phy.rx`, `node.verify`, `mac.cbr`, `node.telemetry`, `det.observation`,
/// `ma.report`, `ma.case`, `ma.decision`, `proto.revocation`.
#[must_use]
pub fn records(shape: &DatasetShape) -> Vec<RecordedRecord> {
    let mut out = Vec::new();

    // ---- spawns, with their labels ----------------------------------------------
    for actor in 0..shape.total_actors() {
        let class = if shape.is_rsu(actor) { "rsu" } else { "car" };
        out.push(rec(
            "gt.spawn",
            0,
            &serde_json::json!({
                "t": 0,
                "actor": actor,
                "is_attacker": shape.is_attacker(actor),
                "attacker_role": if shape.is_attacker(actor) { "position_falsifier" } else { "none" },
                "colluding_group_id": if shape.collusion && shape.is_attacker(actor) { Some("grp_0") } else { None },
                "is_faulty": shape.is_faulty(actor),
                "class": class,
                "is_rsu": shape.is_rsu(actor),
            }),
        ));
    }

    // ---- pseudonyms: each device learns its certificates, rotating through them -----
    for actor in 0..shape.total_actors() {
        for k in 0..shape.pseudonyms.max(1) {
            // Spread the rotations across the run so a certificate has a real window.
            let at = u64::from(k) * u64::from(shape.steps) * STEP_NS
                / u64::from(shape.pseudonyms.max(1));
            out.push(rec(
                "sec.cert",
                at,
                &serde_json::json!({
                    "t": at,
                    "node": actor,
                    "event": "change",
                    "digest": digest_of(actor, k),
                }),
            ));
        }
    }

    // ---- the run --------------------------------------------------------------------
    let attack_from = u64::from(shape.steps) * STEP_NS / 4;
    let attack_to = u64::from(shape.steps) * STEP_NS * 3 / 4;
    let mut msg = 0u64;
    for step in 0..shape.steps {
        let t = u64::from(step) * STEP_NS;
        let t_s = q3(t as f64 / 1e9);
        let period = (u64::from(step) * u64::from(shape.pseudonyms.max(1))
            / u64::from(shape.steps.max(1))) as u32;

        for actor in 0..shape.total_actors() {
            if shape.is_rsu(actor) {
                continue;
            }
            let x = q3(f64::from(actor) * 40.0 + f64::from(step) * 12.5);
            let y = q3(f64::from(actor) * 3.5);
            let speed = q3(12.0 + mix(u64::from(actor), u64::from(step)) * 3.0);
            out.push(rec(
                "gt.kinematics",
                t,
                &serde_json::json!({
                    "t": t, "actor": actor, "x_m": x, "y_m": y, "z_m": 0.0,
                    "speed_mps": speed, "acc_mps2": 0.0, "heading_rad": 0.0,
                    "lane": 1, "lane_pos_m": x, "class": "car",
                }),
            ));

            let attacking = shape.is_attacker(actor) && t >= attack_from && t <= attack_to;
            if attacking {
                out.push(rec(
                    "gt.attack.action",
                    t,
                    &serde_json::json!({
                        "t": t, "actor": actor, "attacker": "ConstPosOffset",
                        "action": "falsify_position", "fields": ["pos"],
                        "changed_bytes_on_air": true, "msg": msg,
                    }),
                ));
            }

            let digest = digest_of(actor, period.min(shape.pseudonyms.saturating_sub(1)));
            out.push(rec(
                "node.tx",
                t,
                &serde_json::json!({
                    "t": t, "node": actor, "msg": msg, "msg_type": "bsm",
                    "bytes_on_wire": 421, "payload_bytes": 39, "envelope_bytes": 111,
                    "airtime_us": 588, "mcs": 2, "power_dbm": 20.0, "channel": 180,
                    "ac": 2, "dcc_state": "relaxed", "signer": "digest", "t_generated": t,
                }),
            ));
            let _ = &digest;

            // One neighbour receives it, which gives the receiver-logs and net-trace
            // profiles their rows.
            let rx = (actor + 1) % shape.devices.max(1);
            if rx != actor {
                let dist = q3(40.0 + mix(u64::from(actor), u64::from(step) + 7) * 200.0);
                let rssi = v2xw_core::math::quantize_to(
                    -60.0 - mix(u64::from(actor), u64::from(step) + 13) * 30.0,
                    crate::grid::Q_DB,
                );
                let lost = mix(u64::from(actor), u64::from(step) + 23) < 0.1;
                out.push(rec(
                    "phy.rx",
                    t,
                    &serde_json::json!({
                        "t_start": t, "t_end": t + 588_000, "tx": actor, "rx": rx,
                        "msg": msg, "rssi_dbm": rssi, "sinr_db": v2xw_core::math::quantize_to(rssi + 95.0, crate::grid::Q_DB),
                        "outcome": if lost { "lost" } else { "ok" },
                        "cause": if lost { Some("per") } else { None },
                        "dist_m": dist, "candidate": true, "payload_bytes": 39,
                    }),
                ));
                if !lost {
                    out.push(rec(
                        "node.verify",
                        t,
                        &serde_json::json!({
                            "t_enqueue": t, "t_start": t + 100_000, "t_done": t + 1_100_000,
                            "node": rx, "primitive": "ecdsa-p256", "cost_us": 1000,
                            "outcome": "valid", "policy": "verify-on-demand",
                            "msg": msg, "queue_depth": 1,
                        }),
                    ));
                }
            }
            msg += 1;
        }

        // Per-node MAC and telemetry samples, once a second.
        if step % 10 == 0 {
            for actor in 0..shape.devices {
                out.push(rec(
                    "mac.cbr",
                    t,
                    &serde_json::json!({
                        "t": t, "node": actor, "channel": 180,
                        "cbr": v2xw_core::math::quantize_to(
                            mix(u64::from(actor), u64::from(step) + 31) * 0.4,
                            crate::grid::Q_RATIO),
                        "busy_us": 12_000, "window_us": 100_000,
                    }),
                ));
                out.push(rec(
                    "node.telemetry",
                    t,
                    &serde_json::json!({
                        "t": t, "node": actor,
                        "cpu": v2xw_core::math::quantize_to(0.1 + mix(u64::from(actor), u64::from(step)) * 0.3, crate::grid::Q_RATIO),
                        "hsm": 0.05, "ram_bytes": 4_194_304, "storage_bytes": 1_048_576,
                        "verify_queue_depth": 2,
                    }),
                ));
            }
        }

        // ---- detection: neighbours observe an attacking device and report it ---------
        if t >= attack_from && t <= attack_to && step % 5 == 0 {
            for attacker in 0..shape.attackers {
                let subject = digest_of(attacker, period.min(shape.pseudonyms.saturating_sub(1)));
                // The reporters are the RSU (always trusted infrastructure) and one
                // neighbouring vehicle, which is the mix the audit's R3 allowance covers.
                let reporters: Vec<u32> = if shape.rsus > 0 {
                    vec![shape.devices, (attacker + 2) % shape.devices.max(1)]
                } else {
                    vec![(attacker + 2) % shape.devices.max(1)]
                };
                for reporter in reporters {
                    let score = v2xw_core::math::quantize_to(
                        1.2 + mix(u64::from(reporter), u64::from(step)) * 2.0,
                        1e-3,
                    );
                    out.push(rec(
                        "det.observation",
                        t,
                        &serde_json::json!({
                            "t": t, "node": reporter, "detector": "positionJump",
                            "subject": subject, "score": score,
                        }),
                    ));
                    out.push(rec(
                        "ma.report",
                        t,
                        &serde_json::json!({
                            "t": t, "reporter": reporter, "subject": subject,
                            "detector": "positionJump",
                        }),
                    ));
                }
            }
        }

        // ---- collusion: an attacker frames a benign device --------------------------
        if shape.collusion && shape.attackers > 0 && step == shape.steps / 2 {
            let victim = shape.devices - 1;
            let subject = digest_of(victim, period.min(shape.pseudonyms.saturating_sub(1)));
            out.push(rec(
                "det.observation",
                t,
                &serde_json::json!({
                    "t": t, "node": 0, "detector": "positionJump",
                    "subject": subject, "score": 1.5,
                }),
            ));
            out.push(rec(
                "ma.report",
                t,
                &serde_json::json!({
                    "t": t, "reporter": 0, "subject": subject, "detector": "positionJump",
                }),
            ));
        }
        let _ = t_s;
    }

    // ---- the authority's cases, decisions and revocation stages --------------------
    let decide_at = attack_to + STEP_NS;
    for attacker in 0..shape.attackers {
        let subject = digest_of(attacker, shape.pseudonyms.saturating_sub(1));
        out.push(rec(
            "ma.case",
            attack_to,
            &serde_json::json!({
                "t": attack_to, "subject": subject, "trigger": "report_threshold",
                "cluster_size": 4, "num_distinct_reporters": 2, "linkage_result": "same",
            }),
        ));
        out.push(rec(
            "ma.decision",
            decide_at,
            &serde_json::json!({
                "t": decide_at, "subject": subject,
                "decision": if shape.revoke { "revoke" } else { "dismiss" },
            }),
        ));
        if shape.revoke {
            for (i, stage) in ["decision", "issued", "published", "downloaded", "enforced"]
                .iter()
                .enumerate()
            {
                let at = decide_at + (i as u64) * STEP_NS;
                out.push(rec(
                    "proto.revocation",
                    at,
                    &serde_json::json!({
                        "t": at, "stage": stage, "id": subject,
                        "entries": u64::from(attacker) + 1,
                        "size_bytes": 64 * (u64::from(attacker) + 1),
                        "node": if *stage == "downloaded" || *stage == "enforced" { Some(1) } else { None },
                    }),
                ));
            }
        }
    }

    out
}

/// The whole dataset, assembled in `profile`.
///
/// # Errors
/// Whatever [`super::DatasetAssembler::finish`] returns.
pub fn dataset(
    shape: &DatasetShape,
    profile: super::DatasetProfile,
) -> crate::Result<super::MaDataset> {
    let mut a = super::DatasetAssembler::new(profile).with_sample_stride(4);
    a.ingest_all(&records(shape))?;
    a.finish()
}

/// A [`RunProvenance`](super::RunProvenance) with plausible, clearly-synthetic values —
/// never a fabricated real one.
///
/// The build time is a fixed string rather than a clock read, and it says `fixture` so it
/// cannot be mistaken for the provenance of a real run.
#[must_use]
pub fn provenance() -> super::RunProvenance {
    let mut config = std::collections::BTreeMap::new();
    config.insert(
        "scenario".to_string(),
        serde_json::Value::from("dataset-fixture"),
    );
    config.insert("duration_s".to_string(), serde_json::Value::from(4.0));
    config.insert("attacker_pct".to_string(), serde_json::Value::from(0.33));
    config.insert("n_rsus".to_string(), serde_json::Value::from(1));
    // `collude_pct` and `attacker_pct` are read by the frozen audit, not by this crate:
    // `C6_collusion_consistency` skips itself entirely when `collude_pct` is absent or
    // zero, and `V3_attacker_pct_plausible` skips when `attacker_pct` is. A config that
    // omitted them would make the audit report 34 passes and two silent skips over
    // exactly the labels the fixture exists to produce — a check that cannot fail.
    config.insert("collude_pct".to_string(), serde_json::Value::from(0.5));
    super::RunProvenance {
        master_seed: 0x00c0_ffee_5eed,
        scenario_hash: "4ef5aca22590dd69e10c91aede255eeb25e1d356b7a1f1d3da94510255969cf7"
            .to_string(),
        world_hash: "a939a698a4a19178d2d8a6639937d49baea15387d700e61fe4c8d5e586913d79".to_string(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: "fixture".to_string(),
        platform: std::env::consts::ARCH.to_string(),
        build_utc: "fixture (no clock read)".to_string(),
        content_digest: "0921e0b9b03d0bf9c224a9c492f56000a6fcd53814616b05d7328f6addd47393"
            .to_string(),
        model_cards: vec![
            (
                "radio/propagation/log-distance".to_string(),
                "1.0.0".to_string(),
            ),
            (
                "security/envelope/1609dot2".to_string(),
                "1.0.0".to_string(),
            ),
            ("threat/detector/legacy-12".to_string(), "1.0.0".to_string()),
        ],
        models: vec![
            super::ModelProvenance {
                id: "radio/propagation/log-distance".to_string(),
                version: "1.0.0".to_string(),
                validation_status: v2xw_core::card::ValidationStatus::LiteratureChecked,
                content_hash: "11".repeat(32),
                todo_calibrate: 2,
                tiers: vec!["medium".to_string(), "high".to_string()],
            },
            super::ModelProvenance {
                id: "security/envelope/1609dot2".to_string(),
                version: "1.0.0".to_string(),
                validation_status: v2xw_core::card::ValidationStatus::UnitTested,
                content_hash: "22".repeat(32),
                todo_calibrate: 0,
                tiers: vec!["medium".to_string()],
            },
            super::ModelProvenance {
                id: "threat/detector/legacy-12".to_string(),
                version: "1.0.0".to_string(),
                validation_status: v2xw_core::card::ValidationStatus::Unvalidated,
                content_hash: "33".repeat(32),
                todo_calibrate: 5,
                tiers: vec!["abstract".to_string(), "medium".to_string()],
            },
        ],
        // The fixture's `node.tx` records all carry `msg_type: "bsm"`, and the J2735 BSM
        // codec is the hand-written real one (04-models.md §8.4 lists PSM, SRM and SSM as
        // the modelled ones), so the declaration says `uper`. `psm` is declared and unused,
        // which exercises the report's stale-declaration row — a fixture that agreed with
        // the implementation on every field would not test the join at all.
        message_encodings: std::collections::BTreeMap::from([
            (
                "bsm".to_string(),
                super::ByteProvenance::Real("uper".to_string()),
            ),
            (
                "psm".to_string(),
                super::ByteProvenance::SizeModel("1.0.0".to_string()),
            ),
        ]),
        config,
        world_licence: None,
        world_attribution: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::DatasetProfile;

    #[test]
    fn the_fixture_exercises_every_channel_the_exporters_read() {
        let recs = records(&DatasetShape::default());
        let mut channels: Vec<&str> = recs.iter().map(|r| r.channel.as_str()).collect();
        channels.sort_unstable();
        channels.dedup();
        for want in [
            "det.observation",
            "gt.attack.action",
            "gt.kinematics",
            "gt.spawn",
            "ma.case",
            "ma.decision",
            "ma.report",
            "mac.cbr",
            "node.telemetry",
            "node.tx",
            "node.verify",
            "phy.rx",
            "proto.revocation",
            "sec.cert",
        ] {
            assert!(channels.contains(&want), "the fixture never emits {want}");
        }
    }

    #[test]
    fn the_fixture_is_reproducible_without_an_rng() {
        let a = records(&DatasetShape::default());
        let b = records(&DatasetShape::default());
        assert_eq!(a, b);
    }

    #[test]
    fn a_device_is_never_both_an_attacker_and_faulty() {
        // The labels have to be disjoint or `C4_correctness_semantics` becomes
        // unfalsifiable: `correct` and `faulty_detection` would both be defensible.
        let shape = DatasetShape::default();
        for actor in 0..shape.devices {
            assert!(
                !(shape.is_attacker(actor) && shape.is_faulty(actor)),
                "{actor}"
            );
        }
    }

    #[test]
    fn the_fixture_assembles_into_a_dataset_with_reports_and_a_revocation() {
        let ds = dataset(&DatasetShape::default(), DatasetProfile::V1).expect("assemble");
        assert!(!ds.ma_reports.is_empty(), "no reports");
        assert_eq!(ds.ma_reports.len(), ds.gt_report_labels.len());
        assert!(!ds.gt_vehicle.is_empty());
        assert_eq!(ds.rsu_count, 1);
        assert!(!ds.revoked_devices().is_empty(), "nothing was revoked");
        // Every correctness class the fixture is meant to produce really appears, or the
        // conformance tests below would be checking an empty set.
        let classes: std::collections::BTreeSet<&str> = ds
            .gt_report_labels
            .iter()
            .map(|l| l.report_correctness.as_str())
            .collect();
        assert!(classes.contains("correct"), "{classes:?}");
        assert!(classes.contains("malicious_false_report"), "{classes:?}");
    }

    #[test]
    fn the_all_benign_shape_produces_no_attacker_and_no_revocation() {
        let ds = dataset(&DatasetShape::all_benign(), DatasetProfile::V2).expect("assemble");
        assert!(ds.gt_vehicle.iter().all(|v| !v.is_attacker));
        assert!(ds.revoked_devices().is_empty());
        assert!(ds.gt_attacks.is_empty());
        assert!(
            ds.gt_emissions_sample.iter().all(|e| !e.falsified),
            "no message is falsified in an all-benign run"
        );
    }
}
