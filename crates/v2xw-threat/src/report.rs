//! The misbehaviour report: shaped so the protocol crate can submit it and the
//! misbehaviour authority can act on it.
//!
//! # Shape
//!
//! [`MisbehaviourReport`] is the legacy `MaReport`
//! (`legacy/scms_sim_ref/schemas/records.py :: MaReport`, itself TS 103 759-shaped) field
//! for field, plus the fusion fingerprint the machine-learning contract reads as
//! `detnorm_*`. Keeping the field set means a run's reports can be scored by the legacy
//! `validate.py` and compared with the corpus, which 07-threats §7 requires of the
//! evaluation harness.
//!
//! # Digests only
//!
//! Every identity in a report is a pseudonym certificate digest. A reporter has nothing
//! else — that is the point of pseudonymity — and neither does the authority until it runs
//! the protocol's identity resolution. Nothing in this module can name an actor, and the
//! `detnorm_*` fingerprint it carries is the detector's own output, which came from belief
//! only.
//!
//! # Forged reports
//!
//! [`forge`] builds a report against a subject the reporter has no evidence about: the
//! collusion and report-poisoning families of 07-threats §2.1 and §2.2. It fills the
//! fingerprint from the same formulas a genuine report uses with plausible inputs, drawn
//! from the attacker's own keyed stream, because the legacy engine learned the hard way
//! that a constant fabricated fingerprint is a free collusion oracle for any model
//! trained on the dataset (`run.py`, the collusion pass).

use serde::{Deserialize, Serialize};
use v2xw_core::ids::NodeId;
use v2xw_core::rng::RngStream;
use v2xw_core::time::SimTime;

use crate::detect::{DetectorId, DetectorParams, Fingerprint, Verdict};
use crate::records::q;

/// What a reporter concluded about the subject's credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertValidity {
    /// The signature verified.
    pub sig_valid: bool,
    /// The certificate was inside its validity window.
    pub not_expired: bool,
    /// The certificate was not on the reporter's copy of the revocation list.
    pub not_revoked: bool,
    /// The certificate chained to a trust anchor the reporter accepts.
    pub chain_ok: bool,
}

impl Default for CertValidity {
    fn default() -> Self {
        Self {
            sig_valid: true,
            not_expired: true,
            not_revoked: true,
            chain_ok: true,
        }
    }
}

/// One check's output, as the report carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectorOutput {
    /// The check's id.
    pub check_id: String,
    /// Its normalised score, quantised.
    pub score: f64,
    /// `fail` when the check fired, `pass` otherwise.
    pub verdict: String,
}

/// A misbehaviour report, TS 103 759-shaped, digests only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MisbehaviourReport {
    /// The report's id, unique within the run.
    pub report_id: String,
    /// When the report reached the authority, on the authority's clock.
    pub ingest_time: SimTime,
    /// When the reporter's detector fired.
    pub detection_time: SimTime,
    /// When the reporter generated the report.
    pub generation_time: SimTime,
    /// The reporting node — carried so a run can charge the report to a node's byte
    /// budget. It is not an identity the authority uses to judge the report; that is
    /// [`Self::reporter_cert_digest`].
    pub reporter: Option<NodeId>,
    /// The reporter's pseudonym certificate digest, hex.
    pub reporter_cert_digest: String,
    /// The subject's pseudonym certificate digest, hex.
    pub subject_cert_digest: String,
    /// The checks that fired, highest score first.
    pub reason_codes: Vec<String>,
    /// Each fired check's output.
    pub detector_outputs: Vec<DetectorOutput>,
    /// What the reporter concluded about the subject's credentials.
    pub cert_validity: CertValidity,
    /// References to the evidence messages the report attaches.
    pub evidence_msg_refs: Vec<String>,
    /// The spatio-temporal bounding box of the evidence, `[xmin, ymin, xmax, ymax]`.
    pub st_bbox: [f64; 4],
    /// The start of the evidence window.
    pub st_tstart: SimTime,
    /// Its end.
    pub st_tend: SimTime,
    /// Whether the authority already holds this report.
    pub duplicate_flag: bool,
    /// The leading check's score.
    pub detector_score: f64,
    /// The largest score in the fingerprint.
    pub detector_score_norm: f64,
    /// The position confidence the subject broadcast, metres.
    pub subject_pos_confidence: f64,
    /// The station type the subject declared.
    pub station_type: String,
    /// The whole fusion fingerprint, `(check id, score)` in
    /// [`DetectorId::ALL`] order — the `detnorm_*` columns.
    pub detnorm: Vec<(String, f64)>,
}

