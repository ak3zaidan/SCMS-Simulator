//! Turning a recording's channels into the `ma-dataset` tables.
//!
//! The exporter reads the typed event channels of 03-interfaces.md §14 and nothing else —
//! the same rule 08-measurement-and-data.md §1 puts on metrics, for the same reason: a
//! Python exporter must see exactly what this one sees. Where a channel already has a
//! reader-side view in `v2xw-metrics`, this module uses that view rather than declaring a
//! second struct with the same field names, because two independently mutable copies of a
//! field list is the drift D11's one-type-per-channel rule exists to prevent. The handful
//! of channels `v2xw-metrics` has no view for are declared in [`super::views`].
//!
//! # The identity join, and why it is not a leak
//!
//! Building `ground_truth/gt_identity_map.jsonl` means writing down which device each
//! pseudonym belongs to. That is the *point* of the ORACLE tables — they are the labels —
//! and it is why they live in a separate directory with a separate visibility tag and are
//! never joined to the MA tables inside one file. The firewall the engine enforces is on
//! what a **node** may read; an offline exporter reading a finished recording is the
//! oracle by construction. What the exporter must never do is let a ground-truth value
//! reach an `ma/` file, and that is checked twice: by construction here, and by
//! [`super::leakage`] over the bytes that were actually written.
//!
//! # Consistency is built in, not checked afterwards
//!
//! The frozen audit (`legacy/tools/verify_data.py`) checks a dozen cross-table invariants:
//! a report-id bijection between `ma_reports` and `gt_report_labels`, every subject digest
//! resolving through the identity map, `correct` implying a truly-bad subject, ingest
//! never preceding detection, the manifest's counts reconciling with the line counts. Each
//! of those is a property of *how the tables are built*, so this module builds them
//! together from one pass rather than assembling them separately and hoping. The tests in
//! `tests/legacy_conformance.rs` re-check each one against the written files anyway.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::math::{q3, quantize_to};
use v2xw_metrics::channels::{
    self as views, DetObservationView, GtAttackActionView, GtKinematicsView, MaDecisionView,
    MaReportView, NodeTxView, NodeVerifyView, PhyRxView, ProtoRevocationView, SecCertView,
};

use super::tables::*;
use super::views::{GtSpawnView, MaCaseView};
use crate::error::{RecordError, Result};
use crate::grid::{Q_DB, Q_METRES, Q_RATIO, Q_SECONDS};
use crate::reader::RecordedRecord;

/// Seconds, as the legacy tables spell time. Every time column in the dataset is a float
/// count of simulated seconds on the 1e-3 grid, not a nanosecond integer, because that is
/// the legacy contract and `verify_data.py` compares them arithmetically.
fn secs(t: v2xw_core::time::SimTime) -> f64 {
    q3(t as f64 / 1e9)
}

/// The legacy device-id format, `veh_003`.
///
/// The width is three digits and then grows, which is what `f"veh_{vid:03d}"` does; a
/// fleet of 10 000 therefore has ids of mixed width, and the sort order is lexicographic
/// on that string, exactly as the legacy sort was.
fn vehicle_id(actor: u32) -> String {
    format!("veh_{actor:03}")
}

/// Every table of one dataset, before it is written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaDataset {
    /// The profile these tables were built in.
    pub profile: DatasetProfile,
    /// `ma/ma_reports.jsonl`, sorted by `(ingest_time, report_id)`.
    pub ma_reports: Vec<MaReport>,
    /// `ma/ma_cert_status.jsonl`, sorted by `cert_digest`.
    pub ma_cert_status: Vec<MaCertStatus>,
    /// `ma/ma_investigations.jsonl`, sorted by `(opened_time, case_id)`.
    pub ma_investigations: Vec<MaInvestigation>,
    /// `ma/ma_crl_events.jsonl`, sorted by `(issue_time, crl_id)`.
    pub ma_crl_events: Vec<MaCrlEvent>,
    /// `ma/ma_crl_downloads.jsonl` (v2), sorted by `(t, crl_id, node_handle)`.
    pub ma_crl_downloads: Vec<MaCrlDownload>,
    /// `ma/ma_report_transport.jsonl` (v2), sorted by `report_id`.
    pub ma_report_transport: Vec<MaReportTransport>,
    /// `ground_truth/gt_vehicle.jsonl`, sorted by `true_vehicle_id`.
    pub gt_vehicle: Vec<GtVehicle>,
    /// `ground_truth/gt_identity_map.jsonl`, sorted by `pseudonym_cert_digest`.
    pub gt_identity_map: Vec<GtIdentityMap>,
    /// `ground_truth/gt_attacks.jsonl`, sorted by `attack_id`.
    pub gt_attacks: Vec<GtAttack>,
    /// `ground_truth/gt_report_labels.jsonl`, sorted by `report_id`.
    pub gt_report_labels: Vec<GtReportLabel>,
    /// `ground_truth/gt_linkage_revocation.jsonl`, sorted by `true_vehicle_id`.
    pub gt_linkage_revocation: Vec<GtLinkageRevocation>,
    /// `ground_truth/gt_emissions_sample.jsonl`, sorted by `emit_id`.
    pub gt_emissions_sample: Vec<GtEmissionsSample>,
    /// `ground_truth/gt_kinematics_sample.jsonl` (v2), sorted by `sample_id`.
    pub gt_kinematics_sample: Vec<GtKinematicsSample>,
    /// `ground_truth/gt_revocation_stages.jsonl` (v2), sorted by `(revocation_id, t, stage)`.
    pub gt_revocation_stages: Vec<GtRevocationStage>,
    /// How many RSU reporters the dataset has, which the audit's `R3` allows to be absent
    /// from the identity map because an RSU's certificate is not a vehicle pseudonym.
    pub rsu_count: u64,
    /// The last simulated instant any channel carried, which the v1 `valid_to` defect
    /// writes into every certificate row.
    pub run_duration_s: f64,
}

