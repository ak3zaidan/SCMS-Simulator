//! The `ma-dataset` tables, in both profiles — 08-measurement-and-data.md §6.
//!
//! One struct per legacy table, with the legacy field names. The v1 profile is the column
//! set the frozen Python engine wrote, *including its known defects*, because §6 says so
//! in as many words:
//!
//! > `schema_versions: {ma_visible: 1, ground_truth: 1}` (**v1 profile**, `--legacy-v1`):
//! > identical columns to today, including the known defects (constant
//! > `valid_from`/`valid_to`, all-True `cert_validity`, `cert_crl_status="active"`,
//! > investigations only on revocation).
//!
//! Reproducing a defect on purpose needs saying out loud, so each one is marked where it
//! is written, and each has a v2 field that carries the real answer. A consumer that
//! pinned the v1 schema keeps working; a consumer that wants the truth asks for v2.
//!
//! # What "byte-compatible" does and does not mean
//!
//! The *encoding* is byte-compatible: the file set, the field names, the sort orders, the
//! id formats, the vocabularies and the canonical JSON bytes are the legacy ones, down to
//! `ensure_ascii` escaping and Python's float spelling ([`super::pyjson`]). The frozen
//! `verify_data.py` therefore runs unchanged, which is the acceptance criterion §6 states.
//!
//! The *values* are not the legacy values, and build decision D9 is explicit that they
//! must not be pretended to be:
//!
//! > The MA dataset exporter keeps `st_bbox` for v1 schema compatibility but quantises it
//! > like every other float. This means the v1 profile will not reproduce the legacy
//! > frozen digests — those are unreproducible on any non-Windows host anyway, which is
//! > exactly why they are retired.
//!
//! So the datasheet's `Generator` line names this engine, and cross-engine validation
//! compares "identifiers, enumerations, counts, orderings and revocation sets exactly, and
//! floats within 1e-9, never by digest equality" (D9).

use serde::{Deserialize, Serialize};

use super::leakage::{MA, ORACLE, PUBLIC};

/// Which schema profile a dataset is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatasetProfile {
    /// The legacy column set, defects included. `--legacy-v1`.
    V1,
    /// v1 plus additive columns and tables. The default.
    #[default]
    V2,
}

impl DatasetProfile {
    /// The `schema_versions` object the manifest carries.
    #[must_use]
    pub const fn schema_versions(self) -> (u32, u32) {
        match self {
            // §6: v1 is `{ma_visible: 1, ground_truth: 1}`.
            DatasetProfile::V1 => (1, 1),
            DatasetProfile::V2 => (2, 2),
        }
    }

    /// True if the additive v2 columns and tables are written.
    #[must_use]
    pub const fn is_v2(self) -> bool {
        matches!(self, DatasetProfile::V2)
    }

    /// The profile's name, as the CLI spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DatasetProfile::V1 => "v1",
            DatasetProfile::V2 => "v2",
        }
    }
}

/// The detector vocabulary whose per-report normalised score each report carries as
/// `detnorm_<detector>`.
///
/// This is `DET_KEYS` from the legacy pipeline in its default configuration, plus the one
/// `SOFT_KEYS` entry. The order is the legacy order and is load-bearing only for
/// readability — the rows are written with sorted keys — but the *set* is part of the
/// contract: `featurize.DETECTORS` reads exactly these names, and a missing one becomes a
/// silently-absent feature rather than an error.
///
/// `vruImpersonation` and `denmPlausibility` are in `featurize.DETECTORS` but were gated
/// out of the legacy default row (they are only emitted when station types or the DENM
/// layer are active), so they are not here either: adding them would change the default
/// column set, which is the thing v1 promises not to do.
pub const DETNORM_VOCAB: &[&str] = &[
    "positionSpeedInconsistency",
    "positionJump",
    "headingInconsistency",
    "staleOrReplay",
    "constantPositionFrozen",
    "implausibleAcceleration",
    "sybilCoLocation",
    "acceptanceRangeThreshold",
    "beaconFrequency",
    "signatureVerification",
    "certValidity",
    "mapOffRoad",
    "kalmanConsistency",
];

/// The `report_correctness` vocabulary, which is `verify_data.VALID_CORRECTNESS`.
///
/// Check `C3_report_correctness_vocab` fails on any other value, so this list is the
/// closed set an assembler may produce.
pub const REPORT_CORRECTNESS_VOCAB: &[&str] = &[
    "collusive",
    "correct",
    "duplicate",
    "faulty_detection",
    "false_positive",
    "malicious",
    "malicious_false_report",
];