/// What a reporter attaches to a verdict to make it a report: the evidence's extent, what
/// it concluded about the subject's credentials, and the two timestamps.
///
/// A struct rather than six more arguments, because the six travel together everywhere and
/// a positional call with two adjacent `SimTime`s and two adjacent `f64`s is a call whose
/// arguments can be silently transposed.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// The position confidence the subject broadcast, metres.
    pub subject_pos_confidence_m: f64,
    /// The station type the subject declared.
    pub station_type: String,
    /// What the reporter concluded about the subject's credentials.
    pub cert_validity: CertValidity,
    /// The evidence's spatial extent, `[xmin, ymin, xmax, ymax]`.
    pub bbox: [f64; 4],
    /// When the reporter's detector fired.
    pub detection_time: SimTime,
    /// When the report reached the authority.
    pub ingest_time: SimTime,
}

impl Evidence {
    /// Evidence with default credential conclusions and a point extent.
    #[must_use]
    pub fn at(
        detection_time: SimTime,
        ingest_time: SimTime,
        subject_pos_confidence_m: f64,
    ) -> Self {
        Self {
            subject_pos_confidence_m,
            station_type: crate::obs::StationType::Vehicle.as_str().to_string(),
            cert_validity: CertValidity::default(),
            bbox: [0.0; 4],
            detection_time,
            ingest_time,
        }
    }
}

impl MisbehaviourReport {
    /// Builds a report from a detector verdict.
    ///
    /// Returns `None` when nothing fired: a report with no fired check is not a report.
    /// Every float is quantised at construction (build decision D9).
    #[must_use]
    pub fn from_verdict(
        report_id: impl Into<String>,
        reporter: NodeId,
        reporter_cert_digest: impl Into<String>,
        verdict: &Verdict,
        evidence: &Evidence,
    ) -> Option<Self> {
        let leading = verdict.leading()?;
        let Evidence {
            subject_pos_confidence_m,
            station_type,
            cert_validity,
            bbox: evidence_bbox,
            detection_time,
            ingest_time,
        } = evidence.clone();
        let report_id = report_id.into();
        Some(Self {
            evidence_msg_refs: vec![format!("{report_id}-m")],
            report_id,
            ingest_time,
            detection_time,
            generation_time: detection_time,
            reporter: Some(reporter),
            reporter_cert_digest: reporter_cert_digest.into(),
            subject_cert_digest: verdict.subject.clone(),
            reason_codes: verdict
                .fired
                .iter()
                .map(|o| o.detector.as_str().to_string())
                .collect(),
            detector_outputs: verdict
                .fired
                .iter()
                .map(|o| DetectorOutput {
                    check_id: o.detector.as_str().to_string(),
                    score: q(o.score),
                    verdict: "fail".to_string(),
                })
                .collect(),
            cert_validity,
            st_bbox: [
                q(evidence_bbox[0]),
                q(evidence_bbox[1]),
                q(evidence_bbox[2]),
                q(evidence_bbox[3]),
            ],
            st_tstart: detection_time,
            st_tend: detection_time,
            duplicate_flag: false,
            detector_score: q(leading.score),
            detector_score_norm: q(verdict.fingerprint.max()),
            subject_pos_confidence: q(subject_pos_confidence_m),
            station_type,
            detnorm: fingerprint_columns(&verdict.fingerprint),
        })
    }

    /// The leading reason: the check the authority correlates on.
    #[must_use]
    pub fn leading_reason(&self) -> Option<&str> {
        self.reason_codes.first().map(String::as_str)
    }

    /// The record this report writes on `ma.report` when it reaches the authority.
    #[must_use]
    pub fn record(&self) -> crate::records::MaReportRecord {
        crate::records::MaReportRecord {
            t: self.ingest_time,
            reporter: self.reporter,
            subject: self.subject_cert_digest.clone(),
            detector: self.leading_reason().map(str::to_string),
        }
    }
}

