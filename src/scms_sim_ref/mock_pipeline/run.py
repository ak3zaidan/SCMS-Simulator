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

# DEFAULT attack selection (round-robin over attackers). Spans the position/speed/heading/timing/
# stealth/identity/credential families; index 0 (ConstPos) is a strong, always-detectable attack so a
# single-attacker run always closes the loop. This tuple is the FROZEN default: the golden data_digest
# depends on its exact contents+order, so it MUST NOT change. The "combined" family (below) is
# deliberately NOT here so the default dataset is byte-identical -- it is opt-in only.
ATTACK_CATALOG = (
    "ConstPos", "ConstPosOffset", "RandomPos", "Teleport", "SineWavePos",
    "ConstSpeedOffset", "RandomSpeed", "StopAndGo",
    "ReversedHeading", "HeadingOffset", "DataReplay",
    "SlowDrift", "AlongRoadOffset", "Sybil",
    "DoS", "DelayedMessages", "InvalidSignature", "ExpiredCert", "NotYetValid",
    "OutOfOrder", "DoSRandom",
)
# The "combined" family (audit gap #9): a single attacker falsifies MULTIPLE fields mutually
# inconsistently, so it trips several detector families at once. attack_claim() renders these, and
# featurize._ATTACK_FAMILY maps them to family "combined". They are OPT-IN ONLY -- excluded from
# ATTACK_CATALOG (the default round-robin) so the default data_digest is byte-identical -- and are
# produced only when the user explicitly asks for them via attack_types=(...), attack_mix, or a single
# attack_type.
COMBINED_ATTACKS = (
    "Disruptive", "PosSpeedInconsistent", "PosHeadingInconsistent", "EventualStop",
)
# The "identity-spoofing" family (VRU-impersonation gap): a moving VEHICLE fraudulently self-declares
# station_type="vru" on its beacon so a receiver grants it the VRU detector exemptions (off-road +
# vehicle-kinematic checks) -- while it otherwise drives like a vehicle. Both members declare vru and
# are caught by the vruImpersonation detector; they differ in which VRU-implausibility they exhibit:
#   * "VruImpersonation"  -- drives HONESTLY but at VEHICLE speed (claimed speed >= the VRU bound), so
#     the detector's speed arm fires.
#   * "VruPositionSpoof"  -- audit GAP #6: CLAIMS a VRU-plausible slow speed (staying UNDER the speed
#     bound) while TELEPORTING its claimed position, a jump no genuine (slow, smoothly-moving) VRU
#     could make, so the detector's position-plausibility arm fires. This is the slow-and-falsifying
#     impersonator that the speed arm alone could not catch.
# Like COMBINED_ATTACKS the family is OPT-IN ONLY -- excluded from ATTACK_CATALOG (the default
# round-robin) so the default data_digest is byte-identical -- and produced only when explicitly
# requested via attack_types / attack_mix / attack_type. featurize._ATTACK_FAMILY folds both into the
# "identity" family (alongside Sybil).
IDENTITY_SPOOF_ATTACKS = (
    "VruImpersonation",
    "VruPositionSpoof",
)
# The "event-message" (DENM) family: a DECENTRALIZED EVENT MESSAGE announces a road hazard/event
# (emergency electronic brake light, stationary vehicle, ...). A "FakeHazard" attacker emits DENMs
# announcing a hazard NOT backed by its own kinematics -- a phantom emergency brake while it is in
# fact cruising -- to induce nearby vehicles to brake/reroute. Like COMBINED_ATTACKS /
# IDENTITY_SPOOF_ATTACKS it is OPT-IN ONLY (excluded from ATTACK_CATALOG so the default data_digest is
# byte-identical) and produced only when explicitly requested via attack_types / attack_mix /
# attack_type. It requires the DENM layer, so it turns that layer on even when denm_rate==0 (mirroring
# how VruImpersonation enables the station_type machinery). featurize._ATTACK_FAMILY folds it into the
# new "event" family; the denmPlausibility detector catches it.
DENM_ATTACKS = (
    "FakeHazard",
)
# Every attack type the engine can RENDER (default catalog + opt-in combined + opt-in identity-spoof +
# opt-in event/DENM). Used to validate the opt-in selectors (attack_mix / attack_type) without
# widening the default set.
KNOWN_ATTACK_TYPES = ATTACK_CATALOG + COMBINED_ATTACKS + IDENTITY_SPOOF_ATTACKS + DENM_ATTACKS

# --- DENM (event-message) layer tunables (consulted only when the DENM layer is enabled) ---
# denm_rate is expressed as expected DENMs per vehicle per 100 seconds of eligibility (a RATE, not a
# probability); the per-step emission probability is denm_rate * dt / 100. 0 => no benign DENMs.
DENM_RATE_WINDOW_S = 100.0
# A FakeHazard attacker still needs a non-zero emission rate even if the benign rate (denm_rate) is 0,
# so the attack is self-contained (selecting it never yields an is_attacker with no falsification).
DENM_FAKE_FALLBACK_RATE = 40.0        # fake DENMs / attacker / 100 s when denm_rate == 0
# A genuine emergency-brake / stationary-vehicle DENM is emitted by a sender that has actually slowed
# to (at most) this speed, so a real event's CLAIMED sender speed sits at/below it. The plausibility
# detector reads the noise-free claimed speed, so real DENMs never trip it (margin to the threshold).
DENM_BENIGN_MAX_SPEED_MPS = 4.0       # benign DENM trigger: sender speed at/below this (post-brake/stop)
DENM_DECEL_TRIG_MPS2 = 2.5            # ...reached via a hard deceleration of at least this magnitude
# denmPlausibility fires when a brake/stationary hazard's sender CLAIMS a speed above this: a beacon
# that announces "I am emergency-braking here" while still moving at vehicle speed contradicts itself.
# Set above DENM_BENIGN_MAX_SPEED_MPS so benign (slow/stopped) senders stay below the firing line.
DENM_IMPLAUSIBLE_SPEED_MPS = 6.0
# Audit GAP #5 event-type cross-check. A GENUINE emergencyElectronicBrakeLight sender has actually
# braked to a near stop: the benign trigger fires ONLY at speed <= DENM_BENIGN_MAX_SPEED_MPS, and the
# claimed speed is the (noise-free) true speed -- so a real brake DENM's claimed speed is at most that
# post-brake bound. A brake DENM whose sender is STILL MOVING NORMALLY (claimed speed above the bound)
# therefore contradicts the very event it announces, EVEN when it stays under the generic 6 m/s
# DENM_IMPLAUSIBLE_SPEED_MPS line (the recall gap: a phantom brake from a sender crawling in
# congestion). This brake-specific bound sits just ABOVE DENM_BENIGN_MAX_SPEED_MPS, so a real brake
# (<= that bound, with margin) is NEVER flagged while a still-moving phantom is. Only emergency-brake
# DENMs use it; stationary/other hazards keep the generic bound.
DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS = DENM_BENIGN_MAX_SPEED_MPS + 0.5

# A self-declared VRU (pedestrian/cyclist) travelling faster than this is not plausibly a VRU -- a fast
# cyclist / e-bike tops out ~8-10 m/s -- so a beacon that DECLARES vru while moving above it is a
# vehicle impersonating a VRU. Used by the vruImpersonation detector; genuine VRUs (~vru_speed_mps,
# a few m/s) stay far below it.
VRU_MAX_PLAUSIBLE_SPEED_MPS = 10.0
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


def _parse_fleet_mix(s: str) -> dict | None:
    """Parse 'car:0.6,truck:0.3,bus:0.1' -> {name: weight} in canonical VEHICLE_TYPES order (so the
    sampling draw is order-stable). Empty/blank -> None (use the default mixed weights). Raises on a
    bad class name or non-positive weight."""
    if not s or not s.strip():
        return None
    parsed = {}
    for part in s.split(","):
        name, _, w = part.strip().partition(":")
        name = name.strip()
        if name not in VEHICLE_TYPES:
            raise ValueError(f"fleet_mix has unknown class {name!r} (valid: {sorted(VEHICLE_TYPES)})")
        wt = float(w)
        if wt <= 0:
            raise ValueError(f"fleet_mix weight for {name!r} must be > 0 (got {wt})")
        parsed[name] = wt
    if not parsed:
        raise ValueError(f"fleet_mix parsed to nothing: {s!r}")
    return {n: parsed[n] for n in VEHICLE_TYPES if n in parsed}   # canonical order


def _parse_attack_mix(s: str) -> dict | None:
    """Parse 'ConstPos:0.6,Sybil:0.4' -> {type: weight} in canonical KNOWN_ATTACK_TYPES order. Empty ->
    None (default round-robin). Raises on an unknown attack type or non-positive weight. The opt-in
    "combined" family is accepted here (KNOWN_ATTACK_TYPES = catalog + combined), so it is reachable via
    attack_mix without being part of the default selection."""
    if not s or not s.strip():
        return None
    parsed = {}
    for part in s.split(","):
        name, _, w = part.strip().partition(":")
        name = name.strip()
        if name not in KNOWN_ATTACK_TYPES:
            raise ValueError(f"attack_mix has unknown type {name!r}")
        wt = float(w)
        if wt <= 0:
            raise ValueError(f"attack_mix weight for {name!r} must be > 0 (got {wt})")
        parsed[name] = wt
    if not parsed:
        raise ValueError(f"attack_mix parsed to nothing: {s!r}")
    return {n: parsed[n] for n in KNOWN_ATTACK_TYPES if n in parsed}   # canonical order


def _weighted_pick(rng, weights: dict) -> str:
    """Deterministic weighted choice over {name: weight} (order-stable) using one rng draw."""
    tot = sum(weights.values()) or 1.0
    r, acc = rng.random(), 0.0
    for name, wt in weights.items():
        acc += wt / tot
        if r <= acc:
            return name
    return next(iter(weights))


def _pick_vehicle_type(rng, fleet: str, weights: dict | None = None) -> str:
    if fleet != "mixed":
        return fleet if fleet in VEHICLE_TYPES else "car"
    w = weights or {n: VEHICLE_TYPES[n]["weight"] for n in VEHICLE_TYPES}
    tot = sum(w.values()) or 1.0
    r, acc = rng.random(), 0.0
    for name, wt in w.items():
        acc += wt / tot
        if r <= acc:
            return name
    return next(iter(w))
_SYBIL_MIN = 4        # distinct certs at NEARLY the same point before sybilCoLocation fires
_CELL_M = 3.0         # co-location cell: small enough that a bumper-to-bumper queue (~7 m spacing)
_RSU_VID_BASE = 10_000_000   # Road-Side Unit vids start here (never collide with vehicle vids)
                      # never fills it, but tightly co-located Sybil ghosts (spoofing one point) do
# log-distance radio model (opt-in; radio_model="logdistance"): a favourable shadow can pull a link
# past radio_range_m, so the candidate window widens to the distance where the MEAN received power is
# RADIO_CAP_SIGMA shadow-std below sensitivity (a link closing beyond that is astronomically rare),
# bounded to RADIO_CAP_MAX_MULT * range so the spatial-bucket search stays O(local). Disc uses neither.
RADIO_CAP_SIGMA = 4.0        # candidate cap headroom in shadow standard deviations
RADIO_CAP_MAX_MULT = 6.0     # hard ceiling on cap / range (bounds the cell-search neighbourhood)


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
    attack_type: str = ""                # single-type narrowing selector; "" (sentinel) = unset ->
                                         # attack_types (the full catalog) is used. Any real value
                                         # (including "ConstPos") narrows the run to that one type.
    attack_types: tuple[str, ...] = ATTACK_CATALOG
    attack_start: float = 5.0
    attack_end: float = 60.0
    attack_intensity: float = 1.0        # scales falsification magnitudes (subtle <1 .. blatant >1)
    attack_mix: str = ""                 # per-type weights, e.g. "ConstPos:0.6,Sybil:0.4"
                                         # (empty = round-robin over attack_types)
    attack_duty_cycle: float = 1.0       # <1: attacker falsifies only in bursts (evades sustained-
                                         # evidence revocation); fraction of each pulse period "on"
    attack_pulse_period_s: float = 20.0  # length of one on/off pulse cycle when duty_cycle < 1
    # CRL-aware evasion: an attacker that WATCHES the public CRL and lies low after an accomplice is
    # revoked -- a realistic feedback-aware adversary that starves the detector of sustained evidence.
    crl_aware_pct: float = 0.0           # fraction of ATTACKERS that monitor the (public) CRL
    crl_dormant_s: float = 45.0          # dormancy after observing a new revocation (broadcast honestly)
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
    # per-vehicle GNSS quality spread: quality = floor + Exp(lambda) drawn once per vehicle at spawn.
    # The floor is the best achievable quality; the exponential tail gives a few vehicles markedly
    # worse fixes (a larger noise scale). Defaults reproduce the historic 0.5 + expovariate(1.2) draw.
    gps_quality_floor: float = 0.5       # best-case per-vehicle GNSS quality (noise-scale floor)
    gps_quality_lambda: float = 1.2      # rate of the exponential quality tail (smaller = heavier tail)
    faulty_pct: float = 0.05             # malfunctioning-sensor (non-attacker) fraction
    faulty_bias_mult: float = 5.0        # faulty = large SUSTAINED bias (smooth, self-consistent)
    weather: str = "clear"
    # --- detection / revocation ---
    consistency_threshold_m: float = 5.0
    heading_threshold_deg: float = 35.0
    detector_lag_s: float = 1.5          # compare each fix to one ~this old (robust to turns/outliers)
    # detector operating point (strictness of the motion + Sybil detectors). Defaults reproduce the
    # historic hardcoded values, so the DEFAULT config stays byte-identical.
    detector_z_threshold: float = 3.0    # motion-residual firing point: a residual must exceed ~this
                                         # many broadcast-uncertainty sigmas to count as a violation
    detector_min_consec: int = 2         # consecutive per-detector violations required before a reason fires
    sybil_min_certs: int = _SYBIL_MIN    # distinct co-located certs (same cell+heading) before
                                         # sybilCoLocation reaches its firing score of 1.0
    sybil_cell_m: float = _CELL_M        # sybil co-location cell size (m): certs are binned to this grid
                                         # (used BOTH when building the cell counts and on lookup)
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
    # opt-in log-distance path-loss + log-normal shadowing radio model. "disc" (default) = the hard
    # d<=range cutoff above, BYTE-IDENTICAL to today. "logdistance" replaces that cutoff with a SOFT
    # probabilistic range: mean received power = -10*n*log10(d/range) dB relative to the sensitivity
    # threshold (so it is 0 dB exactly at d==range -> median range == radio_range_m), plus a per-link
    # log-normal shadowing draw (dB); a link is HEARD iff received power >= sensitivity(+margin). The
    # existing packet_loss_base/nlos_loss/congestion/weather losses then compose ON TOP as per-packet
    # drops on the links that do close. The three tunables below are consulted ONLY when logdistance.
    radio_model: str = "disc"            # "disc" (hard range) | "logdistance" (soft path-loss+shadowing)
    pathloss_exponent: float = 2.7       # log-distance exponent n (urban ~2.7-3.5; free space 2.0)
    shadowing_sigma_db: float = 4.0      # log-normal shadowing std (dB); 0 -> near-hard cutoff at range
    rx_sensitivity_margin_db: float = 0.0  # + shrinks / - extends the effective range vs radio_range_m
    # candidate-window cap for logdistance reception (see the RADIO_CAP_* module notes). Consulted ONLY
    # when radio_model=="logdistance"; the "disc" default takes neither, so it is byte-identical regardless.
    radio_cap_sigma: float = RADIO_CAP_SIGMA        # candidate cap headroom in shadow standard deviations
    radio_cap_max_mult: float = RADIO_CAP_MAX_MULT  # hard ceiling on cap / range (bounds the cell search)
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
    # --- vulnerable road users (VRUs: pedestrians/cyclists carrying VAM-broadcasting devices) ---
    # OPT-IN benign actor class. VRUs move slowly on/near the network but are NOT bound to vehicle
    # car-following (IDM); their beacon SELF-DECLARES station_type=vru (an MA-visible field), and a
    # receiver that sees that declaration suppresses the off-road + vehicle-kinematic detectors (a VRU
    # is legitimately off the road centerline and moves slowly/erratically). vru_pct=0 (the default)
    # spawns none, draws NO extra RNG, and is BYTE-IDENTICAL. See make_vru + the detection-pass note.
    vru_pct: float = 0.0                  # fraction of spawned ACTORS that are VRUs (0 = none)
    vru_speed_mps: float = 1.8           # VRU travel speed (~1.4 walking .. ~5 cycling)
    # a beacon DECLARING station_type=vru but CLAIMING a speed at/above this is a vehicle impersonating
    # a VRU (a fast e-bike tops out ~8-10 m/s); drives the vruImpersonation speed arm. Genuine VRUs
    # (~vru_speed_mps) stay far below it. Consulted only when station types are in play (byte-identical
    # otherwise). Default reproduces the historic VRU_MAX_PLAUSIBLE_SPEED_MPS.
    vru_max_plausible_speed_mps: float = VRU_MAX_PLAUSIBLE_SPEED_MPS
    # --- DENM (event-message) layer (opt-in; default OFF -> byte-identical) ---
    # DENMs are event-triggered messages (emergency electronic brake light, stationary vehicle, ...),
    # distinct from the periodic CAM/beacon. A vehicle that experiences a REAL trigger (hard decel to a
    # near-stop, or being stationary) occasionally broadcasts a signed DENM announcing that real event,
    # at rate denm_rate. denm_rate is expected DENMs per vehicle per 100 s (a RATE, not a probability);
    # 0 (the default) => no DENMs are ever emitted, NO extra RNG is drawn, and the dataset is
    # BYTE-IDENTICAL. The opt-in "FakeHazard" attack turns the layer on even at denm_rate=0 (it emits
    # phantom DENMs at a fallback rate) so the attack is self-contained. See the broadcast pre-pass.
    denm_rate: float = 0.0               # expected benign DENMs per vehicle per 100 s (0 = none)
    # DENM plausibility / trigger thresholds (consulted only when the DENM layer is enabled -> the
    # default path is byte-identical). Defaults reproduce the historic module constants. The brake-
    # specific implausibility bound stays DERIVED at the use site as denm_benign_max_speed_mps + 0.5
    # (kept just above the benign bound, so a real brake DENM is never flagged) -- no separate field.
    denm_rate_window_s: float = DENM_RATE_WINDOW_S       # window (s) that denm_rate is expressed per
    denm_fake_fallback_rate: float = DENM_FAKE_FALLBACK_RATE  # fake DENMs/attacker/window when denm_rate==0
    denm_benign_max_speed_mps: float = DENM_BENIGN_MAX_SPEED_MPS  # benign DENM trigger: sender speed
                                         # at/below this (post-brake/stop); also the base of the derived
                                         # brake-implausible bound (this + 0.5)
    denm_decel_trig_mps2: float = DENM_DECEL_TRIG_MPS2   # ...reached via a hard decel of at least this
    denm_implausible_speed_mps: float = DENM_IMPLAUSIBLE_SPEED_MPS  # denmPlausibility fires when a
                                         # hazard's sender CLAIMS a speed above this (generic bound)
    road_network: str = "linear"         # linear | grid | ring | spider | custom (see custom_network)
    grid_w: int = 6                      # grid width | ring nodes | spider arms
    grid_h: int = 6                      # grid height | spider rings
    grid_block_m: float = 120.0
    grid_dropout: float = 0.0            # remove this fraction of grid roads (irregular/incomplete grid)
    custom_network: str = ""             # road_network="custom": JSON {"nodes":[[x,y]...m],
                                         # "edges":[[a,b]...]} -- an arbitrary user/AI-designed map
    n_lanes: int = 1                     # >1: parallel lanes per road -> overtaking, less gridlock
    lane_width_m: float = 3.5
    trip_speed_min: float = 8.0
    trip_speed_max: float = 18.0
    fleet: str = "mixed"                 # "mixed" (car/moto/truck/bus) | a single type name
    fleet_mix: str = ""                  # custom mixed composition, e.g. "car:0.6,truck:0.3,bus:0.1"
                                         # (only when fleet="mixed"; empty = default weights)
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
    # per-edge speed hierarchy (highway vs residential). OFF by default -> byte-identical output.
    # GRID: every arterial_every-th row & column is an arterial posted at arterial_speed_mps; all
    # other (local) roads are posted at local_speed_mps (0 for either tier = uncapped, i.e. the
    # vehicle's desired speed). RING: arterial_speed_mps caps the whole ring (no rows/columns to
    # tier), and arterial_every/local_speed_mps do not apply. arterial_every == 0 disables the grid
    # rule; arterial_speed_mps == 0 disables the ring cap.
    arterial_every: int = 0              # grid: spacing (in grid lines) of arterial roads (0 = off)
    arterial_speed_mps: float = 0.0      # arterial cap (grid) / whole-ring cap (ring); 0 = uncapped
    local_speed_mps: float = 0.0         # grid: local (non-arterial) road cap; 0 = uncapped
    # cornering: vehicles slow into sharp turns rather than taking 90-degree grid corners at full
    # speed -> realistic speed dips at intersections (opt-in; car-following + grid only). Off by
    # default: it makes benign kinematics harder to separate from attacks (more realistic, but it
    # shifts the tuned operating point), so enable it deliberately for harder/realer corpora.
    turn_slowdown: bool = False
    turn_speed_mps: float = 6.0          # speed cap through a sharp bend (m/s); ~21 km/h
    turn_min_angle_deg: float = 40.0     # only bends sharper than this are slowed
    # discretionary lane changes (MOBIL-style; audit gap #7). OFF by default -> byte-identical output.
    # Only meaningful with n_lanes>1 AND traffic_flow (routed car-following): a vehicle held up in its
    # own lane will move to an adjacent lane when a MOBIL incentive holds and the target lane offers a
    # safe gap, then its perpendicular offset transitions SMOOTHLY over lane_change_time_s -> a real
    # lateral velocity + brief heading swing. This is the dominant benign source of the HARDEST
    # misbehaviour false positives (sudden lateral move + heading deviation that mimics a position/
    # heading lie), so enabling it adds that benign-difficulty tail. validate_config requires the
    # n_lanes>1 + traffic_flow combination when it is on; when off, no RNG is drawn and the world is
    # unchanged. The lateral maneuver is INTENTIONALLY visible to the detectors (benign FP pressure);
    # the MA's sustained-evidence gate is what keeps a single lane change from causing a revocation.
    lane_changes: bool = False
    lane_change_time_s: float = 2.5      # smooth lateral transition duration (s); shapes the transient
    lane_change_politeness: float = 0.2  # MOBIL politeness: weight on OTHER vehicles' accel change
    lane_change_threshold: float = 0.2   # MOBIL incentive threshold (m/s^2) to commit to a change
    # gap-acceptance / yielding at UNSIGNALIZED intersections (audit gap #7 remainder). OFF by default
    # -> byte-identical output. At an intersection with no signal governing a vehicle this step, crossing
    # streams are otherwise mutually invisible (the IDM leader search rejects >45-deg heading differences),
    # so two cross streams drive THROUGH each other with no yield. When ON, a vehicle approaching an
    # unsignalized node YIELDS to conflicting cross-traffic that outranks it under a deterministic
    # first-come rule -- the vehicle currently CLOSEST to the node has priority, ties broken by vehicle id.
    # A lower-priority vehicle treats the node as a virtual stopped leader (the SAME stop mechanism traffic
    # lights use) until the higher-priority conflicting vehicle has cleared (once it passes the node it is
    # no longer a claimant). The rule is computed from the START-OF-STEP snapshot (order-independent) and
    # priority is a strict total order, so the yield relation is acyclic -> the top vehicle always makes
    # progress -> no gridlock/starvation. Only meaningful with traffic_flow + a routed network, and it
    # governs UNsignalized nodes only: with traffic_lights ON every node is signalized, so gap_acceptance
    # defers entirely to the signal (they coexist, but gap primarily targets the traffic_lights=false
    # case). Produces realistic slowing at uncontrolled junctions (benign speed transients) that the MA's
    # sustained-evidence gate absorbs without false revocations. No RNG is drawn on either path.
    gap_acceptance: bool = False
    # time-varying demand (rush hour / night) + origin-destination bias
    demand_profile: str = "uniform"      # "uniform" | "rush" | "night"
    od_model: str = "uniform"            # trip destination law: "uniform" | "gravity" (distance-decay)
    od_gravity_scale: float = 2.0        # gravity: hop decay scale (smaller -> shorter trips)
    boundary_origins: bool = False       # trips ORIGINATE at the grid perimeter (realistic sources/sinks)
    n_rsus: int = 0                      # fixed Road-Side Units: static, always-trusted receivers (0=off)
    rsu_placement: str = "spread"        # where RSUs go: spread|perimeter|center|corners|all(grid nodes)
    rsu_range_m: float = 0.0             # RSU radio range (m); 0 = use radio_range_m (vehicles' range)
    rsu_coords: str = ""                 # explicit RSU positions "x1,y1;x2,y2;..." (overrides placement)
    attack_delay_s: float = 2.0          # flow: an attacker starts falsifying this long after spawn
    attack_delay_jitter_s: float = 0.0   # spread attack onset across attackers by up to this (realism)
    state_prune_every: int = 50          # flow: steps between detection-state LRU prunes
    state_prune_ttl: int = 20            # flow: evict detection state untouched this many steps
    live_interval_s: float = 0.0         # >0: write a throttled live_state.json for the GUI map
                                         # (best-effort viz; NOT part of the data digest)
    events: str = ""                     # JSON scenario timeline (deterministic mid-run dynamics):
                                         # [{"t":120,"until":240,"type":"demand","mult":3.0},
                                         #  {"t":60,"type":"weather","value":"fog"},
                                         #  {"t":90,"until":150,"type":"close_edge","edge":[3,7]},
                                         #  {"t":100,"until":200,"type":"attack_wave"}]
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
    is_rsu: bool = False                 # fixed Road-Side Unit: a static, always-trusted receiver
    is_vru: bool = False                 # vulnerable road user (pedestrian/cyclist); ORACLE-only truth.
                                         # Its beacon SELF-DECLARES station_type=vru (MA-visible); never
                                         # an attacker/faulty/colluder.
    rx_range: float = 0.0                # receiver radio range override (0 = use cfg.radio_range_m)
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
    crl_aware: bool = False                 # watches the public CRL and goes dormant after a bust
    dormant_until: float = 0.0             # CRL-aware: broadcast honestly (no falsification) while t < this
    crl_seen: int = 0                      # last observed CRL size (excluding this vehicle's own entry)
    # vehicle class (heterogeneous fleet)
    veh_type: str = "car"
    veh_length: float = 4.5
    idm_a: float = 1.8                     # per-class max acceleration
    idm_b: float = 2.5                     # per-class comfortable deceleration
    desired_speed: float = 0.0            # free-flow target (car-following cap)
    # car-following kinematic state (integrated each step when cf=True)
    cf: bool = False
    lane_off: float = 0.0                  # perpendicular lane offset (multi-lane roads); the CURRENT
                                           # (possibly mid-transition) offset used for rendering
    lane_idx: int = 0                      # committed lane index 0..n_lanes-1 (source for lane changes)
    lc_active: bool = False                # mid discretionary (MOBIL) lane-change lateral transition
    lc_t0: float = 0.0                     # transition start time
    lc_off0: float = 0.0                   # lateral offset at transition start
    lc_off1: float = 0.0                   # lateral offset target (adjacent lane centre)
    lc_dur: float = 0.0                    # effective transition duration (speed-adaptive; caps heading swing)
    lc_cooldown: float = 0.0               # no new lane change before this time (anti-oscillation gate)
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