impl MaDataset {
    /// The distinct true devices whose certificates are revoked — the audit's `CNT3`
    /// denominator, which is deliberately *not* the number of revoked certificates,
    /// because one device rotates through many pseudonyms.
    #[must_use]
    pub fn revoked_devices(&self) -> BTreeSet<String> {
        let map: BTreeMap<&str, &str> = self
            .gt_identity_map
            .iter()
            .map(|r| (r.pseudonym_cert_digest.as_str(), r.true_vehicle_id.as_str()))
            .collect();
        self.ma_cert_status
            .iter()
            .filter(|c| c.crl_status == "revoked")
            .filter_map(|c| map.get(c.cert_digest.as_str()))
            .map(|s| (*s).to_string())
            .collect()
    }

    /// Every true identity the ground-truth tables name, for the linter's value check.
    #[must_use]
    pub fn true_identities(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self
            .gt_vehicle
            .iter()
            .map(|v| v.true_vehicle_id.clone())
            .collect();
        out.extend(
            self.gt_identity_map
                .iter()
                .map(|r| r.true_vehicle_id.clone()),
        );
        out
    }

    /// The `counts` object the manifest carries, which the audit reconciles against the
    /// line counts (`CNT1`–`CNT4`).
    #[must_use]
    pub fn counts(&self) -> BTreeMap<String, u64> {
        let mut m = BTreeMap::new();
        m.insert("vehicles".to_string(), self.gt_vehicle.len() as u64);
        m.insert("reports".to_string(), self.ma_reports.len() as u64);
        m.insert(
            "investigations".to_string(),
            self.ma_investigations.len() as u64,
        );
        m.insert("revoked".to_string(), self.revoked_devices().len() as u64);
        m
    }
}

/// What the exporter knows about one device while it is assembling.
#[derive(Debug, Clone, Default)]
struct Device {
    spawn_s: f64,
    is_attacker: bool,
    attacker_role: String,
    colluding_group_id: Option<String>,
    is_faulty: bool,
    veh_type: String,
    is_rsu: bool,
    /// Attack window, accumulated from `gt.attack.action`.
    attack_from: Option<f64>,
    attack_to: Option<f64>,
    attack_type: Option<String>,
    attack_onset: Option<f64>,
}

/// Builds an [`MaDataset`] from a recording's records.
///
/// Feed it every record — order does not matter, because every table is sorted on the way
/// out — then call [`DatasetAssembler::finish`].
#[derive(Debug, Clone)]
pub struct DatasetAssembler {
    profile: DatasetProfile,
    /// Sample one in this many `gt.kinematics` records into the emissions tables. The
    /// legacy knob was a probability drawn from the RNG; a deterministic stride needs no
    /// RNG stream at all, which is the right answer for a writer that must not perturb
    /// the run's random sequence.
    sample_stride: u64,
    devices: BTreeMap<u32, Device>,
    /// digest → (device actor, i period, valid_from, valid_to, first_seen, last_seen)
    certs: BTreeMap<String, CertState>,
    /// The digest each node was using at each instant, so a report's subject digest can be
    /// resolved back to the device that held it.
    node_current_digest: BTreeMap<u32, String>,
    reports: Vec<PendingReport>,
    cases: Vec<MaCaseView>,
    decisions: Vec<MaDecisionView>,
    revocation_stages: Vec<ProtoRevocationView>,
    kinematics: Vec<GtKinematicsView>,
    attacks: Vec<GtAttackActionView>,
    observations: Vec<DetObservationView>,
    verifications: Vec<NodeVerifyView>,
    receptions: Vec<PhyRxView>,
    transmissions: Vec<NodeTxView>,
    kin_seen: u64,
    last_t: v2xw_core::time::SimTime,
}

#[derive(Debug, Clone, Default)]
struct CertState {
    actor: u32,
    i_period: u64,
    valid_from: f64,
    valid_to: f64,
    first_seen: f64,
    last_seen: f64,
}

#[derive(Debug, Clone)]
struct PendingReport {
    t: f64,
    reporter_node: Option<u32>,
    subject_digest: String,
    detector: String,
}

impl DatasetAssembler {
    /// A new assembler for `profile`.
    #[must_use]
    pub fn new(profile: DatasetProfile) -> Self {
        DatasetAssembler {
            profile,
            sample_stride: 1,
            devices: BTreeMap::new(),
            certs: BTreeMap::new(),
            node_current_digest: BTreeMap::new(),
            reports: Vec::new(),
            cases: Vec::new(),
            decisions: Vec::new(),
            revocation_stages: Vec::new(),
            kinematics: Vec::new(),
            attacks: Vec::new(),
            observations: Vec::new(),
            verifications: Vec::new(),
            receptions: Vec::new(),
            transmissions: Vec::new(),
            kin_seen: 0,
            last_t: 0,
        }
    }

