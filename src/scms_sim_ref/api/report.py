"""`ReportFormat` -- the misbehaviour-report seam, and the last EMPTY slot in the registry.

`report_format` has been a registered slot with no interface, no built-in and no consumer since the
plugin architecture landed. That is a real gap rather than a cosmetic one: **a misbehaviour report's
format is part of the protocol a deployment speaks.** Two deployments can agree on ITS-G5 on the air
and still disagree completely on what a report to the Misbehaviour Authority looks like -- ETSI
TS 103 759 `TemplateAsr`, IEEE 1609.2.1's own reporting, or (as here) a private JSON row. A stack
that is swappable on the air and hard-coded on the backhaul is only half swappable.

WHAT THE BUILT-IN IS, AND WHAT IT IS NOT
----------------------------------------
`codecs.reports.MaReportV1Format` is today's row, expressed THROUGH this seam rather than beside it:
the same fields, the same rounding, the same conditional columns. It is graded by a test that runs
the engine with `--report-format ma_report_v1` and asserts `data_digest` is the pinned default
golden, so "the built-in reproduces the historic format" is a measured property and not a claim.

It is **not TS 103 759 conformant**, and :meth:`ReportFormat.standards_claim` on it says so in as
many words. The real report is `TemplateAsr{observations, v2xPduEvidence, nonV2xPduEvidence}` with
`v2xPduEvidence ::= SEQUENCE (SIZE(1..MAX)) OF V2xPduStream` -- MANDATORY, minimum one -- and no
score, confidence or severity field anywhere. This engine's `detector_outputs[{check_id, score,
verdict}]` is a private extension and its reason codes are F2MD names, not the normative
`(tgtId, obsId)` pairs.

:class:`ReportInput` DOES carry `evidence_pdus`: the real octets
:meth:`~scms_sim_ref.api.codec.MessageCodec.evidence_pdu` produced for the messages the report is
about. So a third-party format that wants to emit a structurally valid `v2xPduEvidence` now has the
bytes to put in it; the built-in keeps the historic synthetic `[f"{rid}-m"]` reference, because
changing it would change the default digest. The plumbing exists; using it is the format's choice.

THE FIREWALL APPLIES HERE, AND HERE IT IS THE SHARPEST IT GETS
--------------------------------------------------------------
A report row is a FILE THE DATASET SHIPS. Anything a format writes into it is available to every
downstream consumer -- including, in a supervised experiment, to the model being trained. So
:class:`ReportInput` is built to the same rule as
:class:`~scms_sim_ref.api.detect.Observation`: **MA-visible fields only.** It carries what the
reporting station observed and what its detectors concluded; it never carries `veh`, the subject's
TRUE position, `falsified`, `is_attacker`, `attack_type` or the report's own ground-truth
correctness label (which the engine writes to `ground_truth/gt_report_labels.jsonl`, a different
file, under the withholding rules). A format that could see those would be a laundering vector
straight into the training set, and it would be undetectable by every digest in the project because
the digest would simply be the digest of the leaked data.
``R2_row_carries_no_oracle_field`` asserts it by name against `FORBIDDEN_FEATURE_KEYS`.

TRUST MODEL
-----------
Identical in kind to :mod:`scms_sim_ref.api.profile`'s, with one addition specific to this slot.
ENFORCED: interface version, method signatures, capability screening, name-hijack refusal,
content-addressed provenance, and capability by omission (a format gets a `ReportInput` and nothing
else -- no config, no vehicle list, no oracle). CONVENTION, graded by ``R1``-``R5`` and by the
pinned goldens: determinism, purity, JSON-serialisability, no oracle keys, and emission of
:data:`REQUIRED_ROW_KEYS`. NOT ENFORCEABLE in-process: import-time code, native nondeterminism, and
reading the oracle files off disk -- a plugin is an ordinary process with ordinary read access to
`out_dir`, which is measured rather than hypothesised in
`docs/realism/ISOLATION-ORACLE-LEAK.md`. For code you do not trust the answer is what it is on every
other slot: run it **out of process**, where the address space it can reach does not contain the
answer key. This slot does not have that mode yet, so a report format is *attested and
content-addressed, not sandboxed*, and this docstring says so rather than letting a reader infer
otherwise from the length of the ENFORCED list.

The addition: this slot's output is DIGEST-BEARING. `ma/ma_reports.jsonl` is inside `data_digest`,
so a nondeterministic format does not merely produce a bad file, it makes the run unreplayable --
which is why ``R1`` runs the format twice over the same input and compares canonical bytes, and why
the engine's default is no format object at all.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Mapping, Optional, Protocol, runtime_checkable

INTERFACE_NAME = "ReportFormat"
INTERFACE_VERSION = "ReportFormat/1.0"

#: Highest interface MINOR this engine understands.
MAX_MINOR = 0

# --------------------------------------------------------------------------- #
# Capabilities
# --------------------------------------------------------------------------- #
#: Emits the per-check score vector (`detnorm_*`) -- a PRIVATE extension; TS 103 759 has no score.
CAP_DETECTOR_SCORES = "detector_scores"
#: Emits the encoded octets of the messages the report is about, not merely a reference to them.
CAP_EVIDENCE_PDU = "evidence_pdu"
#: Claims the TS 103 759 `TemplateAsr` SHAPE (three fields, `v2xPduEvidence` non-empty).
CAP_TS103759_SHAPE = "ts103759_shape"
#: Emits the receiver's measured RSSI for the subject's frame.
CAP_RSSI = "rssi"
#: Emits the subject's self-declared station type.
CAP_STATION_TYPE = "station_type"

KNOWN_CAPABILITIES = frozenset({
    CAP_DETECTOR_SCORES, CAP_EVIDENCE_PDU, CAP_RSSI, CAP_STATION_TYPE, CAP_TS103759_SHAPE,
})

#: Nothing is reserved: the slot has no default object, so no format is grandfathered against a
#: pinned digest the way `legacy_global_rng` grandfathers a built-in channel model.
RESERVED_CAPABILITIES: frozenset = frozenset()

#: Keys the ENGINE ITSELF reads back off a rendered row. `_write_outputs` sorts on
#: `(ingest_time, report_id)` and the flow path streams in that order, so a format that omits them
#: does not produce an unusual dataset, it produces a `KeyError` at write time. Graded at load by
#: ``R3`` rather than discovered at the end of an hour-long run.
REQUIRED_ROW_KEYS = ("ingest_time", "report_id", "reporter_cert_digest", "subject_cert_digest")


# --------------------------------------------------------------------------- #
# The input
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class ReportInput:
    """ONE misbehaviour report, in engine units, MA-VISIBLE ONLY. See the module docstring.

    Values are UNROUNDED. Rounding is a property of a FORMAT -- today's row rounds `ingest_time` to
    3 decimals, `detection_time` to 6, scores to 3 -- and a seam that pre-rounded would be handing
    every format the built-in's choices and calling them the engine's.
    """

    report_id: str
    ingest_time: float                    #: when the MA received it (engine seconds)
    detection_time: float                 #: when the reporter DETECTED (engine seconds)
    generation_time: float                #: when the reporter GENERATED the report
    reporter_cert_digest: str             #: HashedId8 hex -- NOT rotation-stable
    subject_cert_digest: str
    reason_codes: tuple                   #: the reasons that fired, most-violating first
    #: Whether `detection_time` is a COMPUTED quantity (a latency model produced it) or the step
    #: time itself. It decides whether rounding it is safe: `round(t, 6)` is a different float from
    #: `t` for a dt that does not divide 1.0 exactly (t = 0.30000000000000004 rounds to 0.3), so a
    #: format that rounded unconditionally would move the digest of every sub-second run that has no
    #: latency model at all. The engine states which case it is in; the format decides what to do.
    round_detection: bool = False
    #: check id -> normalised score, the FULL vector this run computes. `>= 1.0` is violating.
    detector_scores: Mapping = field(default_factory=dict)
    #: The ordered column vocabulary. Order is load-bearing: it fixes JSON key insertion order in
    #: the historic format and therefore reaches `data_digest`.
    detector_keys: tuple = ()
    score: float = 1.0                    #: the fusion's score for the firing reason
    score_norm: float = 1.0               #: the fusion's normalised score
    subject_pos_confidence: float = 0.0   #: the subject's own claimed 95 % radius, metres
    sig_valid: bool = True                #: what THIS receiver concluded about the signature
    cert_crl_status: str = "active"
    #: The subject's SELF-DECLARED station type, or None when the engine is not emitting the column.
    station_type: Optional[str] = None
    #: Received signal strength of the subject's frame, dBm. Legitimately measurable by the receiver
    #: PHY, so MA-visible -- but computed from the subject's TRUE position, which is exactly what
    #: makes RSSI-versus-claimed-distance a detector. `None` is a legitimate emitted value, so
    #: whether the column exists at all is :attr:`emit_rssi`, never `rssi_dbm is None`.
    rssi_dbm: Optional[float] = None
    emit_rssi: bool = False
    st_bbox: tuple = (0.0, 0.0, 0.0, 0.0)   #: [xmin, ymin, xmax, ymax] over reporter and subject
    st_tstart: float = 0.0
    st_tend: float = 0.0
    duplicate_flag: bool = False
    #: References to the evidence messages, as the historic format spells them.
    evidence_msg_refs: tuple = ()
    #: The ENCODED OCTETS of those messages, from `MessageCodec.evidence_pdu`. Empty when no codec
    #: is active. This is what a TS 103 759 `v2xPduEvidence` entry actually requires.
    evidence_pdus: tuple = ()
    #: The codec profile those octets came from, so a consumer knows which decoder to reach for.
    evidence_profile_id: Optional[str] = None
    #: What the reporter concluded about the subject's certificate.
    cert_validity: Mapping = field(default_factory=dict)


@runtime_checkable
class ReportFormat(Protocol):
    """Three methods. `render` is the whole seam; the other two are declarations."""

    interface_version: str
    plugin_id: str
    format_id: str             #: "ma_report_v1" | ...

    def capabilities(self) -> frozenset: ...

    def standards_claim(self) -> Mapping:
        """What this format may honestly assert. Same rule as the codec and profile slots: name the
        standard AND the clause when you implement one, and never claim conformance testing."""

    def render(self, report: "ReportInput") -> Mapping:
        """The row that goes into `ma/ma_reports.jsonl`.

        MUST be a JSON-serialisable mapping, MUST be a pure function of `report`, and MUST carry
        :data:`REQUIRED_ROW_KEYS`. It lands inside `data_digest`, so it must also be deterministic
        across processes -- no `id()`, no `set` iteration order, no wall clock.
        """


#: The load-time signature contract (`registry._check_signature`).
REPORT_SPEC = {
    "capabilities": (),
    "standards_claim": (),
    "render": ("report",),
}


class ReportFormatBase:
    """Optional convenience base. Inheriting is never required."""

    interface_version = INTERFACE_VERSION
    plugin_id = "report_format"
    format_id = "abstract"

    def capabilities(self) -> frozenset:
        return frozenset()

    def standards_claim(self) -> Mapping:
        return {}

    def render(self, report: "ReportInput") -> Mapping:
        raise NotImplementedError


__all__ = [
    "CAP_DETECTOR_SCORES", "CAP_EVIDENCE_PDU", "CAP_RSSI", "CAP_STATION_TYPE",
    "CAP_TS103759_SHAPE", "INTERFACE_NAME", "INTERFACE_VERSION", "KNOWN_CAPABILITIES", "MAX_MINOR",
    "REPORT_SPEC", "REQUIRED_ROW_KEYS", "RESERVED_CAPABILITIES", "ReportFormat",
    "ReportFormatBase", "ReportInput",
]
