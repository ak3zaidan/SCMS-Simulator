"""Closed-loop reference pipeline (pre-MOSAIC), now with realistic, diverse data.

Runs the entire SCMS misbehaviour lifecycle in pure Python so the architecture,
schemas, trust boundaries, leakage firewall, and reproducibility can be validated
without any MOSAIC/SUMO/Java toolchain:

    provision (real linkage values) -> signed CAMs over a MODELLED GNSS sensor ->
    live attacks (13-type catalog across 7 families) -> receiver multi-detector
    fusion -> misbehaviour reports (digests only) -> MA correlation (ONLINE) ->
    REAL two-LA linkage resolution (MA never learns the true identity) ->
    persistence-gated revocation -> CRL issuance -> enforcement -> reports stop.

Realism the earlier reference lacked (the "precision was an unrealistic 1.0
because benign data was perfect truth" problem):
  * a per-vehicle GNSS error model (OU-correlated bias + white noise + rare
    multipath outliers, scaled by heterogeneous per-vehicle quality and weather),
    so honest vehicles broadcast MEASURED state and generate realistic false
    positives that a persistence gate must survive;
  * 2-D mobility with heading, per-vehicle direction/speed and gentle lane wander;
  * a diverse attack catalog (position/speed/heading/stealth/timing families),
    assigned per attacker, including stealthy variants that deliberately evade;
  * a faulty (malfunctioning-sensor) class distinct from attackers;
  * a receiver multi-detector fingerprint (detnorm_* per report) for fusion models;
  * per-message ground-truth emission samples for the calibration scorecard.

Everything downstream of message reception is the real design. Deterministic:
same seed+config -> byte-identical data.
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

# Attack catalog spanning the 7 families the featurizer knows (position/speed/heading/
# combined/timing/stealth). Assigned round-robin to attackers; index 0 (ConstPos) is a
# strong, always-detectable attack so a single-attacker run always closes the loop.
ATTACK_CATALOG = (
    "ConstPos", "ConstPosOffset", "RandomPos", "Teleport", "SineWavePos",
    "ConstSpeedOffset", "RandomSpeed", "StopAndGo",
    "ReversedHeading", "HeadingOffset", "DataReplay",
    "SlowDrift", "AlongRoadOffset",
)
WEATHER_MULT = {"clear": 1.0, "rain": 1.5, "fog": 2.0, "snow": 2.5}


# --------------------------------------------------------------------------- #
# Configuration
# --------------------------------------------------------------------------- #
@dataclass
class PipelineConfig:
    seed: int = 1001
    n_vehicles: int = 12
    attacker_ids: tuple[int, ...] = (7,)
    attacker_pct: float = 0.0            # >0 overrides attacker_ids (fraction of the fleet)
    n_steps: int = 40
    dt: float = 1.0
    nominal_speed: float = 15.0          # m/s
    attack_type: str = "ConstPos"        # default single-attacker type (index-0 fallback)
    attack_types: tuple[str, ...] = ATTACK_CATALOG
    attack_start: float = 5.0
    attack_end: float = 60.0
    # --- GNSS / sensor realism ---
    gps_sigma_m: float = 1.2             # white per-axis noise (× per-vehicle quality × weather)
    gps_bias_sigma_m: float = 1.5        # OU-correlated slow bias amplitude
    gps_bias_tau_s: float = 20.0         # bias correlation time
    gps_outlier_rate: float = 0.01       # per-message multipath outlier probability
    gps_outlier_mag_m: float = 12.0
    faulty_pct: float = 0.05             # malfunctioning-sensor (non-attacker) fraction
    faulty_bias_mult: float = 5.0        # faulty = large SUSTAINED bias (smooth, self-consistent)
    weather: str = "clear"
    # --- detection / revocation ---
    consistency_threshold_m: float = 5.0
    pos_jump_max_m: float = 45.0
    heading_threshold_deg: float = 35.0
    report_prob: float = 0.9
    report_threshold_k: int = 3          # distinct reporters to open an investigation
    revoke_min_seconds: int = 4          # AND reports in >= this many distinct seconds
    revoke_persist_s: float = 3.0        # AND spanning >= this long (blunts transient benign FPs)
    net_delay_max: float = 2.0
    crl_propagation_delay: float = 2.0
    emit_sample_prob: float = 0.03       # per-message ground-truth emission sampling
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
    # mobility
    direction: float = 0.0               # radians
    wander_amp: float = 0.0
    wander_w: float = 0.0
    phase: float = 0.0
    # sensor / class
    gps_q: float = 1.0                   # per-vehicle GNSS quality multiplier
    is_faulty: bool = False
    attack_type: str = "none"
    # mutable per-run state
    bias_x: float = 0.0
    bias_y: float = 0.0
    frozen: Optional[tuple[float, float]] = None
    drift: float = 0.0
    hist: list = field(default_factory=list)   # (x,y,speed,heading) claim history for replay
    onset: Optional[float] = None

    def true_state(self, t: float) -> tuple[float, float, float, float]:
        """True (x, y, speed, heading[deg]) at time t: straight heading + gentle lane wander."""
        ux, uy = math.cos(self.direction), math.sin(self.direction)
        nx, ny = -uy, ux
        along = self.speed * t
        lat = self.wander_amp * math.sin(self.wander_w * t + self.phase)
        x = self.spawn_x + along * ux + lat * nx
        y = self.lane_y + along * uy + lat * ny
        vlat = self.wander_amp * self.wander_w * math.cos(self.wander_w * t + self.phase)
        vx, vy = self.speed * ux + vlat * nx, self.speed * uy + vlat * ny
        speed = math.hypot(vx, vy)
        heading = math.degrees(math.atan2(vy, vx)) % 360.0
        return x, y, speed, heading


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


def _ang_diff(a: float, b: float) -> float:
    d = abs((a - b) % 360.0)
    return d if d <= 180.0 else 360.0 - d


# --------------------------------------------------------------------------- #
# Pipeline
# --------------------------------------------------------------------------- #
def run_pipeline(cfg: PipelineConfig) -> RunResult:
    rng = random.Random(cfg.seed)
    wmult = WEATHER_MULT.get(cfg.weather, 1.0)
    la1, la2 = LinkageAuthority(1), LinkageAuthority(2)
    pca, ra = PseudonymCA(), RegistrationAuthority()

    # ---- attacker / faulty assignment (deterministic) ----
    if cfg.attacker_pct > 0:
        arng = random.Random(f"{cfg.seed}:attackers")
        k = max(1, int(round(cfg.n_vehicles * cfg.attacker_pct)))
        attacker_set = set(arng.sample(range(cfg.n_vehicles), min(k, cfg.n_vehicles)))
    else:
        attacker_set = set(cfg.attacker_ids)
    non_attackers = [v for v in range(cfg.n_vehicles) if v not in attacker_set]
    frng = random.Random(f"{cfg.seed}:faulty")
    n_faulty = int(cfg.n_vehicles * cfg.faulty_pct)
    faulty_set = set(frng.sample(non_attackers, min(n_faulty, len(non_attackers)))) if n_faulty else set()
    attackers_sorted = sorted(attacker_set)
    catalog = cfg.attack_types or (cfg.attack_type,)

    # ---- Provisioning (real linkage values via two Linkage Authorities) ----
    vehicles: list[Vehicle] = []
    gt_vehicle, gt_idmap = [], []
    for vid in range(cfg.n_vehicles):
        is_att = vid in attacker_set
        vr = random.Random(f"{cfg.seed}:veh:{vid}")
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
        atype = catalog[attackers_sorted.index(vid) % len(catalog)] if is_att else "none"
        v = Vehicle(
            vid=vid, spawn_x=float(vid * 20), lane_y=float(vid % 4) * 4.0,
            speed=cfg.nominal_speed * (0.85 + 0.3 * vr.random()),
            is_attacker=is_att, priv=key, pub=pub, cert_digest=cert_digest,
            linkage_ctx=ctx, i_period=i_period, j_index=j, request_hash=req_hash,
            direction=(vr.random() - 0.5) * 0.5,          # small spread around +x
            wander_amp=vr.random() * 1.5, wander_w=0.15 + vr.random() * 0.25,
            phase=vr.random() * 6.283, gps_q=0.5 + vr.expovariate(1.2),
            is_faulty=(vid in faulty_set), attack_type=atype)
        vehicles.append(v)
        gt_vehicle.append(R.GtVehicle(true_vehicle_id=true_id, spawn_time=0.0, is_attacker=is_att,
                                      attacker_role=(atype if is_att else "none"),
                                      is_faulty=(vid in faulty_set)))
        gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=cert_digest,
                                        i_period=i_period, valid_from=0.0, valid_to=cfg.n_steps * cfg.dt))

    digest_to_vehicle = {v.cert_digest: v for v in vehicles}
    vrng = {v.vid: random.Random(f"{cfg.seed}:sensor:{v.vid}") for v in vehicles}

    # ---- MA state (updated ONLINE during the loop) ----
    ma_reports: list[dict] = []
    gt_report_labels: list[R.GtReportLabel] = []
    gt_emissions: list[dict] = []
    ma_investigations: list[R.MaInvestigation] = []
    ma_crl_events: list[R.MaCrlEvent] = []
    gt_linkage_rev: list[R.GtLinkageRevocation] = []
    crl_entries: list[CrlLinkageEntry] = []
    reporters_by_subject: dict[str, set[str]] = {}
    subj_seconds: dict[str, set[int]] = {}
    subj_span: dict[str, list[float]] = {}
    revocation_time: dict[str, float] = {}
    revoked_digests: list[str] = []
    last_claimed: dict[tuple[int, str], tuple[float, float, float, float, float]] = {}
    cert_first_seen: dict[str, float] = {}
    cert_last_seen: dict[str, float] = {}
    counters = {"report": 0, "case": 0, "crl": 0}

    def measure(v: Vehicle, x: float, y: float) -> tuple[float, float, float]:
        """Advance the per-vehicle GNSS error state and return (mx, my, pos_conf)."""
        r = vrng[v.vid]
        a = math.exp(-cfg.dt / cfg.gps_bias_tau_s)
        bmag = cfg.gps_bias_sigma_m * (cfg.faulty_bias_mult if v.is_faulty else 1.0)
        q = bmag * math.sqrt(max(1e-9, 1 - a * a))
        v.bias_x = a * v.bias_x + q * r.gauss(0, 1)
        v.bias_y = a * v.bias_y + q * r.gauss(0, 1)
        sigma = cfg.gps_sigma_m * v.gps_q * wmult
        mx = x + v.bias_x + r.gauss(0, sigma)
        my = y + v.bias_y + r.gauss(0, sigma)
        if r.random() < cfg.gps_outlier_rate:
            ang = r.random() * 6.283
            mx += cfg.gps_outlier_mag_m * math.cos(ang)
            my += cfg.gps_outlier_mag_m * math.sin(ang)
        conf = 2.448 * math.sqrt(sigma * sigma + v.bias_x * v.bias_x + v.bias_y * v.bias_y)
        return mx, my, conf

    def attack_claim(v: Vehicle, t: float, mx: float, my: float, mspeed: float,
                     mheading: float) -> tuple[float, float, float, float]:
        """Falsified claim for an active attacker; returns (cx, cy, cspeed, cheading)."""
        r = vrng[v.vid]
        typ = v.attack_type
        cx, cy, cs, ch = mx, my, mspeed, mheading
        if typ == "ConstPos":
            if v.frozen is None:
                v.frozen = (mx, my)
            cx, cy = v.frozen
        elif typ == "ConstPosOffset":
            cx, cy = mx + 25.0, my + 25.0
        elif typ == "RandomPos":
            cx, cy = mx + r.uniform(-60, 60), my + r.uniform(-60, 60)
        elif typ == "Teleport":
            if int(t) % 4 == 0:
                cx, cy = mx + 150.0, my + 80.0
        elif typ == "SineWavePos":
            cx, cy = mx, my + 20.0 * math.sin(0.6 * t)
        elif typ == "ConstSpeedOffset":
            cs = mspeed + 12.0
        elif typ == "RandomSpeed":
            cs = r.uniform(0, 40)
        elif typ == "StopAndGo":
            cs = 0.0 if int(t) % 2 == 0 else 35.0
        elif typ == "ReversedHeading":
            ch = (mheading + 180.0) % 360.0
        elif typ == "HeadingOffset":
            ch = (mheading + 45.0) % 360.0
        elif typ == "DataReplay":
            if len(v.hist) >= 5:
                cx, cy, cs, ch = v.hist[-5]
        elif typ == "SlowDrift":
            v.drift += 0.35
            cx, cy = mx + v.drift, my
        elif typ == "AlongRoadOffset":
            hr = math.radians(mheading)
            cx, cy = mx + 30.0 * math.cos(hr), my + 30.0 * math.sin(hr)
        return cx, cy, cs, ch

    Z = 3.0                     # residual must exceed ~3x the broadcast uncertainty to count
    MIN_CONSEC = 2              # consecutive violations required before a reason fires

    def detectors(ref, cx, cy, cs, ch, t, conf) -> dict:
        """Confidence-normalized residuals (detnorm ~1 at the firing threshold). A single GNSS
        outlier gives a one-off spike but is filtered by the streak gate + not advancing the ref."""
        det = {d: 0.0 for d in ("positionSpeedInconsistency", "positionJump",
                                "headingInconsistency", "staleOrReplay", "constantPositionFrozen")}
        px, py, ps, ph, pt = ref
        dtt = max(1e-6, t - pt)
        disp = math.hypot(cx - px, cy - py)
        tol = max(conf, 0.5 * cfg.consistency_threshold_m)     # uncertainty scale
        det["positionSpeedInconsistency"] = abs(disp - cs * dtt) / (Z * tol)
        det["positionJump"] = disp / (cs * dtt + Z * tol + cfg.consistency_threshold_m)
        if disp > 5.0:
            bearing = math.degrees(math.atan2(cy - py, cx - px)) % 360.0
            det["headingInconsistency"] = _ang_diff(ch, bearing) / cfg.heading_threshold_deg
        if cx == px and cy == py and cs > 0.5:
            det["constantPositionFrozen"] = 1.5
            det["staleOrReplay"] = 1.2
        return det

    def resolve_and_revoke(subject_digest: str, t: float) -> None:
        counters["case"] += 1
        counters["crl"] += 1
        case_id = f"case_{counters['case']:04d}"
        prov = pca.resolve(subject_digest)
        ls1_i, la_id1 = la1.seed_at(prov["la_handle1"], prov["i"])
        ls2_i, la_id2 = la2.seed_at(prov["la_handle2"], prov["i"])
        ra.blacklist_request(prov["request_hash"], t)
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
            x, y, tspeed, theading = tx.true_state(t)
            mx, my, conf = measure(tx, x, y)
            attacking = tx.is_attacker and cfg.attack_start <= t <= cfg.attack_end
            if attacking:
                cx, cy, cs, ch = attack_claim(tx, t, mx, my, tspeed, theading)
            else:
                cx, cy, cs, ch = mx, my, tspeed, theading
            tx.hist.append((cx, cy, cs, ch))
            falsified = attacking and (math.hypot(cx - mx, cy - my) > 1.0
                                       or abs(cs - tspeed) > 1.0 or _ang_diff(ch, theading) > 5.0)
            if falsified and tx.onset is None:
                tx.onset = t
            if rng.random() < cfg.emit_sample_prob:
                gt_emissions.append(dict(
                    emit_id=f"emt_{len(gt_emissions):08d}", t=round(t, 3),
                    true_vehicle_id=f"veh_{tx.vid:03d}", true_x=round(x, 3), true_y=round(y, 3),
                    claimed_x=round(cx, 3), claimed_y=round(cy, 3), claimed_speed=round(cs, 3),
                    pos_conf=round(conf, 3), is_attacker=tx.is_attacker, is_faulty=tx.is_faulty,
                    falsified=bool(falsified), _visibility=R.ORACLE))
            for rx in vehicles:
                if rx.vid == tx.vid:
                    continue
                rt = revocation_time.get(tx.cert_digest)
                if rt is not None and t >= rt + cfg.crl_propagation_delay:
                    continue                                        # ENFORCEMENT: drop revoked cert
                key = (rx.vid, tx.cert_digest)
                st = last_claimed.get(key)
                if st is None:                                      # first reception -> seed reference
                    last_claimed[key] = {"ref": (cx, cy, cs, ch, t), "streak": {}}
                    continue
                ref = st["ref"]
                det = detectors(ref, cx, cy, cs, ch, t, conf)
                violating = any(v >= 1.0 for v in det.values())
                for k, v in det.items():
                    st["streak"][k] = st["streak"].get(k, 0) + 1 if v >= 1.0 else 0
                if not violating:
                    st["ref"] = (cx, cy, cs, ch, t)                 # advance ref only on clean samples
                fired = {k: det[k] for k in det if st["streak"].get(k, 0) >= MIN_CONSEC}
                if not fired:
                    continue
                if rng.random() > cfg.report_prob:                  # suppression / loss
                    continue
                counters["report"] += 1
                rid = f"rpt_{counters['report']:05d}"
                delay = rng.uniform(0.0, cfg.net_delay_max)
                px, py = ref[0], ref[1]
                score_norm = max(det.values())
                reasons = sorted(fired, key=lambda k: -det[k])
                row = R.MaReport(
                    report_id=rid, ingest_time=round(t + delay, 3), detection_time=t, generation_time=t,
                    reporter_cert_digest=rx.cert_digest, subject_cert_digest=tx.cert_digest,
                    reason_codes=reasons,
                    detector_outputs=[{"check_id": reasons[0], "score": round(det[reasons[0]], 3),
                                       "verdict": "fail"}],
                    cert_validity={"sig_valid": True, "not_expired": True,
                                   "not_revoked": True, "chain_ok": True},
                    evidence_msg_refs=[f"{rid}-m"],
                    st_bbox=[min(cx, px), min(cy, py), max(cx, px), max(cy, py)],
                    st_tstart=ref[4], st_tend=t, duplicate_flag=False).to_dict()
                row["detector_score"] = round(det[reasons[0]], 3)
                row["detector_score_norm"] = round(score_norm, 3)
                row["subject_pos_confidence"] = round(conf, 3)
                row["cert_crl_status"] = "active"
                row["sig_valid"] = True
                for d, val in det.items():
                    row[f"detnorm_{d}"] = round(val, 3)
                ma_reports.append(row)
                subj = digest_to_vehicle[tx.cert_digest]
                correctness = ("correct" if subj.is_attacker
                               else ("faulty_detection" if subj.is_faulty else "false_positive"))
                gt_report_labels.append(R.GtReportLabel(
                    report_id=rid, reporter_true_id=f"veh_{rx.vid:03d}",
                    subject_true_id=f"veh_{tx.vid:03d}", report_correctness=correctness))
                reporters_by_subject.setdefault(tx.cert_digest, set()).add(rx.cert_digest)
                subj_seconds.setdefault(tx.cert_digest, set()).add(int(t))
                span = subj_span.setdefault(tx.cert_digest, [t, t])
                span[1] = t
                touched_subjects.add(tx.cert_digest)
        # Online MA decision: revoke subjects with SUSTAINED multi-reporter evidence.
        for subject_digest in sorted(touched_subjects):
            if subject_digest in revocation_time:
                continue
            reporters = reporters_by_subject[subject_digest]
            secs = subj_seconds[subject_digest]
            span = subj_span[subject_digest]
            if (len(reporters) >= cfg.report_threshold_k and len(secs) >= cfg.revoke_min_seconds
                    and (span[1] - span[0]) >= cfg.revoke_persist_s):
                resolve_and_revoke(subject_digest, t)

    # ---- attack ground truth (with onset) ----
    gt_attacks = [R.GtAttack(
        attack_id=f"atk_{v.vid}", true_vehicle_id=f"veh_{v.vid:03d}", attack_type=v.attack_type,
        start_time=cfg.attack_start, end_time=cfg.attack_end, attack_onset_time=v.onset,
        params={}) for v in vehicles if v.is_attacker]

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
                                gt_vehicle, gt_idmap, gt_attacks, gt_report_labels, gt_linkage_rev,
                                gt_emissions)
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
                   gt_attacks, gt_report_labels, gt_linkage_rev, gt_emissions) -> dict[str, str]:
    os.makedirs(os.path.join(cfg.out_dir, "ma"), exist_ok=True)
    os.makedirs(os.path.join(cfg.out_dir, "ground_truth"), exist_ok=True)
    files = {
        "ma/ma_reports.jsonl": sorted(ma_reports, key=lambda r: (r["ingest_time"], r["report_id"])),
        "ma/ma_investigations.jsonl": sorted(ma_invest, key=lambda r: (r.opened_time, r.case_id)),
        "ma/ma_crl_events.jsonl": sorted(ma_crl, key=lambda r: (r.issue_time, r.crl_id)),
        "ma/ma_cert_status.jsonl": sorted(ma_cert, key=lambda r: r.cert_digest),
        "ground_truth/gt_vehicle.jsonl": sorted(gt_veh, key=lambda r: r.true_vehicle_id),
        "ground_truth/gt_identity_map.jsonl": sorted(gt_idmap, key=lambda r: r.pseudonym_cert_digest),
        "ground_truth/gt_attacks.jsonl": sorted(gt_attacks, key=lambda r: r.attack_id),
        "ground_truth/gt_report_labels.jsonl": sorted(gt_report_labels, key=lambda r: r.report_id),
        "ground_truth/gt_linkage_revocation.jsonl": sorted(gt_linkage_rev, key=lambda r: r.true_vehicle_id),
        "ground_truth/gt_emissions_sample.jsonl": sorted(gt_emissions, key=lambda r: r["emit_id"]),
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
        "generator": "scms_sim_ref.mock_pipeline (pre-MOSAIC reference, realistic v2)",
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
    p.add_argument("--attacker-pct", type=float, default=0.0)
    p.add_argument("--faulty-pct", type=float, default=0.05)
    p.add_argument("--weather", default="clear", choices=list(WEATHER_MULT))
    p.add_argument("--out", default="datasets/poc_run")
    args = p.parse_args(argv)
    cfg = PipelineConfig(seed=args.seed, n_vehicles=args.vehicles, n_steps=args.steps,
                         attacker_pct=args.attacker_pct, faulty_pct=args.faulty_pct,
                         weather=args.weather, out_dir=args.out)
    res = run_pipeline(cfg)
    print(f"vehicles={res.n_vehicles} reports={res.n_reports} "
          f"investigations={res.n_investigations} revoked={res.n_revoked}")
    print(f"revoked_cert_digests={res.revoked_cert_digests}")
    print(f"data_digest={res.data_digest}")
    print(f"outputs in {os.path.abspath(res.out_dir)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
