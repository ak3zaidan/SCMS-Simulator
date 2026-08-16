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
from collections import Counter
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
    "SlowDrift", "AlongRoadOffset", "Sybil",
    "DoS", "DelayedMessages",
)
WEATHER_MULT = {"clear": 1.0, "rain": 1.5, "fog": 2.0, "snow": 2.5}
WEATHER_RADIO_LOSS = {"clear": 0.0, "rain": 0.03, "fog": 0.02, "snow": 0.06}
_SYBIL_MIN = 4        # distinct certs co-located in one cell before sybilCoLocation fires
_CELL_M = 5.0         # spatial cell size for co-location


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
    gps_degrade_rate: float = 0.006      # per-step prob a benign vehicle enters a bad-GNSS burst
    gps_degrade_factor: float = 6.0      # noise multiplier during a burst (canyon/tunnel/foliage)
    gps_degrade_dur_s: float = 3.0       # burst length (< the revocation persistence gate)
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
    # --- radio / channel realism ---
    radio_range_m: float = 500.0         # a receiver only hears transmitters within this range
    packet_loss_base: float = 0.0        # baseline per-message loss
    nlos_loss: float = 0.0               # 0..1 obstruction loss, growing with distance/range
    chan_capacity: int = 40              # in-range CAMs/step before congestion (CBR) loss kicks in
    art_max_m: float = 900.0             # acceptance-range threshold (max plausible claim distance)
    max_accel_mps2: float = 12.0         # implausible-acceleration threshold
    freq_max: float = 6.0                # beacon-rate normalizer (CAMs/interval)
    dos_burst: int = 12                  # CAMs/interval a DoS attacker floods
    delay_s: float = 6.0                 # DelayedMessages staleness
    stale_max_s: float = 5.0             # staleness threshold for staleOrReplay
    # --- pseudonym rotation ---
    rotate_period_s: float = 0.0         # 0 = one pseudonym per vehicle (no rotation)
    # --- collusion / false accusation ---
    collude_pct: float = 0.0             # fraction of ATTACKERS that also file false reports
    victim_pct: float = 0.10             # fraction of benign vehicles targeted by colluders
    ma_defense: bool = True              # trusted-reporter gating (reputation + rate limit)
    reputation_max: int = 40             # a reporter itself reported more than this is distrusted
    report_budget: int = 30              # a reporter filing more than this is rate-limited
    # --- Sybil ---
    sybil_ghosts: int = 6                # ghost identities a "Sybil" attacker fabricates
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
    # pseudonyms / sybil / collusion
    pseudonyms: list = field(default_factory=list)   # [{k,i,j,digest,valid_from,valid_to}]
    ghosts: list = field(default_factory=list)       # sybil ghost cert digests
    colluder: bool = False
    victims: list = field(default_factory=list)      # vids this colluder falsely reports
    revoked: bool = False
    revocation_time: Optional[float] = None
    # mutable per-run state
    bias_x: float = 0.0
    bias_y: float = 0.0
    degrade_until: float = -1.0          # in a transient bad-GNSS burst while t < this
    frozen: Optional[tuple[float, float]] = None
    drift: float = 0.0
    hist: list = field(default_factory=list)   # (x,y,speed,heading) claim history for replay
    onset: Optional[float] = None

    def active_pseudonym(self, t: float, rotate_period_s: float) -> dict:
        if rotate_period_s <= 0 or len(self.pseudonyms) <= 1:
            return self.pseudonyms[0]
        idx = min(int(t / rotate_period_s), len(self.pseudonyms) - 1)
        return self.pseudonyms[idx]

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
    # colluders (subset of attackers) + their benign victims
    crng = random.Random(f"{cfg.seed}:collusion")
    n_coll = int(round(len(attackers_sorted) * cfg.collude_pct))
    colluder_set = set(crng.sample(attackers_sorted, n_coll)) if n_coll else set()
    n_victims = int(round(len(non_attackers) * cfg.victim_pct))
    victim_pool = sorted(frng.sample(non_attackers, min(n_victims, len(non_attackers)))) if (n_victims and colluder_set) else []

    total_time = cfg.n_steps * cfg.dt
    n_rot = 1 if cfg.rotate_period_s <= 0 else max(1, math.ceil(total_time / cfg.rotate_period_s))
    pseudonym_info: dict[str, dict] = {}   # digest -> {i,j,lv,veh,ghost,valid_from,valid_to}

    # ---- Provisioning (real linkage values via two Linkage Authorities) ----
    vehicles: list[Vehicle] = []
    gt_vehicle, gt_idmap = [], []
    for vid in range(cfg.n_vehicles):
        is_att = vid in attacker_set
        vr = random.Random(f"{cfg.seed}:veh:{vid}")
        la_h1, la_h2 = f"lc1:{vid}", f"lc2:{vid}"
        la_id1, la_id2 = 0x0001, 0x0002
        ctx = DeviceLinkageContext(la_id1, la_id2,
                                   cfg.derive(f"ls1:{vid}", 16), cfg.derive(f"ls2:{vid}", 16))
        la1.register(la_h1, ctx.ls1_0, la_id1)
        la2.register(la_h2, ctx.ls2_0, la_id2)
        req_hash = hashlib.sha256(f"req|{cfg.seed}|{vid}".encode()).hexdigest()[:16]
        true_id = f"veh_{vid:03d}"
        ra.bind(req_hash, true_id)
        atype = catalog[attackers_sorted.index(vid) % len(catalog)] if is_att else "none"

        # rotating pseudonyms (one when rotation is off); one CRL entry at i=0 covers all of them
        pseudonyms = []
        for k in range(n_rot):
            i_k, j_k = k // cfg.jmax, (vid + k) % cfg.jmax
            pk = ca.keypair_from_seed(cfg.derive(f"key:{vid}:{k}"))
            dig = ca.hashed_id8(ca.public_bytes(pk)).hex()
            pca.issue(dig, req_hash, i_k, j_k, la_h1, la_h2)
            vf = k * cfg.rotate_period_s if cfg.rotate_period_s > 0 else 0.0
            vt = (k + 1) * cfg.rotate_period_s if cfg.rotate_period_s > 0 else total_time
            pseudonyms.append({"k": k, "i": i_k, "j": j_k, "digest": dig, "valid_from": vf, "valid_to": vt})
            pseudonym_info[dig] = {"i": i_k, "j": j_k, "lv": ctx.linkage_value_for(i_k, j_k),
                                   "ghost": False, "veh_vid": vid}
            gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=dig,
                                            i_period=i_k, valid_from=round(vf, 3), valid_to=round(vt, 3)))
        p0 = pseudonyms[0]
        v = Vehicle(
            vid=vid, spawn_x=float(vid * 20), lane_y=float(vid % 4) * 4.0,
            speed=cfg.nominal_speed * (0.85 + 0.3 * vr.random()),
            is_attacker=is_att, priv=None, pub=b"", cert_digest=p0["digest"],
            linkage_ctx=ctx, i_period=p0["i"], j_index=p0["j"], request_hash=req_hash,
            direction=(vr.random() - 0.5) * 0.5,
            wander_amp=vr.random() * 1.5, wander_w=0.15 + vr.random() * 0.25,
            phase=vr.random() * 6.283, gps_q=0.5 + vr.expovariate(1.2),
            is_faulty=(vid in faulty_set), attack_type=atype, pseudonyms=pseudonyms,
            colluder=(vid in colluder_set), victims=list(victim_pool) if vid in colluder_set else [])
        # sybil ghost identities (extra co-located pseudonyms of the same true vehicle)
        if is_att and atype == "Sybil":
            for g in range(cfg.sybil_ghosts):
                gj = (cfg.jmax - 1 - g) % cfg.jmax
                gk = ca.keypair_from_seed(cfg.derive(f"ghost:{vid}:{g}"))
                gdig = ca.hashed_id8(ca.public_bytes(gk)).hex()
                pca.issue(gdig, req_hash, 0, gj, la_h1, la_h2)
                v.ghosts.append(gdig)
                pseudonym_info[gdig] = {"i": 0, "j": gj, "lv": ctx.linkage_value_for(0, gj),
                                        "ghost": True, "veh_vid": vid}
                gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=gdig,
                                                i_period=0, valid_from=0.0, valid_to=total_time))
        vehicles.append(v)
        gt_vehicle.append(R.GtVehicle(true_vehicle_id=true_id, spawn_time=0.0, is_attacker=is_att,
                                      attacker_role=(atype if is_att else "none"),
                                      is_faulty=(vid in faulty_set),
                                      colluding_group_id=("colluders" if vid in colluder_set else None)))

    digest_to_vehicle = {d: vehicles[info["veh_vid"]] for d, info in pseudonym_info.items()}
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
    revoked_vehicles: dict[int, float] = {}          # vid -> revocation time (vehicle-level)
    revoked_digests: list[str] = []                  # triggering digests (for the CRL sanity check)
    filed_by: dict[str, int] = {}                    # reports FILED per reporter cert (rate-limit)
    received_by: dict[str, int] = {}                 # reports RECEIVED per subject cert (reputation)
    last_claimed: dict[tuple[int, str], dict] = {}
    cert_first_seen: dict[str, float] = {}
    cert_last_seen: dict[str, float] = {}
    counters = {"report": 0, "case": 0, "crl": 0}

    def measure(v: Vehicle, x: float, y: float, t: float) -> tuple[float, float, float]:
        """Advance the per-vehicle GNSS error state and return (mx, my, pos_conf)."""
        r = vrng[v.vid]
        a = math.exp(-cfg.dt / cfg.gps_bias_tau_s)
        bmag = cfg.gps_bias_sigma_m * (cfg.faulty_bias_mult if v.is_faulty else 1.0)
        q = bmag * math.sqrt(max(1e-9, 1 - a * a))
        v.bias_x = a * v.bias_x + q * r.gauss(0, 1)
        v.bias_y = a * v.bias_y + q * r.gauss(0, 1)
        # transient bad-GNSS bursts (urban canyon / tunnel / foliage) -> a benign vehicle emits
        # SUSTAINED large residuals for a few seconds, the realistic source of benign false positives
        if not v.is_attacker and t >= v.degrade_until and r.random() < cfg.gps_degrade_rate:
            v.degrade_until = t + cfg.gps_degrade_dur_s
        sigma_nom = cfg.gps_sigma_m * v.gps_q * wmult
        sigma = sigma_nom * (cfg.gps_degrade_factor if t < v.degrade_until else 1.0)
        mx = x + v.bias_x + r.gauss(0, sigma)
        my = y + v.bias_y + r.gauss(0, sigma)
        if r.random() < cfg.gps_outlier_rate:
            ang = r.random() * 6.283
            mx += cfg.gps_outlier_mag_m * math.cos(ang)
            my += cfg.gps_outlier_mag_m * math.sin(ang)
        # broadcast confidence reflects the NOMINAL error, not the multipath spike -- a real receiver
        # underreports uncertainty during a burst, so the sustained residual reads as misbehaviour
        conf = 2.448 * math.sqrt(sigma_nom * sigma_nom + v.bias_x * v.bias_x + v.bias_y * v.bias_y)
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
        det = {d: 0.0 for d in ("positionSpeedInconsistency", "positionJump", "headingInconsistency",
                                "staleOrReplay", "constantPositionFrozen", "implausibleAcceleration")}
        px, py, ps, ph, pt = ref
        dtt = max(1e-6, t - pt)
        disp = math.hypot(cx - px, cy - py)
        tol = max(conf, 0.5 * cfg.consistency_threshold_m)     # uncertainty scale
        det["positionSpeedInconsistency"] = abs(disp - cs * dtt) / (Z * tol)
        det["positionJump"] = disp / (cs * dtt + Z * tol + cfg.consistency_threshold_m)
        det["implausibleAcceleration"] = (abs(cs - ps) / dtt) / cfg.max_accel_mps2
        # heading is only trustworthy when the displacement dominates position noise -- otherwise the
        # bearing between two noisy fixes is ill-defined (a false-positive source for noisy/faulty GNSS)
        if disp > max(5.0, 2.5 * conf):
            bearing = math.degrees(math.atan2(cy - py, cx - px)) % 360.0
            det["headingInconsistency"] = _ang_diff(ch, bearing) / cfg.heading_threshold_deg
        if cx == px and cy == py and cs > 0.5:
            det["constantPositionFrozen"] = 1.5
            det["staleOrReplay"] = 1.2
        return det

    DET_KEYS = ("positionSpeedInconsistency", "positionJump", "headingInconsistency",
                "staleOrReplay", "constantPositionFrozen", "implausibleAcceleration",
                "sybilCoLocation", "acceptanceRangeThreshold", "beaconFrequency")
    MOTION_KEYS = ("positionSpeedInconsistency", "positionJump", "headingInconsistency",
                   "constantPositionFrozen", "implausibleAcceleration")
    touched_subjects: set[str] = set()

    def trusted(reporter_digest: str) -> bool:
        """Collusion-robust MA gate: only count a reporter that is not revoked, not itself
        heavily reported (reputation), and not spraying beyond a report budget (rate limit)."""
        if not cfg.ma_defense:
            return True
        rv = digest_to_vehicle.get(reporter_digest)
        if rv is None:
            return False
        return (rv.vid not in revoked_vehicles
                and filed_by.get(reporter_digest, 0) <= cfg.report_budget
                and received_by.get(reporter_digest, 0) < cfg.reputation_max)

    def file_report(t, reporter_digest, subject_digest, subject_veh, reasons, det, conf,
                    cx, cy, px, py, malicious):
        counters["report"] += 1
        rid = f"rpt_{counters['report']:05d}"
        delay = rng.uniform(0.0, cfg.net_delay_max)
        rep_veh = digest_to_vehicle[reporter_digest]
        row = R.MaReport(
            report_id=rid, ingest_time=round(t + delay, 3), detection_time=t, generation_time=t,
            reporter_cert_digest=reporter_digest, subject_cert_digest=subject_digest,
            reason_codes=reasons,
            detector_outputs=[{"check_id": reasons[0], "score": round(det.get(reasons[0], 1.0), 3),
                               "verdict": "fail"}],
            cert_validity={"sig_valid": True, "not_expired": True, "not_revoked": True, "chain_ok": True},
            evidence_msg_refs=[f"{rid}-m"],
            st_bbox=[min(cx, px), min(cy, py), max(cx, px), max(cy, py)],
            st_tstart=t, st_tend=t, duplicate_flag=False).to_dict()
        row["detector_score"] = round(det.get(reasons[0], 1.0), 3)
        row["detector_score_norm"] = round(max(det.values()) if det else 1.0, 3)
        row["subject_pos_confidence"] = round(conf, 3)
        row["cert_crl_status"] = "active"
        row["sig_valid"] = True
        for k in DET_KEYS:
            row[f"detnorm_{k}"] = round(det.get(k, 0.0), 3)
        ma_reports.append(row)
        if malicious:
            correctness = "malicious_false_report"
        elif subject_veh.is_attacker:
            correctness = "correct"
        elif subject_veh.is_faulty:
            correctness = "faulty_detection"
        else:
            correctness = "false_positive"
        gt_report_labels.append(R.GtReportLabel(
            report_id=rid, reporter_true_id=f"veh_{rep_veh.vid:03d}",
            subject_true_id=f"veh_{subject_veh.vid:03d}", report_correctness=correctness))
        reporters_by_subject.setdefault(subject_digest, set()).add(reporter_digest)
        subj_seconds.setdefault(subject_digest, set()).add(int(t))
        span = subj_span.setdefault(subject_digest, [t, t])
        span[1] = t
        filed_by[reporter_digest] = filed_by.get(reporter_digest, 0) + 1
        received_by[subject_digest] = received_by.get(subject_digest, 0) + 1
        touched_subjects.add(subject_digest)

    def resolve_and_revoke(veh: Vehicle, trigger_digest: str, t: float) -> None:
        counters["case"] += 1
        counters["crl"] += 1
        case_id = f"case_{counters['case']:04d}"
        prov = pca.resolve(trigger_digest)
        ls1_0, la_id1 = la1.seed_at(prov["la_handle1"], 0)   # revoke from period 0 -> covers ALL
        ls2_0, la_id2 = la2.seed_at(prov["la_handle2"], 0)   # of this vehicle's pseudonyms + ghosts
        ra.blacklist_request(prov["request_hash"], t)
        crl_entries.append(CrlLinkageEntry(i=0, la_id1=la_id1, la_id2=la_id2,
                                           ls1_i=ls1_0, ls2_i=ls2_0, jmax=cfg.jmax))
        veh.revoked = True
        veh.revocation_time = t
        revoked_vehicles[veh.vid] = t
        revoked_digests.append(trigger_digest)
        reporters = {r for r in reporters_by_subject.get(trigger_digest, set()) if trusted(r)}
        ma_investigations.append(R.MaInvestigation(
            case_id=case_id, opened_time=t, trigger="report_threshold",
            cluster_size=len(reporters), num_distinct_reporters=len(reporters),
            linkage_result="same", identity_resolved=True, revocation_decision="revoke",
            resolution_time=t, decision_time=t,
            resolved_case_handle=hashlib.sha256(case_id.encode()).hexdigest()[:12]))
        ma_crl_events.append(R.MaCrlEvent(crl_id=f"crl_{counters['crl']:04d}", issue_time=t,
                                          entry_type="seed", num_entries=len(crl_entries)))
        gt_linkage_rev.append(R.GtLinkageRevocation(true_vehicle_id=f"veh_{veh.vid:03d}",
                                                    should_have_been_revoked=veh.is_attacker,
                                                    true_revocation_time=t))

    def enforced(veh: Vehicle, t: float) -> bool:
        return veh.revoked and veh.revocation_time is not None and t >= veh.revocation_time + cfg.crl_propagation_delay

    # ---- Simulation loop: pre-pass broadcast -> detect -> collude -> (online) revoke ----
    for step in range(cfg.n_steps):
        t = step * cfg.dt
        touched_subjects.clear()

        # PRE-PASS: every active broadcast this step (real pseudonyms + sybil ghosts)
        broadcasts: list[dict] = []
        for tx in vehicles:
            if enforced(tx, t):
                continue
            ps = tx.active_pseudonym(t, cfg.rotate_period_s)
            digest = ps["digest"]
            cert_first_seen.setdefault(digest, t)
            cert_last_seen[digest] = t
            x, y, tspeed, theading = tx.true_state(t)
            mx, my, conf = measure(tx, x, y, t)
            attacking = tx.is_attacker and cfg.attack_start <= t <= cfg.attack_end
            msg_count, cg = 1, t                                  # CAMs this interval; claimed gen time
            if attacking:
                cx, cy, cs, ch = attack_claim(tx, t, mx, my, tspeed, theading)
                if tx.attack_type == "DoS":
                    msg_count = cfg.dos_burst                     # flood the channel
                elif tx.attack_type == "DelayedMessages":
                    cg = t - cfg.delay_s                          # stale timestamp
            else:
                cx, cy, cs, ch = mx, my, tspeed, theading
            tx.hist.append((cx, cy, cs, ch))
            falsified = attacking and (math.hypot(cx - mx, cy - my) > 1.0 or abs(cs - tspeed) > 1.0
                                       or _ang_diff(ch, theading) > 5.0 or msg_count > 1 or cg < t - 1e-6)
            if falsified and tx.onset is None:
                tx.onset = t
            broadcasts.append(dict(veh=tx, digest=digest, cx=cx, cy=cy, cs=cs, ch=ch, conf=conf,
                                   ghost=False, x=x, y=y, falsified=falsified, msg_count=msg_count, cg=cg))
            if attacking and tx.attack_type == "Sybil":     # fabricate co-located ghost identities
                sr = vrng[tx.vid]
                for gdig in tx.ghosts:
                    cert_first_seen.setdefault(gdig, t)
                    cert_last_seen[gdig] = t
                    broadcasts.append(dict(veh=tx, digest=gdig, cx=cx + sr.uniform(-2, 2),
                                           cy=cy + sr.uniform(-2, 2), cs=cs, ch=ch, conf=conf,
                                           ghost=True, x=x, y=y, falsified=True, msg_count=1, cg=t))

        # per-message ground-truth emission sampling (real broadcasts only)
        for b in broadcasts:
            if b["ghost"]:
                continue
            if rng.random() < cfg.emit_sample_prob:
                tx = b["veh"]
                gt_emissions.append(dict(
                    emit_id=f"emt_{len(gt_emissions):08d}", t=round(t, 3),
                    true_vehicle_id=f"veh_{tx.vid:03d}", true_x=round(b["x"], 3), true_y=round(b["y"], 3),
                    claimed_x=round(b["cx"], 3), claimed_y=round(b["cy"], 3), claimed_speed=round(b["cs"], 3),
                    pos_conf=round(b["conf"], 3), is_attacker=tx.is_attacker, is_faulty=tx.is_faulty,
                    falsified=bool(b["falsified"]), _visibility=R.ORACLE))

        # sybil co-location: distinct certs sharing a spatial cell this step
        cells = Counter((round(b["cx"] / _CELL_M), round(b["cy"] / _CELL_M)) for b in broadcasts)

        # DETECTION pass (receiver-outer): a receiver only hears in-range transmitters, with
        # distance/NLOS/weather/congestion packet loss -> the report graph becomes spatially LOCAL
        # (reporters near the subject) instead of all-to-all, and far-away attackers go unobserved.
        rx_pos = {rx.vid: rx.true_state(t)[:2] for rx in vehicles if not enforced(rx, t)}
        wx_loss = WEATHER_RADIO_LOSS.get(cfg.weather, 0.0)
        for rx in vehicles:
            if enforced(rx, t):
                continue
            rxx, rxy = rx_pos[rx.vid]
            in_range = [(b, math.hypot(b["x"] - rxx, b["y"] - rxy)) for b in broadcasts
                        if b["veh"].vid != rx.vid and math.hypot(b["x"] - rxx, b["y"] - rxy) <= cfg.radio_range_m]
            load = sum(b["msg_count"] for b, _ in in_range)
            cong = min(0.8, max(0.0, (load - cfg.chan_capacity) / max(1, cfg.chan_capacity)) * 0.5)
            reporter_digest = rx.active_pseudonym(t, cfg.rotate_period_s)["digest"]
            for b, dist in in_range:
                loss = cfg.packet_loss_base + cfg.nlos_loss * (dist / cfg.radio_range_m) + cong + wx_loss
                if loss > 0 and rng.random() < loss:
                    continue                                    # packet dropped on the channel
                tx, digest, cx, cy, cs, ch, conf = (b["veh"], b["digest"], b["cx"], b["cy"],
                                                    b["cs"], b["ch"], b["conf"])
                key = (rx.vid, digest)
                st = last_claimed.get(key)
                if st is None:
                    st = {"ref": (cx, cy, cs, ch, t), "streak": {}}
                    last_claimed[key] = st
                    det = {k: 0.0 for k in DET_KEYS}
                else:
                    det = detectors(st["ref"], cx, cy, cs, ch, t, conf)
                # radio-dependent detectors (need the receiver position + per-message metadata)
                det["sybilCoLocation"] = cells[(round(cx / _CELL_M), round(cy / _CELL_M))] / _SYBIL_MIN
                det["acceptanceRangeThreshold"] = math.hypot(cx - rxx, cy - rxy) / cfg.art_max_m
                det["beaconFrequency"] = b["msg_count"] / cfg.freq_max
                det["staleOrReplay"] = max(det.get("staleOrReplay", 0.0), (t - b["cg"]) / cfg.stale_max_s)
                motion_violating = any(det.get(k, 0.0) >= 1.0 for k in MOTION_KEYS)
                for k in DET_KEYS:
                    st["streak"][k] = st["streak"].get(k, 0) + 1 if det.get(k, 0.0) >= 1.0 else 0
                if not motion_violating:
                    st["ref"] = (cx, cy, cs, ch, t)             # advance ref only on clean motion
                fired = {k: det[k] for k in DET_KEYS if st["streak"].get(k, 0) >= MIN_CONSEC}
                if not fired:
                    continue
                if rng.random() > cfg.report_prob:
                    continue
                reasons = sorted(fired, key=lambda k: -det[k])
                file_report(t, reporter_digest, digest, tx, reasons, det, conf,
                            cx, cy, st["ref"][0], st["ref"][1], malicious=False)

        # COLLUSION pass: colluders file fabricated reports against benign victims
        for tx in vehicles:
            if not tx.colluder or enforced(tx, t) or not (cfg.attack_start <= t <= cfg.attack_end):
                continue
            reporter_digest = tx.active_pseudonym(t, cfg.rotate_period_s)["digest"]
            for vv in tx.victims:
                victim = vehicles[vv]
                if enforced(victim, t):
                    continue
                subject_digest = victim.active_pseudonym(t, cfg.rotate_period_s)["digest"]
                if rng.random() > cfg.report_prob:
                    continue
                det = {k: 0.0 for k in DET_KEYS}
                det["positionSpeedInconsistency"] = 1.3         # plausible fabricated evidence
                file_report(t, reporter_digest, subject_digest, victim,
                            ["positionSpeedInconsistency"], det, 5.0, 0.0, 0.0, 0.0, 0.0, malicious=True)

        # Online MA decision: revoke subjects with SUSTAINED, TRUSTED multi-reporter evidence.
        for subject_digest in sorted(touched_subjects):
            veh = digest_to_vehicle[subject_digest]
            if veh.vid in revoked_vehicles:
                continue
            reporters = {r for r in reporters_by_subject[subject_digest] if trusted(r)}
            secs = subj_seconds[subject_digest]
            span = subj_span[subject_digest]
            if (len(reporters) >= cfg.report_threshold_k and len(secs) >= cfg.revoke_min_seconds
                    and (span[1] - span[0]) >= cfg.revoke_persist_s):
                resolve_and_revoke(veh, subject_digest, t)

    # ---- attack ground truth (with onset) ----
    gt_attacks = [R.GtAttack(
        attack_id=f"atk_{v.vid}", true_vehicle_id=f"veh_{v.vid:03d}", attack_type=v.attack_type,
        start_time=cfg.attack_start, end_time=cfg.attack_end, attack_onset_time=v.onset,
        params={}) for v in vehicles if v.is_attacker]

    # ---- Real-linkage sanity: the CRL entry must revoke EVERY observed cert of a revoked vehicle ----
    for vid in revoked_vehicles:
        for d in cert_first_seen:
            info = pseudonym_info[d]
            if info["veh_vid"] != vid:
                continue
            assert any(e.matches(info["i"], info["j"], info["lv"]) for e in crl_entries), \
                "CRL entry failed to revoke a pseudonym of its target device"

    # ---- Certificate status (MA-visible): a cert is revoked iff its vehicle is ----
    ma_cert_status = [R.MaCertStatus(
        cert_digest=d, first_seen=f, last_seen=cert_last_seen[d], valid_from=0.0,
        valid_to=cfg.n_steps * cfg.dt, issuing_pca="PCA-1",
        crl_status=("revoked" if pseudonym_info[d]["veh_vid"] in revoked_vehicles else "active"),
        revocation_time=revoked_vehicles.get(pseudonym_info[d]["veh_vid"]))
        for d, f in cert_first_seen.items()]

    # ---- Write outputs + manifest ----
    data_files = _write_outputs(cfg, ma_reports, ma_investigations, ma_crl_events, ma_cert_status,
                                gt_vehicle, gt_idmap, gt_attacks, gt_report_labels, gt_linkage_rev,
                                gt_emissions)
    data_digest = _data_digest(cfg.out_dir, data_files)
    _write_manifest(cfg, data_files, data_digest,
                    counts=dict(vehicles=cfg.n_vehicles, reports=len(ma_reports),
                                investigations=len(ma_investigations), revoked=len(revoked_vehicles)))

    return RunResult(out_dir=cfg.out_dir, n_vehicles=cfg.n_vehicles, n_reports=len(ma_reports),
                     n_investigations=len(ma_investigations), n_revoked=len(revoked_vehicles),
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
    p.add_argument("--rotate-period", type=float, default=0.0, help="pseudonym rotation period (s); 0=off")
    p.add_argument("--collude-pct", type=float, default=0.0, help="fraction of attackers that false-report")
    p.add_argument("--victim-pct", type=float, default=0.10, help="fraction of benign vehicles targeted")
    p.add_argument("--sybil-ghosts", type=int, default=6, help="ghost identities a Sybil attacker fakes")
    p.add_argument("--no-ma-defense", action="store_true", help="disable trusted-reporter gating")
    p.add_argument("--radio-range", type=float, default=500.0, help="reception range (m)")
    p.add_argument("--packet-loss", type=float, default=0.0, help="baseline per-message loss")
    p.add_argument("--nlos", type=float, default=0.0, help="distance-growing obstruction loss (0..1)")
    p.add_argument("--chan-capacity", type=int, default=40, help="in-range CAMs/step before congestion")
    p.add_argument("--out", default="datasets/poc_run")
    args = p.parse_args(argv)
    cfg = PipelineConfig(seed=args.seed, n_vehicles=args.vehicles, n_steps=args.steps,
                         attacker_pct=args.attacker_pct, faulty_pct=args.faulty_pct,
                         weather=args.weather, rotate_period_s=args.rotate_period,
                         collude_pct=args.collude_pct, victim_pct=args.victim_pct,
                         sybil_ghosts=args.sybil_ghosts, ma_defense=not args.no_ma_defense,
                         radio_range_m=args.radio_range, packet_loss_base=args.packet_loss,
                         nlos_loss=args.nlos, chan_capacity=args.chan_capacity, out_dir=args.out)
    res = run_pipeline(cfg)
    print(f"vehicles={res.n_vehicles} reports={res.n_reports} "
          f"investigations={res.n_investigations} revoked={res.n_revoked}")
    print(f"revoked_cert_digests={res.revoked_cert_digests}")
    print(f"data_digest={res.data_digest}")
    print(f"outputs in {os.path.abspath(res.out_dir)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
