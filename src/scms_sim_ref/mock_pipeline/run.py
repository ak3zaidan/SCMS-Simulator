"""Closed-loop reference pipeline (pre-MOSAIC).

Runs the entire SCMS misbehaviour lifecycle in pure Python so the architecture,
schemas, trust boundaries, leakage firewall, and reproducibility can be validated
before any MOSAIC/SUMO/Java toolchain exists:

    provision (real linkage values) -> signed CAMs -> live attack -> local
    detection -> misbehaviour reports (digests only) -> MA correlation (ONLINE)
    -> REAL two-LA linkage resolution (MA never learns the true identity) ->
    revocation -> CRL issuance -> enforcement (revoked certs dropped) -> reports stop.

The MA processes reports online, so a detected attacker is revoked mid-run and
enforcement then suppresses its further messages -- the loop genuinely closes.

Outputs under `out_dir`:
    ma/            ma_reports, ma_investigations, ma_crl_events, ma_cert_status  (MA/PUBLIC)
    ground_truth/  gt_vehicle, gt_identity_map, gt_attacks, gt_report_labels,
                   gt_linkage_revocation                                        (ORACLE)
    manifest.json

The mobility/radio layer is a deliberate abstraction; MOSAIC + SUMO replace it
later. Everything downstream of message reception is the real design.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import random
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Optional

from .. import __version__
from ..scms_core import crypto_abstract as ca
from ..scms_core.linkage import CrlLinkageEntry, DeviceLinkageContext, linkage_seed_at
from ..schemas import records as R


# --------------------------------------------------------------------------- #
# Configuration
# --------------------------------------------------------------------------- #
@dataclass
class PipelineConfig:
    seed: int = 1001
    n_vehicles: int = 12
    attacker_ids: tuple[int, ...] = (7,)
    n_steps: int = 40
    dt: float = 1.0
    nominal_speed: float = 15.0          # m/s
    attack_type: str = "ConstPos"        # constant-position falsification
    attack_start: float = 5.0
    attack_end: float = 60.0
    consistency_threshold_m: float = 5.0
    report_prob: float = 0.9
    report_threshold_k: int = 3          # distinct reporters to open an investigation
    net_delay_max: float = 2.0
    crl_propagation_delay: float = 2.0
    jmax: int = 20
    out_dir: str = "datasets/poc_run"

    def derive(self, label: str, n: int = 32) -> bytes:
        return hashlib.sha256(f"{self.seed}|{label}".encode()).digest()[:n]


# --------------------------------------------------------------------------- #
# SCMS back-end entities -- each holds ONLY what its trust boundary permits
# --------------------------------------------------------------------------- #
class LinkageAuthority:
    """LA1 or LA2: holds per-device initial seeds; returns forward seeds only."""

    def __init__(self, which: int):
        self.which = which
        self._seed0: dict[str, bytes] = {}
        self._la_id: dict[str, int] = {}

    def register(self, la_handle: str, ls0: bytes, la_id: int) -> None:
        self._seed0[la_handle] = ls0
        self._la_id[la_handle] = la_id

    def seed_at(self, la_handle: str, i: int) -> tuple[bytes, int]:
        la_id = self._la_id[la_handle]
        return linkage_seed_at(la_id, self._seed0[la_handle], i), la_id


class PseudonymCA:
    """PCA: maps a cert digest to an OPAQUE provisioning record (no identity)."""

    def __init__(self):
        self._prov: dict[str, dict] = {}

    def issue(self, cert_digest, request_hash, i, j, la_handle1, la_handle2) -> None:
        self._prov[cert_digest] = dict(request_hash=request_hash, i=i, j=j,
                                       la_handle1=la_handle1, la_handle2=la_handle2)

    def resolve(self, cert_digest: str) -> Optional[dict]:
        return self._prov.get(cert_digest)


class RegistrationAuthority:
    """RA: the ONLY entity mapping a provisioning request to a true identity."""

    def __init__(self):
        self._request_to_identity: dict[str, str] = {}
        self.blacklist: set[str] = set()
        self.blacklist_events: list[tuple[float, str]] = []   # -> ground truth only

    def bind(self, request_hash: str, true_vehicle_id: str) -> None:
        self._request_to_identity[request_hash] = true_vehicle_id

    def blacklist_request(self, request_hash: str, when: float) -> None:
        true_id = self._request_to_identity[request_hash]     # internal only
        self.blacklist.add(true_id)
        self.blacklist_events.append((when, true_id))


# --------------------------------------------------------------------------- #
# Runtime state
# --------------------------------------------------------------------------- #
@dataclass
class Vehicle:
    vid: int
    spawn_x: float
    lane_y: float
    speed: float
    is_attacker: bool
    priv: object
    pub: bytes
    cert_digest: str
    linkage_ctx: DeviceLinkageContext
    i_period: int
    j_index: int
    request_hash: str

    def true_pos(self, t: float) -> tuple[float, float]:
        return (self.spawn_x + self.speed * t, self.lane_y)


@dataclass
class RunResult:
    out_dir: str
    n_vehicles: int
    n_reports: int
    n_investigations: int
    n_revoked: int
    revoked_cert_digests: list[str]
    data_digest: str
    counts: dict = field(default_factory=dict)


# --------------------------------------------------------------------------- #
# Pipeline
# --------------------------------------------------------------------------- #
def run_pipeline(cfg: PipelineConfig) -> RunResult:
    rng = random.Random(cfg.seed)
    la1, la2 = LinkageAuthority(1), LinkageAuthority(2)
    pca, ra = PseudonymCA(), RegistrationAuthority()

    # ---- Provisioning (real linkage values via two Linkage Authorities) ----
    vehicles: list[Vehicle] = []
    gt_vehicle, gt_idmap = [], []
    for vid in range(cfg.n_vehicles):
        is_att = vid in cfg.attacker_ids
        key = ca.keypair_from_seed(cfg.derive(f"key:{vid}"))
        pub = ca.public_bytes(key)
        cert_digest = ca.hashed_id8(pub).hex()
        la_h1, la_h2 = f"lc1:{vid}", f"lc2:{vid}"
        la_id1, la_id2 = 0x0001, 0x0002
        ctx = DeviceLinkageContext(la_id1, la_id2,
                                   cfg.derive(f"ls1:{vid}", 16), cfg.derive(f"ls2:{vid}", 16))
        la1.register(la_h1, ctx.ls1_0, la_id1)
        la2.register(la_h2, ctx.ls2_0, la_id2)
        i_period, j = 0, vid % cfg.jmax
        req_hash = hashlib.sha256(f"req|{cfg.seed}|{vid}".encode()).hexdigest()[:16]
        pca.issue(cert_digest, req_hash, i_period, j, la_h1, la_h2)
        true_id = f"veh_{vid:03d}"
        ra.bind(req_hash, true_id)
        vehicles.append(Vehicle(vid, float(vid * 20), float(vid % 3) * 4.0, cfg.nominal_speed,
                                is_att, key, pub, cert_digest, ctx, i_period, j, req_hash))
        gt_vehicle.append(R.GtVehicle(true_vehicle_id=true_id, spawn_time=0.0, is_attacker=is_att,
                                      attacker_role=(cfg.attack_type if is_att else "none")))
        gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=cert_digest,
                                        i_period=i_period, valid_from=0.0, valid_to=cfg.n_steps * cfg.dt))

    digest_to_vehicle = {v.cert_digest: v for v in vehicles}
    gt_attacks = [R.GtAttack(attack_id=f"atk_{v.vid}", true_vehicle_id=f"veh_{v.vid:03d}",
                             attack_type=cfg.attack_type, start_time=cfg.attack_start,
                             end_time=cfg.attack_end,
                             params={"frozen_pos": list(v.true_pos(cfg.attack_start))})
                  for v in vehicles if v.is_attacker]

    # ---- MA state (updated ONLINE during the loop) ----
    ma_reports: list[R.MaReport] = []
    gt_report_labels: list[R.GtReportLabel] = []
    ma_investigations: list[R.MaInvestigation] = []
    ma_crl_events: list[R.MaCrlEvent] = []
    gt_linkage_rev: list[R.GtLinkageRevocation] = []
    crl_entries: list[CrlLinkageEntry] = []
    reporters_by_subject: dict[str, set[str]] = {}
    revocation_time: dict[str, float] = {}
    revoked_digests: list[str] = []
    last_claimed: dict[tuple[int, str], tuple[float, float, float]] = {}
    cert_first_seen: dict[str, float] = {}
    cert_last_seen: dict[str, float] = {}
    counters = {"report": 0, "case": 0, "crl": 0}

    def claimed_state(v: Vehicle, t: float) -> tuple[float, float, float]:
        if v.is_attacker and cfg.attack_start <= t <= cfg.attack_end and cfg.attack_type == "ConstPos":
            fx, fy = v.true_pos(cfg.attack_start)          # freeze position, keep claiming speed
            return fx, fy, v.speed
        tx, ty = v.true_pos(t)
        return tx, ty, v.speed

    def resolve_and_revoke(subject_digest: str, t: float) -> None:
        """MA investigation: real two-LA resolution + revocation. MA never learns identity."""
        counters["case"] += 1
        counters["crl"] += 1
        case_id = f"case_{counters['case']:04d}"
        prov = pca.resolve(subject_digest)                          # opaque record, no identity
        ls1_i, la_id1 = la1.seed_at(prov["la_handle1"], prov["i"])  # forward-only seeds
        ls2_i, la_id2 = la2.seed_at(prov["la_handle2"], prov["i"])
        ra.blacklist_request(prov["request_hash"], t)              # identity stays inside the RA
        crl_entries.append(CrlLinkageEntry(i=prov["i"], la_id1=la_id1, la_id2=la_id2,
                                           ls1_i=ls1_i, ls2_i=ls2_i, jmax=cfg.jmax))
        revocation_time[subject_digest] = t
        revoked_digests.append(subject_digest)
        reporters = reporters_by_subject.get(subject_digest, set())
        ma_investigations.append(R.MaInvestigation(
            case_id=case_id, opened_time=t, trigger="report_threshold",
            cluster_size=len(reporters), num_distinct_reporters=len(reporters),
            linkage_result="same", identity_resolved=True, revocation_decision="revoke",
            resolution_time=t, decision_time=t,
            resolved_case_handle=hashlib.sha256(case_id.encode()).hexdigest()[:12]))
        ma_crl_events.append(R.MaCrlEvent(crl_id=f"crl_{counters['crl']:04d}", issue_time=t,
                                          entry_type="seed", num_entries=len(crl_entries)))
        subj = digest_to_vehicle[subject_digest]
        gt_linkage_rev.append(R.GtLinkageRevocation(true_vehicle_id=f"veh_{subj.vid:03d}",
                                                    should_have_been_revoked=subj.is_attacker,
                                                    true_revocation_time=t))

    # ---- Simulation loop: broadcast -> receive -> detect -> report -> (online) revoke ----
    for step in range(cfg.n_steps):
        t = step * cfg.dt
        touched_subjects: set[str] = set()
        for tx in vehicles:
            cert_first_seen.setdefault(tx.cert_digest, t)
            cert_last_seen[tx.cert_digest] = t
            cx, cy, cs = claimed_state(tx, t)
            for rx in vehicles:
                if rx.vid == tx.vid:
                    continue
                rt = revocation_time.get(tx.cert_digest)
                if rt is not None and t >= rt + cfg.crl_propagation_delay:
                    continue                                        # ENFORCEMENT: drop revoked cert
                key = (rx.vid, tx.cert_digest)
                prev = last_claimed.get(key)
                last_claimed[key] = (cx, cy, t)
                if prev is None:
                    continue
                px, py, pt = prev
                inconsistency = abs(math.hypot(cx - px, cy - py) - cs * (t - pt))
                if inconsistency <= cfg.consistency_threshold_m:
                    continue
                if rng.random() > cfg.report_prob:                  # suppression / loss
                    continue
                counters["report"] += 1
                rid = f"rpt_{counters['report']:05d}"
                delay = rng.uniform(0.0, cfg.net_delay_max)
                ma_reports.append(R.MaReport(
                    report_id=rid, ingest_time=t + delay, detection_time=t, generation_time=t,
                    reporter_cert_digest=rx.cert_digest, subject_cert_digest=tx.cert_digest,
                    reason_codes=["positionSpeedConsistency"],
                    detector_outputs=[{"check_id": "positionSpeedConsistency",
                                        "score": round(inconsistency, 3), "verdict": "fail"}],
                    cert_validity={"sig_valid": True, "not_expired": True,
                                   "not_revoked": True, "chain_ok": True},
                    evidence_msg_refs=[f"{rid}-m"],
                    st_bbox=[min(cx, px), min(cy, py), max(cx, px), max(cy, py)],
                    st_tstart=pt, st_tend=t, duplicate_flag=False))
                subj = digest_to_vehicle[tx.cert_digest]
                gt_report_labels.append(R.GtReportLabel(
                    report_id=rid, reporter_true_id=f"veh_{rx.vid:03d}",
                    subject_true_id=f"veh_{tx.vid:03d}",
                    report_correctness=("correct" if subj.is_attacker else "false_positive")))
                reporters_by_subject.setdefault(tx.cert_digest, set()).add(rx.cert_digest)
                touched_subjects.add(tx.cert_digest)
        # Online MA decision at end of step: revoke subjects that crossed the threshold.
        for subject_digest in sorted(touched_subjects):
            if subject_digest in revocation_time:
                continue
            if len(reporters_by_subject[subject_digest]) >= cfg.report_threshold_k:
                resolve_and_revoke(subject_digest, t)

    # ---- Real-linkage sanity: every CRL entry must actually revoke its target ----
    for d in revoked_digests:
        v = digest_to_vehicle[d]
        assert any(e.matches(v.i_period, v.j_index,
                             v.linkage_ctx.linkage_value_for(v.i_period, v.j_index))
                   for e in crl_entries), "CRL entry failed to revoke its target device"

    # ---- Certificate status (MA-visible) ----
    ma_cert_status = [R.MaCertStatus(
        cert_digest=d, first_seen=f, last_seen=cert_last_seen[d], valid_from=0.0,
        valid_to=cfg.n_steps * cfg.dt, issuing_pca="PCA-1",
        crl_status=("revoked" if d in revocation_time else "active"),
        revocation_time=revocation_time.get(d)) for d, f in cert_first_seen.items()]

    # ---- Write outputs + manifest ----
    data_files = _write_outputs(cfg, ma_reports, ma_investigations, ma_crl_events, ma_cert_status,
                                gt_vehicle, gt_idmap, gt_attacks, gt_report_labels, gt_linkage_rev)
    data_digest = _data_digest(cfg.out_dir, data_files)
    _write_manifest(cfg, data_files, data_digest,
                    counts=dict(vehicles=cfg.n_vehicles, reports=len(ma_reports),
                                investigations=len(ma_investigations), revoked=len(revoked_digests)))

    return RunResult(out_dir=cfg.out_dir, n_vehicles=cfg.n_vehicles, n_reports=len(ma_reports),
                     n_investigations=len(ma_investigations), n_revoked=len(revoked_digests),
                     revoked_cert_digests=sorted(revoked_digests), data_digest=data_digest,
                     counts=dict(cert_status=len(ma_cert_status), gt_reports=len(gt_report_labels)))


# --------------------------------------------------------------------------- #
# Output helpers (deterministic)
# --------------------------------------------------------------------------- #
def _write_jsonl(path: str, rows: list) -> str:
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        for r in rows:
            payload = r.to_dict() if hasattr(r, "to_dict") else r
            fh.write(ca.canonical_bytes(payload).decode("utf-8") + "\n")
    return path


def _write_outputs(cfg, ma_reports, ma_invest, ma_crl, ma_cert, gt_veh, gt_idmap,
                   gt_attacks, gt_report_labels, gt_linkage_rev) -> dict[str, str]:
    os.makedirs(os.path.join(cfg.out_dir, "ma"), exist_ok=True)
    os.makedirs(os.path.join(cfg.out_dir, "ground_truth"), exist_ok=True)
    files = {
        "ma/ma_reports.jsonl": sorted(ma_reports, key=lambda r: (r.ingest_time, r.report_id)),
        "ma/ma_investigations.jsonl": sorted(ma_invest, key=lambda r: (r.opened_time, r.case_id)),
        "ma/ma_crl_events.jsonl": sorted(ma_crl, key=lambda r: (r.issue_time, r.crl_id)),
        "ma/ma_cert_status.jsonl": sorted(ma_cert, key=lambda r: r.cert_digest),
        "ground_truth/gt_vehicle.jsonl": sorted(gt_veh, key=lambda r: r.true_vehicle_id),
        "ground_truth/gt_identity_map.jsonl": sorted(gt_idmap, key=lambda r: r.pseudonym_cert_digest),
        "ground_truth/gt_attacks.jsonl": sorted(gt_attacks, key=lambda r: r.attack_id),
        "ground_truth/gt_report_labels.jsonl": sorted(gt_report_labels, key=lambda r: r.report_id),
        "ground_truth/gt_linkage_revocation.jsonl": sorted(gt_linkage_rev, key=lambda r: r.true_vehicle_id),
    }
    return {rel: _write_jsonl(os.path.join(cfg.out_dir, rel), rows) for rel, rows in files.items()}


def _file_sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        h.update(fh.read())
    return h.hexdigest()


def _data_digest(out_dir: str, data_files: dict[str, str]) -> str:
    """Single digest over all DATA files (manifest excluded -> determinism-safe)."""
    h = hashlib.sha256()
    for rel in sorted(data_files):
        h.update(rel.encode())
        h.update(_file_sha256(data_files[rel]).encode())
    return h.hexdigest()


def _write_manifest(cfg, data_files, data_digest, counts) -> None:
    manifest = {
        "dataset_version": __version__,
        "build_utc": datetime.now(timezone.utc).isoformat(),   # NOT part of data_digest
        "generator": "scms_sim_ref.mock_pipeline (pre-MOSAIC reference)",
        "seed": cfg.seed,
        "config": {k: (list(v) if isinstance(v, tuple) else v) for k, v in cfg.__dict__.items()},
        "schema_versions": {"ma_visible": 1, "ground_truth": 1},
        "standards_profile": {"report": "ETSI TS 103 759 (shape)", "cert": "IEEE 1609.2",
                              "linkage": "CAMP SCP2"},
        "data_digest_sha256": data_digest,
        "outputs": [{"path": rel, "sha256": _file_sha256(p)} for rel, p in sorted(data_files.items())],
        "counts": counts,
    }
    with open(os.path.join(cfg.out_dir, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(manifest, fh, indent=2, sort_keys=True)
        fh.write("\n")


def main(argv: Optional[list[str]] = None) -> int:
    import argparse
    p = argparse.ArgumentParser(description="Run the SCMS closed-loop reference pipeline.")
    p.add_argument("--seed", type=int, default=1001)
    p.add_argument("--vehicles", type=int, default=12)
    p.add_argument("--steps", type=int, default=40)
    p.add_argument("--out", default="datasets/poc_run")
    args = p.parse_args(argv)
    res = run_pipeline(PipelineConfig(seed=args.seed, n_vehicles=args.vehicles,
                                      n_steps=args.steps, out_dir=args.out))
    print(f"vehicles={res.n_vehicles} reports={res.n_reports} "
          f"investigations={res.n_investigations} revoked={res.n_revoked}")
    print(f"revoked_cert_digests={res.revoked_cert_digests}")
    print(f"data_digest={res.data_digest}")
    print(f"outputs in {os.path.abspath(res.out_dir)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