@dataclass
class _GtVehicleAware(R.GtVehicle):
    """gt_vehicle row carrying the ORACLE-only is_crl_aware label. Emitted ONLY when crl_aware_pct>0
    so the default path stays byte-identical (plain R.GtVehicle). Inherits to_dict()/asdict()."""
    is_crl_aware: bool = False


@dataclass
class _GtVehicleVru(R.GtVehicle):
    """gt_vehicle row carrying the ORACLE-only is_vru label. Emitted ONLY when vru_pct>0 so the
    default path stays byte-identical (plain R.GtVehicle). is_vru is GROUND TRUTH (never a feature);
    the MA-visible signal is the SEPARATE, legitimately-transmitted station_type field on the beacon /
    ma_cert_status. Mirrors the _GtVehicleAware precedent -- schema stays untouched."""
    is_vru: bool = False


@dataclass
class _GtVehicleVruAware(_GtVehicleAware):
    """gt_vehicle row when BOTH the VRU and CRL-aware opt-ins are active: carries both ORACLE labels."""
    is_vru: bool = False


def _make_gt_vehicle(cfg, v, is_vru: bool, gt_kw: dict):
    """Build the gt_vehicle row, adding ORACLE-only labels ONLY for the opt-ins that are active so the
    default (and each single-feature) path stays byte-identical. is_vru/is_crl_aware are ground truth;
    they never reach a feature table (the MA-visible station_type does that job instead)."""
    if cfg.vru_pct > 0 and cfg.crl_aware_pct > 0:
        return _GtVehicleVruAware(is_vru=is_vru, is_crl_aware=v.crl_aware, **gt_kw)
    if cfg.vru_pct > 0:
        return _GtVehicleVru(is_vru=is_vru, **gt_kw)
    if cfg.crl_aware_pct > 0:
        return _GtVehicleAware(is_crl_aware=v.crl_aware, **gt_kw)
    return R.GtVehicle(**gt_kw)


@dataclass
class _MaCertStatusVru(R.MaCertStatus):
    """ma_cert_status row carrying the MA-VISIBLE self-declared station_type (vehicle|vru). Emitted
    ONLY when vru_pct>0 so the default path stays byte-identical (plain R.MaCertStatus). This is the
    legitimately-transmitted station type the MA observed on the cert's beacons -- NOT the oracle
    is_vru label -- so it is safe to feed ML features."""
    station_type: str = "vehicle"


def _parse_rsu_coords(s: str) -> list:
    """Parse explicit RSU positions 'x1,y1;x2,y2;...' -> [(x,y), ...]. Empty -> []. Raises on bad format."""
    if not s or not s.strip():
        return []
    out = []
    for pair in s.split(";"):
        pair = pair.strip()
        if not pair:
            continue
        xs, _, ys = pair.partition(",")
        out.append((float(xs), float(ys)))
    if not out:
        raise ValueError(f"rsu_coords parsed to nothing: {s!r}")
    return out


def _parse_custom_network(s) -> tuple[list, list]:
    """Parse + sanity-check the custom_network JSON -> (nodes, edges). Raises ValueError with a
    design-actionable message (this is the feedback loop for AI/user map design). Accepts an
    already-parsed dict too (tool layers sometimes hand the object through)."""
    if isinstance(s, dict):
        doc = s
    else:
        if not s or not str(s).strip():
            raise ValueError('road_network="custom" needs custom_network JSON: '
                             '{"nodes": [[x,y], ...metres], "edges": [[a,b], ...node indices]}')
        try:
            doc = json.loads(s)
        except json.JSONDecodeError as e:
            raise ValueError(f"custom_network is not valid JSON: {e}") from None
    if not isinstance(doc, dict) or "nodes" not in doc or "edges" not in doc:
        raise ValueError('custom_network JSON must be an object with "nodes" and "edges"')
    return doc["nodes"], doc["edges"]


# Scenario-event timeline: each event type, its required keys, and what it changes mid-run.
EVENT_TYPES = {
    "demand": "arrival-rate multiplier while active: {t, until, mult}",
    "weather": "weather changes at t (sensor noise, radio loss, new drivers' speed): {t, value}",
    "close_edge": "road closed to NEW trips while active (navigation avoidance): {t, edge, [until]}",
    "attack_wave": "attackers only falsify inside attack_wave windows (if any are defined): {t, until}",
    "attack_zone": "attackers only falsify while INSIDE an active zone (geofenced campaign): "
                   "{t, x, y, radius, [until]}",
}


def _parse_events(s) -> list[dict]:
    """Parse + validate the events JSON timeline -> chronologically sorted list. Empty -> [].
    Accepts an already-parsed list too (tool layers sometimes hand the object through)."""
    if isinstance(s, list):
        evs = s
    else:
        if not s or not str(s).strip():
            return []
        try:
            evs = json.loads(s)
        except json.JSONDecodeError as e:
            raise ValueError(f"events is not valid JSON: {e}") from None
    if not isinstance(evs, list):
        raise ValueError("events must be a JSON list of event objects")
    out = []
    for k, e in enumerate(evs):
        if not isinstance(e, dict) or "type" not in e or "t" not in e:
            raise ValueError(f"event {k} must be an object with at least 't' and 'type'")
        et = e["type"]
        if et not in EVENT_TYPES:
            raise ValueError(f"event {k}: unknown type {et!r}; valid: {sorted(EVENT_TYPES)}")
        t0 = float(e["t"])
        until = float(e["until"]) if e.get("until") is not None else None
        if t0 < 0 or (until is not None and until <= t0):
            raise ValueError(f"event {k}: need 0 <= t < until (got t={t0}, until={until})")
        ev = {"t": t0, "until": until, "type": et}
        if et == "demand":
            if until is None:
                raise ValueError(f"event {k}: demand needs 'until'")
            ev["mult"] = float(e.get("mult", 1.0))
            if ev["mult"] < 0:
                raise ValueError(f"event {k}: demand mult must be >= 0")
        elif et == "weather":
            v = e.get("value")
            if v not in WEATHER_MULT:
                raise ValueError(f"event {k}: weather value must be one of {sorted(WEATHER_MULT)}")
            ev["value"] = v
        elif et == "close_edge":
            edge = e.get("edge")
            if not isinstance(edge, (list, tuple)) or len(edge) != 2:
                raise ValueError(f"event {k}: close_edge needs 'edge': [a,b] (custom/spider node "
                                 f"indices) or [[i,j],[i2,j2]] (grid intersections)")
            ev["edge"] = edge
        elif et == "attack_wave":
            if until is None:
                raise ValueError(f"event {k}: attack_wave needs 'until'")
        elif et == "attack_zone":
            try:
                ev["x"], ev["y"] = float(e["x"]), float(e["y"])
                ev["radius"] = float(e["radius"])
            except (KeyError, TypeError, ValueError):
                raise ValueError(f"event {k}: attack_zone needs numeric x, y, radius") from None
            if ev["radius"] <= 0:
                raise ValueError(f"event {k}: attack_zone radius must be > 0")
        out.append(ev)
    out.sort(key=lambda e: (e["t"], e["type"]))
    return out


