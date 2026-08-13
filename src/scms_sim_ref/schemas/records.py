"""Record schemas with an explicit trust-visibility firewall.

Two disjoint families:

  * MA-VISIBLE  (visibility="MA" / "PUBLIC")  -> may feed ML features.
  * GROUND TRUTH (visibility="ORACLE")        -> labels/evaluation ONLY, never features.

The `FORBIDDEN_FEATURE_KEYS` registry and `is_forbidden_feature_key()` predicate
are the machine-checkable definition of leakage used by the leakage linter
(datagen.leakage_linter). If a key that identifies a real vehicle, a true kinematic
value, or an attack label ever appears in a feature table, the build fails.

Dataclasses (not pydantic) are used so serialization is trivially deterministic;
pydantic validators can wrap these later without changing the on-disk shape.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any, Optional

# --------------------------------------------------------------------------- #
# Visibility tags
# --------------------------------------------------------------------------- #
MA = "MA"          # received/derived by the Misbehavior Authority
PUBLIC = "PUBLIC"  # e.g. the CRL, visible to everyone
ORACLE = "ORACLE"  # simulation ground truth; no SCMS entity may read this


# --------------------------------------------------------------------------- #
# Leakage registry -- the machine-checkable definition of "ground truth"
# --------------------------------------------------------------------------- #
FORBIDDEN_FEATURE_KEYS = frozenset({
    "true_vehicle_id", "reporter_true_id", "subject_true_id",
    "real_id", "realid", "sender_real_id", "reported_real_id",
    "senderRealId", "reportedRealId",          # the exact F2MD leakage fields
    "true_x", "true_y", "true_speed", "true_heading", "true_accel",
    "is_attacker", "attacker_role", "attack_type", "attack_id",
    "is_faulty", "attack_family", "falsified",  # fault/attack labels (added post-audit)
    "colluding_group_id", "true_linkage_seed", "true_revocation_time",
    "should_have_been_revoked", "report_correctness",
})


def is_forbidden_feature_key(key: str) -> bool:
    """True if `key` names ground truth and must never appear as a feature."""
    # Strip leading underscores first: a leaf column literally named `_is_attacker`
    # must not slip past by hiding behind the visibility-field underscore convention.
    k = key.strip().lstrip("_")
    if k in FORBIDDEN_FEATURE_KEYS:
        return True
    lk = k.lower()
    norm = lk.replace("_", "")   # separator-insensitive: catches camelCase (driverRealId, xTrueId)
    return (
        lk.startswith("true_")
        or lk.startswith("label_")   # any evaluation label
        or lk.startswith("attack_")  # attack_type / attack_family / attack_id / ...
        or lk.endswith("_true_id")
        or lk.endswith("real_id")
        or norm.endswith("realid")   # driverRealId / ownerRealId / sender_real_id / ...
        or norm.endswith("trueid")   # vehicleTrueId / subjectTrueId / ...
        or lk in {"realid", "vehicleid_true"}
    )


# --------------------------------------------------------------------------- #
# MA-visible records
# --------------------------------------------------------------------------- #
@dataclass
class MaEvidenceMessage:
    """A signed CAM/BSM enclosed as report evidence -- claimed values only."""
    msg_ref: str
    subject_cert_digest: str          # HashedId8 hex -- NOT a real identity
    gen_time: float
    claimed_x: float
    claimed_y: float
    claimed_speed: float
    claimed_heading: float
    sig_valid: bool
    cert_status_at_eval: str          # valid|expired|not_yet_valid|revoked|forged
    _visibility: str = MA

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class MaReport:
    """One ingested Misbehaviour Report (TS 103 759-shaped). Digests only."""
    report_id: str
    ingest_time: float
    detection_time: float
    generation_time: float
    reporter_cert_digest: str
    subject_cert_digest: str
    reason_codes: list[str]
    detector_outputs: list[dict[str, Any]]
    cert_validity: dict[str, bool]
    evidence_msg_refs: list[str]
    st_bbox: list[float]              # [xmin, ymin, xmax, ymax]
    st_tstart: float
    st_tend: float
    duplicate_flag: bool = False
    _visibility: str = MA

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class MaCertStatus:
    cert_digest: str
    first_seen: float
    last_seen: float
    valid_from: float
    valid_to: float
    issuing_pca: str
    crl_status: str                   # unknown|active|revoked
    revocation_time: Optional[float] = None
    _visibility: str = MA

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class MaInvestigation:
    """MA-side case state. `identity_resolved` may be True, but the enrollment
    identity itself NEVER reaches the MA -- only an opaque resolution handle."""
    case_id: str
    opened_time: float
    trigger: str
    cluster_size: int
    num_distinct_reporters: int
    linkage_result: str               # same|different|unknown
    identity_resolved: bool
    revocation_decision: str          # pending|revoke|dismiss
    resolution_time: Optional[float] = None
    decision_time: Optional[float] = None
    resolved_case_handle: Optional[str] = None   # opaque; not a real vehicle id
    _visibility: str = MA

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class MaCrlEvent:
    crl_id: str
    issue_time: float
    entry_type: str                   # seed|digest
    num_entries: int
    _visibility: str = PUBLIC

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


# --------------------------------------------------------------------------- #
# Ground-truth records (ORACLE) -- separate files, labels only
# --------------------------------------------------------------------------- #
@dataclass
class GtVehicle:
    true_vehicle_id: str
    spawn_time: float
    is_attacker: bool
    attacker_role: str = "none"
    colluding_group_id: Optional[str] = None
    _visibility: str = ORACLE

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class GtIdentityMap:
    true_vehicle_id: str
    pseudonym_cert_digest: str
    i_period: int
    valid_from: float
    valid_to: float
    _visibility: str = ORACLE

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class GtAttack:
    attack_id: str
    true_vehicle_id: str
    attack_type: str
    start_time: float
    end_time: float
    params: dict[str, Any] = field(default_factory=dict)
    _visibility: str = ORACLE

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class GtReportLabel:
    report_id: str
    reporter_true_id: str
    subject_true_id: str
    report_correctness: str           # correct|false_positive|malicious|duplicate|collusive
    _visibility: str = ORACLE

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass
class GtLinkageRevocation:
    true_vehicle_id: str
    should_have_been_revoked: bool
    true_revocation_time: Optional[float] = None
    _visibility: str = ORACLE

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)