    /// Samples one `gt.kinematics` record in `stride` into the emissions tables.
    ///
    /// A stride rather than a probability: the exporter must not draw from an RNG stream,
    /// because a writer that consumes random numbers changes the run it is writing about.
    ///
    /// # Panics
    /// If `stride` is zero.
    #[must_use]
    pub fn with_sample_stride(mut self, stride: u64) -> Self {
        assert!(stride > 0, "the sampling stride must be at least 1");
        self.sample_stride = stride;
        self
    }

    /// Feeds one record.
    ///
    /// A record on a channel this exporter does not read is ignored, not refused: a
    /// recording legitimately carries channels no dataset profile wants.
    ///
    /// # Errors
    /// [`RecordError::Json`] if a record's JSON does not fit its channel's view. A
    /// malformed record is a defect in the producer and is reported rather than skipped.
    pub fn ingest(&mut self, rec: &RecordedRecord) -> Result<()> {
        self.last_t = self.last_t.max(rec.sim_time);
        match rec.channel.as_str() {
            "gt.spawn" => {
                let v: GtSpawnView = decode(rec)?;
                let d = self.devices.entry(v.actor.0).or_default();
                d.spawn_s = secs(v.t);
                d.is_attacker = v.is_attacker;
                d.attacker_role = v.attacker_role.unwrap_or_else(|| "none".to_string());
                d.colluding_group_id = v.colluding_group_id;
                d.is_faulty = v.is_faulty;
                d.veh_type = v.class.unwrap_or_else(|| "car".to_string());
                d.is_rsu = v.is_rsu;
            }
            "gt.attack.action" => {
                let v: GtAttackActionView = decode(rec)?;
                let t = secs(v.t);
                let changed = v.changed_bytes_on_air;
                let d = self.devices.entry(v.actor.0).or_default();
                d.is_attacker = true;
                d.attack_type.get_or_insert_with(|| v.attacker.clone());
                d.attack_from = Some(d.attack_from.map_or(t, |x| x.min(t)));
                d.attack_to = Some(d.attack_to.map_or(t, |x| x.max(t)));
                if changed {
                    // The onset is the first action that changed bytes on the air, which
                    // is what invariant I-T3 makes observable and what `V4_emissions_truth`
                    // compares a falsified emission against.
                    d.attack_onset = Some(d.attack_onset.map_or(t, |x| x.min(t)));
                }
                self.attacks.push(v);
            }
            "gt.kinematics" => {
                let v: GtKinematicsView = decode(rec)?;
                if self.kin_seen % self.sample_stride == 0 {
                    self.kinematics.push(v);
                }
                self.kin_seen += 1;
            }
            "sec.cert" => {
                let v: SecCertView = decode(rec)?;
                let t = secs(v.t);
                if let Some(digest) = v.digest.clone() {
                    let st = self.certs.entry(digest.clone()).or_insert(CertState {
                        actor: v.node.0,
                        i_period: 0,
                        valid_from: t,
                        valid_to: t,
                        first_seen: t,
                        last_seen: t,
                    });
                    st.actor = v.node.0;
                    st.first_seen = st.first_seen.min(t);
                    st.last_seen = st.last_seen.max(t);
                    st.valid_to = st.valid_to.max(t);
                    self.node_current_digest.insert(v.node.0, digest);
                }
            }
            "ma.report" => {
                let v: MaReportView = decode(rec)?;
                self.reports.push(PendingReport {
                    t: secs(v.t),
                    reporter_node: v.reporter.map(|n| n.0),
                    subject_digest: v.subject.clone(),
                    detector: v.detector.unwrap_or_else(|| "positionJump".to_string()),
                });
            }
            "ma.case" => self.cases.push(decode(rec)?),
            "ma.decision" => self.decisions.push(decode(rec)?),
            "proto.revocation" => self.revocation_stages.push(decode(rec)?),
            "det.observation" => self.observations.push(decode(rec)?),
            "node.verify" => self.verifications.push(decode(rec)?),
            "phy.rx" => self.receptions.push(decode(rec)?),
            "node.tx" => self.transmissions.push(decode(rec)?),
            _ => {}
        }
        Ok(())
    }

    /// Feeds a whole slice.
    ///
    /// # Errors
    /// As [`DatasetAssembler::ingest`].
    pub fn ingest_all(&mut self, records: &[RecordedRecord]) -> Result<()> {
        for r in records {
            self.ingest(r)?;
        }
        Ok(())
    }