/// The fingerprint as quantised `(check id, score)` columns, in [`DetectorId::ALL`] order.
#[must_use]
pub fn fingerprint_columns(f: &Fingerprint) -> Vec<(String, f64)> {
    f.iter()
        .map(|(d, v)| (d.as_str().to_string(), q(v)))
        .collect()
}

/// The distributions a forged report's fabricated evidence is drawn from.
///
/// Every default is the legacy collusion pass's own
/// (`run.py`, "Fabricate PLAUSIBLE evidence from the colluder's own keyed stream").
#[derive(Debug, Clone, PartialEq)]
pub struct ForgeryProfile {
    /// The leading fabricated score, drawn uniformly in this range. Legacy
    /// `cfab.uniform(1.05, 4.0)` on `positionSpeedInconsistency`.
    pub leading_score: (f64, f64),
    /// The fabricated Sybil count, drawn uniformly over the integers in this range and
    /// divided by `sybil_min_certs`. Legacy `cfab.randint(1, 2)`.
    pub sybil_count: (u64, u64),
    /// The fabricated repetition count, divided by `freq_max`. Legacy
    /// `cfab.randint(1, 3)`.
    pub beacon_count: (u64, u64),
    /// The fabricated staleness score. Legacy `cfab.uniform(0.0, 0.15)`.
    pub stale_score: (f64, f64),
    /// The fabricated subject position confidence, metres. Legacy
    /// `cfab.uniform(2.0, 9.0)`.
    pub pos_confidence_m: (f64, f64),
    /// The check the forged report leads with.
    pub leading_check: DetectorId,
    /// The victim receiver's `sybil_min_certs`, so the fabricated co-location score lands
    /// in the range a genuine report's does.
    pub sybil_min_certs: u32,
    /// The victim receiver's `freq_max`, for the same reason.
    pub freq_max: f64,
}

impl Default for ForgeryProfile {
    fn default() -> Self {
        Self {
            leading_score: (1.05, 4.0),
            sybil_count: (1, 2),
            beacon_count: (1, 3),
            stale_score: (0.0, 0.15),
            pos_confidence_m: (2.0, 9.0),
            leading_check: DetectorId::PositionSpeedInconsistency,
            sybil_min_certs: DetectorParams::default().sybil_min_certs,
            freq_max: DetectorParams::default().freq_max,
        }
    }
}

