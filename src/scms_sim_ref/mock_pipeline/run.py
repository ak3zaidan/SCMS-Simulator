"""Closed-loop reference pipeline (pre-MOSAIC), now with realistic, diverse data.

Runs the entire SCMS misbehaviour lifecycle in pure Python so the architecture,
schemas, trust boundaries, leakage firewall, and reproducibility can be validated
without any MOSAIC/SUMO/Java toolchain:

    provision (real linkage values) -> signed CAMs over a MODELLED GNSS sensor ->
    live attacks (19-type catalog across 8 families) -> receiver multi-detector
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
import dataclasses
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
    "DoS", "DelayedMessages", "InvalidSignature", "ExpiredCert", "NotYetValid",
    "OutOfOrder", "DoSRandom",
)
WEATHER_MULT = {"clear": 1.0, "rain": 1.5, "fog": 2.0, "snow": 2.5}        # GNSS error multiplier
WEATHER_RADIO_LOSS = {"clear": 0.0, "rain": 0.03, "fog": 0.02, "snow": 0.06}
WEATHER_SPEED_MULT = {"clear": 1.0, "rain": 0.85, "fog": 0.75, "snow": 0.6}  # drivers slow in bad weather
# Heterogeneous fleet: distinct length + kinematics per class (trucks/buses slower & sluggish, motos
# nimble). Weights are the "mixed" fleet composition; "car" fleet is homogeneous.
VEHICLE_TYPES = {
    "car":        {"speed_mult": 1.00, "length": 4.5,  "accel": 1.8, "decel": 2.5, "weight": 0.75},
    "motorcycle": {"speed_mult": 1.10, "length": 2.2,  "accel": 2.5, "decel": 3.0, "weight": 0.08},
    "truck":      {"speed_mult": 0.80, "length": 12.0, "accel": 0.8, "decel": 1.5, "weight": 0.10},
    "bus":        {"speed_mult": 0.85, "length": 12.0, "accel": 0.9, "decel": 1.6, "weight": 0.07},
}


def _pick_vehicle_type(rng, fleet: str) -> str:
    if fleet != "mixed":
        return fleet if fleet in VEHICLE_TYPES else "car"
    r, acc = rng.random(), 0.0
    for name, p in VEHICLE_TYPES.items():
        acc += p["weight"]
        if r <= acc:
            return name
    return "car"
_SYBIL_MIN = 4        # distinct certs at NEARLY the same point before sybilCoLocation fires
_CELL_M = 3.0         # co-location cell: small enough that a bumper-to-bumper queue (~7 m spacing)
                      # never fills it, but tightly co-located Sybil ghosts (spoofing one point) do


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
    attack_intensity: float = 1.0        # scales falsification magnitudes (subtle <1 .. blatant >1)
    attack_duty_cycle: float = 1.0       # <1: attacker falsifies only in bursts (evades sustained-
                                         # evidence revocation); fraction of each pulse period "on"
    attack_pulse_period_s: float = 20.0  # length of one on/off pulse cycle when duty_cycle < 1
    # --- GNSS / sensor realism ---
    gps_sigma_m: float = 1.2             # white per-axis noise (× per-vehicle quality × weather)
    gps_bias_sigma_m: float = 1.5        # OU-correlated slow bias amplitude
    gps_bias_tau_s: float = 20.0         # bias correlation time
    gps_outlier_rate: float = 0.01       # per-message multipath outlier probability
    gps_outlier_mag_m: float = 12.0
    gps_degrade_rate: float = 0.006      # per-step prob a benign vehicle enters a bad-GNSS burst
    gps_degrade_factor: float = 6.0      # noise multiplier during a burst (canyon/tunnel/foliage)
    gps_degrade_dur_s: float = 3.0       # burst length (< the revocation persistence gate)
    gps_jam_rate: float = 0.0            # per-step prob a benign vehicle loses GNSS fix entirely
    gps_jam_dur_s: float = 4.0           # outage length; it HONESTLY broadcasts huge uncertainty
    faulty_pct: float = 0.05             # malfunctioning-sensor (non-attacker) fraction
    faulty_bias_mult: float = 5.0        # faulty = large SUSTAINED bias (smooth, self-consistent)
    weather: str = "clear"
    # --- detection / revocation ---
    consistency_threshold_m: float = 5.0
    heading_threshold_deg: float = 35.0
    detector_lag_s: float = 1.5          # compare each fix to one ~this old (robust to turns/outliers)
    report_prob: float = 0.9
    report_threshold_k: int = 3          # distinct reporters to open an investigation
    revoke_min_seconds: int = 4          # AND reports in >= this many distinct seconds
    revoke_persist_s: float = 3.0        # AND spanning >= this long (blunts transient benign FPs)
    revoke_window_s: float = 15.0        # evidence must be SUSTAINED within this recent window
                                         # (so bursty benign faults spread over a trip never add up)
    net_delay_max: float = 2.0
    crl_propagation_delay: float = 2.0
    emit_sample_prob: float = 0.03       # per-message ground-truth emission sampling
    # --- radio / channel realism ---
    radio_range_m: float = 500.0         # a receiver only hears transmitters within this range
    packet_loss_base: float = 0.0        # baseline per-message loss
    nlos_loss: float = 0.0               # 0..1 obstruction loss, growing with distance/range
    chan_capacity: int = 40              # in-range CAMs/step before congestion (CBR) loss kicks in
    art_max_m: float = 150.0             # tolerance for claiming a position beyond the radio range
    offroad_tol_m: float = 15.0          # map check: claimed distance from the nearest road tolerated
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
    # --- long-running traffic flow (opt-in; default is the fixed-fleet linear model) ---
    traffic_flow: bool = False           # spawn/despawn vehicles over time (steady-state population)
    duration_s: float = 0.0              # flow: sim length in seconds (overrides n_steps if > 0)
    arrival_rate: float = 2.0            # flow: mean vehicles spawned per second
    max_total_vehicles: int = 0          # flow: cap total spawns (0 = unlimited) -> bounds memory
    road_network: str = "linear"         # "linear" (straight lines) | "grid" (routed trips)
    grid_w: int = 6
    grid_h: int = 6
    grid_block_m: float = 120.0
    n_lanes: int = 1                     # >1: parallel lanes per road -> overtaking, less gridlock
    lane_width_m: float = 3.5
    trip_speed_min: float = 8.0
    trip_speed_max: float = 18.0
    fleet: str = "mixed"                 # "mixed" (car/moto/truck/bus) | a single type name
    # car-following (IDM) -> queues, stop-and-go, congestion (flow + grid only)
    car_following: bool = True
    idm_accel: float = 1.5               # max acceleration (m/s^2)
    idm_decel: float = 2.0               # comfortable deceleration (m/s^2)
    idm_time_headway: float = 1.3        # desired time gap to leader (s)
    idm_min_gap: float = 2.5             # jam distance (m)
    veh_length_m: float = 5.0
    idm_lookahead_m: float = 70.0        # leader search distance ahead
    traffic_lights: bool = False         # signalized intersections (grid nodes) -> stops & queues
    light_cycle_s: float = 24.0          # full signal cycle (half green per axis)
    # cornering: vehicles slow into sharp turns rather than taking 90-degree grid corners at full
    # speed -> realistic speed dips at intersections (opt-in; car-following + grid only). Off by
    # default: it makes benign kinematics harder to separate from attacks (more realistic, but it
    # shifts the tuned operating point), so enable it deliberately for harder/realer corpora.
    turn_slowdown: bool = False
    turn_speed_mps: float = 6.0          # speed cap through a sharp bend (m/s); ~21 km/h
    turn_min_angle_deg: float = 40.0     # only bends sharper than this are slowed
    # time-varying demand (rush hour / night) + origin-destination bias
    demand_profile: str = "uniform"      # "uniform" | "rush" | "night"
    od_model: str = "uniform"            # trip destination law: "uniform" | "gravity" (distance-decay)
    od_gravity_scale: float = 2.0        # gravity: hop decay scale (smaller -> shorter trips)
    boundary_origins: bool = False       # trips ORIGINATE at the grid perimeter (realistic sources/sinks)
    attack_delay_s: float = 2.0          # flow: an attacker starts falsifying this long after spawn
    attack_delay_jitter_s: float = 0.0   # spread attack onset across attackers by up to this (realism)
    state_prune_every: int = 50          # flow: steps between detection-state LRU prunes
    state_prune_ttl: int = 20            # flow: evict detection state untouched this many steps
    live_interval_s: float = 0.0         # >0: write a throttled live_state.json for the GUI map
                                         # (best-effort viz; NOT part of the data digest)
    verbose: bool = False                # print a flow progress heartbeat (CLI/GUI set this True)
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
    # lifecycle (flow mode): spawn/despawn over time + a routed trip
    spawn_time: float = 0.0
    finish_time: Optional[float] = None   # None = never despawns (fixed-fleet)
    trip: object = None                    # roads.Trip when road_network="grid"
    attack_from: float = 0.0
    attack_to: float = 0.0
    pulse_phase: float = 0.0               # per-vehicle offset [0,1) for intermittent (pulsed) attacks
    # vehicle class (heterogeneous fleet)
    veh_type: str = "car"
    veh_length: float = 4.5
    idm_a: float = 1.8                     # per-class max acceleration
    idm_b: float = 2.5                     # per-class comfortable deceleration
    desired_speed: float = 0.0            # free-flow target (car-following cap)
    # car-following kinematic state (integrated each step when cf=True)
    cf: bool = False
    lane_off: float = 0.0                  # perpendicular lane offset (multi-lane roads)
    s_pos: float = 0.0                     # arc-length travelled along the route
    cur_v: float = 0.0                     # current speed
    cur_x: float = 0.0
    cur_y: float = 0.0
    cur_h: float = 0.0
    # mutable per-run state
    bias_x: float = 0.0
    bias_y: float = 0.0
    degrade_until: float = -1.0          # in a transient bad-GNSS burst while t < this
    jam_until: float = -1.0              # in a total GNSS outage (goes silent) while t < this
    frozen: Optional[tuple[float, float]] = None
    drift: float = 0.0
    drift_rate: float = 0.0
    hist: list = field(default_factory=list)   # (x,y,speed,heading) claim history for replay
    onset: Optional[float] = None

    def active_pseudonym(self, t: float, rotate_period_s: float) -> dict:
        if rotate_period_s <= 0 or len(self.pseudonyms) <= 1:
            return self.pseudonyms[0]
        # rotate on the vehicle's OWN clock (since spawn), so flow vehicles rotate mid-trip
        idx = min(int(max(0.0, t - self.spawn_time) / rotate_period_s), len(self.pseudonyms) - 1)
        return self.pseudonyms[idx]

    def true_state(self, t: float) -> tuple[float, float, float, float]:
        """True (x, y, speed, heading[deg]) at time t."""
        if self.cf:                       # car-following: kinematics integrated in the loop
            return self.cur_x, self.cur_y, self.cur_v, self.cur_h
        if self.trip is not None:
            return self.trip.state(t)
        # straight heading + gentle lane wander, on the vehicle's own clock
        t = t - self.spawn_time
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


_PROB_FIELDS = ("report_prob", "attacker_pct", "faulty_pct", "collude_pct", "victim_pct",
                "packet_loss_base", "nlos_loss", "gps_outlier_rate", "gps_degrade_rate",
                "attack_duty_cycle")

# Named CLI scenario presets (mirror the GUI one-click presets). Keys are argparse dests, so any flag
# the user also passes still overrides the preset (they are applied via parser.set_defaults). Reach a
# realistic long routed run in one command, e.g.: python -m ...run --preset urban_rush --featurize
CLI_PRESETS = {
    "urban_rush": dict(flow=True, road="grid", grid=6, lanes=2, arrival_rate=3.0, duration=300.0,
                       demand="rush", traffic_lights=True, od_model="gravity", fleet="mixed",
                       attacker_pct=0.15, faulty_pct=0.06, collude_pct=0.3, rotate_period=60.0,
                       weather="clear", radio_range=250.0),
    "highway": dict(flow=True, road="grid", grid=4, lanes=3, arrival_rate=4.0, duration=300.0,
                    demand="uniform", traffic_lights=False, fleet="mixed", attacker_pct=0.12,
                    faulty_pct=0.05, weather="clear", radio_range=400.0),
    "night_rain": dict(flow=True, road="grid", grid=6, lanes=2, arrival_rate=1.2, duration=300.0,
                       demand="night", traffic_lights=True, od_model="gravity", fleet="mixed",
                       attacker_pct=0.2, faulty_pct=0.05, weather="rain", gps_jam_rate=0.01,
                       radio_range=220.0),
    "gridlock": dict(flow=True, road="grid", grid=5, lanes=1, arrival_rate=5.0, duration=250.0,
                     demand="rush", traffic_lights=True, turn_slowdown=True, fleet="mixed",
                     attacker_pct=0.15, faulty_pct=0.05, weather="clear", radio_range=200.0),
    "stealth_hard": dict(flow=True, road="grid", grid=6, lanes=2, arrival_rate=2.0, duration=300.0,
                         demand="uniform", od_model="gravity", fleet="mixed", attacker_pct=0.2,
                         attack_intensity=0.5, attack_duty_cycle=0.3, faulty_pct=0.08,
                         weather="fog", radio_range=250.0),
}


def validate_config(cfg: PipelineConfig) -> PipelineConfig:
    """Fail fast on impossible configs and clamp fractions to [0,1]. Returns the (mutated) config.

    Guards the degenerate cases an adversarial audit flagged: zero dt / IDM params (division by
    zero), a grid too small to route on, unknown enum values. Called at the top of run_pipeline."""
    if cfg.dt <= 0:
        raise ValueError(f"dt must be > 0 (got {cfg.dt})")
    if cfg.n_steps < 0:
        raise ValueError(f"n_steps must be >= 0 (got {cfg.n_steps})")
    if cfg.jmax < 1:
        raise ValueError(f"jmax must be >= 1 (got {cfg.jmax})")
    if cfg.radio_range_m <= 0:
        raise ValueError(f"radio_range_m must be > 0 (got {cfg.radio_range_m})")
    if cfg.idm_accel <= 0 or cfg.idm_decel <= 0:
        raise ValueError(f"idm_accel and idm_decel must be > 0 (got {cfg.idm_accel}, {cfg.idm_decel})")
    if cfg.weather not in WEATHER_MULT:
        raise ValueError(f"weather must be one of {sorted(WEATHER_MULT)} (got {cfg.weather!r})")
    if cfg.demand_profile not in ("uniform", "rush", "night"):
        raise ValueError(f"demand_profile must be uniform|rush|night (got {cfg.demand_profile!r})")
    if cfg.od_model not in ("uniform", "gravity"):
        raise ValueError(f"od_model must be uniform|gravity (got {cfg.od_model!r})")
    if cfg.turn_speed_mps < 0:
        raise ValueError(f"turn_speed_mps must be >= 0 (got {cfg.turn_speed_mps})")
    if cfg.attack_pulse_period_s <= 0:
        raise ValueError(f"attack_pulse_period_s must be > 0 (got {cfg.attack_pulse_period_s})")
    if cfg.od_gravity_scale <= 0:
        raise ValueError(f"od_gravity_scale must be > 0 (got {cfg.od_gravity_scale})")
    if cfg.fleet != "mixed" and cfg.fleet not in VEHICLE_TYPES:
        raise ValueError(f"fleet must be 'mixed' or one of {sorted(VEHICLE_TYPES)} (got {cfg.fleet!r})")
    if cfg.road_network not in ("linear", "grid"):
        raise ValueError(f"road_network must be linear|grid (got {cfg.road_network!r})")
    if cfg.traffic_flow:
        if cfg.arrival_rate < 0:
            raise ValueError(f"arrival_rate must be >= 0 (got {cfg.arrival_rate})")
        if cfg.duration_s < 0:
            raise ValueError(f"duration_s must be >= 0 (got {cfg.duration_s})")
        if cfg.road_network == "grid" and (cfg.grid_w < 2 or cfg.grid_h < 2):
            raise ValueError(f"grid road network needs grid_w and grid_h >= 2 "
                             f"(got {cfg.grid_w}x{cfg.grid_h})")
    for name in _PROB_FIELDS:                       # clamp fractions rather than produce nonsense
        setattr(cfg, name, min(1.0, max(0.0, float(getattr(cfg, name)))))
    return cfg


# tuple-typed config fields -- JSON has no tuples, so lists are coerced back on load
_TUPLE_FIELDS = ("attacker_ids", "attack_types")


def config_schema() -> dict:
    """Machine-readable schema of every PipelineConfig field: {name: {type, default}}. Lets tools
    build config UIs/validators programmatically against the full knob surface."""
    out = {}
    for f in dataclasses.fields(PipelineConfig):
        default = f.default
        if default is dataclasses.MISSING:
            default = None
        elif isinstance(default, tuple):
            default = list(default)
        out[f.name] = {"type": str(f.type), "default": default}
    return out


def config_from_dict(d: dict) -> PipelineConfig:
    """Build a PipelineConfig from a plain dict (e.g. a saved run's manifest).

    Accepts either a raw config dict or a full manifest.json (which nests the config under a
    "config" key). Unknown keys are ignored with a stderr warning (forward/backward compatible
    across config-schema changes); list values for tuple fields are coerced back to tuples. This
    is the inverse of what `_write_manifest` serializes, so a saved run replays byte-for-byte."""
    import sys as _sys
    if "config" in d and isinstance(d["config"], dict):
        d = d["config"]
    known = {f.name for f in dataclasses.fields(PipelineConfig)}
    unknown = sorted(set(d) - known)
    if unknown:
        print(f"[config] ignoring {len(unknown)} unknown key(s): {', '.join(unknown)}", file=_sys.stderr)
    kw = {k: v for k, v in d.items() if k in known}
    for name in _TUPLE_FIELDS:
        if name in kw and isinstance(kw[name], list):
            kw[name] = tuple(kw[name])
    return PipelineConfig(**kw)


# --------------------------------------------------------------------------- #
# Pipeline
# --------------------------------------------------------------------------- #
# Graceful interruption of long (multi-hour) flow runs: a SIGINT (Ctrl-C) sets this flag; the step
# loop checks it and breaks to the NORMAL finalization path, so the partial dataset is still written
# with a valid manifest + digest instead of being lost. PER_STEP_HOOK is a test/telemetry seam called
# once per step (default None -> zero effect); tests use it to simulate an interrupt deterministically.
_ABORT = {"flag": False}
PER_STEP_HOOK = None


def run_pipeline(cfg: PipelineConfig) -> RunResult:
    validate_config(cfg)
    _ABORT["flag"] = False                            # fresh per run (module state is not reentrant)
    rng = random.Random(cfg.seed)
    wmult = WEATHER_MULT.get(cfg.weather, 1.0)
    la1, la2 = LinkageAuthority(1), LinkageAuthority(2)
    pca, ra = PseudonymCA(), RegistrationAuthority()

    catalog = cfg.attack_types or (cfg.attack_type,)
    total_time = cfg.n_steps * cfg.dt
    net = None
    if cfg.road_network == "grid":
        from .roads import GridNetwork
        net = GridNetwork(cfg.grid_w, cfg.grid_h, cfg.grid_block_m)
    pseudonym_info: dict[str, dict] = {}   # digest -> {i,j,lv,ghost,veh_vid}
    vehicles: list[Vehicle] = []
    gt_vehicle, gt_idmap = [], []
    atk_counter = {"n": 0}                  # running index for round-robin attack-type assignment

    def make_vehicle(vid, spawn_time, is_att, is_flt, is_coll, trip, life_hint):
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
        if is_att:
            atype = catalog[atk_counter["n"] % len(catalog)]
            atk_counter["n"] += 1
        else:
            atype = "none"
        cf = bool(cfg.traffic_flow and cfg.car_following and trip is not None)
        if cf:
            # car-following: arrival time is dynamic (congestion) -> despawn on route completion.
            finish_time = None
            life = 2.0 * trip.length / max(1.0, cfg.trip_speed_min)   # generous upper bound
        else:
            finish_time = (trip.t1 if trip is not None
                           else (spawn_time + life_hint if cfg.traffic_flow else None))
            life = life_hint if finish_time is None else max(cfg.dt, finish_time - spawn_time)
        n_rot = 1 if cfg.rotate_period_s <= 0 else max(1, math.ceil(life / cfg.rotate_period_s))
        pseudonyms = []
        for k in range(n_rot):
            i_k, j_k = k // cfg.jmax, (vid + k) % cfg.jmax
            pk = ca.keypair_from_seed(cfg.derive(f"key:{vid}:{k}"))
            dig = ca.hashed_id8(ca.public_bytes(pk)).hex()
            pca.issue(dig, req_hash, i_k, j_k, la_h1, la_h2)
            vf = spawn_time + (k * cfg.rotate_period_s if cfg.rotate_period_s > 0 else 0.0)
            vt = spawn_time + ((k + 1) * cfg.rotate_period_s if cfg.rotate_period_s > 0 else life)
            pseudonyms.append({"k": k, "i": i_k, "j": j_k, "digest": dig, "valid_from": vf, "valid_to": vt})
            pseudonym_info[dig] = {"i": i_k, "j": j_k, "lv": ctx.linkage_value_for(i_k, j_k),
                                   "ghost": False, "veh_vid": vid}
            gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=dig,
                                            i_period=i_k, valid_from=round(vf, 3), valid_to=round(vt, 3)))
        p0 = pseudonyms[0]
        base_speed = trip.speed if trip is not None else cfg.nominal_speed * (0.85 + 0.3 * vr.random())
        vtype = _pick_vehicle_type(vr, cfg.fleet)
        tp = VEHICLE_TYPES[vtype]
        speed = base_speed * tp["speed_mult"] * WEATHER_SPEED_MULT.get(cfg.weather, 1.0)
        v = Vehicle(
            vid=vid, spawn_x=float(vid * 20), lane_y=float(vid % 4) * 4.0, speed=speed,
            is_attacker=is_att, priv=None, pub=b"", cert_digest=p0["digest"],
            linkage_ctx=ctx, i_period=p0["i"], j_index=p0["j"], request_hash=req_hash,
            direction=(vr.random() - 0.5) * 0.5,
            wander_amp=vr.random() * 1.5, wander_w=0.15 + vr.random() * 0.25,
            phase=vr.random() * 6.283, gps_q=0.5 + vr.expovariate(1.2),
            is_faulty=is_flt, attack_type=atype, pseudonyms=pseudonyms, colluder=is_coll,
            spawn_time=spawn_time, finish_time=finish_time, trip=trip)
        v.veh_type, v.veh_length = vtype, tp["length"]
        v.idm_a, v.idm_b, v.desired_speed = tp["accel"], tp["decel"], speed
        if cfg.n_lanes > 1:
            v.lane_off = (vr.randrange(cfg.n_lanes) - (cfg.n_lanes - 1) / 2.0) * cfg.lane_width_m
        if cf:
            v.cf = True
            v.cur_v = speed
            v.cur_x, v.cur_y, v.cur_h = trip.at_distance(0.0)
            if v.lane_off:
                hr = math.radians(v.cur_h)
                v.cur_x += v.lane_off * -math.sin(hr)
                v.cur_y += v.lane_off * math.cos(hr)
        v.attack_from = (spawn_time + cfg.attack_delay_s) if cfg.traffic_flow else cfg.attack_start
        if is_att and cfg.attack_delay_jitter_s > 0:  # varied attack onset across attackers (realism)
            v.attack_from += random.Random(f"{cfg.seed}:onset:{vid}").random() * cfg.attack_delay_jitter_s
        v.attack_to = (spawn_time + life) if cfg.traffic_flow else cfg.attack_end
        if is_att and cfg.attack_duty_cycle < 1.0:   # desync pulses across attackers (own rng stream)
            v.pulse_phase = random.Random(f"{cfg.seed}:pulse:{vid}").random()
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
                                                i_period=0, valid_from=round(spawn_time, 3),
                                                valid_to=round(spawn_time + life, 3)))
        vehicles.append(v)
        gt_vehicle.append(R.GtVehicle(true_vehicle_id=true_id, spawn_time=round(spawn_time, 3),
                                      is_attacker=is_att, attacker_role=(atype if is_att else "none"),
                                      is_faulty=is_flt, veh_type=vtype,
                                      colluding_group_id=("colluders" if is_coll else None)))
        return v

    if cfg.traffic_flow:
        # ---- FLOW: vehicles arrive over time and despawn at trip end (steady-state population) ----
        n_steps = int(round((cfg.duration_s if cfg.duration_s > 0 else total_time) / cfg.dt))
        total_time = n_steps * cfg.dt

        def demand_mult(frac):    # time-of-day arrival-rate multiplier in (0, 1]
            if cfg.demand_profile == "rush":     # morning + evening peaks, quiet midday/edges
                peak = math.exp(-((frac - 0.25) / 0.09) ** 2) + math.exp(-((frac - 0.75) / 0.09) ** 2)
                return 0.2 + 0.8 * min(1.0, peak)
            if cfg.demand_profile == "night":    # sparse throughout, gently ramping
                return 0.15 + 0.25 * frac
            return 1.0                            # uniform

        center = (net.w // 2, net.h // 2) if net is not None else None
        rrng = random.Random(f"{cfg.seed}:flow")
        vid, tt = 0, 0.0
        while True:                              # thinning: candidates at max rate, kept per demand
            tt += rrng.expovariate(cfg.arrival_rate) if cfg.arrival_rate > 0 else total_time
            if tt >= total_time:
                break
            if cfg.max_total_vehicles and vid >= cfg.max_total_vehicles:
                break                            # cap total spawns (predictable memory bound)
            frac = tt / total_time
            if rrng.random() > demand_mult(frac):
                continue                          # thinned out (off-peak)
            is_att = rrng.random() < cfg.attacker_pct
            is_flt = (not is_att) and rrng.random() < cfg.faulty_pct
            is_coll = is_att and rrng.random() < cfg.collude_pct
            spd = cfg.trip_speed_min + rrng.random() * (cfg.trip_speed_max - cfg.trip_speed_min)
            # OD bias: during a rush peak, most trips head to the centre (commute-to-core)
            dest = center if (net is not None and cfg.demand_profile == "rush"
                              and rrng.random() < demand_mult(frac) - 0.2) else None
            trip = (net.random_trip(rrng, spd, tt, dest_hint=dest, od_model=cfg.od_model,
                                    gravity_scale=cfg.od_gravity_scale,
                                    boundary_origin=cfg.boundary_origins)
                    if net is not None else None)
            make_vehicle(vid, tt, is_att, is_flt, is_coll, trip, life_hint=90.0)
            vid += 1
    else:
        # ---- FIXED FLEET: N vehicles present for the whole run (the default model) ----
        n_steps = cfg.n_steps
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
        crng = random.Random(f"{cfg.seed}:collusion")
        n_coll = int(round(len(attackers_sorted) * cfg.collude_pct))
        colluder_set = set(crng.sample(attackers_sorted, n_coll)) if n_coll else set()
        n_victims = int(round(len(non_attackers) * cfg.victim_pct))
        victim_pool = sorted(frng.sample(non_attackers, min(n_victims, len(non_attackers)))) if (n_victims and colluder_set) else []
        for vid in range(cfg.n_vehicles):
            make_vehicle(vid, 0.0, vid in attacker_set, vid in faulty_set, vid in colluder_set,
                         None, life_hint=total_time)
        for v in vehicles:
            if v.colluder:
                v.victims = list(victim_pool)

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
    subj_events: dict[str, list] = {}    # subject digest -> [(time, reporter_digest)] in a sliding window
    reported_vids: set[int] = set()       # true vids ever reported (for the live map colouring only)
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
        k = cfg.attack_intensity                             # scales falsification magnitude
        cx, cy, cs, ch = mx, my, mspeed, mheading
        if typ == "ConstPos":
            if v.frozen is None:
                v.frozen = (mx, my)
            cx, cy = v.frozen
        elif typ == "ConstPosOffset":
            cx, cy = mx + 25.0 * k, my + 25.0 * k
        elif typ == "RandomPos":
            cx, cy = mx + r.uniform(-60, 60) * k, my + r.uniform(-60, 60) * k
        elif typ == "Teleport":
            if int(t) % 4 == 0:
                cx, cy = mx + 150.0 * k, my + 80.0 * k
        elif typ == "SineWavePos":
            cx, cy = mx, my + 20.0 * k * math.sin(0.6 * t)
        elif typ == "ConstSpeedOffset":
            cs = mspeed + 12.0 * k
        elif typ == "RandomSpeed":
            cs = r.uniform(0, 40)
        elif typ == "StopAndGo":
            cs = 0.0 if int(t) % 2 == 0 else 35.0
        elif typ == "ReversedHeading":
            ch = (mheading + 180.0) % 360.0
        elif typ == "HeadingOffset":
            ch = (mheading + 45.0 * k) % 360.0
        elif typ == "DataReplay":
            if len(v.hist) >= 5:
                cx, cy, cs, ch = v.hist[-5]
        elif typ == "SlowDrift":
            # position drift whose rate ramps up: stealthy at onset (evades), but the growing
            # velocity discrepancy eventually crosses the plausibility threshold (high-latency catch).
            v.drift_rate = min(5.0, v.drift_rate + 0.10 * k)
            v.drift += v.drift_rate * cfg.dt
            cx, cy = mx + v.drift, my
        elif typ == "AlongRoadOffset":
            hr = math.radians(mheading)
            cx, cy = mx + 30.0 * k * math.cos(hr), my + 30.0 * k * math.sin(hr)
        elif typ == "DoSRandom":                             # flood with random content
            cx, cy = mx + r.uniform(-60, 60) * k, my + r.uniform(-60, 60) * k
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
        avg_v = 0.5 * (cs + ps)                                # avg over the interval (accel/decel-safe)
        jerk_slack = 0.3 * abs(cs - ps) * dtt                  # extra tolerance for stop-and-go jerk
        det["positionSpeedInconsistency"] = max(0.0, abs(disp - avg_v * dtt) - jerk_slack) / (Z * tol)
        det["positionJump"] = disp / (avg_v * dtt + Z * tol + cfg.consistency_threshold_m)
        det["implausibleAcceleration"] = (abs(cs - ps) / dtt) / cfg.max_accel_mps2
        # headingInconsistency is computed in the loop over a SHORT (one-step) baseline, not this
        # lagged reference -- a 1.5 s baseline spans road turns and reads them as heading lies.
        if cx == px and cy == py and cs > 0.5:
            det["constantPositionFrozen"] = 1.5
            det["staleOrReplay"] = 1.2
        return det

    DET_KEYS = ("positionSpeedInconsistency", "positionJump", "headingInconsistency",
                "staleOrReplay", "constantPositionFrozen", "implausibleAcceleration",
                "sybilCoLocation", "acceptanceRangeThreshold", "beaconFrequency",
                "signatureVerification", "certValidity", "mapOffRoad")
    # SOFT features: carried in the fusion fingerprint (detnorm_*) for ML, but NEVER trigger a report
    # on their own -- a constant-velocity tracker false-positives on curves, so it must stay soft.
    SOFT_KEYS = ("kalmanConsistency",)
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
                    cx, cy, px, py, malicious, sig_valid=True):
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
        row["sig_valid"] = bool(sig_valid)
        for k in (*DET_KEYS, *SOFT_KEYS):
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
        subj_events.setdefault(subject_digest, []).append((t, reporter_digest))
        reported_vids.add(subject_veh.vid)
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
        reporters = {r for (_tt, r) in subj_events.get(trigger_digest, []) if trusted(r)}
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

    # ---- streaming output (flow only): keep long runs memory-bounded ----
    stream = cfg.traffic_flow
    stream_counts = {"reports": 0, "labels": 0, "emit": 0}
    fh_rep = fh_lbl = fh_emit = None
    if stream:
        os.makedirs(os.path.join(cfg.out_dir, "ma"), exist_ok=True)
        os.makedirs(os.path.join(cfg.out_dir, "ground_truth"), exist_ok=True)
        fh_rep = open(os.path.join(cfg.out_dir, "ma", "ma_reports.jsonl"), "w", encoding="utf-8", newline="\n")
        fh_lbl = open(os.path.join(cfg.out_dir, "ground_truth", "gt_report_labels.jsonl"), "w", encoding="utf-8", newline="\n")
        fh_emit = open(os.path.join(cfg.out_dir, "ground_truth", "gt_emissions_sample.jsonl"), "w", encoding="utf-8", newline="\n")

    def flush_streams():
        for row in ma_reports:
            fh_rep.write(ca.canonical_bytes(row).decode("utf-8") + "\n")
        for row in gt_report_labels:
            fh_lbl.write(ca.canonical_bytes(row.to_dict()).decode("utf-8") + "\n")
        for row in gt_emissions:
            fh_emit.write(ca.canonical_bytes(row).decode("utf-8") + "\n")
        stream_counts["reports"] += len(ma_reports)
        stream_counts["labels"] += len(gt_report_labels)
        stream_counts["emit"] += len(gt_emissions)
        ma_reports.clear(); gt_report_labels.clear(); gt_emissions.clear()

    def prune_state(step_now: int, active: dict) -> None:
        cutoff = step_now - cfg.state_prune_ttl
        for k in [k for k, st in last_claimed.items() if st.get("touch", -1) < cutoff]:
            del last_claimed[k]
        for d in [d for d in subj_events if pseudonym_info[d]["veh_vid"] not in active]:
            subj_events.pop(d, None)

    live_path = os.path.join(cfg.out_dir, "live_state.json")
    live_every = max(1, int(round(cfg.live_interval_s / cfg.dt))) if cfg.live_interval_s > 0 else 0
    if live_every:
        os.makedirs(cfg.out_dir, exist_ok=True)

    def write_live(active_list, t):
        # throttled live snapshot for the GUI map: [x, y, state]; state 0 benign/1 attacker/2 reported/
        # 3 revoked. Best-effort, atomic-replaced, and NOT part of the data digest (determinism-safe).
        vs = []
        for v in active_list:
            x, y = (v.cur_x, v.cur_y) if v.cf else v.true_state(t)[:2]
            state = (3 if v.vid in revoked_vehicles else 1 if v.is_attacker
                     else 2 if v.vid in reported_vids else 0)
            vs.append([round(x, 1), round(y, 1), state])
        try:
            with open(live_path + ".tmp", "w", encoding="utf-8") as fh:
                json.dump({"t": round(t, 1), "n": len(vs), "vehicles": vs}, fh)
            os.replace(live_path + ".tmp", live_path)
        except OSError:
            pass

    # car-following: the Intelligent Driver Model gives realistic accel/decel, so vehicles queue and
    # experience stop-and-go behind slower leaders -> genuine congestion the detectors must tolerate.
    cf_active = bool(cfg.traffic_flow and cfg.car_following and net is not None)
    _CF_CELL = max(cfg.idm_lookahead_m, 30.0)
    _lights = bool(cfg.traffic_lights and net is not None)
    _turn = bool(cfg.turn_slowdown and net is not None)
    _half_cycle = max(1.0, cfg.light_cycle_s / 2.0)
    # map-matching: on a grid the roads are the lines x=k*block and y=k*block. A claimed position far
    # from every road is implausible (HD-map check) -> catches lateral/diagonal position offsets.
    _gblk = cfg.grid_block_m
    _gw = (net.w - 1) * _gblk if net is not None else 0.0
    _gh = (net.h - 1) * _gblk if net is not None else 0.0

    def _offroad(x, y):
        if net is None:
            return 0.0
        vx = round(x / _gblk) * _gblk
        dv = abs(x - vx) if (0 <= vx <= _gw and -_gblk <= y <= _gh + _gblk) else 1e9
        hy = round(y / _gblk) * _gblk
        dh = abs(y - hy) if (0 <= hy <= _gh and -_gblk <= x <= _gw + _gblk) else 1e9
        return min(dv, dh)

    def _light_green(nx: int, ny: int, axis_x: bool, t: float) -> bool:
        # checkerboard phase offset so adjacent intersections alternate; each axis gets half the cycle
        offset = ((nx + ny) % 2) * _half_cycle
        x_phase = int((t + offset) // _half_cycle) % 2 == 0
        return x_phase == axis_x

    def _idm_accel(v_cur, v0, gap, v_lead, lead_len, a_max, b_dec):
        if gap == math.inf:
            a = a_max * (1 - (v_cur / max(0.1, v0)) ** 4)
        else:
            gap_b = max(0.5, gap - lead_len)                # leader occupies its own length
            dv = v_cur - v_lead
            s_star = cfg.idm_min_gap + max(0.0, v_cur * cfg.idm_time_headway
                                           + v_cur * dv / (2 * math.sqrt(a_max * b_dec)))
            a = a_max * (1 - (v_cur / max(0.1, v0)) ** 4 - (s_star / gap_b) ** 2)
        return max(-6.0, min(a_max, a))

    def car_follow(active_list, t):
        # snapshot start-of-step positions so the update is order-independent (deterministic)
        snap = {v.vid: (v.cur_x, v.cur_y, v.cur_h, v.cur_v, v.veh_length) for v in active_list}
        buckets: dict = {}
        for v in active_list:
            buckets.setdefault((int(v.cur_x // _CF_CELL), int(v.cur_y // _CF_CELL)), []).append(v.vid)
        for v in active_list:
            vx, vy, vh, _vv, _vl = snap[v.vid]
            hr = math.radians(vh)
            cosh, sinh = math.cos(hr), math.sin(hr)
            cx0, cy0 = int(vx // _CF_CELL), int(vy // _CF_CELL)
            best_gap, best_v, best_len = math.inf, 0.0, cfg.veh_length_m
            for dcx in (-1, 0, 1):
                for dcy in (-1, 0, 1):
                    for wvid in buckets.get((cx0 + dcx, cy0 + dcy), ()):
                        if wvid == v.vid:
                            continue
                        wx, wy, wh, wv, wl = snap[wvid]
                        dx, dy = wx - vx, wy - vy
                        fwd = dx * cosh + dy * sinh
                        if fwd <= 0.0 or fwd > cfg.idm_lookahead_m:
                            continue
                        if abs(-dx * sinh + dy * cosh) > 3.0 or _ang_diff(wh, vh) > 45.0:
                            continue
                        if fwd < best_gap:
                            best_gap, best_v, best_len = fwd, wv, wl
            if _lights:                                  # stop at a red signal on the next intersection
                node, dnode = v.trip.next_node(v.s_pos)
                if node is not None and dnode < cfg.idm_lookahead_m:
                    ni = int(round(node[0] / cfg.grid_block_m))
                    nj = int(round(node[1] / cfg.grid_block_m))
                    axis_x = abs(cosh) >= abs(sinh)      # travelling mostly E-W vs N-S
                    if not _light_green(ni, nj, axis_x, t):
                        stop_gap = max(0.0, dnode - 2.0)  # halt ~2 m before the stop line
                        if stop_gap < best_gap:
                            best_gap, best_v, best_len = stop_gap, 0.0, 0.0
            if _turn and best_gap > 0.5:                 # slow into a sharp bend (curve-speed cap)
                td, bend = v.trip.next_turn(v.s_pos)
                if bend >= cfg.turn_min_angle_deg and td < cfg.idm_lookahead_m and td < best_gap:
                    best_gap, best_v, best_len = td, min(v.desired_speed, cfg.turn_speed_mps), 0.0
            a = _idm_accel(v.cur_v, v.desired_speed, best_gap, best_v, best_len, v.idm_a, v.idm_b)
            v.cur_v = max(0.0, min(v.desired_speed, v.cur_v + a * cfg.dt))
            v.s_pos += v.cur_v * cfg.dt
            if v.s_pos >= v.trip.length:
                v.s_pos = v.trip.length
                v.finish_time = t          # route complete -> despawn next step
            v.cur_x, v.cur_y, v.cur_h = v.trip.at_distance(v.s_pos)
            if v.lane_off:                 # offset into this vehicle's lane (perpendicular to heading)
                hr = math.radians(v.cur_h)
                v.cur_x += v.lane_off * -math.sin(hr)
                v.cur_y += v.lane_off * math.cos(hr)

    # ---- Simulation loop: activate -> car-follow -> pre-pass -> detect -> collude -> revoke ----
    spawn_order = sorted(vehicles, key=lambda v: (v.spawn_time, v.vid))
    spawn_ptr = 0
    active: dict[int, Vehicle] = {}
    t = 0.0                                           # defined even if interrupted before step 0
    # Catch Ctrl-C for a graceful stop (finish the step, then finalize). Only in the main thread; a
    # second Ctrl-C restores the default handler so it hard-aborts. Restored after the loop.
    import signal as _signal
    _prev_sigint = None
    try:
        def _on_sigint(signum, frame):
            if _ABORT["flag"]:                        # second Ctrl-C -> hard abort as usual
                _signal.signal(_signal.SIGINT, _prev_sigint or _signal.SIG_DFL)
                raise KeyboardInterrupt
            _ABORT["flag"] = True
        _prev_sigint = _signal.signal(_signal.SIGINT, _on_sigint)
    except (ValueError, TypeError):                   # not the main thread -> no graceful handling
        _prev_sigint = None
    for step in range(n_steps):
        if PER_STEP_HOOK is not None:
            PER_STEP_HOOK(step)                       # test/telemetry seam (may set _ABORT)
        if _ABORT["flag"]:
            print(f"[interrupted at step {step} -> finalizing partial dataset]", flush=True)
            n_steps = step                            # manifest reflects the steps actually run
            break
        t = step * cfg.dt
        touched_subjects.clear()
        while spawn_ptr < len(spawn_order) and spawn_order[spawn_ptr].spawn_time <= t:
            av = spawn_order[spawn_ptr]; active[av.vid] = av; spawn_ptr += 1
        for vid in [vid for vid, v in active.items() if v.finish_time is not None and t > v.finish_time]:
            active.pop(vid).hist.clear()
            vrng.pop(vid, None)              # a despawned vehicle never transmits again -> free its RNG
        active_list = [active[vid] for vid in sorted(active)]
        if cf_active:
            car_follow(active_list, t)

        # PRE-PASS: every active broadcast this step (real pseudonyms + sybil ghosts)
        broadcasts: list[dict] = []
        for tx in active_list:
            if enforced(tx, t):
                continue
            ps = tx.active_pseudonym(t, cfg.rotate_period_s)
            digest = ps["digest"]
            cert_first_seen.setdefault(digest, t)
            cert_last_seen[digest] = t
            x, y, tspeed, theading = tx.true_state(t)
            mx, my, conf = measure(tx, x, y, t)
            attacking = tx.is_attacker and tx.attack_from <= t <= tx.attack_to
            if attacking and cfg.attack_duty_cycle < 1.0:  # intermittent: falsify only in bursts
                period = max(1e-6, cfg.attack_pulse_period_s)
                phase = ((t - tx.attack_from) / period + tx.pulse_phase) % 1.0
                attacking = phase < cfg.attack_duty_cycle
            if (not attacking) and cfg.gps_jam_rate > 0:
                r = vrng[tx.vid]
                if t >= tx.jam_until and r.random() < cfg.gps_jam_rate:
                    tx.jam_until = t + cfg.gps_jam_dur_s
                if t < tx.jam_until:
                    continue                                      # GNSS outage -> no fix -> goes silent
            msg_count, cg, sig_ok = 1, t, True                    # CAMs; claimed gen time; signature ok
            cvf, cvt = ps["valid_from"], ps["valid_to"]           # cert validity window (MA-visible)
            if attacking:
                cx, cy, cs, ch = attack_claim(tx, t, mx, my, tspeed, theading)
                if tx.attack_type == "DoS":
                    msg_count = cfg.dos_burst                     # flood the channel
                elif tx.attack_type == "DelayedMessages":
                    cg = t - cfg.delay_s                          # stale timestamp
                elif tx.attack_type == "DataReplay":
                    cg = t - 5.0 * cfg.dt                         # replayed frame carries its old gen time
                elif tx.attack_type == "OutOfOrder":
                    cg = t - vrng[tx.vid].uniform(cfg.delay_s, 2.0 * cfg.delay_s)  # non-monotonic gen time
                elif tx.attack_type == "DoSRandom":
                    msg_count = cfg.dos_burst                     # flood + random content (set in claim)
                elif tx.attack_type == "InvalidSignature":
                    sig_ok = False                                # forged / tampered message
                elif tx.attack_type == "ExpiredCert":
                    cvt = t - 5.0                                 # reuse a cert past its validity
                elif tx.attack_type == "NotYetValid":
                    cvf = t + 5.0                                 # present a not-yet-valid cert
            else:
                cx, cy, cs, ch = mx, my, tspeed, theading
            tx.hist.append((cx, cy, cs, ch))
            cert_bad = (t > cvt + 1.0) or (t < cvf - 1.0)
            falsified = attacking and (math.hypot(cx - mx, cy - my) > 1.0 or abs(cs - tspeed) > 1.0
                                       or _ang_diff(ch, theading) > 5.0 or msg_count > 1
                                       or cg < t - 1e-6 or not sig_ok or cert_bad)
            if falsified and tx.onset is None:
                tx.onset = t
            broadcasts.append(dict(veh=tx, digest=digest, cx=cx, cy=cy, cs=cs, ch=ch, conf=conf,
                                   ghost=False, x=x, y=y, falsified=falsified, msg_count=msg_count,
                                   cg=cg, sig_ok=sig_ok, cvf=cvf, cvt=cvt))
            if attacking and tx.attack_type == "Sybil":     # fabricate co-located ghost identities
                sr = vrng[tx.vid]
                for gdig in tx.ghosts:
                    cert_first_seen.setdefault(gdig, t)
                    cert_last_seen[gdig] = t
                    broadcasts.append(dict(veh=tx, digest=gdig, cx=cx + sr.uniform(-1, 1),
                                           cy=cy + sr.uniform(-1, 1), cs=cs, ch=ch, conf=conf,
                                           ghost=True, x=x, y=y, falsified=True, msg_count=1,
                                           cg=t, sig_ok=True, cvf=0.0, cvt=total_time))

        # per-message ground-truth emission sampling (real broadcasts only)
        for b in broadcasts:
            if b["ghost"]:
                continue
            if rng.random() < cfg.emit_sample_prob:
                tx = b["veh"]
                gt_emissions.append(dict(
                    emit_id=f"emt_{stream_counts['emit'] + len(gt_emissions):08d}", t=round(t, 3),
                    true_vehicle_id=f"veh_{tx.vid:03d}", true_x=round(b["x"], 3), true_y=round(b["y"], 3),
                    claimed_x=round(b["cx"], 3), claimed_y=round(b["cy"], 3), claimed_speed=round(b["cs"], 3),
                    pos_conf=round(b["conf"], 3), is_attacker=tx.is_attacker, is_faulty=tx.is_faulty,
                    falsified=bool(b["falsified"]), _visibility=R.ORACLE))

        # sybil co-location: distinct certs at nearly the same point AND heading. Keying on heading
        # too means crossing traffic converging at an intersection (different headings) is not
        # mistaken for a Sybil (whose ghosts copy the attacker's single position + heading).
        cells = Counter((round(b["cx"] / _CELL_M), round(b["cy"] / _CELL_M), int(b["ch"] // 45) % 8)
                        for b in broadcasts)

        # DETECTION pass (receiver-outer): a receiver only hears in-range transmitters, with
        # distance/NLOS/weather/congestion packet loss -> the report graph becomes spatially LOCAL
        # (reporters near the subject) instead of all-to-all, and far-away attackers go unobserved.
        rx_pos = {rx.vid: rx.true_state(t)[:2] for rx in active_list if not enforced(rx, t)}
        wx_loss = WEATHER_RADIO_LOSS.get(cfg.weather, 0.0)
        # spatial index over broadcasts (cell = radio range) so each receiver only tests transmitters
        # in its own + adjacent cells -> reception is O(active x local density), not O(active^2). Cell
        # size = range means the 3x3 neighbourhood provably contains every in-range pair; candidates
        # are re-sorted into broadcast order so packet-loss RNG (hence output) is byte-identical.
        rng_cell = max(cfg.radio_range_m, 1.0)
        bcell: dict = {}
        for bi, b in enumerate(broadcasts):
            bcell.setdefault((int(b["x"] // rng_cell), int(b["y"] // rng_cell)), []).append(bi)
        for rx in active_list:
            if enforced(rx, t):
                continue
            rxx, rxy = rx_pos[rx.vid]
            cx0, cy0 = int(rxx // rng_cell), int(rxy // rng_cell)
            cand = []
            for dcx in (-1, 0, 1):
                for dcy in (-1, 0, 1):
                    cand.extend(bcell.get((cx0 + dcx, cy0 + dcy), ()))
            cand.sort()
            in_range = []
            for bi in cand:
                b = broadcasts[bi]
                if b["veh"].vid == rx.vid:
                    continue
                d = math.hypot(b["x"] - rxx, b["y"] - rxy)
                if d <= cfg.radio_range_m:
                    in_range.append((b, d))
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
                    st = {"h": [(cx, cy, cs, ch, t)], "streak": {}, "touch": step}
                    last_claimed[key] = st
                    det = {k: 0.0 for k in DET_KEYS}
                    ref = st["h"][0]
                else:
                    st["touch"] = step
                    h = st["h"]
                    # LAGGED reference: the newest fix at least detector_lag_s old (else the oldest).
                    # A short lag keeps the path ~straight over the interval, so turns don't read as
                    # position/speed inconsistency, while a single GNSS outlier can't sustain a streak.
                    ref = h[0]
                    for f in h:
                        if t - f[4] >= cfg.detector_lag_s:
                            ref = f
                        else:
                            break
                    det = detectors(ref, cx, cy, cs, ch, t, conf)
                    # headingInconsistency over a ONE-STEP baseline (most recent prior fix): turns are
                    # negligible over one step, so a moving vehicle's bearing matches its claimed heading.
                    prev = h[-1]
                    dprev = math.hypot(cx - prev[0], cy - prev[1])
                    if cs > 3.0 and (t - prev[4]) <= 2.0 * cfg.dt and dprev > max(5.0, 2.5 * conf):
                        bearing = math.degrees(math.atan2(cy - prev[1], cx - prev[0])) % 360.0
                        det["headingInconsistency"] = _ang_diff(ch, bearing) / cfg.heading_threshold_deg
                    h.append((cx, cy, cs, ch, t))
                    while len(h) > 1 and t - h[0][4] > cfg.detector_lag_s + 2 * cfg.dt:
                        h.pop(0)
                # radio-dependent detectors (need the receiver position + per-message metadata)
                det["sybilCoLocation"] = cells[(round(cx / _CELL_M), round(cy / _CELL_M),
                                                int(ch // 45) % 8)] / _SYBIL_MIN
                # a receiver only physically hears in-range transmitters, so a claim placing the
                # sender far BEYOND the radio range is implausible (excess distance / tolerance).
                det["acceptanceRangeThreshold"] = max(0.0, math.hypot(cx - rxx, cy - rxy)
                                                      - cfg.radio_range_m) / cfg.art_max_m
                det["beaconFrequency"] = b["msg_count"] / cfg.freq_max
                det["staleOrReplay"] = max(det.get("staleOrReplay", 0.0), (t - b["cg"]) / cfg.stale_max_s)
                det["mapOffRoad"] = _offroad(cx, cy) / cfg.offroad_tol_m
                det["certValidity"] = 1.5 if (t > b["cvt"] + 1.0 or t < b["cvf"] - 1.0) else 0.0
                # soft constant-velocity (alpha-beta) consistency residual -> fusion feature only
                kf = st.get("kf")
                if kf is None:
                    st["kf"] = (cx, cy, 0.0, 0.0, t)
                    det["kalmanConsistency"] = 0.0
                else:
                    ex, ey, evx, evy, et = kf
                    dtk = max(1e-3, t - et)
                    predx, predy = ex + evx * dtk, ey + evy * dtk
                    rxk, ryk = cx - predx, cy - predy
                    det["kalmanConsistency"] = math.hypot(rxk, ryk) / (2 * cfg.consistency_threshold_m + conf)
                    st["kf"] = (predx + 0.5 * rxk, predy + 0.5 * ryk,
                                evx + 0.3 * rxk / dtk, evy + 0.3 * ryk / dtk, t)
                if not b["sig_ok"]:
                    # signature fails -> the content cannot be trusted, so the plausibility detectors
                    # are moot; the receiver only reports the crypto-verification failure itself.
                    det = {k: 0.0 for k in DET_KEYS}
                    det["signatureVerification"] = 1.5
                for k in DET_KEYS:
                    st["streak"][k] = st["streak"].get(k, 0) + 1 if det.get(k, 0.0) >= 1.0 else 0
                fired = {k: det[k] for k in DET_KEYS if st["streak"].get(k, 0) >= MIN_CONSEC}
                if not fired:
                    continue
                if rng.random() > cfg.report_prob:
                    continue
                reasons = sorted(fired, key=lambda k: -det[k])
                file_report(t, reporter_digest, digest, tx, reasons, det, conf,
                            cx, cy, ref[0], ref[1], malicious=False, sig_valid=b["sig_ok"])

        # COLLUSION pass: colluders file fabricated reports against benign victims. In flow mode
        # victims are chosen dynamically (nearby active benign vehicles); in fixed mode from the list.
        for tx in active_list:
            if not tx.colluder or enforced(tx, t) or not (tx.attack_from <= t <= tx.attack_to):
                continue
            reporter_digest = tx.active_pseudonym(t, cfg.rotate_period_s)["digest"]
            if cfg.traffic_flow:
                txx, txy = rx_pos.get(tx.vid, tx.true_state(t)[:2])
                cand = [v for v in active_list if not v.is_attacker and not enforced(v, t)
                        and math.hypot(rx_pos[v.vid][0] - txx, rx_pos[v.vid][1] - txy) <= cfg.radio_range_m]
                victim_vehicles = cand[:2]
            else:
                victim_vehicles = [vehicles[vv] for vv in tx.victims if vv in active and not enforced(active[vv], t)]
            for victim in victim_vehicles:
                subject_digest = victim.active_pseudonym(t, cfg.rotate_period_s)["digest"]
                if rng.random() > cfg.report_prob:
                    continue
                det = {k: 0.0 for k in DET_KEYS}
                det["positionSpeedInconsistency"] = 1.3         # plausible fabricated evidence
                file_report(t, reporter_digest, subject_digest, victim,
                            ["positionSpeedInconsistency"], det, 5.0, 0.0, 0.0, 0.0, 0.0, malicious=True)

        # Online MA decision: revoke subjects with SUSTAINED, TRUSTED evidence in a RECENT window.
        # (Lifetime-accumulated evidence would let bursty benign faults spread over a long trip add
        # up to a false revocation; a sliding window requires genuinely sustained misbehaviour.)
        for subject_digest in sorted(touched_subjects):
            veh = digest_to_vehicle[subject_digest]
            if veh.vid in revoked_vehicles:
                continue
            ev = [e for e in subj_events[subject_digest] if e[0] >= t - cfg.revoke_window_s]
            subj_events[subject_digest] = ev
            trust = {r for (_tt, r) in ev if trusted(r)}
            secs = {int(tt) for (tt, _r) in ev}
            span = (ev[-1][0] - ev[0][0]) if ev else 0.0
            if (len(trust) >= cfg.report_threshold_k and len(secs) >= cfg.revoke_min_seconds
                    and span >= cfg.revoke_persist_s):
                resolve_and_revoke(veh, subject_digest, t)

        if live_every and step % live_every == 0:
            write_live(active_list, t)
        if cfg.verbose and cfg.traffic_flow and n_steps >= 40 and step % max(1, n_steps // 20) == 0:
            print(f"[flow t={t:.0f}/{total_time:.0f}s] active={len(active)} spawned={spawn_ptr} "
                  f"reports={counters['report']} revoked={len(revoked_vehicles)}", flush=True)
        if stream:
            flush_streams()
            if (step + 1) % cfg.state_prune_every == 0:
                prune_state(step, active)

    if _prev_sigint is not None:                      # restore the caller's Ctrl-C behaviour
        try:
            _signal.signal(_signal.SIGINT, _prev_sigint)
        except (ValueError, TypeError):
            pass

    if stream:
        for fh in (fh_rep, fh_lbl, fh_emit):
            fh.close()

    # ---- attack ground truth (with onset) ----
    gt_attacks = [R.GtAttack(
        attack_id=f"atk_{v.vid}", true_vehicle_id=f"veh_{v.vid:03d}", attack_type=v.attack_type,
        start_time=round(v.attack_from, 3), end_time=round(v.attack_to, 3), attack_onset_time=v.onset,
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
        valid_to=total_time, issuing_pca="PCA-1",
        crl_status=("revoked" if pseudonym_info[d]["veh_vid"] in revoked_vehicles else "active"),
        revocation_time=revoked_vehicles.get(pseudonym_info[d]["veh_vid"]))
        for d, f in cert_first_seen.items()]

    # ---- Write outputs + manifest ----
    if stream:
        # ma_reports / gt_report_labels / gt_emissions were streamed to disk during the loop
        data_files = _write_side_files(cfg, ma_investigations, ma_crl_events, ma_cert_status,
                                       gt_vehicle, gt_idmap, gt_attacks, gt_linkage_rev)
        for rel in ("ma/ma_reports.jsonl", "ground_truth/gt_report_labels.jsonl",
                    "ground_truth/gt_emissions_sample.jsonl"):
            data_files[rel] = os.path.join(cfg.out_dir, rel)
        n_reports, n_gt_reports = stream_counts["reports"], stream_counts["labels"]
    else:
        data_files = _write_outputs(cfg, ma_reports, ma_investigations, ma_crl_events, ma_cert_status,
                                    gt_vehicle, gt_idmap, gt_attacks, gt_report_labels, gt_linkage_rev,
                                    gt_emissions)
        n_reports, n_gt_reports = len(ma_reports), len(gt_report_labels)
    data_digest = _data_digest(cfg.out_dir, data_files)
    _write_manifest(cfg, data_files, data_digest,
                    counts=dict(vehicles=len(vehicles), reports=n_reports,
                                investigations=len(ma_investigations), revoked=len(revoked_vehicles)))

    return RunResult(out_dir=cfg.out_dir, n_vehicles=len(vehicles), n_reports=n_reports,
                     n_investigations=len(ma_investigations), n_revoked=len(revoked_vehicles),
                     revoked_cert_digests=sorted(revoked_digests), data_digest=data_digest,
                     counts=dict(cert_status=len(ma_cert_status), gt_reports=n_gt_reports))


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


def _write_side_files(cfg, ma_invest, ma_crl, ma_cert, gt_veh, gt_idmap,
                      gt_attacks, gt_linkage_rev) -> dict[str, str]:
    """Write everything EXCEPT the three big tables that flow mode streamed to disk directly."""
    os.makedirs(os.path.join(cfg.out_dir, "ma"), exist_ok=True)
    os.makedirs(os.path.join(cfg.out_dir, "ground_truth"), exist_ok=True)
    files = {
        "ma/ma_investigations.jsonl": sorted(ma_invest, key=lambda r: (r.opened_time, r.case_id)),
        "ma/ma_crl_events.jsonl": sorted(ma_crl, key=lambda r: (r.issue_time, r.crl_id)),
        "ma/ma_cert_status.jsonl": sorted(ma_cert, key=lambda r: r.cert_digest),
        "ground_truth/gt_vehicle.jsonl": sorted(gt_veh, key=lambda r: r.true_vehicle_id),
        "ground_truth/gt_identity_map.jsonl": sorted(gt_idmap, key=lambda r: r.pseudonym_cert_digest),
        "ground_truth/gt_attacks.jsonl": sorted(gt_attacks, key=lambda r: r.attack_id),
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
    import sys as _sys
    # phase 1: read --preset early so it can seed the parser defaults (any other flag still overrides)
    _pre = argparse.ArgumentParser(add_help=False)
    _pre.add_argument("--preset", choices=list(CLI_PRESETS))
    _preargs, _ = _pre.parse_known_args(argv if argv is not None else _sys.argv[1:])
    p = argparse.ArgumentParser(description="Run the SCMS closed-loop reference pipeline.")
    p.add_argument("--preset", choices=list(CLI_PRESETS),
                   help="named scenario preset (" + ", ".join(CLI_PRESETS) + "); flags still override it")
    p.add_argument("--seed", type=int, default=1001)
    p.add_argument("--vehicles", type=int, default=12)
    p.add_argument("--steps", type=int, default=40)
    p.add_argument("--attacker-pct", type=float, default=0.0)
    p.add_argument("--attack-intensity", type=float, default=1.0, help="scale falsification magnitude (subtle<1)")
    p.add_argument("--attack-duty-cycle", type=float, default=1.0,
                   help="<1: attacker falsifies only in bursts (intermittent/pulsed, evades revocation)")
    p.add_argument("--attack-pulse-period", type=float, default=20.0, help="pulse cycle length (s) when duty<1")
    p.add_argument("--attack-delay-jitter", type=float, default=0.0,
                   help="spread attacker onset times by up to this many seconds (realistic varied onset)")
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
    # long-running traffic flow
    p.add_argument("--flow", action="store_true", help="traffic-flow mode: vehicles spawn/despawn over time")
    p.add_argument("--duration", type=float, default=0.0, help="flow: sim length in seconds")
    p.add_argument("--arrival-rate", type=float, default=2.0, help="flow: mean vehicles spawned per second")
    p.add_argument("--road", default="linear", choices=["linear", "grid"], help="road network model")
    p.add_argument("--grid", type=int, default=6, help="grid road network dimension (grid x grid)")
    p.add_argument("--grid-block", type=float, default=120.0, help="grid block spacing (m)")
    p.add_argument("--lanes", type=int, default=1, help="parallel lanes per road (overtaking; reduces gridlock)")
    p.add_argument("--od-model", default="uniform", choices=["uniform", "gravity"],
                   help="trip destination law: uniform | gravity (realistic distance-decay trip lengths)")
    p.add_argument("--od-gravity-scale", type=float, default=2.0,
                   help="gravity OD: hop decay scale (smaller -> shorter trips)")
    p.add_argument("--turn-speed", type=float, default=6.0,
                   help="cornering: speed cap (m/s) through a sharp bend when --turn-slowdown is on")
    p.add_argument("--boundary-origins", action="store_true",
                   help="trips originate at the grid perimeter (realistic network sources/sinks)")
    p.add_argument("--demand", default="uniform", choices=["uniform", "rush", "night"],
                   help="time-varying arrival-demand profile")
    p.add_argument("--fleet", default="mixed", help="'mixed' or a single vehicle class (car/truck/bus/motorcycle)")
    p.add_argument("--live-interval", type=float, default=0.0, help="write live_state.json every N sim-seconds (GUI map)")
    p.add_argument("--no-car-following", action="store_true", help="disable IDM car-following")
    p.add_argument("--turn-slowdown", action="store_true", help="slow into sharp grid corners (realer, harder)")
    p.add_argument("--traffic-lights", action="store_true", help="signalized intersections (grid)")
    p.add_argument("--gps-jam-rate", type=float, default=0.0, help="per-step prob a benign vehicle loses GNSS fix")
    p.add_argument("--max-total-vehicles", type=int, default=0, help="flow: cap total spawns (0=unlimited)")
    p.add_argument("--featurize", action="store_true", help="build ML tables after generation")
    p.add_argument("--grid-h", type=int, default=0, help="grid height (0 = square, = --grid)")
    p.add_argument("--config", default=None,
                   help="load config from a JSON file (raw config dict or a run's manifest.json) and "
                        "replay it exactly; --out and --featurize still apply. Ignores other flags.")
    p.add_argument("--dump-config", default=None,
                   help="write the effective config to this JSON path, then run as usual")
    p.add_argument("--dump-config-schema", default=None,
                   help="write the config field schema (name/type/default) to this JSON path and exit")
    p.add_argument("--list-presets", action="store_true",
                   help="print the named scenario presets and their settings, then exit")
    p.add_argument("--check-config", default=None,
                   help="validate a config/manifest JSON (no run); exit 0 if valid, 1 with the error")
    p.add_argument("--out", default=None)
    if _preargs.preset:                              # preset seeds defaults; explicit flags override
        p.set_defaults(**CLI_PRESETS[_preargs.preset])
    args = p.parse_args(argv)
    if args.list_presets:                             # print preset names + settings and exit
        for name, kw in CLI_PRESETS.items():
            print(f"{name}:")
            print("   " + ", ".join(f"{k}={v}" for k, v in sorted(kw.items())))
        return 0
    if args.check_config:                             # validate a config/manifest JSON and exit
        try:
            with open(args.check_config, encoding="utf-8") as fh:
                cfg = config_from_dict(json.load(fh))
            validate_config(cfg)
        except (OSError, ValueError, TypeError) as e:
            print(f"config INVALID: {type(e).__name__}: {e}", file=_sys.stderr)
            return 1
        print(f"config OK: {len(cfg.__dict__)} fields, seed={cfg.seed}, out_dir={cfg.out_dir}")
        return 0
    if args.dump_config_schema:                       # emit the config field schema and exit (no run)
        with open(args.dump_config_schema, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(config_schema(), fh, indent=2, sort_keys=True)
        print(f"config schema ({len(config_schema())} fields) written to "
              f"{os.path.abspath(args.dump_config_schema)}")
        return 0
    if args.config:                                  # replay a saved config exactly
        with open(args.config, encoding="utf-8") as fh:
            cfg = config_from_dict(json.load(fh))
        if args.out:
            cfg.out_dir = args.out
        cfg.verbose = True
        res = run_pipeline(cfg)
        _emit_result(res, args.featurize)
        if args.dump_config:
            _dump_config(cfg, args.dump_config)
        return 0
    cfg = PipelineConfig(seed=args.seed, n_vehicles=args.vehicles, n_steps=args.steps,
                         attacker_pct=args.attacker_pct, attack_intensity=args.attack_intensity,
                         attack_duty_cycle=args.attack_duty_cycle,
                         attack_pulse_period_s=args.attack_pulse_period,
                         attack_delay_jitter_s=args.attack_delay_jitter,
                         faulty_pct=args.faulty_pct,
                         weather=args.weather, rotate_period_s=args.rotate_period,
                         collude_pct=args.collude_pct, victim_pct=args.victim_pct,
                         sybil_ghosts=args.sybil_ghosts, ma_defense=not args.no_ma_defense,
                         radio_range_m=args.radio_range, packet_loss_base=args.packet_loss,
                         nlos_loss=args.nlos, chan_capacity=args.chan_capacity,
                         traffic_flow=args.flow, duration_s=args.duration, arrival_rate=args.arrival_rate,
                         road_network=("grid" if (args.flow and args.road == "linear") else args.road),
                         grid_w=args.grid, grid_h=(args.grid_h or args.grid), grid_block_m=args.grid_block,
                         n_lanes=args.lanes,
                         demand_profile=args.demand, od_model=args.od_model,
                         od_gravity_scale=args.od_gravity_scale, boundary_origins=args.boundary_origins,
                         car_following=not args.no_car_following,
                         turn_slowdown=args.turn_slowdown, turn_speed_mps=args.turn_speed,
                         fleet=args.fleet, live_interval_s=args.live_interval,
                         traffic_lights=args.traffic_lights, gps_jam_rate=args.gps_jam_rate,
                         max_total_vehicles=args.max_total_vehicles, verbose=True,
                         out_dir=(args.out or "datasets/poc_run"))
    res = run_pipeline(cfg)
    _emit_result(res, args.featurize)
    if args.dump_config:
        _dump_config(cfg, args.dump_config)
    return 0


def _emit_result(res, featurize: bool) -> None:
    print(f"vehicles={res.n_vehicles} reports={res.n_reports} "
          f"investigations={res.n_investigations} revoked={res.n_revoked}")
    print(f"revoked_cert_digests={res.revoked_cert_digests[:8]}")
    print(f"data_digest={res.data_digest}")
    try:                                             # immediate detection-quality feedback (top layer)
        from ..datagen import validate as _val
        s, _ = _val.validate(res.out_dir)
        lat = (s.get("detection_latency_s") or {}).get("median_s")
        print(f"detection: precision={s.get('precision')} recall={s.get('recall')} "
              f"attackers={s.get('attackers')} revoked={s.get('revoked')}"
              + (f" latency_med={lat}s" if lat is not None else ""))
    except Exception:                                # never let a summary print break the run
        pass
    if featurize:
        from ..datagen import featurize as _feat
        _feat.build(res.out_dir)
        print("featurized ml/ tables written")
    print(f"outputs in {os.path.abspath(res.out_dir)}")


def _dump_config(cfg: PipelineConfig, path: str) -> None:
    """Serialize the effective config to JSON (same shape config_from_dict reads back)."""
    d = {k: (list(v) if isinstance(v, tuple) else v) for k, v in cfg.__dict__.items()}
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(d, fh, indent=2, sort_keys=True)
    print(f"effective config written to {os.path.abspath(path)}")


if __name__ == "__main__":
    raise SystemExit(main())