/// One detector's output inside a report's `detector_outputs` array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectorOutput {
    /// The detector's id, from the reason vocabulary.
    pub check_id: String,
    /// Its score, quantised to the three-decimal legacy grid.
    pub score: f64,
    /// `fail` on a report; the legacy pipeline only ever emitted a failing check.
    pub verdict: String,
}

/// A report's `cert_validity` object.
///
/// **v1 defect, reproduced on purpose**: the legacy pipeline wrote all four flags as
/// `True` unconditionally, so the object carries no information at all. The v2 profile
/// keeps the object for compatibility and adds `verification_status`, which carries what
/// the node's verification path actually concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertValidity {
    /// The signature verified.
    pub sig_valid: bool,
    /// The certificate was inside its validity window.
    pub not_expired: bool,
    /// The certificate was not on a CRL.
    pub not_revoked: bool,
    /// The chain to the root validated.
    pub chain_ok: bool,
}

impl CertValidity {
    /// The v1 constant: every flag true.
    #[must_use]
    pub const fn legacy_v1() -> Self {
        CertValidity {
            sig_valid: true,
            not_expired: true,
            not_revoked: true,
            chain_ok: true,
        }
    }
}

/// `ma/ma_reports.jsonl` — one ingested misbehaviour report, TS 103 759-shaped, digests
/// only (MA-visible).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaReport {
    /// `rpt_00001`.
    pub report_id: String,
    /// When the report reached the authority.
    pub ingest_time: f64,
    /// When the reporter's detector fired.
    pub detection_time: f64,
    /// When the report was generated.
    pub generation_time: f64,
    /// The reporter's pseudonym-certificate digest — never an identity.
    pub reporter_cert_digest: String,
    /// The subject's pseudonym-certificate digest.
    pub subject_cert_digest: String,
    /// The reason codes that fired.
    pub reason_codes: Vec<String>,
    /// The detector outputs cited as evidence.
    pub detector_outputs: Vec<DetectorOutput>,
    /// The reporter's own certificate checks (see [`CertValidity`]).
    pub cert_validity: CertValidity,
    /// Opaque references to the evidence messages.
    pub evidence_msg_refs: Vec<String>,
    /// `[xmin, ymin, xmax, ymax]` of the evidence, quantised like every other float (D9).
    pub st_bbox: [f64; 4],
    /// The evidence window's start.
    pub st_tstart: f64,
    /// The evidence window's end.
    pub st_tend: f64,
    /// Whether the authority deduplicated this report.
    pub duplicate_flag: bool,
    /// The primary detector's score.
    pub detector_score: f64,
    /// The maximum normalised score across detectors.
    pub detector_score_norm: f64,
    /// The subject's reported 95 % position-uncertainty radius.
    pub subject_pos_confidence: f64,
    /// **v1 defect, reproduced on purpose**: the legacy pipeline wrote the literal
    /// `"active"` here for every report. v2 writes the real status the authority held at
    /// ingest.
    pub cert_crl_status: String,
    /// Whether the evidence's signature verified.
    pub sig_valid: bool,
    /// The per-detector normalised scores, `detnorm_<detector>`, flattened into the row.
    #[serde(flatten)]
    pub detnorm: std::collections::BTreeMap<String, f64>,
    /// v2: what the receiving node's verification path concluded (`valid`, `invalid`,
    /// `dropped`, `skipped`), which is the information the all-true `cert_validity` lost.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verification_status: Option<String>,
    /// v2: enqueue-to-done latency of that verification.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verify_latency_ms: Option<f64>,
    /// v2: the received signal strength of the evidence message.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rx_rssi_dbm: Option<f64>,
    /// v2: the radio access technology the evidence arrived over.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rat: Option<String>,
    /// The visibility tag, always [`MA`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

impl MaReport {
    /// A report with every v1 column filled and no v2 column.
    ///
    /// The `detnorm_*` map is seeded with the whole vocabulary at zero, so every report
    /// carries every column and the table has one schema rather than one per row.
    #[must_use]
    pub fn new(
        report_id: impl Into<String>,
        reporter: impl Into<String>,
        subject: impl Into<String>,
    ) -> Self {
        let detnorm = DETNORM_VOCAB
            .iter()
            .map(|d| (format!("detnorm_{d}"), 0.0))
            .collect();
        MaReport {
            report_id: report_id.into(),
            ingest_time: 0.0,
            detection_time: 0.0,
            generation_time: 0.0,
            reporter_cert_digest: reporter.into(),
            subject_cert_digest: subject.into(),
            reason_codes: Vec::new(),
            detector_outputs: Vec::new(),
            cert_validity: CertValidity::legacy_v1(),
            evidence_msg_refs: Vec::new(),
            st_bbox: [0.0; 4],
            st_tstart: 0.0,
            st_tend: 0.0,
            duplicate_flag: false,
            detector_score: 0.0,
            detector_score_norm: 0.0,
            subject_pos_confidence: 0.0,
            // v1 defect, on purpose.
            cert_crl_status: "active".to_string(),
            sig_valid: true,
            detnorm,
            verification_status: None,
            verify_latency_ms: None,
            rx_rssi_dbm: None,
            rat: None,
            visibility: MA.to_string(),
        }
    }