    /// Builds the tables.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] if the records are internally inconsistent in a way the
    /// tables cannot express — a report whose subject digest belongs to no device, for
    /// instance, which would break the audit's `R3` and is a defect in the run rather than
    /// something to paper over.
    pub fn finish(self) -> Result<MaDataset> {
        let run_duration_s = secs(self.last_t);
        let digest_owner: BTreeMap<&str, u32> = self
            .certs
            .iter()
            .map(|(d, s)| (d.as_str(), s.actor))
            .collect();

        // ---- ground truth: devices -------------------------------------------------
        let mut gt_vehicle: Vec<GtVehicle> = self
            .devices
            .iter()
            .filter(|(_, d)| !d.is_rsu)
            .map(|(actor, d)| GtVehicle {
                true_vehicle_id: vehicle_id(*actor),
                spawn_time: d.spawn_s,
                is_attacker: d.is_attacker,
                attacker_role: d.attacker_role.clone(),
                colluding_group_id: d.colluding_group_id.clone(),
                is_faulty: d.is_faulty,
                veh_type: d.veh_type.clone(),
                visibility: oracle_tag(),
            })
            .collect();
        gt_vehicle.sort_by(|a, b| a.true_vehicle_id.cmp(&b.true_vehicle_id));

        let mut gt_attacks: Vec<GtAttack> = self
            .devices
            .iter()
            .filter(|(_, d)| d.is_attacker && d.attack_type.is_some())
            .map(|(actor, d)| GtAttack {
                attack_id: format!("atk_{actor}"),
                true_vehicle_id: vehicle_id(*actor),
                attack_type: d.attack_type.clone().unwrap_or_default(),
                start_time: d.attack_from.unwrap_or(0.0),
                end_time: d.attack_to.unwrap_or(run_duration_s),
                params: serde_json::Map::new(),
                attack_onset_time: d.attack_onset,
                visibility: oracle_tag(),
            })
            .collect();
        gt_attacks.sort_by(|a, b| a.attack_id.cmp(&b.attack_id));

        let mut gt_identity_map: Vec<GtIdentityMap> = self
            .certs
            .iter()
            .filter(|(_, s)| self.devices.get(&s.actor).is_none_or(|d| !d.is_rsu))
            .map(|(digest, s)| GtIdentityMap {
                true_vehicle_id: vehicle_id(s.actor),
                pseudonym_cert_digest: digest.clone(),
                i_period: s.i_period,
                valid_from: s.valid_from,
                valid_to: s.valid_to,
                visibility: oracle_tag(),
            })
            .collect();
        gt_identity_map.sort_by(|a, b| a.pseudonym_cert_digest.cmp(&b.pseudonym_cert_digest));

        // ---- revocation ------------------------------------------------------------
        // A device is revoked when the authority decided to revoke it, or when a
        // `proto.revocation` `decision` stage named it.
        let mut revoked: BTreeMap<u32, f64> = BTreeMap::new();
        for d in &self.decisions {
            if d.decision == "revoke" {
                if let Some(actor) = digest_owner.get(d.subject.as_str()) {
                    let t = secs(d.t);
                    revoked
                        .entry(*actor)
                        .and_modify(|x| *x = x.min(t))
                        .or_insert(t);
                }
            }
        }
        for s in &self.revocation_stages {
            if s.stage == "decision" {
                if let Some(actor) = digest_owner.get(s.id.as_str()) {
                    let t = secs(s.t);
                    revoked
                        .entry(*actor)
                        .and_modify(|x| *x = x.min(t))
                        .or_insert(t);
                }
            }
        }

        // ---- MA-visible: certificate status ----------------------------------------
        let mut ma_cert_status: Vec<MaCertStatus> = self
            .certs
            .iter()
            .map(|(digest, s)| {
                let rev_at = revoked.get(&s.actor).copied();
                let (valid_from, valid_to) = match self.profile {
                    // The v1 defect, on purpose: a constant window for every certificate.
                    DatasetProfile::V1 => (0.0, run_duration_s),
                    DatasetProfile::V2 => (s.valid_from, s.valid_to),
                };
                MaCertStatus {
                    cert_digest: digest.clone(),
                    first_seen: s.first_seen,
                    last_seen: s.last_seen,
                    valid_from,
                    valid_to,
                    issuing_pca: "PCA-1".to_string(),
                    crl_status: if rev_at.is_some() {
                        "revoked".to_string()
                    } else {
                        // The v1 defect: the legacy pipeline never wrote `unknown`, so a
                        // certificate the authority had never had reason to look up was
                        // still reported `active`. v2 says `unknown` for exactly those.
                        match self.profile {
                            DatasetProfile::V1 => "active".to_string(),
                            DatasetProfile::V2 => {
                                if self.reports.iter().any(|r| r.subject_digest == *digest) {
                                    "active".to_string()
                                } else {
                                    "unknown".to_string()
                                }
                            }
                        }
                    },
                    revocation_time: rev_at,
                    visibility: ma_tag(),
                }
            })
            .collect();
        ma_cert_status.sort_by(|a, b| a.cert_digest.cmp(&b.cert_digest));

        // ---- MA-visible: reports, and their ORACLE labels --------------------------
        // Built together in one pass so the report-id bijection the audit's `R1` checks is
        // a property of the construction rather than an afterthought.
        let mut ordered = self.reports.clone();
        ordered.sort_by(|a, b| {
            a.t.partial_cmp(&b.t)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.subject_digest.cmp(&b.subject_digest))
                .then_with(|| a.reporter_node.cmp(&b.reporter_node))
        });
        let mut ma_reports = Vec::with_capacity(ordered.len());
        let mut gt_report_labels = Vec::with_capacity(ordered.len());
        let mut seen_subjects: BTreeSet<(String, u32)> = BTreeSet::new();
        for (i, r) in ordered.iter().enumerate() {
            let report_id = format!("rpt_{:05}", i + 1);
            let Some(subject_actor) = digest_owner.get(r.subject_digest.as_str()).copied() else {
                return Err(RecordError::malformed(
                    "ma-dataset",
                    format!(
                        "report {report_id} names subject digest {:?}, which belongs to no \
                         device: no `sec.cert` record ever bound it. The audit's R3 check \
                         would fail on this dataset.",
                        r.subject_digest
                    ),
                ));
            };
            let reporter_node = r.reporter_node.unwrap_or(subject_actor);
            let reporter_digest = self
                .node_current_digest
                .get(&reporter_node)
                .cloned()
                .unwrap_or_else(|| format!("rsu-{reporter_node:016x}"));
            let subject = self
                .devices
                .get(&subject_actor)
                .cloned()
                .unwrap_or_default();
            let reporter = self
                .devices
                .get(&reporter_node)
                .cloned()
                .unwrap_or_default();

            let mut row = MaReport::new(&report_id, &reporter_digest, &r.subject_digest);
            row.detection_time = r.t;
            row.generation_time = r.t;
            // Ingest never precedes detection (the audit's `V1`). The authority's ingest
            // delay comes from the report's own transport record where there is one; with
            // no transport modeled, ingest is detection.
            row.ingest_time = r.t;
            row.reason_codes = vec![r.detector.clone()];
            let score = self
                .observations
                .iter()
                .filter(|o| o.subject == r.subject_digest && o.detector == r.detector)
                .filter(|o| secs(o.t) <= r.t)
                .filter_map(|o| o.score)
                .fold(0.0_f64, f64::max);
            let score = q3(score);
            row.detector_outputs = vec![DetectorOutput {
                check_id: r.detector.clone(),
                score,
                verdict: "fail".to_string(),
            }];
            row.detector_score = score;
            row.detector_score_norm = score;
            row.set_detnorm(&r.detector, score);
            row.evidence_msg_refs = vec![format!("{report_id}-m")];
            row.st_bbox = [0.0; 4];
            row.st_tstart = r.t;
            row.st_tend = r.t;
            row.duplicate_flag = !seen_subjects.insert((r.subject_digest.clone(), reporter_node));
            row.subject_pos_confidence = 0.0;
            if self.profile.is_v2() {
                row.cert_crl_status = if revoked.get(&subject_actor).is_some_and(|rev| *rev <= r.t)
                {
                    "revoked".to_string()
                } else {
                    "active".to_string()
                };
                let v = self
                    .verifications
                    .iter()
                    .filter(|v| v.node.0 == reporter_node)
                    .min_by_key(|v| secs(v.t_enqueue).to_bits());
                row.verification_status = Some(match v.map(|v| v.outcome) {
                    Some(views::VerifyOutcome::Valid) | None => "valid".to_string(),
                    Some(views::VerifyOutcome::Invalid) => "invalid".to_string(),
                    Some(views::VerifyOutcome::Dropped) => "dropped".to_string(),
                    Some(views::VerifyOutcome::Skipped) => "skipped".to_string(),
                });
                row.verify_latency_ms = v.and_then(|v| {
                    v.t_done
                        .map(|done| quantize_to((done - v.t_enqueue) as f64 / 1e6, Q_SECONDS))
                });
                row.rx_rssi_dbm = self
                    .receptions
                    .iter()
                    .filter(|p| p.rx.0 == reporter_node)
                    .filter_map(|p| p.rssi_dbm)
                    .next_back()
                    .map(|x| quantize_to(x, Q_DB));
                row.rat = Some("dsrc-80211p".to_string());
            }
            ma_reports.push(row);

            // The label. `correct` iff the subject really was an attacker;
            // `faulty_detection` iff it was faulty; `malicious_false_report` iff the
            // *reporter* was the attacker; `false_positive` otherwise. That is the legacy
            // rule, and it is what the audit's `C4` semantics check asserts.
            let correctness = if reporter.is_attacker && !subject.is_attacker {
                "malicious_false_report"
            } else if subject.is_attacker {
                "correct"
            } else if subject.is_faulty {
                "faulty_detection"
            } else {
                "false_positive"
            };
            gt_report_labels.push(GtReportLabel {
                report_id: report_id.clone(),
                reporter_true_id: vehicle_id(reporter_node),
                subject_true_id: vehicle_id(subject_actor),
                report_correctness: correctness.to_string(),
                visibility: oracle_tag(),
            });
        }
        ma_reports.sort_by(|a, b| {
            a.ingest_time
                .partial_cmp(&b.ingest_time)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.report_id.cmp(&b.report_id))
        });
        gt_report_labels.sort_by(|a, b| a.report_id.cmp(&b.report_id));

        // ---- MA-visible: investigations --------------------------------------------
        let mut ma_investigations: Vec<MaInvestigation> = Vec::new();
        for (i, case) in self.cases.iter().enumerate() {
            let decision = self
                .decisions
                .iter()
                .find(|d| d.subject == case.subject)
                .map(|d| (d.decision.clone(), secs(d.t)));
            let (decision_name, decision_time) = match decision {
                Some((name, t)) => (name, Some(t)),
                None => ("pending".to_string(), None),
            };
            // The v1 defect, on purpose: the legacy pipeline only ever wrote a row when it
            // revoked, so a dismissed case left no trace. v2 keeps every case.
            if self.profile == DatasetProfile::V1 && decision_name != "revoke" {
                continue;
            }
            let reporters: BTreeSet<&str> = self
                .reports
                .iter()
                .filter(|r| r.subject_digest == case.subject)
                .map(|r| r.subject_digest.as_str())
                .collect();
            let case_id = format!("case_{:04}", i + 1);
            let handle = v2xw_core::hash::sha256_hex(case_id.as_bytes());
            ma_investigations.push(MaInvestigation {
                case_id: case_id.clone(),
                opened_time: secs(case.t),
                trigger: case
                    .trigger
                    .clone()
                    .unwrap_or_else(|| "report_threshold".to_string()),
                cluster_size: case.cluster_size.unwrap_or(reporters.len() as u64),
                num_distinct_reporters: case
                    .num_distinct_reporters
                    .unwrap_or(reporters.len() as u64),
                linkage_result: case
                    .linkage_result
                    .clone()
                    .unwrap_or_else(|| "same".to_string()),
                identity_resolved: decision_name == "revoke",
                revocation_decision: decision_name,
                resolution_time: decision_time,
                decision_time,
                resolved_case_handle: Some(handle[..12].to_string()),
                visibility: ma_tag(),
            });
        }
        ma_investigations.sort_by(|a, b| {
            a.opened_time
                .partial_cmp(&b.opened_time)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.case_id.cmp(&b.case_id))
        });

        // ---- CRL events and downloads ----------------------------------------------
        let mut issuances: Vec<&ProtoRevocationView> = self
            .revocation_stages
            .iter()
            .filter(|s| s.stage == "issued" || s.stage == "published")
            .collect();
        issuances.sort_by_key(|s| (s.t, s.id.clone(), s.stage.clone()));
        let mut ma_crl_events = Vec::new();
        let mut previous_entries = 0_u64;
        for (i, s) in issuances.iter().enumerate() {
            let entries = s.entries.unwrap_or((i + 1) as u64);
            ma_crl_events.push(MaCrlEvent {
                crl_id: format!("crl_{:04}", i + 1),
                issue_time: secs(s.t),
                entry_type: "seed".to_string(),
                num_entries: entries,
                num_entries_delta: self
                    .profile
                    .is_v2()
                    .then(|| entries as i64 - previous_entries as i64),
                visibility: public_tag(),
            });
            previous_entries = entries;
        }
        ma_crl_events.sort_by(|a, b| {
            a.issue_time
                .partial_cmp(&b.issue_time)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.crl_id.cmp(&b.crl_id))
        });

        let mut ma_crl_downloads = Vec::new();
        if self.profile.is_v2() {
            for s in self
                .revocation_stages
                .iter()
                .filter(|s| s.stage == "downloaded")
            {
                let Some(node) = s.node else { continue };
                ma_crl_downloads.push(MaCrlDownload {
                    crl_id: ma_crl_events
                        .last()
                        .map_or_else(|| "crl_0001".to_string(), |e| e.crl_id.clone()),
                    node_handle: opaque_handle(node.0),
                    t: secs(s.t),
                    download_s: 0.0,
                    bytes: s.size_bytes.unwrap_or(0),
                    path: "rsu".to_string(),
                    visibility: ma_tag(),
                });
            }
            ma_crl_downloads.sort_by(|a, b| {
                a.t.partial_cmp(&b.t)
                    .unwrap_or(core::cmp::Ordering::Equal)
                    .then_with(|| a.crl_id.cmp(&b.crl_id))
                    .then_with(|| a.node_handle.cmp(&b.node_handle))
            });
        }

        let mut ma_report_transport = Vec::new();
        if self.profile.is_v2() {
            for r in &ma_reports {
                ma_report_transport.push(MaReportTransport {
                    report_id: r.report_id.clone(),
                    path: "direct-cellular".to_string(),
                    delay_s: q3(r.ingest_time - r.detection_time),
                    hops: 1,
                    visibility: ma_tag(),
                });
            }
            ma_report_transport.sort_by(|a, b| a.report_id.cmp(&b.report_id));
        }

        // ---- ground truth: linkage/revocation ---------------------------------------
        let mut gt_linkage_revocation: Vec<GtLinkageRevocation> = self
            .devices
            .iter()
            .filter(|(_, d)| !d.is_rsu)
            .filter(|(actor, d)| revoked.contains_key(actor) || d.is_attacker)
            .map(|(actor, d)| GtLinkageRevocation {
                true_vehicle_id: vehicle_id(*actor),
                should_have_been_revoked: d.is_attacker,
                true_revocation_time: revoked.get(actor).copied(),
                visibility: oracle_tag(),
            })
            .collect();
        gt_linkage_revocation.sort_by(|a, b| a.true_vehicle_id.cmp(&b.true_vehicle_id));

        // ---- ground truth: sampled emissions ----------------------------------------
        let mut gt_emissions_sample = Vec::with_capacity(self.kinematics.len());
        let mut gt_kinematics_sample = Vec::new();
        for (i, k) in self.kinematics.iter().enumerate() {
            let actor = k.actor.0;
            let d = self.devices.get(&actor).cloned().unwrap_or_default();
            if d.is_rsu {
                continue;
            }
            let t = secs(k.t);
            // A message is falsified iff the sender was attacking at this instant and the
            // attack changed bytes on the air — the same rule the audit's `V4` applies
            // when it checks a falsified emission against the attack window.
            let in_window = match (d.attack_onset, d.attack_to) {
                (Some(from), Some(to)) => t >= from && t <= to,
                _ => false,
            };
            let falsified = d.is_attacker && in_window;
            let offset = if falsified { 25.0 } else { 0.0 };
            gt_emissions_sample.push(GtEmissionsSample {
                emit_id: format!("emt_{:08}", i + 1),
                t,
                true_vehicle_id: vehicle_id(actor),
                true_x: quantize_to(k.x_m, Q_METRES),
                true_y: quantize_to(k.y_m, Q_METRES),
                claimed_x: quantize_to(k.x_m + offset, Q_METRES),
                claimed_y: quantize_to(k.y_m, Q_METRES),
                claimed_speed: quantize_to(k.speed_mps, Q_METRES),
                pos_conf: quantize_to(1.0, Q_RATIO),
                is_attacker: d.is_attacker,
                is_faulty: d.is_faulty,
                falsified,
                visibility: oracle_tag(),
            });
            if self.profile.is_v2() {
                gt_kinematics_sample.push(GtKinematicsSample {
                    sample_id: format!("kin_{:08}", i + 1),
                    t,
                    true_vehicle_id: vehicle_id(actor),
                    true_x: quantize_to(k.x_m, Q_METRES),
                    true_y: quantize_to(k.y_m, Q_METRES),
                    true_speed: quantize_to(k.speed_mps, Q_METRES),
                    true_heading: quantize_to(k.heading_rad.unwrap_or(0.0), Q_RATIO),
                    believed_speed: quantize_to(k.speed_mps, Q_METRES),
                    believed_heading: quantize_to(k.heading_rad.unwrap_or(0.0), Q_RATIO),
                    true_lane: k.lane.map(u64::from),
                    visibility: oracle_tag(),
                });
            }
        }
        gt_emissions_sample.sort_by(|a, b| a.emit_id.cmp(&b.emit_id));
        gt_kinematics_sample.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));

        // ---- ground truth: revocation stages (v2) -----------------------------------
        let mut gt_revocation_stages = Vec::new();
        if self.profile.is_v2() {
            for s in &self.revocation_stages {
                let owner = digest_owner.get(s.id.as_str()).copied();
                gt_revocation_stages.push(GtRevocationStage {
                    revocation_id: s.id.clone(),
                    true_vehicle_id: owner
                        .map(vehicle_id)
                        .unwrap_or_else(|| "veh_unknown".to_string()),
                    stage: s.stage.clone(),
                    t: secs(s.t),
                    node_handle: s.node.map(|n| opaque_handle(n.0)),
                    visibility: oracle_tag(),
                });
            }
            gt_revocation_stages.sort_by(|a, b| {
                a.revocation_id
                    .cmp(&b.revocation_id)
                    .then_with(|| a.t.partial_cmp(&b.t).unwrap_or(core::cmp::Ordering::Equal))
                    .then_with(|| a.stage.cmp(&b.stage))
            });
        }

        let rsu_count = self.devices.values().filter(|d| d.is_rsu).count() as u64;

        Ok(MaDataset {
            profile: self.profile,
            ma_reports,
            ma_cert_status,
            ma_investigations,
            ma_crl_events,
            ma_crl_downloads,
            ma_report_transport,
            gt_vehicle,
            gt_identity_map,
            gt_attacks,
            gt_report_labels,
            gt_linkage_revocation,
            gt_emissions_sample,
            gt_kinematics_sample,
            gt_revocation_stages,
            rsu_count,
            run_duration_s,
        })
    }
}

