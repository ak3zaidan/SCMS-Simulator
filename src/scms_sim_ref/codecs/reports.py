"""`ma_report_v1` -- today's misbehaviour-report row, expressed THROUGH the `report_format` seam.

The slot had been registered and EMPTY since the plugin architecture landed. This is the built-in
that fills it, and it is deliberately not an improvement on the historic row: it is the historic row,
field for field, rounding for rounding, conditional column for conditional column. That is what
makes it a proof rather than a claim -- `tests/test_protocol_profile.py` runs the engine with
`--report-format ma_report_v1` and asserts `data_digest` is the pinned default golden
`0bd93655...`, so "the built-in reproduces the format the engine has always written" is MEASURED.
A format that improved anything could not be graded that way, and a seam whose built-in cannot be
graded against the artifact it replaces is a rewrite wearing a plugin's clothes.

What it honestly is, and is not: see :class:`~scms_sim_ref.schemas.records.MaReport`. Field-name
correspondence with ETSI TS 103 759 V2.2.1 `TemplateAsr` only. `v2xPduEvidence` is
`SEQUENCE (SIZE(1..MAX)) OF V2xPduStream` -- mandatory, minimum one -- and `evidence_msg_refs` here
is a synthetic self-reference, not the observed PDUs. The standard has no score, confidence or
severity field anywhere, so `detector_outputs[{check_id, score, verdict}]` and every `detnorm_*`
column are a private extension.

:class:`~scms_sim_ref.api.report.ReportInput` now carries `evidence_pdus` -- the real octets
`MessageCodec.evidence_pdu()` produced -- so a format that wants a structurally valid
`v2xPduEvidence` has the bytes for it. This one does not use them, because using them would change
the default digest. :class:`Ts103759ShapeFormat` below does, and is what a deployment that needs the
real shape selects instead.
"""
from __future__ import annotations

from ..api.report import (CAP_DETECTOR_SCORES, CAP_EVIDENCE_PDU, CAP_RSSI, CAP_STATION_TYPE,
                          CAP_TS103759_SHAPE, INTERFACE_VERSION, ReportFormatBase, ReportInput)
from ..schemas.records import MaReport

#: Rounding, per field, exactly as `run.py::file_report` has always applied it. A tuple rather than
#: magic numbers at four call sites, so the one thing a reader most wants to check is checkable.
ROUND_INGEST = 3
ROUND_DETECTION = 6
ROUND_SCORE = 3


class MaReportV1Format(ReportFormatBase):
    """The engine's historic row. Default-equivalent, and graded against the pinned golden."""

    interface_version = INTERFACE_VERSION
    plugin_id = "ma_report_v1"
    format_id = "ma_report_v1"

    def __init__(self, *, params=None, rng=None, env=None):
        p = dict(params or {})
        unknown = sorted(p)
        if unknown:
            raise ValueError(f"ma_report_v1 does not accept params {unknown}")
        self.params = p

    @classmethod
    def from_plugin(cls, *, params=None, rng=None, env=None):
        return cls(params=params, rng=rng, env=env)

    def capabilities(self) -> frozenset:
        return frozenset({CAP_DETECTOR_SCORES, CAP_RSSI, CAP_STATION_TYPE})

    def standards_claim(self) -> dict:
        return {
            "format": "ma_report_v1 -- engine-private misbehaviour report row",
            "encoding": "canonical JSON (sorted keys, compact separators), one object per line",
            "inspired_by": "ETSI TS 103 759 V2.2.1 TemplateAsr",
            "conformant_to": None,
            "deviations": [
                "v2xPduEvidence (SEQUENCE (SIZE(1..MAX)) OF V2xPduStream, mandatory) is replaced by "
                "a synthetic self-reference in evidence_msg_refs; no PDU octets are carried",
                "detector_outputs / detector_score / detector_score_norm / detnorm_* are a private "
                "extension -- TS 103 759 has no score, confidence or severity field",
                "reason codes are F2MD detector names, not the normative (tgtId, obsId) pairs",
            ],
        }

    def render(self, report: ReportInput) -> dict:
        r = report
        row = MaReport(
            report_id=r.report_id,
            ingest_time=round(r.ingest_time, ROUND_INGEST),
            # NOT rounded on the legacy path: the engine writes `t` verbatim there, and
            # `round(t, 6)` is a different float for a dt that does not divide 1.0 exactly
            # (t = 0.30000000000000004 rounds to 0.3), which would move the digest of any
            # sub-second default run. The engine therefore hands this field ALREADY in the form it
            # wants and the format rounds only when a latency model made it a computed quantity --
            # which is what `round_detection` on the input expresses.
            detection_time=(round(r.detection_time, ROUND_DETECTION) if r.round_detection
                            else r.detection_time),
            generation_time=r.generation_time,
            reporter_cert_digest=r.reporter_cert_digest,
            subject_cert_digest=r.subject_cert_digest,
            reason_codes=list(r.reason_codes),
            detector_outputs=[{"check_id": r.reason_codes[0],
                               "score": round(r.score, ROUND_SCORE), "verdict": "fail"}],
            cert_validity=dict(r.cert_validity),
            evidence_msg_refs=list(r.evidence_msg_refs),
            st_bbox=list(r.st_bbox),
            st_tstart=r.st_tstart, st_tend=r.st_tend,
            duplicate_flag=bool(r.duplicate_flag)).to_dict()
        row["detector_score"] = round(r.score, ROUND_SCORE)
        row["detector_score_norm"] = round(r.score_norm, ROUND_SCORE)
        row["subject_pos_confidence"] = round(r.subject_pos_confidence, ROUND_SCORE)
        row["cert_crl_status"] = r.cert_crl_status
        row["sig_valid"] = bool(r.sig_valid)
        if r.station_type is not None:
            row["station_type"] = r.station_type
        if r.emit_rssi:
            row["rssi_dbm"] = None if r.rssi_dbm is None else round(float(r.rssi_dbm), 2)
        scores = r.detector_scores or {}
        for k in r.detector_keys:
            row[f"detnorm_{k}"] = round(scores.get(k, 0.0), ROUND_SCORE)
        return row