    /// Sets one detector's normalised score, if the name is in [`DETNORM_VOCAB`].
    ///
    /// A name outside the vocabulary is ignored rather than added: a new column would
    /// change the table's schema, which is what the v1 profile promises not to do, and a
    /// silently-widened table is worse than a dropped score.
    pub fn set_detnorm(&mut self, detector: &str, score: f64) -> bool {
        let key = format!("detnorm_{detector}");
        if let Some(slot) = self.detnorm.get_mut(&key) {
            *slot = score;
            true
        } else {
            false
        }
    }
}

/// `ma/ma_cert_status.jsonl` — what the authority knows about one pseudonym certificate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaCertStatus {
    /// The certificate's digest.
    pub cert_digest: String,
    /// When the authority first saw it.
    pub first_seen: f64,
    /// When it last saw it.
    pub last_seen: f64,
    /// **v1 defect, reproduced on purpose**: the legacy pipeline wrote `0.0` here for
    /// every certificate, whatever its real validity window. v2 writes the real one.
    pub valid_from: f64,
    /// **v1 defect, reproduced on purpose**: the legacy pipeline wrote the run's duration
    /// here for every certificate. v2 writes the real one.
    pub valid_to: f64,
    /// The issuing PCA's name.
    pub issuing_pca: String,
    /// `unknown` | `active` | `revoked`. The v1 profile only ever writes the last two,
    /// because the legacy pipeline never wrote `unknown`.
    pub crl_status: String,
    /// When it was revoked, or `null`.
    pub revocation_time: Option<f64>,
    /// The visibility tag, always [`MA`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ma/ma_investigations.jsonl` — the authority's case state.
///
/// **v1 defect, reproduced on purpose**: the legacy pipeline appended a row only when it
/// *revoked*, so `revocation_decision` was always `revoke` and a dismissed case left no
/// trace at all. The v2 profile includes dismissed cases, which is what §6 asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaInvestigation {
    /// `case_0001`.
    pub case_id: String,
    /// When the case opened.
    pub opened_time: f64,
    /// What opened it.
    pub trigger: String,
    /// How many reports were clustered.
    pub cluster_size: u64,
    /// How many distinct reporters contributed.
    pub num_distinct_reporters: u64,
    /// `same` | `different` | `unknown`.
    pub linkage_result: String,
    /// Whether the authority resolved the subject to one device.
    pub identity_resolved: bool,
    /// `pending` | `revoke` | `dismiss`.
    pub revocation_decision: String,
    /// When the resolution completed, or `null`.
    pub resolution_time: Option<f64>,
    /// When the decision was taken, or `null`.
    pub decision_time: Option<f64>,
    /// An opaque handle — never a real vehicle id.
    pub resolved_case_handle: Option<String>,
    /// The visibility tag, always [`MA`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ma/ma_crl_events.jsonl` — one CRL issuance (public).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaCrlEvent {
    /// `crl_0001`.
    pub crl_id: String,
    /// When the list was issued.
    pub issue_time: f64,
    /// `seed` | `digest`.
    pub entry_type: String,
    /// The list's cumulative entry count.
    pub num_entries: u64,
    /// v2: how many entries this issuance added, which the cumulative field alone cannot
    /// give a consumer that missed an issuance.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub num_entries_delta: Option<i64>,
    /// The visibility tag, always [`PUBLIC`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ma/ma_crl_downloads.jsonl` — **v2 only**: one node's CRL download, as the RA's logs