def _rsu_spots(cfg: "PipelineConfig", net) -> list:
    """Placement coordinates for the RSUs. Explicit rsu_coords wins; else per cfg.rsu_placement."""
    explicit = _parse_rsu_coords(cfg.rsu_coords)
    if explicit:
        return explicit
    p = cfg.rsu_placement
    if net is None:                              # linear corridor: evenly along the road
        span = max(1.0, cfg.n_vehicles * 20.0)
        n = cfg.n_rsus
        return [(span * (i + 0.5) / n, 0.0) for i in range(n)]
    nodes = net.nodes
    grid_nodes = bool(nodes) and isinstance(nodes[0], tuple)   # ring nodes are ints
    if p in ("corners", "center") and not grid_nodes:
        p = "spread"                            # corners/center are grid-only -> fall back on a ring
    if p == "all":                              # one RSU at every intersection (n_rsus ignored)
        picked = nodes
    elif p == "corners":
        w, h = net.w - 1, net.h - 1
        corners = [(0, 0), (w, 0), (0, h), (w, h)]
        picked = [corners[i % 4] for i in range(cfg.n_rsus)]
    elif p == "perimeter":
        b = net.boundary
        step = max(1, len(b) // max(1, cfg.n_rsus))
        picked = [b[(i * step) % len(b)] for i in range(cfg.n_rsus)]
    elif p == "center":                         # cluster near the grid centre
        cx, cy = (net.w - 1) / 2.0, (net.h - 1) / 2.0
        near = sorted(nodes, key=lambda n: abs(n[0] - cx) + abs(n[1] - cy))
        picked = [near[i % len(near)] for i in range(cfg.n_rsus)]
    else:                                       # "spread": evenly across all intersections
        step = max(1, len(nodes) // max(1, cfg.n_rsus))
        picked = [nodes[(i * step) % len(nodes)] for i in range(cfg.n_rsus)]
    return [net._coord(n) for n in picked]


def _ang_diff(a: float, b: float) -> float:
    d = abs((a - b) % 360.0)
    return d if d <= 180.0 else 360.0 - d


_PROB_FIELDS = ("report_prob", "attacker_pct", "faulty_pct", "collude_pct", "victim_pct",
                "packet_loss_base", "nlos_loss", "gps_outlier_rate", "gps_degrade_rate",
                "attack_duty_cycle", "crl_aware_pct", "vru_pct")

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
    if cfg.radio_model not in ("disc", "logdistance"):
        raise ValueError(f"radio_model must be disc|logdistance (got {cfg.radio_model!r})")
    if cfg.pathloss_exponent <= 0:
        raise ValueError(f"pathloss_exponent must be > 0 (got {cfg.pathloss_exponent})")
    if cfg.shadowing_sigma_db < 0:
        raise ValueError(f"shadowing_sigma_db must be >= 0 (got {cfg.shadowing_sigma_db})")
    if not -20.0 <= cfg.rx_sensitivity_margin_db <= 20.0:
        raise ValueError(f"rx_sensitivity_margin_db must be in [-20, 20] dB "
                         f"(got {cfg.rx_sensitivity_margin_db})")
    if cfg.radio_cap_sigma <= 0:
        raise ValueError(f"radio_cap_sigma must be > 0 (got {cfg.radio_cap_sigma})")
    if cfg.radio_cap_max_mult < 1:
        raise ValueError(f"radio_cap_max_mult must be >= 1 (got {cfg.radio_cap_max_mult})")
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
    if cfg.crl_dormant_s <= 0:
        raise ValueError(f"crl_dormant_s must be > 0 (got {cfg.crl_dormant_s})")
    if cfg.od_gravity_scale <= 0:
        raise ValueError(f"od_gravity_scale must be > 0 (got {cfg.od_gravity_scale})")
    if cfg.n_rsus < 0:
        raise ValueError(f"n_rsus must be >= 0 (got {cfg.n_rsus})")
    if cfg.rsu_placement not in ("spread", "perimeter", "center", "corners", "all"):
        raise ValueError(f"rsu_placement must be spread|perimeter|center|corners|all "
                         f"(got {cfg.rsu_placement!r})")
    if cfg.rsu_range_m < 0:
        raise ValueError(f"rsu_range_m must be >= 0 (got {cfg.rsu_range_m})")
    _parse_rsu_coords(cfg.rsu_coords)    # raises ValueError on a malformed coordinate string
    if cfg.n_lanes < 1:
        raise ValueError(f"n_lanes must be >= 1 (got {cfg.n_lanes})")
    if cfg.lane_width_m <= 0:
        raise ValueError(f"lane_width_m must be > 0 (got {cfg.lane_width_m})")
    if cfg.lane_changes:                                  # discretionary lane changes: only meaningful
        if cfg.n_lanes <= 1:                             # with >1 lane AND routed car-following (flow)
            raise ValueError("lane_changes needs n_lanes > 1 (multi-lane roads to change between)")
        if not cfg.traffic_flow:
            raise ValueError("lane_changes needs traffic_flow=true (routed car-following mobility)")
        if cfg.lane_change_time_s <= 0:
            raise ValueError(f"lane_change_time_s must be > 0 (got {cfg.lane_change_time_s})")
        if cfg.lane_change_politeness < 0:
            raise ValueError(f"lane_change_politeness must be >= 0 (got {cfg.lane_change_politeness})")
        if cfg.lane_change_threshold < 0:
            raise ValueError(f"lane_change_threshold must be >= 0 (got {cfg.lane_change_threshold})")
    if cfg.gap_acceptance:                                # yielding at unsignalized intersections:
        if not cfg.traffic_flow:                          # needs routed car-following on a real network
            raise ValueError("gap_acceptance needs traffic_flow=true (routed car-following mobility)")
        if cfg.road_network == "linear":                 # a straight road has no intersection nodes to yield at
            raise ValueError("gap_acceptance needs a routed network with intersections "
                             "(grid/ring/spider/custom), not the linear road")
        # NOTE: gap_acceptance governs UNsignalized nodes only. With traffic_lights on, every node is
        # signalized, so it defers entirely to the signal -> it is most meaningful with traffic_lights=false.
    if cfg.light_cycle_s <= 0:
        raise ValueError(f"light_cycle_s must be > 0 (got {cfg.light_cycle_s})")
    if cfg.grid_block_m <= 0:
        raise ValueError(f"grid_block_m must be > 0 (got {cfg.grid_block_m})")
    if not (0.0 <= cfg.grid_dropout <= 1.0):
        raise ValueError(f"grid_dropout must be in [0,1] (got {cfg.grid_dropout})")
    if cfg.arterial_every < 0:
        raise ValueError(f"arterial_every must be >= 0 (0 = off) (got {cfg.arterial_every})")
    for _nm in ("arterial_speed_mps", "local_speed_mps"):   # 0 = uncapped; else a posted limit
        _sp = float(getattr(cfg, _nm))
        if _sp and not (1.0 <= _sp <= 70.0):
            raise ValueError(f"{_nm} must be 0 (uncapped) or 1-70 m/s "
                             f"(33 ~ 120 km/h highway, 8.3 ~ 30 km/h zone) (got {_sp})")
    # Speed-cap knobs are consumed only by the topology that models them: grid uses all three, ring
    # uses arterial_speed_mps (a single whole-ring cap) only. Reject them elsewhere so they are never
    # silent dead knobs (custom maps carry per-edge speeds in the custom_network JSON instead).
    if cfg.arterial_every or cfg.arterial_speed_mps or cfg.local_speed_mps:
        if cfg.road_network == "ring" and (cfg.arterial_every or cfg.local_speed_mps):
            raise ValueError("ring uses only arterial_speed_mps (whole-ring cap); arterial_every/"
                             "local_speed_mps apply to a grid road only")
        if cfg.road_network not in ("grid", "ring"):
            raise ValueError(f"arterial_*/local_speed_mps speed caps apply only to grid/ring roads, "
                             f"not road_network={cfg.road_network!r} (custom maps set per-edge speeds "
                             f"in the custom_network JSON)")
    if cfg.fleet != "mixed" and cfg.fleet not in VEHICLE_TYPES:
        raise ValueError(f"fleet must be 'mixed' or one of {sorted(VEHICLE_TYPES)} (got {cfg.fleet!r})")
    _parse_fleet_mix(cfg.fleet_mix)      # raises ValueError on a bad class name / weight
    _parse_attack_mix(cfg.attack_mix)    # raises ValueError on an unknown attack type / weight
    if cfg.trip_speed_min <= 0 or cfg.trip_speed_max < cfg.trip_speed_min:
        raise ValueError(f"need 0 < trip_speed_min <= trip_speed_max "
                         f"(got {cfg.trip_speed_min}, {cfg.trip_speed_max})")
    if cfg.vru_pct >= 1.0:                              # VRUs are a FRACTION of actors; ratio vru/(1-vru)
        raise ValueError(f"vru_pct must be < 1.0 (it is a fraction of actors; got {cfg.vru_pct})")
    if cfg.vru_pct > 0 and cfg.vru_speed_mps <= 0:      # VRUs must actually move (walking/cycling)
        raise ValueError(f"vru_speed_mps must be > 0 when vru_pct > 0 (got {cfg.vru_speed_mps})")
    if cfg.denm_rate < 0:                                # DENMs/veh/100s (a rate, not a probability)
        raise ValueError(f"denm_rate must be >= 0 (got {cfg.denm_rate})")
    # DENM / VRU thresholds (defaults reproduce the historic module constants)
    if cfg.denm_rate_window_s <= 0:
        raise ValueError(f"denm_rate_window_s must be > 0 (got {cfg.denm_rate_window_s})")
    if cfg.denm_fake_fallback_rate < 0:
        raise ValueError(f"denm_fake_fallback_rate must be >= 0 (got {cfg.denm_fake_fallback_rate})")
    if cfg.denm_benign_max_speed_mps <= 0:
        raise ValueError(f"denm_benign_max_speed_mps must be > 0 (got {cfg.denm_benign_max_speed_mps})")
    if cfg.denm_decel_trig_mps2 <= 0:
        raise ValueError(f"denm_decel_trig_mps2 must be > 0 (got {cfg.denm_decel_trig_mps2})")
    if cfg.denm_implausible_speed_mps <= 0:
        raise ValueError(f"denm_implausible_speed_mps must be > 0 (got {cfg.denm_implausible_speed_mps})")
    if cfg.vru_max_plausible_speed_mps <= 0:
        raise ValueError(f"vru_max_plausible_speed_mps must be > 0 (got {cfg.vru_max_plausible_speed_mps})")
    # detector operating point (strictness of the motion + Sybil detectors)
    if cfg.detector_z_threshold <= 0:
        raise ValueError(f"detector_z_threshold must be > 0 (got {cfg.detector_z_threshold})")
    if cfg.detector_min_consec < 1:
        raise ValueError(f"detector_min_consec must be >= 1 (got {cfg.detector_min_consec})")
    if cfg.sybil_min_certs < 2:
        raise ValueError(f"sybil_min_certs must be >= 2 (got {cfg.sybil_min_certs})")
    if cfg.sybil_cell_m <= 0:
        raise ValueError(f"sybil_cell_m must be > 0 (got {cfg.sybil_cell_m})")
    # per-vehicle GNSS quality spread
    if cfg.gps_quality_floor < 0:
        raise ValueError(f"gps_quality_floor must be >= 0 (got {cfg.gps_quality_floor})")
    if cfg.gps_quality_lambda <= 0:
        raise ValueError(f"gps_quality_lambda must be > 0 (got {cfg.gps_quality_lambda})")
    if cfg.road_network not in ("linear", "grid", "ring", "spider", "custom"):
        raise ValueError(f"road_network must be linear|grid|ring|spider|custom "
                         f"(got {cfg.road_network!r})")
    if cfg.road_network == "custom":
        from .roads import CustomNetwork
        CustomNetwork(*_parse_custom_network(cfg.custom_network))   # full design validation
    if cfg.road_network in ("spider", "custom") and not cfg.traffic_flow:
        raise ValueError(f"road_network={cfg.road_network!r} needs traffic_flow=true: fixed-fleet "
                         f"vehicles drive straight lines, which would put them off the designed "
                         f"roads (set traffic_flow=true and an arrival_rate)")
    ev = _parse_events(cfg.events)                                  # raises on a malformed timeline
    if any(e["type"] == "close_edge" for e in ev):
        if cfg.road_network in ("linear", "ring"):
            raise ValueError("close_edge events need a grid, spider, or custom road network")
    if cfg.traffic_flow:
        if cfg.arrival_rate < 0:
            raise ValueError(f"arrival_rate must be >= 0 (got {cfg.arrival_rate})")
        if cfg.duration_s < 0:
            raise ValueError(f"duration_s must be >= 0 (got {cfg.duration_s})")
        if cfg.road_network == "grid" and (cfg.grid_w < 2 or cfg.grid_h < 2):
            raise ValueError(f"grid road network needs grid_w and grid_h >= 2 "
                             f"(got {cfg.grid_w}x{cfg.grid_h})")
        if cfg.road_network == "ring" and cfg.grid_w < 3:
            raise ValueError(f"ring road network needs grid_w >= 3 nodes (got {cfg.grid_w})")
        if cfg.road_network == "spider" and (cfg.grid_w < 3 or cfg.grid_h < 1):
            raise ValueError(f"spider network needs grid_w >= 3 arms and grid_h >= 1 rings "
                             f"(got {cfg.grid_w} arms x {cfg.grid_h} rings)")
    if cfg.attacker_ids:
        bad = [a for a in cfg.attacker_ids if not isinstance(a, int) or isinstance(a, bool)]
        if bad:
            raise ValueError(f"attacker_ids must be integer vehicle ids (got non-int {bad!r}); a "
                             f"JSON-loaded list of strings is coerced by config_from_dict")
        if cfg.attacker_pct == 0 and not cfg.traffic_flow:   # attacker_ids used only here
            oor = [a for a in cfg.attacker_ids if a < 0 or a >= cfg.n_vehicles]
            if oor:
                raise ValueError(f"attacker_ids {oor!r} out of range for n_vehicles={cfg.n_vehicles} "
                                 f"(valid ids 0..{cfg.n_vehicles - 1})")
    for name in _PROB_FIELDS:                       # clamp fractions rather than produce nonsense
        setattr(cfg, name, min(1.0, max(0.0, float(getattr(cfg, name)))))
    return cfg


# tuple-typed config fields -- JSON has no tuples, so lists are coerced back on load
_TUPLE_FIELDS = ("attacker_ids", "attack_types")


def _field_group(name: str) -> str:
    """Category for a config field, so UIs/tools can group the ~97 knobs sensibly."""
    g = [
        ("RSU", ("n_rsus", "rsu_")),
        ("Attacks", ("attack", "attacker", "sybil_ghosts", "collude", "victim", "dos", "delay",
                     "crl_aware", "crl_dormant")),
        ("Mobility", ("fleet", "trip_", "idm_", "demand", "arrival", "n_lanes", "lane_", "turn_",
                      "gap_acceptance", "car_following", "veh_length", "od_", "boundary",
                      "traffic_flow", "duration", "max_total", "nominal_speed", "state_prune", "vru_")),
        ("Network", ("road_network", "grid", "custom_network", "traffic_lights", "light_cycle",
                     "arterial", "local_speed")),
        ("Scenario events", ("events",)),
        ("Messages", ("denm",)),
        ("GNSS/sensor", ("gps_", "faulty", "weather")),
        ("Radio", ("radio", "packet", "nlos", "chan", "freq", "art_max", "stale", "pathloss",
                   "shadowing", "rx_sensitivity")),
        ("Detection/MA", ("consistency", "heading", "detector", "report", "revoke", "reputation",
                          "ma_defense", "max_accel", "offroad", "rotate", "beacon", "net_delay",
                          "crl_", "sybil_min_certs", "sybil_cell_m")),
        ("Run", ("seed", "n_vehicles", "n_steps", "dt", "jmax", "out_dir", "verbose",
                 "live_interval", "emit_sample")),
    ]
    for label, prefixes in g:
        if any(name == p or name.startswith(p) for p in prefixes):
            return label
    return "Other"


# Enumerated fields -> their valid options (sourced from the live constants so they never drift).
_ENUM_OPTIONS = {
    "weather": list(WEATHER_MULT),
    "radio_model": ["disc", "logdistance"],
    "road_network": ["linear", "grid", "ring", "spider", "custom"],
    "demand_profile": ["uniform", "rush", "night"],
    "od_model": ["uniform", "gravity"],
    "fleet": ["mixed", *VEHICLE_TYPES],
    "rsu_placement": ["spread", "perimeter", "center", "corners", "all"],
    "attack_type": ["", *ATTACK_CATALOG, *COMBINED_ATTACKS, *IDENTITY_SPOOF_ATTACKS,
                    *DENM_ATTACKS],  # "" = unset
}

# Per-field documentation + ranges/units so every knob is self-describing in UIs and tooling.
# Keys: h=help, lo=min, hi=max, st=step, u=unit. (Fractions [0,1] get lo/hi/st filled by default.)
_FIELD_META = {
    # Run
    "seed": dict(h="RNG seed — same seed + config gives a byte-identical dataset", lo=0, st=1),
    "n_vehicles": dict(h="Fixed-fleet vehicle count (ignored in traffic-flow mode)", lo=1, hi=5000),
    "n_steps": dict(h="Fixed-fleet simulation steps (ignored when duration_s > 0)", lo=0),
    "dt": dict(h="Simulation timestep", lo=0.1, hi=5, st=0.1, u="s"),
    "emit_sample_prob": dict(h="Per-message ground-truth emission sampling probability", lo=0, hi=1, st=0.01),
    "live_interval_s": dict(h="Write the live-map JSON every N sim-seconds (0 = off)", lo=0, u="s"),
    "verbose": dict(h="Print a progress heartbeat while running"),
    "jmax": dict(h="Linkage j-index period size (SCMS internal)", lo=1),
    "out_dir": dict(h="Output directory for the generated dataset"),
    # Mobility
    "nominal_speed": dict(h="Fixed-fleet cruising speed", lo=1, hi=60, u="m/s"),
    "traffic_flow": dict(h="Spawn/despawn vehicles over time on a road network (vs a fixed fleet)"),
    "duration_s": dict(h="Traffic-flow length; overrides n_steps when > 0", lo=0, u="s"),
    "arrival_rate": dict(h="Mean vehicle arrivals per second (traffic-flow)", lo=0, hi=20, st=0.5, u="/s"),
    "max_total_vehicles": dict(h="Cap total spawns (0 = unlimited) — bounds memory on long runs", lo=0),
    "vru_pct": dict(h="Fraction of spawned actors that are VRUs (pedestrians/cyclists; benign, "
                      "self-declaring station_type=vru; 0 = none, byte-identical)", lo=0, hi=1, st=0.05),
    "vru_speed_mps": dict(h="VRU travel speed (~1.4 walking .. ~5 cycling)", lo=0.1, hi=10, st=0.1, u="m/s"),
    "vru_max_plausible_speed_mps": dict(h="A beacon declaring station_type=vru but claiming a speed "
                                          "at/above this is a vehicle impersonating a VRU (drives the "
                                          "vruImpersonation speed arm; lower fires more readily)",
                                        lo=1, hi=30, st=0.5, u="m/s"),
    # Messages (DENM event-message layer)
    "denm_rate": dict(h="Benign event-message (DENM) rate: expected DENMs per vehicle per 100 s from a "
                        "REAL trigger (hard brake to a near-stop / stationary); 0 = none, byte-identical. "
                        "The opt-in FakeHazard attack emits phantom DENMs regardless of this rate.",
                      lo=0, hi=200, st=5, u="/100s"),
    "denm_rate_window_s": dict(h="Time window that denm_rate / denm_fake_fallback_rate are expressed "
                                 "per (rate normalizer)", lo=1, u="s"),
    "denm_fake_fallback_rate": dict(h="Phantom DENMs per attacker per window a FakeHazard emits when "
                                      "denm_rate == 0 (keeps the attack self-contained)", lo=0, u="/100s"),
    "denm_benign_max_speed_mps": dict(h="Benign DENM trigger: sender speed at/below this (post-brake/"
                                        "stop); also the base of the derived brake-implausible bound "
                                        "(this + 0.5)", lo=0.1, hi=30, st=0.5, u="m/s"),
    "denm_decel_trig_mps2": dict(h="Hard-deceleration magnitude that arms a benign brake DENM", lo=0.1,
                                 hi=10, st=0.5, u="m/s²"),
    "denm_implausible_speed_mps": dict(h="denmPlausibility fires when a hazard's sender CLAIMS a speed "
                                         "above this generic bound (lower flags more DENMs)", lo=0.1,
                                       hi=30, st=0.5, u="m/s"),
    "n_lanes": dict(h="Parallel lanes per road (overtaking; relieves gridlock)", lo=1, hi=6),
    "lane_width_m": dict(h="Lane width for multi-lane offsets", lo=1, hi=6, st=0.25, u="m"),
    "trip_speed_min": dict(h="Minimum desired trip speed", lo=1, hi=60, u="m/s"),
    "trip_speed_max": dict(h="Maximum desired trip speed", lo=1, hi=60, u="m/s"),
    "fleet": dict(h="Vehicle mix: 'mixed' or a single class"),
    "fleet_mix": dict(h="Custom class weights, e.g. car:0.6,truck:0.3,bus:0.1 (blank = default mix)"),
    "car_following": dict(h="IDM car-following (queues, stop-and-go, congestion)"),
    "idm_accel": dict(h="IDM maximum acceleration", lo=0.1, hi=5, st=0.1, u="m/s²"),
    "idm_decel": dict(h="IDM comfortable deceleration", lo=0.1, hi=6, st=0.1, u="m/s²"),
    "idm_time_headway": dict(h="IDM desired time gap to the leader", lo=0.3, hi=4, st=0.1, u="s"),
    "idm_min_gap": dict(h="IDM jam distance / minimum gap", lo=0.5, hi=10, st=0.5, u="m"),
    "veh_length_m": dict(h="Per-class fallback vehicle length; the per-vehicle-type length from the "
                           "fleet mix overrides it in car-following/IDM", lo=1, hi=20, u="m"),
    "idm_lookahead_m": dict(h="IDM leader search distance", lo=10, hi=200, u="m"),
    "turn_slowdown": dict(h="Slow into sharp grid corners (more realistic, harder to detect)"),
    "turn_speed_mps": dict(h="Speed cap through a sharp bend", lo=1, hi=20, u="m/s"),
    "turn_min_angle_deg": dict(h="Only bends sharper than this are slowed", lo=0, hi=180, u="°"),
    "lane_changes": dict(h="MOBIL discretionary lane changes (needs n_lanes>1 + flow; realistic benign "
                           "lateral move + heading swing that mimics a position/heading lie)"),
    "lane_change_time_s": dict(h="Smooth lane-change lateral transition duration", lo=0.5, hi=6, st=0.5, u="s"),
    "lane_change_politeness": dict(h="MOBIL politeness: weight on other vehicles' acceleration change",
                                   lo=0, hi=2, st=0.1),
    "lane_change_threshold": dict(h="MOBIL incentive threshold to commit to a lane change", lo=0, hi=3,
                                  st=0.1, u="m/s²"),
    "gap_acceptance": dict(h="Yield to conflicting cross-traffic at UNSIGNALIZED intersections "
                             "(deterministic first-come priority; needs traffic_flow + a routed "
                             "network; governs unsignalized nodes only; realistic slowing without "
                             "false revocations)"),
    "demand_profile": dict(h="Arrival-demand shape over the run"),
    "od_model": dict(h="Trip destination law: uniform or distance-decay gravity"),
    "od_gravity_scale": dict(h="Gravity hop-decay scale (smaller = shorter trips)", lo=0.1, hi=10, st=0.5),
    "boundary_origins": dict(h="Trips originate at the network perimeter (realistic sources/sinks)"),
    "state_prune_every": dict(h="Steps between detection-state prunes (flow memory)", lo=1),
    "state_prune_ttl": dict(h="Evict detection state untouched this many steps", lo=1),
    # Network
    "road_network": dict(h="Road topology: straight lines, routed grid, ring, radial spider city, "
                           "or a fully custom node/edge map (see custom_network)"),
    "custom_network": dict(h='Custom map JSON {"nodes":[[x,y]...metres],"edges":[[a,b]...]} — any '
                             'connected road graph (AI/user-designed); used when road_network=custom'),
    "events": dict(h='Scenario timeline JSON: [{"t":s,"type":"demand|weather|close_edge|attack_wave",'
                     '...}] — mid-run demand surges, weather fronts, road closures, attack waves'),
    "grid_w": dict(h="Grid columns (grid) / number of intersections (ring)", lo=2, hi=40),
    "grid_h": dict(h="Grid rows (0 = square, equal to grid_w)", lo=0, hi=40),
    "grid_block_m": dict(h="Spacing between adjacent intersections", lo=20, hi=500, u="m"),
    "grid_dropout": dict(h="Fraction of grid roads removed (irregular grid; stays connected)", lo=0, hi=1, st=0.05),
    "traffic_lights": dict(h="Signalized intersections (stops + queues)"),
    "light_cycle_s": dict(h="Full signal cycle; half green per axis", lo=2, hi=120, u="s"),
    "arterial_every": dict(h="Grid: every Nth row & column is a faster arterial road (0 = off)",
                           lo=0, hi=10, st=1),
    "arterial_speed_mps": dict(h="Speed limit on arterial roads (grid) / the whole ring (0 = uncapped)",
                               lo=0, hi=70, st=1, u="m/s"),
    "local_speed_mps": dict(h="Grid: speed limit on local (non-arterial) roads (0 = uncapped)",
                            lo=0, hi=70, st=1, u="m/s"),
    # Attacks
    "attacker_ids": dict(h="Fixed-fleet attacker vehicle ids (used only when attacker_pct = 0)"),
    "attacker_pct": dict(h="Fraction of vehicles that are attackers", lo=0, hi=1, st=0.05),
    "attack_type": dict(h="Single attack type: any value (incl. an opt-in 'combined' type), with "
                          "attack_types left at its default (full catalog), narrows the whole run to "
                          "only this type; blank (default) = unset, so attack_types is used instead"),
    "attack_types": dict(h="Enabled attack types (round-robin), comma-separated"),
    "attack_start": dict(h="Fixed-fleet: attack begins at this time", lo=0, u="s"),
    "attack_end": dict(h="Fixed-fleet: attack ends at this time", lo=0, u="s"),
    "attack_intensity": dict(h="Falsification magnitude scale (subtle <1 .. blatant >1)", lo=0, hi=5, st=0.25),
    "attack_mix": dict(h="Per-type weights, e.g. ConstPos:0.6,Sybil:0.4 (blank = round-robin)"),
    "attack_duty_cycle": dict(h="Fraction of each pulse the attacker falsifies (<1 = intermittent)", lo=0, hi=1, st=0.05),
    "attack_pulse_period_s": dict(h="On/off cycle length for pulsed attacks", lo=1, u="s"),
    "crl_aware_pct": dict(h="Fraction of attackers that watch the public CRL and lie low after a bust", lo=0, hi=1, st=0.05),
    "crl_dormant_s": dict(h="How long a CRL-aware attacker broadcasts honestly after a new revocation", lo=1, u="s"),
    "attack_delay_s": dict(h="Flow: attacker starts falsifying this long after spawn", lo=0, u="s"),
    "attack_delay_jitter_s": dict(h="Spread attacker onset by up to this much", lo=0, u="s"),
    "dos_burst": dict(h="CAMs per interval a DoS attacker floods", lo=1),
    "delay_s": dict(h="DelayedMessages staleness", lo=0, u="s"),
    "collude_pct": dict(h="Fraction of attackers that also file false reports", lo=0, hi=1, st=0.05),
    "victim_pct": dict(h="Fraction of benign vehicles colluders frame (fixed fleet: of the whole "
                         "fleet; flow: of in-range benign candidates; 0 = legacy 2-nearest)",
                       lo=0, hi=1, st=0.05),
    "sybil_ghosts": dict(h="Ghost identities a Sybil attacker fabricates", lo=0, hi=20),
    # GNSS / sensor
    "gps_sigma_m": dict(h="White per-axis GNSS noise", lo=0, hi=20, st=0.1, u="m"),
    "gps_bias_sigma_m": dict(h="OU-correlated slow GNSS bias amplitude", lo=0, hi=20, st=0.1, u="m"),
    "gps_bias_tau_s": dict(h="GNSS bias correlation time", lo=1, u="s"),
    "gps_outlier_rate": dict(h="Per-message multipath outlier probability", lo=0, hi=1, st=0.01),
    "gps_outlier_mag_m": dict(h="Multipath outlier magnitude", lo=0, u="m"),
    "gps_degrade_rate": dict(h="Per-step prob a benign vehicle enters a bad-GNSS burst", lo=0, hi=1, st=0.005),
    "gps_degrade_factor": dict(h="Noise multiplier during a bad-GNSS burst", lo=1, hi=20),
    "gps_degrade_dur_s": dict(h="Bad-GNSS burst length", lo=0, u="s"),
    "gps_jam_rate": dict(h="Per-step prob a benign vehicle loses GNSS fix (goes silent)", lo=0, hi=1, st=0.01),
    "gps_jam_dur_s": dict(h="GNSS outage length", lo=0, u="s"),
    "gps_quality_floor": dict(h="Best-case per-vehicle GNSS quality: the noise-scale floor added to the "
                                "exponential draw (higher raises mean GNSS error across the fleet)",
                              lo=0, hi=10, st=0.1),
    "gps_quality_lambda": dict(h="Rate of the exponential per-vehicle GNSS-quality tail (smaller = "
                                 "heavier tail, more vehicles with poor fixes)", lo=0.1, hi=10, st=0.1),
    "faulty_pct": dict(h="Fraction of benign vehicles with a malfunctioning sensor", lo=0, hi=1, st=0.05),
    "faulty_bias_mult": dict(h="Faulty-sensor sustained bias multiplier", lo=1, hi=20),
    "weather": dict(h="Weather — degrades GNSS accuracy, radio, and speed"),
    # Detection / MA
    "consistency_threshold_m": dict(h="Position/speed consistency tolerance", lo=0, u="m"),
    "heading_threshold_deg": dict(h="Heading-inconsistency threshold", lo=0, hi=180, u="°"),
    "detector_lag_s": dict(h="Lagged-reference age used by detectors", lo=0, u="s"),
    "report_prob": dict(h="Probability a receiver files a report on a detection", lo=0, hi=1, st=0.05),
    "report_threshold_k": dict(h="Distinct reporters needed to open an investigation", lo=1, hi=20),
    "revoke_min_seconds": dict(h="AND reports in at least this many distinct seconds", lo=1),
    "revoke_persist_s": dict(h="AND evidence spanning at least this long", lo=0, u="s"),
    "revoke_window_s": dict(h="Sustained-evidence sliding window", lo=1, u="s"),
    "net_delay_max": dict(h="Maximum report ingest delay", lo=0, u="s"),
    "crl_propagation_delay": dict(h="CRL propagation delay before enforcement", lo=0, u="s"),
    "offroad_tol_m": dict(h="Map off-road tolerance (HD-map check)", lo=0, u="m"),
    "max_accel_mps2": dict(h="Implausible-acceleration threshold", lo=1, u="m/s²"),
    "rotate_period_s": dict(h="Pseudonym rotation period (0 = no rotation)", lo=0, u="s"),
    "ma_defense": dict(h="Trusted-reporter gating (reputation + rate limit)"),
    "reputation_max": dict(h="A reporter itself reported more than this is distrusted", lo=1),
    "report_budget": dict(h="A reporter filing more than this is rate-limited", lo=1),
    "detector_z_threshold": dict(h="Motion-residual firing point: a residual must exceed ~this many "
                                   "broadcast-uncertainty sigmas to count as a violation (lower = "
                                   "stricter, more violations)", lo=0.5, hi=10, st=0.25),
    "detector_min_consec": dict(h="Consecutive per-detector violations required before a reason fires "
                                  "(1 = fire on the first, higher needs a sustained streak)", lo=1, hi=10),
    "sybil_min_certs": dict(h="Distinct co-located certs (same cell + heading) before sybilCoLocation "
                              "reaches its firing score of 1.0 (lower fires more readily)", lo=2, hi=20),
    "sybil_cell_m": dict(h="Sybil co-location cell size: certs are binned to this grid; smaller demands "
                           "tighter co-location to flag", lo=0.5, hi=20, st=0.5, u="m"),
    # Radio
    "radio_range_m": dict(h="Vehicle reception range", lo=10, hi=2000, u="m"),
    "radio_model": dict(h="Reachability model: disc (hard range) | logdistance (soft path-loss + shadowing)"),
    "pathloss_exponent": dict(h="Log-distance path-loss exponent n (urban ~2.7-3.5; logdistance only)",
                              lo=1.5, hi=6.0, st=0.1),
    "shadowing_sigma_db": dict(h="Log-normal shadowing std in dB (0 = near-hard cutoff; logdistance only)",
                               lo=0, hi=12, st=0.5, u="dB"),
    "rx_sensitivity_margin_db": dict(h="Sensitivity margin dB: + shrinks / - extends range (logdistance only)",
                                     lo=-20, hi=20, st=0.5, u="dB"),
    "radio_cap_sigma": dict(h="logdistance candidate-window cap headroom in shadow standard deviations "
                              "(disc ignores it)", lo=0.5, hi=12, st=0.5),
    "radio_cap_max_mult": dict(h="logdistance hard ceiling on candidate cap / range, bounding the cell "
                                 "search (disc ignores it)", lo=1, hi=20, st=0.5),
    "packet_loss_base": dict(h="Baseline per-message packet loss", lo=0, hi=1, st=0.01),
    "nlos_loss": dict(h="Distance-growing obstruction (NLOS) loss", lo=0, hi=1, st=0.05),
    "chan_capacity": dict(h="In-range CAMs/step before congestion loss", lo=1),
    "art_max_m": dict(h="Acceptance-range tolerance beyond radio range", lo=0, u="m"),
    "freq_max": dict(h="Beacon-rate normalizer (CAMs/interval)", lo=1),
    "stale_max_s": dict(h="Staleness threshold for staleOrReplay", lo=0, u="s"),
    # RSU
    "n_rsus": dict(h="Number of Road-Side Units — static, always-trusted receivers (0 = off)", lo=0, hi=200),
    "rsu_placement": dict(h="Where RSUs are placed on the network"),
    "rsu_range_m": dict(h="RSU radio range (0 = same as radio_range_m)", lo=0, u="m"),
    "rsu_coords": dict(h="Explicit RSU positions x1,y1;x2,y2;... (overrides placement)"),
}


def config_schema() -> dict:
    """Machine-readable schema of every PipelineConfig field: for each, {type, default, group, widget,
    help, options, min, max, step, unit}. widget in {bool, select, int, float, text}. Lets UIs/tools
    render a fully self-describing form (dropdowns for enums, ranges/units for numbers)."""
    out = {}
    for f in dataclasses.fields(PipelineConfig):
        default = f.default
        if default is dataclasses.MISSING:
            default = None
        elif isinstance(default, tuple):
            default = list(default)
        typ = str(f.type)
        opts = _ENUM_OPTIONS.get(f.name)
        if isinstance(default, bool):
            widget = "bool"
        elif opts is not None:
            widget = "select"
        elif typ.startswith("int"):
            widget = "int"
        elif typ.startswith("float"):
            widget = "float"
        else:
            widget = "text"
        meta = _FIELD_META.get(f.name, {})
        out[f.name] = {"type": typ, "default": default, "group": _field_group(f.name),
                       "widget": widget, "help": meta.get("h", ""), "options": opts,
                       "min": meta.get("lo"), "max": meta.get("hi"),
                       "step": meta.get("st"), "unit": meta.get("u")}
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
        if name in kw and isinstance(kw[name], (list, tuple)):
            seq = kw[name]
            # JSON has no int/tuple types, so a saved ("7",) round-trips as ["7"]. Coerce ELEMENT
            # types too, else attacker_ids stays a tuple of strings and never matches integer vids
            # (silent zero-attacker datasets from the GUI advanced panel). attack_types stays str.
            if name == "attacker_ids":
                kw[name] = tuple(int(x) for x in seq)
            elif name == "attack_types":
                kw[name] = tuple(str(x) for x in seq)
            else:
                kw[name] = tuple(seq)
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
# Telemetry seam (default None -> zero effect, byte-identical): when set to a callable it is invoked
# with a small dict on each INITIATED discretionary lane change (vid, t, from_off, to_off, is_attacker,
# is_faulty, peak_heading_dev_deg). Tests use it to assert lane transitions + heading transients occur.
LANE_CHANGE_HOOK = None
# Telemetry seam (default None -> zero effect, byte-identical): when set to a callable it is invoked
# with a small dict each time a vehicle YIELDS at an unsignalized intersection under gap-acceptance
# (vid, t, node[x,y], dnode, speed, is_attacker, is_faulty). Tests use it to assert yields occur.
GAP_YIELD_HOOK = None


def run_pipeline(cfg: PipelineConfig) -> RunResult:
    validate_config(cfg)
    _ABORT["flag"] = False                            # fresh per run (module state is not reentrant)
    rng = random.Random(cfg.seed)
    wmult = WEATHER_MULT.get(cfg.weather, 1.0)
    la1, la2 = LinkageAuthority(1), LinkageAuthority(2)
    pca, ra = PseudonymCA(), RegistrationAuthority()

    # attack_type is a single-type narrowing selector (F2 fix). It defaults to the sentinel "" (unset),
    # so ANY explicit value -- including "ConstPos", a real catalog member -- narrows the run to just
    # that type, as long as attack_types is left at its default (full catalog). A sentinel/unset
    # attack_type leaves the catalog alone, so the default path (attack_types non-empty) never consults
    # it and the golden digest is byte-identical. An explicitly-set attack_types still wins over it.
    # The narrowing target may be an opt-in "combined" type; attack_claim() renders those.
    if cfg.attack_type and cfg.attack_types == PipelineConfig.attack_types:
        catalog = (cfg.attack_type,)
    else:
        # Fall back to ConstPos (not the "" sentinel) if BOTH are empty, so an explicit
        # attack_types=() never injects "" as a literal no-op "attacker" that poisons ground truth
        # with undetectable positives (audit F-#2). Default path (attack_types non-empty) unaffected.
        catalog = cfg.attack_types or (cfg.attack_type or "ConstPos",)
    fleet_weights = _parse_fleet_mix(cfg.fleet_mix)   # None -> default mixed weights (byte-identical)
    attack_weights = _parse_attack_mix(cfg.attack_mix)  # None -> round-robin catalog (byte-identical)
    # A run carries the MA-visible self-declared station_type (and the vruImpersonation detector) when
    # EITHER genuine VRUs are present (vru_pct>0) OR the opt-in VRU-impersonation attack is selected via
    # any of the attack selectors. When NEITHER holds no beacon ever declares "vru", so gating the
    # station_type field + the extra detector key on this flag keeps the DEFAULT path byte-identical.
    _impersonation_enabled = (any(a in catalog for a in IDENTITY_SPOOF_ATTACKS)
                              or (attack_weights is not None
                                  and any(a in attack_weights for a in IDENTITY_SPOOF_ATTACKS)))
    _emit_station_type = cfg.vru_pct > 0 or _impersonation_enabled
    # The DENM (event-message) layer is active when benign DENMs are requested (denm_rate>0) OR the
    # opt-in FakeHazard attack is selected via any selector (it emits phantom DENMs even at denm_rate=0,
    # so it turns the layer on -- exactly as VruImpersonation enables the station_type machinery). When
    # NEITHER holds, no DENM is ever built, no DENM RNG stream is drawn, and the extra detector key /
    # output files are gated off -> the DEFAULT path is byte-identical.
    _fakehazard_enabled = ("FakeHazard" in catalog
                           or (attack_weights is not None and "FakeHazard" in attack_weights))
    _denm_enabled = cfg.denm_rate > 0 or _fakehazard_enabled
    # per-step emission probabilities (rate/100s -> per-step); benign uses denm_rate, a FakeHazard
    # attacker falls back to a fixed rate when denm_rate==0 so the attack is never a silent no-op.
    _denm_p = cfg.denm_rate * cfg.dt / cfg.denm_rate_window_s
    _denm_fake_p = ((cfg.denm_rate if cfg.denm_rate > 0 else cfg.denm_fake_fallback_rate)
                    * cfg.dt / cfg.denm_rate_window_s)
    total_time = cfg.n_steps * cfg.dt
    net = None
    if cfg.road_network == "grid":
        from .roads import GridNetwork
        net = GridNetwork(cfg.grid_w, cfg.grid_h, cfg.grid_block_m,
                          dropout=cfg.grid_dropout, seed=cfg.seed,
                          arterial_every=cfg.arterial_every,
                          arterial_speed=cfg.arterial_speed_mps, local_speed=cfg.local_speed_mps)
    elif cfg.road_network == "ring":
        from .roads import RingNetwork
        net = RingNetwork(cfg.grid_w, cfg.grid_block_m,   # grid_w = number of ring intersections
                          ring_speed=cfg.arterial_speed_mps)
    elif cfg.road_network == "spider":
        from .roads import CustomNetwork, spider_graph
        net = CustomNetwork(*spider_graph(cfg.grid_w, cfg.grid_h, cfg.grid_block_m))
    elif cfg.road_network == "custom":
        from .roads import CustomNetwork
        net = CustomNetwork(*_parse_custom_network(cfg.custom_network))
    events = _parse_events(cfg.events)                     # validated timeline (possibly empty)

    # ---- scenario-event timeline (deterministic mid-run dynamics; empty -> byte-identical) ----
    _weather_evs = [e for e in events if e["type"] == "weather"]        # chronological
    _demand_evs = [e for e in events if e["type"] == "demand"]
    _closure_evs = [e for e in events if e["type"] == "close_edge"]
    _wave_evs = [e for e in events if e["type"] == "attack_wave"]

    def weather_at(t: float) -> str:
        w = cfg.weather
        for e in _weather_evs:                             # last front at/before t wins
            if e["t"] <= t:
                w = e["value"]
            else:
                break
        return w

    def ev_demand_mult(t: float) -> float:
        m = 1.0
        for e in _demand_evs:
            if e["t"] <= t < e["until"]:
                m *= e["mult"]
        return m

    def attack_wave_active(t: float) -> bool:
        # no attack_wave events -> attacks follow their own windows (unchanged default)
        if not _wave_evs:
            return True
        return any(e["t"] <= t < e["until"] for e in _wave_evs)

    _zone_evs = [e for e in events if e["type"] == "attack_zone"]

    def attack_zone_ok(x: float, y: float, t: float) -> bool:
        # no zones -> attack anywhere; else the attacker's TRUE position must be inside an active
        # zone (a geofenced spoofing campaign, e.g. "attack the downtown core")
        if not _zone_evs:
            return True
        for e in _zone_evs:
            if e["t"] <= t and (e["until"] is None or t < e["until"]):
                if math.hypot(x - e["x"], y - e["y"]) <= e["radius"]:
                    return True
        return False
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
            if attack_weights:                       # weighted per-type assignment (own rng stream)
                atype = _weighted_pick(random.Random(f"{cfg.seed}:atkmix:{vid}"), attack_weights)
            else:                                    # default: round-robin over the catalog
                atype = catalog[atk_counter["n"] % len(catalog)]
            atk_counter["n"] += 1
        else:
            atype = "none"
        cf = bool(cfg.traffic_flow and cfg.car_following and trip is not None)
        if cf:
            # car-following: arrival time is dynamic (congestion) -> despawn on route completion.
            finish_time = None
            # Cert validity window must cover the REALISTIC (congested) trip duration, not just 2x
            # free-flow: under traffic lights + queues a benign trip can take far longer, and if the
            # window is too short the honest cert "expires" mid-trip -> mass false certValidity
            # positives (precision collapse). Budget = 3x free-flow (stop-and-go) + ~half a signal
            # cycle of waiting per intersection on the route + slack.
            eff_min = cfg.trip_speed_min
            if getattr(trip, "caps", None):          # slow zones can undercut trip_speed_min --
                mc = min((c for c in trip.caps if c is not None), default=None)
                if mc is not None:                   # -- budget the cert life for the slowest zone
                    eff_min = min(eff_min, mc)
            free_flow = trip.length / max(1.0, eff_min)
            light_budget = ((trip.length / max(1.0, cfg.grid_block_m)) * (cfg.light_cycle_s * 0.5)
                            if cfg.traffic_lights else 0.0)
            life = 3.0 * free_flow + light_budget + 30.0
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
            if cf and k == n_rot - 1:
                # dynamic (congestion-dependent) despawn can outrun any life estimate; the vehicle's
                # FINAL cert must stay valid for its whole presence, so cap it past the sim end -> a
                # present benign vehicle can never show an "expired" cert (attacks override cvt/cvf).
                vt = max(vt, total_time + cfg.dt)
            pseudonyms.append({"k": k, "i": i_k, "j": j_k, "digest": dig, "valid_from": vf, "valid_to": vt})
            pseudonym_info[dig] = {"i": i_k, "j": j_k, "lv": ctx.linkage_value_for(i_k, j_k),
                                   "ghost": False, "veh_vid": vid}
            gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=dig,
                                            i_period=i_k, valid_from=round(vf, 3), valid_to=round(vt, 3)))
        p0 = pseudonyms[0]
        base_speed = trip.speed if trip is not None else cfg.nominal_speed * (0.85 + 0.3 * vr.random())
        vtype = _pick_vehicle_type(vr, cfg.fleet, fleet_weights)
        tp = VEHICLE_TYPES[vtype]
        speed = base_speed * tp["speed_mult"] * WEATHER_SPEED_MULT.get(
            weather_at(spawn_time) if _weather_evs else cfg.weather, 1.0)
        v = Vehicle(
            vid=vid, spawn_x=float(vid * 20), lane_y=float(vid % 4) * 4.0, speed=speed,
            is_attacker=is_att, priv=None, pub=b"", cert_digest=p0["digest"],
            linkage_ctx=ctx, i_period=p0["i"], j_index=p0["j"], request_hash=req_hash,
            direction=(vr.random() - 0.5) * 0.5,
            wander_amp=vr.random() * 1.5, wander_w=0.15 + vr.random() * 0.25,
            phase=vr.random() * 6.283, gps_q=cfg.gps_quality_floor + vr.expovariate(cfg.gps_quality_lambda),
            is_faulty=is_flt, attack_type=atype, pseudonyms=pseudonyms, colluder=is_coll,
            spawn_time=spawn_time, finish_time=finish_time, trip=trip)
        v.veh_type, v.veh_length = vtype, tp["length"]
        v.idm_a, v.idm_b, v.desired_speed = tp["accel"], tp["decel"], speed
        if cfg.n_lanes > 1:
            _li = vr.randrange(cfg.n_lanes)   # single draw (unchanged): also seeds the lane-change index
            v.lane_off = (_li - (cfg.n_lanes - 1) / 2.0) * cfg.lane_width_m
            v.lane_idx = _li
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
        if is_att and cfg.crl_aware_pct > 0:          # CRL-aware assignment (own rng; default draws none)
            v.crl_aware = random.Random(f"{cfg.seed}:crlaware:{vid}").random() < cfg.crl_aware_pct
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
        gt_kw = dict(true_vehicle_id=true_id, spawn_time=round(spawn_time, 3),
                     is_attacker=is_att, attacker_role=(atype if is_att else "none"),
                     is_faulty=is_flt, veh_type=vtype,
                     colluding_group_id=("colluders" if is_coll else None))
        # ORACLE labels (is_crl_aware / is_vru) are emitted ONLY when their opt-in is on -> a run with
        # neither active is byte-identical (plain R.GtVehicle). Vehicles are never VRUs (is_vru=False).
        gt_vehicle.append(_make_gt_vehicle(cfg, v, False, gt_kw))
        return v

    def make_vru(vid, spawn_time, life, nodes):
        """Create one VRU (pedestrian/cyclist) actor: a slow, benign, SELF-DECLARING transmitter that
        lives near the network but is NOT bound to vehicle car-following (IDM). It carries the same SCMS
        credentials a vehicle does (so the MA sees + LINKS its cert like any device), and its beacon
        declares station_type=vru (MA-visible; set in the broadcast pre-pass). It is never an attacker/
        faulty/colluder and must never be revoked in a correct run. `vid` is its index in `vehicles` (a
        valid list index, so digest_to_vehicle/vrng resolve it). Reached ONLY when vru_pct>0, and it
        draws exclusively from its own string-keyed rng streams -> the vehicle path stays byte-identical."""
        vr = random.Random(f"{cfg.seed}:vru:{vid}")
        la_h1, la_h2 = f"lc1:{vid}", f"lc2:{vid}"
        la_id1, la_id2 = 0x0001, 0x0002
        ctx = DeviceLinkageContext(la_id1, la_id2,
                                   cfg.derive(f"ls1:{vid}", 16), cfg.derive(f"ls2:{vid}", 16))
        la1.register(la_h1, ctx.ls1_0, la_id1)
        la2.register(la_h2, ctx.ls2_0, la_id2)
        req_hash = hashlib.sha256(f"req|{cfg.seed}|{vid}".encode()).hexdigest()[:16]
        true_id = f"veh_{vid:03d}"
        ra.bind(req_hash, true_id)
        j0 = vid % cfg.jmax
        pk = ca.keypair_from_seed(cfg.derive(f"key:{vid}:0"))
        dig = ca.hashed_id8(ca.public_bytes(pk)).hex()
        pca.issue(dig, req_hash, 0, j0, la_h1, la_h2)
        vf = spawn_time
        vt = max(spawn_time + life, total_time + cfg.dt)   # cert covers the VRU's whole presence -> a
        pseudonyms = [{"k": 0, "i": 0, "j": j0, "digest": dig,    # benign VRU never shows an expired cert
                       "valid_from": vf, "valid_to": vt}]
        pseudonym_info[dig] = {"i": 0, "j": j0, "lv": ctx.linkage_value_for(0, j0),
                               "ghost": False, "veh_vid": vid}
        gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=dig,
                                        i_period=0, valid_from=round(vf, 3), valid_to=round(vt, 3)))
        # placement: near a network node but DELIBERATELY off the road centerline (a plaza / pedestrian
        # zone / separated path), i.e. > offroad_tol_m from any road in both axes, so a naive mapOffRoad
        # check WOULD flag them -- which is exactly why a VRU-declared beacon suppresses that detector.
        if nodes:
            nx, ny = nodes[vr.randrange(len(nodes))]
            dx = cfg.offroad_tol_m * (1.3 + 0.7 * vr.random()) * (1.0 if vr.random() < 0.5 else -1.0)
            dy = cfg.offroad_tol_m * (1.3 + 0.7 * vr.random()) * (1.0 if vr.random() < 0.5 else -1.0)
            sx, sy = float(nx) + dx, float(ny) + dy
        else:                                              # no routed network (linear): just offset laterally
            sx, sy = float(vid * 20), float(vid % 4) * 4.0 + cfg.offroad_tol_m * 2.0
        v = Vehicle(
            vid=vid, spawn_x=sx, lane_y=sy, speed=cfg.vru_speed_mps,
            is_attacker=False, priv=None, pub=b"", cert_digest=dig, linkage_ctx=ctx,
            i_period=0, j_index=j0, request_hash=req_hash,
            direction=vr.random() * 6.283, wander_amp=0.5 + vr.random(),
            wander_w=0.1 + vr.random() * 0.2, phase=vr.random() * 6.283,
            gps_q=cfg.gps_quality_floor + vr.expovariate(cfg.gps_quality_lambda), is_faulty=False, attack_type="none",
            pseudonyms=pseudonyms, colluder=False, spawn_time=spawn_time,
            finish_time=(spawn_time + life if cfg.traffic_flow else None), trip=None)
        v.is_vru = True
        v.veh_type, v.veh_length = "vru", 0.5
        vehicles.append(v)
        gt_kw = dict(true_vehicle_id=true_id, spawn_time=round(spawn_time, 3),
                     is_attacker=False, attacker_role="none", is_faulty=False,
                     veh_type="vru", colluding_group_id=None)
        gt_vehicle.append(_make_gt_vehicle(cfg, v, True, gt_kw))
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

        center = net.center if net is not None else None
        # demand surges above the base arrival rate need a faster candidate stream; the extra
        # candidates are thinned back out outside surge windows. No events -> cand_boost = 1.0 ->
        # identical draw sequence -> byte-identical output.
        cand_boost = max([1.0] + [ev_demand_mult(b) for b in sorted({e["t"] for e in _demand_evs})])
        cand_rate = cfg.arrival_rate * cand_boost
        closure_sig: tuple = ()
        rrng = random.Random(f"{cfg.seed}:flow")
        vid, tt = 0, 0.0
        while True:                              # thinning: candidates at max rate, kept per demand
            tt += rrng.expovariate(cand_rate) if cand_rate > 0 else total_time
            if tt >= total_time:
                break
            if cfg.max_total_vehicles and vid >= cfg.max_total_vehicles:
                break                            # cap total spawns (predictable memory bound)
            frac = tt / total_time
            if rrng.random() > demand_mult(frac) * ev_demand_mult(tt) / cand_boost:
                continue                          # thinned out (off-peak / outside a surge)
            if _closure_evs and net is not None:  # timed road closures divert NEW trips (navigation)
                sig = tuple(e["t"] <= tt and (e["until"] is None or tt < e["until"])
                            for e in _closure_evs)
                if sig != closure_sig and hasattr(net, "set_closures"):
                    closure_sig = sig
                    net.set_closures([e["edge"] for e, on in zip(_closure_evs, sig) if on])
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
        if _closure_evs and net is not None and hasattr(net, "set_closures"):
            net.set_closures([])                 # routing closures only affect the spawn pre-pass
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

    # ---- VRUs (opt-in): benign pedestrians/cyclists added AFTER the vehicle fleet so the vehicle RNG
    # sequence is untouched. vids are assigned contiguously past the vehicles (== list index, so the
    # digest_to_vehicle/vrng lookups below resolve them). vru_pct=0 -> this block is skipped entirely
    # -> byte-identical. "fraction of spawned ACTORS" => n_vru/(n_veh+n_vru)=vru_pct, i.e. count/rate is
    # scaled by vru_pct/(1-vru_pct). VRUs draw only from dedicated string-keyed streams. ----
    if cfg.vru_pct > 0:
        _vru_ratio = cfg.vru_pct / (1.0 - cfg.vru_pct)          # vru_pct clamped < 1 by validate_config
        _vru_nodes = net.geometry()["nodes"] if net is not None else None
        if cfg.traffic_flow:
            _vru_rate = cfg.arrival_rate * _vru_ratio
            _vru_flow_rng = random.Random(f"{cfg.seed}:vruflow")
            _vtt = 0.0
            while _vru_rate > 0:
                _vtt += _vru_flow_rng.expovariate(_vru_rate)
                if _vtt >= total_time:
                    break
                make_vru(len(vehicles), _vtt, 90.0, _vru_nodes)
        else:
            _n_vru = int(round(_vru_ratio * cfg.n_vehicles))
            for _ in range(_n_vru):
                make_vru(len(vehicles), 0.0, total_time, _vru_nodes)

    digest_to_vehicle = {d: vehicles[info["veh_vid"]] for d, info in pseudonym_info.items()}
    vrng = {v.vid: random.Random(f"{cfg.seed}:sensor:{v.vid}") for v in vehicles}
    # Dedicated per-vehicle DENM RNG stream (string-keyed), created + drawn ONLY when the DENM layer is
    # enabled, so every other RNG-driven output is untouched and the DEFAULT path is byte-identical.
    vdenm_rng = ({v.vid: random.Random(f"{cfg.seed}:denm:{v.vid}") for v in vehicles}
                 if _denm_enabled else {})

    # ---- Road-Side Units (opt-in): fixed, always-trusted receivers ----
    # RSUs never move and never transmit; they only OBSERVE in-range CAMs and file reports like any
    # receiver, but as trusted infrastructure. They add corroborating, non-gameable evidence near
    # their location. n_rsus=0 (default) -> rsus=[] -> the reception loop is byte-identical.
    rsus: list[Vehicle] = []
    rsu_range = cfg.rsu_range_m if cfg.rsu_range_m > 0 else cfg.radio_range_m
    if cfg.n_rsus > 0 or cfg.rsu_coords.strip():
        spots = _rsu_spots(cfg, net)
        for ri, (sx, sy) in enumerate(spots):
            pk = ca.keypair_from_seed(cfg.derive(f"rsu:{ri}"))
            dig = ca.hashed_id8(ca.public_bytes(pk)).hex()
            rsu = Vehicle(vid=_RSU_VID_BASE + ri, spawn_x=float(sx), lane_y=float(sy), speed=0.0,
                          is_attacker=False, priv=None, pub=b"", cert_digest=dig, linkage_ctx=None,
                          i_period=0, j_index=0, request_hash="", is_rsu=True, wander_amp=0.0,
                          rx_range=rsu_range,
                          pseudonyms=[{"k": 0, "i": 0, "j": 0, "digest": dig,
                                       "valid_from": 0.0, "valid_to": total_time + cfg.dt}])
            pseudonym_info[dig] = {"i": 0, "j": 0, "lv": None, "ghost": False, "veh_vid": rsu.vid}
            digest_to_vehicle[dig] = rsu
            rsus.append(rsu)

    # ---- MA state (updated ONLINE during the loop) ----
    ma_reports: list[dict] = []
    gt_report_labels: list[R.GtReportLabel] = []
    gt_emissions: list[dict] = []
    # DENM (event-message) records. gt_denm carries the ORACLE real-vs-fake flag (label side only);
    # ma_denm_log is the MA-VISIBLE log of observed DENMs (no real/fake flag) that featurize turns into
    # leakage-safe per-subject DENM-count features. Both stay empty (and unwritten) on the default path.
    gt_denm: list[dict] = []
    ma_denm_log: list[dict] = []
    _denm_prev_v: dict[int, float] = {}   # per-vehicle previous true speed -> benign hard-brake trigger
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
    # Cert digests the MA ever OBSERVED declaring station_type="vru" on a beacon (MA-visible; genuine
    # VRUs always, a VruImpersonation attacker while attacking). Drives the cert_status station type so
    # it reflects the OBSERVED declaration, never the oracle is_vru. Empty on the default path.
    vru_declared_digests: set[str] = set()
    counters = {"report": 0, "case": 0, "crl": 0, "denm": 0}
    # Per-colluder RNG for FABRICATED false-report evidence: keyed by seed+vid (deterministic), one
    # stream per colluder so draws accumulate across the run -> the fabricated detector score / pos
    # confidence VARY per report (see the collusion pass) instead of a constant fingerprint. A
    # dedicated stream (not the sensor vrng) leaves every other RNG-driven output byte-identical.
    collude_fab_rng: dict[int, random.Random] = {}
    # Flow-mode collusion victim designation (BUG 6): a STABLE per-vehicle coin decides whether a
    # benign vehicle is a framing victim, so ~victim_pct of benign vehicles are targeted for the whole
    # run (mirrors the fixed-fleet shared victim_pool). Cached per vid; only consulted when victim_pct>0.
    _flow_victim_flag: dict[int, bool] = {}

    def _is_flow_victim(vid: int) -> bool:
        f = _flow_victim_flag.get(vid)
        if f is None:
            f = random.Random(f"{cfg.seed}:collude_victim:{vid}").random() < cfg.victim_pct
            _flow_victim_flag[vid] = f
        return f

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
        w_now = wmult if not _weather_evs else WEATHER_MULT.get(weather_at(t), 1.0)
        sigma_nom = cfg.gps_sigma_m * v.gps_q * w_now
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
        # ---- "combined" family (opt-in): each falsifies MULTIPLE fields mutually inconsistently so it
        # trips several detector families at once (that is the whole point of the family). ----
        elif typ == "Disruptive":
            # lie on ALL THREE fields at once, each inconsistent with the others: erratic position
            # jumps (position-jump), a speed those jumps cannot justify (position/speed + implausible
            # accel from the oscillation), and a heading rotated off the travel direction (heading).
            cx = mx + r.uniform(-55, 55) * k
            cy = my + r.uniform(-55, 55) * k
            cs = max(0.0, mspeed + (18.0 if int(t) % 2 == 0 else -12.0) * k)
            ch = (mheading + 100.0 * k) % 360.0
        elif typ == "PosSpeedInconsistent":
            # the claimed POSITION keeps advancing along the real track (so its displacement implies a
            # normal driving speed) while the claimed SPEED says nearly stopped -- position and speed
            # flatly contradict. Heading stays honest. The near-zero speed makes the displacement read
            # as both a position jump AND a position/speed inconsistency to receivers.
            cs = max(0.0, mspeed - 20.0 * k)
        elif typ == "PosHeadingInconsistent":
            # the claimed POSITION swings hard sideways (perpendicular to travel) while the claimed
            # HEADING keeps pointing straight ahead -- the motion direction and the claimed heading
            # disagree. Speed stays honest. The sideways swing trips the position detectors; the
            # heading-vs-bearing gap trips the heading detector.
            hr = math.radians(mheading + 90.0)               # perpendicular to travel
            swing = 30.0 * k * math.sin(1.3 * t)
            cx, cy = mx + swing * math.cos(hr), my + swing * math.sin(hr)
            ch = mheading                                    # still claim straight-ahead heading
        elif typ == "EventualStop":
            # drives honestly, then "parks": after a short delay it freezes its claimed position while
            # the TRUE vehicle keeps moving, yet keeps claiming a small residual creeping speed. The
            # frozen position with a still-positive speed trips the constant-position-frozen and
            # stale/replay detectors -- "stopped here" contradicts "still moving".
            if (t - v.attack_from) >= 6.0:
                if v.frozen is None:
                    v.frozen = (mx, my)
                cx, cy = v.frozen
                cs = 0.8 + 2.0 * k                           # residual > 0.5 so the frozen detector stays live
        # ---- "identity-spoof" position variant (opt-in; audit GAP #6) ----
        elif typ == "VruPositionSpoof":
            # Declares station_type="vru" (set in the broadcast pre-pass) and CLAIMS a VRU-plausible
            # slow speed -- so the SPEED arm of vruImpersonation (claimed speed >= VRU_MAX_PLAUSIBLE_
            # SPEED_MPS) never trips and the beacon stays "under the bound" -- while TELEPORTING its
            # claimed position on a wide circle each step. The per-interval displacement implies a speed
            # far above any VRU, so the VRU POSITION-plausibility arm of vruImpersonation catches it even
            # though its claimed speed is slow. The mapOffRoad + vehicle-kinematic detectors are gated
            # off for a vru-declared beacon, so this arm is what closes the slow-impersonator gap.
            cs = 2.0                                          # plausible VRU crawl (< the VRU speed bound)
            ang = 2.0 * t
            cx, cy = mx + 60.0 * k * math.cos(ang), my + 60.0 * k * math.sin(ang)
        # NOTE: "VruImpersonation" deliberately has NO branch here -- it drives HONESTLY (position/speed/
        # heading are the true values) and falsifies only its self-declared station_type (set to "vru" in
        # the broadcast pre-pass) to steal the VRU detector exemptions. It is caught by vruImpersonation.
        # NOTE: "FakeHazard" likewise has NO branch here -- its CAMs are honest; the falsification is a
        # PHANTOM DENM (emitted in the broadcast pre-pass) announcing a hazard its own kinematics do not
        # back. It is caught by the denmPlausibility detector.
        return cx, cy, cs, ch

    Z = cfg.detector_z_threshold        # residual must exceed ~this x the broadcast uncertainty to count
    MIN_CONSEC = cfg.detector_min_consec  # consecutive violations required before a reason fires

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
    # vruImpersonation is added ONLY when station types are in play (VRUs or the opt-in impersonation
    # attack). Otherwise no beacon declares "vru", the detector is always 0.0, and appending its
    # detnorm_* to every report would perturb the DEFAULT digest -- so it is gated here to stay
    # byte-identical, mirroring how the station_type report field is gated on _emit_station_type.
    if _emit_station_type:
        DET_KEYS = DET_KEYS + ("vruImpersonation",)
    # denmPlausibility is added ONLY when the DENM layer is enabled. Otherwise no DENM is ever received,
    # the detector is always 0.0, and appending its detnorm_* to every report would perturb the DEFAULT
    # digest -- so it is gated here to stay byte-identical, mirroring the vruImpersonation gate above.
    if _denm_enabled:
        DET_KEYS = DET_KEYS + ("denmPlausibility",)
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
        if getattr(rv, "is_rsu", False):
            return True                          # trusted infrastructure: never rate-limited/distrusted
        return (rv.vid not in revoked_vehicles
                and filed_by.get(reporter_digest, 0) <= cfg.report_budget
                and received_by.get(reporter_digest, 0) < cfg.reputation_max)

    def file_report(t, reporter_digest, subject_digest, subject_veh, reasons, det, conf,
                    cx, cy, px, py, malicious, sig_valid=True, station_type="vehicle"):
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
        if _emit_station_type:                           # MA-VISIBLE self-declared station type on the
            row["station_type"] = station_type          # subject's beacon; key absent by default (byte-identical)
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

    def emit_denm(tx: Vehicle, b_cam: dict, real: bool, event_type: str, t: float) -> None:
        """Append a DECENTRALIZED EVENT MESSAGE (DENM) broadcast for `tx` this step, plus its MA-visible
        log row and its ORACLE ground-truth row. A DENM is a distinct, SIGNED broadcast kind
        (msg_type="denm") announcing a road hazard at a claimed location; it reuses the sender's cert
        (b_cam["digest"]) and its current claimed kinematics, so a receiver can check the announced
        event against the sender's OWN observed CAM state. `real` (a benign vehicle's true event vs a
        FakeHazard attacker's phantom one) is ORACLE-only -- it never touches the broadcast content the
        MA sees. Reached only when the DENM layer is enabled -> no effect on the default path."""
        counters["denm"] += 1
        did = f"dnm_{counters['denm']:06d}"
        ev_x, ev_y, ev_spd = b_cam["cx"], b_cam["cy"], b_cam["cs"]
        # MA-visible plausibility of a brake/stationary hazard from the sender's OWN claimed speed:
        # a real (slow/stopped) sender scores < 1; a phantom hazard from a cruising sender scores > 1.
        plaus = max(0.0, ev_spd) / cfg.denm_implausible_speed_mps
        broadcasts.append(dict(
            veh=tx, digest=b_cam["digest"], cx=ev_x, cy=ev_y, cs=ev_spd, ch=b_cam["ch"],
            conf=b_cam["conf"], ghost=False, x=b_cam["x"], y=b_cam["y"], falsified=(not real),
            msg_count=1, cg=t, sig_ok=True, cvf=b_cam["cvf"], cvt=b_cam["cvt"],
            station_type=b_cam["station_type"], msg_type="denm", event_type=event_type, denm_id=did))
        ma_denm_log.append(dict(                           # MA-VISIBLE (no real/fake flag): observed DENM
            denm_id=did, cert_digest=b_cam["digest"], detection_time=round(t, 3),
            event_type=event_type, claimed_x=round(ev_x, 3), claimed_y=round(ev_y, 3),
            claimed_speed=round(ev_spd, 3), denm_plausibility=round(plaus, 3)))
        gt_denm.append(dict(                               # ORACLE: real-vs-fake flag lives ONLY here
            denm_id=did, t=round(t, 3), true_vehicle_id=f"veh_{tx.vid:03d}", cert_digest=b_cam["digest"],
            event_type=event_type, claimed_x=round(ev_x, 3), claimed_y=round(ev_y, 3),
            sender_speed=round(ev_spd, 3), is_attacker=tx.is_attacker, is_fake=(not real),
            _visibility=R.ORACLE))
        if not real and tx.onset is None:                  # the phantom DENM is this attacker's onset
            tx.onset = t

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
        # static road geometry for the GUI map (drawn under the vehicles; viz-only, not digested)
        try:
            geo = net.geometry() if net is not None else {"nodes": [], "edges": []}
            with open(os.path.join(cfg.out_dir, "network.json"), "w", encoding="utf-8") as fh:
                json.dump({**geo, "road_network": cfg.road_network, "events": events}, fh)
        except OSError:
            pass

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
    # gap-acceptance at UNsignalized intersections: opt-in, and (like _turn) a no-op unless the world
    # supports it (routed car-following). It governs unsignalized nodes only, so with traffic_lights on
    # (every node signalized) it defers entirely to the signal -> effectively active only when _lights is
    # off. When off, none of the yield code below is reached and NO rng is drawn -> byte-identical output.
    _gap = bool(cf_active and cfg.gap_acceptance and not _lights)
    # discretionary (MOBIL) lane changes: opt-in, and (like _turn) a no-op unless the world supports it
    # (multi-lane + routed car-following). When off, none of the code/RNG below is reached.
    _lane_changes = bool(cf_active and cfg.n_lanes > 1 and cfg.lane_changes)
    _lc_time = max(cfg.dt, cfg.lane_change_time_s)        # lateral transition duration (>= one step)
    _lc_cool = max(cfg.dt, 2.0 * _lc_time)               # anti-oscillation: settle before reconsidering
    _lc_half = 0.5 * cfg.lane_width_m                     # half-lane window for lane classification
    _lc_reconsider = min(1.0, 0.25 * cfg.dt)             # ~per-second reconsideration (staggers changes)
    _LC_BSAFE = 4.0                                       # MOBIL safety: max decel imposed on a cut-in follower
    _LC_MIN_SPEED = 3.0                                   # discretionary changes need real motion (m/s)
    _LC_MAXDEV_TAN = math.tan(math.radians(12.0))        # cap the lane-change heading swing to ~12 deg
    lc_rng: dict = {}                                     # per-vehicle string-keyed streams (ON path only)
    # map-matching (HD-map check): distance from a claimed position to the nearest road; a claim far
    # off-road is implausible. Each network defines dist_to_road for its topology (grid lines / ring
    # chords) -> catches lateral/diagonal position offsets regardless of topology.
    def _offroad(x, y):
        return net.dist_to_road(x, y) if net is not None else 0.0

    def _light_green(phase: int, axis_x: bool, t: float) -> bool:
        # `phase` (0/1) is a stable per-intersection 2-colouring from the network (net.node_phase),
        # so adjacent intersections alternate on ANY topology; each axis gets half the cycle. (On a
        # grid this equals the historical (i+j)%2 checkerboard -> grid signal timing is unchanged.)
        offset = (phase % 2) * _half_cycle
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

    def _mobil_decide(v, t, snap, buckets, cx0, cy0, vx, vy, cosh, sinh, vh, vspd, vlen, params):
        """MOBIL-style discretionary lane change on the START-OF-STEP snapshot (order-independent).

        Change to an adjacent lane iff (a) our own lane is blocked (a leader within lookahead), (b) the
        target lane's new follower is NOT forced to brake harder than _LC_BSAFE (safety), and (c) the
        incentive -- our acceleration gain plus a politeness-weighted term for the affected followers --
        exceeds lane_change_threshold. Deterministic: reads only the frozen snapshot + static params."""
        if vspd < _LC_MIN_SPEED:              # discretionary changes need real motion, not a dead crawl
            return                            # (also keeps the heading swing physically plausible)
        INF = math.inf
        # lane-classified nearest leader (fwd>0) / follower (fwd<0), each [gap, speed, length, vid]
        cur_L = [INF, 0.0, cfg.veh_length_m, None]; cur_F = [INF, 0.0, cfg.veh_length_m, None]
        lft_L = [INF, 0.0, cfg.veh_length_m, None]; lft_F = [INF, 0.0, cfg.veh_length_m, None]
        rgt_L = [INF, 0.0, cfg.veh_length_m, None]; rgt_F = [INF, 0.0, cfg.veh_length_m, None]
        for dcx in (-1, 0, 1):
            for dcy in (-1, 0, 1):
                for wvid in buckets.get((cx0 + dcx, cy0 + dcy), ()):
                    if wvid == v.vid:
                        continue
                    wx, wy, wh, wv, wl = snap[wvid]
                    if _ang_diff(wh, vh) > 45.0:                # only same-direction traffic
                        continue
                    dx, dy = wx - vx, wy - vy
                    fwd = dx * cosh + dy * sinh
                    if abs(fwd) > cfg.idm_lookahead_m:
                        continue
                    lat = -dx * sinh + dy * cosh               # +lat = to our LEFT (higher lane index)
                    if abs(lat) <= _lc_half:
                        L, F = cur_L, cur_F
                    elif _lc_half < lat <= 3.0 * _lc_half:
                        L, F = lft_L, lft_F
                    elif -3.0 * _lc_half <= lat < -_lc_half:
                        L, F = rgt_L, rgt_F
                    else:
                        continue
                    if fwd > 0.0:
                        if fwd < L[0]:
                            L[0], L[1], L[2], L[3] = fwd, wv, wl, wvid
                    elif -fwd < F[0]:
                        F[0], F[1], F[2], F[3] = -fwd, wv, wl, wvid
        if cur_L[0] >= cfg.idm_lookahead_m:                    # own lane not blocked -> no reason to move
            return
        a_max, b_dec, v0 = v.idm_a, v.idm_b, v.desired_speed
        a_old = _idm_accel(vspd, v0, cur_L[0], cur_L[1], cur_L[2], a_max, b_dec)

        def _incentive(tgt_L, tgt_F):
            a_new = _idm_accel(vspd, v0, tgt_L[0], tgt_L[1], tgt_L[2], a_max, b_dec)
            d_new = 0.0
            if tgt_F[3] is not None:                           # would-be follower in the target lane
                fdes, fa, fb = params[tgt_F[3]]
                fspd = snap[tgt_F[3]][3]
                a_f_after = _idm_accel(fspd, fdes, tgt_F[0], vspd, vlen, fa, fb)
                if a_f_after < -_LC_BSAFE:                     # safety veto: forces a hard brake
                    return None
                gap_before = (tgt_L[0] + tgt_F[0]) if tgt_L[3] is not None else INF
                a_f_before = _idm_accel(fspd, fdes, gap_before, tgt_L[1], tgt_L[2], fa, fb)
                d_new = a_f_after - a_f_before
            d_old = 0.0
            if cur_F[3] is not None:                           # follower we leave behind benefits
                odes, oa, ob = params[cur_F[3]]
                ospd = snap[cur_F[3]][3]
                a_o_before = _idm_accel(ospd, odes, cur_F[0], vspd, vlen, oa, ob)
                gap_after = (cur_L[0] + cur_F[0]) if cur_L[3] is not None else INF
                a_o_after = _idm_accel(ospd, odes, gap_after, cur_L[1], cur_L[2], oa, ob)
                d_old = a_o_after - a_o_before
            return (a_new - a_old) + cfg.lane_change_politeness * (d_new + d_old)

        best = None                                            # (incentive, target_lane_index)
        if v.lane_idx + 1 <= cfg.n_lanes - 1:                  # consider the LEFT adjacent lane
            inc = _incentive(lft_L, lft_F)
            if inc is not None and inc > cfg.lane_change_threshold:
                best = (inc, v.lane_idx + 1)
        if v.lane_idx - 1 >= 0:                                # consider the RIGHT adjacent lane
            inc = _incentive(rgt_L, rgt_F)
            if inc is not None and inc > cfg.lane_change_threshold and (best is None or inc > best[0]):
                best = (inc, v.lane_idx - 1)
        if best is None:
            return
        new_idx = best[1]
        new_off = (new_idx - (cfg.n_lanes - 1) / 2.0) * cfg.lane_width_m
        # speed-adaptive duration: stretch the transition just enough that the peak lateral velocity
        # (smoothstep peak = 1.5*Delta/T) never swings the heading past ~_LC_MAXDEV_TAN of forward speed
        # -> a slow vehicle changes lanes GENTLY (a few deg), not a physically-implausible sideways lurch.
        dur = max(_lc_time, 1.5 * abs(new_off - v.lane_off) / (_LC_MAXDEV_TAN * max(vspd, 0.5)))
        v.lc_active, v.lc_t0, v.lc_off0, v.lc_off1, v.lc_dur = True, t, v.lane_off, new_off, dur
        v.lane_idx, v.lc_cooldown = new_idx, t + max(_lc_cool, 2.0 * dur)
        if LANE_CHANGE_HOOK is not None:                       # telemetry seam (None by default)
            peak_lat = 1.5 * abs(new_off - v.lc_off0) / dur    # smoothstep peak lateral velocity
            LANE_CHANGE_HOOK(dict(vid=v.vid, t=round(t, 3), from_off=round(v.lc_off0, 3),
                                  to_off=round(new_off, 3), is_attacker=v.is_attacker,
                                  is_faulty=v.is_faulty,
                                  peak_heading_dev_deg=round(math.degrees(
                                      math.atan2(peak_lat, max(0.5, vspd))), 3)))

    def _lane_step(v, t, snap, buckets, cx0, cy0, vx, vy, cosh, sinh, vh, vspd, vlen, params):
        """Maybe start a lane change, then integrate one step of the smooth lateral transition.

        Returns the lateral velocity (m/s) this step so the caller can add the matching brief heading
        deviation -- the realistic benign transient (a real lane change swings the heading a few deg)."""
        if (not v.lc_active) and t >= v.lc_cooldown:
            r = lc_rng.get(v.vid)
            if r is None:
                r = lc_rng[v.vid] = random.Random(f"{cfg.seed}:lanechg:{v.vid}")
            if r.random() < _lc_reconsider:                   # drivers reconsider intermittently
                _mobil_decide(v, t, snap, buckets, cx0, cy0, vx, vy, cosh, sinh, vh, vspd, vlen, params)
        if not v.lc_active:
            return 0.0
        prev = v.lane_off
        elapsed = (t - v.lc_t0) + cfg.dt
        if elapsed >= v.lc_dur:                               # transition complete -> settle in target
            v.lane_off, v.lc_active = v.lc_off1, False
        else:
            p = elapsed / v.lc_dur                            # smoothstep -> zero lateral velocity at ends
            v.lane_off = v.lc_off0 + (v.lc_off1 - v.lc_off0) * (p * p * (3.0 - 2.0 * p))
        return (v.lane_off - prev) / cfg.dt

    def car_follow(active_list, t):
        # VRUs are not car-following actors (no trip/IDM state), so exclude them from the routed
        # kinematics. When no VRUs are present every active vehicle is cf -> this filter is a no-op and
        # the update is byte-identical.
        active_list = [v for v in active_list if v.cf]
        # snapshot start-of-step positions so the update is order-independent (deterministic)
        snap = {v.vid: (v.cur_x, v.cur_y, v.cur_h, v.cur_v, v.veh_length) for v in active_list}
        buckets: dict = {}
        for v in active_list:
            buckets.setdefault((int(v.cur_x // _CF_CELL), int(v.cur_y // _CF_CELL)), []).append(v.vid)
        # static per-vehicle kinematics needed to score a neighbour's IDM accel in a MOBIL decision
        # (positions/speeds always come from `snap`; these don't change within a step). ON path only.
        params = {w.vid: (w.desired_speed, w.idm_a, w.idm_b) for w in active_list} if _lane_changes else None
        # gap-acceptance: deterministic first-come yielding at UNsignalized intersections, computed from
        # the start-of-step `snap` so it is order-independent. Group approaching vehicles by the node they
        # are heading for; at each node the CLOSEST claimant has priority (ties -> lower vid). A vehicle
        # must yield (treat the node as a virtual stopped leader below) if a CONFLICTING cross-traffic
        # claimant outranks it. Priority is a strict total order, so the yield relation is acyclic -> the
        # closest vehicle at every node always makes progress -> no gridlock/starvation. No rng is drawn.
        gap_stop: dict = {}
        if _gap:
            claims: dict = {}
            for w in active_list:
                node, dnode = w.trip.next_node(w.s_pos)
                if node is None or dnode >= cfg.idm_lookahead_m:
                    continue
                claims.setdefault((round(node[0], 2), round(node[1], 2)), []).append((dnode, w.vid))
            for lst in claims.values():
                if len(lst) < 2:                          # a lone approacher has nobody to yield to
                    continue
                lst.sort()                                # (dnode, vid): strict total priority order
                for i in range(1, len(lst)):              # rank 0 is granted the node; the rest may yield
                    dnode_i, vid_i = lst[i]
                    hi = snap[vid_i][2]
                    for j in range(i):                    # a higher-priority CONFLICTING claimant -> yield
                        # cross-traffic conflict = heading differs by more than 45 deg (the same traffic
                        # the IDM leader search skips) but is not near-opposing (>=135 deg: opposing
                        # through movements pass side by side, they do not cross).
                        if 45.0 < _ang_diff(hi, snap[lst[j][1]][2]) < 135.0:
                            gap_stop[vid_i] = dnode_i
                            break
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
                    phase = net.node_phase(node)         # stable 2-colouring (topology-agnostic)
                    axis_x = abs(cosh) >= abs(sinh)      # travelling mostly E-W vs N-S
                    if not _light_green(phase, axis_x, t):
                        stop_gap = max(0.0, dnode - 2.0)  # halt ~2 m before the stop line
                        if stop_gap < best_gap:
                            best_gap, best_v, best_len = stop_gap, 0.0, 0.0
            elif _gap and v.vid in gap_stop:             # yield to conflicting cross-traffic (unsignalized)
                dnode = gap_stop[v.vid]                   # reuse the traffic-light stop mechanism: a virtual
                stop_gap = max(0.0, dnode - 2.0)          # stopped leader ~2 m before the intersection line
                if stop_gap < best_gap:
                    best_gap, best_v, best_len = stop_gap, 0.0, 0.0
                if GAP_YIELD_HOOK is not None:           # telemetry seam (None by default -> digest-safe)
                    GAP_YIELD_HOOK(dict(vid=v.vid, t=round(t, 3), dnode=round(dnode, 3),
                                        speed=round(v.cur_v, 3), is_attacker=v.is_attacker,
                                        is_faulty=v.is_faulty))
            if _turn and best_gap > 0.5:                 # slow into a sharp bend (curve-speed cap)
                td, bend = v.trip.next_turn(v.s_pos)
                if bend >= cfg.turn_min_angle_deg and td < cfg.idm_lookahead_m and td < best_gap:
                    best_gap, best_v, best_len = td, min(v.desired_speed, cfg.turn_speed_mps), 0.0
            v0 = v.desired_speed
            if v.trip.caps is not None:              # per-edge speed limit (highway vs residential)
                cap = v.trip.cap_at(v.s_pos)
                if cap is not None and cap < v0:
                    v0 = cap
            a = _idm_accel(v.cur_v, v0, best_gap, best_v, best_len, v.idm_a, v.idm_b)
            v.cur_v = max(0.0, min(v0, v.cur_v + a * cfg.dt))
            v.s_pos += v.cur_v * cfg.dt
            if v.s_pos >= v.trip.length:
                v.s_pos = v.trip.length
                v.finish_time = t          # route complete -> despawn next step
            v.cur_x, v.cur_y, v.cur_h = v.trip.at_distance(v.s_pos)
            lat_rate = 0.0
            if _lane_changes:              # maybe start / advance a smooth discretionary lane change
                lat_rate = _lane_step(v, t, snap, buckets, cx0, cy0, vx, vy, cosh, sinh, vh, _vv, _vl, params)
            if v.lane_off:                 # offset into this vehicle's lane (perpendicular to heading)
                hr = math.radians(v.cur_h)
                v.cur_x += v.lane_off * -math.sin(hr)
                v.cur_y += v.lane_off * math.cos(hr)
            if lat_rate:                   # a lane change in progress -> lateral velocity swings heading
                v.cur_h = (v.cur_h + math.degrees(math.atan2(lat_rate, max(0.5, v.cur_v)))) % 360.0

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
            vdenm_rng.pop(vid, None)         # (empty/no-op unless the DENM layer is on -> digest-safe)
            lc_rng.pop(vid, None)            # (empty/no-op unless lane_changes is on -> digest-safe)
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
            attacking = (tx.is_attacker and tx.attack_from <= t <= tx.attack_to
                         and attack_wave_active(t) and attack_zone_ok(x, y, t))
            if attacking and cfg.attack_duty_cycle < 1.0:  # intermittent: falsify only in bursts
                period = max(1e-6, cfg.attack_pulse_period_s)
                phase = ((t - tx.attack_from) / period + tx.pulse_phase) % 1.0
                attacking = phase < cfg.attack_duty_cycle
            if attacking and tx.crl_aware:            # CRL-aware: watch the PUBLIC CRL, lie low after a bust
                seen = len(revoked_vehicles) - (1 if tx.vid in revoked_vehicles else 0)
                if seen > tx.crl_seen:                # an accomplice was just revoked -> go dormant
                    tx.dormant_until = t + cfg.crl_dormant_s
                    tx.crl_seen = seen
                if t < tx.dormant_until:              # broadcast honestly until the heat dies down
                    attacking = False
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
            # MA-VISIBLE self-declared station type carried on the beacon. Genuine VRUs always declare
            # "vru"; an IDENTITY_SPOOF_ATTACKS attacker is a moving VEHICLE that FRAUDULENTLY declares
            # "vru" while attacking (so a receiver grants it the VRU detector exemptions) -- either
            # driving honestly at vehicle speed (VruImpersonation) or falsifying its position while
            # claiming a slow speed (VruPositionSpoof). Everyone else declares "vehicle".
            declared_station = "vru" if tx.is_vru else "vehicle"
            if attacking and tx.attack_type in IDENTITY_SPOOF_ATTACKS:
                declared_station = "vru"
            tx.hist.append((cx, cy, cs, ch))
            cert_bad = (t > cvt + 1.0) or (t < cvf - 1.0)
            falsified = attacking and (math.hypot(cx - mx, cy - my) > 1.0 or abs(cs - tspeed) > 1.0
                                       or _ang_diff(ch, theading) > 5.0 or msg_count > 1
                                       or cg < t - 1e-6 or not sig_ok or cert_bad
                                       or (declared_station == "vru" and not tx.is_vru))
            if falsified and tx.onset is None:
                tx.onset = t
            if declared_station == "vru":                # remember the MA-observed declaration per cert
                vru_declared_digests.add(digest)
            b_cam = dict(veh=tx, digest=digest, cx=cx, cy=cy, cs=cs, ch=ch, conf=conf,
                         ghost=False, x=x, y=y, falsified=falsified, msg_count=msg_count,
                         cg=cg, sig_ok=sig_ok, cvf=cvf, cvt=cvt, station_type=declared_station)
            broadcasts.append(b_cam)
            # ---- DENM (event-message) emission (opt-in; only when the DENM layer is enabled) ----
            # A FakeHazard attacker emits a PHANTOM hazard (emergency brake) while cruising -- its own
            # claimed speed contradicts the announced event. A benign vehicle emits a DENM only on a
            # REAL trigger: a hard deceleration to a near-stop, or being stationary -- so the announced
            # event corroborates its own low claimed speed. VRUs/RSUs never emit DENMs. Draws come from
            # the dedicated per-vehicle DENM RNG only, so every other output is unchanged.
            if _denm_enabled and not tx.is_rsu and not tx.is_vru:
                dr = vdenm_rng[tx.vid]
                if attacking and tx.attack_type == "FakeHazard":
                    if _denm_fake_p > 0.0 and dr.random() < _denm_fake_p:
                        emit_denm(tx, b_cam, real=False,
                                  event_type="emergencyElectronicBrakeLight", t=t)
                elif _denm_p > 0.0:
                    prev = _denm_prev_v.get(tx.vid)
                    _denm_prev_v[tx.vid] = tspeed
                    decel = ((prev - tspeed) / cfg.dt) if prev is not None else 0.0
                    stationary = tspeed < 0.5
                    hard_brake = (decel >= cfg.denm_decel_trig_mps2
                                  and tspeed <= cfg.denm_benign_max_speed_mps)
                    if (stationary or hard_brake) and dr.random() < _denm_p:
                        emit_denm(tx, b_cam, real=True,
                                  event_type=("stationaryVehicle" if stationary
                                              else "emergencyElectronicBrakeLight"), t=t)
            if attacking and tx.attack_type == "Sybil":     # fabricate co-located ghost identities
                sr = vrng[tx.vid]
                for gdig in tx.ghosts:
                    cert_first_seen.setdefault(gdig, t)
                    cert_last_seen[gdig] = t
                    broadcasts.append(dict(veh=tx, digest=gdig, cx=cx + sr.uniform(-1, 1),
                                           cy=cy + sr.uniform(-1, 1), cs=cs, ch=ch, conf=conf,
                                           ghost=True, x=x, y=y, falsified=True, msg_count=1,
                                           cg=t, sig_ok=True, cvf=0.0, cvt=total_time,
                                           station_type="vehicle"))

        # per-message ground-truth emission sampling (real CAM broadcasts only; DENMs have their own
        # dedicated ground-truth stream gt_denm, so they are excluded here)
        for b in broadcasts:
            if b["ghost"] or b.get("msg_type") == "denm":
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
        cells = Counter((round(b["cx"] / cfg.sybil_cell_m), round(b["cy"] / cfg.sybil_cell_m),
                         int(b["ch"] // 45) % 8)
                        for b in broadcasts if b.get("msg_type") != "denm")

        # DETECTION pass (receiver-outer): a receiver only hears in-range transmitters, with
        # distance/NLOS/weather/congestion packet loss -> the report graph becomes spatially LOCAL
        # (reporters near the subject) instead of all-to-all, and far-away attackers go unobserved.
        # RSUs are static receivers: appended AFTER vehicles so vehicle-side reception (and its RNG
        # draws) is unchanged -> byte-identical when n_rsus=0 (rsus is empty).
        # VRUs are self-declaring TRANSMITTERS only (they broadcast VAMs, they do NOT run the MA
        # detector pipeline / file reports), so they are excluded from the receiver set. This keeps the
        # reporter population -- hence vehicle revocation precision -- unaffected by adding VRUs. With no
        # VRUs present the filter is a no-op -> byte-identical.
        receivers = [v for v in active_list if not v.is_vru] + rsus
        rx_pos = {rx.vid: rx.true_state(t)[:2] for rx in receivers if not enforced(rx, t)}
        wx_loss = WEATHER_RADIO_LOSS.get(weather_at(t) if _weather_evs else cfg.weather, 0.0)
        # spatial index over broadcasts (cell = radio range) so each receiver only tests transmitters
        # in its own + adjacent cells -> reception is O(active x local density), not O(active^2). Cell
        # size = range means the 3x3 neighbourhood provably contains every in-range pair; candidates
        # are re-sorted into broadcast order so packet-loss RNG (hence output) is byte-identical.
        rng_cell = max(cfg.radio_range_m, 1.0)
        # opt-in soft radio (log-distance path loss + per-link log-normal shadowing): governs whether
        # a link physically closes, replacing the hard d<=rr disc. radio_model=="disc" takes NONE of
        # this branch and draws NO extra rng -> byte-identical to today. See PipelineConfig.radio_model.
        radio_logdist = (cfg.radio_model == "logdistance")
        bcell: dict = {}
        for bi, b in enumerate(broadcasts):
            bcell.setdefault((int(b["x"] // rng_cell), int(b["y"] // rng_cell)), []).append(bi)
        for rx in receivers:
            if enforced(rx, t):
                continue
            rxx, rxy = rx_pos[rx.vid]
            # per-receiver range: an RSU may reach further than vehicles, so it searches a wider cell
            # window (radius = ceil(range/cell)). Vehicles keep rx_range=0 -> range=radio_range_m ->
            # radius 1 -> the original 3x3 -> byte-identical.
            rr = rx.rx_range or cfg.radio_range_m
            if radio_logdist:
                # a favourable shadow can pull a link past rr: widen the candidate window to the cap
                # distance (mean signal RADIO_CAP_SIGMA shadow-std below sensitivity), bounded so the
                # search stays O(local). A - margin (range-extending) widens it further.
                cap = rr * 10.0 ** ((cfg.radio_cap_sigma * cfg.shadowing_sigma_db
                                     - min(0.0, cfg.rx_sensitivity_margin_db))
                                    / (10.0 * cfg.pathloss_exponent))
                cap = max(rr, min(cap, rr * cfg.radio_cap_max_mult))
                rad = max(1, int(math.ceil(cap / rng_cell)))
            else:
                rad = max(1, int(math.ceil(rr / rng_cell)))
            cx0, cy0 = int(rxx // rng_cell), int(rxy // rng_cell)
            cand = []
            for dcx in range(-rad, rad + 1):
                for dcy in range(-rad, rad + 1):
                    cand.extend(bcell.get((cx0 + dcx, cy0 + dcy), ()))
            cand.sort()
            in_range = []
            for bi in cand:
                b = broadcasts[bi]
                if b["veh"].vid == rx.vid:
                    continue
                d = math.hypot(b["x"] - rxx, b["y"] - rxy)
                if not radio_logdist:
                    if d <= rr:                                 # hard range disc (default; unchanged)
                        in_range.append((b, d))
                    continue
                # --- log-distance path loss + log-normal shadowing (soft probabilistic range) ---
                if d > cap:
                    continue                                    # cheap distance cap before the dB math
                # mean received power relative to sensitivity: +10*n*log10(rr/d) dB, so exactly 0 dB at
                # d==rr (calibration -> median range == rr), positive closer, negative past rr. A + margin
                # raises the sensitivity bar (shrinks range); - lowers it (extends). d floored at 1 m.
                mean_db = (10.0 * cfg.pathloss_exponent * math.log10(rr / max(d, 1.0))
                           - cfg.rx_sensitivity_margin_db)
                # per-LINK log-normal shadowing from a dedicated string-keyed stream (seed+tx+rx+step);
                # NOT the global rng, so the disc path's rng sequence stays byte-identical.
                shadow_db = random.Random(
                    f"{cfg.seed}:shadow:{b['digest']}:{rx.vid}:{step}").gauss(0.0, cfg.shadowing_sigma_db)
                if mean_db + shadow_db >= 0.0:                  # received power >= sensitivity(+margin)
                    in_range.append((b, d))
            load = sum(b["msg_count"] for b, _ in in_range)
            cong = min(0.8, max(0.0, (load - cfg.chan_capacity) / max(1, cfg.chan_capacity)) * 0.5)
            reporter_digest = rx.active_pseudonym(t, cfg.rotate_period_s)["digest"]
            for b, dist in in_range:
                loss = cfg.packet_loss_base + cfg.nlos_loss * (dist / rr) + cong + wx_loss
                if loss > 0 and rng.random() < loss:
                    continue                                    # packet dropped on the channel
                if b.get("msg_type") == "denm":
                    # DENM (event message): the receiver checks whether the announced brake/stationary
                    # hazard CORROBORATES the sender's own observed kinematics. The DENM carries the
                    # sender's claimed speed (its own CAM state); a genuine emergency-brake/stationary
                    # sender is slow (score < 1 -> plausible), while a phantom hazard from a cruising
                    # sender scores above 1 -> denmPlausibility fires. This reads MA-VISIBLE evidence
                    # only (the claimed event + the sender's claimed speed); it never consults the oracle
                    # real/fake flag, and benign DENMs (low claimed speed) never trip it -> no false
                    # revocations. An unverifiable (bad-sig) DENM carries no trustworthy content -> skip.
                    if b["sig_ok"]:
                        denm_speed = max(0.0, b["cs"])
                        # Audit GAP #5: the plausibility bound is now EVENT-TYPE aware. An
                        # emergencyElectronicBrakeLight sender that has truly braked is at/below the
                        # post-brake bound (DENM_BENIGN_MAX_SPEED_MPS); one still MOVING NORMALLY above
                        # it announces a brake its own kinematics contradict, EVEN below the generic
                        # 6 m/s line -- closing the slow-in-congestion phantom-brake gap. A stationary/
                        # other hazard keeps the generic bound. Benign DENMs (brake speed <= the benign
                        # max with margin, stationary < 0.5) stay below their bound -> never flagged.
                        thresh = (cfg.denm_benign_max_speed_mps + 0.5  # brake bound DERIVED just above benign
                                  if b.get("event_type") == "emergencyElectronicBrakeLight"
                                  else cfg.denm_implausible_speed_mps)
                        score = denm_speed / thresh
                        if score >= 1.0 and rng.random() <= cfg.report_prob:
                            det = {k: 0.0 for k in DET_KEYS}
                            det["denmPlausibility"] = score
                            file_report(t, reporter_digest, b["digest"], b["veh"],
                                        ["denmPlausibility"], det, b["conf"],
                                        b["cx"], b["cy"], b["cx"], b["cy"], malicious=False,
                                        sig_valid=True, station_type=b["station_type"])
                    continue
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
                det["sybilCoLocation"] = cells[(round(cx / cfg.sybil_cell_m), round(cy / cfg.sybil_cell_m),
                                                int(ch // 45) % 8)] / cfg.sybil_min_certs
                # a receiver only physically hears in-range transmitters, so a claim placing the
                # sender far BEYOND THIS RECEIVER'S range is implausible (excess distance / tolerance).
                # Use rr (the actual per-receiver range used for reception above), not the global
                # radio_range_m: an RSU with a longer rsu_range_m legitimately hears distant honest
                # vehicles and must not flag them as out-of-range.
                det["acceptanceRangeThreshold"] = max(0.0, math.hypot(cx - rxx, cy - rxy)
                                                      - rr) / cfg.art_max_m
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
                elif b["station_type"] == "vru":
                    # VRU-appropriate plausibility, gated on the SELF-DECLARED station type carried on
                    # the received beacon (an MA-VISIBLE field, NOT the oracle is_vru): pedestrians/
                    # cyclists legitimately travel OFF the road centerline and move slowly/erratically,
                    # so the HD-map off-road check and the vehicle-kinematic (IDM-shaped) detectors would
                    # raise benign false positives -> benign false revocations. Suppress exactly those
                    # for a VRU-declared beacon. Detectors that are meaningful regardless of station type
                    # stay ON: sybilCoLocation, signatureVerification, certValidity, acceptanceRange-
                    # Threshold (impossible-distance claim), beaconFrequency (flooding/DoS), staleOrReplay.
                    for _mk in MOTION_KEYS:
                        det[_mk] = 0.0
                    det["mapOffRoad"] = 0.0
                    # The gate above trusts a SELF-DECLARED field, so a moving VEHICLE that declares
                    # station_type="vru" would otherwise dodge every suppressed detector for free. Close
                    # it with TWO arms, both keyed on the DECLARED type + MA-VISIBLE claimed kinematics:
                    #   (1) SPEED arm -- a plausible-VRU beacon claims a few m/s, so a CLAIMED speed
                    #       above the cyclist bound (VRU_MAX_PLAUSIBLE_SPEED_MPS) is a vehicle. cs is the
                    #       noise-free broadcast value (NOT a displacement estimate), so GNSS jitter on a
                    #       slow genuine VRU never inflates it. Catches VruImpersonation (honest driving).
                    #   (2) POSITION arm (audit GAP #6) -- a genuine VRU moves SMOOTHLY at ~vru speed, so
                    #       the displacement of its claimed position since the lagged reference implies at
                    #       most a VRU-grade speed. A declared-VRU whose claimed position JUMPS/teleports
                    #       implies a speed far above the VRU bound even while it CLAIMS a slow speed (so
                    #       the speed arm stays quiet). Catches VruPositionSpoof -- the slow-and-position-
                    #       falsifying impersonator the speed arm alone missed. The tolerance is the VRU
                    #       speed bound over the interval PLUS the broadcast confidence (Z*conf, which
                    #       scales with the VRU's own GNSS noise) PLUS a full multipath-outlier magnitude,
                    #       so GNSS jitter/outliers on a real VRU can NEVER push a genuine VRU to fire.
                    if "vruImpersonation" in DET_KEYS:
                        speed_arm = max(0.0, cs) / cfg.vru_max_plausible_speed_mps
                        dtt_v = max(cfg.dt, t - ref[4])
                        vru_allow = (cfg.vru_max_plausible_speed_mps * dtt_v
                                     + Z * max(conf, 0.5 * cfg.consistency_threshold_m)
                                     + cfg.gps_outlier_mag_m)
                        jump_arm = math.hypot(cx - ref[0], cy - ref[1]) / max(1e-6, vru_allow)
                        det["vruImpersonation"] = max(speed_arm, jump_arm)
                for k in DET_KEYS:
                    st["streak"][k] = st["streak"].get(k, 0) + 1 if det.get(k, 0.0) >= 1.0 else 0
                fired = {k: det[k] for k in DET_KEYS if st["streak"].get(k, 0) >= MIN_CONSEC}
                if not fired:
                    continue
                if rng.random() > cfg.report_prob:
                    continue
                reasons = sorted(fired, key=lambda k: -det[k])
                file_report(t, reporter_digest, digest, tx, reasons, det, conf,
                            cx, cy, ref[0], ref[1], malicious=False, sig_valid=b["sig_ok"],
                            station_type=b["station_type"])

        # COLLUSION pass: colluders file fabricated reports against benign victims. In flow mode
        # victims are chosen dynamically (nearby active benign vehicles); in fixed mode from the list.
        for tx in active_list:
            if not tx.colluder or enforced(tx, t) or not (tx.attack_from <= t <= tx.attack_to):
                continue
            reporter_digest = tx.active_pseudonym(t, cfg.rotate_period_s)["digest"]
            if cfg.traffic_flow:
                txx, txy = rx_pos.get(tx.vid, tx.true_state(t)[:2])
                cand = [v for v in active_list if not v.is_attacker and not enforced(v, t) and not v.is_vru
                        and math.hypot(rx_pos[v.vid][0] - txx, rx_pos[v.vid][1] - txy) <= cfg.radio_range_m]
                if cfg.victim_pct <= 0.0:
                    victim_vehicles = cand[:2]                   # legacy default -> byte-identical
                else:
                    # honor victim_pct in flow too, mirroring the fixed-fleet victim_pool: a STABLE
                    # fraction of benign vehicles are designated victims for the whole run (a per-
                    # vehicle coin keyed by seed, shared across colluders), and a colluder frames the
                    # designated victims currently in range. Stable targeting (vs re-sampling each
                    # step) keeps a colluder's victim set persistent, as in fixed mode.
                    victim_vehicles = [v for v in cand if _is_flow_victim(v.vid)]
            else:
                victim_vehicles = [vehicles[vv] for vv in tx.victims if vv in active and not enforced(active[vv], t)]
            for victim in victim_vehicles:
                subject_digest = victim.active_pseudonym(t, cfg.rotate_period_s)["digest"]
                if rng.random() > cfg.report_prob:
                    continue
                # Fabricate PLAUSIBLE evidence from the colluder's own keyed stream so the detector
                # score and pos-confidence VARY per report within the range genuine misbehavior
                # reports occupy (positionSpeedInconsistency ~1..4, conf ~2..9), instead of a constant
                # fingerprint (was 1.3 / 5.0) an ML model could use as a free collusion oracle. The
                # report still frames this victim; its label stays malicious_false_report.
                cfab = collude_fab_rng.setdefault(tx.vid, random.Random(f"{cfg.seed}:collude_fab:{tx.vid}"))
                det = {k: 0.0 for k in DET_KEYS}
                det["positionSpeedInconsistency"] = cfab.uniform(1.05, 4.0)
                # Every GENUINE in-range report also carries the always-on radio detectors -- a self
                # sybil count (>=1/_SYBIL_MIN) and a beacon rate (>=1/freq_max) -- plus a small
                # staleness. Leaving them exactly 0.0 made every fabricated report separable by those
                # structural zeros (the collusion "oracle" the varied score/conf did NOT remove). Fill
                # them from the SAME formulas genuine reports use with plausible inputs, kept below
                # each detector's fire threshold so positionSpeedInconsistency stays the fired reason.
                det["sybilCoLocation"] = cfab.randint(1, 2) / cfg.sybil_min_certs
                det["beaconFrequency"] = cfab.randint(1, 3) / cfg.freq_max
                det["staleOrReplay"] = cfab.uniform(0.0, 0.15)
                fab_conf = cfab.uniform(2.0, 9.0)
                file_report(t, reporter_digest, subject_digest, victim,
                            ["positionSpeedInconsistency"], det, fab_conf, 0.0, 0.0, 0.0, 0.0, malicious=True)

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
    # When station types are in play each row also carries the MA-VISIBLE self-declared station_type
    # (vehicle|vru) the MA OBSERVED on the cert's beacons -- the leakage-safe signal a model may learn
    # from, distinct from the ORACLE is_vru label in gt_vehicle. It is derived from the OBSERVED
    # declaration (vru_declared_digests), never the oracle is_vru: a VruImpersonation attacker (a real
    # vehicle, is_vru=False) that broadcast station_type="vru" therefore appears as a declared VRU here,
    # exactly as the MA saw it -- so the MA-visible field never leaks the attacker's true type. Neither
    # VRUs nor the impersonation attack present -> plain R.MaCertStatus -> byte-identical.
    def _cert_status_row(d, f):
        kw = dict(cert_digest=d, first_seen=f, last_seen=cert_last_seen[d], valid_from=0.0,
                  valid_to=total_time, issuing_pca="PCA-1",
                  crl_status=("revoked" if pseudonym_info[d]["veh_vid"] in revoked_vehicles else "active"),
                  revocation_time=revoked_vehicles.get(pseudonym_info[d]["veh_vid"]))
        if _emit_station_type:
            st = "vru" if d in vru_declared_digests else "vehicle"
            return _MaCertStatusVru(station_type=st, **kw)
        return R.MaCertStatus(**kw)
    ma_cert_status = [_cert_status_row(d, f) for d, f in cert_first_seen.items()]

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
    # DENM (event-message) outputs: an MA-VISIBLE observed-DENM log + the ORACLE real/fake ground truth.
    # Added to the digest ONLY when the DENM layer is enabled, so the DEFAULT file set (and digest) is
    # byte-identical. Written from bounded accumulators (never streamed), which is fine given the rate.
    if _denm_enabled:
        os.makedirs(os.path.join(cfg.out_dir, "ma"), exist_ok=True)
        os.makedirs(os.path.join(cfg.out_dir, "ground_truth"), exist_ok=True)
        data_files["ma/ma_denm_log.jsonl"] = _write_jsonl(
            os.path.join(cfg.out_dir, "ma", "ma_denm_log.jsonl"),
            sorted(ma_denm_log, key=lambda r: r["denm_id"]))
        data_files["ground_truth/gt_denm_emissions.jsonl"] = _write_jsonl(
            os.path.join(cfg.out_dir, "ground_truth", "gt_denm_emissions.jsonl"),
            sorted(gt_denm, key=lambda r: r["denm_id"]))
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
    p.add_argument("--attack-mix", default="", help="per-type attack weights, e.g. 'ConstPos:0.6,Sybil:0.4'")
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
    p.add_argument("--detector-z-threshold", type=float, default=3.0,
                   help="motion-residual firing point in broadcast-uncertainty sigmas (lower=stricter)")
    p.add_argument("--detector-min-consec", type=int, default=2,
                   help="consecutive per-detector violations before a reason fires (1=fire on first)")
    p.add_argument("--sybil-min-certs", type=int, default=_SYBIL_MIN,
                   help="distinct co-located certs before sybilCoLocation fires (lower=fires readily)")
    p.add_argument("--sybil-cell-m", type=float, default=_CELL_M,
                   help="sybil co-location cell size (m); smaller demands tighter co-location")
    p.add_argument("--crl-aware-pct", type=float, default=0.0,
                   help="fraction of attackers that watch the public CRL and go dormant after a bust")
    p.add_argument("--crl-dormant-s", type=float, default=45.0,
                   help="how long a CRL-aware attacker lies low (broadcasts honestly) after a new revocation")
    p.add_argument("--no-ma-defense", action="store_true", help="disable trusted-reporter gating")
    p.add_argument("--radio-range", type=float, default=500.0, help="reception range (m)")
    p.add_argument("--packet-loss", type=float, default=0.0, help="baseline per-message loss")
    p.add_argument("--nlos", type=float, default=0.0, help="distance-growing obstruction loss (0..1)")
    p.add_argument("--chan-capacity", type=int, default=40, help="in-range CAMs/step before congestion")
    p.add_argument("--radio-model", choices=["disc", "logdistance"], default="disc",
                   help="reachability: disc (hard range) | logdistance (soft path-loss + shadowing)")
    p.add_argument("--pathloss-exponent", type=float, default=2.7,
                   help="log-distance path-loss exponent n (logdistance only)")
    p.add_argument("--shadowing-sigma-db", type=float, default=4.0,
                   help="log-normal shadowing std in dB; 0 = near-hard cutoff (logdistance only)")
    p.add_argument("--rx-sensitivity-margin-db", type=float, default=0.0,
                   help="sensitivity margin dB: + shrinks / - extends range (logdistance only)")
    p.add_argument("--radio-cap-sigma", type=float, default=RADIO_CAP_SIGMA,
                   help="logdistance candidate-cap headroom in shadow std devs (disc ignores it)")
    p.add_argument("--radio-cap-max-mult", type=float, default=RADIO_CAP_MAX_MULT,
                   help="logdistance hard ceiling on candidate cap / range (disc ignores it)")
    # long-running traffic flow
    p.add_argument("--flow", action="store_true", help="traffic-flow mode: vehicles spawn/despawn over time")
    p.add_argument("--duration", type=float, default=0.0, help="flow: sim length in seconds")
    p.add_argument("--arrival-rate", type=float, default=2.0, help="flow: mean vehicles spawned per second")
    p.add_argument("--road", default="linear", choices=["linear", "grid", "ring", "spider", "custom"],
                   help="road network model (spider: --grid = arms, --grid-h = rings)")
    p.add_argument("--custom-network", default="", metavar="JSON_OR_FILE",
                   help='custom map: inline JSON {"nodes":[[x,y]...],"edges":[[a,b]...]} or a file path')
    p.add_argument("--events", default="", metavar="JSON_OR_FILE",
                   help="scenario timeline: inline JSON list of events or a file path")
    p.add_argument("--grid", type=int, default=6, help="grid road network dimension (grid x grid)")
    p.add_argument("--grid-block", type=float, default=120.0, help="grid block spacing (m)")
    p.add_argument("--grid-dropout", type=float, default=0.0,
                   help="remove this fraction of grid roads for an irregular/incomplete grid (0..1)")
    p.add_argument("--lanes", type=int, default=1, help="parallel lanes per road (overtaking; reduces gridlock)")
    p.add_argument("--lane-width", type=float, default=3.5, help="lane width (m) for multi-lane offsets")
    p.add_argument("--lane-changes", action="store_true",
                   help="MOBIL discretionary lane changes (needs --lanes>1 + --flow; realistic benign "
                        "lateral move + heading swing)")
    p.add_argument("--lane-change-time", type=float, default=2.5,
                   help="smooth lane-change lateral transition duration (s)")
    p.add_argument("--lane-change-politeness", type=float, default=0.2,
                   help="MOBIL politeness factor (weight on other vehicles' accel change)")
    p.add_argument("--lane-change-threshold", type=float, default=0.2,
                   help="MOBIL incentive threshold (m/s^2) to commit to a lane change")
    p.add_argument("--light-cycle", type=float, default=24.0, help="traffic-light full cycle (s); half green per axis")
    p.add_argument("--arterial-every", type=int, default=0,
                   help="grid: every Nth row & column is a faster arterial road (0=off)")
    p.add_argument("--arterial-speed", type=float, default=0.0,
                   help="speed limit (m/s) on arterials (grid) / the whole ring (0=uncapped)")
    p.add_argument("--local-speed", type=float, default=0.0,
                   help="grid: speed limit (m/s) on local (non-arterial) roads (0=uncapped)")
    p.add_argument("--od-model", default="uniform", choices=["uniform", "gravity"],
                   help="trip destination law: uniform | gravity (realistic distance-decay trip lengths)")
    p.add_argument("--od-gravity-scale", type=float, default=2.0,
                   help="gravity OD: hop decay scale (smaller -> shorter trips)")
    p.add_argument("--turn-speed", type=float, default=6.0,
                   help="cornering: speed cap (m/s) through a sharp bend when --turn-slowdown is on")
    p.add_argument("--boundary-origins", action="store_true",
                   help="trips originate at the grid perimeter (realistic network sources/sinks)")
    p.add_argument("--n-rsus", type=int, default=0,
                   help="fixed Road-Side Units: static, always-trusted receivers spread over the map (0=off)")
    p.add_argument("--rsu-placement", default="spread",
                   choices=["spread", "perimeter", "center", "corners", "all"],
                   help="where RSUs are placed on the grid")
    p.add_argument("--rsu-range", type=float, default=0.0,
                   help="RSU radio range (m); 0 = same as vehicles' --radio-range")
    p.add_argument("--rsu-coords", default="",
                   help="explicit RSU positions 'x1,y1;x2,y2;...' (overrides --rsu-placement)")
    p.add_argument("--demand", default="uniform", choices=["uniform", "rush", "night"],
                   help="time-varying arrival-demand profile")
    p.add_argument("--fleet", default="mixed", help="'mixed' or a single vehicle class (car/truck/bus/motorcycle)")
    p.add_argument("--fleet-mix", default="", help="custom mixed composition, e.g. 'car:0.6,truck:0.3,bus:0.1'")
    p.add_argument("--trip-speed-min", type=float, default=8.0, help="min desired trip speed (m/s)")
    p.add_argument("--trip-speed-max", type=float, default=18.0, help="max desired trip speed (m/s)")
    p.add_argument("--idm-accel", type=float, default=1.5, help="IDM max acceleration (m/s^2)")
    p.add_argument("--idm-decel", type=float, default=2.0, help="IDM comfortable deceleration (m/s^2)")
    p.add_argument("--idm-time-headway", type=float, default=1.3, help="IDM desired time gap to leader (s)")
    p.add_argument("--idm-min-gap", type=float, default=2.5, help="IDM jam distance (m)")
    p.add_argument("--idm-lookahead", type=float, default=70.0, help="IDM leader search distance (m)")
    p.add_argument("--live-interval", type=float, default=0.0, help="write live_state.json every N sim-seconds (GUI map)")
    p.add_argument("--no-car-following", action="store_true", help="disable IDM car-following")
    p.add_argument("--turn-slowdown", action="store_true", help="slow into sharp grid corners (realer, harder)")
    p.add_argument("--traffic-lights", action="store_true", help="signalized intersections (grid)")
    p.add_argument("--gap-acceptance", action="store_true",
                   help="yield to conflicting cross-traffic at UNSIGNALIZED intersections (first-come "
                        "priority; needs --flow + a routed network; realistic slowing at junctions)")
    p.add_argument("--gps-jam-rate", type=float, default=0.0, help="per-step prob a benign vehicle loses GNSS fix")
    p.add_argument("--gps-quality-floor", type=float, default=0.5,
                   help="best-case per-vehicle GNSS quality (noise-scale floor; higher=worse mean error)")
    p.add_argument("--gps-quality-lambda", type=float, default=1.2,
                   help="rate of the exponential per-vehicle GNSS-quality tail (smaller=heavier tail)")
    p.add_argument("--max-total-vehicles", type=int, default=0, help="flow: cap total spawns (0=unlimited)")
    p.add_argument("--vru-pct", type=float, default=0.0,
                   help="fraction of spawned actors that are VRUs (pedestrians/cyclists; benign, "
                        "self-declaring station_type=vru; 0=none, byte-identical)")
    p.add_argument("--vru-speed", type=float, default=1.8, help="VRU travel speed (m/s; ~1.4 walk .. ~5 cycle)")
    p.add_argument("--denm-rate", type=float, default=0.0,
                   help="benign event-message (DENM) rate: DENMs/veh/100s from a real trigger (0=off, "
                        "byte-identical). FakeHazard emits phantom DENMs regardless of this rate.")
    p.add_argument("--denm-rate-window-s", type=float, default=DENM_RATE_WINDOW_S,
                   help="window (s) the DENM rate is expressed per (rate normalizer)")
    p.add_argument("--denm-fake-fallback-rate", type=float, default=DENM_FAKE_FALLBACK_RATE,
                   help="phantom DENMs/attacker/window a FakeHazard emits when denm-rate==0")
    p.add_argument("--denm-benign-max-speed", type=float, default=DENM_BENIGN_MAX_SPEED_MPS,
                   help="benign DENM trigger: sender speed at/below this (m/s); base of brake bound +0.5")
    p.add_argument("--denm-decel-trig", type=float, default=DENM_DECEL_TRIG_MPS2,
                   help="hard-decel magnitude (m/s^2) that arms a benign brake DENM")
    p.add_argument("--denm-implausible-speed", type=float, default=DENM_IMPLAUSIBLE_SPEED_MPS,
                   help="denmPlausibility generic bound: fires above this claimed speed (m/s; lower=more flags)")
    p.add_argument("--vru-max-plausible-speed", type=float, default=VRU_MAX_PLAUSIBLE_SPEED_MPS,
                   help="a vru-declaring beacon claiming >= this speed (m/s) is a vehicle impersonating a VRU")
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
                         attack_mix=args.attack_mix,
                         attack_duty_cycle=args.attack_duty_cycle,
                         attack_pulse_period_s=args.attack_pulse_period,
                         attack_delay_jitter_s=args.attack_delay_jitter,
                         faulty_pct=args.faulty_pct,
                         weather=args.weather, rotate_period_s=args.rotate_period,
                         collude_pct=args.collude_pct, victim_pct=args.victim_pct,
                         sybil_ghosts=args.sybil_ghosts, ma_defense=not args.no_ma_defense,
                         crl_aware_pct=args.crl_aware_pct, crl_dormant_s=args.crl_dormant_s,
                         radio_range_m=args.radio_range, packet_loss_base=args.packet_loss,
                         nlos_loss=args.nlos, chan_capacity=args.chan_capacity,
                         radio_model=args.radio_model, pathloss_exponent=args.pathloss_exponent,
                         shadowing_sigma_db=args.shadowing_sigma_db,
                         rx_sensitivity_margin_db=args.rx_sensitivity_margin_db,
                         traffic_flow=args.flow, duration_s=args.duration, arrival_rate=args.arrival_rate,
                         road_network=("grid" if (args.flow and args.road == "linear") else args.road),
                         custom_network=_inline_or_file(args.custom_network),
                         events=_inline_or_file(args.events),
                         grid_w=args.grid, grid_h=(args.grid_h or args.grid), grid_block_m=args.grid_block,
                         n_lanes=args.lanes, lane_width_m=args.lane_width, light_cycle_s=args.light_cycle,
                         grid_dropout=args.grid_dropout,
                         arterial_every=args.arterial_every, arterial_speed_mps=args.arterial_speed,
                         local_speed_mps=args.local_speed,
                         demand_profile=args.demand, od_model=args.od_model,
                         od_gravity_scale=args.od_gravity_scale, boundary_origins=args.boundary_origins,
                         n_rsus=args.n_rsus, rsu_placement=args.rsu_placement, rsu_range_m=args.rsu_range,
                         rsu_coords=args.rsu_coords,
                         car_following=not args.no_car_following,
                         turn_slowdown=args.turn_slowdown, turn_speed_mps=args.turn_speed,
                         lane_changes=args.lane_changes, lane_change_time_s=args.lane_change_time,
                         lane_change_politeness=args.lane_change_politeness,
                         lane_change_threshold=args.lane_change_threshold,
                         fleet=args.fleet, fleet_mix=args.fleet_mix,
                         trip_speed_min=args.trip_speed_min, trip_speed_max=args.trip_speed_max,
                         idm_accel=args.idm_accel, idm_decel=args.idm_decel,
                         idm_time_headway=args.idm_time_headway, idm_min_gap=args.idm_min_gap,
                         idm_lookahead_m=args.idm_lookahead,
                         live_interval_s=args.live_interval,
                         traffic_lights=args.traffic_lights, gap_acceptance=args.gap_acceptance,
                         gps_jam_rate=args.gps_jam_rate,
                         max_total_vehicles=args.max_total_vehicles,
                         vru_pct=args.vru_pct, vru_speed_mps=args.vru_speed,
                         vru_max_plausible_speed_mps=args.vru_max_plausible_speed,
                         denm_rate=args.denm_rate,
                         denm_rate_window_s=args.denm_rate_window_s,
                         denm_fake_fallback_rate=args.denm_fake_fallback_rate,
                         denm_benign_max_speed_mps=args.denm_benign_max_speed,
                         denm_decel_trig_mps2=args.denm_decel_trig,
                         denm_implausible_speed_mps=args.denm_implausible_speed,
                         detector_z_threshold=args.detector_z_threshold,
                         detector_min_consec=args.detector_min_consec,
                         sybil_min_certs=args.sybil_min_certs, sybil_cell_m=args.sybil_cell_m,
                         gps_quality_floor=args.gps_quality_floor,
                         gps_quality_lambda=args.gps_quality_lambda,
                         radio_cap_sigma=args.radio_cap_sigma,
                         radio_cap_max_mult=args.radio_cap_max_mult,
                         verbose=True,
                         out_dir=(args.out or "datasets/poc_run"))
    res = run_pipeline(cfg)
    _emit_result(res, args.featurize)
    if args.dump_config:
        _dump_config(cfg, args.dump_config)
    return 0


def _inline_or_file(s: str) -> str:
    """CLI convenience: a JSON-looking value is used inline; anything else is read as a file path."""
    s = (s or "").strip()
    if not s or s.startswith("{") or s.startswith("["):
        return s
    with open(s, encoding="utf-8") as fh:
        return fh.read()


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