class Ts103759ShapeFormat(MaReportV1Format):
    """The same content in the TS 103 759 `TemplateAsr` SHAPE, with REAL `v2xPduEvidence`.

    Three fields -- `observations`, `v2xPduEvidence`, `nonV2xPduEvidence` -- and the second one
    carries the octets the codec actually put on the air, hex-encoded, one entry per observed PDU.
    That closes the structural gap `MaReport`'s own docstring names: *"a report that does not carry
    the observed PDUs is structurally not a TS 103 759 report under any encoding"*.

    It is still NOT conformant and says so: the shape is right, the ENCODING is JSON rather than
    COER, the observations carry this engine's private score vector where the standard's are mostly
    `::= NULL`, and the reason codes are F2MD names rather than the normative `(tgtId, obsId)`
    pairs. Shipping it is what makes the difference between the two formats VISIBLE in a dataset
    instead of arguable in a document -- and it is the reason `evidence_pdus` exists on the input.

    Selecting it CHANGES `data_digest` by construction, which is correct: a different report format
    is a different dataset, and the manifest records which one produced it.
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "ts103759_shape"
    format_id = "ts103759_shape"

    def capabilities(self) -> frozenset:
        return frozenset({CAP_DETECTOR_SCORES, CAP_EVIDENCE_PDU, CAP_RSSI, CAP_STATION_TYPE,
                          CAP_TS103759_SHAPE})

    def standards_claim(self) -> dict:
        return {
            "format": "ts103759_shape -- ETSI TS 103 759 V2.2.1 TemplateAsr SHAPE, JSON encoding",
            "encoding": "canonical JSON; v2xPduEvidence entries are lowercase hex of the real "
                        "encoded PDUs from MessageCodec.evidence_pdu()",
            "inspired_by": "ETSI TS 103 759 V2.2.1 TemplateAsr",
            "conformant_to": None,
            "deviations": [
                "encoding is JSON, not COER -- TS 103 097 COER is not implemented in this repository",
                "observations carry a private score vector; the standard's are mostly ::= NULL",
                "reason codes are F2MD detector names, not the normative (tgtId, obsId) pairs",
            ],
        }

    def render(self, report: ReportInput) -> dict:
        row = super().render(report)
        # The three TemplateAsr fields, alongside the flat row the rest of this pipeline reads.
        # Keeping both is deliberate: `REQUIRED_ROW_KEYS` must survive, and a dataset that is
        # readable by the engine's own tooling AND carries the standard's structure is strictly
        # more useful than one that is only the second.
        row["observations"] = [{"reason_code": rc,
                                "score": round((report.detector_scores or {}).get(rc, 0.0),
                                               ROUND_SCORE)}
                               for rc in report.reason_codes]
        row["v2xPduEvidence"] = [{"pdu_hex": bytes(p).hex(),
                                  "octets": len(bytes(p)),
                                  "profile_id": report.evidence_profile_id}
                                 for p in report.evidence_pdus]
        row["nonV2xPduEvidence"] = []
        # `SEQUENCE (SIZE(1..MAX))` is MANDATORY, MINIMUM ONE. Say so in the row rather than
        # emitting an empty list that looks like a valid encoding of nothing.
        row["v2xPduEvidence_present"] = bool(report.evidence_pdus)
        return row


__all__ = ["MaReportV1Format", "ROUND_DETECTION", "ROUND_INGEST", "ROUND_SCORE",
           "Ts103759ShapeFormat"]
