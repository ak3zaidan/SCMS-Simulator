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

import copy
import hashlib
import inspect
import json
import math
import os
import random
import types
from collections import Counter
import dataclasses
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Optional

from .. import __version__
from ..api import channel as _api_channel
from ..api import registry as _api_registry
from ..api.channel import (CAP_CBR, CAP_LEGACY_GLOBAL_RNG, CAP_LINK_STATE, CAP_REACH, CAP_RSSI,
                           CAP_STATEFUL, DELIVERED, LOSS_ADDITIVE_LEGACY,
                           LOSS_INDEPENDENT_SURVIVAL, LinkChannelModelBase, LinkOutcome,
                           PerLinkAdapter, StationSnapshot, StepFrame, Transmission)
from ..api import codec as _api_codec
from ..api import detect as _api_detect
from ..api import profile as _api_profile
from ..api import report as _api_report
from ..api import guard as _pguard
from ..api import integrity as _integrity
from ..api import isolate as _isolate
from ..api import srcgate as _srcgate
from ..api.detect import Observation
from ..api.errors import ConfigError, PluginDriftError            # noqa: F401 (re-exported)
from ..api.rng import RngNamespace
from . import detectors as _detectors                             # registers the `check` builtins
from ..codecs import etsi_rules as _etsi_rules       # stdlib-only; no ASN.1 runtime is touched
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


# Attack types whose falsification has NO amplitude to scale: ConstPos freezes to first-seen position,
# ReversedHeading is a fixed 180-degree flip, DataReplay replays real past state (the lie is staleness),
# and VruImpersonation/FakeHazard broadcast HONEST CAMs (they falsify a declared flag / emit phantom
# DENMs, not a magnitude). attack_magnitude_scale is meaningless for these -> reject it rather than
# silently no-op, so the control is honest.
_UNSCALABLE_MAGNITUDE_TYPES = frozenset({
    "ConstPos", "ReversedHeading", "DataReplay", "VruImpersonation", "FakeHazard"})


def _parse_magnitude_scale(s: str) -> dict[str, float]:
    """Parse 'RandomPos:2.0,ConstPosOffset:0.5' -> {type: scale}: a per-type POSITIVE multiplier on that
    type's falsification magnitude (applied ON TOP of the global attack_intensity dial). Empty/blank ->
    {} (every type scale 1.0 -> byte-identical). Mirrors _parse_attack_mix: each name must be a known,
    magnitude-bearing attack type and each scale a float > 0; raises ValueError on a bad name/value.
    To DROP a type entirely use attack_types/attack_mix, not a scale of 0 (a 0-magnitude attacker would
    be labelled an attacker yet emit no falsification -> mislabelled ground truth)."""
    if not s or not s.strip():
        return {}
    parsed = {}
    for part in s.split(","):
        name, _, sc = part.strip().partition(":")
        name = name.strip()
        if name not in KNOWN_ATTACK_TYPES:
            raise ValueError(f"attack_magnitude_scale has unknown type {name!r}")
        if name in _UNSCALABLE_MAGNITUDE_TYPES:
            raise ValueError(f"attack_magnitude_scale cannot scale {name!r}: it has no falsification "
                             f"magnitude (fixed/honest). Scalable types only.")
        val = float(sc)
        if val <= 0:
            raise ValueError(f"attack_magnitude_scale for {name!r} must be > 0 (got {val}); "
                             f"to drop a type use attack_types/attack_mix, not a 0 scale")
        parsed[name] = val
    if not parsed:
        raise ValueError(f"attack_magnitude_scale parsed to nothing: {s!r}")
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


# =========================================================================== #
# Geometric V2X channel model  (opt-in: radio_model="geometric")
#
# A pure-Python, dependency-free GEMV2-lite: every (tx, rx) pair is classified
# LOS / NLOSv (a vehicle in the way) / NLOSb (a building in the way) and then
# put through the 3GPP TR 37.885 path loss for that state, a Gudmundson AR(1)
# shadowing process carried per link, and a per-packet Nakagami-m fade, before
# an independent-survival composition decides delivery.
#
# Every constant below is transcribed from ONE audited copy in
# datagen/refdata/{pathloss_3gpp_tr37885,nakagami_fading,phy_80211p_profile}.json
# -- see tests/test_geometric_channel.py, which grades the implementation
# against the refdata file rather than against a second copy of the formula.
# =========================================================================== #

# TR 37.885 Table 6.2.1-1: PL_dB = a + b*log10(d_m) + c*log10(fc_GHz).
# NOTE highway LOS uses the standard's literal 32.4 intercept, which sits 0.0478 dB below exact
# free space -- reusing a generic Friis helper here misses the 0.01 dB conformance gate by ~5x.
TR37885_PATHLOSS = {
    "urban_los":   (38.77, 16.7, 18.2),
    "urban_nlos":  (36.85, 30.0, 18.9),     # "NLOSb" in GEMV2 vocabulary: building-blocked
    "highway_los": (32.4,  20.0, 20.0),
}
TR37885_FC_GHZ = 5.9                        # ITS-G5 / DSRC control channel
# Shadow-fading std: NLOSv keeps the LOS sigma because its blockage spread is a separate random
# variable (TR37885_NLOSV below); applying the NLOS 4 dB here would double-count the blockage.
TR37885_SHADOW_SIGMA_DB = {"LOS": 3.0, "NLOSv": 3.0, "NLOSb": 4.0}
TR37885_SHADOW_DECORR_M = {"LOS": 10.0, "NLOSv": 13.0, "NLOSb": 13.0}
# NLOSv additional loss: PL_LOS + max(0, N(mu, sigma)), mu = base + max(0, 15*log10(d) - 41).
# The base selects on BLOCKER HEIGHT vs antenna height, not on distance; the distance term is
# identically zero below 10^(41/15) = 541.17 m, i.e. constant across the whole urban regime.
TR37885_NLOSV = {"both_below": (9.0, 4.5), "one_below": (5.0, 4.0), "both_above": (0.0, 0.0)}
TR37885_BLOCKER_HEIGHT_M = {"car": 1.6, "motorcycle": 1.6, "truck": 3.0, "bus": 3.0}
V2X_ANTENNA_HEIGHT_M = 1.5           # roof-mounted OBU antenna
RSU_ANTENNA_HEIGHT_M = 5.0           # pole-mounted RSU: above a car blocker, below nothing useful
# Small-scale fading (nakagami_fading.m_by_distance_adopted): m = 3 / 1.5 / 1.0 over 0-50 / 50-150 /
# >150 m. Power is Gamma(shape=m, scale=1/m) -- unit MEAN, so fading redistributes power without
# adding any (gammavariate(m, 1/m); using scale=1 would inflate mean power by a factor of m).
NAKAGAMI_M_BANDS = ((50.0, 3.0), (150.0, 1.5), (float("inf"), 1.0))
# Vendored VeReMi-NextGen INET 802.11p profile (phy_80211p_profile.json): 6 Mb/s QPSK-1/2, 10 MHz.
PHY_NOISE_DBM = -110.0               # radioMedium.backgroundNoise.power
PHY_SNIR_THRESHOLD_DB = 4.0          # receiver.snirThreshold -- a hard step, see _per_from_rx_dbm
PHY_CS_THRESHOLD_DBM = -85.0         # ETSI DCC channel-busy probe level (etsi_cam_dcc)
# PPDU airtime for a ~500 B signed CAM at 6 Mb/s in 10 MHz (712 us) plus AIFS(AC_BE) + mean backoff
# (207.5 us). Counting the MAC overhead is the difference between meeting and missing the CBR gate.
PHY_FRAME_AIRTIME_S = (712.0 + 207.5) * 1e-6

# Implementation tunables of the geometry (not standards constants).
GEO_BUILDING_CELL_M = 3.0            # occupancy-raster resolution for the NLOSb blockage test
GEO_BUILDING_MAX_CELLS = 32_000_000  # auto-coarsen the raster rather than blow up memory. One cell
                                     # is ONE BYTE, so this ceiling is 32 MB; it was 6 000 000,
                                     # which the 2 km^2 OSM extract (0.22 M cells) never came near
                                     # but the whole 66 km^2 city does -- InTAS needs 7.41 M cells at
                                     # 3 m and would have been silently coarsened to 6 m, i.e. the
                                     # full-city scene would have been classified at HALF the
                                     # extract's resolution and the comparison between them would
                                     # not have been like-for-like. MEASURED on InTAS: 6 m instead
                                     # of 3 m moves NLOSb at 200 m from 0.2471 to 0.3170 and the
                                     # 0.90-awareness-equivalent range from 338.5 m to 304.7 m --
                                     # and 304.7 m is, to 0.7%, the 306.8 m that
                                     # CROSS-ENGINE-RADIO.md attributes to "the Python instrument on
                                     # MOSAIC's scene", which is how we know that column was
                                     # silently measured at 6 m (docs/realism/FULL-CITY-SCENE.md).
GEO_VEHICLE_CELL_M = 25.0            # uniform grid over vehicle blockers for the NLOSv test
GEO_BLOCKER_HALF_WIDTH_M = 1.0       # a vehicle body blocks the LOS line within this lateral offset
GEO_ENDPOINT_CLEAR_M = 6.0           # ignore raster hits this close to either antenna: the road
                                     # graph is RDP-simplified (10 m) and the raster has a ~1-cell
                                     # halo, so an antenna can nominally land on a building cell
GEO_FADING_HEADROOM_DB = 10.0        # candidate-window headroom for a favourable Nakagami fade


def tr37885_pathloss_db(state: str, d_m: float, fc_ghz: float = TR37885_FC_GHZ) -> float:
    """3GPP TR 37.885 mean path loss in dB. `state` is a key of TR37885_PATHLOSS.

    Distance is floored at 1 m (the formulas diverge at 0 and are not defined in the reactive near
    field anyway). This is the ONLY place the constants are evaluated."""
    try:
        a, b, c = TR37885_PATHLOSS[state]
    except KeyError:
        raise ValueError(f"unknown TR 37.885 state {state!r} "
                         f"(have {sorted(TR37885_PATHLOSS)})") from None
    return a + b * math.log10(max(float(d_m), 1.0)) + c * math.log10(fc_ghz)


def tr37885_nlosv_mu_db(base_db: float, d_m: float) -> float:
    """NLOSv mean extra loss: base + max(0, 15*log10(d) - 41) (zero distance term below 541.17 m)."""
    return base_db + max(0.0, 15.0 * math.log10(max(float(d_m), 1.0)) - 41.0)


def nakagami_m_for_distance(d_m: float) -> float:
    """Nakagami shape factor for a link of this length (3 / 1.5 / 1.0 over 0-50 / 50-150 / >150 m)."""
    for upper, m in NAKAGAMI_M_BANDS:
        if d_m <= upper:
            return m
    return NAKAGAMI_M_BANDS[-1][1]


def hidden_terminal_fraction(d_m: float, sense_r_m: float) -> float:
    """Fraction of a receiver's sensing disc that the TRANSMITTER cannot carrier-sense.

    Exact two-disc geometry: with equal sensing radius R and separation d, the shared (mutually
    sensed) area is 2R^2*acos(d/2R) - (d/2)*sqrt(4R^2 - d^2); everything else in the receiver's disc
    holds terminals that are hidden from the transmitter. 0 at d = 0, 1 at d >= 2R."""
    R = max(float(sense_r_m), 1e-9)
    d = max(0.0, float(d_m))
    if d >= 2.0 * R:
        return 1.0
    shared = 2.0 * R * R * math.acos(d / (2.0 * R)) - (d / 2.0) * math.sqrt(max(0.0, 4.0 * R * R - d * d))
    return max(0.0, min(1.0, 1.0 - shared / (math.pi * R * R)))


class _BuildingRaster:
    """Uniform occupancy grid over building footprints + a supercover ray march for LOS testing.

    Pure Python/bytearray -- shapely is deliberately NOT a dependency (see PHASE2-DESIGN.md). Each
    polygon contributes its INTERIOR (even-odd scanline fill at cell centres) and its WALLS (the
    edges stamped by the same DDA used for queries), so a footprint thinner than one cell still
    blocks. `blocked()` walks only the cells the segment actually crosses and exits on the first
    hit, so an urban query costs a handful of lookups rather than a polygon sweep."""

    def __init__(self, polygons, cell_m: float = GEO_BUILDING_CELL_M,
                 max_cells: int = GEO_BUILDING_MAX_CELLS):
        pts = [p for ring in polygons for p in ring]
        if not pts:
            raise ValueError("_BuildingRaster needs at least one polygon")
        self.x0 = min(p[0] for p in pts) - cell_m
        self.y0 = min(p[1] for p in pts) - cell_m
        x1 = max(p[0] for p in pts) + cell_m
        y1 = max(p[1] for p in pts) + cell_m
        # auto-coarsen instead of allocating an unbounded grid for a very large extract
        while True:
            nx = max(1, int((x1 - self.x0) / cell_m) + 1)
            ny = max(1, int((y1 - self.y0) / cell_m) + 1)
            if nx * ny <= max_cells:
                break
            cell_m *= 2.0
        self.cell = float(cell_m)
        self.nx, self.ny = nx, ny
        self.grid = bytearray(nx * ny)
        self.n_polygons = len(polygons)
        for ring in polygons:
            self._fill(ring)
            for i in range(len(ring)):
                ax, ay = ring[i]
                bx, by = ring[(i + 1) % len(ring)]
                self._stamp(ax, ay, bx, by)
        self.occupied_cells = sum(1 for v in self.grid if v)

    def _fill(self, ring) -> None:
        """Even-odd scanline fill of the ring interior at cell-centre resolution."""
        ys = [p[1] for p in ring]
        iy_lo = max(0, int((min(ys) - self.y0) / self.cell))
        iy_hi = min(self.ny - 1, int((max(ys) - self.y0) / self.cell))
        n = len(ring)
        for iy in range(iy_lo, iy_hi + 1):
            yc = self.y0 + (iy + 0.5) * self.cell
            xs = []
            for i in range(n):
                ax, ay = ring[i]
                bx, by = ring[(i + 1) % n]
                if (ay > yc) != (by > yc):
                    xs.append(ax + (yc - ay) * (bx - ax) / (by - ay))
            xs.sort()
            row = iy * self.nx
            for k in range(0, len(xs) - 1, 2):
                ix_lo = max(0, int(math.ceil((xs[k] - self.x0) / self.cell - 0.5)))
                ix_hi = min(self.nx - 1, int((xs[k + 1] - self.x0) / self.cell - 0.5))
                for ix in range(ix_lo, ix_hi + 1):
                    self.grid[row + ix] = 1

    def _stamp(self, x0, y0, x1, y1) -> None:
        for ix, iy, _t in self._cells(x0, y0, x1, y1):
            if 0 <= ix < self.nx and 0 <= iy < self.ny:
                self.grid[iy * self.nx + ix] = 1

    def _cells(self, x0, y0, x1, y1):
        """Amanatides-Woo supercover traversal: yields (ix, iy, t) for every cell the segment
        crosses, t being the normalised position along the segment where the cell is entered."""
        cell = self.cell
        ix = int(math.floor((x0 - self.x0) / cell))
        iy = int(math.floor((y0 - self.y0) / cell))
        ix1 = int(math.floor((x1 - self.x0) / cell))
        iy1 = int(math.floor((y1 - self.y0) / cell))
        dx, dy = x1 - x0, y1 - y0
        stepx = 1 if dx > 0 else -1
        stepy = 1 if dy > 0 else -1
        inf = float("inf")
        tdx = abs(cell / dx) if dx else inf
        tdy = abs(cell / dy) if dy else inf
        if dx:
            nxb = self.x0 + (ix + (1 if dx > 0 else 0)) * cell
            tmx = (nxb - x0) / dx
        else:
            tmx = inf
        if dy:
            nyb = self.y0 + (iy + (1 if dy > 0 else 0)) * cell
            tmy = (nyb - y0) / dy
        else:
            tmy = inf
        t = 0.0
        guard = 4 * (abs(ix1 - ix) + abs(iy1 - iy)) + 8
        for _ in range(guard):
            yield ix, iy, t
            if ix == ix1 and iy == iy1:
                return
            if tmx < tmy:
                ix += stepx
                t = tmx
                tmx += tdx
            else:
                iy += stepy
                t = tmy
                tmy += tdy
            if t > 1.0:
                return

    def blocked(self, x0, y0, x1, y1, endpoint_clear_m: float = GEO_ENDPOINT_CLEAR_M) -> bool:
        """True if the segment crosses an occupied cell, ignoring the first/last `endpoint_clear_m`."""
        length = math.hypot(x1 - x0, y1 - y0)
        if length <= 2.0 * endpoint_clear_m:
            return False
        skip = endpoint_clear_m / length
        nx, ny, grid = self.nx, self.ny, self.grid
        for ix, iy, t in self._cells(x0, y0, x1, y1):
            if t < skip or t > 1.0 - skip:
                continue
            if 0 <= ix < nx and 0 <= iy < ny and grid[iy * nx + ix]:
                return True
        return False


class _VehicleBlockerIndex:
    """Uniform grid over the step's vehicle positions (same structure as the broadcast spatial hash
    at the reception choke point) answering "is a vehicle body sitting on this LOS line?"."""

    __slots__ = ("cell", "buckets")

    def __init__(self, cell_m: float = GEO_VEHICLE_CELL_M):
        self.cell = float(cell_m)
        self.buckets: dict = {}

    def rebuild(self, entries) -> None:
        """`entries` = iterable of (vid, x, y, blocker_height_m)."""
        self.buckets = {}
        c = self.cell
        for vid, x, y, h in entries:
            self.buckets.setdefault((int(x // c), int(y // c)), []).append((vid, x, y, h))

    def tallest_blocker(self, x0, y0, x1, y1, skip_a: int, skip_b: int,
                        half_width_m: float = GEO_BLOCKER_HALF_WIDTH_M) -> float:
        """Height of the tallest vehicle whose body intersects the segment (0.0 = clear).

        Only the cells the segment actually passes through are visited -- sampled every `cell` metres
        along the line with its 3x3 neighbourhood, which provably covers every cell within one cell
        (>> half_width_m) of the segment. Enumerating the segment's bounding box instead is
        O(length^2/cell^2) and is what made a 700 m link cost 841 bucket probes instead of ~80."""
        dx, dy = x1 - x0, y1 - y0
        ll = dx * dx + dy * dy
        if ll <= 1e-9:
            return 0.0
        c = self.cell
        length = math.sqrt(ll)
        steps = int(length / c) + 1
        best = 0.0
        get = self.buckets.get
        seen = set()
        for k in range(steps + 1):
            s0 = min(1.0, (k * c) / length)
            bx = int((x0 + s0 * dx) // c)
            by = int((y0 + s0 * dy) // c)
            for cx in (bx - 1, bx, bx + 1):
                for cy in (by - 1, by, by + 1):
                    key = (cx, cy)
                    if key in seen:
                        continue
                    seen.add(key)
                    for vid, vx, vy, h in get(key, ()):
                        if vid == skip_a or vid == skip_b or h <= best:
                            continue
                        s = ((vx - x0) * dx + (vy - y0) * dy) / ll
                        if not (0.0 < s < 1.0):      # strictly BETWEEN the two antennas
                            continue
                        px, py = x0 + s * dx, y0 + s * dy
                        if (vx - px) ** 2 + (vy - py) ** 2 <= half_width_m * half_width_m:
                            best = h
        return best


# =========================================================================== #
# The three built-in channel models, on the published `LinkChannelModel` interface.
#
# MIGRATION PRINCIPLE (PLUGIN-ARCHITECTURE.md 8.1): CODE MOTION ONLY. The refactor must not change
# WHICH FUNCTION IS CALLED WITH WHICH ARGUMENTS IN WHICH ORDER. Everything else follows -- and the
# five gates in 8.4 (goldens, global-RNG draw parity, per-link outcome parity, schema equality,
# two-run determinism) are what make that reviewable rather than merely lucky.
#
# `disc` and `logdistance` are GRANDFATHERED: they draw the packet-loss coin from the engine's
# global `rng` (the reception loop's `rng.random() < loss`) and compose loss ADDITIVELY (a sum that
# can exceed 1.0), which is exactly what the plugin contract forbids. Rather than change it -- which
# would move 0bd93655... -- they DECLARE `legacy_global_rng` + `loss_composition:additive_legacy`,
# the resolver REFUSES both from any non-built-in, and the manifest records the grandfathering. A
# deliberate, documented wart with a scheduled close (roadmap phase 6), not an oversight.
#
# Two engine facts the interface has to carry, and the memo's single `reach_m` does not:
#   * reach is PER RECEIVER (an RSU's `rsu_range_m` legitimately exceeds a vehicle's), and
#   * under `logdistance` the candidate-search WINDOW (shadow-widened) and the declared DELIVERY
#     reach (`rr`, what `acceptanceRangeThreshold` bounds on) are DIFFERENT NUMBERS.
# Hence `reach_m_for(rx)` and `window_m(rx)` as separate methods. Collapsing them moves 939b4faa...
# =========================================================================== #

#: Bytes on the wire for one native_v1 PDU. Both engines currently hard-code 300 B
#: (`SignedCam.java:18`, `Dcc.java:81`); TS 103 097 signer alternation (digest vs certificate) is
#: worth ~150-200 B, which is why every CBR estimate is systematically wrong and why the real number
#: has to come from a `MessageCodec` (roadmap phase 5). Recorded here so the seam exists.
NATIVE_WIRE_SIZE_BYTES = 300


class DiscChannel(LinkChannelModelBase):
    """`radio_model="disc"` (the DEFAULT): the hard range disc, `heard iff d <= rr`.

    Draws nothing itself. The delivery coin is the engine's own global-`rng` additive-loss test,
    which is precisely the grandfathered behaviour -- see the module note above.
    """

    plugin_id = "disc"
    interface_version = _api_channel.INTERFACE_VERSION

    #: WAIVERS ARE DATA (the Django doctrine): a built-in states what it legitimately cannot pass
    #: rather than the suite being edited around it, and the justification travels into
    #: `conformance_report.json` and from there into the manifest.
    conformance_waivers = {
        "C9_monotone_in_distance":
            "C9's attenuation arm asks that delivery depend on distance at all across a 19x "
            "distance ratio. `disc` is the deliberate unit-disc IDEALISATION -- `heard iff "
            "d <= rr`, PDR exactly 1.0 at every distance inside the disc and 0.0 outside, no rssi "
            "at all -- so it fails that arm BY DEFINITION, not by defect. It is the default only "
            "because it is the cheapest and the historical baseline `0bd93655...` is pinned to it; "
            "any study of range-dependent reception must use `logdistance` or `geometric`. The "
            "monotone arms (adjacent and cumulative) still run and still hold.",
    }

    def __init__(self, *, range_m: float):
        self.reach_m = float(range_m)

    @classmethod
    def from_plugin(cls, *, params, rng, env):
        return cls(range_m=float(params.get("range_m", env["radio_range_m"])))

    def capabilities(self):
        return frozenset({CAP_REACH, CAP_LEGACY_GLOBAL_RNG, LOSS_ADDITIVE_LEGACY})

    def reach_m_for(self, rx: StationSnapshot) -> float:
        # `rx.rx_range or cfg.radio_range_m` -- an RSU may reach further than a vehicle.
        return rx.rx_range_m or self.reach_m

    def evaluate(self, tx, rx, d_m, txn):
        return DELIVERED if d_m <= (rx.rx_range_m or self.reach_m) else None


class LogDistanceChannel(LinkChannelModelBase):
    """`radio_model="logdistance"`: log-distance path loss + per-link log-normal shadowing.

    Mean received power relative to sensitivity is `10*n*log10(rr/d) - margin` dB, so it is exactly
    0 dB at `d == rr` (calibration: median range == `radio_range_m`); a link closes iff
    `mean + shadow >= 0`.

    The shadowing stream is keyed on the sender's CERT DIGEST and the step
    (`f"{seed}:shadow:{digest}:{rx_vid}:{step}"`). That is a known correctness bug -- a pseudonym
    rotation resamples the channel, and the draw is neither reciprocal nor persistent -- and it is
    DELIBERATELY PRESERVED here because fixing it moves 939b4faa... It is scheduled for a re-pin,
    not smuggled into a refactor.
    """

    plugin_id = "logdistance"
    interface_version = _api_channel.INTERFACE_VERSION

    #: Conformance waivers this implementation DECLARES -- Django's `django_test_skips` doctrine:
    #: a backend states what it legitimately cannot pass as DATA it ships, with a written
    #: justification that travels into `conformance_report.json` and from there into the manifest.
    #: Never an edit to the suite. Measured 2026-08-31 by `scms-poc conformance --ref logdistance`.
    conformance_waivers = {
        "C8_reach_honesty":
            "reach_m is a MEDIAN-range calibration, not an upper bound. The model is 0 dB at "
            "d == radio_range_m by construction and a link closes iff mean + shadow >= 0, so a "
            "favourable log-normal shadow legitimately closes links past it -- that IS the model. "
            "Measured on the v1 harness at range_m=500, sigma=4 dB, n=2.7: 999 of 7071 delivered "
            "links (14.13 %) land beyond the declared reach, worst excess 940.1 m, and 296 of them "
            "(4.19 % of deliveries) exceed art_max_m=150 m and would therefore score >= 1.0 on "
            "acceptanceRangeThreshold for an HONEST sender at its true position. That false-positive "
            "channel is real but does not fire in the default 5x5 / 120 m grid (measured: 0 ART "
            "false positives over 2953 reports, seed 17) because the whole map is smaller than "
            "reach + tolerance. Closing it means either declaring the widened window as the reach "
            "(which moves 939b4faa...) or giving the model a hard cutoff (which is a different "
            "model); it is scheduled with the roadmap phase 6 re-pin, not smuggled into a refactor."}

    def __init__(self, *, seed: int, range_m: float, pathloss_exponent: float,
                 shadowing_sigma_db: float, rx_sensitivity_margin_db: float,
                 cap_sigma: float, cap_max_mult: float):
        self.seed = int(seed)
        self.reach_m = float(range_m)
        self.n = float(pathloss_exponent)
        self.sigma_db = float(shadowing_sigma_db)
        self.margin_db = float(rx_sensitivity_margin_db)
        self.cap_sigma = float(cap_sigma)
        self.cap_max_mult = float(cap_max_mult)
        self.step = -1
        # A favourable shadow can pull a link past rr, so the candidate window widens to the cap
        # distance and is bounded so the cell search stays O(local). A - margin widens it further.
        self._widen = 10.0 ** ((self.cap_sigma * self.sigma_db - min(0.0, self.margin_db))
                               / (10.0 * self.n))

    @classmethod
    def from_plugin(cls, *, params, rng, env):
        g = lambda k: params[k] if k in params else env[k]            # noqa: E731
        return cls(seed=env["seed"], range_m=g("radio_range_m"),
                   pathloss_exponent=g("pathloss_exponent"),
                   shadowing_sigma_db=g("shadowing_sigma_db"),
                   rx_sensitivity_margin_db=g("rx_sensitivity_margin_db"),
                   cap_sigma=g("radio_cap_sigma"), cap_max_mult=g("radio_cap_max_mult"))

    def capabilities(self):
        return frozenset({CAP_REACH, CAP_LEGACY_GLOBAL_RNG, LOSS_ADDITIVE_LEGACY})

    def begin_step(self, frame: StepFrame) -> None:
        self.step = frame.step

    def reach_m_for(self, rx: StationSnapshot) -> float:
        # NOT the widened window: `acceptanceRangeThreshold` bounds on rr under this model
        # (run.py's `art_reach = cap if geo_chan is not None else rr`).
        return rx.rx_range_m or self.reach_m

    def window_m(self, rx: StationSnapshot) -> float:
        rr = rx.rx_range_m or self.reach_m
        return max(rr, min(rr * self._widen, rr * self.cap_max_mult))

    def evaluate(self, tx, rx, d_m, txn):
        rr = rx.rx_range_m or self.reach_m
        mean_db = 10.0 * self.n * math.log10(rr / max(d_m, 1.0)) - self.margin_db
        shadow_db = random.Random(
            f"{self.seed}:shadow:{txn.cert_digest}:{rx.vid}:{self.step}").gauss(0.0, self.sigma_db)
        return DELIVERED if mean_db + shadow_db >= 0.0 else None


class GeometricChannel(LinkChannelModelBase):
    """The `radio_model="geometric"` link model: classification -> path loss -> AR(1) shadowing ->
    per-packet Nakagami fade -> independent-survival composition.

    RNG DISCIPLINE. Every draw comes from a dedicated string-keyed stream carried per link, never
    from the pipeline's global `rng`, so switching the model on cannot perturb any other stream --
    and the default `disc` path never constructs this object at all.

      * shadowing  : one Random per UNORDERED pair `f"{seed}:shadow2:{lo}:{hi}"`, advanced exactly
                     once per step (the channel is reciprocal), keyed on the TRUE vehicle ids so a
                     pseudonym rotation no longer resamples the channel (the old per-step draw at
                     the cert digest was both non-reciprocal and memoryless -- roadmap G5).
      * per-packet : one Random per ORDERED pair `f"{seed}:geo:{tx}:{rx}"` supplying the NLOSv
                     blockage draw, the Nakagami fade and the delivery coin.
      * canyon     : the synthetic-map NLOSb fallback shares the shadowing stream, so it is
                     re-decided on the same spatial cadence as the shadowing it accompanies.
    """

    plugin_id = "geometric"
    interface_version = _api_channel.INTERFACE_VERSION

    def __init__(self, cfg, buildings=None, dt: float = 1.0):
        self.seed = cfg.seed
        self.dt = max(float(dt), 1e-9)
        self.env = cfg.radio_env
        self.los_state = "urban_los" if self.env == "urban" else "highway_los"
        self.nlos_state = "urban_nlos"      # TR 37.885 reuses the urban NLOS formula on highways
        self.tx_dbm = float(cfg.radio_tx_power_dbm)
        self.sens_dbm = float(cfg.radio_rx_sensitivity_dbm)
        # Decode floor: the vendored PHY bounds range by SENSITIVITY, but keep the SNIR arm explicit
        # so a noisier configuration bites. See _per_from_rx_dbm for why this is a hard step.
        self.decode_floor_dbm = max(self.sens_dbm, PHY_NOISE_DBM + PHY_SNIR_THRESHOLD_DB)
        self.canyon_per_m = max(0.0, float(cfg.radio_nlosb_density_per_km)) / 1000.0
        self.buildings = _BuildingRaster(buildings) if buildings else None
        self._shadow: dict = {}             # unordered pair -> per-link AR(1) + classification state
        self._packet: dict = {}             # ordered pair -> per-packet Random
        self.blockers = _VehicleBlockerIndex()
        self.step = -1
        self.stats = Counter()
        # candidate-window cap: the distance at which the MEDIAN LOS signal is cap_sigma shadow
        # sigmas plus a fading headroom below the decode floor. Bounded by radio_cap_max_mult *
        # radio_range_m exactly as the logdistance branch is, so the cell search stays O(local) --
        # at realistic power the TR 37.885 urban-LOS slope (b = 16.7) puts the unbounded cap in the
        # tens of kilometres, so the multiplier, not the physics, is the performance dial here.
        a, b, c = TR37885_PATHLOSS[self.los_state]
        budget = (self.tx_dbm - self.decode_floor_dbm
                  + cfg.radio_cap_sigma * TR37885_SHADOW_SIGMA_DB["LOS"] + GEO_FADING_HEADROOM_DB)
        reach = 10.0 ** ((budget - a - c * math.log10(TR37885_FC_GHZ)) / b)
        # `reach_m` is the interface name; `cap_m` remains a read-only alias for one minor version.
        self.reach_m = max(1.0, min(reach, cfg.radio_range_m * cfg.radio_cap_max_mult))
        # carrier-sense radius for the hidden-terminal term, on the same LOS budget at -85 dBm
        sense_budget = self.tx_dbm - PHY_CS_THRESHOLD_DBM
        self.sense_m = min(self.reach_m,
                           10.0 ** ((sense_budget - a - c * math.log10(TR37885_FC_GHZ)) / b))

    @property
    def cap_m(self) -> float:
        """Deprecated alias for :attr:`reach_m` (kept for one minor version; PLUGIN-ARCH 8.2)."""
        return self.reach_m

    @classmethod
    def from_plugin(cls, *, params, rng, env):
        """Built-in construction contract. `rng` (an RngNamespace) is deliberately unused: this
        model already carries its OWN string-keyed per-link streams and draws zero from any shared
        object -- which is exactly the property the contract asks third parties to reproduce."""
        return cls(env["config"], buildings=env.get("buildings"), dt=env["dt"])

    def capabilities(self):
        return frozenset({CAP_RSSI, CAP_LINK_STATE, CAP_REACH, CAP_CBR, CAP_STATEFUL,
                          LOSS_INDEPENDENT_SURVIVAL})

    # -- per-step ---------------------------------------------------------------------------- #
    def begin_step(self, frame, blocker_entries=None) -> None:
        """ABI form `begin_step(StepFrame)`; legacy form `begin_step(step:int, blocker_entries)`.

        A blocker is any station with a non-zero `blocker_h_m`: RSUs are receivers, not blockers,
        and a pedestrian is not an obstruction, so both carry 0.0 and drop out here. The engine
        supplies `frame.stations` in its own active-vehicle order, so the rebuilt index is entry-for-
        entry identical to the list comprehension this replaced.
        """
        if isinstance(frame, StepFrame):
            entries = [(s.vid, s.x, s.y, s.blocker_h_m)
                       for s in frame.stations.values() if s.blocker_h_m > 0.0]
            step = frame.step
        else:
            step, entries = frame, (blocker_entries if blocker_entries is not None else ())
        self.step = step
        self.blockers.rebuild(entries)

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float:
        """Interface name for :meth:`cbr`. `rx_vid` is unused: this estimator is a function of the
        offered load the receiver decodes, not of which receiver it is."""
        return self.cbr(offered)

    def delivery_coin(self, tx_vid: int, rx_vid: int) -> float:
        """The delivery coin, from THIS LINK's own keyed stream -- never the engine's global `rng`.

        Looked up (not created) by ordered pair: `evaluate` has already put the stream in place for
        every link that reached the composition site, so the draw is the same object, in the same
        order, as the pre-refactor `_prng.random()`."""
        return self._packet[(tx_vid, rx_vid)].random()

    def prune(self, live_vids) -> None:
        """Drop per-link state for pairs where NEITHER endpoint is still active."""
        for k in [k for k in self._shadow if k[0] not in live_vids and k[1] not in live_vids]:
            self._shadow.pop(k, None)
        for k in [k for k in self._packet if k[0] not in live_vids and k[1] not in live_vids]:
            self._packet.pop(k, None)

    def cbr(self, load_msgs_per_step: float) -> float:
        """Modelled channel busy ratio: offered frames per second x PPDU+MAC airtime, capped at 1.

        Zeroth-order (it counts the load the receiver decodes, so it ignores energy between the
        -85 dBm probe level and the -81 dBm decode floor and double-counts overlapping frames at
        high load) -- the Sepulcre et al. analytical estimator PHASE2-DESIGN step 6 binds is not
        implemented here; this replaces the linear ramp on raw message count, not that model."""
        return min(1.0, max(0.0, load_msgs_per_step) / self.dt * PHY_FRAME_AIRTIME_S)

    def collision_loss(self, dist_m: float, cbr: float) -> float:
        """Hidden-terminal collision probability: the pure-ALOHA vulnerable-period result
        1 - exp(-2G) applied to the share of the offered load the transmitter cannot carrier-sense
        (CSMA defers the rest). Coarse by construction; it is 0 at co-location and grows with the
        separation, which is the qualitative behaviour the flat ramp it replaces did not have."""
        g = cbr * hidden_terminal_fraction(dist_m, self.sense_m)
        return 1.0 - math.exp(-2.0 * g) if g > 0.0 else 0.0

    # -- per-link ---------------------------------------------------------------------------- #
    def _link_state(self, tx_vid, rx_vid, txx, txy, rxx, rxy, d, tx_h, rx_h):
        """Classify + advance the AR(1) shadowing for this link, once per step.

        Returns (state, shadow_db, nlosv_mu_base, nlosv_sigma) with state in LOS / NLOSv / NLOSb."""
        key = (tx_vid, rx_vid) if tx_vid < rx_vid else (rx_vid, tx_vid)
        st = self._shadow.get(key)
        if st is None:
            st = {"rng": random.Random(f"{self.seed}:shadow2:{key[0]}:{key[1]}"),
                  "s": None, "step": -1, "pa": None, "pb": None,
                  "state": "LOS", "mu": 0.0, "sig": 0.0}
            self._shadow[key] = st
        if st["step"] == self.step:
            return st["state"], st["s"], st["mu"], st["sig"]
        rng = st["rng"]
        # Gudmundson decorrelation is driven by how far the link has moved through the environment:
        # the SUM of the two endpoint displacements, so a convoy at constant separation still sees
        # its shadowing decorrelate as the street scrolls past (a |delta d| measure would not).
        pa, pb = (txx, txy), (rxx, rxy)
        if key[0] != tx_vid:
            pa, pb = pb, pa
        if st["pa"] is None:
            moved = float("inf")
        else:
            moved = (math.dist(pa, st["pa"]) + math.dist(pb, st["pb"]))
        st["pa"], st["pb"] = pa, pb

        # --- classification -------------------------------------------------------------------
        blocker_h = self.blockers.tallest_blocker(txx, txy, rxx, rxy, tx_vid, rx_vid)
        if self.buildings is not None:
            nlosb = self.buildings.blocked(txx, txy, rxx, rxy)
        elif self.canyon_per_m > 0.0:
            # Synthetic map with no footprints: an urban-canyon blocker density (expected blockages
            # per km) as a Poisson process along the path, P(LOS) = exp(-lambda*d). Re-decided only
            # once the link has moved a decorrelation distance, so the verdict is spatially coherent
            # instead of flickering every step.
            prev = st.get("canyon")
            if prev is None or moved >= TR37885_SHADOW_DECORR_M["NLOSb"]:
                prev = rng.random() >= math.exp(-self.canyon_per_m * d)
                st["canyon"] = prev
            nlosb = prev
        else:
            nlosb = False
        if nlosb:
            state, mu_base, sig_v = "NLOSb", 0.0, 0.0
        elif blocker_h > 0.0:
            below = (tx_h < blocker_h) + (rx_h < blocker_h)
            mu_base, sig_v = TR37885_NLOSV[("both_above", "one_below", "both_below")[below]]
            state = "NLOSv" if below else "LOS"     # both antennas above the blocker -> no loss
        else:
            state, mu_base, sig_v = "LOS", 0.0, 0.0
        self.stats[state] += 1

        # --- AR(1) / Gudmundson shadowing -------------------------------------------------------
        sigma = TR37885_SHADOW_SIGMA_DB[state]
        decorr = TR37885_SHADOW_DECORR_M[state]
        if st["s"] is None or moved == float("inf"):
            s = rng.gauss(0.0, sigma)
        else:
            rho = math.exp(-moved / decorr)
            s = rho * st["s"] + math.sqrt(max(0.0, 1.0 - rho * rho)) * rng.gauss(0.0, sigma)
        st.update(s=s, step=self.step, state=state, mu=mu_base, sig=sig_v)
        return state, s, mu_base, sig_v

    def _per_from_rx_dbm(self, rx_dbm: float) -> float:
        """SINR -> PER for the 6 Mb/s QPSK-1/2 10 MHz 802.11p profile.

        No coded SINR->PER waterfall for this profile is held anywhere in this repository
        (refdata nakagami_fading.sinr_to_per_mapping is recorded as UNAVAILABLE), and inventing one
        would be fabrication. This is therefore the vendored stack's own hard step -- decode iff the
        received power clears both the -81 dBm sensitivity and the 4 dB SNIR bar. A step has no
        partial-reception region on its own; the per-packet Nakagami fade applied BEFORE it is what
        turns the population PDR into a smooth waterfall (and what the >= 100 m gray-zone gate
        measures)."""
        return 0.0 if rx_dbm >= self.decode_floor_dbm else 1.0

    def evaluate_raw(self, tx_vid, rx_vid, txx, txy, rxx, rxy, d, tx_h, rx_h):
        """One packet on one link. Returns (heard, rssi_dbm, state, packet_rng).

        `rssi_dbm` is the FADED received power -- what the receiver's PHY actually measures for this
        frame. It is computed from TRUE geometry (txx/txy are the transmitter's true position, never
        its claimed one), which is exactly what makes an RSSI-vs-claimed-distance detector possible:
        a Sybil ghost inherits its attacker's true-position RSSI. It is receiver-measurable and so
        legitimately MA-visible."""
        d = max(float(d), 1.0)
        state, shadow_db, mu_base, sig_v = self._link_state(
            tx_vid, rx_vid, txx, txy, rxx, rxy, d, tx_h, rx_h)
        pkey = (tx_vid, rx_vid)
        prng = self._packet.get(pkey)
        if prng is None:
            prng = random.Random(f"{self.seed}:geo:{tx_vid}:{rx_vid}")
            self._packet[pkey] = prng
        if state == "NLOSb":
            pl = tr37885_pathloss_db(self.nlos_state, d)
        else:
            pl = tr37885_pathloss_db(self.los_state, d)
            if state == "NLOSv":
                # censored Gaussian: draw and clamp at 0, never shortcut to the mean
                pl += max(0.0, prng.gauss(tr37885_nlosv_mu_db(mu_base, d), sig_v))
        mean_rx = self.tx_dbm - pl + shadow_db
        m = nakagami_m_for_distance(d)
        fade_db = 10.0 * math.log10(max(prng.gammavariate(m, 1.0 / m), 1e-12))
        rx_dbm = mean_rx + fade_db
        heard = self._per_from_rx_dbm(rx_dbm) <= 0.0
        return heard, rx_dbm, state, prng

    def evaluate(self, tx, rx, d_m, txn):
        """`LinkChannelModel.evaluate` -- the interface form of :meth:`evaluate_raw`.

        Pure translation: identical arguments in identical order, so every draw on every per-link
        stream is made at exactly the point it was made before the refactor."""
        heard, rssi, state, _prng = self.evaluate_raw(
            tx.vid, rx.vid, tx.x, tx.y, rx.x, rx.y, d_m, tx.ant_h_m, rx.ant_h_m)
        return LinkOutcome(rssi_dbm=rssi, link_state=state) if heard else None


# The built-in registry, in DISPLAY order. This tuple -- not a `switch`, not an array index -- is
# what `radio_model` now selects through, and it is what `_ENUM_OPTIONS` / the argparse `choices` /
# the GUI dropdown all read, so those four sites can never drift apart again. F2MD's
# integer-indexed enum + `switch` (`MdAppTypes.h` + `F2MDVeinsApp.cc`) is the anti-pattern this
# replaces; the registration shape comes from Artery / ns-3 / MOSAIC instead.
for _name, _cls in (("disc", DiscChannel), ("logdistance", LogDistanceChannel),
                    ("geometric", GeometricChannel)):
    _api_registry.register_builtin("channel_model", _name, _cls)
del _name, _cls


class InternalMobility:
    """The engine's OWN mobility, named so it can be SELECTED rather than merely assumed.

    `roads.random_trip` picks a shortest-path route, `run_pipeline.car_follow` integrates the
    Intelligent Driver Model along it, and traffic lights come from `net.node_phase`'s 2-colouring
    -- or, with `real_signals`, from the imported `<tlLogic>` program of the movement the vehicle is
    actually making. This class holds no code: it is the registry entry that makes "internal" one option among
    several instead of the hard-coded only one, exactly as `disc` is for the channel. It is the
    DEFAULT and it is what every pinned golden was measured on."""

    NAME = "internal"
    INTERFACE_NAME = "scms.mobility"
    INTERFACE_VERSION = "1.0"

    @staticmethod
    def capabilities() -> frozenset:
        return frozenset({"position", "speed", "heading", "spawn", "despawn", "route_length",
                          "car_following", "signals"})


#: The `mobility` slot -- empty until now, which is why the IDM model was not selectable at all.
#: Registration order is the display order that `_ENUM_OPTIONS["mobility_source"]`, the argparse
#: `choices` and the GUI dropdown all read, so those cannot drift apart. `SumoReplayMobility` is
#: imported lazily by name (its module pulls in `sumolib`/`libsumo` only inside functions, so this
#: import stays cheap for the 60-odd test modules that import run.py).
from .sumo_trace import SumoReplayMobility as _SumoReplayMobility   # noqa: E402
for _name, _cls in (("internal", InternalMobility), ("sumo_replay", _SumoReplayMobility)):
    _api_registry.register_builtin("mobility", _name, _cls)
del _name, _cls

#: Config scalars a channel model may read at construction. NOT the oracle: every one of these is
#: user-supplied config that already appears verbatim in `manifest["config"]`. `config` and
#: `buildings` are handed over for BUILT-IN construction (`GeometricChannel.from_plugin`); a third
#: party should read `params` instead, which is the half that gets hashed into the manifest lock.
_CHANNEL_ENV_KEYS = ("radio_range_m", "pathloss_exponent", "shadowing_sigma_db",
                     "rx_sensitivity_margin_db", "radio_cap_sigma", "radio_cap_max_mult",
                     "radio_env", "radio_tx_power_dbm", "radio_rx_sensitivity_dbm",
                     "radio_nlosb_density_per_km", "chan_capacity", "packet_loss_base",
                     "nlos_loss", "seed", "dt")


class ReadOnlyConfig:
    """Attribute-read-only view of `PipelineConfig`, handed to a plugin as `env["config"]`.

    `_channel_env` used to pass the LIVE dataclass instance. `_write_manifest` serialises
    `cfg.__dict__` at the END of the run, so a plugin that wrote through that reference -- one line,
    `env["config"].report_prob = 1.0`, at construction or at step 30 -- silently rewrote the config
    the artifact claims produced it, at exit 0, with an identical `provenance_digest` and a clean
    `verify-plugins`. The manifest replay contract (constraint 2) was broken with no drift signal
    anywhere.

    Two defences, and they are deliberately different in kind:

    * this view, which makes the ACCIDENTAL and the one-line-deliberate write a loud immediate
      error rather than a silent success; and
    * :func:`_assert_config_unmoved`, which is the one that actually holds. Python has no way to
      make a reference unforgeable -- `env` is a plain dict, a determined plugin can reach the real
      object through `gc`, through a frame walk, or through this view's own slot -- so the
      enforceable statement is not "you cannot write" but "if the config moved, the run does not
      produce an artifact". Stated the same way C5 states that PEP 578 is detection, not sandboxing.
    """

    __slots__ = ("_ReadOnlyConfig__cfg",)

    def __init__(self, cfg):
        object.__setattr__(self, "_ReadOnlyConfig__cfg", cfg)

    def __getattr__(self, name):
        if name.startswith("__"):                       # no __dict__ / __setattr__ escape hatch
            raise AttributeError(name)
        return getattr(object.__getattribute__(self, "_ReadOnlyConfig__cfg"), name)

    def __setattr__(self, name, value):
        raise ConfigError(
            f"env['config'] is READ-ONLY: a plugin may not write {name!r} (or any other field) on "
            f"the run's configuration. The manifest records `cfg.__dict__`, so a mid-run write "
            f"makes the dataset unreplayable while the manifest still says exit 0. Declare the knob "
            f"through your own `config_fields()` and read it from `params` instead.")

    def __delattr__(self, name):
        self.__setattr__(name, None)

    def __repr__(self) -> str:                          # pragma: no cover - debugging aid
        return f"ReadOnlyConfig(seed={self.seed})"


def _config_dict(cfg) -> dict:
    """`cfg.__dict__` in the shape `manifest["config"]` carries it. ONE definition, used by the
    snapshot, the comparison and the manifest writer, so those three can never disagree about what
    "the config" is."""
    return {k: (list(v) if isinstance(v, tuple) else v) for k, v in cfg.__dict__.items()}


def _assert_config_unmoved(before: dict, cfg, when: str) -> None:
    """Refuse to continue if anything moved `cfg` since `before` was taken.

    THE enforceable half of the read-only-config contract. A plugin that mutates the run's config --
    at construction, or at step 30 -- produces a dataset that NO config describes: the steps before
    the write ran under the old value and the steps after under the new one. Writing either into
    `manifest["config"]` would be a lie, so the run fails instead. Nothing in the engine mutates
    `cfg` between `validate_config` and the manifest write, so this can only fire on plugin code.
    """
    now = _config_dict(cfg)
    moved = sorted(k for k in set(before) | set(now) if before.get(k, _MISSING) != now.get(k, _MISSING))
    if not moved:
        return
    detail = "; ".join(f"{k}: {before.get(k, _MISSING)!r} -> {now.get(k, _MISSING)!r}"
                       for k in moved[:5])
    raise ConfigError(
        f"the run configuration was MUTATED {when}: {detail}"
        + (f" (and {len(moved) - 5} more)" if len(moved) > 5 else "")
        + ". Only a plugin can do this. The dataset is unreplayable -- the steps before the write "
          "ran under one config and the steps after under another -- so no manifest is written.")


_MISSING = object()


def _channel_env(cfg, buildings, dt):
    env = {k: getattr(cfg, k) for k in _CHANNEL_ENV_KEYS if hasattr(cfg, k)}
    env["dt"] = dt
    env["buildings"] = buildings
    # Read-only. The built-ins reach it through `GeometricChannel.from_plugin` and only ever READ
    # attributes off it, so the view is transparent to them.
    env["config"] = ReadOnlyConfig(cfg)
    return env


#: Keys a `plugins.channel_model` section may carry. Closed so a typo is an error, not a no-op.
_CHANNEL_SECTION_KEYS = frozenset({"ref", "params", "conformance", "source_gate"})


def _slot_source_gate(cfg, slot: str) -> str:
    """`plugins.<slot>.source_gate` for the single-object slots, defaulting to "on".

    Same key, same two values and the same meaning the `check` / `fusion` arrays already give it,
    extended to the slots that take ONE plugin. "off" is the explicit, RECORDED opt-out for code the
    user wrote or audited: it turns off the runtime plugin guard
    (:mod:`~scms_sim_ref.api.guard`) for that slot, and because it lives in `cfg.plugins` it is
    serialised verbatim into `manifest["config"]` and replays.
    """
    sel = (getattr(cfg, "plugins", None) or {}).get(slot)
    if not isinstance(sel, dict):
        return _srcgate.DEFAULT_MODE
    return _srcgate.check_mode(sel.get("source_gate", _srcgate.DEFAULT_MODE),
                               f"plugins.{slot}.source_gate")


def _guard_label(cfg, slot: str, ref: str, builtin: bool):
    """The label a plugin call is guarded under, or None for "do not guard".

    None for a BUILT-IN (it is the engine; guarding it would refuse the engine's own `legacy_rng`
    capability and buy nothing) and for a slot whose `source_gate` the config turned off.
    `guard.guarded(fn, None)` returns `fn` itself, so both cases pay not even a wrapper frame.
    """
    if builtin or _slot_source_gate(cfg, slot) == "off":
        return None
    return f"plugins.{slot} {ref!r}"


def _obj_guard_label(cfg, slot: str, obj):
    """:func:`_guard_label` for an already-constructed single-object plugin (codec / profile /
    report format), which the engine holds as an INSTANCE rather than as a ref."""
    if obj is None:
        return None
    cls = obj if isinstance(obj, type) else type(obj)
    return _guard_label(cfg, slot, cls.__name__, _api_registry.is_builtin(slot, cls))

#: `plugins.<slot>.conformance`: "off" (default, and the only zero-cost setting) or "required".
CONFORMANCE_MODES = ("off", "required")

#: Per slot, the checks the IN-RUN attestation cannot run: the ones that themselves run a full
#: pipeline. Attestation happens INSIDE a pipeline, so running one of these there would put a
#: pipeline inside a pipeline. Excluded BEFORE the suite runs, not filtered out of its report
#: afterwards, and named in the embedded summary so nobody reads `passed: N` and believes the
#: artifact-level check ran.
_ATTEST_EXCLUDES_BY_SLOT = {"channel_model": ("C12_pipeline_two_run_digest",),
                            "check": ("D6_off_by_default_is_byte_identical",)}

#: Back-compat alias: the channel slot's list, which is what this name meant when only that slot
#: could be attested.
_ATTEST_EXCLUDES = _ATTEST_EXCLUDES_BY_SLOT["channel_model"]


def _channel_conformance(cfg) -> str:
    sel = (cfg.plugins or {}).get("channel_model") if isinstance(cfg.plugins, dict) else None
    mode = str(sel.get("conformance", "off")) if isinstance(sel, dict) else "off"
    if mode not in CONFORMANCE_MODES:
        raise ConfigError(f"plugins.channel_model.conformance must be one of "
                          f"{list(CONFORMANCE_MODES)} (got {mode!r})")
    return mode


def _attest(slot: str, ref: str, params: dict) -> dict:
    """Run the v1 conformance suite against this plugin BEFORE CONSTRUCTION and refuse a failing one.

    **Order, and it is the whole point.** This used to run AFTER `instantiate`. A hostile `__init__`
    therefore executed before it was ever gated: it could rebind `random.Random.random`, replace
    `api.channel.check_outcome`, or -- decisively -- monkeypatch `conformance.runner.run_ref` or
    `ConformanceReport.ok`, and then "pass", because the process performing the attestation was one
    the candidate had already edited. An attestation performed by code the subject can rewrite
    attests to nothing. Both callers now attest BEFORE the run's instance exists.

    **Place.** Conformance must itself construct the candidate -- there is no way to grade a model
    without building one -- so the suite runs in a CHILD interpreter
    (`conformance/attest.py`), which is discarded a moment later and shares no object with this run.
    The child brackets the suite with an integrity sentinel of its own and reports what it saw, so
    "the candidate tampered while being attested" is a recorded property of the attestation rather
    than something this process has to infer. Cost: one interpreter start per attested plugin, on a
    path that is off by default.


    The design's third delivery route -- *"let the engine refuse an unattested plugin"* -- expressed
    as a config declaration rather than a flag, so it lands in `manifest["config"]` and replays like
    everything else. The resulting summary is written into
    `manifest["plugins"]["loaded"][*]["conformance"]`, which is what turns *"this dataset was
    produced by a conformant plugin"* into a machine-checkable property of the artifact.

    OFF BY DEFAULT, and that is not timidity. Attestation now costs an interpreter start plus the
    suite (measured ~1.0 s per attested plugin, against ~0.11 s when it ran in-process), and the PEP
    578 audit hook C5 installs -- which can never be removed once added -- is now paid by the child
    and thrown away with it rather than left on the engine process forever. Neither belongs on the
    engine path of a run that did not ask for it, and the honest home for a fourteen-check suite is a
    CI gate, not every `run_pipeline`.

    **C12 is excluded, and it has to be**: C12 runs two full pipelines, so running it from inside
    `build_channel` -- which is itself inside a pipeline -- nests a pipeline in a pipeline. It is
    excluded BEFORE the suite runs, not filtered out of the report afterwards, and the distinction is
    not academic: the first implementation filtered afterwards, cost 0.30 s, and was still quietly
    running the two pipelines its own summary said were excluded. The exclusion is named in the
    summary, so nobody reads `passed: 12` here and believes the artifact-level check ran.
    """
    from ..conformance.attest import run_out_of_process
    excludes = _ATTEST_EXCLUDES_BY_SLOT.get(slot, ())
    report = run_out_of_process(slot, ref, params, exclude=excludes)
    summary = dict(report.get("summary") or {})
    if report.get("error"):
        raise ConfigError(
            f"plugins.{slot}.conformance=required and attesting {ref!r} FAILED to complete: "
            f"{report['error']}\n{report.get('traceback', '')}")
    if not summary.get("ok"):
        failed = [r["check"] for r in (report.get("checks") or [])
                  if r.get("status") in ("FAIL", "ERROR")]
        raise ConfigError(
            f"plugins.{slot}.conformance=required and {ref!r} does not conform: {failed} failed.\n"
            + _attest_text(report))
    # The child's own integrity verdict. A candidate whose CONSTRUCTION rebinds an engine or RNG
    # object is refused here even though every contract check passed -- which is the ordering defect
    # stated as a rule: passing a suite you were able to edit is not passing a suite.
    integ = report.get("integrity") or {}
    if not integ.get("ok", True):
        raise ConfigError(
            f"plugins.{slot}.conformance=required and {ref!r} TAMPERED with the interpreter while "
            f"being attested: {integ.get('tampered')}. Conformance is run in a child process "
            f"precisely so this is visible instead of being applied to the process doing the "
            f"judging; a plugin that rewrites the engine's own objects at construction is refused "
            f"whatever its check results say.")
    summary["attested_out_of_process"] = True
    summary["integrity"] = integ
    summary["excluded"] = list(excludes)
    summary["excluded_reason"] = (
        f"{', '.join(excludes)} runs a full pipeline; running it from inside one would put a "
        f"pipeline inside a pipeline. Run it from the CLI or CI instead: "
        f"scms-poc conformance --slot {slot} --ref <ref>") if excludes else ""
    return summary


def _attest_text(report: dict) -> str:
    """The child's rows, rendered the way `ConformanceReport.to_text` renders them in-process."""
    rows = report.get("checks") or []
    width = max((len(r.get("check", "")) for r in rows), default=10)
    lines = [f"conformance {report.get('suite')} :: {report.get('slot')} :: {report.get('ref')} "
             f"(attested out of process)"]
    for r in rows:
        detail = f"  {r.get('detail')}" if r.get("detail") else ""
        lines.append(f"  {r.get('status', '?'):<6} {r.get('check', '?'):<{width}}{detail}")
    return "\n".join(lines)


def _channel_selection(cfg):
    """(ref, params) for the channel slot: the `plugins` block wins, else the `radio_model` enum.

    D2: discovery may be automatic, ACTIVATION is always config-declared. Both spellings live in
    `cfg`, so both land in `manifest["config"]` and replay through `config_from_dict` with no new
    plumbing -- which is the whole reason a dotted path in config beats an entry point.
    """
    sel = (cfg.plugins or {}).get("channel_model") if isinstance(cfg.plugins, dict) else None
    if not sel:
        return str(cfg.radio_model), {}
    if isinstance(sel, str):
        sel = {"ref": sel}
    if not isinstance(sel, dict) or "ref" not in sel:
        raise ConfigError("plugins.channel_model must be a string or {'ref': ..., 'params': {...}}")
    extra = sorted(set(sel) - _CHANNEL_SECTION_KEYS)
    if extra:
        # A mistyped key here is a plugin knob that silently did nothing -- refuse it, the same way
        # an undeclared param is refused, rather than accept a config that does not mean what it says.
        raise ConfigError(f"plugins.channel_model: unknown key(s) {extra}; "
                          f"known: {sorted(_CHANNEL_SECTION_KEYS)}")
    params = sel.get("params") or {}
    if not isinstance(params, dict):
        raise ConfigError("plugins.channel_model.params must be an object")
    ref = str(sel["ref"])
    if cfg.radio_model != PipelineConfig.radio_model and cfg.radio_model != ref:
        raise ConfigError(
            f"ambiguous channel selection: radio_model={cfg.radio_model!r} and "
            f"plugins.channel_model.ref={ref!r} disagree; set only one")
    return ref, params


def build_channel(cfg, buildings=None, dt: float = 1.0):
    """Resolve + construct the run's channel model. Once, before step 0; every failure fatal here.

    Returns `(adapter, provenance)`. A `LinkChannelModel` is wrapped in `PerLinkAdapter`; a
    `BatchChannelModel` is used as-is. A plugin instance is a PER-RUN object built inside the
    pipeline, never a module global -- the in-process multi-run drivers (`datagen/foundry.py`,
    `campaign.py`, `massive.py`, `gui/agent.py`) would otherwise cross-contaminate.
    """
    ref, params = _channel_selection(cfg)
    # LOAD UNDER A SENTINEL, and the snapshot is taken BEFORE `resolve` on purpose. `resolve`
    # IMPORTS the plugin's module, and module-level code runs on import -- an earlier hook than
    # `__init__` and one no gate in this project looked at. Everything from that import to the
    # constructor's return is inside the bracket.
    _guard = _integrity.Sentinel(armed=_integrity.armed_for(cfg))
    cls, how, iv, shape = _api_registry.resolve("channel_model", ref)
    # ATTESTATION FIRST, and OUT OF PROCESS. This used to run twenty lines further down, AFTER the
    # instance existed -- so a hostile `__init__` ran before it was ever gated and could rewrite the
    # machinery about to judge it. See `_attest`.
    conformance = (_attest("channel_model", ref, params)
                   if _channel_conformance(cfg) == "required" else None)
    pid = _api_registry.plugin_id_of(cls, ref if ":" not in ref else ref.rsplit(":", 1)[-1].lower())
    rng_ns = RngNamespace(cfg.seed, pid)
    # Attestation moved in front of construction still leaves the run's OWN instance being built in
    # this interpreter, and `__init__` is arbitrary code. The comparison happens the moment the
    # constructor returns, so an import-time or construction-time rebind of `random.Random`, of
    # `api.channel.check_outcome`, of `run._attest` or of any other watched object is fatal HERE --
    # before step 0, before an output directory exists. Built-ins are the engine, so only a
    # third-party resolution is graded.
    model = _api_registry.instantiate(cls, params=params, rng=rng_ns,
                                      env=_channel_env(cfg, buildings, dt))
    if how != "builtin":
        _guard.verify("while LOADING the channel plugin",
                      subject=f"plugins.channel_model {ref!r}")
    caps = _api_registry.check_capabilities("channel_model", ref, cls, how, model.capabilities())
    # The adapter carries the namespace so that `chan.begin_step(frame)` in the step loop advances it
    # exactly once per step. Without that wiring `RngNamespace._step` never leaves -1 and every
    # `stream()` key ends `:s-1`, which silently turns a stateless per-packet draw into a per-run
    # constant -- see `BatchAdapter.begin_step`.
    # `validate` runs `api.channel.check_outcome` on every delivered link of a THIRD-PARTY model:
    # rssi in the declared dBm band, link_state in the closed vocabulary, finite non-negative delay.
    # Conformance (C7) is the same statement but is OFF BY DEFAULT, so without this the default
    # third-party path had NO outcome validation at all and an out-of-vocabulary or absurd value
    # flowed straight into the MA-visible dataset as evidence. Built-ins are exempt: the pinned
    # goldens grade them, and this is a call per delivered link on a ~10^7-link loop.
    _validate = (how != "builtin")
    # THE RUNTIME PLUGIN GUARD, on the same third-party/built-in line `validate` draws. The adapters
    # route every model-facing call through `guard.guarded`, which refuses `sys._getframe` and the
    # other reflective routes into THIS loop's frame from inside CPython -- the half a static source
    # scan cannot do, because `getattr(sys, "_get"+"frame")` defeats a name match and reaches the
    # same C function. See api/guard.py for what it is and, more importantly, what it is not.
    _glabel = _guard_label(cfg, "channel_model", ref, how == "builtin")
    chan = (_api_channel.BatchAdapter(model, rng_ns, _validate, guard_label=_glabel)
            if shape == "batch"
            else PerLinkAdapter(model, rng_ns, _validate, guard_label=_glabel))

    def _provenance():
        # Built AT THE END of the run, not here: `declared_streams` is the set of stream labels the
        # plugin ACTUALLY consumed, and a model that draws only during the loop (which is all of
        # them) has consumed none at construction time. Recording an always-empty list would be
        # fabrication by omission -- D3's third property is that a plugin DECLARES the streams it
        # uses, as a manifest field.
        return _api_registry.make_provenance("channel_model", 0, ref, cls, how, iv, caps,
                                             rng_ns.declared_streams(), params,
                                             conformance=conformance)

    return chan, _provenance


# --------------------------------------------------------------------------- #
# The MESSAGE-CODEC seam (PLUGIN-ARCHITECTURE.md section 2.5 / D5)
#
# Built exactly the way `build_channel` is, and deliberately so: `_codec_selection` is
# `_channel_selection` with one enum swapped, and the same two spellings resolve to the same object
# -- a built-in NAME in `cfg.message_codec`, or `plugins.message_codec.ref` for anything else. That
# is what makes a third party able to supply a whole protocol stack (a different CAM profile, a
# C-V2X message set, a thresholding scheme's own PDU) without forking the engine, on the identical
# machinery they already use to supply a detector or a channel model.
#
# THE ONE DIFFERENCE FROM EVERY OTHER SLOT: there is no default object. `radio_model` defaults to
# `disc`, a real channel; `message_codec` defaults to `""`, which constructs NOTHING. A codec that
# ran by default would encode every frame of every run -- and the whole point of the empty default
# is that the pinned digests are reachable at ZERO cost, not merely at equal output.
# --------------------------------------------------------------------------- #
_CODEC_SECTION_KEYS = frozenset({"ref", "params", "conformance", "source_gate"})


def _codec_selection(cfg):
    """`(ref, params)` for the codec slot, or `(None, {})` when no codec is selected.

    The `plugins` block wins over the enum, and the two disagreeing is a REFUSAL rather than a
    precedence rule -- the same treatment `radio_model` vs `plugins.channel_model.ref` gets, for the
    same reason: a config that names two different codecs does not mean what it says.
    """
    sel = (cfg.plugins or {}).get("message_codec") if isinstance(cfg.plugins, dict) else None
    name = str(getattr(cfg, "message_codec", "") or "")
    if not sel:
        return (name or None), {}
    if isinstance(sel, str):
        sel = {"ref": sel}
    if not isinstance(sel, dict) or "ref" not in sel:
        raise ConfigError("plugins.message_codec must be a string or {'ref': ..., 'params': {...}}")
    extra = sorted(set(sel) - _CODEC_SECTION_KEYS)
    if extra:
        raise ConfigError(f"plugins.message_codec: unknown key(s) {extra}; "
                          f"known: {sorted(_CODEC_SECTION_KEYS)}")
    params = sel.get("params") or {}
    if not isinstance(params, dict):
        raise ConfigError("plugins.message_codec.params must be an object")
    ref = str(sel["ref"])
    if name and name != ref:
        raise ConfigError(
            f"ambiguous codec selection: message_codec={name!r} and "
            f"plugins.message_codec.ref={ref!r} disagree; set only one")
    return ref, params


def _codec_conformance(cfg) -> str:
    sel = (cfg.plugins or {}).get("message_codec") if isinstance(cfg.plugins, dict) else None
    mode = str(sel.get("conformance", "off")) if isinstance(sel, dict) else "off"
    if mode not in CONFORMANCE_MODES:
        raise ConfigError(f"plugins.message_codec.conformance must be one of "
                          f"{list(CONFORMANCE_MODES)} (got {mode!r})")
    return mode


def build_codec(cfg):
    """Resolve + construct the run's message codec, or return `(None, None)` when none is selected.

    Same sentinel discipline as `build_channel`: the snapshot is taken BEFORE `resolve`, because
    `resolve` imports the plugin's module and module-level code is an earlier hook than `__init__`.
    A CONSTRUCTION failure -- `asn1tools` missing, an unknown parameter, an ETSI StationType name
    that does not exist -- is fatal HERE, before step 0 and before an output directory exists, which
    is exactly what conformance check C10 asks of every slot.
    """
    ref, params = _codec_selection(cfg)
    if not ref:
        return None, None
    _guard = _integrity.Sentinel(armed=_integrity.armed_for(cfg))
    cls, how, iv, _shape = _api_registry.resolve("message_codec", ref)
    # The codec package keeps its OWN fixed in-tree mapping for exactly the reason the check slot
    # does: `register_builtin` writes into a process-global dict, so any imported distribution could
    # rebind `etsi_cam_en302637_2` to its own class and be resolved AS A BUILT-IN -- inheriting the
    # built-in exemption from the source gate and from outcome validation. Identity against the
    # shipped tuple is the whole check, and it is a named refusal rather than a silent substitution.
    from .. import codecs as _codecs_pkg
    if _codecs_pkg.is_hijacked(ref):
        raise ConfigError(
            f"plugins.message_codec {ref!r} resolved to "
            f"{cls.__module__}.{getattr(cls, '__name__', cls)!r}, but {ref!r} is a BUILT-IN codec "
            f"name owned by {_codecs_pkg.BUILTIN_CODEC_BY_NAME[ref].__module__}. Something called "
            f"register_builtin('message_codec', {ref!r}, ...) and replaced it. Give the third-party "
            f"profile its own ref.")
    conformance = (_attest("message_codec", ref, params)
                   if _codec_conformance(cfg) == "required" else None)
    pid = _api_registry.plugin_id_of(cls, ref if ":" not in ref else ref.rsplit(":", 1)[-1].lower())
    rng_ns = RngNamespace(cfg.seed, pid)
    codec = _api_registry.instantiate(cls, params=params, rng=rng_ns, env={"config":
                                                                          ReadOnlyConfig(cfg)})
    if how != "builtin":
        _guard.verify("while LOADING the message codec", subject=f"plugins.message_codec {ref!r}")
    caps = _api_registry.check_capabilities("message_codec", ref, cls, how, codec.capabilities())

    def _provenance():
        return _api_registry.make_provenance("message_codec", 0, ref, cls, how, iv, caps,
                                             rng_ns.declared_streams(), params,
                                             conformance=conformance)

    return codec, _provenance


# --------------------------------------------------------------------------- #
# The PROTOCOL-PROFILE seam (api/profile.py)
#
# One declaration says which protocol this run speaks. `build_profile` is `build_codec` with the
# enum swapped and ONE addition: when nothing is declared, the ITS-G5 profile is synthesised FROM
# THE LAYER FLAGS, so `--cam-rules` and `--dcc` keep meaning exactly what they meant and now mean it
# THROUGH the seam. There is no path in this file that reads `codecs.etsi_rules` inside the step
# loop any more: every ETSI constant, state machine, airtime and latency the engine uses arrives
# through a profile method. That is the test of whether the seam is real -- delete the built-in and
# the engine still runs whatever profile the config names.
# --------------------------------------------------------------------------- #
_PROFILE_SECTION_KEYS = frozenset({"ref", "params", "conformance", "source_gate"})

#: The BUILT-IN profile synthesised from the layer flags when no profile is declared.
DEFAULT_PROTOCOL_PROFILE = "etsi_its_g5"


def _section(cfg, slot: str, keys: frozenset, enum_field: str):
    """`(ref, params)` for a single-object plugin slot, or `(None, {})` when nothing is selected.

    The shared body of `_codec_selection` / `_profile_selection` / `_report_format_selection`: the
    `plugins` block wins over the enum field, and the two DISAGREEING is a refusal rather than a
    precedence rule -- a config that names two different implementations of one slot does not mean
    what it says.
    """
    sel = (cfg.plugins or {}).get(slot) if isinstance(cfg.plugins, dict) else None
    name = str(getattr(cfg, enum_field, "") or "")
    if not sel:
        return (name or None), {}
    if isinstance(sel, str):
        sel = {"ref": sel}
    if not isinstance(sel, dict) or "ref" not in sel:
        raise ConfigError(f"plugins.{slot} must be a string or {{'ref': ..., 'params': {{...}}}}")
    extra = sorted(set(sel) - keys)
    if extra:
        raise ConfigError(f"plugins.{slot}: unknown key(s) {extra}; known: {sorted(keys)}")
    params = sel.get("params") or {}
    if not isinstance(params, dict):
        raise ConfigError(f"plugins.{slot}.params must be an object")
    ref = str(sel["ref"])
    if name and name != ref:
        raise ConfigError(f"ambiguous {slot} selection: {enum_field}={name!r} and "
                          f"plugins.{slot}.ref={ref!r} disagree; set only one")
    return ref, params


def _slot_conformance(cfg, slot: str) -> str:
    sel = (cfg.plugins or {}).get(slot) if isinstance(cfg.plugins, dict) else None
    mode = str(sel.get("conformance", "off")) if isinstance(sel, dict) else "off"
    if mode not in CONFORMANCE_MODES:
        raise ConfigError(f"plugins.{slot}.conformance must be one of {list(CONFORMANCE_MODES)} "
                          f"(got {mode!r})")
    return mode


def profile_layers_on(cfg) -> bool:
    """True when any layer of the built-in stack is requested by a flag rather than by a ref."""
    return bool(cfg.message_codec or cfg.cam_generation_rules or cfg.dcc or cfg.net_latency_model
                or (isinstance(cfg.plugins, dict) and cfg.plugins.get("message_codec")))


def _profile_selection(cfg):
    """`(ref, params)` for the protocol-profile slot, or `(None, {})`.

    With nothing declared and at least one layer flag set, this synthesises the built-in:
    `("etsi_its_g5", {...the flags...})`. The params are DERIVED FROM cfg rather than defaulted
    inside the profile, so `manifest["config"]` still says exactly which layers ran and the run
    replays from the config alone.
    """
    ref, params = _section(cfg, "protocol_profile", _PROFILE_SECTION_KEYS, "protocol_profile")
    if ref:
        return ref, params
    if not profile_layers_on(cfg):
        return None, {}
    return DEFAULT_PROTOCOL_PROFILE, {"signer": cfg.message_signer,
                                      "generation": bool(cfg.cam_generation_rules),
                                      "congestion": bool(cfg.dcc),
                                      "latency": bool(cfg.net_latency_model)}


def build_profile(cfg, codec):
    """Resolve + construct the run's protocol profile, or `(None, None)` when none is selected.

    Same discipline as `build_channel` / `build_codec`, in the same order and for the same reasons:
    the integrity sentinel is armed BEFORE `resolve` (module-level code is an earlier hook than
    `__init__`), a built-in NAME may not be hijacked by a third party, attestation happens in a
    child process before this one constructs anything, and every construction failure is fatal HERE,
    before step 0 and before an output directory exists.

    `codec` is the already-built `message_codec` -- built by `build_codec`, which owns that slot's
    sentinel, hijack refusal and lock entry -- and is INJECTED through `env`. A third-party profile
    is free to ignore it and return its own codec from `codec()`; the engine uses whatever
    `profile.codec()` answers, so a stack that brings its own wire format needs no engine change.
    """
    ref, params = _profile_selection(cfg)
    if not ref:
        return None, None
    _guard = _integrity.Sentinel(armed=_integrity.armed_for(cfg))
    cls, how, iv, _shape = _api_registry.resolve("protocol_profile", ref)
    from .. import codecs as _codecs_pkg
    if _codecs_pkg.is_hijacked(ref, "protocol_profile"):
        raise ConfigError(
            f"plugins.protocol_profile {ref!r} resolved to "
            f"{cls.__module__}.{getattr(cls, '__name__', cls)!r}, but {ref!r} is a BUILT-IN profile "
            f"name owned by {_codecs_pkg.shipped('protocol_profile', ref).__module__}. Something "
            f"called register_builtin('protocol_profile', {ref!r}, ...) and replaced it. Give the "
            f"third-party stack its own ref.")
    conformance = (_attest("protocol_profile", ref, params)
                   if _slot_conformance(cfg, "protocol_profile") == "required" else None)
    pid = _api_registry.plugin_id_of(cls, ref if ":" not in ref else ref.rsplit(":", 1)[-1].lower())
    rng_ns = RngNamespace(cfg.seed, pid)
    prof = _api_registry.instantiate(cls, params=params, rng=rng_ns,
                                     env={"config": ReadOnlyConfig(cfg), "message_codec": codec})
    if how != "builtin":
        _guard.verify("while LOADING the protocol profile",
                      subject=f"plugins.protocol_profile {ref!r}")
    caps = _api_registry.check_capabilities("protocol_profile", ref, cls, how, prof.capabilities())

    def _provenance():
        return _api_registry.make_provenance("protocol_profile", 0, ref, cls, how, iv, caps,
                                             rng_ns.declared_streams(), params,
                                             conformance=conformance)

    return prof, _provenance


# --------------------------------------------------------------------------- #
# The REPORT-FORMAT seam (api/report.py) -- the slot that was registered and EMPTY
# --------------------------------------------------------------------------- #
_REPORT_SECTION_KEYS = frozenset({"ref", "params", "conformance", "source_gate"})


def _report_format_selection(cfg):
    return _section(cfg, "report_format", _REPORT_SECTION_KEYS, "report_format")


def build_report_format(cfg):
    """Resolve + construct the run's misbehaviour-report format, or `(None, None)`.

    `None` is the DEFAULT and means the engine writes its historic row inline. That is not a
    fallback for a missing built-in -- `ma_report_v1` exists and reproduces that row exactly -- it is
    the same discipline every other seam here follows: the pinned digests must be reachable at ZERO
    cost, not merely at equal output, so the default constructs no object and calls nothing.
    """
    ref, params = _report_format_selection(cfg)
    if not ref:
        return None, None
    _guard = _integrity.Sentinel(armed=_integrity.armed_for(cfg))
    cls, how, iv, _shape = _api_registry.resolve("report_format", ref)
    from .. import codecs as _codecs_pkg
    if _codecs_pkg.is_hijacked(ref, "report_format"):
        raise ConfigError(
            f"plugins.report_format {ref!r} resolved to "
            f"{cls.__module__}.{getattr(cls, '__name__', cls)!r}, but {ref!r} is a BUILT-IN format "
            f"name owned by {_codecs_pkg.shipped('report_format', ref).__module__}. Something "
            f"called register_builtin('report_format', {ref!r}, ...) and replaced it. Give the "
            f"third-party format its own ref.")
    conformance = (_attest("report_format", ref, params)
                   if _slot_conformance(cfg, "report_format") == "required" else None)
    pid = _api_registry.plugin_id_of(cls, ref if ":" not in ref else ref.rsplit(":", 1)[-1].lower())
    rng_ns = RngNamespace(cfg.seed, pid)
    fmt = _api_registry.instantiate(cls, params=params, rng=rng_ns,
                                    env={"config": ReadOnlyConfig(cfg)})
    if how != "builtin":
        _guard.verify("while LOADING the report format", subject=f"plugins.report_format {ref!r}")
    caps = _api_registry.check_capabilities("report_format", ref, cls, how, fmt.capabilities())

    def _provenance():
        return _api_registry.make_provenance("report_format", 0, ref, cls, how, iv, caps,
                                             rng_ns.declared_streams(), params,
                                             conformance=conformance)

    return fmt, _provenance


class WireEncoder:
    """The engine's one call site into a codec: broadcast dict -> (PDU octets, wire size).

    Holds the `StationView` per station kind (vehicle / VRU / RSU) rather than rebuilding it per
    message -- it is frozen, it changes never, and constructing 2.2 million of them is 2.2 million
    allocations the run does not need.

    **The firewall applies here.** A :class:`~scms_sim_ref.api.codec.Claim` is built from the
    MA-VISIBLE half of the broadcast dict only: the CLAIMED position, speed and heading, the
    self-declared station type and the claimed generation time. `b["x"]`, `b["y"]` (the sender's
    TRUE position), `b["falsified"]`, `b["ghost"]`, `b["tspd"]`, `b["thdg"]` and `b["veh"]` never
    cross it -- which matters more here than anywhere else, because these octets are exactly what a
    TS 103 759 `v2xPduEvidence` entry would carry into a report.
    """

    __slots__ = ("codec", "signer", "_views", "_frame", "bytes_by_type", "count_by_type", "_sizer",
                 "_encode", "_envelope", "_decoders", "_probed", "_wire_size_capability")

    def __init__(self, codec, signer: str = "digest", frame=None, epoch_unix=None, sizer=None,
                 guard_label=None):
        self.codec = codec
        self.signer = signer
        #: WHO OWNS THE FRAME LENGTH. The profile does, when there is one: `wire_size_bytes` is
        #: where the security envelope is accounted for, and accounting for it in two places is how
        #: a CBR estimate silently ends up counting the certificate twice or not at all. With no
        #: profile the codec answers directly, which is what every run before the seam did.
        # A `sizer` handed in has already been guarded under the PROFILE's gate by the caller;
        # wrapping it again here would only add a frame.
        self._sizer = (sizer if sizer is not None
                       else _pguard.guarded(codec.wire_size_bytes, guard_label))
        self._encode = _pguard.guarded(codec.evidence_pdu, guard_label)
        #: WIRE-SIZE HONESTY (see `_check_wire`). `(msg_type, signer) -> size - len(pdu)`, filled on
        #: the first frame of each kind and asserted on every later one, but ONLY for a codec that
        #: DECLARES `wire_size` -- the capability whose published meaning is "wire_size_bytes() is
        #: derived from a real encode, not a constant". A codec that does not declare it is saying
        #: its length is a MODELLED number (the `native_v1` profile deliberately charges the
        #: engine's legacy 300 B), and holding a declared constant to the length of its own payload
        #: would be refusing the thing it declared.
        self._envelope: dict = {}
        try:
            caps = frozenset(codec.capabilities())
        except Exception:                                       # pragma: no cover - hostile codec
            caps = frozenset()
        self._wire_size_capability = _api_codec.CAP_WIRE_SIZE in caps
        #: `msg_type -> decode_<msg_type>`, for the round-trip probe. Absent decoders simply mean
        #: that PDU type is not probed; `decode_cam` is the only one `CODEC_SPEC` requires.
        self._decoders = {mt: _pguard.guarded(getattr(codec, f"decode_{mt}"), guard_label)
                          for mt in ("cam", "denm", "vam")
                          if getattr(codec, f"decode_{mt}", None) is not None}
        self._probed: dict = {}
        f = frame if frame is not None else getattr(codec, "frame", _api_codec.DEFAULT_FRAME)
        e = epoch_unix if epoch_unix is not None else getattr(
            codec, "epoch_unix", _api_codec.DEFAULT_EPOCH_UNIX)
        self._frame = f
        self._views = {
            "vehicle": _api_codec.StationView(frame=f, epoch_unix=e),
            "rsu": _api_codec.StationView(frame=f, epoch_unix=e, is_rsu=True),
        }
        self.bytes_by_type: dict = {}
        self.count_by_type: dict = {}

    @staticmethod
    def station_id(digest: str) -> int:
        """ETSI `StationID` (0..2^32-1) from the pseudonym's HashedId8.

        The top 32 bits of the certificate digest. It is NOT rotation-stable, and that is correct:
        a StationID that survived a pseudonym change would defeat the pseudonym.
        """
        try:
            return int(digest[:8], 16)
        except (TypeError, ValueError):
            return int(hashlib.sha256(str(digest).encode()).hexdigest()[:8], 16)

    def claim_for(self, b: dict, msg_type: str) -> "_api_codec.Claim":
        return _api_codec.Claim(
            station_id=self.station_id(b["digest"]), cert_digest=str(b["digest"]),
            msg_type=msg_type, gen_time=float(b["cg"]),
            x=float(b["cx"]), y=float(b["cy"]), speed=float(b["cs"]), heading=float(b["ch"]),
            pos_conf=float(b["conf"]), station_type=str(b.get("station_type", "vehicle")),
            msg_count=int(b.get("msg_count", 1)), event_type=b.get("event_type"),
            sig_ok=bool(b.get("sig_ok", True)),
            cert_valid_from=float(b.get("cvf", 0.0)), cert_valid_to=float(b.get("cvt", 0.0)),
            sequence_number=int(b.get("seq", 0)))

    @staticmethod
    def wire_msg_type(b: dict) -> str:
        """The PDU a real station would actually send for this broadcast.

        A DENM is a DENM; a station DECLARING `station_type="vru"` sends a VAM (TS 103 300-3), not a
        CAM. Keyed on the SELF-DECLARED type, never on `veh.is_vru`, so a VruImpersonation attacker
        that declares `vru` really does put a VAM on the air -- which is the whole shape of that
        attack and would be lost if the oracle decided the PDU type.
        """
        if b.get("msg_type") == "denm":
            return "denm"
        return "vam" if b.get("station_type") == "vru" else "cam"

    def encode(self, b: dict) -> tuple:
        """`(payload octets, wire size in bytes, claim)` for one broadcast."""
        mt = self.wire_msg_type(b)
        claim = self.claim_for(b, mt)
        view = self._views["rsu" if b["veh"].is_rsu else "vehicle"]
        pdu = self._encode(claim, view)
        size = int(self._sizer(claim, self.signer))
        n = self._check_wire(mt, pdu, size, claim, view)
        self.bytes_by_type[mt] = self.bytes_by_type.get(mt, 0) + n
        self.count_by_type[mt] = self.count_by_type.get(mt, 0) + 1
        return pdu, size, claim

    #: Frame lengths outside this band are refused rather than charged. A 802.11p MPDU cannot
    #: exceed 4095 octets and an ITS-G5 CAM is a few hundred; the conformance suite (C.protocol)
    #: already asserts the same `(0, 8192]` band against `wire_size_bytes` at LOAD time, and this is
    #: the same statement made on every message of the run instead of once on a sample.
    MAX_WIRE_BYTES = 8192

    #: How often the emitted PDU is decoded back. The first frame of each PDU TYPE plus every
    #: `DECODE_PROBE_EVERY`-th frame of it, so the probe is a deterministic function of the message
    #: index and cannot depend on a clock, a thread or a codec's own state.
    DECODE_PROBE_EVERY = 4096

    #: How far a decoded position may sit from the encoded claim. ETSI's 1/10-microdegree grid is
    #: ~1 cm; this is loose enough that a lossy but honest profile passes and tight enough that a
    #: PDU which does not carry the claim at all cannot.
    DECODE_TOLERANCE_M = 5.0

    def _check_wire(self, mt: str, pdu, size: int, claim, view) -> int:
        """Refuse a codec whose declared frame length or emitted PDU is not what it says it is.

        THE DEFECT. `wire_size_bytes` was taken verbatim and fed straight into airtime -> CBR ->
        collision -> latency -> `detection_time` -> `data_digest`, with nothing checking it against
        the octets the same codec had just produced; and `evidence_pdu` was written into the dataset
        (and into a TS 103 759 `v2xPduEvidence` entry) without ever being decoded. A codec reporting
        fabricated sizes therefore produced three different datasets from one config, and a 4-byte
        blob that decodes to nothing was recorded as evidence. Both are ARITHMETIC the engine can do
        itself, so both are done here rather than trusted:

        1. **shape** -- the PDU is real octets and the size is an int in `(0, MAX_WIRE_BYTES]`.
        2. **envelope consistency** -- for a codec DECLARING `wire_size` ("derived from a real
           encode"), `size - len(pdu)` is the security envelope and must be non-negative and the
           same for every frame of one `(msg_type, signer)`. Fabricating a length then means
           fabricating the payload to match it, which is what check 3 is for.
        3. **the PDU decodes back to the claim it was made from**, on a deterministic sample.

        Returns the PDU length, which the caller was going to compute anyway.
        """
        if not isinstance(pdu, (bytes, bytearray)):
            raise ConfigError(
                f"message codec {type(self.codec).__name__}: evidence_pdu() returned "
                f"{type(pdu).__name__}, not octets. These bytes are what a TS 103 759 "
                f"v2xPduEvidence entry carries and what the dataset records as evidence.")
        n = len(pdu)
        if not 0 < n <= self.MAX_WIRE_BYTES:
            raise ConfigError(
                f"message codec {type(self.codec).__name__}: evidence_pdu() returned {n} octets for "
                f"a {mt}, outside (0, {self.MAX_WIRE_BYTES}]")
        if not 0 < size <= self.MAX_WIRE_BYTES:
            raise ConfigError(
                f"message codec {type(self.codec).__name__}: wire_size_bytes(..., "
                f"{self.signer!r}) = {size} for a {mt}, outside (0, {self.MAX_WIRE_BYTES}]. This "
                f"number is charged to airtime -> CBR -> collision loss -> latency -> "
                f"detection_time and reaches data_digest.")
        if self._wire_size_capability:
            delta = size - n
            key = (mt, self.signer)
            known = self._envelope.get(key)
            if known is None:
                if delta < 0:
                    raise ConfigError(
                        f"message codec {type(self.codec).__name__} declares the {_api_codec.CAP_WIRE_SIZE!r} "
                        f"capability -- 'wire_size_bytes() is derived from a real encode' -- but "
                        f"charges {size} B for a {mt} whose own encoded PDU is {n} B. A frame "
                        f"cannot be shorter than the payload it carries.")
                self._envelope[key] = delta
            elif delta != known:
                raise ConfigError(
                    f"message codec {type(self.codec).__name__} declares "
                    f"{_api_codec.CAP_WIRE_SIZE!r}, so wire_size_bytes() minus the encoded payload "
                    f"is the security envelope for signer {self.signer!r} and is a CONSTANT. It was "
                    f"{known} B and is now {delta} B for a {mt} ({size} B charged, {n} B encoded). "
                    f"A declared-from-the-encoder length that does not track the encoder is a "
                    f"fabricated CBR.")
        seen = self._probed.get(mt, 0)
        self._probed[mt] = seen + 1
        if seen == 0 or seen % self.DECODE_PROBE_EVERY == 0:
            self._probe_decode(mt, pdu, claim, seen)
        return n

    def _probe_decode(self, mt: str, pdu, claim, index: int) -> None:
        """Decode one emitted PDU and check it still carries the claim it was built from."""
        decode = self._decoders.get(mt)
        if decode is None:
            return                                     # no decoder for this PDU type: nothing to say
        try:
            back = decode(bytes(pdu))
        except NotImplementedError:
            return                                     # a codec that declares no decoder for it
        except Exception as e:
            raise ConfigError(
                f"message codec {type(self.codec).__name__}: the {mt.upper()} it emitted does not "
                f"decode -- decode_{mt}() raised {type(e).__name__}: {e}. These octets are written "
                f"into the dataset as evidence and would go into a TS 103 759 v2xPduEvidence entry; "
                f"a PDU nobody can decode is not evidence, and the length charged for it is not a "
                f"frame length. (probe at {mt} #{index})") from None
        dx = float(getattr(back, "x", float("nan"))) - float(claim.x)
        dy = float(getattr(back, "y", float("nan"))) - float(claim.y)
        if not (abs(dx) <= self.DECODE_TOLERANCE_M and abs(dy) <= self.DECODE_TOLERANCE_M):
            raise ConfigError(
                f"message codec {type(self.codec).__name__}: the {mt.upper()} it emitted decodes to "
                f"a position {dx:+.1f}, {dy:+.1f} m from the claim it was encoded from (tolerance "
                f"{self.DECODE_TOLERANCE_M} m). The emitted octets are what the dataset records as "
                f"evidence, so they have to be the message. (probe at {mt} #{index})")

    def size_for(self, claim, signer: str) -> int:
        """Frame length for a claim under a GIVEN signer arm.

        Used when a real security layer is active: the ARM is then decided by TS 103 097's
        once-per-second attachment rule rather than by config, but the SIZE must still come from
        the codec's measured envelope. See the note at the `seal()` call site for why the real
        `SignedMessage`'s own length is the wrong number to charge airtime for.
        """
        return int(self._sizer(claim, signer))

    def stats(self) -> dict:
        out = {"pdus": dict(sorted(self.count_by_type.items())),
               "payload_bytes": dict(sorted(self.bytes_by_type.items()))}
        out["mean_payload_bytes"] = {
            k: round(self.bytes_by_type[k] / self.count_by_type[k], 3)
            for k in sorted(self.count_by_type) if self.count_by_type[k]}
        return out


# --------------------------------------------------------------------------- #
# The DETECTOR seam (PLUGIN-ARCHITECTURE.md section 2.2 / phase 3)
# --------------------------------------------------------------------------- #
#: Keys one entry of the `plugins.check` ARRAY may carry. Closed so a typo is an error.
#:
#: `isolated: true` runs the check in its OWN INTERPRETER (`api/isolate.py`): the engine serialises
#: the `Observation` -- and only the `Observation` -- to a child, which returns a score. It is a
#: CONFIG key rather than a flag precisely so it lands verbatim in `manifest["config"]["plugins"]`
#: and replays: a dataset produced by an isolated detector says so in its own manifest.
_CHECK_ENTRY_KEYS = frozenset({"ref", "params", "conformance", "source_gate", "isolated"})

#: Keys the `plugins.fusion` section may carry.
_FUSION_SECTION_KEYS = frozenset({"ref", "params", "source_gate"})

#: Inside `plugins.check`, expands IN PLACE to this run's built-in suite in its canonical order.
#:
#: An EXTENSION to the design's section 3.3 shape, and a deliberate one. The array is the explicit,
#: ordered, replayable input the design requires -- but a user whose only goal is to ADD one
#: detector should not have to transcribe fourteen built-in names to keep them, and a transcription
#: is exactly the kind of hand-maintained list this phase exists to delete. `["@builtins", {...}]`
#: says "the shipped suite, then mine", stays a single config string, and replays byte-identically.
BUILTIN_CHECKS_TOKEN = "@builtins"

#: The default `fusion` reference. `streak_v1` reproduces the engine's historical rule exactly.
DEFAULT_FUSION = "streak_v1"


def default_check_refs(*, station_types: bool = False, denm: bool = False) -> tuple:
    """This run's built-in check suite, in the SHIPPED order.

    That order IS evaluation order IS `DET_KEYS` order, and it reaches `data_digest` through the
    fusion's stable-sort tie-break over equal scores. The two GATED entries keep the "registering is
    not enabling" property that stops a newly registered check from perturbing the default digest.

    **Expanded from `detectors.BUILTIN_CHECKS`, a fixed in-tree tuple, and NOT from the live
    registry -- which is a fix, not a style preference.** `_api_registry.builtin_names("check")` is a
    view of a process-global dict that `register_builtin()` writes into, and importing a
    distribution is enough to call it. So `@builtins` used to expand to *whatever was registered at
    the moment the config was resolved*: an installed-but-undeclared plugin that registered itself at
    import time got into the suite of a run that never asked for it, contributed a column, and moved
    the digest -- the exact inverse of the D6 property this seam is built on. Reading the shipped
    tuple instead makes the expansion a function of the ENGINE VERSION alone.
    """
    on = {"station_type": bool(station_types), "denm": bool(denm)}
    out = []
    for cls in _detectors.BUILTIN_CHECKS:
        gate = getattr(cls, "gate", None)
        if gate is None or on.get(gate, False):
            out.append(cls.reason_code)
    return tuple(out)


def _assert_not_hijacked(slot: str, ref: str, cls) -> None:
    """A ref that resolved through the BUILT-IN tier must be the class this engine ships.

    The other half of the same hole. `register_builtin` overwrites per (slot, name), so a third
    party could bind its own class to `positionJump` and be resolved as a built-in: `_builtin_params`
    would hand it the engine's config fields, `is_builtin` would let it keep the reserved
    `legacy_raw_compare` grandfathering, its column would NOT be `x_`-namespaced, and the source gate
    would skip it. Identity against the fixed in-tree mapping is the whole check.
    """
    shipped = (_detectors.BUILTIN_CHECK_BY_CODE if slot == "check"
               else _detectors.BUILTIN_FUSION_BY_NAME).get(ref)
    if shipped is not None and cls is not shipped:
        raise ConfigError(
            f"plugins.{slot} {ref!r} resolved to {cls.__module__}.{getattr(cls, '__name__', cls)!r}, "
            f"but {ref!r} is a BUILT-IN name owned by "
            f"{shipped.__module__}.{shipped.__name__}. Something called "
            f"register_builtin({slot!r}, {ref!r}, ...) and replaced it -- a third-party component "
            f"cannot take a built-in's name, its engine config fields or its reserved capabilities. "
            f"Give it its own ref and its own reason_code.")


def _checks_selection(cfg, *, station_types: bool, denm: bool) -> tuple:
    """((ref, params, conformance, source_gate, isolated), ...) for the check slot: `plugins` wins,
    else the built-ins."""
    default = tuple((r, {}, "off", _srcgate.DEFAULT_MODE, False)
                    for r in default_check_refs(station_types=station_types, denm=denm))
    sel = (cfg.plugins or {}).get("check") if isinstance(cfg.plugins, dict) else None
    if not sel:
        return default
    if isinstance(sel, (str, dict)):
        sel = [sel]
    if not isinstance(sel, (list, tuple)):
        raise ConfigError("plugins.check must be an ARRAY of {'ref': ..., 'params': {...}} entries "
                          "(its ORDER is digest-bearing, so it is an explicit input)")
    out = []
    for entry in sel:
        if entry == BUILTIN_CHECKS_TOKEN:
            out.extend(default)
            continue
        if isinstance(entry, str):
            entry = {"ref": entry}
        if not isinstance(entry, dict) or "ref" not in entry:
            raise ConfigError(f"plugins.check entry {entry!r} must be a ref string, "
                              f"{BUILTIN_CHECKS_TOKEN!r}, or {{'ref': ..., 'params': {{...}}}}")
        extra = sorted(set(entry) - _CHECK_ENTRY_KEYS)
        if extra:
            raise ConfigError(f"plugins.check entry: unknown key(s) {extra}; "
                              f"known: {sorted(_CHECK_ENTRY_KEYS)}")
        params = entry.get("params") or {}
        if not isinstance(params, dict):
            raise ConfigError("plugins.check[].params must be an object")
        mode = str(entry.get("conformance", "off"))
        if mode not in CONFORMANCE_MODES:
            raise ConfigError(f"plugins.check[].conformance must be one of "
                              f"{list(CONFORMANCE_MODES)} (got {mode!r})")
        isolated = entry.get("isolated", False)
        if not isinstance(isolated, bool):
            raise ConfigError(f"plugins.check[].isolated must be true or false (got {isolated!r}); "
                              f"true runs the check in its own interpreter, where the run's ground "
                              f"truth is not in the address space at all")
        if isolated and str(entry["ref"]) in _detectors.BUILTIN_CHECK_BY_CODE:
            # Refused HERE rather than in the worker, which would only report the built-in's name as
            # unresolvable: the child imports `scms_sim_ref.api` and nothing else, so it has never
            # heard of the engine's own registry.
            raise ConfigError(
                f"plugins.check {entry['ref']!r} is a BUILT-IN check and cannot be isolated: it IS "
                f"the engine, its knobs are engine config fields, and running it out of process "
                f"would buy nothing and cost a round trip per message. Isolation exists for "
                f"third-party code you have not reviewed.")
        if isolated and float(getattr(cfg, "live_interval_s", 0.0) or 0.0) > 0:
            # `live_state.json` is written DURING the loop and carries a per-vehicle state byte in
            # which 1 == attacker -- the oracle, refreshed every `live_interval_s`, in a file the
            # child can open. Withholding the ground-truth streams while leaving this on would make
            # the mode's claim false again, so the combination is refused rather than silently
            # degraded: a benchmark host that asked for a live map gets told, not disarmed.
            raise ConfigError(
                "plugins.check[].isolated=true with live_interval_s>0: live_state.json is written "
                "while the run is going and marks every attacker (state 1), so an isolated "
                "third-party detector could read the oracle out of it. Set live_interval_s=0 (drop "
                "--live-interval) for a run that grades a submitted detector.")
        # THE SOURCE GATE DEFAULTS OFF FOR AN ISOLATED CHECK, and that is the honest default rather
        # than a weakening. The gate is a static name-match whose entire justification is that an
        # in-process detector's `sys._getframe` reaches the reception loop's broadcast dict; out of
        # process that walk finds this engine's frames nowhere, so refusing a submission for
        # containing the name would be theatre. Set `"source_gate": "on"` explicitly and the parent
        # still screens the module file the worker reports -- reading a file is not importing it.
        sgate = _srcgate.check_mode(
            entry.get("source_gate", ("off" if isolated else _srcgate.DEFAULT_MODE)),
            "plugins.check[].source_gate")
        out.append((str(entry["ref"]), params, mode, sgate, isolated))
    seen = [r for r, _p, _c, _g, _i in out]
    dupes = sorted({r for r in seen if seen.count(r) > 1})
    if dupes:
        # The CHEAP half of the duplicate-column guard: the same ref twice, caught without resolving
        # anything. The half that actually bites -- two DIFFERENT refs resolving to the same
        # (plugin_id, reason_code), and therefore to the same column -- can only be caught after
        # resolution, and is in `build_checks`.
        raise ConfigError(f"plugins.check declares {dupes} more than once; each check contributes "
                          f"exactly one detnorm_* column, so duplicates are refused")
    return tuple(out)


def _fusion_selection(cfg):
    """(ref, params, source_gate) for the fusion slot."""
    sel = (cfg.plugins or {}).get("fusion") if isinstance(cfg.plugins, dict) else None
    if not sel:
        return DEFAULT_FUSION, {}, _srcgate.DEFAULT_MODE
    if isinstance(sel, str):
        sel = {"ref": sel}
    if not isinstance(sel, dict) or "ref" not in sel:
        raise ConfigError("plugins.fusion must be a string or {'ref': ..., 'params': {...}}")
    extra = sorted(set(sel) - _FUSION_SECTION_KEYS)
    if extra:
        raise ConfigError(f"plugins.fusion: unknown key(s) {extra}; "
                          f"known: {sorted(_FUSION_SECTION_KEYS)}")
    params = sel.get("params") or {}
    if not isinstance(params, dict):
        raise ConfigError("plugins.fusion.params must be an object")
    return (str(sel["ref"]), params,
            _srcgate.check_mode(sel.get("source_gate", _srcgate.DEFAULT_MODE),
                                "plugins.fusion.source_gate"))


def _detector_env(cfg) -> dict:
    """Config scalars the detection layer may read at construction. NOT the oracle: every one is
    user-supplied config that already appears verbatim in `manifest["config"]`."""
    return {"dt": cfg.dt, "seed": cfg.seed, "t0": 0.0}


def _builtin_params(cls, cfg, declared: dict, slot: str, ref: str) -> dict:
    """A BUILT-IN's params are the engine's own config fields, resolved from `cfg`.

    Its knobs were `cfg.<field>` reads in the inline block; they already have `_FIELD_META` entries,
    argparse flags, GUI widgets and a `manifest["config"]` slot. Mirroring them (rather than
    re-declaring defaults inside the check) means `--detector-z-threshold` keeps working and there
    is exactly ONE definition of each. Overriding one through `plugins.<slot>.params` is refused
    for the same reason: two spellings of one knob is how they drift apart.
    """
    fields = getattr(cls, "cfg_fields", {}) or {}
    if declared:
        raise ConfigError(
            f"plugins.{slot} {ref!r} is a BUILT-IN: its knobs are engine config fields, so set "
            f"{sorted(fields.values())} directly instead of through params (got {sorted(declared)})")
    return {name: getattr(cfg, field) for name, field in fields.items()}


def _plugin_params(cls, declared: dict, slot: str, ref: str) -> dict:
    """A third party's params: its own `FieldSpec` defaults, overridden by what the config declared,
    with an undeclared name refused rather than silently ignored."""
    spec = _plugin_config_fields(cls)
    params = {name: fs.default for name, fs in spec.items()}
    for k, v in sorted(declared.items()):
        fs = spec.get(k)
        if fs is None and spec:
            raise ConfigError(f"plugins.{slot}.params: {ref} declares no field {k!r}; "
                              f"known: {sorted(spec)}")
        if fs is not None:
            fs.validate(f"plugins.{slot}.params.{k}", v)
        params[k] = v
    return params


class LoadedCheck:
    """One resolved, constructed check plus everything the engine needs to run and record it."""

    __slots__ = ("ref", "instance", "column", "plugin_id", "builtin", "soft", "precision",
                 "msg_types", "vru_suppressed", "params", "rng", "provenance", "isolated",
                 "guard_label")

    def __init__(self, ref, instance, column, plugin_id, builtin, params, rng, provenance,
                 isolated=False, guard_label=None):
        self.ref, self.instance, self.column = ref, instance, column
        #: The label this check's `evaluate` is guarded under, or None for "not guarded" -- a
        #: built-in, an isolated worker (it has no code in this process to guard) or a plugin whose
        #: `source_gate` the config explicitly turned off. See `_guard_label`.
        self.guard_label = guard_label
        self.plugin_id, self.builtin = plugin_id, builtin
        #: True when `instance` is an `api.isolate.IsolatedCheck` -- a proxy for a plugin running in
        #: its own interpreter. It answers `evaluate(obs, state, params, rng)` exactly as an
        #: in-process check does, and carries the same declared metadata, read off the worker's
        #: handshake instead of off a class this process imported.
        self.isolated = bool(isolated)
        self.soft = bool(getattr(instance, "soft", False))
        # Section 4.1: the engine rounds a plugin's returned score to its DECLARED precision before
        # the `>= 1.0` compare, because that compare is a cliff and a last-ulp difference flips a
        # whole report. The built-ins compare the RAW float -- that is what the goldens were pinned
        # on -- and declare `legacy_raw_compare`, which the resolver refuses from third parties.
        caps = (instance.capabilities if isolated else instance.capabilities())
        self.precision = (None if _api_detect.CAP_LEGACY_RAW_COMPARE in caps
                          else int(getattr(instance, "precision", 3)))
        self.msg_types = tuple(getattr(instance, "msg_types", ("cam",)))
        self.vru_suppressed = bool(getattr(instance, "vru_suppressed", False))
        self.params, self.rng, self.provenance = params, rng, provenance


class CheckSuite:
    """The ordered check vector plus the fusion -- the detection layer of one run.

    `keys` is `DET_KEYS`: the ordered HARD columns. The order is digest-bearing through the FUSION's
    stable-sort tie-break (the rows themselves are canonicalised with sorted keys, so insertion order
    never reaches the bytes). `soft_keys` are scored and emitted but can never fire.
    """

    __slots__ = ("checks", "fusion", "fusion_params", "fusion_rng", "fusion_ref", "keys",
                 "soft_keys", "columns", "zero", "cam_plan", "denm_plan", "vru_suppressed",
                 "sig_column", "sig_suppressed", "third_party", "fusion_wrap", "_prov",
                 "workers", "isolated", "fusion_guard")

    def __init__(self, checks, fusion, fusion_params, fusion_rng, fusion_ref, prov,
                 *, fusion_builtin=True, fusion_pid=None, workers=(), fusion_guard=None):
        self.checks = tuple(checks)
        #: The label a THIRD-PARTY fusion's `decide` is guarded under, or None. The fusion sees the
        #: same caller frame the checks do, one call later, so gating only the check slot would
        #: leave the identical vector open.
        self.fusion_guard = fusion_guard
        #: Live `api.isolate.IsolatedCheck` workers, in load order. `close()` reaps them; the child
        #: also exits on EOF of its stdin, so a parent that dies without reaching `close()` still
        #: leaves nothing behind.
        self.workers = tuple(workers)
        self.isolated = tuple(c.column for c in self.checks if getattr(c, "isolated", False))
        #: The plugin id a THIRD-PARTY fusion's state is namespaced under, or None for the built-in.
        #:
        #: The check slot has always wrapped a third party's per-(rx, sender) state in
        #: `NamespacedState`; the fusion slot handed the raw engine dict to EVERYONE, built-in or
        #: not. So a third-party fusion -- which is called on the same `st` object, once per message,
        #: after the whole check vector -- could read and REWRITE `h` (the claim history every
        #: history-bearing check compares against), `streak` (the built-in fusion's consecutive-
        #: violation counters) and `kf` (the soft tracker's state), with none of the discipline the
        #: check slot enforces one call earlier on the same dict. It gets the same wrapper now.
        self.fusion_wrap = None if fusion_builtin else fusion_pid
        self.fusion, self.fusion_params = fusion, fusion_params
        self.fusion_rng, self.fusion_ref = fusion_rng, fusion_ref
        self.keys = tuple(c.column for c in self.checks if not c.soft)
        self.soft_keys = tuple(c.column for c in self.checks if c.soft)
        self.columns = self.keys + self.soft_keys
        #: The template every per-message score vector is copied from: every column present, in
        #: declared order, at 0.0. A check that does not apply to this message type therefore scores
        #: exactly 0.0, which is what the inline `det = {k: 0.0 for k in DET_KEYS}` did.
        self.zero = {c: 0.0 for c in self.columns}
        self.cam_plan = self._plan("cam")
        self.denm_plan = self._plan("denm")
        self.vru_suppressed = tuple(c.column for c in self.checks if c.vru_suppressed)
        sig = [c.column for c in self.checks
               if c.builtin and c.column == _detectors.SignatureVerification.reason_code]
        self.sig_column = sig[0] if sig else None
        #: On a signature failure the content cannot be trusted at all, so every plausibility score
        #: is suppressed and only the crypto failure is reported. That suppression is the engine's,
        #: not any one check's, and it covers third-party columns too.
        self.sig_suppressed = tuple(c for c in self.columns if c != self.sig_column)
        self.third_party = tuple(c for c in self.checks if not c.builtin)
        self._prov = prov

    def _plan(self, msg_type):
        """The per-message call plan for one message type: (column, evaluate, params, rng, wrap,
        precision) tuples, in declared order. Built ONCE per run; the reception loop is the hottest
        loop in the engine and must not re-derive this per message."""
        plan = []
        for c in self.checks:
            if msg_type not in c.msg_types:
                continue
            # An ISOLATED check gets `None` too, and for the opposite reason to a built-in's: the
            # namespacing still happens, but on the far side of the serialiser. The proxy has to
            # extract `state["plugin:<id>"]`, ship it and put it back regardless, so wrapping the
            # dict here would only build a `NamespacedState` for the proxy to immediately unwrap.
            wrap = None if (c.builtin or getattr(c, "isolated", False)) else c.plugin_id
            # THE RUNTIME GUARD IS BOUND INTO THE CALL PLAN, once per run, so the hot loop calls the
            # guarded bound method directly and a built-in's entry is the raw bound method it always
            # was (`guarded(fn, None) is fn`). Measured cost on the guarded path: ~100 ns per call.
            plan.append((c.column, _pguard.guarded(c.instance.evaluate, c.guard_label),
                         c.params, c.rng, wrap, c.precision))
        return tuple(plan)

    def begin_step(self, step: int) -> None:
        for c in self.checks:
            c.rng.begin_step(step)
        self.fusion_rng.begin_step(step)

    def provenance(self) -> list:
        return [p() for p in self._prov]

    def close(self) -> None:
        """FINISH every isolated worker and reap its process. Always called, on every path.

        `finish()` is what collects the stream labels the manifest records and what asserts the
        worker scored exactly as many messages as the engine sent it -- a worker that skipped one
        produced a dataset nobody can reproduce, so that is a hard error and not a warning.
        """
        for w in self.workers:
            try:
                w.finish()
            finally:
                w.close()


def build_checks(cfg, *, station_types: bool = False, denm: bool = False) -> CheckSuite:
    """Resolve + construct the run's detection layer. Once, before step 0; every failure fatal here.

    A plugin that raises mid-loop would produce a partial dataset whose digest matches nothing, and
    it must NOT take the SIGINT path that finalises a VALID manifest for a partial run. Instances are
    PER-RUN objects, never module globals, so the in-process multi-run drivers cannot cross-
    contaminate.
    """
    loaded, prov, columns = [], [], {}
    workers: list = []
    try:
        return _build_checks(cfg, loaded, prov, columns, workers,
                             station_types=station_types, denm=denm)
    except BaseException:
        # An isolated check that loads and a later one that does not must not leave a live child
        # behind: the in-process multi-run drivers (`datagen/foundry.py`, `campaign.py`,
        # `massive.py`, `gui/agent.py`) would then accumulate one per failed run.
        for w in workers:
            w.close()
        raise


def _build_checks(cfg, loaded, prov, columns, workers, *, station_types: bool,
                  denm: bool) -> CheckSuite:
    for order, (ref, declared, mode, sgate, isolated) in enumerate(
            _checks_selection(cfg, station_types=station_types, denm=denm)):
        if isolated:
            loaded.append(_load_isolated_check(cfg, order, ref, declared, mode, sgate, columns,
                                               prov, workers))
            continue
        # Snapshot BEFORE `resolve`, which imports the plugin's module -- module-level code is an
        # earlier hook than `__init__`. See `build_channel`.
        _guard = _integrity.Sentinel(armed=_integrity.armed_for(cfg))
        cls, how, iv, _shape = _api_registry.resolve("check", ref)
        _assert_not_hijacked("check", ref, cls)
        builtin = how == "builtin" and _api_registry.is_builtin("check", cls)
        if not builtin:
            # THE SOURCE GATE, at plugin RESOLUTION -- the moment a third-party class first exists in
            # this process and before anything of it has been constructed or called. A guard rail,
            # never a sandbox: see api/srcgate.py, whose refusal message says so in full.
            _srcgate.gate("check", ref, cls, mode=sgate)
        # ATTESTATION FIRST, and OUT OF PROCESS -- the check slot repeated the channel slot's
        # ordering defect verbatim (instantiate at run.py:1493, attest at :1499), so a hostile
        # `__init__` ran before the D1-D7 suite that was supposed to gate it. See `_attest`.
        attested = _attest("check", ref, declared) if mode == "required" else None
        pid = _api_registry.plugin_id_of(cls, _fallback_pid(ref))
        params = (_builtin_params(cls, cfg, declared, "check", ref) if builtin
                  else _plugin_params(cls, declared, "check", ref))
        rng_ns = RngNamespace(cfg.seed, pid)
        # A third party's import-time and `__init__` code is arbitrary code running in the engine's
        # interpreter, and any rebind it performs is fatal before step 0. See `build_channel`.
        inst = _api_registry.instantiate(cls, params=params, rng=rng_ns, env=_detector_env(cfg))
        if not builtin:
            _guard.verify("while LOADING a detector plugin", subject=f"plugins.check {ref!r}")
        caps = _api_registry.check_capabilities("check", ref, cls, how, inst.capabilities())
        code = str(inst.reason_code)
        # SECTION 4.4: a third party's column is `x_<plugin_id>_<code>`, so it can never collide
        # with a built-in's or with a future standardised name. The namespaced string is ALSO the
        # reason code that lands in `reason_codes`, so the collision-freedom is end-to-end.
        column = _claim_column(columns, ref, order, pid, code, builtin)
        loaded.append(LoadedCheck(ref, inst, column, pid, builtin, params, rng_ns, None,
                                  guard_label=(None if (builtin or sgate == "off")
                                               else f"plugins.check {ref!r}")))
        prov.append(_check_provenance("check", order, ref, cls, how, iv, caps, rng_ns, params,
                                      conformance=attested))
    fref, fdeclared, fsgate = _fusion_selection(cfg)
    _fguard = _integrity.Sentinel(armed=_integrity.armed_for(cfg))
    fcls, fhow, fiv, _fshape = _api_registry.resolve("fusion", fref)
    _assert_not_hijacked("fusion", fref, fcls)
    fbuiltin = fhow == "builtin" and _api_registry.is_builtin("fusion", fcls)
    if not fbuiltin:
        _srcgate.gate("fusion", fref, fcls, mode=fsgate)
    fpid = _api_registry.plugin_id_of(fcls, _fallback_pid(fref))
    fparams = (_builtin_params(fcls, cfg, fdeclared, "fusion", fref) if fbuiltin
               else _plugin_params(fcls, fdeclared, "fusion", fref))
    frng = RngNamespace(cfg.seed, fpid)
    keys = tuple(c.column for c in loaded if not c.soft)
    fenv = dict(_detector_env(cfg), keys=keys,
                soft_keys=tuple(c.column for c in loaded if c.soft))
    if fbuiltin:
        # GRANDFATHERED, and structurally reachable by a BUILT-IN ONLY: the env that carries the
        # engine's global stream is built differently for a third party, which never sees the key.
        # The `report_prob` Bernoulli is drawn from that single stream, whose draw COUNT AND ORDER
        # are load-bearing for every pinned golden; moving it to a keyed namespace is a scheduled
        # re-pin, not something a refactor may do quietly. `check_capabilities` below independently
        # refuses the DECLARATION from anything that is not a built-in.
        fenv["legacy_rng"] = _LEGACY_RNG.get("rng")
    fusion = _api_registry.instantiate(fcls, params=fparams, rng=frng, env=fenv)
    if not fbuiltin:
        _fguard.verify("while LOADING the fusion plugin", subject=f"plugins.fusion {fref!r}")
    fcaps = _api_registry.check_capabilities("fusion", fref, fcls, fhow, fusion.capabilities())
    fprov = _check_provenance("fusion", 0, fref, fcls, fhow, fiv, fcaps, frng, fparams)
    return CheckSuite(loaded, fusion, fparams, frng, fref, prov + [fprov],
                      fusion_builtin=fbuiltin, fusion_pid=fpid, workers=workers,
                      fusion_guard=(None if (fbuiltin or fsgate == "off")
                                    else f"plugins.fusion {fref!r}"))


def _load_isolated_check(cfg, order, ref, declared, mode, sgate, columns, prov, workers):
    """Load one `check` OUT OF PROCESS. **The plugin's module is never imported here.**

    The order is the whole point, and it is the same discipline `_attest` established for the
    in-process slots, taken one step further:

    1. **spawn + resolve in the child.** The plugin's module-level code and its `__init__` run in an
       interpreter that holds no engine object -- no broadcast dict, no `Vehicle`, no
       `PipelineConfig`, no global `random.Random(cfg.seed)`, and no frame of this loop.
    2. **screen and validate in the parent, from what the child reported.** The declared params are
       range-checked against the `FieldSpec`s the worker sent, by `_params_from_spec` (which is
       `_plugin_params` with the spec taken off the wire instead of out of a class this process
       imported); the column is claimed against the same table the in-process entries use; and
       if `source_gate` was explicitly turned on, the parent reads (does not import) the module file
       the worker named and scans it.
    3. **construct in the child**, with the params the parent resolved -- and refuse if the worker's
       echoed `params_sha256` is not the one the manifest is about to record.

    No integrity `Sentinel` brackets this, and its absence is deliberate rather than an oversight:
    the sentinel exists to catch a third party rebinding objects in THIS interpreter, and an isolated
    plugin has no code running here to do it with. The whole-run sentinel still covers the run.
    """
    attested = (_attest("check", ref, declared) if mode == "required" else None)
    # No timeout knob on `PipelineConfig`, deliberately: it is a FAIL-STOP ceiling, it can only ever
    # turn a run into a failure, and adding a config field would put a number that cannot affect any
    # result into `manifest["config"]` and into `config_schema()`.
    worker = _isolate.IsolatedCheck(ref, declared, seed=cfg.seed, env=_detector_env(cfg),
                                    timeout=_isolate.DEFAULT_TIMEOUT_S,
                                    deny=(cfg.out_dir,) if cfg.out_dir else ())
    workers.append(worker)
    meta = worker.spawn()
    if sgate == "on":
        # The gate, applied to a module the parent has read but not imported. Advisory by default
        # in this mode (see `_checks_selection`) and enforced when the config asks for it.
        _gate_isolated_source(ref, meta)
    spec = _isolate.spec_from_wire(meta.get("config_fields") or {})
    params = _params_from_spec(spec, declared, "check", ref)
    caps = _api_registry.check_capabilities("check", ref, None, "isolated",
                                            worker.construct(params))
    column = _claim_column(columns, ref, order, worker.plugin_id, worker.reason_code, False)
    prov.append(_isolated_provenance(order, worker, params, attested))
    return LoadedCheck(ref, worker, column, worker.plugin_id, False, params, worker.rng, None,
                       isolated=True)


def _params_from_spec(spec: dict, declared: dict, slot: str, ref: str) -> dict:
    """`_plugin_params`, driven by a `FieldSpec` map instead of a class.

    Identical semantics deliberately: the defaults are the spec's, an undeclared name is REFUSED
    rather than ignored, and every supplied value goes through the plugin author's own bounds. The
    only difference is where the spec came from -- a wire frame instead of a `config_fields()` call
    in this process -- which is what keeps the plugin out of the engine.
    """
    params = {name: fs.default for name, fs in spec.items()}
    for k, v in sorted((declared or {}).items()):
        fs = spec.get(k)
        if fs is None and spec:
            raise ConfigError(f"plugins.{slot}.params: {ref} declares no field {k!r}; "
                              f"known: {sorted(spec)}")
        if fs is not None:
            fs.validate(f"plugins.{slot}.params.{k}", v)
        params[k] = v
    return params


def _gate_isolated_source(ref: str, meta: dict) -> None:
    """Run the source gate over the module file the WORKER reported, without importing it."""
    path = meta.get("module_path")
    if not path or not os.path.isfile(path):
        raise _srcgate.SourceGateError(
            f"plugins.check {ref!r}: source_gate='on' with isolated=true, but the worker reported "
            f"no readable module file ({path!r}). The gate refuses what it cannot read.")
    try:
        from importlib.util import decode_source
        with open(path, "rb") as fh:
            src = decode_source(fh.read())
    except OSError as e:
        raise _srcgate.SourceGateError(f"plugins.check {ref!r}: cannot read {path}: {e}") from None
    findings = _srcgate.scan_source(src, path)
    if findings:
        raise _srcgate.SourceGateError(_srcgate._message("check", ref, path, findings))


def _isolated_provenance(order, worker, params, conformance):
    """Deferred exactly as the in-process form is: `declared_streams` is what the worker reports at
    FINISH, and FINISH has not happened yet when the suite is built."""
    def _build():
        return _isolate.provenance_record("check", order, worker, params, conformance=conformance)
    return _build


def _claim_column(columns: dict, ref, order, pid, code, builtin) -> str:
    """The check's emitted column, refused if another entry already claimed it.

    **DE-DUPLICATION BY THE RESOLVED COLUMN, not by the ref string.** `_checks_selection` can only
    see that two entries spell the same ref; what actually collides is the resolved
    `(plugin_id, reason_code)` pair, and two DIFFERENT refs reach one pair routinely -- a subclass,
    an alias, a re-export, the same class published under two entry-point names, a dotted path
    alongside the entry-point name for the same class.

    Measured before this guard: `@builtins` plus a check plus a trivial subclass of it loaded 15
    checks into 14 distinct column slots. The per-message call plan evaluated that column twice, the
    second score silently overwrote the first in every report row, and the zero-template the engine
    copies per message was one entry short. No warning anywhere, and the ML tables carried one
    column where the manifest's lock recorded two detectors.
    """
    column = code if builtin else _api_detect.namespaced_key(pid, code)
    if column in columns:
        first_ref, first_order = columns[column]
        raise ConfigError(
            f"plugins.check entries {first_order} ({first_ref!r}) and {order} ({ref!r}) both "
            f"resolve to column {column!r}"
            + ("" if builtin else f" -- the (plugin_id, reason_code) pair ({pid!r}, {code!r})") +
            f". Each check contributes exactly one detnorm_* column, so the second would silently "
            f"overwrite the first in every report row. Two different refs may not share a "
            f"(plugin_id, reason_code) pair; change one of them.")
    columns[column] = (ref, order)
    return column


def _fallback_pid(ref: str) -> str:
    """A plugin id for a ref that declares none: the class name, lowercased."""
    tail = ref.rsplit(":", 1)[-1] if ":" in ref else ref
    out = "".join(ch if ch.isalnum() else "_" for ch in tail).lower().strip("_")
    return out[:32] or "plugin"


def _check_provenance(slot, order, ref, cls, how, iv, caps, rng_ns, params, conformance=None):
    """Deferred exactly as the channel's is: `declared_streams` is the set of labels the plugin
    ACTUALLY consumed, and a check that draws only during the loop has consumed none at
    construction time. Recording an always-empty list would be fabrication by omission."""
    def _build():
        return _api_registry.make_provenance(slot, order, ref, cls, how, iv, caps,
                                             rng_ns.declared_streams(), params,
                                             conformance=conformance)
    return _build


def _round_score(value, precision: int, column: str) -> float:
    """A third-party check's returned score, rounded to its DECLARED precision and range-checked.

    Section 4.1: the engine rounds BEFORE the `>= 1.0` compare, because that comparison is a CLIFF --
    a last-ulp difference flips a whole report, and float reduction order is not portable. A NaN or
    an infinity is refused outright rather than propagated: it would compare false against every
    threshold and silently disable the check for the rest of the run.
    """
    v = float(value)
    if not (v == v and -math.inf < v < math.inf):
        raise ConfigError(f"check {column!r} returned a non-finite score {value!r}; a detnorm must "
                          f"be a finite float (>= 1.0 == violating)")
    return round(v, precision)


#: The run's global `random.Random(cfg.seed)`, parked here for the duration of `build_checks` so the
#: grandfathered built-in fusion can be handed it. A module global rather than a parameter because
#: `build_checks` is also called by the conformance harness and by tests with no engine loop; it is
#: written and cleared inside `run_pipeline`, and it is never handed to a third-party plugin.
_LEGACY_RNG: dict = {"rng": None}


#: Drift a replay was told to ACCEPT, waiting to be written into that replay's own manifest.
#:
#: Section 4.3 is explicit that `--allow-plugin-drift` *"writes the drift into the new manifest, it
#: does not silence it"*, and that is the only version of the flag worth having: a silenced drift
#: turns a replay into an unmarked different run, which is the exact failure the lock exists to
#: prevent.
#:
#: BOUND TO THE CONFIG OBJECT, not merely to the process. `config_from_dict` records
#: `{"cfg": <the config it just built>, "drifts": [...]}` and `run_pipeline` claims it only when the
#: config it was handed IS that object. The weaker "consume at the start of the next run" design was
#: tried and is wrong: a caller that builds a drifted config and never runs it (a `--check-config`,
#: a GUI validation, a test) leaves a record that the NEXT unrelated `run_pipeline` then stamps into
#: its manifest -- and the in-process multi-run drivers (`datagen/foundry.py`, `campaign.py`,
#: `massive.py`, `gui/agent.py`) make that a routine occurrence, not a corner case. The strong
#: reference is deliberate: it keeps the config alive so an `is` comparison cannot be fooled by
#: address reuse, and it is bounded at exactly one object.
#:
#: A module global rather than a field on the config because `_write_manifest` serialises
#: `cfg.__dict__` verbatim -- anything parked on the config becomes a config key and would then have
#: to survive `config_from_dict` on the way back in.
_PLUGIN_DRIFT_ALLOWED: dict = {"cfg": None, "drifts": []}


def _claim_drift_record(cfg) -> list:
    """Take the drift record belonging to `cfg`, if there is one. Idempotent, and a no-op for any
    config that was not the one a drifted replay produced."""
    if _PLUGIN_DRIFT_ALLOWED["cfg"] is not cfg:
        return []
    drifts = list(_PLUGIN_DRIFT_ALLOWED["drifts"])
    _PLUGIN_DRIFT_ALLOWED["cfg"], _PLUGIN_DRIFT_ALLOWED["drifts"] = None, []
    return drifts


def plugin_block(records, drift=None, integrity=None) -> dict:
    """`manifest["plugins"]` -- the LOCK (what was loaded), against `cfg.plugins` (what was asked).

    Not part of `data_digest_sha256` (which by design covers data files only) and carrying its own
    `provenance_digest`. Never fabricates: an identity that cannot be established is recorded as
    `null` plus `provenance_incomplete: true`.
    """
    loaded = [r.to_dict() for r in records]
    block = {"api_version": _api_registry.API_VERSION,
             "interface_versions": {
                 _api_channel.INTERFACE_NAME: _api_channel.INTERFACE_VERSION.split("/", 1)[1],
                 _api_detect.INTERFACE_NAME: _api_detect.INTERFACE_VERSION.split("/", 1)[1]},
             "loaded": loaded,
             "provenance_digest": _api_registry.provenance_digest(records)}
    if drift:
        # Present ONLY when a replay actually accepted drift, so the default manifest shape is
        # untouched. Its presence is the machine-readable statement "this dataset was produced by
        # code that did not match the manifest it was replayed from", and `loaded` above records
        # what ran, so the two locks diff cleanly.
        block["drift_allowed"] = list(drift)
    if integrity:
        # Present ONLY when the run armed integrity monitoring (i.e. `cfg.plugins` declared
        # something), so a manifest written without plugins is byte-identical to what it was. It
        # records what the monitor covered and what it saw, because "the engine was still made of the
        # objects it started with, and its own stream had advanced exactly `engine_rng_words` words"
        # is a claim an artifact should carry rather than a sentence in a changelog. Read it with
        # `api/integrity.py`'s docstring: a PASS means nothing on the watch list moved, never that
        # the plugin was honest.
        block["integrity"] = dict(integrity)
    return block


def empty_plugin_block() -> dict:
    return plugin_block([])


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
    attack_magnitude_scale: str = ""     # per-type falsification-magnitude multiplier (on TOP of the
                                         # global attack_intensity dial), e.g.
                                         # "RandomPos:2.0,ConstPosOffset:0.5,HeadingOffset:1.5".
                                         # Empty (default) => every type scale 1.0 => byte-identical.
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
    # --- ORACLE mobility record (opt-in; DEFAULT OFF, so the default file set, the default
    # `data_digest` and every pinned golden are byte-identical) -----------------------------------
    # `gt_emissions_sample.jsonl` is written INSIDE the broadcast pre-pass, which means it is gated
    # on `enforced()`: a revoked vehicle stops broadcasting, so its kinematic record ENDS at
    # revocation while the vehicle keeps driving. That makes the emission stream a record of what
    # the MA could HEAR, which is exactly right for a detection dataset and exactly wrong for a
    # traffic measurement. Measured on the InTAS AM peak hour: 9,143 of 14,896 vehicles revoked
    # (61.38%) and only 5,949,526 of 13,589,568 vehicle-steps surviving (43.78%) -- and with
    # detection precision 0.308 most of those revocations were BENIGN vehicles. The loss GROWS with
    # run length and with the false-positive rate, so it cannot be corrected by a constant.
    # See docs/realism/TRAFFIC-PANEL-SURVIVORSHIP.md and PYTHON-ENGINE-VALIDATION.md section 1.
    #
    # With this on, the engine writes the mobility a SECOND time, from BEFORE the enforcement gate:
    # every active station, every step, whatever the CRL says, whatever `emit_sample_prob` is and
    # whatever the GNSS-jam draw did. `datagen.realism_bench` prefers it for the whole traffic panel.
    # It is ORACLE (`ground_truth/`, `_visibility=ORACLE`, forbidden-key set) and is withheld from an
    # isolated third-party detector exactly like the other two ground-truth streams.
    emit_mobility_oracle: bool = False    # write ground_truth/gt_mobility_oracle.jsonl (un-enforced)
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
    # "geometric" is the third model: 3GPP TR 37.885 path loss under a per-link LOS / NLOSv (vehicle
    # in the way) / NLOSb (building in the way) classification, Gudmundson AR(1) shadowing carried
    # per link, per-packet Nakagami-m fading and an independent-survival loss product. It consults
    # the radio_env / radio_tx_power_dbm / radio_rx_sensitivity_dbm / radio_nlosb_density_per_km
    # knobs below and NONE of the logdistance ones; "disc" (default) takes neither branch.
    radio_model: str = "disc"            # "disc" (hard range) | "logdistance" | "geometric"
    pathloss_exponent: float = 2.7       # log-distance exponent n (urban ~2.7-3.5; free space 2.0)
    shadowing_sigma_db: float = 4.0      # log-normal shadowing std (dB); 0 -> near-hard cutoff at range
    rx_sensitivity_margin_db: float = 0.0  # + shrinks / - extends the effective range vs radio_range_m
    # candidate-window cap for logdistance reception (see the RADIO_CAP_* module notes). Consulted ONLY
    # when radio_model=="logdistance"; the "disc" default takes neither, so it is byte-identical regardless.
    radio_cap_sigma: float = RADIO_CAP_SIGMA        # candidate cap headroom in shadow standard deviations
    radio_cap_max_mult: float = RADIO_CAP_MAX_MULT  # hard ceiling on cap / range (bounds the cell search)
    # --- geometric (3GPP TR 37.885) channel model; consulted ONLY when radio_model=="geometric" ---
    radio_env: str = "urban"             # TR 37.885 LOS family: "urban" | "highway" (NLOS reuses urban)
    radio_tx_power_dbm: float = 23.0     # EIRP. A deployed ITS-G5/DSRC OBU runs 20-23 dBm (ETSI caps
                                         # EIRP at 33); the vendored VeReMi-NextGen INET config's
                                         # 13.0103 dBm reaches only ~204 m median on highway LOS, so
                                         # the >=500 m awareness gate is unreachable below ~20.8 dBm.
    radio_rx_sensitivity_dbm: float = -81.0   # decode floor (vendored NextGen 6 Mb/s 802.11p profile)
    radio_nlosb_density_per_km: float = 4.0   # SYNTHETIC-MAP FALLBACK ONLY: expected building
                                         # blockages per km of link path, P(LOS) = exp(-lambda*d),
                                         # used when the map carries no footprints. Ignored entirely
                                         # once real building polygons are present.
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
    # --- REAL imported signal programs (mock_pipeline/signals.py) ------------------------------
    # THE ACTIVATION SURFACE for `roads._LaneFrameMixin.set_signal_plan` / `signals.SignalPlan`.
    # `traffic_lights` above is a TOY: one 24 s cycle for the whole city, split half/half between
    # "mostly E-W" and "mostly N-S" off a graph 2-colouring, applied to EVERY junction or to none,
    # with no yellow, no turn phase, no offset, and no notion that the MOVEMENT (which way you are
    # turning) decides your colour. `real_signals` instead drives each junction from the
    # `<tlLogic>` program the .net.xml actually ships -- InTAS carries 98 of them, cycles 77-116 s
    # (86 of 98 exactly 90 s), 3-11 phases, 1042 controlled connections, 1489 green characters of
    # which 303 are PERMISSIVE 'g', and every one of them carrying a yellow.
    #
    # THE THREE COLOURS ARE DISTINCT, behaviourally and not just in the record:
    #   'G' protected green -- the movement owns the junction, proceed;
    #   'g'/'s' permissive green -- proceed but GIVE WAY to the conflicting protected stream (a
    #           permissive left crosses oncoming through traffic; flattening 'g' into 'G' would put
    #           two head-on movements through each other at every permissive phase);
    #   'y'/'u' yellow -- stop, unless already inside the dilemma zone (cannot stop at the
    #           comfortable deceleration), which is the standard rule and keeps yellow from
    #           manufacturing -6 m/s^2 emergency stops that read as attacks;
    #   'r'     red -- stop.
    # A junction the plan does not govern answers None and keeps TODAY'S behaviour exactly: the toy
    # cycle when `traffic_lights` is on, no signal at all when it is off. That is what makes this
    # composable with `traffic_lights` rather than an alternative to it.
    #
    # Only an IMPORTED city ships programs, so this needs road_network="sumo" (read straight from
    # the .net.xml) or "custom" with a document carrying the `signal_programs` layer that
    # `netimport.py --signals` writes. Default OFF -> `set_signal_plan` is never called, the map
    # allocates nothing, `signal_char` is not reached, and NOT ONE rng draw is added (a movement's
    # colour at time t is a pure function of the program), so every pinned golden holds.
    real_signals: bool = False
    # --- pedestrian infrastructure (mock_pipeline/vru.py) ---------------------------------------
    # THE ACTIVATION SURFACE for `vru.SidewalkNetwork`. Without it a VRU is placed
    # `offroad_tol_m * (1.3..2.0)` metres off a random junction in BOTH axes and walks a dead
    # straight line for its whole life: measured on InTAS, 5.59% of its sampled positions are on a
    # pedestrian-legal area and 54.47% are beyond `offroad_tol_m` of any road. With it, a VRU walks
    # derived sidewalks, waits at the kerb and crosses at crossings. Draws exclusively from
    # `f"{seed}:vruwalk:{vid}"`, a stream that exists nowhere else, so it perturbs neither the
    # vehicle fleet nor the existing `f"{seed}:vru:{vid}"` placement stream. Default OFF, and
    # reached only when vru_pct > 0 -> byte-identical on every pinned path.
    sidewalks: bool = False
    sidewalk_width_m: float = 2.0        # footway width (m); RASt 06 puts a two-way footway at 2.5,
                                         # absolute minimum 1.5 -- 2.0 is the documented middle
    kerb_clearance_m: float = 0.5        # gap from the kerb line to the inner edge of the footway
    crossing_wait_max_s: float = 8.0     # longest kerb wait at an UNSIGNALISED crossing (a gap-
                                         # acceptance surrogate: vehicles do not yield to pedestrians)
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
    # --- plugins (D2) -------------------------------------------------------------------------
    # THE activation surface. Added at the END of the dataclass so field order is stable, and
    # DEFAULTING TO EMPTY so every pinned golden holds -- that is not a happy accident, it is the
    # requirement that dictated the shape (registering is not enabling).
    #
    #   {"channel_model": {"ref": "geometric", "params": {}}}
    #
    # `ref` resolves through three tiers -- built-in registry name, installed entry point, then a
    # dotted path "package.module:Class". Only a CONFIG FIELD lands in `manifest["config"]` and
    # replays through `config_from_dict`; entry-point iteration order is machine state, which is
    # why discovery may be automatic but activation never is.
    plugins: dict = field(default_factory=dict)
    # --- directed carriageway geometry (roads.py `_LaneFrameMixin`) ----------------------------
    # THE ACTIVATION SURFACE for `roads.enable_directed_lanes()` / `roads.edges_from_directed()`.
    # Both existed, were tested, and were reachable ONLY by monkeypatching `roads.GridNetwork` from
    # Python -- every producer of the "head-on overlaps 117/49/32 -> 0" measurement (the commit
    # message, `tests/test_directed_roads.py`, `datasets/_netfidelity/_ab_directed.py`) installs a
    # `class _Directed(GridNetwork)` subclass, because run.py was owned by a parallel workflow at the
    # time. The measurement was real; the shipped product could not produce it. These three fields
    # are what a user sets instead. All default-inert -> every pinned golden holds.
    directed_lanes: bool = False         # each direction of travel gets its OWN carriageway, offset
                                         # sideways off the centreline, so opposing streams no longer
                                         # share one polyline. Lanes per direction = n_lanes.
    drive_side: str = "right"            # "right" | "left" -- which side the carriageway sits on
    custom_network_directed: bool = False  # road_network="custom": build from the document's
                                         # `directed_edges` layer (one-way flags, per-direction lane
                                         # counts, shape polylines) instead of the undirected
                                         # `edges` array, then trim to the largest strongly
                                         # connected component. Without it, everything netimport.py
                                         # and osm.py extract about one-way streets is discarded.
    # --- SUMO-backed mobility (mock_pipeline/sumo_trace.py) ------------------------------------
    # THE POINT: this engine writes the datasets, and its mobility is a hand-rolled IDM over a
    # synthetic graph that has never been validated against a calibrated traffic model -- every
    # real-world GEH number in this repo measured the OTHER (MOSAIC/SUMO) engine. Setting
    # `mobility_source="sumo_replay"` makes SUMO produce the movement while the whole SCMS / attack
    # / detector / MA stack above it stays exactly as it is. All default-inert: `mobility_source`
    # defaults to the built-in "internal" model, nothing below is read on that path, and NOT ONE
    # rng draw is added -- so every pinned golden holds byte for byte.
    mobility_source: str = "internal"    # "internal" (roads.Trip + the IDM in car_follow) or
                                         # "sumo_replay" (a frozen SUMO trajectory artifact)
    sumo_net: str = ""                   # road_network="sumo": the .net.xml the engine reasons
                                         # about. It MUST be the net the trace was frozen on --
                                         # both go through one netimport transform, which is what
                                         # keeps dist_to_road / mapOffRoad / the geometric channel's
                                         # building blockage meaningful.
    sumo_frame_city: str = ""            # geo-referenced net: re-project into osm.py's local frame
                                         # for THIS city, i.e. the exact (lat0, lon0, kx, ky) tuple
                                         # derived from that extract's ROAD ways. Empty = keep the
                                         # net's own metric coordinates (procedural nets).
    sumo_buildings: str = ""             # road_network="sumo": a SUMO polygon additional-file
                                         # (e.g. InTAS's buildings.poly.xml) whose type="building"
                                         # footprints become the geometric channel's NLOSb geometry.
                                         # THE SCENE IS THE MAP PLUS ITS BUILDINGS: without this the
                                         # whole-city path has roads and no footprints and falls back
                                         # to the synthetic canyon density, which is exactly the
                                         # comparison CROSS-ENGINE-RADIO.md says not to make. The
                                         # polygons go through the SAME netimport transform the
                                         # junctions did and are GATED on landing on them
                                         # (netimport._assert_buildings_aligned). Empty = no
                                         # footprints, byte-identical to before.
    sumo_trace: str = ""                 # mobility_source="sumo_replay": the frozen artifact
    sumo_trace_sha256: str = ""          # its sha256. FILLED IN by validate_config and therefore
                                         # recorded in manifest["config"], so a re-frozen trajectory
                                         # (different SUMO seed, different SUMO build) is a
                                         # DETECTABLE INPUT CHANGE -- a refused run with a
                                         # diagnostic -- instead of a silent digest break that looks
                                         # like an engine regression. Set it explicitly to PIN it.
    sumo_cert_slack_s: float = 30.0      # replay: extra pseudonym-certificate lifetime past the
                                         # trace's EXACT despawn time (the internal model can only
                                         # budget an estimate; SUMO already drove the whole trip)
    sumo_offroad_p95_max_m: float = 8.0  # replay coherence GATE: refuse the run if the p95
                                         # distance from a replayed position to the engine's nearest
                                         # road exceeds this. A road-following vehicle sits within
                                         # half a carriageway of the centreline; anything scattered
                                         # means the two frames disagree.
    # --- THE REAL PROTOCOL STACK (mock_pipeline/run.py + codecs/etsi_rules.py + scms_core) -------
    # Five independent opt-ins that together turn "a bare dict on an abstract channel" into "a real
    # PDU on a modelled 802.11p access layer under the real generation and congestion rules". Each
    # one is INERT at its default, draws NO random number on any path, and is composable with the
    # others; a run with all five off is byte-identical to every pinned digest.
    #
    # `message_codec` is the ACTIVATION SURFACE for the `message_codec` plugin slot, spelled the way
    # `radio_model` spells the channel slot: a built-in NAME here, or `plugins.message_codec.ref`
    # for a third party. "" (the default) means NO codec object is constructed at all -- not
    # `native_v1`, which is a real object with a real cost -- so the default path is exactly what it
    # was. With one selected, EVERY CAM, DENM and VAM is encoded through it, the resulting PDU
    # LENGTH is what the channel charges airtime for, and the manifest's `standards_profile` becomes
    # the codec's own `standards_claim()` instead of the engine's hard-coded "no ASN.1 encoding".
    message_codec: str = ""              # "" = off | native_v1 | etsi_cam_en302637_2 | ...
    # TS 103 097 signer alternation, consulted by `MessageCodec.wire_size_bytes` when
    # `security_model="none"`. Worth 126 octets per frame on the measured envelope (93 vs 219), i.e.
    # 168 us of airtime -- which is why it is a parameter and not a constant. With
    # `security_model="ecdsa"` this field is IGNORED: the size then comes from the real
    # `SignedMessage.wire_octets()`, and which arm a frame carries is decided by the once-per-second
    # attachment rule the standard states, not by a config setting.
    message_signer: str = "digest"       # none | digest | certificate
    # EN 302 637-2 V1.4.1 clause 6.1.3 CAM generation rules: a CAM on a 4 m / 4 deg / 0.5 m/s
    # dynamics trigger, floored at T_GenCamMin = 0.1 s, with a T_GenCamMax = 1.0 s heart-beat.
    # OFF: one CAM per vehicle per step, i.e. a flat 1 Hz at the default dt. NOTE THE dt COUPLING:
    # the rules can only fire faster than the heart-beat when the engine steps faster than it, so
    # this knob is a no-op at dt >= 1.0 and its whole effect appears at dt <= 0.5.
    cam_generation_rules: bool = False
    # ETSI TS 102 687 V1.2.1 reactive DCC. Each station maps the CBR its OWN receiver measured on
    # the previous step onto the 5-state table and takes T_off as a floor on its CAM interval. At
    # low density it correctly does NOTHING (relaxed state, T_off 100 ms == T_GenCamMin); it only
    # bites once CBR crosses 0.30. Requires cam_generation_rules (there is no rate to limit
    # otherwise) and is refused without it rather than silently ignored.
    dcc: bool = False
    # Per-packet latency: propagation (d/c) + access (AIFS + backoff/(1-CBR) + PPDU airtime, on the
    # REAL frame length) + a derived stack constant. Replaces `net_delay_max`'s uniform ingest draw,
    # which is a report-upload delay with nothing to do with the channel. Deterministic by
    # construction -- it draws NOTHING, where the uniform draw it replaces consumes one number from
    # the global stream per report.
    net_latency_model: bool = False
    ma_backhaul_s: float = 0.0           # deterministic MA report upload delay added on top of the
                                         # per-packet latency (net_latency_model only)
    # Real cryptography. "ecdsa" provisions every pseudonym through the butterfly expansion
    # (scms_core/provisioning.py), signs every PDU with ECDSA-P256-SHA256 over the 1609.2 double
    # hash, and makes `sig_ok` the RESULT of a verification instead of a boolean the attack switch
    # sets. It also makes the PCA unable to link a device's pseudonyms, which is the property the
    # SCMS exists for and which the label scheme (`derive(f"key:{vid}:{k}")`) does not have.
    security_model: str = "none"         # "none" | "ecdsa"
    # THE PROTOCOL PROFILE -- which stack this run speaks, as ONE declaration.
    #
    # The four knobs above are the ITS-G5 profile's own layers, and leaving them spelled as engine
    # booleans is what made "the network side is modular" untrue: a third party could supply a
    # codec, but not a generation rule, not a congestion controller, not an airtime model and not a
    # latency model. Every one of those now reaches the engine through `api.profile.ProtocolProfile`,
    # and the built-in ITS-G5 stack is resolved through that seam like anybody else's.
    #
    # "" (the default) means: build `etsi_its_g5` IF any of the four layers above is on, and build
    # NOTHING at all otherwise. So the default path constructs no profile object, and a run that
    # says `--cam-rules` gets the built-in profile with its generation layer enabled -- the same
    # object a third party would replace. A third-party stack is declared as
    # `plugins.protocol_profile.ref`, exactly as a channel model or a detector is.
    protocol_profile: str = ""           # "" = derive from the layer flags | etsi_its_g5 | ...
    # THE MISBEHAVIOUR-REPORT FORMAT. A report's format is part of the protocol a deployment speaks,
    # and this slot has been registered and EMPTY since the plugin architecture landed. "" keeps the
    # engine's historic inline row; `ma_report_v1` is that identical row expressed through the seam
    # (and is asserted byte-identical against the pinned golden); `ts103759_shape` re-shapes it into
    # the TS 103 759 `TemplateAsr` three-field form carrying the real encoded evidence octets.
    report_format: str = ""              # "" = off | ma_report_v1 | ts103759_shape | ...

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


def _parse_custom_network(s, *, directed: bool = False) -> tuple[list, list]:
    """Parse + sanity-check the custom_network JSON -> (nodes, edges). Raises ValueError with a
    design-actionable message (this is the feedback loop for AI/user map design). Accepts an
    already-parsed dict too (tool layers sometimes hand the object through).

    `directed=False` (the default, and what `road_network="custom"` did unconditionally until
    2026-08-31) reads ONLY the undirected `edges` array, so a reader that predates the richer schema
    sees exactly the old bidirectional graph. That default is right for backward compatibility and
    wrong as the only option: `osm.network_document` also writes a `directed_edges` layer carrying
    one record per LEGAL DIRECTION with its own one-way flag, per-direction lane count and shape
    polyline, and discarding it threw away everything `netimport.py`'s 670 lines and `osm.py`'s tag
    parsing extract. Measured on the real Ingolstadt extract (`osm_to_network(attrs=True)`): the
    document carries **627 directed records** with lane counts 1..3 and `oneway_share` 0.28, and the
    `CustomNetwork` the engine built from that same document reported `directed=False`, **0** one-way
    edges and **0** lane-specified edges -- the pre-change model, on the only path a user can take.

    `directed=True` (`custom_network_directed`) builds from that layer instead, then trims to the
    largest strongly connected component -- which a bbox-clipped import needs, because clipping
    leaves nodes you can enter and never leave, and `CustomNetwork` refuses a directed map that is
    not strongly connected rather than stranding trips inside it. Same document, same measurement:
    **326 nodes / 610 edges, `directed=True`, 160 one-way, 385 lane-specified.**

    What is still NOT consumed, stated rather than implied: `shape` polylines pass through
    `edges_from_directed` and ARE honoured, but the bare `signal_nodes` index list is still ignored,
    for the reason that made it dangerous in the first place -- `largest_strong_component` REMAPS
    node indices and returns only counts, not the remap, so a consumer that indexed the original
    `signal_nodes` into the trimmed graph would signalise the wrong junctions. The richer
    `signal_programs` layer IS consumed, by `cfg.real_signals`, and it is safe for exactly the
    reason the index list is not: `signals.SignalPlan.from_records` resolves every node index
    against the DOCUMENT's own `nodes` array once, at build time, and then addresses junctions by
    COORDINATE, which survives the trim unchanged. `buildings` is likewise unaffected -- polygons in
    metres, not indices.
    """
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
    if not directed:
        return doc["nodes"], doc["edges"]
    directed_edges = doc.get("directed_edges")
    if not directed_edges:
        raise ValueError(
            'custom_network_directed=true but the document carries no "directed_edges" layer. '
            'Produce one with `python -m scms_sim_ref.mock_pipeline.netimport --city <name> '
            '--out map.json` (or osm.py with --attrs), or set custom_network_directed=false to use '
            'the undirected "edges" array.')
    from .roads import edges_from_directed, largest_strong_component
    nodes, edges, info = largest_strong_component(doc["nodes"], edges_from_directed(directed_edges))
    if len(nodes) < 2 or not edges:
        raise ValueError(
            f"the directed layer has no drivable strongly-connected core: "
            f"{len(doc['nodes'])} nodes and {len(directed_edges)} directed edges reduced to "
            f"{len(nodes)} nodes / {len(edges)} edges. A bbox clip that cuts every return path "
            f"leaves a map on which no round trip exists.")
    return nodes, edges


def _make_ped_signal_fn(net, cfg):
    """`signal_fn(junction_xy, arm_bearing_deg, t) -> may a pedestrian START crossing that arm?`

    A crossing conflicts with the traffic travelling ALONG the arm it crosses, so the walk signal is
    the complement of that arm's vehicle green. Which vehicle model answers that depends on what is
    switched on, and the point of routing it through one factory is that the pedestrians and the
    vehicles at a junction are then provably on ONE clock rather than two that agree by coincidence:

      * `real_signals` -- the arm's own `<tlLogic>` links, pooled most-permissive-first, out of the
        very `SignalPlan` `car_follow` reads. A pedestrian may step off the kerb exactly when no
        conflicting vehicle movement is green. Yellow counts as NOT green here (a vehicle inside the
        dilemma zone is still coming), which is the conservative reading and the one that matches
        the clearance interval a real pedestrian phase has.
      * `traffic_lights` only -- `vru.engine_signal_fn`, which reproduces `_light_green` exactly.
      * both, at a junction with no imported program -- falls through to the toy cycle, which is
        precisely "keep today's behaviour where there is no program".

    Deterministic and RNG-free on every branch. Returns None when no crossing can be signalised, in
    which case `SidewalkNetwork.walk` uses its unsignalised gap-acceptance surrogate throughout.
    """
    plan = getattr(net, "signal_plan", None)
    toy = None
    if cfg.traffic_lights:
        from .vru import engine_signal_fn                # noqa: PLC0415  (opt-in path only)
        toy = engine_signal_fn(net, cfg.light_cycle_s / 2.0)
    if plan is None:
        return toy
    from .signals import GREEN, char_colour              # noqa: PLC0415  (opt-in path only)
    # Per junction: the approaches, with the BEARING OF TRAVEL of each (from -> junction) and its
    # pooled link indices. Built once here rather than per probe: `walk()` solves a kerb wait by
    # stepping the signal forward in 0.5 s probes, so an O(approaches) scan inside that loop would
    # be paid tens of thousands of times per pedestrian.
    idx: dict = {}
    for node_key in plan.programs:
        rows = []
        for frm, links in plan.approaches(node_key):
            rows.append((math.degrees(math.atan2(node_key[1] - frm[1],
                                                 node_key[0] - frm[0])) % 360.0, links))
        if rows:
            idx[node_key] = rows

    def signal_fn(node_xy, arm_bearing_deg: float, t: float) -> bool:
        key = (float(node_xy[0]), float(node_xy[1]))
        rows = idx.get(key)
        if rows is None:
            return True if toy is None else toy(node_xy, arm_bearing_deg, t)
        # every approach whose direction of travel lies along this arm, either way down it
        links: list = []
        for bearing, ls in rows:
            d = _ang_diff(bearing, arm_bearing_deg)
            if d <= 45.0 or d >= 135.0:
                links.extend(ls)
        if not links:
            return True                    # no signalised vehicle movement uses this arm
        return char_colour(plan.programs[key].char_of(t, links)) != GREEN

    return signal_fn


def _custom_network_doc(s):
    """The custom-network document as a dict, or None when there is nothing parseable.

    A read-only accessor for the OPTIONAL layers (`signal_programs` today) that
    `_parse_custom_network` deliberately does not return -- it hands back `(nodes, edges)` and
    nothing else, and every caller that needs a side layer would otherwise re-implement the
    string/dict/JSON tri-state. Never raises: `_parse_custom_network` is the validator, and a
    document malformed enough to fail here fails there with the design-actionable message."""
    if isinstance(s, dict):
        return s
    if not s or not str(s).strip():
        return None
    try:
        doc = json.loads(s)
    except json.JSONDecodeError:
        return None
    return doc if isinstance(doc, dict) else None


def _parse_buildings(s) -> list:
    """Optional `"buildings"` layer of a custom-network document -> [[[x, y], ...], ...] in metres.

    Written by `osm.py --buildings`, which projects `building=*` footprints with the ROAD graph's own
    projection tuple so the two layers are registered. Consumed only by radio_model="geometric" for
    the NLOSb blockage test; absent/empty -> the synthetic urban-canyon fallback. Any other
    road_network (grid/ring/spider/linear) carries no footprints by construction."""
    if isinstance(s, dict):
        doc = s
    else:
        if not s or not str(s).strip():
            return []
        try:
            doc = json.loads(s)
        except json.JSONDecodeError:
            return []
    polys = doc.get("buildings") if isinstance(doc, dict) else None
    if not polys:
        return []
    out = []
    for k, ring in enumerate(polys):
        if not isinstance(ring, (list, tuple)) or len(ring) < 3:
            raise ValueError(f"custom_network buildings[{k}] needs >= 3 vertices (got {ring!r})")
        pts = []
        for p in ring:
            try:
                x, y = float(p[0]), float(p[1])
            except (TypeError, ValueError, IndexError):
                raise ValueError(f"buildings[{k}] vertex must be [x, y] in metres "
                                 f"(got {p!r})") from None
            if not (math.isfinite(x) and math.isfinite(y)):
                raise ValueError(f"buildings[{k}] has a non-finite coordinate")
            pts.append((x, y))
        out.append(pts)
    return out


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
    # The closed enum is gone: `radio_model` is now a BUILT-IN REGISTRY KEY. The message is built
    # from the live registry, so the four sites that used to hand-repeat the three names
    # (this check, `_ENUM_OPTIONS`, the argparse `choices`, the GUI dropdown) cannot drift.
    # A THIRD-PARTY model is selected through `plugins.channel_model`, not through this flag.
    _radio_builtins = _api_registry.builtin_names("channel_model")
    if cfg.radio_model not in _radio_builtins:
        raise ValueError(f"radio_model must be {'|'.join(_radio_builtins)} "
                         f"(got {cfg.radio_model!r}); a third-party channel model is declared via "
                         f"plugins.channel_model, e.g. "
                         f"{{'channel_model': {{'ref': 'pkg.mod:Class'}}}}")
    if cfg.radio_env not in ("urban", "highway"):
        raise ValueError(f"radio_env must be urban|highway (got {cfg.radio_env!r})")
    if not -20.0 <= cfg.radio_tx_power_dbm <= 40.0:
        raise ValueError(f"radio_tx_power_dbm must be in [-20, 40] dBm "
                         f"(got {cfg.radio_tx_power_dbm}); ETSI caps ITS-G5 EIRP at 33 dBm")
    if not -120.0 <= cfg.radio_rx_sensitivity_dbm <= 0.0:
        raise ValueError(f"radio_rx_sensitivity_dbm must be in [-120, 0] dBm "
                         f"(got {cfg.radio_rx_sensitivity_dbm})")
    if cfg.radio_tx_power_dbm <= cfg.radio_rx_sensitivity_dbm:
        raise ValueError(f"radio_tx_power_dbm ({cfg.radio_tx_power_dbm}) must exceed "
                         f"radio_rx_sensitivity_dbm ({cfg.radio_rx_sensitivity_dbm}): no link budget")
    if cfg.radio_nlosb_density_per_km < 0:
        raise ValueError(f"radio_nlosb_density_per_km must be >= 0 "
                         f"(got {cfg.radio_nlosb_density_per_km})")
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
    # --- the real protocol stack (all five default-inert) --------------------------------------- #
    _codec_builtins = _api_registry.builtin_names("message_codec")
    if cfg.message_codec and cfg.message_codec not in _codec_builtins:
        raise ValueError(
            f"message_codec must be '' (off) or one of {'|'.join(_codec_builtins)} "
            f"(got {cfg.message_codec!r}); a third-party wire format is declared via "
            f"plugins.message_codec, not through this field")
    if cfg.message_signer not in _api_codec.SIGNER_FORMS:
        raise ValueError(f"message_signer must be one of {list(_api_codec.SIGNER_FORMS)} "
                         f"(got {cfg.message_signer!r})")
    if cfg.security_model not in ("none", "ecdsa"):
        raise ValueError(f"security_model must be none|ecdsa (got {cfg.security_model!r})")
    if cfg.dcc and not cfg.cam_generation_rules:
        # Refused, not silently ignored: DCC's only actuator in this engine is the CAM service's
        # T_GenCam floor, and with the flat one-CAM-per-step generator there is no rate to limit.
        # Accepting it would put "dcc: true" in the manifest of a run where DCC did nothing.
        raise ValueError("dcc=True requires cam_generation_rules=True: reactive DCC acts by "
                         "raising the CAM service's minimum inter-CAM interval, and with the flat "
                         "one-CAM-per-step generator there is no interval to raise")
    if cfg.cam_generation_rules and cfg.dt > _etsi_rules.T_GEN_CAM_MAX_S:
        raise ValueError(
            f"cam_generation_rules=True needs dt <= {_etsi_rules.T_GEN_CAM_MAX_S} s (got "
            f"{cfg.dt}): EN 302 637-2's heart-beat IS T_GenCamMax, so at a coarser step every step "
            f"is a heart-beat and the dynamics triggers can never be the reason a CAM is sent. Use "
            f"dt <= 0.5 for a run where the rules actually bind")
    if cfg.ma_backhaul_s < 0:
        raise ValueError(f"ma_backhaul_s must be >= 0 (got {cfg.ma_backhaul_s})")
    # --- the protocol-profile and report-format seams --------------------------------------------- #
    _profile_builtins = _api_registry.builtin_names("protocol_profile")
    if cfg.protocol_profile and cfg.protocol_profile not in _profile_builtins:
        raise ValueError(
            f"protocol_profile must be '' (derive from the layer flags) or one of "
            f"{'|'.join(_profile_builtins)} (got {cfg.protocol_profile!r}); a third-party stack is "
            f"declared via plugins.protocol_profile, not through this field")
    _report_builtins = _api_registry.builtin_names("report_format")
    if cfg.report_format and cfg.report_format not in _report_builtins:
        raise ValueError(
            f"report_format must be '' (off) or one of {'|'.join(_report_builtins)} "
            f"(got {cfg.report_format!r}); a third-party format is declared via "
            f"plugins.report_format, not through this field")
    _validate_profile_plugin(cfg)
    _validate_report_format_plugin(cfg)
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
    if cfg.real_signals:
        # Only an IMPORTED city ships <tlLogic> programs. Grid/ring/spider are procedural and a
        # `linear` road has no junctions at all, so the flag could only ever be a silent no-op
        # there -- exactly the class of dead knob this repo refuses to ship.
        if cfg.road_network not in ("sumo", "custom"):
            raise ValueError(
                f"real_signals needs road_network='sumo' (read the <tlLogic> programs straight out "
                f"of the .net.xml) or 'custom' with a document carrying the `signal_programs` layer "
                f"that `python -m scms_sim_ref.mock_pipeline.netimport --signals` writes (got "
                f"road_network={cfg.road_network!r}): a procedural grid/ring/spider map has no real "
                f"programs to import, so the flag would do nothing")
        if not cfg.traffic_flow:
            raise ValueError("real_signals needs traffic_flow=true: a signal is obeyed by the ROUTED "
                             "car-following integrator, and a fixed-fleet vehicle drives a straight "
                             "line past every junction")
        if not cfg.car_following:
            raise ValueError("real_signals needs car_following=true: the stop at a red is applied as "
                             "a virtual stopped leader in the IDM, and there is no other brake")
        if cfg.mobility_source != "internal":
            # SUMO already ran the signal control that produced the frozen trajectory; the IDM
            # integrator is OFF under replay (`cf_active`), so this would be read by nothing.
            raise ValueError(
                f"real_signals is meaningless with mobility_source={cfg.mobility_source!r}: a "
                f"replayed trajectory was already driven through SUMO's own (actuated) signal "
                f"control and the engine's car-following integrator is disabled under replay, so "
                f"no vehicle would ever read the imported programs")
        if cfg.road_network == "custom":
            _doc = _custom_network_doc(cfg.custom_network)
            if not (_doc or {}).get("signal_programs"):
                raise ValueError(
                    "real_signals=true but the custom_network document carries no `signal_programs` "
                    "layer. Re-import the net with `python -m scms_sim_ref.mock_pipeline.netimport "
                    "--net <city>.net.xml --signals --strong --out map.json`, or set "
                    "real_signals=false")
    if cfg.sidewalks:
        # Sidewalks are derived from the ROAD graph, so `linear` (which builds no network object at
        # all) has nothing to offset off. And they exist to carry VRUs: with vru_pct=0 nothing walks.
        if cfg.road_network == "linear":
            raise ValueError("sidewalks needs a routed road network (grid, ring, spider, custom or "
                             "sumo): road_network='linear' has no graph to derive footways from")
        if cfg.vru_pct <= 0:
            raise ValueError("sidewalks needs vru_pct > 0: the pedestrian network exists to carry "
                             "VRUs, and with none spawned nothing would ever walk on it")
    if cfg.sidewalk_width_m <= 0:
        raise ValueError(f"sidewalk_width_m must be > 0 (got {cfg.sidewalk_width_m})")
    if cfg.kerb_clearance_m < 0:
        raise ValueError(f"kerb_clearance_m must be >= 0 (got {cfg.kerb_clearance_m})")
    if cfg.crossing_wait_max_s < 0:
        raise ValueError(f"crossing_wait_max_s must be >= 0 (got {cfg.crossing_wait_max_s})")
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
    _parse_magnitude_scale(cfg.attack_magnitude_scale)  # raises on an unknown type / negative scale
    if cfg.trip_speed_min <= 0 or cfg.trip_speed_max < cfg.trip_speed_min:
        raise ValueError(f"need 0 < trip_speed_min <= trip_speed_max "
                         f"(got {cfg.trip_speed_min}, {cfg.trip_speed_max})")
    if cfg.vru_pct >= 1.0:                              # VRUs are a FRACTION of actors; ratio vru/(1-vru)
        raise ValueError(f"vru_pct must be < 1.0 (it is a fraction of actors; got {cfg.vru_pct})")
    if cfg.vru_pct > 0 and cfg.vru_speed_mps <= 0:      # VRUs must actually move (walking/cycling)
        raise ValueError(f"vru_speed_mps must be > 0 when vru_pct > 0 (got {cfg.vru_speed_mps})")
    if cfg.vru_pct > 0 and cfg.vru_speed_mps >= cfg.vru_max_plausible_speed_mps:
        # genuine VRUs declare station_type=vru; the vruImpersonation SPEED arm fires at
        # claimed speed >= vru_max_plausible_speed_mps, so a VRU travelling at/above that bound would
        # flag ITSELF every step -> false revocation of benign actors. Keep VRU speed below the bound.
        raise ValueError(f"vru_speed_mps ({cfg.vru_speed_mps}) must be < vru_max_plausible_speed_mps "
                         f"({cfg.vru_max_plausible_speed_mps}) or genuine VRUs self-flag as impersonators")
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
    if cfg.denm_benign_max_speed_mps >= cfg.denm_implausible_speed_mps:
        # the benign DENM trigger must sit BELOW the implausibility line, else a benign (slow) sender
        # crosses its own detector, and raising benign_max above attacker cruising speed hides FakeHazard.
        raise ValueError(f"denm_benign_max_speed_mps ({cfg.denm_benign_max_speed_mps}) must be < "
                         f"denm_implausible_speed_mps ({cfg.denm_implausible_speed_mps})")
    if cfg.vru_max_plausible_speed_mps <= 0:
        raise ValueError(f"vru_max_plausible_speed_mps must be > 0 (got {cfg.vru_max_plausible_speed_mps})")
    # detector operating point (strictness of the motion + Sybil detectors). Upper bounds are generous
    # -- they only reject values so extreme the run is degenerate (nothing ever fires / everything does).
    if not (0 < cfg.detector_z_threshold <= 50):
        raise ValueError(f"detector_z_threshold must be in (0, 50] (got {cfg.detector_z_threshold})")
    if not (1 <= cfg.detector_min_consec <= 100):
        raise ValueError(f"detector_min_consec must be in [1, 100] (got {cfg.detector_min_consec})")
    if cfg.sybil_min_certs < 2:
        raise ValueError(f"sybil_min_certs must be >= 2 (got {cfg.sybil_min_certs})")
    if cfg.sybil_cell_m <= 0:
        raise ValueError(f"sybil_cell_m must be > 0 (got {cfg.sybil_cell_m})")
    # per-vehicle GNSS quality spread. Cap the floor: a huge floor makes EVERY benign vehicle read as
    # an attacker (mass false positives) -- reject the degenerate regime rather than emit a misleading set.
    if not (0 <= cfg.gps_quality_floor <= 20):
        raise ValueError(f"gps_quality_floor must be in [0, 20] (got {cfg.gps_quality_floor})")
    if cfg.gps_quality_lambda <= 0:
        raise ValueError(f"gps_quality_lambda must be > 0 (got {cfg.gps_quality_lambda})")
    if cfg.road_network not in ("linear", "grid", "ring", "spider", "custom", "sumo"):
        raise ValueError(f"road_network must be linear|grid|ring|spider|custom|sumo "
                         f"(got {cfg.road_network!r})")
    if cfg.road_network == "custom":
        from .roads import CustomNetwork
        CustomNetwork(*_parse_custom_network(               # full design validation
            cfg.custom_network, directed=cfg.custom_network_directed))
    elif cfg.custom_network_directed and cfg.road_network != "sumo":
        raise ValueError(f"custom_network_directed needs road_network='custom' or 'sumo' (got "
                         f"{cfg.road_network!r}): it selects which layer of the custom-network "
                         f"DOCUMENT to build from, and no other topology has one")
    _validate_mobility(cfg)
    from .roads import DRIVE_SIDES
    if cfg.drive_side not in DRIVE_SIDES:
        raise ValueError(f"drive_side must be one of {sorted(DRIVE_SIDES)} (got {cfg.drive_side!r})")
    if cfg.directed_lanes:
        # `linear` builds no network object at all (vehicles drive straight lines), so there is no
        # graph to give carriageways to. Refusing here beats a flag that silently does nothing --
        # the whole class of defect this field exists to close.
        if cfg.road_network == "linear":
            raise ValueError("directed_lanes needs a routed road network (grid, ring, spider or "
                             "custom): road_network='linear' has no graph to offset off")
        if not cfg.traffic_flow:
            raise ValueError("directed_lanes needs traffic_flow=true: carriageway offsets apply to "
                             "ROUTED trips, and a fixed-fleet vehicle drives a straight line")
    if cfg.road_network in ("spider", "custom", "sumo") and not cfg.traffic_flow:
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
    # Plugin declarations: shape-check here, then let each plugin's own validator speak. This runs
    # AFTER the engine's own checks and BEFORE the _PROB_FIELDS clamp, exactly as the design places
    # it, so a plugin's message is the FIRST thing a user sees about its own knobs -- and no `if` is
    # added to this 200-line block per plugin knob.
    _validate_plugins(cfg)
    for name in _PROB_FIELDS:                       # clamp fractions rather than produce nonsense
        setattr(cfg, name, min(1.0, max(0.0, float(getattr(cfg, name)))))
    return cfg


def _validate_mobility(cfg) -> None:
    """The SUMO-backed mobility surface, validated as one coherent unit rather than knob by knob.

    Everything here is inert unless `mobility_source` is moved off `"internal"` or `road_network`
    is set to `"sumo"`, and neither costs an rng draw, so the default path is byte-identical.

    THE HASH IS FILLED IN HERE, before `run_pipeline` takes its config snapshot -- which is the only
    place it can be, because the snapshot is what `manifest["config"]` records and
    `_assert_config_unmoved` forbids any later write. Leaving `sumo_trace_sha256` empty means "adopt
    whatever is on disk and record it"; setting it means "PIN it", and a mismatch is refused. That
    is what turns SUMO's nondeterminism into a detectable INPUT change: a trajectory re-frozen under
    a different SUMO seed or a different SUMO build stops the run with a diagnostic instead of
    silently producing a different `data_digest` that reads as an engine regression."""
    _mob = _api_registry.builtin_names("mobility")
    if cfg.mobility_source not in _mob:
        raise ValueError(f"mobility_source must be {'|'.join(_mob)} (got {cfg.mobility_source!r})")
    if cfg.road_network == "sumo":
        if not cfg.sumo_net:
            raise ValueError('road_network="sumo" needs sumo_net=<path to a SUMO .net.xml>: the '
                             'engine imports that net (netimport.py) as the graph it reasons about')
        if not os.path.exists(cfg.sumo_net):
            raise ValueError(f"sumo_net not found: {cfg.sumo_net!r}")
    elif cfg.sumo_net:
        raise ValueError(f"sumo_net needs road_network='sumo' (got {cfg.road_network!r}); it names "
                         f"the network the engine builds from, not an extra layer on another one")
    if cfg.sumo_frame_city and cfg.road_network != "sumo":
        raise ValueError("sumo_frame_city needs road_network='sumo': it selects the projection "
                         "frame the .net.xml is re-projected into")
    if cfg.sumo_buildings:
        if cfg.road_network != "sumo":
            raise ValueError(f"sumo_buildings needs road_network='sumo' (got "
                             f"{cfg.road_network!r}): the footprints are projected with THAT net's "
                             f"transform and gated against ITS junctions. A custom-network map "
                             f"carries its own polygons in the document's 'buildings' layer.")
        if not os.path.exists(cfg.sumo_buildings):
            raise ValueError(f"sumo_buildings not found: {cfg.sumo_buildings!r}")
    if cfg.sumo_cert_slack_s < 0:
        raise ValueError(f"sumo_cert_slack_s must be >= 0 (got {cfg.sumo_cert_slack_s})")
    if cfg.sumo_offroad_p95_max_m <= 0:
        raise ValueError(f"sumo_offroad_p95_max_m must be > 0 (got {cfg.sumo_offroad_p95_max_m})")
    if cfg.mobility_source != "sumo_replay":
        if cfg.sumo_trace:
            raise ValueError("sumo_trace needs mobility_source='sumo_replay'; a frozen trajectory "
                             "that nothing replays is a config that lies about the run")
        if cfg.sumo_trace_sha256:
            raise ValueError("sumo_trace_sha256 needs mobility_source='sumo_replay'")
        return
    # ---- mobility_source == "sumo_replay" ---------------------------------------------------- #
    if not cfg.traffic_flow:
        raise ValueError("mobility_source='sumo_replay' needs traffic_flow=true: the frozen "
                         "trajectory IS an arrival process (vehicles depart and arrive over time), "
                         "and a fixed fleet has nowhere to put it")
    if cfg.road_network != "sumo":
        raise ValueError(
            f"mobility_source='sumo_replay' needs road_network='sumo' (got "
            f"{cfg.road_network!r}). The replayed vehicles must move on the SAME network the engine "
            f"reasons about -- both are derived from one .net.xml through one netimport transform. "
            f"Replaying onto any other graph makes dist_to_road, mapOffRoad and the geometric "
            f"channel's building blockage measurements of two different cities.")
    if cfg.directed_lanes:
        raise ValueError("mobility_source='sumo_replay' is incompatible with directed_lanes: SUMO "
                         "has ALREADY placed each vehicle on its own carriageway and lane, and "
                         "offsetting the engine's centrelines again moves the roads off the "
                         "replayed traffic (the coherence gate would then fail on your own map)")
    if not cfg.sumo_trace:
        raise ValueError("mobility_source='sumo_replay' needs sumo_trace=<frozen artifact>; "
                         "produce one with `python -m scms_sim_ref.mock_pipeline.sumo_trace "
                         "--net map.net.xml --routes map.rou.xml --steps 300 --run-seed 42 "
                         "--out map.trace`")
    if not os.path.exists(cfg.sumo_trace):
        raise ValueError(f"sumo_trace not found: {cfg.sumo_trace!r}")
    from .sumo_trace import file_sha256
    have = file_sha256(cfg.sumo_trace)
    if cfg.sumo_trace_sha256 and cfg.sumo_trace_sha256 != have:
        raise ValueError(
            f"the frozen trajectory at {cfg.sumo_trace!r} does not match the sha256 this config "
            f"pins.\n  pinned:  {cfg.sumo_trace_sha256}\n  on disk: {have}\n"
            f"The INPUT changed -- a different SUMO seed, a different SUMO build, or a different "
            f"network/demand. That is a new dataset, not a reproduction of the pinned one: re-pin "
            f"sumo_trace_sha256 deliberately, or restore the artifact this config was written for.")
    cfg.sumo_trace_sha256 = have


def _validate_plugins(cfg) -> None:
    # Accept a JSON STRING as well as an object, the same way `custom_network` and `events` do:
    # a CLI flag, a GUI text field and a copilot tool call all deliver a string, and the alternative
    # is a class of "invalid config" errors that say nothing useful. Normalised to a dict here, so
    # what lands in manifest["config"] is always the object form.
    if isinstance(cfg.plugins, str):
        s = cfg.plugins.strip()
        if not s:
            cfg.plugins = {}
        else:
            try:
                cfg.plugins = json.loads(s)
            except json.JSONDecodeError as e:
                raise ValueError(f"plugins is not valid JSON: {e}") from None
    if cfg.plugins in (None, {}):
        cfg.plugins = {}
        return                                      # DEFAULT EMPTY -> zero behaviour change
    if not isinstance(cfg.plugins, dict):
        raise ValueError(f"plugins must be an object (got {type(cfg.plugins).__name__})")
    unknown = sorted(set(cfg.plugins) - set(_api_registry.SLOTS))
    if unknown:
        raise ValueError(f"plugins: unknown slot(s) {unknown}; known: {list(_api_registry.SLOTS)}")
    unsupported = sorted(s for s in cfg.plugins if s not in _CONSUMED_SLOTS and cfg.plugins[s])
    if unsupported:
        # Say what is not there rather than accept it and silently ignore it -- an ignored plugin
        # section is exactly the "replays as a different run with exit code 0" failure D4 closes.
        raise ValueError(f"plugins: slot(s) {unsupported} are declared but not yet consumed by this "
                         f"engine (consumed: {sorted(_CONSUMED_SLOTS)}); remove them or upgrade")
    _validate_detector_plugins(cfg)
    _validate_codec_plugin(cfg)
    ref, params = _channel_selection(cfg)
    _channel_conformance(cfg)          # reject a bad `conformance` mode HERE, not at step 0
    _slot_source_gate(cfg, "channel_model")        # ...and a bad `source_gate` for the same reason
    cls, how, _iv, _shape = _api_registry.resolve("channel_model", ref)
    spec = _plugin_config_fields(cls)
    for k, v in sorted(params.items()):
        fs = spec.get(k)
        if fs is None and spec:
            raise ValueError(f"plugins.channel_model.params: {cls.__name__} declares no field {k!r}"
                             f"; known: {sorted(spec)}")
        if fs is not None:
            fs.validate(f"plugins.channel_model.params.{k}", v)
    # A plugin's OWN validator, raising its OWN message. Declared as a classmethod/staticmethod so
    # it is reachable before construction; anything deeper belongs in __init__ (conformance C10
    # requires invalid params to raise at CONSTRUCTION, never at step k > 0).
    own = inspect.getattr_static(cls, "validate_params", None)
    if isinstance(own, (classmethod, staticmethod)):
        getattr(cls, "validate_params")(params)


#: Plugin slots this engine actually CONSUMES. A declared-but-unconsumed slot is refused rather than
#: ignored -- an ignored plugin section replays as a different run with exit code 0, which is the
#: exact failure the lock exists to prevent.
_CONSUMED_SLOTS = frozenset({"channel_model", "check", "fusion", "message_codec",
                             "protocol_profile", "report_format"})


def _validate_codec_plugin(cfg) -> None:
    """Shape + params validation for the `message_codec` slot, at CONFIG time.

    Resolves and validates but does NOT construct: `--check-config`, the GUI's validation pass and
    the copilot must be able to reject `plugins.message_codec.params.lat0 = "north"` without
    building a codec, and -- for the ETSI profiles -- without needing `asn1tools` installed at all,
    since construction is the only thing that requires it.
    """
    ref, params = _codec_selection(cfg)
    if not ref:
        return
    _codec_conformance(cfg)
    _slot_source_gate(cfg, "message_codec")
    cls, _how, _iv, _shape = _api_registry.resolve("message_codec", ref)
    spec = _plugin_config_fields(cls)
    for k, v in sorted(params.items()):
        fs = spec.get(k)
        if fs is None and spec:
            raise ValueError(f"plugins.message_codec.params: {cls.__name__} declares no field "
                             f"{k!r}; known: {sorted(spec)}")
        if fs is not None:
            fs.validate(f"plugins.message_codec.params.{k}", v)
    own = inspect.getattr_static(cls, "validate_params", None)
    if isinstance(own, (classmethod, staticmethod)):
        getattr(cls, "validate_params")(params)


def _validate_slot_plugin(cfg, slot: str, keys: frozenset, enum_field: str) -> None:
    """Shape + params validation for a single-object slot, at CONFIG time.

    Resolves and validates but does NOT construct, for the same reason `_validate_codec_plugin`
    does: `--check-config`, the GUI's validation pass and the copilot must be able to reject
    `plugins.protocol_profile.params.congestion = "yes"` without building a stack, and -- for the
    ETSI profiles -- without needing `asn1tools` installed at all, since construction is the only
    thing that requires it.
    """
    ref, params = _section(cfg, slot, keys, enum_field)
    if not ref:
        return
    _slot_conformance(cfg, slot)
    # `source_gate` is validated at CONFIG time for the same reason `conformance` is: a typo in a
    # key that decides whether the runtime plugin guard is armed must be an error the GUI and
    # `--check-config` refuse, not a silent default.
    _slot_source_gate(cfg, slot)
    cls, _how, _iv, _shape = _api_registry.resolve(slot, ref)
    spec = _plugin_config_fields(cls)
    for k, v in sorted(params.items()):
        fs = spec.get(k)
        if fs is None and spec:
            raise ValueError(f"plugins.{slot}.params: {cls.__name__} declares no field {k!r}; "
                             f"known: {sorted(spec)}")
        if fs is not None:
            fs.validate(f"plugins.{slot}.params.{k}", v)
    own = inspect.getattr_static(cls, "validate_params", None)
    if isinstance(own, (classmethod, staticmethod)):
        getattr(cls, "validate_params")(params)


def _validate_profile_plugin(cfg) -> None:
    _validate_slot_plugin(cfg, "protocol_profile", _PROFILE_SECTION_KEYS, "protocol_profile")


def _validate_report_format_plugin(cfg) -> None:
    _validate_slot_plugin(cfg, "report_format", _REPORT_SECTION_KEYS, "report_format")


def _validate_detector_plugins(cfg) -> None:
    """Shape + params validation for the `check` / `fusion` slots, at CONFIG time.

    Fail-fast with the plugin's own message, before step 0 and before any output directory exists.
    The suite is not CONSTRUCTED here -- construction happens once inside `run_pipeline`, because a
    plugin instance is a per-run object and the GUI/copilot validate configs they never run.
    """
    checks = [(r, p, g, iso) for r, p, _mode, g, iso in _checks_selection(cfg, station_types=True,
                                                                         denm=True)]
    fref, fparams, fgate = _fusion_selection(cfg)
    columns: dict = {}
    for slot, entries in (("check", checks), ("fusion", [(fref, fparams, fgate, False)])):
        for order, (ref, declared, sgate, isolated) in enumerate(entries):
            if isolated:
                # AN ISOLATED CHECK IS NOT RESOLVED HERE, and that is the whole point: resolving it
                # means IMPORTING it, and module-level code is an earlier hook than `__init__`. Its
                # params, its column and its source screening are validated against the `FieldSpec`s
                # the WORKER reports, in `build_checks`, before step 0 and before any output
                # directory exists -- the same fail-fast, one phase later. The cost is stated in
                # docs/realism/DETECTOR-PLUGIN.md: `--dump-config-schema` and the GUI's advanced
                # panel cannot show an isolated plugin's knobs, because showing them means running
                # the plugin's code in the process that is asking.
                continue
            cls, how, _iv, _shape = _api_registry.resolve(slot, ref)
            _assert_not_hijacked(slot, ref, cls)
            builtin = how == "builtin" and _api_registry.is_builtin(slot, cls)
            if not builtin:
                # The source gate runs at CONFIG time too, so `--check-config`, the GUI's validation
                # pass and the copilot refuse a frame-walking plugin without building a run.
                _srcgate.gate(slot, ref, cls, mode=sgate)
            if builtin:
                _builtin_params(cls, cfg, declared, slot, ref)
            else:
                _plugin_params(cls, declared, slot, ref)
            if slot == "check":
                # The duplicate-column guard, at CONFIG time as well as at load: `--check-config`,
                # the GUI and the copilot must refuse a collision without building a suite. Both
                # feature gates are open here (`_checks_selection` above), so this grades the widest
                # vector the config could produce.
                _claim_column(columns, ref, order,
                              _api_registry.plugin_id_of(cls, _fallback_pid(ref)),
                              str(getattr(cls, "reason_code", "")), builtin)
            own = inspect.getattr_static(cls, "validate_params", None)
            if isinstance(own, (classmethod, staticmethod)):
                getattr(cls, "validate_params")(declared)


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
                      "traffic_flow", "duration", "max_total", "nominal_speed", "state_prune", "vru_",
                      "crossing_wait", "mobility_source", "sumo_trace", "sumo_cert_slack",
                      "sumo_offroad")),
        ("Network", ("road_network", "grid", "custom_network", "traffic_lights", "light_cycle",
                     "real_signals", "sidewalks", "sidewalk_width", "kerb_clearance",
                     "arterial", "local_speed", "directed_lanes", "drive_side",
                     "sumo_net", "sumo_frame_city", "sumo_buildings")),
        ("Scenario events", ("events",)),
        ("Plugins", ("plugins",)),
        ("Protocol", ("message_codec", "message_signer", "cam_generation_rules", "dcc",
                      "net_latency_model", "ma_backhaul_s", "security_model",
                      "protocol_profile", "report_format")),
        ("Messages", ("denm",)),
        ("GNSS/sensor", ("gps_", "faulty", "weather")),
        ("Radio", ("radio", "packet", "nlos", "chan", "freq", "art_max", "stale", "pathloss",
                   "shadowing", "rx_sensitivity")),
        ("Detection/MA", ("consistency", "heading", "detector", "report", "revoke", "reputation",
                          "ma_defense", "max_accel", "offroad", "rotate", "beacon", "net_delay",
                          "crl_", "sybil_min_certs", "sybil_cell_m")),
        ("Run", ("seed", "n_vehicles", "n_steps", "dt", "jmax", "out_dir", "verbose",
                 "live_interval", "emit_sample", "emit_mobility_oracle")),
    ]
    for label, prefixes in g:
        if any(name == p or name.startswith(p) for p in prefixes):
            return label
    return "Other"


# Enumerated fields -> their valid options (sourced from the live constants so they never drift).
_ENUM_OPTIONS = {
    "weather": list(WEATHER_MULT),
    # sourced from the BUILT-IN REGISTRY, in registration (display) order -- one source of truth for
    # the validate_config check, this list, the argparse choices and the GUI dropdown
    "radio_model": list(_api_registry.builtin_names("channel_model")),
    "radio_env": ["urban", "highway"],
    # same rule as radio_model: sourced from the BUILT-IN REGISTRY so validate_config's message,
    # this list, the argparse choices and the GUI dropdown are one source of truth
    "mobility_source": list(_api_registry.builtin_names("mobility")),
    # Same rule again, one slot over: the codec options come from the BUILT-IN REGISTRY (which the
    # lazy registrar populates on first look), with "" prepended for "no codec at all". "" is not a
    # registry entry and never can be -- it is the ABSENCE of a codec object, which is what makes
    # the default path free rather than merely cheap.
    "message_codec": ["", *_api_registry.builtin_names("message_codec")],
    "message_signer": list(_api_codec.SIGNER_FORMS),
    "security_model": ["none", "ecdsa"],
    # Same rule, two slots over. "" on `protocol_profile` means "derive the built-in stack from the
    # layer flags"; "" on `report_format` means the engine's historic inline row. Neither is a
    # registry entry, and neither can be: both are the ABSENCE of an object.
    "protocol_profile": ["", *_api_registry.builtin_names("protocol_profile")],
    "report_format": ["", *_api_registry.builtin_names("report_format")],
    "road_network": ["linear", "grid", "ring", "spider", "custom", "sumo"],
    "drive_side": ["right", "left"],
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
    "emit_mobility_oracle": dict(h="Also write an ORACLE mobility record that is NOT gated on "
                                   "broadcasting (every vehicle, every step, including after "
                                   "revocation) — the unbiased source for the traffic panel"),
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
                           "a fully custom node/edge map (see custom_network), or a SUMO .net.xml "
                           "imported through netimport (see sumo_net)"),
    # SUMO-backed mobility
    "mobility_source": dict(h="Where vehicle movement comes from: 'internal' (the engine's own "
                              "routed IDM car-following — the default, and what every pinned "
                              "golden was measured on) or 'sumo_replay' (a frozen SUMO trajectory "
                              "artifact, so the movement is SUMO's while the SCMS/attack/detector/"
                              "MA stack above it is unchanged). Needs sumo_trace + road_network=sumo"),
    "sumo_net": dict(h="road_network=sumo: the SUMO .net.xml the engine imports as its road graph. "
                       "With sumo_replay it MUST be the net the trace was frozen on — one import, "
                       "one transform, so the vehicles and the roads share a frame"),
    "sumo_frame_city": dict(h="Geo-referenced .net.xml: re-project it into osm.py's local frame for "
                              "this city (the exact lat0/lon0/kx/ky derived from that extract's "
                              "road ways), so the net registers with osm.py roads and building "
                              "footprints. Empty = keep the net's own metric coordinates"),
    "sumo_buildings": dict(h="road_network=sumo: a SUMO polygon additional-file (InTAS ships "
                             "buildings.poly.xml) whose type=\"building\" footprints become the "
                             "geometric channel's NLOSb geometry. Projected with the SAME transform "
                             "as the net and gated on landing on its junctions. Empty = the "
                             "whole-city map runs with no footprints and falls back to the "
                             "synthetic canyon density"),
    "sumo_trace": dict(h="mobility_source=sumo_replay: the frozen trajectory artifact "
                         "(python -m scms_sim_ref.mock_pipeline.sumo_trace --net ... --routes ... "
                         "--steps N --run-seed S --out map.trace)"),
    "sumo_trace_sha256": dict(h="sha256 of the frozen trajectory. Left empty it is FILLED IN from "
                                "the file and recorded in manifest[config]; set explicitly it PINS "
                                "the input, and a re-frozen trajectory is refused rather than "
                                "silently producing a different data_digest"),
    "sumo_cert_slack_s": dict(h="Replay: extra pseudonym-certificate lifetime past the trace's exact "
                                "despawn time (SUMO already drove the trip, so the budget is exact "
                                "rather than an estimate)", lo=0, hi=600, st=5, u="s"),
    "sumo_offroad_p95_max_m": dict(h="Replay coherence gate: refuse the run if the p95 distance from "
                                     "a replayed position to the engine's nearest road exceeds this "
                                     "(a misregistered frame scatters it)", lo=0.5, hi=200, st=0.5,
                                   u="m"),
    "custom_network": dict(h='Custom map JSON {"nodes":[[x,y]...metres],"edges":[[a,b]...]} — any '
                             'connected road graph (AI/user-designed); used when road_network=custom'),
    "custom_network_directed": dict(h="Build the custom map from its directed_edges layer (one-way "
                                      "flags, per-direction lanes, shape polylines) instead of the "
                                      "undirected edges array, trimmed to the largest strongly "
                                      "connected component. Needs road_network=custom"),
    "directed_lanes": dict(h="Give each direction of travel its own carriageway, offset sideways "
                             "off the road centreline (lanes per direction = n_lanes). Removes "
                             "head-on overlaps; needs a routed network and traffic_flow"),
    "drive_side": dict(h="Which side of the centreline a direction's carriageway sits on"),
    "events": dict(h='Scenario timeline JSON: [{"t":s,"type":"demand|weather|close_edge|attack_wave",'
                     '...}] — mid-run demand surges, weather fronts, road closures, attack waves'),
    "grid_w": dict(h="Grid columns (grid) / number of intersections (ring)", lo=2, hi=40),
    "grid_h": dict(h="Grid rows (0 = square, equal to grid_w)", lo=0, hi=40),
    "grid_block_m": dict(h="Spacing between adjacent intersections", lo=20, hi=500, u="m"),
    "grid_dropout": dict(h="Fraction of grid roads removed (irregular grid; stays connected)", lo=0, hi=1, st=0.05),
    "traffic_lights": dict(h="Signalized intersections (stops + queues)"),
    "light_cycle_s": dict(h="Full signal cycle; half green per axis", lo=2, hi=120, u="s"),
    "real_signals": dict(h="Drive each junction from the REAL <tlLogic> program the imported "
                           ".net.xml ships — per-movement colour, yellow, and permissive green "
                           "distinct from protected — instead of the toy 2-phase cycle. Needs an "
                           "imported map (road_network sumo/custom); a junction with no program "
                           "keeps today's behaviour"),
    "sidewalks": dict(h="VRUs walk derived sidewalks and cross at crossings instead of random-"
                        "walking off-road; needs vru_pct > 0 and a routed network"),
    "sidewalk_width_m": dict(h="Footway width (sidewalks)", lo=0.5, hi=8, st=0.1, u="m"),
    "kerb_clearance_m": dict(h="Gap from the kerb line to the inner edge of the footway",
                             lo=0, hi=5, st=0.1, u="m"),
    "crossing_wait_max_s": dict(h="Longest kerb wait at an UNSIGNALISED crossing (gap-acceptance "
                                  "surrogate)", lo=0, hi=60, st=0.5, u="s"),
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
    "attack_magnitude_scale": dict(h="Per-type falsification-magnitude multiplier (on top of "
                                     "attack_intensity), e.g. RandomPos:2.0,ConstPosOffset:0.5 "
                                     "(blank = every type 1.0)"),
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
    # Protocol (the real stack)
    "message_codec": dict(h="Wire format for every CAM/DENM/VAM: '' (off, no codec constructed) | "
                            "native_v1 (the engine's own representation, and the legacy 300 B "
                            "airtime assumption) | etsi_cam_en302637_2 / etsi_denm_en302637_3 / "
                            "etsi_vam_ts103300_3 (real UPER against the vendored ETSI ASN.1 "
                            "modules). The encoded PDU LENGTH drives airtime, CBR and collision "
                            "loss. Third-party profiles go through plugins.message_codec"),
    "message_signer": dict(h="TS 103 097 signer alternation used for the frame's wire size when "
                             "security_model=none: none | digest (HashedId8, +93 B) | certificate "
                             "(+219 B). Ignored under security_model=ecdsa, where the real "
                             "once-per-second attachment rule decides"),
    "cam_generation_rules": dict(h="ETSI EN 302 637-2 CAM triggering: emit on a 4 m / 4 deg / "
                                   "0.5 m/s dynamics change, floored at T_GenCamMin 0.1 s with a "
                                   "T_GenCamMax 1.0 s heart-beat (needs dt <= 1.0; only binds "
                                   "below 0.5). Off = one CAM per vehicle per step"),
    "dcc": dict(h="ETSI TS 102 687 reactive DCC: the measured CBR selects a state and its T_off "
                  "becomes a floor on the CAM interval. Correctly inert below CBR 0.30. Requires "
                  "cam_generation_rules"),
    "net_latency_model": dict(h="Per-packet latency (propagation + AIFS/backoff/airtime on the "
                                "REAL frame length + a derived stack constant) in place of the "
                                "uniform report-ingest draw. Deterministic: it draws nothing"),
    "ma_backhaul_s": dict(h="Deterministic MA report-upload delay added on top of the per-packet "
                            "latency (net_latency_model only)", lo=0, u="s"),
    "security_model": dict(h="none = sig_ok is a boolean the attack switch sets (today). ecdsa = "
                             "butterfly-provisioned pseudonyms, real ECDSA-P256 signing over the "
                             "1609.2 double hash, and sig_ok as the RESULT of a verification"),
    "protocol_profile": dict(h="WHICH PROTOCOL this run speaks, as one declaration: '' = build the "
                               "built-in etsi_its_g5 stack from the four layer flags above (and "
                               "nothing at all when they are off) | etsi_its_g5 explicitly. A "
                               "profile owns the codec, the generation rules, the congestion "
                               "controller, the wire-size accounting and the airtime/latency "
                               "model. A third-party stack goes through plugins.protocol_profile"),
    "report_format": dict(h="Misbehaviour-report format: '' (the engine's historic inline row) | "
                            "ma_report_v1 (that identical row through the report_format seam) | "
                            "ts103759_shape (the TS 103 759 TemplateAsr shape carrying the REAL "
                            "encoded evidence octets; changes data_digest by construction). "
                            "Third-party formats go through plugins.report_format"),
    # Radio
    "radio_range_m": dict(h="Vehicle reception range", lo=10, hi=2000, u="m"),
    "radio_model": dict(h="Reachability model: disc (hard range) | logdistance (soft path-loss + "
                          "shadowing) | geometric (3GPP TR 37.885 LOS/NLOSv/NLOSb + AR(1) shadowing "
                          "+ per-packet Nakagami fading)"),
    "radio_env": dict(h="geometric only: TR 37.885 LOS formula family (urban 38.77+16.7log10 d vs "
                        "highway 32.4+20log10 d); NLOS always uses the urban formula"),
    "radio_tx_power_dbm": dict(h="geometric only: transmit EIRP. Deployed ITS-G5 OBUs run 20-23 dBm; "
                                 "ETSI caps EIRP at 33; below ~20.8 dBm no channel model can reach "
                                 "500 m on highway LOS", lo=-20, hi=40, st=0.5, u="dBm"),
    "radio_rx_sensitivity_dbm": dict(h="geometric only: receiver decode floor (vendored VeReMi-NextGen "
                                       "6 Mb/s 802.11p profile: -81 dBm)", lo=-120, hi=0, st=1, u="dBm"),
    "radio_nlosb_density_per_km": dict(h="geometric only, SYNTHETIC MAPS ONLY: urban-canyon building "
                                         "blocker density, P(LOS) = exp(-lambda*d). Ignored when the "
                                         "map carries real building footprints", lo=0, hi=50, st=0.5,
                                       u="/km"),
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
    # Plugins
    "plugins": dict(h="Component plugins to ACTIVATE, as {slot: {ref, params}}. `ref` resolves "
                      "through the built-in registry, an installed entry point, then a dotted path "
                      "'package.module:Class'. Empty (the default) = built-ins only, byte-identical. "
                      "Example: {\"channel_model\": {\"ref\": \"geometric\"}}. A check/fusion entry "
                      "also takes conformance:'off'|'required', source_gate:'on'|'off' and "
                      "isolated:true|false. source_gate scans the plugin's own source for frame "
                      "walking, gc reflection and engine-internal imports and refuses them by "
                      "default (a guard rail, NOT a sandbox -- an in-process plugin is TRUSTED "
                      "code). isolated:true runs a CHECK in its own interpreter, which is the real "
                      "boundary: the engine sends it the Observation and nothing else, so the "
                      "run's ground truth is not in its address space at all. Same seed, same "
                      "scores, same digest; ~40 us per delivered message. Use it for a detector "
                      "you have not reviewed. See docs/realism/DETECTOR-PLUGIN.md section 2"),
}


def config_schema(cfg: "Optional[PipelineConfig]" = None) -> dict:
    """Machine-readable schema of every PipelineConfig field: for each, {type, default, group, widget,
    help, options, min, max, step, unit}. widget in {bool, select, int, float, text}. Lets UIs/tools
    render a fully self-describing form (dropdowns for enums, ranges/units for numbers).

    With a `cfg` whose `plugins` block declares components, the DECLARED PLUGINS' OWN knobs are
    merged in under `plugins.<slot>.<field>`, sorted, from each plugin's `config_fields()`
    (:class:`~scms_sim_ref.api.fields.FieldSpec`). That is ns-3's `AddAttribute` idea mapped onto
    machinery this repo already had: one declaration in the plugin, and the GUI advanced panel, the
    copilot cheat-sheet and `--dump-config-schema` get its knobs for free -- collapsing the
    4-to-6 hand-maintained declarations per knob down to one, for plugin knobs.

    Called with NO argument (every existing caller) the output is exactly the dataclass's fields and
    nothing else, which is what keeps the GUI/copilot contract and the phase-1 schema gate intact.
    """
    out = {}
    for f in dataclasses.fields(PipelineConfig):
        default = f.default
        if default is dataclasses.MISSING and f.default_factory is not dataclasses.MISSING:
            default = f.default_factory()           # e.g. plugins -> {} rather than a bare null
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
    if cfg is not None and getattr(cfg, "plugins", None):
        out.update(_plugin_schema(cfg))
    return out


def _plugin_schema(cfg) -> dict:
    """`plugins.<slot>.<field>` entries contributed by the DECLARED plugins, in sorted order."""
    extra: dict = {}
    plugins = cfg.plugins if isinstance(cfg.plugins, dict) else {}
    for slot in sorted(plugins):
        if slot not in _api_registry.SLOTS or not plugins[slot]:
            continue
        try:
            refs = _declared_refs(cfg, slot)
        except Exception:            # a schema query must never be the thing that fails a run
            continue
        for ref in refs:
            try:
                cls, how, _iv, _shape = _api_registry.resolve(slot, ref)
            except Exception:
                continue
            if how == "builtin" and _api_registry.is_builtin(slot, cls):
                # A BUILT-IN's knobs are already top-level `PipelineConfig` fields with their own
                # schema entries; emitting them a second time under `plugins.` would give the GUI two
                # widgets for one number, which is the drift this seam exists to remove.
                continue
            for name, fs in sorted(_plugin_config_fields(cls).items()):
                extra[f"plugins.{slot}.{name}"] = fs.to_schema(group="Plugins")
    return extra


def _declared_refs(cfg, slot: str) -> tuple:
    """Every ref the config declares for one slot, in declared order."""
    if slot == "channel_model":
        return (_channel_selection(cfg)[0],)
    if slot == "check":
        # ISOLATED refs are omitted: every caller of this helper goes on to `resolve()` the ref, and
        # resolving an isolated plugin in the engine's process is precisely what it was declared
        # isolated to avoid.
        return tuple(r for r, _p, _c, _g, iso in _checks_selection(cfg, station_types=True,
                                                                   denm=True) if not iso)
    if slot == "fusion":
        return (_fusion_selection(cfg)[0],)
    sel = cfg.plugins.get(slot)
    return (str(sel.get("ref")),) if isinstance(sel, dict) and sel.get("ref") else ()


def _plugin_config_fields(cls) -> dict:
    """`cls.config_fields()` -> {name: FieldSpec}, or {} when the plugin declares none.

    Reachable BEFORE construction, which is the point: a plugin's knobs have to be documentable and
    validatable without first building the plugin out of the very params being validated. Declared
    as a classmethod/staticmethod it is called directly; declared as a plain instance method it is
    skipped (no instance exists yet) rather than raising.
    """
    fn = getattr(cls, "config_fields", None)
    if not callable(fn):
        return {}
    try:
        spec = fn()
    except TypeError:
        return {}
    return dict(spec) if spec else {}


def config_from_dict(d: dict, *, strict_plugins: bool = True,
                     allow_plugin_drift: bool = False) -> PipelineConfig:
    """Build a PipelineConfig from a plain dict (e.g. a saved run's manifest).

    Accepts either a raw config dict or a full manifest.json (which nests the config under a
    "config" key). Unknown *ordinary* keys are ignored with a stderr warning (forward/backward
    compatible across config-schema changes); list values for tuple fields are coerced back to
    tuples. This is the inverse of what `_write_manifest` serializes, so a saved run replays
    byte-for-byte.

    PLUGIN DRIFT (D4). Dropping unknown keys with a warning and continuing was the worst available
    failure mode for a plugin-configured manifest: it would replay as A DIFFERENT RUN WITH EXIT
    CODE 0. Two things close that hole:

    * `plugins` is now a real config field, so it is never dropped;
    * when a FULL MANIFEST is supplied, its `plugins` LOCK is re-resolved and its content hashes
      re-checked BEFORE the run, raising :class:`PluginDriftError` on mismatch. Built-ins are
      exempt from enforcement (their identity is `dataset_version` plus the pinned goldens; keying
      on run.py's own file hash would make every manifest unreplayable after any engine edit) --
      their hashes are still recorded. `allow_plugin_drift=True` proceeds AND RECORDS the drift in
      the new manifest; it does not silence it.
    * an unknown key that names a plugin slot is a hard error rather than a warning, because it can
      only mean the manifest was written by an engine whose plugin surface this one cannot honour.
    """
    import sys as _sys
    drifts: list = []
    lock = d.get("plugins") if isinstance(d.get("plugins"), dict) and "config" in d else None
    if "config" in d and isinstance(d["config"], dict):
        d = d["config"]
    known = {f.name for f in dataclasses.fields(PipelineConfig)}
    unknown = sorted(set(d) - known)
    if strict_plugins:
        plugin_shaped = [k for k in unknown if k == "plugins" or k.startswith("plugin")]
        if plugin_shaped:
            raise ConfigError(
                f"config carries plugin key(s) {plugin_shaped} this engine does not understand; "
                f"replaying it would silently produce a DIFFERENT run. Known slots: "
                f"{list(_api_registry.SLOTS)}")
        if lock is not None and lock.get("loaded"):
            drifts = _api_registry.verify_lock(lock, allow_drift=allow_plugin_drift)
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
    cfg = PipelineConfig(**kw)
    if drifts:
        # Bound to THIS config object, so only the run of THIS config records it (see
        # `_PLUGIN_DRIFT_ALLOWED`). A drifted config that is validated and never run records nothing.
        _PLUGIN_DRIFT_ALLOWED["cfg"], _PLUGIN_DRIFT_ALLOWED["drifts"] = cfg, list(drifts)
    return cfg


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
# Telemetry seam (default None -> zero effect, byte-identical): invoked with a small dict every time
# a REAL imported <tlLogic> program governs a vehicle's next movement (vid, t, node[x,y], dnode,
# char, action). Reached only when `real_signals` is on, and gated on the hook being set, so it can
# never move a digest. Tests use it to assert that the colour a vehicle is told is the colour its own
# movement has -- which is the one thing a plausible-looking wrong mapping would hide.
SIGNAL_HOOK = None

#: A permissive green ('g'/'s') grants no right of way, so the driver still needs a gap in the
#: conflicting protected stream. HCM 6th edition puts the base critical headway for a PERMITTED
#: left turn from an exclusive lane at 4.1 s; a conflicting vehicle further away than that in TIME
#: is a gap this driver takes. Without a gap criterion a permissive left would simply wait out the
#: whole opposing green and block its own approach, which is a modelling artifact rather than
#: traffic.
PERMISSIVE_CRITICAL_GAP_S = 4.1
#: How sharp a turn has to be before it counts as turning ACROSS the opposing stream (deg of
#: heading change through the junction). 20 deg keeps a curved through-movement out of it.
PERMISSIVE_ACROSS_DEG = 20.0
#: Yellow is a stop UNLESS the vehicle is already inside the dilemma zone -- it cannot reach the
#: stop line's far side and cannot stop at its own comfortable deceleration, so it proceeds. Without
#: this rule a yellow manufactures -6 m/s^2 emergency stops (the IDM's floor) out of ordinary
#: traffic, which is a benign kinematic transient the detectors would then have to absorb.
YELLOW_DILEMMA = True


def run_pipeline(cfg: PipelineConfig) -> RunResult:
    # THE FIRST STATEMENT OF THE RUN, and it has to be. `validate_config` RESOLVES every declared
    # plugin ref, and resolving imports the module -- so a `random.Random = Impostor` at MODULE SCOPE
    # has already run by the time `build_channel` is reached. Module-level code is an earlier hook
    # than `__init__`, it is one line, and the source gate's name list does not contain it. Snapshot
    # before anything reads `cfg.plugins` at all.
    _armed = _integrity.armed_for(cfg)
    _run_sentinel = _integrity.Sentinel(armed=_armed)
    validate_config(cfg)
    _run_sentinel.verify("while RESOLVING the declared plugins (module import time)")
    # THE CONFIG SNAPSHOT. Taken after validation (which is the last thing allowed to touch `cfg`)
    # and deep-copied, so a plugin mutating a nested container -- `cfg.plugins[...]["params"][k]` --
    # is caught as well as a scalar write. This dict, not the live object, is what the manifest
    # records, and `_assert_config_unmoved` compares against it before step 0 and again before the
    # manifest is written. See `ReadOnlyConfig` for why the snapshot, not the view, is the half that
    # actually holds.
    _cfg0 = copy.deepcopy(_config_dict(cfg))
    _ABORT["flag"] = False                            # fresh per run (module state is not reentrant)
    # CLAIMED BY IDENTITY, not merely consumed: a drift accepted by one replay is recorded in THAT
    # run's manifest and in no other, even when the drifted config is built and never run.
    _drift_allowed = _claim_drift_record(cfg)
    # WHOLE-RUN INTEGRITY MONITORING, armed exactly when the config declares plugins -- the only way
    # third-party code enters a run. Two instruments, and they catch different things:
    #
    #   * `_run_sentinel` snapshots the identity of the RNG primitives, the engine's own gates
    #     (`check_outcome`, `srcgate.gate`, `_attest`, `_data_digest`, `_write_manifest`, ...) and the
    #     objects the plugin boundary is made of, and re-compares them just before the manifest is
    #     written. This is what sees a `random.Random` class rebind installed at step 30 -- an attack
    #     that passes ALL FOUR of C3's traps, because conformance exercises a bounded window and a
    #     fixed-window contract test can only certify behaviour it observed. Lengthening the window
    #     is not the fix; monitoring that holds for the whole run is.
    #   * `rng` is a `WitnessedRandom` when armed: bit-identical to `random.Random` (only `random()`
    #     and `getrandbits()` are overridden, each adding one integer increment), but it counts
    #     Mersenne-Twister words, so at the end of the run the engine can PROVE its own stream is
    #     exactly as far along as the draws it made would put it. That catches the second form --
    #     a `random.Random.random` rebind, which leaves every generator STATE a snapshot could
    #     compare perfectly intact while silently supplying different numbers.
    #
    # Neither is a sandbox and neither is evidence of honesty: reading the oracle through a frame
    # walk moves nothing on either list. See `api/integrity.py` and DETECTOR-PLUGIN.md section 2.
    rng = _integrity.engine_random(cfg.seed, armed=_armed)
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
    mag_scale = _parse_magnitude_scale(cfg.attack_magnitude_scale)  # {} -> every type scale 1.0 (byte-identical)
    # A run carries the MA-visible self-declared station_type (and the vruImpersonation detector) when
    # EITHER genuine VRUs are present (vru_pct>0) OR the opt-in VRU-impersonation attack is selected via
    # any of the attack selectors. When NEITHER holds no beacon ever declares "vru", so gating the
    # station_type field + the extra detector key on this flag keeps the DEFAULT path byte-identical.
    _impersonation_enabled = (any(a in catalog for a in IDENTITY_SPOOF_ATTACKS)
                              or (attack_weights is not None
                                  and any(a in attack_weights for a in IDENTITY_SPOOF_ATTACKS)))
    _emit_station_type = cfg.vru_pct > 0 or _impersonation_enabled
    # ---- CHANNEL MODEL: resolved and constructed ONCE, before step 0 -----------------------------
    # Resolution happens here and every failure is fatal here (PLUGIN-ARCH 3.2): a plugin that
    # raises mid-loop would produce a partial dataset whose digest matches nothing, and it must NOT
    # take the deliberate SIGINT path below, which finalises a VALID manifest for a partial run.
    # The instance is a PER-RUN object, never a module global, so the in-process multi-run drivers
    # (datagen/foundry.py, campaign.py, massive.py, gui/agent.py) cannot cross-contaminate.
    # Building footprints come from the custom-network document's optional "buildings" layer
    # (written by `osm.py --buildings`, projected with the ROAD graph's own projection tuple so the
    # two layers are registered); a synthetic map has none and falls back to the canyon density.
    _geo_buildings = _parse_buildings(cfg.custom_network) if cfg.road_network == "custom" else []
    # THE WHOLE-CITY PATH's footprints. `road_network="sumo"` imports a real SUMO net, and a SUMO
    # scenario ships its buildings as a separate polygon additional-file rather than inside the
    # network document -- so without this the largest scene in the project would run with roads and
    # no buildings and silently fall back to the canyon density. `scene_from_net` reads the net
    # itself and builds the transform with the SAME `netimport._transformer` call the road import
    # makes, so there is no way for the two layers to end up in different frames; it then GATES on
    # the footprints landing on the junctions (measured: the gate fires on an 11 m displacement).
    _geo_building_stats: dict = {}
    if cfg.road_network == "sumo" and cfg.sumo_buildings:
        from .netimport import scene_from_net as _scene_from_net
        from .sumo_trace import frame_for_city as _frame_for_city
        _geo_buildings, _geo_building_stats = _scene_from_net(
            cfg.sumo_net, cfg.sumo_buildings, projection=_frame_for_city(cfg.sumo_frame_city))
        if cfg.verbose:
            print(f"[sumo scene] {os.path.basename(cfg.sumo_buildings)} -> "
                  f"{_geo_building_stats['polygons']} footprints "
                  f"({_geo_building_stats['vertices']} vertices), median centroid offset "
                  f"{_geo_building_stats.get('median_offset_m')} m from the median junction, "
                  f"{_geo_building_stats.get('junctions_in_footprint_frac')} of junctions inside "
                  f"a footprint", flush=True)
    chan, chan_provenance = build_channel(cfg, buildings=_geo_buildings, dt=cfg.dt)
    # A construction-time write to `env["config"]` is fatal HERE, before step 0 -- which is where
    # every plugin failure belongs (PLUGIN-ARCH 3.2), and is early enough that no output directory
    # has been created.
    _assert_config_unmoved(_cfg0, cfg, "while constructing the channel plugin")
    if _armed:
        _run_sentinel.verify("after LOADING the channel plugin")
    _chan_caps = chan.capabilities()
    _chan_additive = LOSS_ADDITIVE_LEGACY in _chan_caps
    _chan_batch = not hasattr(chan, "evaluate_link")
    _chan_prune = getattr(chan, "prune", None)
    _chan_env_ro = types.MappingProxyType({"buildings": _geo_buildings, "weather": cfg.weather})
    # `geo_chan` is the BUILT-IN geometric instance when that is what is active, else None. Two
    # sites still reach into this model's internals -- the colluder's fabricated-RSSI synthesis and
    # the per-link state prune -- so they stay gated on the concrete type rather than on a
    # capability. Generalising them belongs with the detector seam (roadmap phase 3), not here.
    geo_chan = getattr(chan, "model", chan)
    geo_chan = geo_chan if isinstance(geo_chan, GeometricChannel) else None
    # A received-power figure is a DECLARED CAPABILITY, not a model name: only a model that declares
    # `rssi` can carry an `rssi_dbm` evidence column. Gated exactly like station_type above -- the
    # key is ABSENT from every ma_reports row under disc/logdistance -> byte-identical default.
    _emit_rssi = CAP_RSSI in _chan_caps
    if cfg.verbose and geo_chan is not None:
        print(f"[geometric radio] env={cfg.radio_env} tx={cfg.radio_tx_power_dbm} dBm "
              f"sens={cfg.radio_rx_sensitivity_dbm} dBm cap={geo_chan.reach_m:.0f} m "
              f"sense={geo_chan.sense_m:.0f} m buildings="
              f"{0 if geo_chan.buildings is None else geo_chan.buildings.n_polygons}"
              f"{'' if geo_chan.buildings is not None else f' (canyon {cfg.radio_nlosb_density_per_km}/km)'}",
              flush=True)
    # ---- THE PROTOCOL STACK: codec, PROFILE, report format, security ---------------------------
    # Constructed here, before step 0, for the same reason the channel is: every failure a wire
    # format, a protocol stack or a PKI can produce (a missing optional dependency, an unknown ETSI
    # StationType name, a rejected parameter, an incoherent layer combination) must be fatal BEFORE
    # any output directory exists.
    #
    # THE PROFILE IS THE SEAM (api/profile.py). Below this block the engine asks a `ProtocolProfile`
    # -- never `codecs.etsi_rules`, and never a config flag -- when a station transmits, how often it
    # is allowed to, what a frame weighs, what that costs on the air and how long it takes to
    # arrive. The built-in ITS-G5 stack is resolved through that seam like anybody else's, which is
    # the whole test of whether the seam is real. Each of profile, report format and security layer
    # is independently None by default, and the whole block costs one `if` per feature on the
    # default path.
    _codec, codec_provenance = build_codec(cfg)
    _assert_config_unmoved(_cfg0, cfg, "while constructing the message codec")
    if _codec is not None and _armed:
        _run_sentinel.verify("after LOADING the message codec")
    # THE PROFILE. One object that owns the codec, the generation rules, the congestion controller,
    # the wire-size accounting and the airtime/latency model. `None` when nothing asked for one, in
    # which case not a single line below it executes and the default path is exactly what it was.
    # The codec built above is INJECTED; a third-party profile may return its own from `codec()`,
    # and everything downstream reads `_profile.codec()` rather than `_codec`.
    _profile, profile_provenance = build_profile(cfg, _codec)
    _assert_config_unmoved(_cfg0, cfg, "while constructing the protocol profile")
    if _profile is not None and _armed:
        _run_sentinel.verify("after LOADING the protocol profile")
    _report_fmt, report_format_provenance = build_report_format(cfg)
    _assert_config_unmoved(_cfg0, cfg, "while constructing the report format")
    if _report_fmt is not None and _armed:
        _run_sentinel.verify("after LOADING the report format")
    if _profile is not None:
        _declared_codec, _codec = _codec, _profile.codec()
        if _declared_codec is not None and _codec is None:
            # REFUSED, not silently ignored. The config asked for a wire format, the profile threw
            # it away, and a run whose manifest says `message_codec: native_v1` while nothing was
            # ever encoded is the "replays as a different run at exit 0" failure the whole plugin
            # lock exists to prevent. A profile is entitled to bring its OWN codec -- that is the
            # point of the seam -- but not to answer `None` to a declared one.
            raise ConfigError(
                f"protocol_profile {getattr(_profile, 'profile_id', '?')!r} returned None from "
                f"codec() while the config declared message_codec="
                f"{(cfg.message_codec or _codec_selection(cfg)[0])!r}. A profile may return its own "
                f"codec, but discarding a declared one would put a wire format in the manifest of a "
                f"run that never encoded anything. Drop the message_codec declaration, or have the "
                f"profile consume env['message_codec'].")
    #: The profile's declared capabilities, which is HOW the engine learns which layers are active.
    #: Not `cfg.cam_generation_rules`: a third-party profile that supplies generation rules gets them
    #: run without an engine flag, which is the whole point of the seam.
    _prof_caps = frozenset(_profile.capabilities()) if _profile is not None else frozenset()
    #: The encoder, or None. Built once; holds the frozen StationViews so the reception loop does
    #: not allocate one per message. The frame LENGTH comes from the profile, which is where the
    #: security envelope is accounted for.
    _wire = (WireEncoder(
        _codec, cfg.message_signer,
        # The two calls the encoder makes per message come from two DIFFERENT plugins, so each is
        # guarded under its own slot's `source_gate`: the frame length from the profile, the octets
        # from the codec.
        sizer=_pguard.guarded(_profile.wire_size_bytes,
                              _obj_guard_label(cfg, "protocol_profile", _profile)),
        guard_label=_obj_guard_label(cfg, "message_codec", _codec))
        if _codec is not None else None)
    _codec_claim = dict(_codec.standards_claim()) if _codec is not None else None
    _profile_claim = dict(_profile.standards_claim()) if _profile is not None else None
    if cfg.verbose and _profile is not None:
        print(f"[protocol profile] {getattr(_profile, 'profile_id', '?')} "
              f"layers={sorted(_prof_caps)}", flush=True)
    if cfg.verbose and _codec is not None:
        print(f"[message codec] {getattr(_codec, 'profile_id', cfg.message_codec)} "
              f"signer={cfg.message_signer}", flush=True)
    if cfg.verbose and _report_fmt is not None:
        print(f"[report format] {getattr(_report_fmt, 'format_id', cfg.report_format)}", flush=True)
    #: Per-station generation service state (EN 302 637-2 for the built-in). Absent -> the historic
    #: one-message-per-station-per-step cadence.
    _cam_state: dict = {} if _api_profile.CAP_GENERATION in _prof_caps else None
    #: Per-station congestion-control entity (TS 102 687 for the built-in). Absent -> no rate limit.
    _dcc_state: dict = {} if _api_profile.CAP_CONGESTION in _prof_caps else None
    #: The last encoded PDU each pseudonym put on the air, for TS 103 759 `v2xPduEvidence`.
    #:
    #: Written ONLY when a report format actually declares `evidence_pdu`. The octets exist on every
    #: run with a codec, but keeping them for a run that will never file them is an unconditional
    #: allocation per broadcast on the hottest path in the engine -- and "the default path pays
    #: nothing" is the rule every seam here is written to.
    #:
    #: One entry per PSEUDONYM, overwritten in place, so the store is O(pseudonyms ever seen) rather
    #: than O(messages) -- and its bound is stated rather than assumed: the 1188-vehicle hour with
    #: 300 s rotation reaches ~14k pseudonyms at ~400 B of PDU, i.e. ~6 MB. It is deliberately NOT
    #: pruned with `last_claimed`: a report can legitimately be filed about a station whose
    #: pseudonym has just rotated, and dropping the evidence for it would make `v2xPduEvidence`
    #: absent exactly on the reports a rotation-aware analysis most wants.
    _evidence_store: dict = {}
    _evidence_on = (_report_fmt is not None and _codec is not None
                    and _api_report.CAP_EVIDENCE_PDU in frozenset(_report_fmt.capabilities()))
    #: The CBR each receiver measured LAST step -- the input DCC reacts to. One step of lag is not
    #: an approximation, it is the causality: a station cannot react to a load it has not yet heard.
    _cbr_measured: dict = {}
    #: Trigger-reason tally and inter-CAM gaps, for the manifest's `protocol` block. Manifest-only,
    #: hence outside `data_digest` by construction.
    _cam_triggers: Counter = Counter()
    _cam_gap_sum, _cam_gap_n, _cam_gap_max = 0.0, 0, 0.0
    _cam_last_t: dict = {}
    _cbr_sum, _cbr_n, _cbr_max = 0.0, 0, 0.0
    _lat_sum, _lat_n, _lat_min, _lat_max = 0.0, 0, float("inf"), 0.0
    #: Latency HISTOGRAM at 1 microsecond resolution, not a list of samples.
    #:
    #: Keeping every value would be exact and would also cost ~860 MB on the 1188-vehicle hour
    #: (26.9 million delivered frames x 32 B per boxed float), which is more than the whole run's
    #: working set. A microsecond-binned Counter is O(distinct latencies) -- a few thousand keys,
    #: because the distribution is dominated by a constant -- and its quantiles are exact to 1 us,
    #: three orders of magnitude below the millisecond the figures are reported in.
    _lat_hist: Counter = Counter()
    _wire_size_sum, _wire_size_n = 0, 0
    #: Airtime per frame size, memoised. `ppdu_symbols` is a ceil over an integer division and the
    #: engine sees a handful of distinct sizes, so this is a dict lookup instead of two divisions
    #: and a ceil on every one of ~10^7 offered frames.
    _airtime_cache: dict = {}

    def _airtime_s(nbytes: int) -> float:
        v = _airtime_cache.get(nbytes)
        if v is None:
            v = _profile.frame_airtime_s(nbytes)
            _airtime_cache[nbytes] = v
        return v

    #: The security layer, or None. Imported INSIDE the branch: `scms_core.ecdsa_p256` probes the
    #: ECDSA backend at import time (`SIGNING_MODE`), and a run that did not ask for real
    #: cryptography must not pay for that probe -- the same discipline `scms_core/__init__` states.
    _sec = None
    if cfg.security_model == "ecdsa":
        from ..scms_core import engine_security as _engine_security
        # `crl` is held BY REFERENCE and is the very list `crl_entries` becomes below, so a
        # revocation appended mid-run is immediately visible to every receiver's verifier -- which
        # is what makes "present a revoked certificate" a receiver-side detection rather than
        # engine-side bookkeeping.
        _sec = _engine_security.SecurityLayer(cfg.derive, jmax=cfg.jmax)
        if cfg.verbose:
            from ..scms_core.ecdsa_p256 import SIGNING_MODE as _SIGNING_MODE
            print(f"[security] ECDSA-P256 over the 1609.2 double hash; nonces={_SIGNING_MODE}; "
                  f"butterfly provisioning (PCA sees one opaque token per certificate)", flush=True)
    #: Attacks whose CURRENT implementation is an edit to a wire field that a real signature makes
    #: unforgeable. Under `security_model="ecdsa"` each is re-expressed as a thing the attacker
    #: DOES (see `scms_core.secured.SIGNATURE_ATTACKS`); the counter records how often the honest
    #: form was unavailable, so the loss is measured rather than hidden.
    _sec_refusals: Counter = Counter()
    #: `secured.VerificationResult.status` tally over every delivered frame. Eight-valued, where
    #: the boolean was two-valued: `unknown_issuer`, `cert_signature_invalid`, `cert_expired`,
    #: `cert_not_yet_valid`, `cert_revoked`, `psid_not_permitted`, `cert_unavailable`,
    #: `signature_invalid`, `ok`.
    _verdicts: Counter = Counter()
    #: vid -> (SignedBroadcast, cx, cy, cs, ch, cg): the first frame this station ever transmitted,
    #: kept so a `DataReplay` attacker can re-emit a frame it really captured, signature included.
    _sec_last_frame: dict = {}
    #: A per-packet latency model is active iff the PROFILE declares it. For the built-in that is
    #: exactly `cfg.net_latency_model`; for a third-party stack it is the stack's own declaration.
    _lat_on = _api_profile.CAP_LATENCY in _prof_caps
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
    _sumo_tf = None            # the netimport transform, SHARED with the replay provider below
    _sumo_info: dict = {}
    _sumo_surface: dict = {}   # roads.CustomNetwork.set_road_surface provenance (sumo path only)
    _sumo_net_sha = ""
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
        net = CustomNetwork(*_parse_custom_network(cfg.custom_network,
                                                   directed=cfg.custom_network_directed))
    elif cfg.road_network == "sumo":
        # THE COHERENCE GUARANTEE, and it is structural rather than a check bolted on afterwards.
        # `engine_network` reads the .net.xml ONCE and hands back both the imported graph and the
        # very transform it applied to the junction coordinates. The replay provider below is
        # constructed with THAT transform, so the vehicles and the roads they drive on land in one
        # frame by construction. (`osm.py`'s trap: a layer re-derived with its own origin misaligns
        # by whole city blocks while every individual street still looks plausible.)
        from .osm import network_document
        from .roads import CustomNetwork
        from .sumo_trace import engine_network, file_sha256 as _net_sha
        _sumo_nodes, _sumo_edges, _sumo_info, _sumo_tf = engine_network(
            cfg.sumo_net, frame_city=cfg.sumo_frame_city,
            directed=cfg.custom_network_directed, signals=cfg.real_signals)
        _sumo_net_sha = _net_sha(cfg.sumo_net)
        net = CustomNetwork(*_parse_custom_network(
            network_document(_sumo_nodes, _sumo_edges, _sumo_info),
            directed=cfg.custom_network_directed))
        # THE DRIVABLE SURFACE, and it is the other half of the coherence guarantee. The graph above
        # answers "where can a trip go"; `dist_to_road` -- which feeds mapOffRoad and the geometric
        # channel's building blockage -- asks "is this vehicle on tarmac", and the graph is a bad
        # proxy for that on a real city: one centreline per physical road (a two-way street has two
        # carriageways, and 35 InTAS node-pairs carry more than one distinct road), and nothing at
        # all inside a junction, where SUMO drives an internal lane for tens of metres. Measured on
        # InTAS: p95 17.434 m -> 3.197 m, max 95.227 m -> 4.803 m, 12.70% -> 0.00% beyond 8 m.
        # Geometry only: no node, no edge, no route and no RNG draw changes because of this.
        if _sumo_info.get("road_surface"):
            _sumo_surface = net.set_road_surface(**_sumo_info["road_surface"])
        if cfg.verbose:
            print(f"[sumo net] {os.path.basename(cfg.sumo_net)} -> {len(net.nodes)} junctions "
                  f"({_sumo_info.get('intersections_deg_ge3', '?')} deg>=3), "
                  f"{_sumo_info.get('n_directed_edges', '?')} directed edges, oneway "
                  f"{_sumo_info.get('oneway_share', 0.0)}, {_sumo_info.get('n_signal_nodes', 0)} "
                  f"signalised junctions, {_sumo_info.get('kept_edges', 0)} graph edges "
                  f"({_sumo_info.get('parallel_road_keys', 0)} node-pairs carry >1 physical road) "
                  f"| surface {_sumo_surface.get('surface_segments', 0)} segments + "
                  f"{_sumo_surface.get('junction_discs', 0)} junction discs", flush=True)
    # ---- OPT-IN directed carriageways -------------------------------------------------------- #
    # `enable_directed_lanes()` gives each direction of travel its own carriageway, offset sideways
    # off the graph centreline, so two vehicles driving opposite ways down one street no longer
    # occupy the same polyline. Default OFF -> not called -> no offsets, no new state, every pinned
    # golden byte-identical. This call is the ONLY thing that stood between the measured
    # head-on-overlap result and a user being able to reproduce it from a config.
    if cfg.directed_lanes and net is not None:
        net.enable_directed_lanes(lane_width_m=cfg.lane_width_m, drive_side=cfg.drive_side,
                                  lanes_per_dir=cfg.n_lanes)
    # ---- OPT-IN REAL signal programs ---------------------------------------------------------- #
    # `signals.SignalPlan` answers "what colour does THIS movement have at time t" out of the
    # <tlLogic> the imported .net.xml ships. Attached to the map here, read by `car_follow` below,
    # and NOTHING else changes: no node, no edge, no route, and no rng draw (a movement's colour is
    # a pure function of the program, so a signalised run adds zero draws to any stream). Default
    # OFF -> `set_signal_plan` is never called and `_LaneFrameMixin.signal_plan` stays the class
    # attribute None, which is what makes `signal_char` a `plan is None` test at the call site
    # rather than a getattr.
    #
    # THE PLAN IS KEYED ON COORDINATES, not node indices, and that is load-bearing rather than
    # stylistic: `roads.largest_strong_component` (which every directed import runs through, and
    # which `engine_network` now runs UNCONDITIONALLY) remaps node indices and returns no remap, so
    # a plan carried across it by index would signalise the wrong junctions and still look entirely
    # plausible. `from_records` resolves the record indices against the array they were WRITTEN
    # against -- the importer's own `nodes`, before any further trim -- exactly once, here.
    _sig_stats: dict = {}
    if cfg.real_signals and net is not None:
        from .signals import SignalPlan
        if cfg.road_network == "sumo":
            _sig_records = _sumo_info.get("signal_programs") or []
            _sig_nodes = _sumo_nodes
        else:                                      # "custom": the netimport --signals document
            _sig_doc = _custom_network_doc(cfg.custom_network) or {}
            _sig_records = _sig_doc.get("signal_programs") or []
            _sig_nodes = _sig_doc.get("nodes") or []
        _sig_stats = net.set_signal_plan(SignalPlan.from_records(_sig_records, _sig_nodes))
        if not _sig_stats.get("programs"):
            # A refusal, not a warning. `real_signals` past validate_config means the layer WAS
            # present; landing zero programs on the graph means every record was skipped (node
            # indices that do not address this `nodes` array), and the run would silently be the
            # toy-signal run the flag exists to replace.
            raise ValueError(
                f"real_signals=true but no imported program landed on the graph: "
                f"{len(_sig_records)} signal_programs records resolved to 0 junctions "
                f"({_sig_stats.get('skipped_records', 0)} skipped). The records' node indices do "
                f"not address the {len(_sig_nodes)} nodes they were read against.")
        if _sig_stats.get("collisions"):
            # Two DIFFERENT programs on one junction coordinate: the importer's node dedupe merged
            # two signalised junctions, and half the movements would be lit by a program that does
            # not govern them. Never silent.
            raise ValueError(
                f"real_signals: {_sig_stats['collisions']} junction coordinate(s) carry two "
                f"different <tlLogic> programs -- the node dedupe merged two signalised junctions, "
                f"so one program's movements would be lit by the other's phase string")
        if cfg.verbose:
            print(f"[real signals] {_sig_stats['programs']} programs on "
                  f"{_sig_stats['approaches']} approaches / {_sig_stats['movements']} movements "
                  f"(of {len(_sig_records)} records, {_sig_stats['skipped_records']} skipped)",
                  flush=True)
    # ---- OPT-IN pedestrian infrastructure ----------------------------------------------------- #
    # `vru.SidewalkNetwork` derives one footway per side of every road edge, a crossing per junction
    # arm and the corner links that join them, then hands out `PedestrianWalk` itineraries that drop
    # straight into `Vehicle.true_state`'s existing `trip is not None` branch. Default OFF, and
    # gated on vru_pct > 0, so nothing here is imported, built or drawn from on any pinned path.
    _sidewalks = None
    _ped_signal_fn = None
    _sidewalk_stats: dict = {}
    if cfg.sidewalks and net is not None and cfg.vru_pct > 0:
        from . import vru as _vru
        # WHICH CROSSINGS ARE SIGNALISED must be the same question the VEHICLES answer, or the two
        # populations at one junction are on two different clocks. `signal_nodes=None` signalises
        # every junction -- exactly what `traffic_lights` does to the vehicles -- and an explicit
        # COORDINATE list signalises only those. Coordinates, never indices: the strong-component
        # trim remaps indices and returns no remap (the same trap `set_signal_plan` documents).
        _ped_sig_nodes = None                      # None = every junction (traffic_lights semantics)
        if not cfg.traffic_lights:
            _ped_sig_nodes = (sorted(net.signal_plan.programs) if net.signal_plan is not None
                              else ())             # real programs only, or nothing at all
        _sidewalks = _vru.build_sidewalks(
            net, lane_width_m=cfg.lane_width_m, lanes_per_dir=cfg.n_lanes,
            drive_side=cfg.drive_side, sidewalk_width_m=cfg.sidewalk_width_m,
            kerb_clearance_m=cfg.kerb_clearance_m, signal_nodes=_ped_sig_nodes)
        if _sidewalks is None:
            raise ValueError("sidewalks=true but no pedestrian network could be derived from this "
                             "road map")
        _sidewalk_stats = _sidewalks.stats()
        _ped_signal_fn = _make_ped_signal_fn(net, cfg)
        if cfg.verbose:
            print(f"[sidewalks] {_sidewalk_stats.get('ped_links', '?')} ped links "
                  f"({_sidewalk_stats.get('sidewalk_total_m', '?')} m footway, "
                  f"{_sidewalk_stats.get('crossings', '?')} crossings, "
                  f"{_sidewalk_stats.get('signalised_crossings', '?')} signalised) over "
                  f"{_sidewalk_stats.get('components', '?')} components", flush=True)
    # ---- OPT-IN SUMO-backed mobility: load the frozen trajectory ----------------------------- #
    # `mobility_source="internal"` (the default) leaves `_replay` None: not one line below runs, no
    # module is imported, no rng is drawn, every pinned golden holds. When it IS on, this is the
    # WHOLE seam -- the provider supplies the spawn schedule and writes cur_x/cur_y/cur_v/cur_h each
    # step, and everything downstream (SCMS, attacks, radio, detectors, MA, reporting) is untouched.
    _replay = None
    _mob_block = None
    if cfg.mobility_source == "sumo_replay":
        from .sumo_trace import SumoReplayMobility, load as _load_trace
        _trace = _load_trace(cfg.sumo_trace)
        if _trace.sha256 != cfg.sumo_trace_sha256:        # belt and braces: validate_config filled it
            raise ValueError(f"the frozen trajectory changed between validation and load "
                             f"({cfg.sumo_trace_sha256} -> {_trace.sha256})")
        _replay = SumoReplayMobility(_trace, dt=cfg.dt, transform=_sumo_tf)
        # THE COHERENCE ASSERTION. A replayed vehicle must be ON the engine's roads: `dist_to_road`
        # feeds the mapOffRoad detector, and the geometric channel ray-casts building footprints
        # registered to this same frame. A road-following vehicle sits within about half a
        # carriageway of the centreline (SUMO puts it on a LANE centre, the engine's edge is the
        # junction-to-junction line); a misregistered frame scatters the distribution instead.
        _offroad_stats = _replay.offroad_stats(net.dist_to_road)
        if _offroad_stats["p95"] > cfg.sumo_offroad_p95_max_m:
            raise ValueError(
                f"SUMO replay is NOT coherent with the engine's network: distance-to-road over "
                f"{_offroad_stats['n']} sampled replayed positions is p50={_offroad_stats['p50']} m, "
                f"p95={_offroad_stats['p95']} m, max={_offroad_stats['max']} m, against a "
                f"sumo_offroad_p95_max_m of {cfg.sumo_offroad_p95_max_m} m. The vehicles are not "
                f"driving on the roads the engine reasons about -- almost always because sumo_net "
                f"is not the .net.xml the trace was frozen on, or because sumo_frame_city projects "
                f"the network into a frame the trajectory was never transformed into.")
        _mob_block = {"source": "sumo_replay",
                      "provider": _replay.describe(),
                      "trace_path_basename": os.path.basename(cfg.sumo_trace),
                      "trace_sha256": _trace.sha256,
                      "network": {"path_basename": os.path.basename(cfg.sumo_net),
                                  "sha256": _sumo_net_sha,
                                  "frame_city": cfg.sumo_frame_city,
                                  "n_nodes": len(net.nodes),
                                  "directed": bool(cfg.custom_network_directed),
                                  "n_signal_nodes": len(_sumo_info.get("signal_nodes", ())),
                                  # what the graph is, and what it had to leave out, on the record
                                  "net_junctions": _sumo_info.get("net_junctions"),
                                  "strong_component_nodes": _sumo_info.get("strong_component_nodes"),
                                  "nodes_dropped_not_strongly_connected":
                                      _sumo_info.get("strong_trimmed_nodes"),
                                  "parallel_road_keys": _sumo_info.get("parallel_road_keys"),
                                  "road_surface": _sumo_surface},
                      "coherence": {"dist_to_road_m": _offroad_stats,
                                    "p95_gate_m": cfg.sumo_offroad_p95_max_m,
                                    # `dist_to_road` is measured against the SURFACE, not the graph
                                    "measured_against": ("road_surface" if _sumo_surface
                                                         else "routing_graph")}}
        if cfg.verbose:
            print(f"[sumo replay] {len(_trace.vehicles)} frozen trajectories, {_trace.n_rows} "
                  f"vehicle-steps, sumo_seed={_trace.meta.get('sumo_seed')} "
                  f"({_trace.meta.get('sumo_version')}), teleports={_trace.meta.get('teleports')} "
                  f"| dist_to_road p50={_offroad_stats['p50']} p95={_offroad_stats['p95']} "
                  f"max={_offroad_stats['max']} m | trace {_trace.sha256[:16]}", flush=True)
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

    def make_vehicle(vid, spawn_time, is_att, is_flt, is_coll, trip, life_hint, replay_span=None):
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
        if replay_span is not None:
            # SUMO REPLAY. The vehicle is a `cf` actor in the only sense `Vehicle.true_state` cares
            # about -- its truth is read from `cur_*`, which the replay provider writes each step --
            # but the IDM integrator never runs for it (`cf_active` is off in this mode).
            #
            # CERTIFICATE LIFETIME, and this is the point of requirement 5. The internal model has
            # to GUESS a trip's duration (3x free-flow + half a signal cycle per intersection + 30 s
            # slack) because congestion makes the arrival time dynamic, and a guess that comes in
            # short expires an honest vehicle's certificate mid-trip -- mass false `certValidity`
            # positives and a precision collapse that is an artifact of the budget, not of any
            # attack. Under replay there is nothing to guess: SUMO already drove the whole trip and
            # the trace records exactly when this vehicle leaves. The budget is that duration, from
            # the SUMO route, plus `sumo_cert_slack_s`.
            cf = True
            finish_time = replay_span.finish_time
            life = max(cfg.dt, replay_span.finish_time - spawn_time) + cfg.sumo_cert_slack_s
        elif cf:
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
        # THE WINDOWS, computed first and identically on both paths. Under `security_model="ecdsa"`
        # the whole device is provisioned in ONE butterfly batch -- which is the point: the RA
        # drains its request queue in a device-mixing order, so the PCA never sees a device's
        # certificates as a group. Issuing them one at a time inside the loop below would hand the
        # PCA the arrival correlation the shuffle exists to destroy.
        _windows = []
        for k in range(n_rot):
            i_k, j_k = k // cfg.jmax, (vid + k) % cfg.jmax
            vf = spawn_time + (k * cfg.rotate_period_s if cfg.rotate_period_s > 0 else 0.0)
            vt = spawn_time + ((k + 1) * cfg.rotate_period_s if cfg.rotate_period_s > 0 else life)
            if cf and k == n_rot - 1:
                # dynamic (congestion-dependent) despawn can outrun any life estimate; the vehicle's
                # FINAL cert must stay valid for its whole presence, so cap it past the sim end -> a
                # present benign vehicle can never show an "expired" cert (attacks override cvt/cvf).
                vt = max(vt, total_time + cfg.dt)
            _windows.append((i_k, j_k, vf, vt))
        _ghost_windows = []
        if _sec is not None and is_att and atype == "Sybil":
            # A Sybil's ghosts are REAL CREDENTIALS it holds and uses simultaneously -- extra
            # j-indices in i-period 0. That is what the attack is under a working SCMS: not forged
            # certificates (those are `ForgedCertificate`, and a receiver rejects them on the
            # issuer), but more legitimate pseudonyms than a station is entitled to run at once.
            _ghost_windows = [(0, (cfg.jmax - 1 - g) % cfg.jmax, spawn_time, spawn_time + life)
                              for g in range(cfg.sybil_ghosts)]
        _creds = None
        if _sec is not None:
            _creds = _sec.provision(true_id, ctx, _windows + _ghost_windows, attacker=is_att)
        pseudonyms = []
        for k, (i_k, j_k, vf, vt) in enumerate(_windows):
            if _creds is not None:
                dig = _creds.credentials[k].digest
            else:
                pk = ca.keypair_from_seed(cfg.derive(f"key:{vid}:{k}"))
                dig = ca.hashed_id8(ca.public_bytes(pk)).hex()
            pca.issue(dig, req_hash, i_k, j_k, la_h1, la_h2)
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
                if _creds is not None:
                    gdig = _creds.credentials[len(_windows) + g].digest
                else:
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
        vf = spawn_time
        vt0 = max(spawn_time + life, total_time + cfg.dt)
        if _sec is not None:
            dig = _sec.provision(true_id, ctx, [(0, j0, vf, vt0)]).credentials[0].digest
        else:
            pk = ca.keypair_from_seed(cfg.derive(f"key:{vid}:0"))
            dig = ca.hashed_id8(ca.public_bytes(pk)).hex()
        pca.issue(dig, req_hash, 0, j0, la_h1, la_h2)
        vt = vt0                                           # cert covers the VRU's whole presence -> a
        pseudonyms = [{"k": 0, "i": 0, "j": j0, "digest": dig,    # benign VRU never shows an expired cert
                       "valid_from": vf, "valid_to": vt}]
        pseudonym_info[dig] = {"i": 0, "j": j0, "lv": ctx.linkage_value_for(0, j0),
                               "ghost": False, "veh_vid": vid}
        gt_idmap.append(R.GtIdentityMap(true_vehicle_id=true_id, pseudonym_cert_digest=dig,
                                        i_period=0, valid_from=round(vf, 3), valid_to=round(vt, 3)))
        # ---- OPT-IN: a real pedestrian itinerary on the derived sidewalk network ----------------
        # `PedestrianWalk` exposes `.state(t)`, which is the whole of what `Vehicle.true_state`'s
        # `trip is not None` branch needs, so substituting it here is the entire behavioural change:
        # the VRU walks footways, waits at kerbs and crosses at crossings instead of holding one
        # heading for its whole life. `spawn_x`/`lane_y` are set to the walk's own start so the
        # spawn coordinate and the trajectory agree (they are read by the GUI and by the fixed-fleet
        # path). Every draw comes from `f"{seed}:vruwalk:{vid}"` -- a stream that exists nowhere
        # else -- so the `f"{seed}:vru:{vid}"` sequence below is untouched and a run with
        # sidewalks off is byte-identical.
        walk = None
        if _sidewalks is not None:
            walk = _sidewalks.walk(vid, cfg.seed, spawn_time, life, cfg.vru_speed_mps,
                                   signal_fn=_ped_signal_fn,
                                   unsignalised_max_wait_s=cfg.crossing_wait_max_s)
        # placement: near a network node but DELIBERATELY off the road centerline (a plaza / pedestrian
        # zone / separated path), i.e. > offroad_tol_m from any road in both axes, so a naive mapOffRoad
        # check WOULD flag them -- which is exactly why a VRU-declared beacon suppresses that detector.
        # With a walk the START COMES FROM THE FOOTWAY and the five placement draws below are simply
        # not taken, so this VRU's own `f"{seed}:vru:{vid}"` stream advances differently from a
        # non-sidewalk run (its gps_q and wander parameters move). That is confined to VRUs on the
        # opt-in path -- no vehicle stream, and no other VRU's stream, is touched.
        if walk is not None:                               # on a footway, by construction
            sx, sy = walk.start()
        elif nodes:
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
            finish_time=(spawn_time + life if cfg.traffic_flow else None), trip=walk)
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
        # `_replay is None` is the DEFAULT and is exactly `while True` there -- a constant load, no
        # draw, no behaviour change. Under SUMO replay the engine's own arrival process is simply not
        # run: SUMO decided who departs when, and the frozen trace records it (block below).
        while _replay is None:                   # thinning: candidates at max rate, kept per demand
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
        if _replay is not None:
            # ---- SUMO REPLAY: the arrival process IS the frozen trace ------------------------- #
            # No thinning, no expovariate, no `random_trip` -- departures, routes and car-following
            # are all SUMO's, decided before this run started. What is still drawn here is the ROLE
            # assignment (attacker / faulty / colluder), which belongs to the SECURITY experiment
            # rather than to the traffic model, and it comes from its OWN string-keyed stream
            # (`{seed}:sumoflow`) so it cannot touch the global `rng` sequence the pinned goldens
            # depend on. Vehicles are created in the trace's canonical `idx` order, which the
            # artifact records, so `vid` is a deterministic function of the trace alone.
            _srng = random.Random(f"{cfg.seed}:sumoflow")
            from .roads import Trip as _Trip
            for _span in _replay.plan(total_time=total_time, max_vehicles=cfg.max_total_vehicles):
                is_att = _srng.random() < cfg.attacker_pct
                is_flt = (not is_att) and _srng.random() < cfg.faulty_pct
                is_coll = is_att and _srng.random() < cfg.collude_pct
                # The engine's `Trip` still exists for this vehicle, built from the DRIVEN polyline
                # rather than from a router: `trip.length` is the SUMO route length and `trip.speed`
                # its mean speed, so everything that reads `Vehicle.trip` keeps working. The
                # per-step kinematics never come from it -- they come from the frozen arrays.
                _trip = _Trip(_span.polyline, _span.mean_speed, _span.spawn_time)
                make_vehicle(vid, _span.spawn_time, is_att, is_flt, is_coll, _trip,
                             life_hint=90.0, replay_span=_span)
                _replay.bind(vid, _span.idx)
                vid += 1
            if cfg.verbose:
                print(f"[sumo replay] {vid} vehicles scheduled from the frozen trace over a "
                      f"{total_time:.0f} s horizon", flush=True)
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
    # ---- ORACLE mobility record + SURVIVORSHIP accounting -----------------------------------------
    # `gt_emissions` above is written inside the broadcast pre-pass and therefore stops at
    # revocation. `gt_mobility` is the SAME motion recorded BEFORE that gate, and the counters
    # beside it are what makes the difference between the two a published NUMBER instead of a
    # footnote. The counters are UNCONDITIONAL (they land in `manifest["counts"]`, which
    # `_data_digest` excludes by construction, so they move no digest) and cost one integer add per
    # vehicle-step; the record itself is opt-in.
    _mob_oracle = bool(cfg.emit_mobility_oracle)
    gt_mobility: list[dict] = []
    #: scalar vehicle-step tallies: simulated (every active station, every step), broadcast (the
    #: ones that actually put a CAM on the air), enforced-out (skipped by the CRL), emitted (rows in
    #: gt_emissions_sample after the emit_sample_prob draw).
    surv = {"sim": 0, "bcast": 0, "enforced": 0, "emit": 0}
    #: vid -> [first_t_present, last_t_present, n_steps_present, first_t_bcast, last_t_bcast,
    #:         n_steps_bcast]. The mean RECORD SPAN of a revoked vehicle against a never-revoked one
    #: is the single number that shows the truncation is not marginal per vehicle.
    surv_span: dict[int, list] = {}
    # DENM (event-message) records. gt_denm carries the ORACLE real-vs-fake flag (label side only);
    # ma_denm_log is the MA-VISIBLE log of observed DENMs (no real/fake flag) that featurize turns into
    # leakage-safe per-subject DENM-count features. Both stay empty (and unwritten) on the default path.
    gt_denm: list[dict] = []
    ma_denm_log: list[dict] = []
    _denm_prev_v: dict[int, float] = {}   # per-vehicle previous true speed -> benign hard-brake trigger
    ma_investigations: list[R.MaInvestigation] = []
    ma_crl_events: list[R.MaCrlEvent] = []
    gt_linkage_rev: list[R.GtLinkageRevocation] = []
    # THE SAME LIST OBJECT the security layer's verifier holds, when there is one. Not a copy: a
    # revocation is `crl_entries.append(...)` mid-run, and a verifier handed a snapshot would keep
    # accepting the revoked certificate for the rest of the run. This is what makes "an attacker
    # presenting a revoked certificate" a RECEIVER-side verdict (`CERT_REVOKED`, recomputed from the
    # published linkage seeds) rather than engine-side vid bookkeeping.
    crl_entries: list[CrlLinkageEntry] = [] if _sec is None else _sec.crl
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
        scale = mag_scale.get(typ, 1.0)                      # per-type magnitude multiplier (1.0 = default)
        k = cfg.attack_intensity * scale                     # fold the per-type scale into the global dial:
        # every "* k" branch below is now scaled per-type for free, and at scale==1.0 k is unchanged.
        # The rng draws (r.uniform(...)) happen BEFORE the "* k" multiply, so folding scale into k never
        # alters the rng stream OR the draw count -> the default (scale 1.0) stays byte-identical.
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
            # NOT k-scaled historically (an absolute claimed speed, not a "* k" magnitude). The uniform
            # draw is taken UNCONDITIONALLY (same rng-draw count for any scale); the per-type scale then
            # widens/narrows the claimed speed's DEVIATION from the true speed. At scale==1.0 cs is the
            # exact historic r.uniform(0, 40) -> byte-identical (the branch avoids any float re-rounding).
            u = r.uniform(0, 40)
            cs = u if scale == 1.0 else max(0.0, mspeed + (u - mspeed) * scale)
        elif typ == "StopAndGo":
            # NOT k-scaled historically (fixed 0/35 alternation, no rng draw). The per-type scale widens/
            # narrows each phase's deviation from the true speed. At scale==1.0 cs is the exact historic
            # 0.0 / 35.0 -> byte-identical.
            go = 0.0 if int(t) % 2 == 0 else 35.0
            cs = go if scale == 1.0 else max(0.0, mspeed + (go - mspeed) * scale)
        elif typ == "ReversedHeading":
            # Left UNSCALED: the falsification is a semantic 180 deg reversal, not an amplitude -- a
            # "partial" reversal would not be a reversal, so there is no magnitude to scale.
            ch = (mheading + 180.0) % 360.0
        elif typ == "HeadingOffset":
            ch = (mheading + 45.0 * k) % 360.0
        elif typ == "DataReplay":
            # Left UNSCALED: it REPLAYS real historical state verbatim; the falsification is the staleness
            # (a stored past fix presented as current), not an amplitude, so there is nothing to scale.
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

    # ---- DETECTION LAYER: resolved and constructed ONCE, before step 0 ---------------------------
    # PLUGIN-ARCHITECTURE.md 2.2/8.3. The nine inline detectors and the streak/report_prob block that
    # used to live here are now `Check` and `Fusion` implementations on the registry
    # (`mock_pipeline/detectors.py`), and this is where the run's suite is assembled. Everything that
    # was digest-bearing about the inline form is preserved BY CONSTRUCTION rather than by care:
    #
    #   * `DET_KEYS` is `suite.keys` -- the checks' columns in REGISTRATION order, which is the order
    #     the inline block evaluated them in, including the two gated appends;
    #   * `SOFT_KEYS` is `suite.soft_keys`;
    #   * `MOTION_KEYS` (the VRU suppression list) is `suite.vru_suppressed`, DECLARED per check
    #     (`vru_suppressed = True`) instead of restated as a fourth hand-maintained tuple.
    #
    # A third party's check contributes `detnorm_x_<plugin_id>_<code>` and cannot collide with any of
    # these. Every failure -- an unresolvable ref, a bad param, a reserved capability -- is fatal
    # HERE, before step 0, and must not take the SIGINT path that finalises a valid partial manifest.
    _LEGACY_RNG["rng"] = rng
    try:
        suite = build_checks(cfg, station_types=_emit_station_type, denm=_denm_enabled)
    finally:
        _LEGACY_RNG["rng"] = None
    if _armed:
        _run_sentinel.verify("after LOADING the detection layer")
    DET_KEYS = suite.keys
    SOFT_KEYS = suite.soft_keys
    #: The ORDERED column vocabulary handed to a report format. Order is load-bearing -- it fixes
    #: `detnorm_*` key insertion order and therefore reaches `data_digest` through the canonical
    #: serialisation -- so it is materialised once here rather than rebuilt per report.
    _REPORT_DET_KEYS = (*DET_KEYS, *SOFT_KEYS)
    _DET_ZERO = suite.zero
    _CAM_PLAN, _DENM_PLAN = suite.cam_plan, suite.denm_plan
    _VRU_SUPPRESSED, _SIG_SUPPRESSED = suite.vru_suppressed, suite.sig_suppressed
    _fusion, _fusion_params, _fusion_rng = suite.fusion, suite.fusion_params, suite.fusion_rng
    _fusion_decide = _pguard.guarded(_fusion.decide, suite.fusion_guard)
    _wrap_state = _api_detect.NamespacedState
    #: None for the BUILT-IN fusion, which reads and writes `streak` on the raw per-link dict and
    #: whose access to it is what every pinned golden was recorded on. A THIRD-PARTY fusion gets the
    #: same `NamespacedState` the check slot has always handed a third-party check -- one call
    #: earlier, on the same dict.
    _fusion_wrap = suite.fusion_wrap
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

    def _evidence_for(subject_digest: str) -> tuple:
        """The encoded octets of the last PDU this subject transmitted, or `()`.

        `()` rather than a placeholder: `v2xPduEvidence` is `SEQUENCE (SIZE(1..MAX))`, so an empty
        sequence is not a valid encoding of "no evidence" and a format must be able to tell the
        difference. The colluder path fabricates an accusation about a station it may never have
        heard, and that report legitimately carries no PDU.
        """
        if not _evidence_on:
            return ()
        pdu = _evidence_store.get(subject_digest)
        return (pdu,) if pdu is not None else ()

    def file_report(t, reporter_digest, subject_digest, subject_veh, reasons, det, conf,
                    cx, cy, px, py, malicious, sig_valid=True, station_type="vehicle",
                    rssi_dbm=None, score=None, score_norm=None, latency=0.0):
        # `score` / `score_norm` come from the FUSION's `ReportDecision` on the two detection paths
        # (that is what makes the summary scores the fusion layer's statement rather than the report
        # writer's); the collusion path, which fabricates its own vector with no fusion involved,
        # leaves them None and keeps the historical derivation.
        counters["report"] += 1
        rid = f"rpt_{counters['report']:05d}"
        # THE TWO DELAYS, and they are different quantities.
        #
        # `latency` is the PER-PACKET air-interface latency of the frame this report is about:
        # propagation + access + stack, computed from the real frame length and the receiver's
        # measured CBR. It is when the receiver DETECTED, so it moves `detection_time`.
        #
        # `net_delay_max` is a REPORT-UPLOAD delay -- how long the report takes to reach the MA --
        # and until now it was `rng.uniform(0, 2.0)`, an ingest delay with nothing to do with the
        # channel and two orders of magnitude larger than one. Under `net_latency_model` it is
        # replaced by `ma_backhaul_s`, a declared constant: the uniform draw is not made at all, so
        # this path stops consuming from the global stream (which is exactly why the feature has to
        # be opt-in -- removing a draw moves every downstream number, and that is a different run,
        # not a corrupted one).
        if _lat_on:
            det_t = t + latency
            ingest_t = det_t + cfg.ma_backhaul_s
        else:
            det_t = t
            ingest_t = t + rng.uniform(0.0, cfg.net_delay_max)
        rep_veh = digest_to_vehicle[reporter_digest]
        if score is None:
            score = det.get(reasons[0], 1.0)
        if score_norm is None:
            score_norm = max(det.values()) if det else 1.0
        if _report_fmt is not None:
            # THE REPORT-FORMAT SEAM. Everything below in the historic branch is `ma_report_v1`'s
            # body; a format plugin renders the same MA-VISIBLE input its own way. The input carries
            # `evidence_pdus` -- the real octets the codec put on the air for this subject -- so a
            # format that wants a structurally valid TS 103 759 `v2xPduEvidence` has the bytes for
            # it, which is the plumbing the previous stage identified as missing.
            row = dict(_report_fmt.render(_api_report.ReportInput(
                report_id=rid, ingest_time=ingest_t, detection_time=det_t, generation_time=t,
                reporter_cert_digest=reporter_digest, subject_cert_digest=subject_digest,
                reason_codes=tuple(reasons), round_detection=_lat_on,
                detector_scores=dict(det), detector_keys=_REPORT_DET_KEYS,
                score=score, score_norm=score_norm, subject_pos_confidence=conf,
                sig_valid=bool(sig_valid), cert_crl_status="active",
                station_type=(station_type if _emit_station_type else None),
                rssi_dbm=rssi_dbm, emit_rssi=_emit_rssi,
                st_bbox=(min(cx, px), min(cy, py), max(cx, px), max(cy, py)),
                st_tstart=t, st_tend=t, duplicate_flag=False,
                evidence_msg_refs=(f"{rid}-m",),
                evidence_pdus=tuple(_evidence_for(subject_digest)),
                evidence_profile_id=(getattr(_codec, "profile_id", None)
                                     if _codec is not None else None),
                cert_validity={"sig_valid": True, "not_expired": True, "not_revoked": True,
                               "chain_ok": True})))
            missing = [k for k in _api_report.REQUIRED_ROW_KEYS if k not in row]
            if missing:
                raise ConfigError(
                    f"plugins.report_format {cfg.report_format or '(plugin)'!r} rendered a row "
                    f"missing {missing}; the engine reads {list(_api_report.REQUIRED_ROW_KEYS)} "
                    f"back off every row to sort and stream it, so a format that omits them does "
                    f"not produce an unusual dataset, it produces a KeyError at write time")
        else:
            row = R.MaReport(
                report_id=rid, ingest_time=round(ingest_t, 3),
                # NOT rounded on the legacy path: `t` is written verbatim there and `round(t, 6)` is
                # a different float for a dt that does not divide 1.0 exactly (t =
                # 0.30000000000000004 rounds to 0.3), which would move the digest of any sub-second
                # default run.
                detection_time=(round(det_t, 6) if _lat_on else t),
                generation_time=t,
                reporter_cert_digest=reporter_digest, subject_cert_digest=subject_digest,
                reason_codes=reasons,
                detector_outputs=[{"check_id": reasons[0], "score": round(score, 3),
                                   "verdict": "fail"}],
                cert_validity={"sig_valid": True, "not_expired": True, "not_revoked": True,
                               "chain_ok": True},
                evidence_msg_refs=[f"{rid}-m"],
                st_bbox=[min(cx, px), min(cy, py), max(cx, px), max(cy, py)],
                st_tstart=t, st_tend=t, duplicate_flag=False).to_dict()
            row["detector_score"] = round(score, 3)
            row["detector_score_norm"] = round(score_norm, 3)
            row["subject_pos_confidence"] = round(conf, 3)
            row["cert_crl_status"] = "active"
            row["sig_valid"] = bool(sig_valid)
            if _emit_station_type:                       # MA-VISIBLE self-declared station type on the
                row["station_type"] = station_type      # subject's beacon; key absent by default (byte-identical)
            if _emit_rssi:
                # MA-VISIBLE received-signal strength of the subject's frame, in dBm. Legitimately
                # measurable by the receiver PHY, so it is NOT ground truth and NOT in
                # FORBIDDEN_FEATURE_KEYS -- but it is computed from the subject's TRUE position on
                # the channel side (GeometricChannel.evaluate), never from the position the subject
                # CLAIMS. That is the whole point: a Sybil ghost, or any position-falsifying
                # attacker, carries the RSSI of its attacker's real location, so RSSI-vs-claimed-
                # distance is a detector. None can only happen on a path with no received frame; the
                # colluder path below synthesises one from its own true link geometry rather than
                # leaving a NULL that would be a perfect oracle for "this accusation was fabricated".
                row["rssi_dbm"] = None if rssi_dbm is None else round(float(rssi_dbm), 2)
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

    def seal(b: dict, t: float, *, attack: str = "", replay_of=None) -> dict:
        """Put ONE broadcast on the wire: encode it, sign it, and record what it weighs.

        This is the single site where a message stops being a dict and becomes octets. Everything
        downstream -- airtime, CBR, collision loss, per-packet latency -- reads `b["wire_bytes"]`,
        so a codec that says a CAM is 41 octets and one that says 300 changes the CHANNEL, not just
        a manifest field. With neither a codec nor a security layer the function adds one dict
        lookup and returns, which is what keeps the default path free.

        Order matters: ENCODE, then SIGN THE ENCODED OCTETS. 1609.2 `SignedDataPayload` carries the
        facilities-layer PDU, so signing anything else would sign a thing that is not on the wire.
        """
        nonlocal _wire_size_sum, _wire_size_n
        if _wire is None and _sec is None:
            return b
        payload, claim = None, None
        if _wire is not None:
            payload, size, claim = _wire.encode(b)
            b["wire_bytes"] = size
            if _evidence_on:
                # These are exactly the octets that crossed the air, so a report carrying them is
                # carrying evidence rather than a reference to evidence. Keyed on the PSEUDONYM, so
                # a rotation legitimately loses the old identity's evidence -- which is what a real
                # receiver would also experience.
                _evidence_store[b["digest"]] = payload
        if _sec is not None:
            if payload is None:
                # No codec selected but security is on: sign the engine's own canonical bytes, so
                # the signature still covers the message CONTENT rather than a placeholder.
                payload = ca.canonical_bytes({
                    "d": b["digest"], "t": round(float(b["cg"]), 6),
                    "x": round(float(b["cx"]), 6), "y": round(float(b["cy"]), 6),
                    "v": round(float(b["cs"]), 6), "h": round(float(b["ch"]), 6),
                    "st": b.get("station_type", "vehicle"), "mt": b.get("msg_type", "cam")})
            if replay_of is not None:
                sb = _sec.replay_of(replay_of)
            else:
                sb = _sec.sign(f"veh_{b['veh'].vid:03d}", b["digest"], payload, t,
                               msg_type=WireEncoder.wire_msg_type(b), attack=attack,
                               claimed_gen_time=b["cg"])
            if sb is not None:
                b["sec"] = sb
                # WHICH LENGTH THE CHANNEL IS CHARGED, and it is deliberately NOT
                # `len(sb.message.wire_octets())`.
                #
                # The signature is real; the SERIALISATION around it is this repository's own
                # canonical, length-prefixed byte string, and `certificate.py` says in as many
                # words that it is NOT TS 103 097 COER. Measured on this run it comes out ~200 B
                # for an AT certificate against the 132 B that `pycrate` produces for the real COER
                # structure -- a 51 % over-estimate that would land straight in CBR and in every
                # collision probability derived from it.
                #
                # So: the security layer decides WHICH ARM the frame carries (the standard's
                # once-per-second attachment rule, not a config field), and the CODEC supplies the
                # SIZE of that arm from `SECURITY_ENVELOPE_BYTES`, which was measured against a real
                # COER encoder. Real cryptography, measured airtime, and neither borrowing the
                # other's error. With no codec there is nothing better to ask, so the object's own
                # length stands -- and is over-stated by that same margin.
                b["wire_bytes"] = (_wire.size_for(claim, sb.signer_form) if claim is not None
                                   else sb.wire_bytes)
        _bw = b.get("wire_bytes")
        if _bw:
            _wire_size_sum += _bw * int(b.get("msg_count", 1))
            _wire_size_n += int(b.get("msg_count", 1))
        return b

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
        # `seq` is the DENM `ActionID.sequenceNumber`, which the ETSI codec puts on the wire and a
        # receiver uses to correlate an update with the original event. Derived from the run-global
        # DENM counter, so it is stable, MA-visible and never a function of ground truth.
        broadcasts.append(seal(dict(
            veh=tx, digest=b_cam["digest"], cx=ev_x, cy=ev_y, cs=ev_spd, ch=b_cam["ch"],
            conf=b_cam["conf"], ghost=False, x=b_cam["x"], y=b_cam["y"], falsified=(not real),
            msg_count=1, cg=t, sig_ok=True, cvf=b_cam["cvf"], cvt=b_cam["cvt"],
            station_type=b_cam["station_type"], msg_type="denm", event_type=event_type,
            denm_id=did, seq=counters["denm"] % 65536), t))
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
    stream_counts = {"reports": 0, "labels": 0, "emit": 0, "mob": 0}
    fh_rep = fh_lbl = fh_emit = fh_mob = None
    # ---- WITHHOLDING, when a third-party detector is running out of process --------------------- #
    # An isolated check is an ordinary OS process with ordinary read access to this directory, so
    # streaming the ORACLE files while it runs hands it the answer key for the run it is being graded
    # on -- measured, and the whole reason `docs/realism/ISOLATION-ORACLE-LEAK.md` exists. While any
    # worker is alive the two ground-truth streams go to `_WithheldStream`s and are laid down only
    # after the last one has been reaped. `ma/ma_reports.jsonl` is NOT withheld: it is MA-VISIBLE, it
    # is the detector's own output rather than the answer, and it is the file the GUI tails.
    withheld: list = []
    _withhold_budget = [WITHHELD_MEMORY_BYTES]
    if stream:
        os.makedirs(os.path.join(cfg.out_dir, "ma"), exist_ok=True)
        fh_rep = open(os.path.join(cfg.out_dir, "ma", "ma_reports.jsonl"), "w", encoding="utf-8", newline="\n")
        _lbl_path = os.path.join(cfg.out_dir, "ground_truth", "gt_report_labels.jsonl")
        _emit_path = os.path.join(cfg.out_dir, "ground_truth", "gt_emissions_sample.jsonl")
        # The ORACLE mobility record is a ground-truth stream like the other two, so it takes the
        # SAME withholding path: an isolated third-party detector must not be able to read the
        # un-enforced trajectory of the run it is being graded on any more than it can read the
        # answer key. Opened only when the opt-in is on -> the default run touches nothing here.
        _mob_path = os.path.join(cfg.out_dir, "ground_truth", "gt_mobility_oracle.jsonl")
        if suite.workers:
            fh_lbl = _WithheldStream(_lbl_path, _withhold_budget)
            fh_emit = _WithheldStream(_emit_path, _withhold_budget)
            withheld = [fh_lbl, fh_emit]
            if _mob_oracle:
                fh_mob = _WithheldStream(_mob_path, _withhold_budget)
                withheld.append(fh_mob)
        else:
            os.makedirs(os.path.join(cfg.out_dir, "ground_truth"), exist_ok=True)
            fh_lbl = open(_lbl_path, "w", encoding="utf-8", newline="\n")
            fh_emit = open(_emit_path, "w", encoding="utf-8", newline="\n")
            if _mob_oracle:
                fh_mob = open(_mob_path, "w", encoding="utf-8", newline="\n")

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
        if fh_mob is not None:
            for row in gt_mobility:
                fh_mob.write(ca.canonical_bytes(row).decode("utf-8") + "\n")
            stream_counts["mob"] += len(gt_mobility)
            gt_mobility.clear()

    def prune_state(step_now: int, active: dict) -> None:
        cutoff = step_now - cfg.state_prune_ttl
        for k in [k for k, st in last_claimed.items() if st.get("touch", -1) < cutoff]:
            del last_claimed[k]
        for d in [d for d in subj_events if pseudonym_info[d]["veh_vid"] not in active]:
            subj_events.pop(d, None)
        # per-link channel state (AR(1) shadowing + per-packet streams) is O(links seen); drop links
        # whose endpoints have BOTH despawned. A despawned vehicle never transmits again, so the
        # dropped state can never be consulted -> determinism-safe. `prune` is an optional part of
        # the interface (a stateless model does nothing here), so this is now one call instead of
        # the engine reaching into a specific model's private dicts.
        if _chan_prune is not None:
            _chan_prune(active)

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
    # Under SUMO replay the IDM integrator is OFF: SUMO already did the car-following (and the
    # signal control, and the gap acceptance, and the lane changes), and re-integrating on top of a
    # frozen trajectory would be two models fighting over the same state.
    cf_active = bool(cfg.traffic_flow and cfg.car_following and net is not None) and _replay is None
    _CF_CELL = max(cfg.idm_lookahead_m, 30.0)
    _lights = bool(cfg.traffic_lights and net is not None)
    _turn = bool(cfg.turn_slowdown and net is not None)
    _half_cycle = max(1.0, cfg.light_cycle_s / 2.0)
    # REAL imported <tlLogic> programs (opt-in; see PipelineConfig.real_signals). `_real` is the only
    # thing that switches on the movement-aware path below; with it off, `net.signal_plan` is the
    # class attribute None, `_sig` collapses to `_lights`, and every branch is the one that was
    # there before, in the same order, drawing the same nothing.
    _real = bool(cfg.real_signals and net is not None and net.signal_plan is not None)
    _sig = _lights or _real
    # 'g' (green, give way) / 's' (green right-arrow, stop first) / 'o' (dark). Bound here rather
    # than imported at module scope so a default run never imports `signals` at all.
    _is_permissive = None
    if _real:
        from .signals import is_permissive as _is_permissive        # noqa: PLC0415
    # Per-vehicle-step tally of what the signals actually SERVED, so a run can be judged on the
    # colours it delivered rather than on the flag being set. Manifest-only (`counts`, which
    # `_data_digest` excludes by construction), integer, and never read back into the simulation.
    _sig_served = {"G": 0, "g": 0, "y": 0, "r": 0, "toy_green": 0, "toy_red": 0, "none": 0,
                   "dilemma_go": 0, "permissive_yield": 0}
    # gap-acceptance at UNsignalized intersections: opt-in, and (like _turn) a no-op unless the world
    # supports it (routed car-following). It governs unsignalized nodes only, so with traffic_lights on
    # (every node signalized) it defers entirely to the signal -> effectively active only when _lights is
    # off. When off, none of the yield code below is reached and NO rng is drawn -> byte-identical output.
    # WITH REAL SIGNALS it stops being vacuous under `_lights`: an imported program hands out
    # PERMISSIVE greens ('g' -- 303 of InTAS's 1489 green characters), and a permissive green is
    # precisely "proceed, but give way exactly as you would with no signal at all". So the claims
    # are computed whenever a real plan is attached, and consumed only by the movements the program
    # does not protect (`_permissive_yield` below).
    _gap = bool(cf_active and cfg.gap_acceptance and (not _lights or _real))
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
        # ---- REAL <tlLogic>: resolve each vehicle's own MOVEMENT colour once, then work out which
        # PERMISSIVE greens have to give way ------------------------------------------------------
        # Two passes and not one, for the same reason gap-acceptance is two: the answer depends on
        # the other vehicles, so it must be computed from the frozen start-of-step `snap` or it
        # would depend on iteration order. Everything here is a pure function of the programs, the
        # geometry and that snapshot -- no rng, on either path. It runs BEFORE gap-acceptance
        # because gap-acceptance has to know which movements a real program already governs.
        sig_state: dict = {}          # vid -> (node, dnode, raw char or None, from_xy, to_xy)
        perm_yield: dict = {}         # vid -> distance to the node, for a permissive green with no gap
        if _real:
            _across_sign = 1.0 if cfg.drive_side == "right" else -1.0
            sclaims: dict = {}
            for w in active_list:
                node, dnode, frm, to = w.trip.next_movement(w.s_pos)
                if node is None or dnode >= cfg.idm_lookahead_m:
                    continue
                ch = net.signal_char(node, frm, to, t)
                sig_state[w.vid] = (node, dnode, ch, frm, to)
                if ch is None:
                    continue                      # no real program here -> the toy/none fallback
                # signed heading change THROUGH the junction: +ve is counter-clockwise, i.e. a LEFT
                # turn in this engine's math-convention heading. A left turn under right-hand
                # traffic is the movement that crosses the oncoming stream; the roles mirror for
                # drive_side="left". `to is None` (the route ends here) reads as straight ahead,
                # which is the conservative answer -- it yields to crossing traffic but not to
                # oncoming.
                turn = 0.0
                if frm is not None and to is not None:
                    _a = math.degrees(math.atan2(node[1] - frm[1], node[0] - frm[0]))
                    _b = math.degrees(math.atan2(to[1] - node[1], to[0] - node[0]))
                    turn = ((_b - _a + 180.0) % 360.0) - 180.0
                sclaims.setdefault((round(node[0], 2), round(node[1], 2)), []).append(
                    (w.vid, ch, snap[w.vid][2], dnode, snap[w.vid][3], turn))
            for lst in sclaims.values():
                prot = [c for c in lst if c[1] == "G"]
                if not prot:
                    continue                      # nobody here has right of way to yield to
                for vid_i, ch_i, h_i, d_i, _v_i, turn_i in lst:
                    if not _is_permissive(ch_i):
                        continue                  # 'G' owns the junction; 'y'/'r' already stop
                    across = (turn_i * _across_sign) > PERMISSIVE_ACROSS_DEG
                    for _vj, _cj, h_j, d_j, v_j, _tj in prot:
                        dh = _ang_diff(h_i, h_j)
                        # crossing traffic always conflicts; the near-OPPOSING stream conflicts only
                        # for the movement that turns across it (a permissive left). Two opposing
                        # THROUGH movements pass side by side and do not.
                        if not (45.0 < dh < 135.0 or (across and dh >= 135.0)):
                            continue
                        if d_j / max(v_j, 0.1) <= PERMISSIVE_CRITICAL_GAP_S:
                            perm_yield[vid_i] = d_i
                            break
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
                if sig_state.get(w.vid, (None, None, None, None, None))[2] is not None:
                    # A REAL program governs this movement, so the junction is not "unsignalized"
                    # for it and the first-come rule must not touch it -- a vehicle on a protected
                    # green would otherwise yield to a closer vehicle sitting at a red. Empty
                    # `sig_state` (real_signals off) makes this a no-op on every existing path.
                    continue
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
            _sig_stopped = False
            if _sig:                                     # stop at a red signal on the next intersection
                if _real:
                    node, dnode, _ch, _frm, _to = sig_state.get(
                        v.vid, (None, math.inf, None, None, None))
                else:
                    node, dnode = v.trip.next_node(v.s_pos)
                    _ch = _frm = _to = None
                if node is not None and dnode < cfg.idm_lookahead_m:
                    if _ch is None:
                        # NO REAL PROGRAM GOVERNS THIS MOVEMENT -- an unsignalised junction, a
                        # junction whose program was not imported, or an approach the program does
                        # not control. Today's behaviour, unchanged: the toy 2-colouring when
                        # `traffic_lights` is on, nothing at all when it is not.
                        if _lights:
                            phase = net.node_phase(node)  # stable 2-colouring (topology-agnostic)
                            axis_x = abs(cosh) >= abs(sinh)   # travelling mostly E-W vs N-S
                            _sig_stopped = not _light_green(phase, axis_x, t)
                            _sig_served["toy_red" if _sig_stopped else "toy_green"] += 1
                        else:
                            _sig_served["none"] += 1
                    elif _ch in ("r", "u"):              # red, and red+yellow ("do not enter" yet)
                        _sig_stopped = True
                        _sig_served["r"] += 1
                    elif _ch == "y":
                        # DILEMMA ZONE. Stop if it can still be done at this driver's comfortable
                        # deceleration; otherwise clear the junction, which is what a real driver
                        # does and what keeps yellow from manufacturing IDM-floor emergency stops.
                        _sig_stopped = (not YELLOW_DILEMMA) or (
                            dnode >= v.cur_v * v.cur_v / (2.0 * max(0.5, v.idm_b)))
                        _sig_served["y"] += 1
                        if not _sig_stopped:
                            _sig_served["dilemma_go"] += 1
                    elif _ch == "G":                     # protected: this movement owns the junction
                        _sig_served["G"] += 1
                    else:                                # 'g'/'s'/'o': green, but must give way
                        _sig_served["g"] += 1
                        if v.vid in perm_yield:
                            _sig_stopped = True
                            _sig_served["permissive_yield"] += 1
                    if _sig_stopped:
                        stop_gap = max(0.0, dnode - 2.0)  # halt ~2 m before the stop line
                        if stop_gap < best_gap:
                            best_gap, best_v, best_len = stop_gap, 0.0, 0.0
                    if SIGNAL_HOOK is not None and _ch is not None:   # telemetry seam (None default)
                        SIGNAL_HOOK(dict(vid=v.vid, t=round(t, 3), node=list(node),
                                         frm=(None if _frm is None else list(_frm)),
                                         to=(None if _to is None else list(_to)),
                                         dnode=round(dnode, 3), char=_ch,
                                         stopped=bool(_sig_stopped)))
            if (not _sig_stopped) and _gap and v.vid in gap_stop:
                # yield to conflicting cross-traffic at an UNSIGNALIZED node. Unreachable while the
                # toy signals are on and no real plan is attached (`_gap` is False there), which is
                # exactly the pre-existing `elif`; with a real plan attached it governs the
                # junctions the imported programs do not.
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
        elif _replay is not None:
            # THE MOBILITY SEAM, in one line and in exactly the place `car_follow` occupies: the
            # provider writes cur_x / cur_y / cur_v / cur_h / s_pos from the frozen trace, and the
            # entire rest of the step -- broadcast pre-pass, channel, detectors, MA, reporting --
            # is untouched code reading untouched fields.
            _replay.advance(active_list, step, t)

        # ---- THE UN-ENFORCED MOBILITY RECORD, taken HERE and not one line later ------------------
        # This is the seam that matters. Everything below it -- the broadcast pre-pass, the channel,
        # the detectors, the MA -- runs on what the SCMS layer permits, and the pre-pass's very first
        # statement is `if enforced(tx, t): continue`. Reading traffic realism off anything downstream
        # of that line measures ENFORCEMENT, not traffic: the peak-hour dataset keeps 43.78% of the
        # vehicle-steps it simulated, and it is mostly FALSE revocations (precision 0.308) deleting
        # them. So the mobility is recorded from up here, where the only thing that has happened to
        # `active_list` is that it moved.
        #
        # NO RNG IS DRAWN and no state is mutated: `true_state(t)` is a pure read of the position the
        # mobility provider just wrote (`cur_*` under car-following/replay, a closed form otherwise),
        # and it is the identical call the pre-pass makes a few lines down. The tallies are integer
        # adds. Both properties are what make this insertion byte-identical on the default path.
        surv["sim"] += len(active_list)
        for _v in active_list:
            _sp = surv_span.get(_v.vid)
            if _sp is None:
                surv_span[_v.vid] = [t, t, 1, None, None, 0]
            else:
                _sp[1] = t
                _sp[2] += 1
        if _mob_oracle:
            _mob_base = stream_counts["mob"] + len(gt_mobility)
            for _i, _v in enumerate(active_list):
                _mx, _my, _mv, _mh = _v.true_state(t)
                gt_mobility.append(dict(
                    mob_id=f"mob_{_mob_base + _i:09d}", t=round(t, 3),
                    true_vehicle_id=f"veh_{_v.vid:03d}",
                    true_x=round(_mx, 3), true_y=round(_my, 3),
                    true_speed=round(_mv, 3), true_heading=round(_mh, 3),
                    # WHY these two flags ride along: they are what lets a consumer reconstruct the
                    # truncated stream exactly (filter on `broadcasting`) and measure the loss
                    # without a second file. `revoked` is the CRL state, `broadcasting` is the
                    # stricter thing the pre-pass actually tests (CRL propagation delay included).
                    revoked=bool(_v.revoked and _v.revocation_time is not None
                                 and t >= _v.revocation_time),
                    broadcasting=(not enforced(_v, t)),
                    is_vru=bool(_v.is_vru), _visibility=R.ORACLE))

        # PRE-PASS: every active broadcast this step (real pseudonyms + sybil ghosts)
        broadcasts: list[dict] = []
        for tx in active_list:
            if enforced(tx, t):
                surv["enforced"] += 1
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
            _replay_src = None                                    # a captured frame to re-emit verbatim
            if attacking:
                cx, cy, cs, ch = attack_claim(tx, t, mx, my, tspeed, theading)
                if tx.attack_type == "DoS":
                    msg_count = cfg.dos_burst                     # flood the channel
                elif tx.attack_type == "DelayedMessages":
                    cg = t - cfg.delay_s                          # stale timestamp
                elif tx.attack_type == "DataReplay":
                    if _sec is None:
                        cg = t - 5.0 * cfg.dt                     # replayed frame carries its old gen time
                    else:
                        # REAL REPLAY: re-emit a frame this attacker actually captured, octet for
                        # octet, ORIGINAL SIGNATURE INCLUDED. The signature is VALID -- a legitimate
                        # key made it over exactly these bytes -- and that is the correct model. A
                        # replay is a FRESHNESS failure caught by `staleOrReplay` against
                        # `generationTime`, never a crypto failure, and a scheme that flagged it as
                        # one would be wrong about what signatures do. The claim fields are restored
                        # from the capture too, so the dict the detectors read and the octets on the
                        # wire agree -- which the "edit the timestamp and keep today's position"
                        # form did not.
                        _captured = _sec_last_frame.get(tx.vid)
                        if _captured is None:
                            _sec_refusals["DataReplay_no_captured_frame"] += 1
                            cg = t - 5.0 * cfg.dt
                        else:
                            _replay_src, cx, cy, cs, ch, cg = _captured
                elif tx.attack_type == "OutOfOrder":
                    cg = t - vrng[tx.vid].uniform(cfg.delay_s, 2.0 * cfg.delay_s)  # non-monotonic gen time
                elif tx.attack_type == "DoSRandom":
                    msg_count = cfg.dos_burst                     # flood + random content (set in claim)
                elif tx.attack_type == "InvalidSignature":
                    # `sig_ok` here is the attacker's INTENT, and it is what the oracle `falsified`
                    # label keys on. Under `security_model="ecdsa"` it is no longer what the
                    # receiver believes: the frame is really signed with a key the certificate does
                    # not name (`secured.sign_with_foreign_key`), and the `sig_ok` a receiver acts
                    # on is the ECDSA verdict computed at reception. The two agreeing is a
                    # measurable property of the run, not an assumption.
                    sig_ok = False                                # forged / tampered message
                elif tx.attack_type == "ExpiredCert":
                    if _sec is None:
                        cvt = t - 5.0                             # reuse a cert past its validity
                    else:
                        # REAL: the validity period is a SIGNED certificate field, so an attacker
                        # cannot edit it -- it can only present a certificate it genuinely holds
                        # whose window has passed, i.e. one of its own earlier pseudonyms. An
                        # attacker still inside its first rotation period has none and therefore
                        # CANNOT mount this attack; the refusal is counted, not papered over.
                        _cred = _sec.stale_credential(f"veh_{tx.vid:03d}", t)
                        if _cred is None:
                            _sec_refusals["ExpiredCert_no_stale_credential"] += 1
                        else:
                            digest = _cred.digest
                            cvf, cvt = _cred.valid_from, _cred.valid_to
                elif tx.attack_type == "NotYetValid":
                    if _sec is None:
                        cvf = t + 5.0                             # present a not-yet-valid cert
                    else:
                        _cred = _sec.future_credential(f"veh_{tx.vid:03d}", t)
                        if _cred is None:
                            _sec_refusals["NotYetValid_no_future_credential"] += 1
                        else:
                            digest = _cred.digest
                            cvf, cvt = _cred.valid_from, _cred.valid_to
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
            # tspd/thdg are the TRUE kinematics already computed above (tx.true_state(t)); they ride
            # along on the broadcast purely so the ground-truth emission sampler can record them
            # without recomputing (ADR 0002). They are ORACLE values: nothing on the reception /
            # detection path reads them, and they never enter an MA-visible row.
            b_cam = dict(veh=tx, digest=digest, cx=cx, cy=cy, cs=cs, ch=ch, conf=conf,
                         ghost=False, x=x, y=y, falsified=falsified, msg_count=msg_count,
                         cg=cg, sig_ok=sig_ok, cvf=cvf, cvt=cvt, station_type=declared_station,
                         tspd=tspeed, thdg=theading)
            # ---- EN 302 637-2 CAM GENERATION (opt-in) --------------------------------------- #
            # Evaluated on the station's EGO STATE -- its true position, speed and heading -- and
            # deliberately NOT on either of the two alternatives:
            #
            #  * not on the FALSIFIED claim, because the CAM service is a facilities-layer timer
            #    that runs the same whether the application above it is lying. Keying the cadence
            #    on the attack would make the message RATE a detector for the attack: a leak, not
            #    a feature.
            #  * not on the MEASURED (GNSS-noisy) fix. Measured here first, then rejected: with
            #    `measure()`'s per-step noise the 4 m position trigger fires on the NOISE rather
            #    than on the movement -- 1548 of 2806 CAMs at dt=0.1, a 0.168 s mean gap and a
            #    5.94 Hz rate against the Java engine's 0.340 s / 2.94 Hz on the same rules. A real
            #    station's ego position comes from a GNSS/INS fusion whose output is smoothed, and
            #    the reference implementation triggers on the exact position, so the noisy fix is
            #    both less realistic and not comparable. The residual is stated rather than hidden:
            #    this engine has no ego-state estimator, so "true position" is standing in for the
            #    output of one.
            _emit_cam = True
            if _cam_state is not None:
                _cs_st = _cam_state.get(tx.vid)
                if _cs_st is None:
                    _cs_st = _profile.new_generation_state()
                    _cam_state[tx.vid] = _cs_st
                # 0.0 means "no congestion control is active". A generation rule applies its own
                # floor on top (the built-in's `max(T_GenCamMin, 0.0)` is T_GenCamMin), which is
                # what lets the engine stop knowing what any particular standard's floor is.
                _floor = 0.0
                if _dcc_state is not None:
                    _dc = _dcc_state.get(tx.vid)
                    if _dc is None:
                        _dc = _profile.new_congestion_state()
                        _dcc_state[tx.vid] = _dc
                    # The CBR this station's OWN receiver measured last step. A station that has
                    # heard nothing sits in the controller's idle state, which for reactive DCC
                    # permits T_off == T_GenCamMin -- i.e. nothing the CAM service was not under.
                    _dc.update(_cbr_measured.get(tx.vid, 0.0))
                    _floor = _dc.min_interval_s()
                _reason = _cs_st.evaluate(_api_profile.GenerationInput(
                    t=t, x=x, y=y, speed=tspeed, heading=theading, dt=cfg.dt,
                    min_interval_s=_floor, is_rsu=bool(tx.is_rsu),
                    station_type=b_cam["station_type"]))
                _emit_cam = bool(_reason)
                if _reason:
                    _cam_triggers[_reason] += 1
                    _prev_t = _cam_last_t.get(tx.vid)
                    if _prev_t is not None:
                        # NOT `_gap`: that name is the run-level GAP-ACCEPTANCE flag
                        # (`cf_active and cfg.gap_acceptance and ...`), read by `car_follow` on
                        # every subsequent step. Binding a float to it here switched unsignalised
                        # yielding ON mid-run and silently changed the mobility -- caught by the
                        # "cam_generation_rules must be a no-op at dt = 1.0" test, which is exactly
                        # what that test is for.
                        _cam_gap = t - _prev_t
                        _cam_gap_sum += _cam_gap
                        _cam_gap_n += 1
                        if _cam_gap > _cam_gap_max:
                            _cam_gap_max = _cam_gap
                    _cam_last_t[tx.vid] = t
            if not _emit_cam:
                # No CAM this step. DENMs are event-triggered and are NOT gated by the CAM
                # service's timer, so the DENM block below still runs -- which is why this is a
                # flag rather than a `continue`.
                b_cam["suppressed"] = True
            else:
                seal(b_cam, t, attack=(tx.attack_type if attacking else ""),
                     replay_of=_replay_src)
                if (_sec is not None and tx.attack_type == "DataReplay"
                        and _replay_src is None and b_cam.get("sec") is not None):
                    # The capture. `setdefault` keeps the FIRST frame this station ever sent, which
                    # -- because `attack_delay_s` is 2 s by default -- is an honest one it really
                    # transmitted. That is what an eavesdropper would have recorded.
                    _sec_last_frame.setdefault(tx.vid, (b_cam["sec"], cx, cy, cs, ch, cg))
                broadcasts.append(b_cam)
                # SURVIVORSHIP: this vehicle-step made it onto the air, so it is one the emission
                # stream can carry. Counted HERE rather than as `sim - enforced` because the
                # pre-pass has a second exit above (a GNSS outage goes silent without being
                # revoked), and conflating a radio-silent step with an enforced one would
                # understate enforcement's own share. Under `cam_generation_rules` a step where the
                # CAM service did not fire is likewise not a step on the air, so it is not counted
                # -- which keeps `bcast` the count of vehicle-steps `gt_emissions` can sample from.
                surv["bcast"] += 1
                _sp = surv_span.get(tx.vid)
                if _sp is not None:
                    if _sp[3] is None:
                        _sp[3] = t
                    _sp[4] = t
                    _sp[5] += 1
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
            if attacking and tx.attack_type == "Sybil" and _emit_cam:
                sr = vrng[tx.vid]
                for gdig in tx.ghosts:
                    cert_first_seen.setdefault(gdig, t)
                    cert_last_seen[gdig] = t
                    _bg = dict(veh=tx, digest=gdig, cx=cx + sr.uniform(-1, 1),
                               cy=cy + sr.uniform(-1, 1), cs=cs, ch=ch, conf=conf,
                               ghost=True, x=x, y=y, falsified=True, msg_count=1,
                               cg=t, sig_ok=True, cvf=0.0, cvt=total_time,
                               station_type="vehicle")
                    # A ghost is a REAL extra pseudonym of the attacker under `security_model=
                    # "ecdsa"` (provisioned as extra j-indices in `make_vehicle`), so it signs with
                    # a genuine, verifiable credential -- which is exactly why co-location, not
                    # cryptography, is what catches a Sybil.
                    broadcasts.append(seal(_bg, t))

        # per-message ground-truth emission sampling (real CAM broadcasts only; DENMs have their own
        # dedicated ground-truth stream gt_denm, so they are excluded here)
        for b in broadcasts:
            if b["ghost"] or b.get("msg_type") == "denm":
                continue
            if rng.random() < cfg.emit_sample_prob:
                tx = b["veh"]
                # ADR 0002: true_speed (m/s) and true_heading (deg) are the simulator's OWN kinematic
                # state at emission time, written verbatim -- NOT reconstructed by differencing
                # true_x/true_y, which conflates a lane change with an acceleration. ORACLE-only
                # (both names are in records.FORBIDDEN_FEATURE_KEYS). heading convention is the
                # engine's native one, degrees CCW from +x (East), [0, 360) -- see true_state() and
                # manifest.conventions.heading. No RNG draw and no reordering: the values were
                # already computed for this broadcast.
                gt_emissions.append(dict(
                    emit_id=f"emt_{stream_counts['emit'] + len(gt_emissions):08d}", t=round(t, 3),
                    true_vehicle_id=f"veh_{tx.vid:03d}", true_x=round(b["x"], 3), true_y=round(b["y"], 3),
                    true_speed=round(b["tspd"], 3), true_heading=round(b["thdg"], 3),
                    claimed_x=round(b["cx"], 3), claimed_y=round(b["cy"], 3), claimed_speed=round(b["cs"], 3),
                    pos_conf=round(b["conf"], 3), is_attacker=tx.is_attacker, is_faulty=tx.is_faulty,
                    falsified=bool(b["falsified"]), _visibility=R.ORACLE))
                surv["emit"] += 1

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
        # ---- the step's CHANNEL FRAME (api.channel.StepFrame) -------------------------------------
        # Station snapshots carry TRUE geometry, which is what makes the channel explicitly INSIDE
        # the oracle boundary: physics must not be steerable by a position-falsifying attacker. They
        # are reused from `rx_pos` wherever it already resolved the position, so no extra
        # `true_state()` call is made for any receiver; only VRUs (transmitters, never receivers)
        # need one. `blocker_h_m` is 0.0 for a VRU (a pedestrian is not an obstruction) and for an
        # RSU (an RSU is a receiver, not a blocker), so the blocker set the geometric model rebuilds
        # from `frame.stations` is entry-for-entry -- and order-for-order -- the list this replaced.
        stations: dict = {}
        for _v in active_list:
            if enforced(_v, t):
                continue
            _p = rx_pos.get(_v.vid) or _v.true_state(t)[:2]
            stations[_v.vid] = StationSnapshot(
                _v.vid, _p[0], _p[1],
                RSU_ANTENNA_HEIGHT_M if _v.is_rsu else V2X_ANTENNA_HEIGHT_M,
                0.0 if _v.is_vru else TR37885_BLOCKER_HEIGHT_M.get(
                    _v.veh_type, TR37885_BLOCKER_HEIGHT_M["car"]),
                _v.is_rsu, _v.is_vru, None, _v.rx_range or 0.0)
        for _r in rsus:
            _p = rx_pos.get(_r.vid)
            if _p is None:
                continue
            stations[_r.vid] = StationSnapshot(_r.vid, _p[0], _p[1], RSU_ANTENNA_HEIGHT_M, 0.0,
                                               True, False, None, _r.rx_range or 0.0)
        # `size_bytes` is the REAL encoded length once a codec is on -- a measured 41-octet UPER CAM
        # inside its measured 93-octet TS 103 097 digest envelope, not the 300 B both engines
        # assumed. That number is what the channel charges airtime for, so selecting a codec changes
        # the CBR a receiver measures and the collisions it suffers, not merely a manifest field.
        transmissions = [Transmission(bi, b["veh"].vid, b.get("msg_type", "cam"), b["msg_count"],
                                      b.get("wire_bytes", NATIVE_WIRE_SIZE_BYTES), b["digest"])
                         for bi, b in enumerate(broadcasts)]
        # index-parallel with `broadcasts`: the SENDER's station snapshot per PDU. Resolved once per
        # step rather than once per candidate link (a Sybil ghost shares its attacker's snapshot,
        # which is exactly right -- the ghost radiates from the attacker's true position).
        tx_snaps = [stations[b["veh"].vid] for b in broadcasts]
        frame = StepFrame(step, t, cfg.dt, stations, transmissions,
                          [rx.vid for rx in receivers if not enforced(rx, t)], wx_loss,
                          _chan_env_ro)
        chan.begin_step(frame)              # advance per-step state EXACTLY ONCE
        suite.begin_step(step)              # detector RNG namespaces: same discipline, same reason
        bcell: dict = {}
        for bi, b in enumerate(broadcasts):
            bcell.setdefault((int(b["x"] // rng_cell), int(b["y"] // rng_cell)), []).append(bi)
        if _chan_batch:
            # ---- D1: ONE EXCHANGE PER STEP, for the whole fleet. --------------------------------
            # This is the shape, and the ONLY shape, in which an out-of-process ns-3 / OMNeT++
            # backend can exist. A mid-size run is ~6 000 link decisions per step over ~3 600 steps
            # (~2.2e7 decisions); at a conservative 2 ms local-socket round trip that is ~12 hours
            # per link and ~7 seconds per step -- about 1e4x. Per-receiver batching would still be
            # ~200 round trips per step (~24 minutes), which is why the candidate windows for EVERY
            # receiver are gathered here, before the receiver loop, and handed over in a single
            # `deliver` call. Outcomes are re-sorted by (rx_vid, tx_index) so the backend's internal
            # ordering is structurally incapable of reaching the digest.
            # Reached only when a BatchChannelModel is declared; the built-ins never take this path.
            _batch_out: dict = {}
            _batch_dist: dict = {}
            _all_cands = []
            for _rx in receivers:
                if enforced(_rx, t):
                    continue
                _rxx, _rxy = rx_pos[_rx.vid]
                _cap = chan.window_m(stations[_rx.vid])
                _rad = max(1, int(math.ceil(_cap / rng_cell)))
                _cx0, _cy0 = int(_rxx // rng_cell), int(_rxy // rng_cell)
                _c = []
                for _dcx in range(-_rad, _rad + 1):
                    for _dcy in range(-_rad, _rad + 1):
                        _c.extend(bcell.get((_cx0 + _dcx, _cy0 + _dcy), ()))
                _c.sort()                       # canonical order: tx_index asc within rx_vid asc
                for _bi in _c:
                    _b = broadcasts[_bi]
                    if _b["veh"].vid == _rx.vid:
                        continue
                    _d = math.hypot(_b["x"] - _rxx, _b["y"] - _rxy)
                    if _d <= _cap:
                        _all_cands.append((_bi, _rx.vid, _d))
                        _batch_dist[(_bi, _rx.vid)] = _d
            for _o in _api_channel.sort_outcomes(chan.deliver(frame, _all_cands)):
                _batch_out.setdefault(_o.rx_vid, []).append(_o)
        for rx in receivers:
            if enforced(rx, t):
                continue
            rxx, rxy = rx_pos[rx.vid]
            rx_snap = stations[rx.vid]
            # per-receiver range: an RSU may reach further than vehicles, so it searches a wider cell
            # window (radius = ceil(range/cell)). Vehicles keep rx_range=0 -> range=radio_range_m ->
            # radius 1 -> the original 3x3 -> byte-identical.
            rr = rx.rx_range or cfg.radio_range_m
            # TWO numbers, deliberately not one (PLUGIN-ARCH 2.1 deviation 2):
            #   cap      = the candidate-SEARCH window. disc: rr. logdistance: rr widened by the
            #              shadow headroom (a favourable shadow can pull a link past rr), bounded so
            #              the cell search stays O(local). geometric: the LINK-BUDGET distance.
            #   rx_reach = the DECLARED DELIVERY reach, which is what acceptanceRangeThreshold bounds
            #              on. disc/logdistance: rr. geometric: the same link-budget distance -- for
            #              that model the reach IS the cap, and keeping rr would flag every honest
            #              long LOS link it legitimately delivers as an impossible claim.
            cap = chan.window_m(rx_snap)
            rx_reach = chan.reach_m_for(rx_snap)
            rad = max(1, int(math.ceil(cap / rng_cell)))
            cx0, cy0 = int(rxx // rng_cell), int(rxy // rng_cell)
            cand = []
            if not _chan_batch:      # the batch path gathered its candidates in the pre-pass above
                for dcx in range(-rad, rad + 1):
                    for dcy in range(-rad, rad + 1):
                        cand.extend(bcell.get((cx0 + dcx, cy0 + dcy), ()))
                cand.sort()
            in_range = []
            link_meta: list = []     # index-parallel with in_range: the model's LinkOutcome per link
            if _chan_batch:
                # the step's single exchange already happened above; read this receiver's share.
                # `_batch_out[rx.vid]` is in (rx_vid, tx_index) order, which IS `cand` order, so
                # `in_range` is built exactly as the per-link path builds it.
                for out in _batch_out.get(rx.vid, ()):
                    in_range.append((broadcasts[out.tx_index], _batch_dist[(out.tx_index, rx.vid)]))
                    link_meta.append(out)
            else:
                evaluate_link = chan.evaluate_link
                for bi in cand:
                    b = broadcasts[bi]
                    if b["veh"].vid == rx.vid:
                        continue
                    d = math.hypot(b["x"] - rxx, b["y"] - rxy)
                    if d > cap:
                        continue                    # cheap cap before any dB math or model call
                    # ONE call, through the resolved model. `disc` re-tests d <= rr and returns the
                    # shared DELIVERED constant; `logdistance` draws its shadow from its own keyed
                    # stream; `geometric` runs classification -> path loss -> AR(1) shadowing ->
                    # per-packet Nakagami fade -> decode floor. None == not delivered.
                    out = evaluate_link(tx_snaps[bi], rx_snap, d, transmissions[bi])
                    if out is not None:
                        in_range.append((b, d))
                        link_meta.append(out)
            load = sum(b["msg_count"] for b, _ in in_range)
            cong = min(0.8, max(0.0, (load - cfg.chan_capacity) / max(1, cfg.chan_capacity)) * 0.5)
            if _wire is None:
                geo_cbr = chan.channel_busy_ratio(rx.vid, load)
            else:
                # CBR FROM REAL AIRTIME. `chan.channel_busy_ratio(rx, load)` multiplies a MESSAGE
                # COUNT by one hard-coded frame time, which is only right when every frame is the
                # same size -- and the whole point of putting a codec on the wire is that they are
                # not (a 41-octet UPER CAM in a digest envelope is 431.5 us; the same CAM with the
                # certificate attached is 599.5 us; a DoS burst is `msg_count` of them). So the
                # engine integrates `frame_airtime_s(size)` over the offered frames itself and hands
                # the result to `collision_loss`, which is the term that actually consumes it.
                # Computed engine-side rather than pushed through the channel interface because
                # `channel_busy_ratio(rx_vid, offered)` takes a count, and reinterpreting its
                # argument as airtime would silently change what a third-party model is being asked.
                _at = 0.0
                for _b, _ in in_range:
                    _at += _b["msg_count"] * _airtime_s(_b.get("wire_bytes",
                                                               NATIVE_WIRE_SIZE_BYTES))
                geo_cbr = _profile.channel_busy_ratio(_at, cfg.dt)
            if _dcc_state is not None:
                # What THIS station will react to next step. Stored per receiver vid, which is the
                # station that measured it -- DCC is a local feedback loop, not a global knob.
                _cbr_measured[rx.vid] = geo_cbr
            # Three float operations per receiver-step, unconditional, so the CBR the run actually
            # experienced is a reported number rather than something a later analysis has to guess.
            # Manifest-only (`_data_digest` excludes manifest.json by construction), so it moves no
            # digest -- the same rule the survivorship counters are recorded under.
            _cbr_sum += geo_cbr
            _cbr_n += 1
            if geo_cbr > _cbr_max:
                _cbr_max = geo_cbr
            reporter_digest = rx.active_pseudonym(t, cfg.rotate_period_s)["digest"]
            for li, (b, dist) in enumerate(in_range):
                rssi_dbm = None
                if not _chan_additive:
                    # INDEPENDENT-SURVIVAL COMPOSITION: p_deliver = prod(1 - p_i). The additive form
                    # kept for the grandfathered built-ins below can exceed 1.0 (roadmap G5); a
                    # product of survival probabilities cannot, and it is the correct composition for
                    # independent impairments. The PHY error is already resolved above (the frame
                    # either cleared the faded decode floor or it never entered in_range), so what
                    # composes here is the MAC/environment loss: hidden-terminal collisions from the
                    # modelled CBR, weather absorption, and the configured baseline. cfg.nlos_loss is
                    # deliberately included so an explicit setting still bites, but it defaults to 0
                    # and setting it under this model DOUBLE-COUNTS the obstruction the geometry
                    # already resolved. The composition is ENGINE-side because the engine owns the
                    # congestion and weather terms; the model declares which composition applies.
                    rssi_dbm = link_meta[li].rssi_dbm
                    p_surv = ((1.0 - min(1.0, max(0.0, cfg.packet_loss_base)))
                              * (1.0 - min(1.0, max(0.0, cfg.nlos_loss * (dist / rr))))
                              * (1.0 - chan.collision_loss(dist, geo_cbr))
                              * (1.0 - min(1.0, max(0.0, wx_loss))))
                    # the delivery coin comes from the LINK's own keyed stream inside the model, so
                    # a model on this composition consumes nothing from the global `rng`
                    if p_surv < 1.0 and chan.delivery_coin(b["veh"].vid, rx.vid) >= p_surv:
                        continue                                # packet dropped on the channel
                else:
                    # GRANDFATHERED (`legacy_global_rng` + `loss_composition:additive_legacy`, both
                    # refused from third parties): the coin comes from the engine's global `rng`,
                    # whose draw count and order are load-bearing for 0bd93655... Closing this is
                    # roadmap phase 6 and needs a deliberate, announced re-pin.
                    loss = cfg.packet_loss_base + cfg.nlos_loss * (dist / rr) + cong + wx_loss
                    if loss > 0 and rng.random() < loss:
                        continue                                # packet dropped on the channel
                # ---- THE FRAME IS DELIVERED. What did it cost, and does it verify? -------------
                # PER-PACKET LATENCY (opt-in). Propagation over the TRUE link distance, plus the
                # access delay this frame's real length and this receiver's measured CBR imply,
                # plus the derived stack constant. Deterministic -- no draw, which is what lets it
                # be switched on without disturbing a single RNG stream.
                lat = 0.0
                if _lat_on:
                    lat = _profile.link_latency_s(
                        dist, geo_cbr, b.get("wire_bytes", NATIVE_WIRE_SIZE_BYTES))
                    _lat_sum += lat
                    _lat_n += 1
                    _lat_hist[int(lat * 1e6)] += 1
                    if lat < _lat_min:
                        _lat_min = lat
                    if lat > _lat_max:
                        _lat_max = lat
                # REAL VERIFICATION (opt-in). `sig_ok` stops being a field the sender set and
                # becomes what THIS receiver concluded, at its own clock, against its own trust
                # store and the live CRL. The certificate is established first and the signature is
                # only evaluated if it survives -- a receiver that has already decided to drop a
                # frame must not pay 36 us of ECDSA on it, which is the whole defence against
                # signature flooding.
                sig_ok_eff = b["sig_ok"]
                if _sec is not None and b.get("sec") is not None:
                    _vr = _sec.verify(b["sec"], t)
                    sig_ok_eff = _vr.sig_ok
                    _verdicts[_vr.status] += 1
                if b.get("msg_type") == "denm":
                    # DENM (event message): the receiver checks whether the announced brake/stationary
                    # hazard CORROBORATES the sender's own observed kinematics. Scored by the checks
                    # that declare `msg_types` containing "denm" (the built-in `denmPlausibility`),
                    # and decided by the fusion's event arm -- a one-shot claim has no history to
                    # streak over. An unverifiable (bad-sig) DENM carries no trustworthy content, so
                    # it is not scored at all.
                    if sig_ok_eff and _DENM_PLAN:
                        # An event message has no per-link history of its own. The sender's CAM
                        # state is used when this receiver already holds one (so a stateful check
                        # sees continuity); otherwise a throwaway dict, NOT a new `last_claimed`
                        # entry -- creating one here would change what the pruner walks.
                        st_d = last_claimed.get((rx.vid, b["digest"]))
                        if st_d is None:
                            st_d = {"h": [], "streak": {}, "touch": step}
                        obs = Observation(
                            b["digest"], b["station_type"], b["cx"], b["cy"], b["cs"], b["ch"],
                            b["conf"], b["cg"], b["msg_count"], "denm", b.get("event_type"),
                            True, b["cvf"], b["cvt"],
                            rxx, rxy, rx_reach, rssi_dbm, link_meta[li].link_state, t, cfg.dt,
                            True, b["cx"], b["cy"], b["cs"], b["ch"], t,
                            b["cx"], b["cy"], b["cs"], b["ch"], t,
                            _offroad(b["cx"], b["cy"]),
                            types.MappingProxyType(
                                {"cell_cert_count": cells[(round(b["cx"] / cfg.sybil_cell_m),
                                                           round(b["cy"] / cfg.sybil_cell_m),
                                                           int(b["ch"] // 45) % 8)],
                                 "cbr": geo_cbr}))
                        det = _DET_ZERO.copy()
                        for _col, _ev, _prm, _rn, _wrap, _prec in _DENM_PLAN:
                            _v = _ev(obs, st_d if _wrap is None else _wrap_state(st_d, _wrap),
                                     _prm, _rn)
                            det[_col] = _v if _prec is None else _round_score(_v, _prec, _col)
                        decision = _fusion_decide(
                            det, st_d if _fusion_wrap is None else _wrap_state(st_d, _fusion_wrap),
                            obs, _fusion_params, _fusion_rng)
                        if decision is not None:
                            file_report(t, reporter_digest, b["digest"], b["veh"],
                                        list(decision.reason_codes), det, b["conf"],
                                        b["cx"], b["cy"], b["cx"], b["cy"], malicious=False,
                                        sig_valid=True, station_type=b["station_type"],
                                        rssi_dbm=rssi_dbm, score=decision.top_score,
                                        score_norm=decision.score_norm, latency=lat)
                    continue
                tx, digest, cx, cy, cs, ch, conf = (b["veh"], b["digest"], b["cx"], b["cy"],
                                                    b["cs"], b["ch"], b["conf"])
                key = (rx.vid, digest)
                st = last_claimed.get(key)
                if st is None:
                    st = {"h": [(cx, cy, cs, ch, t)], "streak": {}, "touch": step}
                    last_claimed[key] = st
                    # First sight: there is no history, so every history-bearing check scores 0.0 and
                    # the reference IS this claim (which is also what the VRU jump arm compares to).
                    ref = prev = st["h"][0]
                    first_sight = True
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
                    # The ONE-STEP baseline (most recent prior fix) the heading check uses: turns are
                    # negligible over one step, so a moving vehicle's bearing matches its heading.
                    prev = h[-1]
                    first_sight = False
                    h.append((cx, cy, cs, ch, t))
                    while len(h) > 1 and t - h[0][4] > cfg.detector_lag_s + 2 * cfg.dt:
                        h.pop(0)
                # THE FIREWALL (PLUGIN-ARCHITECTURE.md 2.2). Everything a check may see, and nothing
                # else: the claim as received, this receiver's own position and DECLARED reach, its
                # PHY's measurement of the frame, the history it already holds for this certificate,
                # its own HD map evaluated at the CLAIMED position, and aggregate MA-visible context.
                # `b` itself -- which carries `veh` (a whole Vehicle with .is_attacker/.attack_type),
                # the sender's TRUE x/y, `falsified` and `ghost` -- never crosses this line.
                obs = Observation(
                    digest, b["station_type"], cx, cy, cs, ch, conf, b["cg"], b["msg_count"],
                    "cam", None, sig_ok_eff, b["cvf"], b["cvt"],
                    rxx, rxy, rx_reach, rssi_dbm, link_meta[li].link_state, t, cfg.dt,
                    first_sight, ref[0], ref[1], ref[2], ref[3], ref[4],
                    prev[0], prev[1], prev[2], prev[3], prev[4],
                    _offroad(cx, cy),
                    types.MappingProxyType(
                        {"cell_cert_count": cells[(round(cx / cfg.sybil_cell_m),
                                                   round(cy / cfg.sybil_cell_m),
                                                   int(ch // 45) % 8)],
                         "cbr": geo_cbr}))
                # THE SCORE VECTOR, in declared order. `_DET_ZERO` carries every column at 0.0, so a
                # check that does not apply to this message type scores exactly 0.0 -- which is what
                # the inline `det = {k: 0.0 for k in DET_KEYS}` did.
                det = _DET_ZERO.copy()
                for _col, _ev, _prm, _rn, _wrap, _prec in _CAM_PLAN:
                    _v = _ev(obs, st if _wrap is None else _wrap_state(st, _wrap), _prm, _rn)
                    det[_col] = _v if _prec is None else _round_score(_v, _prec, _col)
                if not sig_ok_eff:
                    # signature fails -> the content cannot be trusted, so the plausibility detectors
                    # are moot; the receiver only reports the crypto-verification failure itself.
                    # (`signatureVerification` scored itself 1.5 above; this suppresses the rest,
                    # third-party columns included -- an untrusted frame is untrusted for everyone.)
                    for _col in _SIG_SUPPRESSED:
                        det[_col] = 0.0
                elif b["station_type"] == "vru":
                    # VRU-appropriate plausibility, gated on the SELF-DECLARED station type carried on
                    # the received beacon (an MA-VISIBLE field, NOT the oracle is_vru): pedestrians/
                    # cyclists legitimately travel OFF the road centerline and move slowly/erratically,
                    # so the HD-map off-road check and the vehicle-kinematic (IDM-shaped) detectors would
                    # raise benign false positives -> benign false revocations. Suppress exactly those
                    # for a VRU-declared beacon. Detectors that are meaningful regardless of station type
                    # stay ON: sybilCoLocation, signatureVerification, certValidity, acceptanceRange-
                    # Threshold (impossible-distance claim), beaconFrequency (flooding/DoS), staleOrReplay.
                    # The suppression list is DECLARED (`vru_suppressed = True` on each check), not a
                    # fourth hand-maintained tuple that had to be edited in lockstep with DET_KEYS.
                    # The gate trusts a SELF-DECLARED field, so a moving VEHICLE that declares
                    # station_type="vru" would otherwise dodge every suppressed detector for free --
                    # which is what the `vruImpersonation` check (still scored above, and NOT in the
                    # suppression list) exists to close.
                    for _col in _VRU_SUPPRESSED:
                        det[_col] = 0.0
                # THE FUSION (layer 2). Streak gate, then the report_prob Bernoulli, then the
                # most-severe-first ordering -- all of it inside the pluggable component now.
                decision = _fusion_decide(
                    det, st if _fusion_wrap is None else _wrap_state(st, _fusion_wrap),
                    obs, _fusion_params, _fusion_rng)
                if decision is None:
                    continue
                reasons = list(decision.reason_codes)
                file_report(t, reporter_digest, digest, tx, reasons, det, conf,
                            cx, cy, ref[0], ref[1], malicious=False, sig_valid=sig_ok_eff,
                            station_type=b["station_type"], rssi_dbm=rssi_dbm,
                            score=decision.top_score, score_norm=decision.score_norm,
                            latency=lat)

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
                # rssi_dbm on a FABRICATED accusation. This path never received a frame, so a NULL
                # (or a missing key) here would be a perfect oracle: "no RSSI => the report is a
                # collusion". The colluder is a real radio standing a real distance from its victim
                # -- it genuinely hears the victim's CAMs, it just lies about their content -- so the
                # honest synthesis is the value the channel model would produce for that TRUE link.
                # Same model, same true geometry, only the *draw* comes from the colluder's own
                # fabrication stream (keeping the reception loop's per-link streams untouched).
                fab_rssi = None
                if geo_chan is not None:
                    vxx, vyy = rx_pos.get(victim.vid, victim.true_state(t)[:2])
                    txx2, txy2 = rx_pos.get(tx.vid, tx.true_state(t)[:2])
                    fab_d = max(1.0, math.hypot(vxx - txx2, vyy - txy2))
                    fab_state = ("urban_nlos" if cfab.random() < (1.0 - math.exp(
                        -geo_chan.canyon_per_m * fab_d)) else geo_chan.los_state)
                    fab_mean = geo_chan.tx_dbm - tr37885_pathloss_db(fab_state, fab_d)
                    fab_sig = TR37885_SHADOW_SIGMA_DB["NLOSb" if fab_state == "urban_nlos" else "LOS"]
                    fab_m = nakagami_m_for_distance(fab_d)
                    # A report is only ever filed on a frame that was DECODED, so the honest column
                    # is the channel law CONDITIONED on clearing the decode floor. Reproduce that by
                    # rejection sampling, not by clamping: clamping would pile a point mass at
                    # exactly the floor and hand an ML model "rssi == -81.00 => fabricated".
                    for _try in range(16):
                        fab_rssi = (fab_mean + cfab.gauss(0.0, fab_sig)
                                    + 10.0 * math.log10(max(cfab.gammavariate(fab_m, 1.0 / fab_m),
                                                            1e-12)))
                        if fab_rssi >= geo_chan.decode_floor_dbm:
                            break
                    else:   # pathological geometry (very long link): stay in the decodable window
                        fab_rssi = geo_chan.decode_floor_dbm + abs(cfab.gauss(0.0, fab_sig))
                file_report(t, reporter_digest, subject_digest, victim,
                            ["positionSpeedInconsistency"], det, fab_conf, 0.0, 0.0, 0.0, 0.0,
                            malicious=True, rssi_dbm=fab_rssi)

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

    # ---- WHOLE-RUN INTEGRITY: the half a bounded conformance window structurally cannot do ----
    # Checked the moment the step loop ends, BEFORE a single output file is written, so a run whose
    # engine was rewritten mid-flight produces no dataset at all rather than a plausible one.
    #
    # THE MEASURED ATTACK THIS EXISTS FOR: a `random.Random` class rebind installed at
    # `frame.step >= 30`. It passes all four of C3's traps, and that is not a hole in the traps --
    # conformance drives a bounded window of steps and the attack simply waits it out. **A
    # fixed-window contract suite can only certify behaviour it observed.** These two lines certify
    # the run.
    _integrity_words = None
    if _armed:
        _run_sentinel.verify("at the END of the run")
        if hasattr(rng, "verify_stream"):
            _integrity_words = rng.verify_stream("at the END of the run")["words"]

    if _prev_sigint is not None:                      # restore the caller's Ctrl-C behaviour
        try:
            _signal.signal(_signal.SIGINT, _prev_sigint)
        except (ValueError, TypeError):
            pass

    if stream:
        for fh in (fh_rep, fh_lbl, fh_emit, fh_mob):
            if fh is not None:
                fh.close()

    # ---- REAP THE ISOLATED WORKERS BEFORE ANY ORACLE FILE EXISTS ----
    # The step loop is over, so nothing below needs a worker except `suite.provenance()`, which reads
    # what FINISH returned. Reaping here rather than after the digest is what makes the withholding
    # claim true of the WHOLE ground-truth set and not just the two streamed files: `gt_vehicle`,
    # `gt_attacks`, `gt_identity_map` and `gt_linkage_revocation` are written by `_write_side_files`
    # a few lines below, and until this call the child was still running and could have opened them.
    # `CheckSuite.close()` is idempotent (`IsolatedCheck.finish` returns {} once closed), so the
    # later call on the normal path is a no-op and every error path still reaps.
    suite.close()
    try:
        for _w in withheld:                               # nothing exists on disk until here
            _w.commit()
    finally:
        for _w in withheld:                               # never leave a sealed spill behind
            _w.discard()

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
        if _mob_oracle:
            data_files["ground_truth/gt_mobility_oracle.jsonl"] = os.path.join(
                cfg.out_dir, "ground_truth", "gt_mobility_oracle.jsonl")
        n_reports, n_gt_reports = stream_counts["reports"], stream_counts["labels"]
    else:
        data_files = _write_outputs(cfg, ma_reports, ma_investigations, ma_crl_events, ma_cert_status,
                                    gt_vehicle, gt_idmap, gt_attacks, gt_report_labels, gt_linkage_rev,
                                    gt_emissions)
        n_reports, n_gt_reports = len(ma_reports), len(gt_report_labels)
        if _mob_oracle:
            # fixed-fleet path: nothing was streamed, so the whole record is still in memory.
            stream_counts["mob"] = len(gt_mobility)
            data_files["ground_truth/gt_mobility_oracle.jsonl"] = _write_jsonl(
                os.path.join(cfg.out_dir, "ground_truth", "gt_mobility_oracle.jsonl"),
                sorted(gt_mobility, key=lambda r: r["mob_id"]))
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
    chan.close()                       # ALWAYS called, including on the SIGINT finalisation path
    # Isolated detector workers: FINISH (which collects their declared streams for the manifest and
    # asserts they scored every message they were sent) and reap the processes. Before
    # `suite.provenance()` below, which reads what FINISH returned. On the flow path this already
    # happened immediately after the step loop -- before any ORACLE file was written -- and
    # `close()` is idempotent; this call is what covers a run that never reached that point.
    suite.close()

    def _protocol_block() -> dict:
        """`counts["protocol"]` -- what the real stack actually did, in measured numbers.

        Empty (so the key is absent, so the manifest is byte-identical) unless a profile or the
        security layer is active. **The engine COUNTS and the profile says what the counts mean**:
        every tally below is an observation of this run, handed to `profile.report()`, which is what
        lets a stack whose congestion controller has no state table (LIMERIC has a continuous
        duty-cycle instead) report its own quantity in its own words with no engine knowledge of it.

        Two blocks stay engine-side, deliberately. `cbr` is what THIS ENGINE'S receivers measured --
        the profile supplied the estimator, but the measurement is the engine's, and it is reported
        for a security-only run that has no profile at all. `security` is the PKI layer, which is
        not part of the profile seam.
        """
        if _profile is None and _sec is None:
            return {}
        out: dict = {}
        if _profile is not None:
            out.update(_profile.report(_api_profile.ProtocolMeasurements(
                generation_states=tuple(_cam_state.values()) if _cam_state else (),
                congestion_states=tuple(_dcc_state.values()) if _dcc_state else (),
                triggers=dict(_cam_triggers),
                gaps=(_cam_gap_n, _cam_gap_sum, _cam_gap_max),
                cbr=(_cbr_sum, _cbr_n, _cbr_max, cfg.dt),
                wire=(_wire_size_sum, _wire_size_n),
                codec_stats=(_wire.stats() if _wire is not None else {}),
                latency_hist=dict(_lat_hist) if _lat_on else {},
                latency=((_lat_sum, _lat_n, (_lat_min if _lat_n else 0.0), _lat_max)
                         if _lat_on else (0.0, 0, 0.0, 0.0)))))
        if _cbr_n and "cbr" not in out:
            # THE FALLBACK, for a run whose profile did not claim the block (or that has no profile
            # at all -- a security-only run still measures a CBR). Measured quantities ONLY: any
            # constant here would be a standard's number published beside a run that may not speak
            # that standard, which is exactly the kind of quiet fabrication the profile seam exists
            # to remove.
            out["cbr"] = {"mean": round(_cbr_sum / _cbr_n, 6), "max": round(_cbr_max, 6),
                          "samples": _cbr_n, "window_s": cfg.dt}
        if _sec is not None:
            s = _sec.stats()
            s["verdicts"] = dict(sorted(_verdicts.items()))
            if _sec_refusals:
                # Attacks that could NOT be mounted honestly, by name. An attacker with no expired
                # credential genuinely cannot present one; recording the refusal is the difference
                # between modelling that and quietly falling back to editing a wire field.
                s["attack_refusals"] = dict(sorted(_sec_refusals.items()))
            out["security"] = s
        return {"protocol": out}

    # The manifest is about to claim that THIS config produced THIS data. Check that first: a
    # mid-run write leaves a dataset that neither the old nor the new value describes, so the honest
    # outcome is no manifest at all rather than a plausible one nobody can replay.
    _assert_config_unmoved(_cfg0, cfg, "during the run")
    _write_manifest(cfg, data_files, data_digest,
                    counts=dict(vehicles=len(vehicles), reports=n_reports,
                                investigations=len(ma_investigations), revoked=len(revoked_vehicles),
                                mobility_survivorship=_survivorship_block(
                                    cfg, surv, surv_span, revoked_vehicles, len(vehicles),
                                    stream_counts["mob"] if _mob_oracle else None),
                                # What the signals and the footways ACTUALLY did, as integers, so a
                                # run is judged on the colours it served rather than on a flag being
                                # set. Emitted only when the opt-in is on; `counts` is outside
                                # `_data_digest` by construction either way.
                                **({"signal_service": dict(_sig_served, plan=_sig_stats)}
                                   if cfg.real_signals else {}),
                                **({"sidewalks": _sidewalk_stats} if _sidewalk_stats else {}),
                                # The SCENE's other half on the whole-city path: which footprint
                                # file was rasterised, how many polygons survived, and every
                                # alignment statistic the registration gate measured. Emitted only
                                # when `sumo_buildings` is set.
                                **({"scene_buildings": _geo_building_stats}
                                   if _geo_building_stats else {}),
                                # THE PROTOCOL STACK'S OWN MEASUREMENTS. Emitted only when at least
                                # one of the five opt-ins is on, so a default manifest is
                                # byte-identical; and `counts` is outside `_data_digest` by
                                # construction either way. This is the block that turns "the CAM
                                # rate is right" from a claim into a number a reader can check.
                                **_protocol_block()),
                    plugins=plugin_block([chan_provenance()] + suite.provenance()
                                         + ([codec_provenance()] if codec_provenance else [])
                                         + ([profile_provenance()] if profile_provenance else [])
                                         + ([report_format_provenance()]
                                            if report_format_provenance else []),
                                         drift=_drift_allowed,
                                         integrity=({"armed": True,
                                                     "verified_at": ["plugin resolution (import)",
                                                                     "channel plugin load",
                                                                     "detection layer load",
                                                                     "end of run"],
                                                     "watched": _run_sentinel.watched,
                                                     "engine_rng_words": _integrity_words,
                                                     "ok": True}
                                                    if _armed else None)),
                    config_snapshot=_cfg0, mobility=_mob_block, codec_claim=_codec_claim,
                    profile_claim=_profile_claim,
                    report_claim=(dict(_report_fmt.standards_claim())
                                  if _report_fmt is not None else None))

    return RunResult(out_dir=cfg.out_dir, n_vehicles=len(vehicles), n_reports=n_reports,
                     n_investigations=len(ma_investigations), n_revoked=len(revoked_vehicles),
                     revoked_cert_digests=sorted(revoked_digests), data_digest=data_digest,
                     counts=dict(cert_status=len(ma_cert_status), gt_reports=n_gt_reports))


# --------------------------------------------------------------------------- #
# Withholding ORACLE output while a third-party detector is alive
# --------------------------------------------------------------------------- #
#: Total bytes of withheld ORACLE output the engine will hold in memory before it starts SEALING the
#: overflow to disk instead. Not a config field, deliberately, for the same reason
#: `isolate.DEFAULT_TIMEOUT_S` is not one: it cannot change a single output byte, it does not belong
#: in `manifest["config"]`, and a number that cannot affect a result must not look replayable.
#:
#: MEASURED, on this host (`docs/realism/ISOLATION-ORACLE-LEAK.md` has the table). The InTAS AM peak
#: -- 1 188 vehicles, 158 767 vehicle-steps, 300 s -- withholds 3.2 MiB and costs +4.1 MB of peak
#: working set, 1.6 % of the run's own 254 MB, with no spill and the same `data_digest`. The
#: heaviest dataset in this repository (a 0.1 s-step InTAS replay, 1 595 741 reports) writes 212 MiB
#: of labels plus 210 MiB of emission samples; at the measured 1.008 bytes of RSS per byte held,
#: buffering all of it would cost ~442 MB. 384 MiB keeps the whole label table and most of the
#: emission table in memory and seals the rest. Below the ceiling nothing whatsoever exists on disk;
#: above it, what exists is ciphertext at +0.1 MB of RSS and ~2 s per 212 MiB round trip.
WITHHELD_MEMORY_BYTES = 384 << 20

#: Keystream block for the spill. One `shake_128` call yields the whole block, so sealing 200 MB
#: costs ~200 hash calls rather than millions of 32-byte ones.
_SEAL_BLOCK = 1 << 20

#: Rows are joined into blocks of about this size as they arrive. See `_WithheldStream.write`.
_COMPACT_BYTES = 8 << 20


def _seal_keystream(key: bytes, block: int) -> bytes:
    return hashlib.shake_128(key + block.to_bytes(8, "big")).digest(_SEAL_BLOCK)


def _seal_xor(data: bytes, key: bytes, block: int) -> bytes:
    """XOR one aligned block with its keystream, through a big-int so the loop runs in C.

    An involution: the same call unseals. `int.from_bytes` drops leading zero BITS, never bytes,
    and `to_bytes(len(data))` restores the exact width -- so this round-trips byte-for-byte,
    including a block that begins with NUL.
    """
    if not data:
        return b""
    ks = _seal_keystream(key, block)[:len(data)]
    return (int.from_bytes(data, "big") ^ int.from_bytes(ks, "big")).to_bytes(len(data), "big")


class _WithheldStream:
    """An ORACLE output stream that must not be READABLE while an isolated detector is alive.

    **Why this exists.** `run_pipeline` streams `ma/ma_reports.jsonl`,
    `ground_truth/gt_report_labels.jsonl` and `ground_truth/gt_emissions_sample.jsonl` as the loop
    goes, to keep a multi-hour run memory-bounded. The first two of those are fine to stream; the
    ground-truth pair is the ANSWER KEY, and an isolated third-party detector is an ordinary OS
    process that can open any file the engine has written. Measured, before this class existed: a
    detector opened `gt_report_labels.jsonl` at its 20 000th message and read the oracle verdict --
    plus `reporter_true_id` and `subject_true_id` -- for every report filed so far in the run it was
    being graded on.

    So when any check is `isolated`, the two ground-truth streams are given one of these instead of a
    file handle. It accepts exactly the writes the file handle accepted, in the same order, and
    :meth:`commit` -- called only after the last worker has been REAPED -- lays them down. The bytes
    are the concatenation of the same strings through the same encoding, so the dataset is
    byte-identical and `test_isolated_and_in_process_agree_bit_for_bit` is what proves it.

    **Two tiers, because the memory is not free.** Up to `budget` bytes across all withheld streams
    the rows are held in memory and NOTHING exists on disk -- no file, no `ground_truth/` directory,
    nothing to open. Past that the overflow spills to `<out_dir>/.withheld/<name>.sealed`, XORed with
    a 32-byte `os.urandom` key that is never written anywhere; `commit` unseals it into the real file
    and deletes it. A child that finds the spill reads noise. The degradation is stated rather than
    hidden: past the ceiling the claim weakens from "nothing on disk" to "nothing readable on disk".

    The key is per-stream, per-run, and drawn from `os.urandom` -- NOT from any engine RNG. It never
    touches a value that reaches the digest (the plaintext is restored bit-for-bit before anything
    hashes it), so this cannot move a golden, and it does not consume an engine stream.
    """

    __slots__ = ("path", "name", "_chunks", "_small", "_small_n", "_mem", "_budget", "_spill",
                 "_spill_path", "_key", "_pending", "_block", "_spilled", "_total")

    def __init__(self, path: str, budget: list):
        self.path = path
        self.name = os.path.basename(path)
        #: Shared, mutable, one-element budget: every withheld stream of a run draws from the same
        #: ceiling, so two streams cannot each spend it.
        self._budget = budget
        self._chunks: list[str] = []                     # compacted, ~_COMPACT_BYTES each
        self._small: list[str] = []                      # the rows since the last compaction
        self._small_n = 0
        self._mem = 0
        self._total = 0
        self._spill = None
        self._spill_path = None
        self._key = None
        self._pending = bytearray()
        self._block = 0
        self._spilled = 0

    # -- the file-handle surface the streaming path uses ------------------------------------- #
    def write(self, text: str) -> None:
        n = len(text)
        self._total += n
        if self._spill is None and self._budget[0] >= n:
            self._budget[0] -= n
            self._small.append(text)
            self._small_n += n
            self._mem += n
            if self._small_n >= _COMPACT_BYTES:
                # COMPACTION, and it is what makes the memory tier affordable. A ground-truth row is
                # ~140 characters, and a `str` costs its characters plus a 49-byte header plus a
                # pointer: holding 1.6 M of them individually measured 1.44 bytes of RSS per byte of
                # output. Joined into 8 MiB blocks the ratio is ~1.0, so the ceiling below buys
                # nearly its own size in real output rather than two thirds of it.
                self._chunks.append("".join(self._small))
                self._small, self._small_n = [], 0
            return
        self._spill_write(text)

    def flush(self) -> None:
        pass

    def close(self) -> None:
        """The loop's own close. A no-op on purpose: :meth:`commit` is the only thing that creates
        the file, and it must not happen until the workers are gone."""

    # -- the spill --------------------------------------------------------------------------- #
    def _spill_write(self, text: str) -> None:
        if self._spill is None:
            self._key = os.urandom(32)
            root = os.path.join(os.path.dirname(self.path), os.pardir, ".withheld")
            root = os.path.normpath(root)
            os.makedirs(root, exist_ok=True)
            self._spill_path = os.path.join(root, self.name + ".sealed")
            self._spill = open(self._spill_path, "wb")
        self._pending.extend(text.encode("utf-8"))
        while len(self._pending) >= _SEAL_BLOCK:
            chunk = bytes(self._pending[:_SEAL_BLOCK])
            del self._pending[:_SEAL_BLOCK]
            self._spill.write(_seal_xor(chunk, self._key, self._block))
            self._block += 1
            self._spilled += len(chunk)

    def commit(self) -> str:
        """Create the real file. **Call only after every isolated worker has been reaped.**"""
        if self._spill is not None:
            if self._pending:
                self._spill.write(_seal_xor(bytes(self._pending), self._key, self._block))
                self._spilled += len(self._pending)
                self._pending = bytearray()
            self._spill.close()
            self._spill = None
        os.makedirs(os.path.dirname(self.path), exist_ok=True)
        if self._small:
            self._chunks.append("".join(self._small))
            self._small, self._small_n = [], 0
        # BINARY, and that is not a style choice: the spill is sealed in fixed 1 MiB blocks, so a
        # block boundary can fall in the middle of a multi-byte UTF-8 sequence and a per-block
        # `.decode()` would raise on it. Writing bytes also makes the byte-identity with the streamed
        # path trivially true -- `newline="\n"` on the streaming handle means no translation either.
        with open(self.path, "wb") as fh:
            # One compacted block at a time, each released as it lands: joining the whole buffer
            # first would materialise the entire file as a `str` AND as `bytes` on top of what is
            # already held -- measured at 754 MB peak for a 212 MiB table, against 306 MB held.
            for i, chunk in enumerate(self._chunks):
                fh.write(chunk.encode("utf-8"))
                self._chunks[i] = ""
            self._budget[0] += self._mem
            self._chunks, self._mem = [], 0
            if self._spill_path:
                with open(self._spill_path, "rb") as sealed:
                    block = 0
                    while True:
                        chunk = sealed.read(_SEAL_BLOCK)
                        if not chunk:
                            break
                        fh.write(_seal_xor(chunk, self._key, block))
                        block += 1
        self.discard()
        return self.path

    def discard(self) -> None:
        """Drop the sealed spill without publishing it. Safe on every path, idempotent."""
        if self._spill is not None:
            try:
                self._spill.close()
            except OSError:                              # pragma: no cover
                pass
            self._spill = None
        if self._spill_path:
            try:
                os.remove(self._spill_path)
            except OSError:                              # pragma: no cover
                pass
            try:
                os.rmdir(os.path.dirname(self._spill_path))
            except OSError:
                pass
            self._spill_path = None
        self._key = None

    # -- what the measurement reports -------------------------------------------------------- #
    @property
    def sealed_path(self):
        """Where the overflow is sealed, or None while everything is still in memory."""
        return self._spill_path

    @property
    def buffered_bytes(self) -> int:
        return self._mem

    @property
    def sealed_bytes(self) -> int:
        return self._spilled + len(self._pending)

    @property
    def total_bytes(self) -> int:
        return self._total


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


def _survivorship_block(cfg, surv: dict, surv_span: dict, revoked: dict, n_vehicles: int,
                        n_oracle_rows: int | None) -> dict:
    """How much of the simulated mobility the EMISSION stream actually kept — as a number.

    `gt_emissions_sample.jsonl` is written downstream of `enforced()`, so a revoked vehicle's
    kinematic record ends at revocation while the vehicle keeps driving. Over the InTAS AM peak hour
    that deleted 56.22% of the vehicle-steps, and it is invisible at the 60-300 s durations the
    engine's traffic metrics were previously read at. Publishing the ratio here means every consumer
    of the dataset can SEE the truncation instead of inheriting it silently — and
    `datagen.realism_bench` reads exactly this block to decide whether a traffic metric is
    measurable at all.

    Lives in ``manifest["counts"]``, which ``_data_digest`` excludes by construction, so this block
    is unconditional and moves no digest. Every field is derived from integer tallies taken in the
    step loop; nothing here re-reads a file or draws from an RNG.

    ``vehicle_steps_survival_frac`` is the honest denominator: broadcast vehicle-steps over
    SIMULATED vehicle-steps. Note it is NOT recoverable from the dataset's own contents — a stream
    that stops at revocation cannot report what it did not record — which is why it is stamped into
    the manifest rather than left to be estimated. (Estimating it from the surviving records is
    measurably biased: never-revoked vehicles are systematically SHORT-trip vehicles, because
    exposure is what earns a false positive.)
    """
    spans_rev, spans_ok, sim_rev, sim_ok = [], [], [], []
    for vid, sp in surv_span.items():
        sim_span = float(sp[1] - sp[0])
        rec_span = 0.0 if sp[3] is None else float(sp[4] - sp[3])
        if vid in revoked:
            spans_rev.append(rec_span); sim_rev.append(sim_span)
        else:
            spans_ok.append(rec_span); sim_ok.append(sim_span)

    def _mean(xs):
        return round(sum(xs) / len(xs), 3) if xs else None

    sim = int(surv["sim"])
    bc = int(surv["bcast"])
    out = {
        "vehicle_steps_simulated": sim,
        "vehicle_steps_broadcast": bc,
        "vehicle_steps_enforced_out": int(surv["enforced"]),
        "vehicle_steps_emitted": int(surv["emit"]),
        "vehicle_steps_survival_frac": (round(bc / sim, 6) if sim else None),
        "vehicles": int(n_vehicles),
        "vehicles_revoked": len(revoked),
        "revoked_vehicle_frac": (round(len(revoked) / n_vehicles, 6) if n_vehicles else None),
        "mean_record_span_s_revoked": _mean(spans_rev),
        "mean_record_span_s_never_revoked": _mean(spans_ok),
        "mean_simulated_span_s_revoked": _mean(sim_rev),
        "mean_simulated_span_s_never_revoked": _mean(sim_ok),
        "emit_sample_prob": cfg.emit_sample_prob,
        # The un-enforced record, when the opt-in wrote one. `oracle_rows == simulated` is the
        # property that makes it the unbiased source, and it is asserted below rather than assumed.
        "oracle_record": ("ground_truth/gt_mobility_oracle.jsonl" if n_oracle_rows is not None
                          else None),
        "oracle_rows": n_oracle_rows,
    }
    if n_oracle_rows is not None:
        assert n_oracle_rows == sim, (
            f"gt_mobility_oracle wrote {n_oracle_rows} rows for {sim} simulated vehicle-steps: the "
            "un-enforced record must be one row per active station per step")
    return out


def _data_digest(out_dir: str, data_files: dict[str, str]) -> str:
    """Single digest over all DATA files (manifest excluded -> determinism-safe)."""
    h = hashlib.sha256()
    for rel in sorted(data_files):
        h.update(rel.encode())
        h.update(_file_sha256(data_files[rel]).encode())
    return h.hexdigest()


#: What this engine may HONESTLY assert, per standard, today. Corrected 2026-08-30 from
#: `{"report": "ETSI TS 103 759 (shape)", "cert": "IEEE 1609.2", "linkage": "CAMP SCP2"}`.
#:
#: The claim `cert: IEEE 1609.2` was NOT SUPPORTABLE and is withdrawn: there is no 1609.2
#: certificate structure anywhere in this repo (a certificate is a hex digest plus a validity
#: window), and NO SIGNING happens on any path -- `grep -c 'ca.sign(\|ca.verify('` over `src/` is
#: zero, `sig_ok` is a boolean set by the attack switch at run.py:3164, and the Java engine passes a
#: literal `true`. What IS real is `HashedId8` (`crypto_abstract.hashed_id8`, the low-order 8 bytes
#: of SHA-256 per IEEE 1609.2 6.4.3), so the claim is downgraded to an IDENTIFIER-ONLY claim that
#: names it. `linkage: CAMP SCP2` is KEPT verbatim -- it is earned: a real seed hash-chain,
#: Davies-Meyer pre-linkage, `lv = plv1 XOR plv2`, forward-only matching, asserted in-run.
#:
#: `_data_digest` (run.py:_data_digest) excludes `manifest.json` BY CONSTRUCTION, so correcting this
#: block moves ZERO digests -- which is why it was not worth deferring behind any code work.
#: See docs/realism/PLUGIN-ARCHITECTURE.md 6.5 and docs/realism/STANDARDS-AUDIT.md.
STANDARDS_PROFILE = {
    "linkage": "CAMP SCP2 -- implemented and enforced (scms_core/linkage.py; asserted in-run)",
    "cert": "HashedId8 identifiers per IEEE 1609.2 6.4.3; NOT a 1609.2 certificate profile",
    "security_envelope": "none -- sig_ok is a simulated boolean; no signature is computed or verified",
    "message": "native_v1 -- engine-private representation; no ASN.1 encoding",
    "report": ("ETSI TS 103 759 V2.2.1: partial field-name correspondence only; not encoded, "
               "not signed, and carrying no v2xPduEvidence"),
}


def standards_profile_for(cfg, codec_claim=None, profile_claim=None, report_claim=None) -> dict:
    """`STANDARDS_PROFILE`, corrected for what the run's protocol stack ACTUALLY did.

    The base dict is the honest claim for a run with no codec and no cryptography: "no ASN.1
    encoding", "sig_ok is a simulated boolean". Both statements become FALSE the moment the
    corresponding opt-in is on, and a manifest that kept saying them would be the one lie this
    whole workstream exists to remove. The replacement text for the message layer is the CODEC'S
    OWN `standards_claim()`, verbatim, never a sentence composed here -- section 6.5's rule is that
    a profile states what it may honestly assert, and the engine is not entitled to improve on it.

    Manifest-only, so this moves no digest.
    """
    prof = dict(STANDARDS_PROFILE)
    if codec_claim:
        prof["message"] = codec_claim.get("message", prof["message"])
        for k in ("asn1_source", "asn1_licence", "asn1_modules", "caveats"):
            if k in codec_claim:
                prof[k] = codec_claim[k]
    if getattr(cfg, "security_model", "none") == "ecdsa":
        prof["cert"] = ("IEEE 1609.2-STRUCTURED explicit certificates carrying real ECDSA-P256 "
                        "signatures over a canonical NON-COER serialisation this repository "
                        "defines (scms_core/certificate.py). HashedId8 is 1609.2 6.4.3 over that "
                        "preimage, so a production 1609.2 stack would compute a different one. "
                        "NOT '1609.2 compliant' and NOT interoperable.")
        prof["security_envelope"] = (
            "IEEE 1609.2 SignedData SHAPE with a REAL ECDSA-P256-SHA256 signature over the 5.3.1 "
            "double hash Hash(tbsData) || Hash(signer certificate), verified receiver-side against "
            "a trust store, the certificate validity window and the live CAMP SCP2 linkage CRL. "
            "sig_ok is the RESULT of that verification. The serialisation is canonical but NOT "
            "TS 103 097 COER, so the octets are not interoperable; the cryptography is real.")
        prof["provisioning"] = (
            "CAMP SCP1 butterfly key expansion (scms_core/butterfly.py + provisioning.py): the RA "
            "expands one caterpillar into per-(i,j) cocoons and drains a device-mixing queue, and "
            "the PCA certifies B + c*G under a randomiser the RA never sees. The PCA's ledger "
            "carries no device identifier, so it cannot group a device's certificates -- which the "
            "single-request_hash label scheme it replaces could do perfectly.")
    # THE PROFILE'S AND THE FORMAT'S OWN WORDS, verbatim and under their own keys. Same rule as the
    # codec's: a component states what it may honestly assert and the engine is not entitled to
    # improve on it. Emitted ONLY when one is active, so a default manifest is unchanged.
    if profile_claim:
        prof["protocol_profile"] = profile_claim
    if report_claim:
        prof["report_format"] = report_claim
    return prof


def _write_manifest(cfg, data_files, data_digest, counts, plugins=None,
                    config_snapshot=None, mobility=None, codec_claim=None,
                    profile_claim=None, report_claim=None) -> None:
    manifest = {
        "dataset_version": __version__,
        "build_utc": datetime.now(timezone.utc).isoformat(),   # NOT part of data_digest
        "generator": "scms_sim_ref.mock_pipeline (pre-MOSAIC reference, realistic v2)",
        "seed": cfg.seed,
        # The SNAPSHOT taken before step 0, not a late read of the live object. `run_pipeline`
        # already refuses to get here if the two differ, so this is belt and braces -- but it means
        # the field that carries the replay contract is never a function of when it was read.
        "config": _config_dict(cfg) if config_snapshot is None else config_snapshot,
        # ground_truth 2 == ADR 0002: gt_emissions_sample carries true_speed / true_heading.
        # ma_visible is unchanged (no MA-visible field moved), so it stays at 1.
        "schema_versions": {"ma_visible": 1, "ground_truth": 2},
        # Units/frames a consumer cannot infer from the numbers. The Python engine's heading is the
        # math convention (atan2(vy, vx)): degrees counter-clockwise from +x/East, [0, 360). The
        # MOSAIC/Java engine writes SUMO/ETSI headings (degrees clockwise from North), so a consumer
        # merging the two MUST read this field rather than assume. Manifest-only -> digest-safe.
        "conventions": {"heading": "deg_ccw_from_east", "speed": "m_s", "position": "m_local_xy"},
        "standards_profile": standards_profile_for(cfg, codec_claim, profile_claim, report_claim),
        # Interpreter/host provenance. NOT cosmetic and NOT digest-bearing: Python's documented
        # reproducibility guarantee covers ONLY Random.random() -- gauss(), uniform(), choice() and
        # shuffle() carry NO cross-version guarantee, and the pinned goldens depend on `gauss` (the
        # shadowing draw) and `uniform` (net_delay). The digests are therefore pinned to a CPython
        # version as much as to a seed, and until now the manifest did not say so. Recording
        # sys.flags.hash_randomization alongside makes the PYTHONHASHSEED=0 discipline (run.ps1,
        # gui.ps1, conftest.py) an auditable property of the artifact rather than a convention.
        "runtime": _api_registry.runtime_block(),
        "data_digest_sha256": data_digest,
        "outputs": [{"path": rel, "sha256": _file_sha256(p)} for rel, p in sorted(data_files.items())],
        "counts": counts,
        # The plugin LOCK (dvc.lock to cfg.plugins' dvc.yaml): what was ACTUALLY loaded, content
        # addressed. Excluded from data_digest by construction; carries its own provenance_digest.
        "plugins": plugins if plugins is not None else empty_plugin_block(),
    }
    # The MOBILITY LOCK, and it is the same dvc.yaml/dvc.lock split the plugin block is: `config`
    # carries the INTENT (a path and a pinned hash), this carries what was actually replayed --
    # the SUMO build and seed that produced it, the network it was frozen on by content, and the
    # measured coherence between the two. Emitted ONLY when the mode is on, so a default run's
    # manifest is byte-identical (and it is outside `data_digest` by construction either way).
    if mobility is not None:
        manifest["mobility"] = mobility
    with open(os.path.join(cfg.out_dir, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(manifest, fh, indent=2, sort_keys=True)
        fh.write("\n")


# --------------------------------------------------------------------------- #
# Plugin subcommands (PLUGIN-ARCHITECTURE.md sections 4.3 and 5)
#
# Dispatched from the FIRST positional token, before the 138-flag parser is built. Argparse
# subparsers were rejected: they would move every existing flag under an implicit default
# subcommand, which is exactly the CLI/GUI contract `tests/test_gui_cli_contract.py` polices. A
# two-line prefix check adds the two commands the design names and cannot perturb anything else.
# --------------------------------------------------------------------------- #
SUBCOMMANDS = ("verify-plugins", "conformance")


def _cli_verify_plugins(argv) -> int:
    """`scms-poc verify-plugins <manifest.json> [--allow-drift] [--json]` -- a CI gate, NO simulation.

    D4's first detection layer, on its own. It re-resolves every non-built-in entry of the manifest's
    plugin LOCK and recomputes `module_sha256` / `dist_sha256` / `interface_version`, so a dataset can
    be revalidated on another machine, months later, WITHOUT paying for the run -- and, crucially,
    before paying for it. The discrimination against the second layer (the pinned goldens) is the
    whole value: identity drift with no digest drift is a harmless refactor; NO identity drift with
    digest drift means the plugin is nondeterministic or the interpreter changed.

    Exit codes: 0 clean, 2 drift (or an unresolvable plugin), 1 for a bad/unreadable manifest.
    """
    import argparse
    import sys as _sys
    p = argparse.ArgumentParser(prog="scms-poc verify-plugins",
                                description="Verify a manifest's plugin lock against what is "
                                            "installed now. No simulation is run.")
    p.add_argument("manifest", help="path to a run's manifest.json")
    p.add_argument("--allow-drift", action="store_true",
                   help="report drift and exit 0 (still prints every drift on stderr)")
    p.add_argument("--json", action="store_true", help="emit a machine-readable result on stdout")
    a = p.parse_args(argv)
    try:
        # utf-8-sig, not utf-8: PowerShell's `Set-Content`/`Out-File` write a UTF-8 BOM by default
        # on this host, and `json.load` refuses one. Reading -sig is byte-identical for BOM-free
        # files, so this only ever adds tolerance.
        with open(a.manifest, encoding="utf-8-sig") as fh:
            man = json.load(fh)
    except (OSError, ValueError) as e:
        print(f"verify-plugins: cannot read {a.manifest}: {e}", file=_sys.stderr)
        return 1
    lock = man.get("plugins") if isinstance(man.get("plugins"), dict) else None
    if lock is None:
        print(f"verify-plugins: {a.manifest} carries no plugins block (written by an engine "
              f"predating the lock)", file=_sys.stderr)
        return 1
    loaded = lock.get("loaded") or []
    third_party = [e for e in loaded if e.get("resolved_via") != "builtin"]
    recomputed = _api_registry.provenance_digest(loaded)
    result = {"manifest": os.path.abspath(a.manifest), "api_version": lock.get("api_version"),
              "loaded": len(loaded), "third_party": len(third_party),
              "provenance_digest": lock.get("provenance_digest"),
              "provenance_digest_recomputed": recomputed,
              "provenance_digest_ok": recomputed == lock.get("provenance_digest"),
              "runtime": man.get("runtime", {}), "drifts": []}
    try:
        result["drifts"] = list(_api_registry.verify_lock(lock, allow_drift=a.allow_drift))
    except PluginDriftError as e:
        result["drifts"] = [str(e)]
        _emit_verify(result, a.json)
        print(f"PLUGIN DRIFT: {e}", file=_sys.stderr)
        return 2
    _emit_verify(result, a.json)
    if not result["provenance_digest_ok"]:
        print("verify-plugins: provenance_digest does not match the recorded `loaded` list -- the "
              "lock itself was edited after the run", file=_sys.stderr)
        return 2
    if result["drifts"] and not a.allow_drift:                 # pragma: no cover - verify_lock raises
        return 2
    return 0


def _emit_verify(result, as_json: bool) -> None:
    if as_json:
        print(json.dumps(result, indent=2, sort_keys=True))
        return
    print(f"verify-plugins {result['manifest']}")
    print(f"  api_version          {result['api_version']}")
    print(f"  loaded               {result['loaded']} ({result['third_party']} third-party)")
    print(f"  provenance_digest    {result['provenance_digest']} "
          f"{'OK' if result['provenance_digest_ok'] else 'MISMATCH'}")
    rt = result.get("runtime") or {}
    if rt:
        print(f"  runtime              {rt.get('python_version')} {rt.get('platform')} "
              f"hash_randomization={rt.get('hash_randomization')}")
    for d in result["drifts"]:
        print(f"  DRIFT                {d}")
    if not result["drifts"]:
        print("  no drift")


def _cli_conformance(argv) -> int:
    """`scms-poc conformance --slot channel_model --ref <ref>` -- grade a plugin against v1.

    The third delivery route for the suite (the other two being "subclass it in your own test suite"
    and "let the engine refuse an unattested plugin"). Writes the same `conformance_report.json`
    that belongs in `manifest["plugins"]["loaded"][*]["conformance"]`, so *"this dataset was produced
    by a conformant plugin"* becomes a machine-checkable property of the artifact.

    Exit codes: 0 conformant (waived failures included -- a waiver is a declared, recorded
    limitation), 1 a check FAILED or ERRORED, 2 the arguments themselves were unusable.
    """
    import argparse
    import sys as _sys
    from ..conformance.runner import CONTRACTS, run_ref
    p = argparse.ArgumentParser(prog="scms-poc conformance",
                                description="Run the v1 conformance suite against one plugin.")
    p.add_argument("--slot", default="channel_model", choices=sorted(CONTRACTS))
    p.add_argument("--ref", required=True,
                   help="built-in registry key, entry-point name, or 'package.module:Class'")
    p.add_argument("--params", default="", help="plugin params as a JSON object")
    p.add_argument("--seed", type=int, default=None, help="override the suite seed")
    p.add_argument("--radio-range", type=float, default=None,
                   help="construction-environment radio_range_m handed to the model")
    p.add_argument("--report", default=None, help="write conformance_report.json here")
    p.add_argument("--json", action="store_true", help="print the report as JSON on stdout")
    a = p.parse_args(argv)
    try:
        params = json.loads(a.params) if a.params.strip() else {}
    except ValueError as e:
        print(f"conformance: --params is not valid JSON: {e}", file=_sys.stderr)
        return 2
    if not isinstance(params, dict):
        print("conformance: --params must be a JSON object", file=_sys.stderr)
        return 2
    try:
        rep = run_ref(a.slot, a.ref, params, seed=a.seed, radio_range_m=a.radio_range)
    except (ValueError, TypeError) as e:
        print(f"conformance: {type(e).__name__}: {e}", file=_sys.stderr)
        return 2
    print(json.dumps(rep.to_dict(), indent=2, sort_keys=True) if a.json else rep.to_text())
    if a.report:
        rep.write(a.report)
        print(f"conformance report written to {os.path.abspath(a.report)}")
    return 0 if rep.ok else 1


def main(argv: Optional[list[str]] = None) -> int:
    import argparse
    import sys as _sys
    _raw = list(argv) if argv is not None else list(_sys.argv[1:])
    if _raw and _raw[0] in SUBCOMMANDS:
        return {"verify-plugins": _cli_verify_plugins,
                "conformance": _cli_conformance}[_raw[0]](_raw[1:])
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
    p.add_argument("--attack-magnitude-scale", default="",
                   help="per-type falsification-magnitude multiplier, e.g. 'RandomPos:2.0,ConstPosOffset:0.5'")
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
    # choices come from the BUILT-IN REGISTRY (one source of truth with validate_config /
    # _ENUM_OPTIONS / the GUI dropdown). A THIRD-PARTY channel model is not selected here: it is
    # declared in the config's `plugins` block, because only a config field replays.
    p.add_argument("--radio-model", choices=list(_api_registry.builtin_names("channel_model")),
                   default="disc",
                   help="reachability: disc (hard range) | logdistance (soft path-loss + shadowing) "
                        "| geometric (3GPP TR 37.885 LOS/NLOSv/NLOSb + AR(1) shadowing + Nakagami)")
    p.add_argument("--plugins", default="",
                   help='ACTIVATE component plugins, as a JSON object {slot: {"ref":..., '
                        '"params":{...}}}. `ref` resolves through the built-in registry, an '
                        'installed entry point, then a dotted path "package.module:Class". '
                        'Empty (the default) = built-ins only, byte-identical. Example: '
                        '--plugins \'{"channel_model": {"ref": "myorg.radio:Rayleigh"}}\'')
    # --- the real protocol stack (codecs/etsi_rules.py + scms_core/engine_security.py) ---------- #
    p.add_argument("--message-codec", default="",
                   choices=["", *_api_registry.builtin_names("message_codec")],
                   help="encode every CAM/DENM/VAM through this wire format and let the resulting "
                        "PDU LENGTH drive airtime/CBR/collision loss. '' (default) constructs no "
                        "codec at all. A third-party profile is declared via plugins.message_codec")
    p.add_argument("--message-signer", default="digest", choices=list(_api_codec.SIGNER_FORMS),
                   help="TS 103 097 signer form used for the frame's wire size (none|digest|"
                        "certificate); ignored under --security-model ecdsa")
    p.add_argument("--cam-rules", action="store_true",
                   help="ETSI EN 302 637-2 CAM generation rules (4 m / 4 deg / 0.5 m/s triggers, "
                        "T_GenCamMin 0.1 s, T_GenCamMax 1.0 s) instead of one CAM per step")
    p.add_argument("--dcc", action="store_true",
                   help="ETSI TS 102 687 reactive DCC over the measured CBR (needs --cam-rules)")
    p.add_argument("--protocol-profile", default="",
                   choices=["", *_api_registry.builtin_names("protocol_profile")],
                   help="WHICH PROTOCOL this run speaks, as one declaration. '' (default) builds "
                        "the built-in etsi_its_g5 stack from the flags above, and nothing at all "
                        "when they are off. A third-party stack -- its own codec, generation "
                        "rules, congestion controller, airtime and latency model -- is declared "
                        "via plugins.protocol_profile, because only a config field replays")
    p.add_argument("--report-format", default="",
                   choices=["", *_api_registry.builtin_names("report_format")],
                   help="misbehaviour-report format: '' (the engine's historic inline row) | "
                        "ma_report_v1 (the identical row through the report_format seam) | "
                        "ts103759_shape (the TS 103 759 TemplateAsr shape with real evidence "
                        "octets). Third-party formats go through plugins.report_format")
    p.add_argument("--net-latency", action="store_true",
                   help="per-packet propagation + access + stack latency in place of the uniform "
                        "report-ingest delay (deterministic; draws no random number)")
    p.add_argument("--ma-backhaul", type=float, default=0.0,
                   help="deterministic MA report-upload delay on top of the per-packet latency (s)")
    p.add_argument("--security-model", default="none", choices=["none", "ecdsa"],
                   help="ecdsa: butterfly-provisioned pseudonyms, real ECDSA-P256 signatures, and "
                        "sig_ok as the result of a verification rather than a flag")
    p.add_argument("--radio-env", choices=["urban", "highway"], default="urban",
                   help="geometric only: TR 37.885 LOS formula family (NLOS always uses urban)")
    p.add_argument("--radio-tx-power-dbm", type=float, default=23.0,
                   help="geometric only: transmit EIRP in dBm (deployed OBUs 20-23; ETSI cap 33)")
    p.add_argument("--radio-rx-sensitivity-dbm", type=float, default=-81.0,
                   help="geometric only: receiver decode floor in dBm (vendored 802.11p: -81)")
    p.add_argument("--radio-nlosb-density-per-km", type=float, default=4.0,
                   help="geometric only, synthetic maps: urban-canyon blocker density per km "
                        "(ignored when the map carries real building footprints)")
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
    p.add_argument("--road", default="linear",
                   choices=["linear", "grid", "ring", "spider", "custom", "sumo"],
                   help="road network model (spider: --grid = arms, --grid-h = rings; "
                        "sumo: import --sumo-net through netimport)")
    # --- SUMO-backed mobility (see mock_pipeline/sumo_trace.py) --------------------------------- #
    p.add_argument("--mobility-source", default="internal",
                   choices=list(_api_registry.builtin_names("mobility")),
                   help="where vehicle movement comes from: internal (routed IDM car-following, the "
                        "default) or sumo_replay (a frozen SUMO trajectory; needs --sumo-trace and "
                        "--road sumo)")
    p.add_argument("--sumo-net", default="", metavar="NET_XML",
                   help="--road sumo: the SUMO .net.xml the engine imports as its road graph "
                        "(with --mobility-source sumo_replay it must be the net the trace was "
                        "frozen on)")
    p.add_argument("--sumo-frame-city", default="", metavar="CITY",
                   help="geo-referenced .net.xml: re-project it into osm.py's local frame for this "
                        "city, so it registers with osm.py roads and building footprints")
    p.add_argument("--sumo-buildings", default="", metavar="POLY_XML",
                   help="--road sumo: a SUMO polygon additional-file (InTAS ships "
                        "buildings.poly.xml) whose type=\"building\" footprints become the "
                        "geometric channel's NLOSb geometry. Projected with the SAME transform as "
                        "the net and gated on landing on its junctions; without it the whole-city "
                        "map has no buildings at all")
    p.add_argument("--sumo-trace", default="", metavar="TRACE",
                   help="--mobility-source sumo_replay: the frozen trajectory artifact "
                        "(python -m scms_sim_ref.mock_pipeline.sumo_trace ...)")
    p.add_argument("--sumo-trace-sha256", default="", metavar="HEX",
                   help="PIN the frozen trajectory: a re-frozen trace (different SUMO seed or "
                        "build) is then refused instead of silently changing data_digest")
    p.add_argument("--sumo-cert-slack", type=float, default=30.0,
                   help="replay: extra certificate lifetime past the trace's exact despawn time (s)")
    p.add_argument("--sumo-offroad-p95-max", type=float, default=8.0,
                   help="replay coherence gate: max p95 distance (m) from a replayed position to "
                        "the engine's nearest road")
    p.add_argument("--custom-network", default="", metavar="JSON_OR_FILE",
                   help='custom map: inline JSON {"nodes":[[x,y]...],"edges":[[a,b]...]} or a file path')
    p.add_argument("--custom-network-directed", action="store_true",
                   help="build the custom map from its directed_edges layer (one-way flags, "
                        "per-direction lane counts, shape polylines) rather than the undirected "
                        "edges array; trimmed to the largest strongly connected component")
    p.add_argument("--directed-lanes", action="store_true",
                   help="give each direction of travel its own carriageway, offset off the road "
                        "centreline (lanes per direction = --lanes); removes head-on overlaps")
    p.add_argument("--drive-side", default="right", choices=["right", "left"],
                   help="which side of the centreline a direction's carriageway sits on")
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
    p.add_argument("--emit-mobility-oracle", action="store_true",
                   help="also write ground_truth/gt_mobility_oracle.jsonl: the mobility BEFORE the "
                        "enforcement gate (every vehicle, every step, including after revocation). "
                        "The unbiased source for datagen.realism_bench's traffic panel; "
                        "gt_emissions_sample.jsonl stops at revocation and is not one. Off by "
                        "default, so the default file set and every pinned digest are unchanged")
    p.add_argument("--no-car-following", action="store_true", help="disable IDM car-following")
    p.add_argument("--turn-slowdown", action="store_true", help="slow into sharp grid corners (realer, harder)")
    p.add_argument("--traffic-lights", action="store_true", help="signalized intersections (grid)")
    p.add_argument("--real-signals", action="store_true",
                   help="drive each junction from the REAL <tlLogic> program the imported .net.xml "
                        "ships (per-movement colour, yellow, permissive 'g' distinct from protected "
                        "'G') instead of the toy 2-phase cycle. Needs --road sumo (or --road custom "
                        "with a map.json written by `netimport --signals`) + --flow. A junction with "
                        "no program keeps today's behaviour exactly; off by default, so every pinned "
                        "digest is unchanged")
    p.add_argument("--sidewalks", action="store_true",
                   help="VRUs walk derived sidewalks and cross at crossings (kerb waits, signal-"
                        "aware where a signal exists) instead of random-walking off-road. Needs "
                        "--vru-pct > 0 and a routed network; draws only from its own rng stream")
    p.add_argument("--sidewalk-width", type=float, default=2.0,
                   help="footway width (m) used to derive sidewalks")
    p.add_argument("--kerb-clearance", type=float, default=0.5,
                   help="gap (m) from the kerb line to the inner edge of the footway")
    p.add_argument("--crossing-wait-max", type=float, default=8.0,
                   help="longest kerb wait (s) at an UNSIGNALISED crossing (gap-acceptance surrogate)")
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
    p.add_argument("--allow-plugin-drift", action="store_true",
                   help="proceed when a replayed manifest's plugin content hashes no longer match "
                        "what is installed. Each drift is still reported on stderr; the new run's "
                        "manifest records what was ACTUALLY loaded, so the change stays visible")
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
            with open(args.check_config, encoding="utf-8-sig") as fh:   # -sig: tolerate a BOM
                cfg = config_from_dict(json.load(fh),
                                       allow_plugin_drift=args.allow_plugin_drift)
            validate_config(cfg)
        except (OSError, ValueError, TypeError, PluginDriftError) as e:
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
        with open(args.config, encoding="utf-8-sig") as fh:             # -sig: tolerate a BOM
            try:
                cfg = config_from_dict(json.load(fh),
                                       allow_plugin_drift=args.allow_plugin_drift)
            except PluginDriftError as e:
                # D4: the manifest's plugin LOCK does not match what is installed now. Fail here,
                # BEFORE step 0, with a non-zero exit -- the alternative (what this code used to do
                # with any unknown key) is a silently different run that exits 0.
                print(f"PLUGIN DRIFT: {e}", file=_sys.stderr)
                return 2
        if args.out:
            cfg.out_dir = args.out
        cfg.verbose = True
        try:
            res = run_pipeline(cfg)
        except ConfigError as e:
            return _plugin_refusal(e)
        _emit_result(res, args.featurize)
        if args.dump_config:
            _dump_config(cfg, args.dump_config)
        return 0
    cfg = PipelineConfig(seed=args.seed, n_vehicles=args.vehicles, n_steps=args.steps,
                         attacker_pct=args.attacker_pct, attack_intensity=args.attack_intensity,
                         attack_mix=args.attack_mix,
                         attack_magnitude_scale=args.attack_magnitude_scale,
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
                         radio_model=args.radio_model, plugins=args.plugins,
                         pathloss_exponent=args.pathloss_exponent,
                         shadowing_sigma_db=args.shadowing_sigma_db,
                         rx_sensitivity_margin_db=args.rx_sensitivity_margin_db,
                         traffic_flow=args.flow, duration_s=args.duration, arrival_rate=args.arrival_rate,
                         road_network=("grid" if (args.flow and args.road == "linear") else args.road),
                         custom_network=_inline_or_file(args.custom_network),
                         custom_network_directed=args.custom_network_directed,
                         directed_lanes=args.directed_lanes, drive_side=args.drive_side,
                         mobility_source=args.mobility_source, sumo_net=args.sumo_net,
                         sumo_frame_city=args.sumo_frame_city, sumo_trace=args.sumo_trace,
                         sumo_buildings=args.sumo_buildings,
                         sumo_trace_sha256=args.sumo_trace_sha256,
                         sumo_cert_slack_s=args.sumo_cert_slack,
                         sumo_offroad_p95_max_m=args.sumo_offroad_p95_max,
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
                         emit_mobility_oracle=args.emit_mobility_oracle,
                         traffic_lights=args.traffic_lights, gap_acceptance=args.gap_acceptance,
                         real_signals=args.real_signals,
                         sidewalks=args.sidewalks, sidewalk_width_m=args.sidewalk_width,
                         kerb_clearance_m=args.kerb_clearance,
                         crossing_wait_max_s=args.crossing_wait_max,
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
                         radio_env=args.radio_env,
                         radio_tx_power_dbm=args.radio_tx_power_dbm,
                         radio_rx_sensitivity_dbm=args.radio_rx_sensitivity_dbm,
                         radio_nlosb_density_per_km=args.radio_nlosb_density_per_km,
                         message_codec=args.message_codec, message_signer=args.message_signer,
                         cam_generation_rules=args.cam_rules, dcc=args.dcc,
                         net_latency_model=args.net_latency, ma_backhaul_s=args.ma_backhaul,
                         security_model=args.security_model,
                         protocol_profile=args.protocol_profile,
                         report_format=args.report_format,
                         verbose=True,
                         out_dir=(args.out or "datasets/poc_run"))
    try:
        res = run_pipeline(cfg)
    except ConfigError as e:
        return _plugin_refusal(e)
    _emit_result(res, args.featurize)
    if args.dump_config:
        _dump_config(cfg, args.dump_config)
    return 0


def _plugin_refusal(e) -> int:
    """A plugin the engine refused to load: a clean, explained, non-zero exit -- never a traceback.

    Narrowed to `ConfigError` on purpose. It is the plugin API's own error class (and a `ValueError`
    subclass, so `validate_config`'s existing contract still holds), so catching it here turns the
    designed refusals -- an unknown ref, a signature mismatch, a reserved capability, a failed
    `conformance = "required"` attestation -- into an operator-legible message, while any OTHER
    exception still surfaces with its traceback rather than being tidied away behind a summary.
    """
    import sys as _sys
    print(f"PLUGIN REFUSED: {e}", file=_sys.stderr)
    return 2


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