/// Builds a forged report against `subject`, drawing the fabricated evidence from `rng`.
///
/// [`ForgeryProfile::sybil_min_certs`] and [`ForgeryProfile::freq_max`] are the *victim
/// receiver's* thresholds, so the fabricated structural scores land in the same range a
/// genuine report's do. That is the point: a forged report whose radio detectors are
/// exactly zero is separable from every genuine one by those structural zeros, which makes
/// collusion trivially detectable in the dataset and the measured robustness meaningless.
///
/// The caller is the attacker path, so this function draws from a stream the attacker
/// owns; it has no access to the subject's real behaviour, which is exactly why the
/// evidence has to be fabricated from a distribution rather than measured.
#[must_use]
pub fn forge(
    report_id: impl Into<String>,
    reporter: NodeId,
    reporter_cert_digest: impl Into<String>,
    subject_cert_digest: impl Into<String>,
    profile: &ForgeryProfile,
    t: SimTime,
    rng: &mut RngStream,
) -> MisbehaviourReport {
    let lead = rng.uniform(profile.leading_score.0, profile.leading_score.1);
    let sybil =
        profile.sybil_count.0 + rng.below(profile.sybil_count.1 - profile.sybil_count.0 + 1);
    let beacons =
        profile.beacon_count.0 + rng.below(profile.beacon_count.1 - profile.beacon_count.0 + 1);
    let stale = rng.uniform(profile.stale_score.0, profile.stale_score.1);
    let conf = rng.uniform(profile.pos_confidence_m.0, profile.pos_confidence_m.1);

    let mut f = Fingerprint::default();
    f.set(profile.leading_check, lead);
    f.set(
        DetectorId::SybilCoLocation,
        sybil as f64 / f64::from(profile.sybil_min_certs),
    );
    f.set(
        DetectorId::BeaconFrequency,
        beacons as f64 / profile.freq_max,
    );
    f.set(DetectorId::StaleOrReplay, stale);

    let report_id = report_id.into();
    let subject_cert_digest = subject_cert_digest.into();
    MisbehaviourReport {
        evidence_msg_refs: vec![format!("{report_id}-m")],
        report_id,
        ingest_time: t,
        detection_time: t,
        generation_time: t,
        reporter: Some(reporter),
        reporter_cert_digest: reporter_cert_digest.into(),
        subject_cert_digest: subject_cert_digest.clone(),
        reason_codes: vec![profile.leading_check.as_str().to_string()],
        detector_outputs: vec![DetectorOutput {
            check_id: profile.leading_check.as_str().to_string(),
            score: q(lead),
            verdict: "fail".to_string(),
        }],
        cert_validity: CertValidity::default(),
        st_bbox: [0.0, 0.0, 0.0, 0.0],
        st_tstart: t,
        st_tend: t,
        duplicate_flag: false,
        detector_score: q(lead),
        detector_score_norm: q(f.max()),
        subject_pos_confidence: q(conf),
        station_type: crate::obs::StationType::Vehicle.as_str().to_string(),
        detnorm: fingerprint_columns(&f),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Observation;

    fn verdict() -> Verdict {
        let mut f = Fingerprint::default();
        f.set(DetectorId::PositionJump, 2.5);
        f.set(DetectorId::PositionSpeedInconsistency, 1.2);
        Verdict {
            subject: "aabbccdd".to_string(),
            fingerprint: f,
            fired: vec![
                Observation {
                    detector: DetectorId::PositionJump,
                    score: 2.5,
                    subject: "aabbccdd".to_string(),
                    at: 1,
                },
                Observation {
                    detector: DetectorId::PositionSpeedInconsistency,
                    score: 1.2,
                    subject: "aabbccdd".to_string(),
                    at: 1,
                },
            ],
        }
    }

    #[test]
    fn a_report_leads_with_the_highest_scoring_check_and_carries_the_whole_fingerprint() {
        let r = MisbehaviourReport::from_verdict(
            "rpt_00001",
            NodeId::new(3),
            "ffee",
            &verdict(),
            &Evidence {
                bbox: [0.0, 0.0, 1.0, 1.0],
                ..Evidence::at(1_000_000_000, 1_200_000_000, 2.0)
            },
        )
        .unwrap();
        assert_eq!(r.leading_reason(), Some("positionJump"));
        assert_eq!(r.detector_score, 2.5);
        assert_eq!(r.detector_score_norm, 2.5);
        assert_eq!(r.detnorm.len(), DetectorId::ALL.len());
        assert_eq!(r.evidence_msg_refs, ["rpt_00001-m"]);
        assert_eq!(r.record().subject, "aabbccdd");
        assert_eq!(r.record().t, 1_200_000_000);
    }

    #[test]
    fn a_verdict_with_nothing_fired_is_not_a_report() {
        let empty = Verdict {
            subject: "aa".to_string(),
            fingerprint: Fingerprint::default(),
            fired: Vec::new(),
        };
        assert!(
            MisbehaviourReport::from_verdict(
                "r",
                NodeId::new(1),
                "bb",
                &empty,
                &Evidence::at(0, 0, 1.0),
            )
            .is_none()
        );
    }

    /// The legacy lesson: a forged report must not be separable by structural zeros.
    #[test]
    fn a_forged_report_carries_plausible_structural_scores() {
        let reg = v2xw_core::rng::RngRegistry::new(11);
        let mut rng = reg.ephemeral(
            v2xw_core::rng::RngDomain::Collusion,
            v2xw_core::rng::EntityRef::Node(NodeId::new(4)),
        );
        let r = forge(
            "rpt_00002",
            NodeId::new(4),
            "cafe",
            "beef",
            &ForgeryProfile::default(),
            5_000_000_000,
            &mut rng,
        );
        let map: std::collections::BTreeMap<&str, f64> =
            r.detnorm.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert!(map["positionSpeedInconsistency"] >= 1.05);
        assert!(map["positionSpeedInconsistency"] <= 4.0);
        assert!(
            map["sybilCoLocation"] > 0.0,
            "structural zero is a free oracle"
        );
        assert!(
            map["beaconFrequency"] > 0.0,
            "structural zero is a free oracle"
        );
        assert!(map["staleOrReplay"] > 0.0);
        assert!(r.subject_pos_confidence >= 2.0 && r.subject_pos_confidence <= 9.0);
    }
}