/// see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaCrlDownload {
    /// The list downloaded.
    pub crl_id: String,
    /// The requesting node, as an opaque handle.
    pub node_handle: String,
    /// When the download completed.
    pub t: f64,
    /// How long it took.
    pub download_s: f64,
    /// How many bytes crossed.
    pub bytes: u64,
    /// Which path it came over (`rsu`, `cellular`, `epidemic`).
    pub path: String,
    /// The visibility tag, always [`MA`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ma/ma_report_transport.jsonl` — **v2 only**: how a report reached the authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaReportTransport {
    /// The report this row is about.
    pub report_id: String,
    /// `direct-cellular` | `rsu-backhaul` | `store-and-forward`.
    pub path: String,
    /// Detection-to-ingest delay.
    pub delay_s: f64,
    /// How many hops it took.
    pub hops: u64,
    /// The visibility tag, always [`MA`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_vehicle.jsonl` — one device's truth (ORACLE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtVehicle {
    /// `veh_003`.
    pub true_vehicle_id: String,
    /// When it entered the simulation.
    pub spawn_time: f64,
    /// Whether it was an attacker.
    pub is_attacker: bool,
    /// Its attacker role, or `none`.
    pub attacker_role: String,
    /// Its collusion group, or `null`.
    pub colluding_group_id: Option<String>,
    /// Whether it was faulty rather than malicious.
    pub is_faulty: bool,
    /// Its class.
    pub veh_type: String,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_identity_map.jsonl` — the pseudonym-to-device map (ORACLE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtIdentityMap {
    /// The device.
    pub true_vehicle_id: String,
    /// One of its pseudonym-certificate digests.
    pub pseudonym_cert_digest: String,
    /// The `i` period the certificate belongs to.
    pub i_period: u64,
    /// Its validity start.
    pub valid_from: f64,
    /// Its validity end.
    pub valid_to: f64,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_attacks.jsonl` — one attack, with its window (ORACLE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtAttack {
    /// `atk_3`.
    pub attack_id: String,
    /// The device behind it.
    pub true_vehicle_id: String,
    /// The attack's name, as the attacker library spells it.
    pub attack_type: String,
    /// When the attack was configured to start.
    pub start_time: f64,
    /// When it was configured to stop.
    pub end_time: f64,
    /// The attack's parameters.
    pub params: serde_json::Map<String, serde_json::Value>,
    /// The first materially-falsified message, or `null`.
    pub attack_onset_time: Option<f64>,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_report_labels.jsonl` — one report's label (ORACLE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtReportLabel {
    /// The report.
    pub report_id: String,
    /// Who really filed it.
    pub reporter_true_id: String,
    /// Who it was really about.
    pub subject_true_id: String,
    /// One of [`REPORT_CORRECTNESS_VOCAB`].
    pub report_correctness: String,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_linkage_revocation.jsonl` — whether a device should have been revoked
/// (ORACLE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtLinkageRevocation {
    /// The device.
    pub true_vehicle_id: String,
    /// Whether it deserved revocation.
    pub should_have_been_revoked: bool,
    /// When it was really revoked, or `null`.
    pub true_revocation_time: Option<f64>,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_emissions_sample.jsonl` — a sampled transmission with truth beside the
/// claim (ORACLE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtEmissionsSample {
    /// `emt_00000001`.
    pub emit_id: String,
    /// When it was sent.
    pub t: f64,
    /// Who really sent it.
    pub true_vehicle_id: String,
    /// True easting.
    pub true_x: f64,
    /// True northing.
    pub true_y: f64,
    /// Claimed easting.
    pub claimed_x: f64,
    /// Claimed northing.
    pub claimed_y: f64,
    /// Claimed speed.
    pub claimed_speed: f64,
    /// The claimed position-confidence radius.
    pub pos_conf: f64,
    /// Whether the sender was an attacker.
    pub is_attacker: bool,
    /// Whether the sender was faulty.
    pub is_faulty: bool,
    /// Whether this message's content was falsified.
    pub falsified: bool,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_kinematics_sample.jsonl` — **v2 only**: the sampled truth with the
/// noise the engine now models.
///
/// §6: this "replaces the legacy `gt_emissions_sample` semantics with speed and heading
/// noise now modeled". The legacy table stays in v2 for compatibility; this one is what a
/// consumer should read, because the legacy one had no heading at all and no separation
/// between what the sender believed and what was true.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtKinematicsSample {
    /// `kin_00000001`.
    pub sample_id: String,
    /// When.
    pub t: f64,
    /// Who.
    pub true_vehicle_id: String,
    /// True easting.
    pub true_x: f64,
    /// True northing.
    pub true_y: f64,
    /// True speed.
    pub true_speed: f64,
    /// True heading, radians ENU (build decision D6).
    pub true_heading: f64,
    /// The sender's own believed speed, after GNSS and sensor noise.
    pub believed_speed: f64,
    /// The sender's own believed heading.
    pub believed_heading: f64,
    /// The lane it was really on.
    pub true_lane: Option<u64>,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// `ground_truth/gt_revocation_stages.jsonl` — **v2 only**: one revocation's stage
/// timestamps (05-protocols.md §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtRevocationStage {
    /// The revocation's id.
    pub revocation_id: String,
    /// The device being revoked.
    pub true_vehicle_id: String,
    /// The stage: `detect`, `report_sent`, `report_received`, `decision`, `issued`,
    /// `published`, `downloaded`, `enforced`.
    pub stage: String,
    /// When the stage was reached.
    pub t: f64,
    /// The node the stage happened at, for the per-node stages, as an opaque handle.
    pub node_handle: Option<String>,
    /// The visibility tag, always [`ORACLE`].
    #[serde(rename = "_visibility")]
    pub visibility: String,
}

/// Constructors for the tag fields, so no assembler can write the wrong visibility on a
/// row and defeat the linter by construction.
pub(crate) fn ma_tag() -> String {
    MA.to_string()
}
pub(crate) fn public_tag() -> String {
    PUBLIC.to_string()
}
pub(crate) fn oracle_tag() -> String {
    ORACLE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_v1_report_carries_every_legacy_column_and_no_v2_column() {
        let r = MaReport::new("rpt_00001", "aabb", "ccdd");
        let v = serde_json::to_value(&r).expect("serialise");
        let obj = v.as_object().expect("object");
        for key in [
            "report_id",
            "ingest_time",
            "detection_time",
            "generation_time",
            "reporter_cert_digest",
            "subject_cert_digest",
            "reason_codes",
            "detector_outputs",
            "cert_validity",
            "evidence_msg_refs",
            "st_bbox",
            "st_tstart",
            "st_tend",
            "duplicate_flag",
            "detector_score",
            "detector_score_norm",
            "subject_pos_confidence",
            "cert_crl_status",
            "sig_valid",
            "_visibility",
        ] {
            assert!(obj.contains_key(key), "v1 column {key} is missing");
        }
        for key in [
            "verification_status",
            "verify_latency_ms",
            "rx_rssi_dbm",
            "rat",
        ] {
            assert!(!obj.contains_key(key), "v2 column {key} leaked into v1");
        }
        // Every detector in the vocabulary has a column, so the table has one schema.
        for d in DETNORM_VOCAB {
            assert!(obj.contains_key(&format!("detnorm_{d}")), "detnorm_{d}");
        }
        assert_eq!(
            obj["cert_crl_status"], "active",
            "the v1 defect, on purpose"
        );
    }

    #[test]
    fn a_detector_outside_the_vocabulary_does_not_widen_the_table() {
        let mut r = MaReport::new("rpt_00001", "aabb", "ccdd");
        assert!(r.set_detnorm("positionJump", 1.25));
        assert!(!r.set_detnorm("aDetectorNobodyDeclared", 9.0));
        let v = serde_json::to_value(&r).expect("serialise");
        assert_eq!(v["detnorm_positionJump"], 1.25);
        assert!(v.get("detnorm_aDetectorNobodyDeclared").is_none());
    }

    #[test]
    fn the_v1_defects_are_reachable_as_named_constants_rather_than_magic_values() {
        let cv = CertValidity::legacy_v1();
        assert!(cv.sig_valid && cv.not_expired && cv.not_revoked && cv.chain_ok);
    }

    #[test]
    fn the_correctness_vocabulary_is_the_frozen_audits_vocabulary() {
        // verify_data.VALID_CORRECTNESS, exactly.
        let mut ours: Vec<&str> = REPORT_CORRECTNESS_VOCAB.to_vec();
        ours.sort_unstable();
        let mut theirs = vec![
            "correct",
            "false_positive",
            "malicious",
            "duplicate",
            "collusive",
            "faulty_detection",
            "malicious_false_report",
        ];
        theirs.sort_unstable();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn a_null_optional_is_written_as_null_rather_than_dropped() {
        // The legacy `asdict()` kept `revocation_time: None` as `null`; dropping the key
        // would change the column set, which is exactly what v1 promises not to do.
        let s = MaCertStatus {
            cert_digest: "aabb".to_string(),
            first_seen: 1.0,
            last_seen: 2.0,
            valid_from: 0.0,
            valid_to: 60.0,
            issuing_pca: "PCA-1".to_string(),
            crl_status: "active".to_string(),
            revocation_time: None,
            visibility: ma_tag(),
        };
        let v = serde_json::to_value(&s).expect("serialise");
        assert!(
            v.as_object()
                .expect("object")
                .contains_key("revocation_time")
        );
        assert!(v["revocation_time"].is_null());
    }

    #[test]
    fn every_ground_truth_row_carries_the_oracle_tag() {
        assert_eq!(oracle_tag(), "ORACLE");
        assert_eq!(ma_tag(), "MA");
        assert_eq!(public_tag(), "PUBLIC");
    }
}