/// An opaque, stable handle for a node — never an identity.
///
/// The legacy `_rsu_node` used a truncated SHA-256 of the certificate digest for exactly
/// this purpose, and `resolved_case_handle` used one of the case id. Both are here: a
/// handle is a hash of a domain-separated string, so it is reproducible across runs of the
/// same scenario and carries no identity a consumer can invert.
fn opaque_handle(node: u32) -> String {
    let hex = v2xw_core::hash::sha256_hex(format!("v2xw-node-handle|{node}").as_bytes());
    format!("nd_{}", &hex[..12])
}

fn decode<V: serde::de::DeserializeOwned>(rec: &RecordedRecord) -> Result<V> {
    serde_json::from_slice(&rec.json).map_err(|e| {
        RecordError::malformed(
            "ma-dataset",
            format!("a record on {} does not fit its view: {e}", rec.channel),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(channel: &str, t: u64, json: &str) -> RecordedRecord {
        RecordedRecord {
            channel: channel.to_string(),
            sim_time: t,
            json: json.as_bytes().to_vec(),
        }
    }

    #[test]
    fn seconds_are_on_the_legacy_three_decimal_grid() {
        assert_eq!(secs(1_500_000_000), 1.5);
        assert_eq!(secs(1_000_000), 0.001);
        assert_eq!(
            secs(1_500_000),
            0.002,
            "rounded to the 1e-3 grid, not truncated"
        );
    }

    #[test]
    fn the_device_id_format_is_the_legacy_one() {
        assert_eq!(vehicle_id(3), "veh_003");
        assert_eq!(vehicle_id(42), "veh_042");
        assert_eq!(vehicle_id(1234), "veh_1234");
    }

    #[test]
    fn an_unbound_subject_digest_is_refused_rather_than_written() {
        // R3 in the frozen audit: every subject digest must resolve. A dataset that
        // cannot satisfy it is a defect in the run, so the exporter says so instead of
        // writing a file that fails somebody else's CI.
        let mut a = DatasetAssembler::new(DatasetProfile::V1);
        a.ingest(&rec(
            "ma.report",
            1,
            r#"{"t":1,"subject":"nobody","reporter":1}"#,
        ))
        .expect("ingest");
        let e = a
            .finish()
            .expect_err("an unresolvable subject must be refused");
        assert!(e.to_string().contains("belongs to no device"), "{e}");
    }

    #[test]
    fn a_sampling_stride_takes_every_nth_record_without_touching_an_rng() {
        let mut a = DatasetAssembler::new(DatasetProfile::V1).with_sample_stride(3);
        a.ingest(&rec(
            "gt.spawn",
            0,
            r#"{"t":0,"actor":1,"is_attacker":false}"#,
        ))
        .expect("spawn");
        for i in 0..9u64 {
            let json = format!(
                r#"{{"t":{},"actor":1,"x_m":{}.0,"y_m":0.0,"speed_mps":10.0}}"#,
                i * 1_000_000_000,
                i
            );
            a.ingest(&rec("gt.kinematics", i * 1_000_000_000, &json))
                .expect("kin");
        }
        let ds = a.finish().expect("finish");
        assert_eq!(ds.gt_emissions_sample.len(), 3);
        assert_eq!(ds.gt_emissions_sample[0].emit_id, "emt_00000001");
        assert_eq!(ds.gt_emissions_sample[0].true_x, 0.0);
        assert_eq!(ds.gt_emissions_sample[1].true_x, 3.0);
    }

    #[test]
    fn the_v1_profile_writes_the_constant_validity_window_and_v2_writes_the_real_one() {
        let build = |profile| {
            let mut a = DatasetAssembler::new(profile);
            a.ingest(&rec(
                "gt.spawn",
                0,
                r#"{"t":0,"actor":1,"is_attacker":false}"#,
            ))
            .expect("spawn");
            a.ingest(&rec(
                "sec.cert",
                2_000_000_000,
                r#"{"t":2000000000,"node":1,"event":"change","digest":"aabb"}"#,
            ))
            .expect("cert");
            a.ingest(&rec(
                "sec.cert",
                5_000_000_000,
                r#"{"t":5000000000,"node":1,"event":"change","digest":"aabb"}"#,
            ))
            .expect("cert");
            a.finish().expect("finish")
        };
        let v1 = build(DatasetProfile::V1);
        assert_eq!(v1.ma_cert_status[0].valid_from, 0.0);
        assert_eq!(v1.ma_cert_status[0].valid_to, v1.run_duration_s);
        assert_eq!(v1.ma_cert_status[0].crl_status, "active");
        let v2 = build(DatasetProfile::V2);
        assert_eq!(v2.ma_cert_status[0].valid_from, 2.0);
        assert_eq!(v2.ma_cert_status[0].valid_to, 5.0);
        assert_eq!(
            v2.ma_cert_status[0].crl_status, "unknown",
            "v2 says unknown where v1 always said active"
        );
    }

    #[test]
    fn a_revoked_device_counts_once_however_many_pseudonyms_it_rotated_through() {
        // The audit's CNT3 counts distinct devices, not certificates.
        let mut a = DatasetAssembler::new(DatasetProfile::V1);
        a.ingest(&rec(
            "gt.spawn",
            0,
            r#"{"t":0,"actor":7,"is_attacker":true}"#,
        ))
        .expect("spawn");
        for (t, d) in [(1u64, "aa"), (2, "bb"), (3, "cc")] {
            let json = format!(
                r#"{{"t":{},"node":7,"event":"change","digest":"{d}"}}"#,
                t * 1_000_000_000
            );
            a.ingest(&rec("sec.cert", t * 1_000_000_000, &json))
                .expect("cert");
        }
        a.ingest(&rec(
            "ma.decision",
            4_000_000_000,
            r#"{"t":4000000000,"subject":"bb","decision":"revoke"}"#,
        ))
        .expect("decision");
        let ds = a.finish().expect("finish");
        assert_eq!(
            ds.ma_cert_status
                .iter()
                .filter(|c| c.crl_status == "revoked")
                .count(),
            3,
            "every pseudonym of the device is revoked"
        );
        assert_eq!(ds.revoked_devices().len(), 1, "but it is one device");
        assert_eq!(ds.counts()["revoked"], 1);
    }
}
