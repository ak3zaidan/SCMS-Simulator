"""Detector-in-the-loop MAP-Elites (Quality-Diversity) misbehavior-scenario foundry.

This turns the deterministic V2X simulator from "a configurable sim" into a *generative data
foundry*: instead of sweeping a hand-authored factorial grid (see ``massive.py``), it runs a closed
adversarial search that produces an archive of misbehavior scenarios that is **diverse by
construction** (one elite per descriptor cell) and **hard by construction** (each elite is the
hardest-to-detect scenario found for its cell). The resulting ``archive.json`` + ``FOUNDRY_REPORT.md``
are a *detector blind-spot map*: where the FIXED default detector fails, and where coverage is thin.

Design (all additive; the engine is imported, never modified):

GENOME
    A scenario is a dict of :class:`PipelineConfig` overrides (a traffic-flow run). Three BASE
    genomes seed the search -- a routed *grid*, a *ring*, and a *spider* city, each with a
    multi-family attacker mix and a duration capped by the driver for loop speed.

OBJECTIVE / FITNESS  (higher == a *better adversarial* scenario == harder to detect, but still VALID)
    Composed from :func:`scms_sim_ref.datagen.validate.validate` signals:

      * ``"evade"``        -> ``1 - recall``            (the MA misses more of the attackers)
      * ``"family:<F>"``   -> ``1 - recall_by_family[F]`` (target one attack family; F must be present)
      * ``"latency"``      -> ``clamp(median_detection_latency_s / duration_s, 0, 1)`` (slow to catch)

    VALIDITY GATE (a scenario is archived only if it is a *real* misbehavior scenario, not an empty
    or degenerate one): it must have produced ``attackers > 0`` AND ``ma_rows > 0`` (reports/detector
    activity exist). Note the gate deliberately does NOT require ``revoked > 0`` -- a scenario with
    many attackers, many reports and ZERO correct revocations (recall == 0) is the *jackpot* for the
    "evade" objective and must stay in the archive. ``family:<F>`` additionally requires family F to
    be present; ``latency`` additionally requires at least one measured detection latency
    (``detection_latency_s.n > 0``). Invalid candidates get fitness 0 and are gated out of the archive.

    IMPORTANT: the search only ever perturbs *adversary* + *environment* + *topology* knobs. It never
    weakens the detector (thresholds, revocation gates, ma_defense are left at their defaults), so the
    archive maps blind spots of the **fixed** production detector -- not of a detector the search
    quietly disabled.

BEHAVIOR DESCRIPTOR  (the QD axes -> an archive cell), :func:`descriptor` -> 4-tuple:
    (a) attack family   -- ``_ATTACK_FAMILY`` of the genome's effective attack types; one of the 9
                           reachable families, or ``"mixed"`` when the genome spans several.
    (b) density band    -- realized ``vehicles`` bucketed sparse (<60) / medium (<120) / dense.
    (c) topology        -- ``road_network`` in {grid, ring, spider}.
    (d) attacker band   -- ``attacker_pct`` bucketed low (<0.15) / med (<0.35) / high.
    Cell space = 10 x 3 x 3 x 3 = 270 cells (see :func:`grid_size`); coverage% = filled / that.

MUTATION  (:func:`mutate`): perturb ONE knob within ``config_schema`` bounds (numeric jitter on
    attacker_pct / arrival_rate / attack_intensity / duty-cycle / crl-aware / radio range / grid dims;
    weather flip; switch attack family or broaden to a mix; per-type magnitude scale; toggle a benign
    realism knob; switch topology). ALWAYS re-validated via ``validate_config``; on an infeasible
    result it resamples (bounded retries) and finally falls back to the (valid) input -> the output is
    ALWAYS feasible. Deterministic given its ``random.Random``.

ARCHIVE  (:class:`Archive`): ``cell -> elite``; ``insert_if_better`` keeps the strictly-higher-fitness
    scenario per cell (ties keep the earlier, deterministic find). Tracks coverage (#cells) and
    QD-score (sum of elite fitnesses).

DETERMINISM: same ``(budget, seed, objective, duration)`` -> byte-identical ``archive.json``. All
    randomness is seeded from the master seed; per-candidate seeds are derived arithmetically; no
    timestamps / wall-clock / ``os.urandom`` are used and no absolute ``out_dir`` is stored.

CLI: ``python -m scms_sim_ref.datagen.foundry --budget 60 --seed 7 --duration 40 --objective evade
     --out datasets/foundry``  (also exposes :func:`run_foundry` for programmatic use).
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import random
import shutil

from scms_sim_ref.mock_pipeline import (  # noqa: E402  (engine hooks -- imported, never modified)
    PipelineConfig,
    config_from_dict,
    config_schema,
    run_pipeline,
    validate_config,
)
from scms_sim_ref.mock_pipeline.run import ATTACK_CATALOG, KNOWN_ATTACK_TYPES  # noqa: E402
from scms_sim_ref.datagen import validate as validate_mod  # noqa: E402
from scms_sim_ref.datagen.featurize import _ATTACK_FAMILY  # noqa: E402  (type -> family, the truth)

# --------------------------------------------------------------------------- #
# Single sources of truth (reused from the engine -- families/types never hardcoded)
# --------------------------------------------------------------------------- #
_SCHEMA = config_schema()
_KNOWN_TYPES = tuple(KNOWN_ATTACK_TYPES)

# family -> the RENDERABLE attack types in it (intersect the family map with what the engine can emit)
_FAMILY_TO_TYPES: dict[str, list[str]] = {}
for _t in _KNOWN_TYPES:
    _FAMILY_TO_TYPES.setdefault(_ATTACK_FAMILY.get(_t, "other"), []).append(_t)
FAMILY_TO_TYPES: dict[str, tuple[str, ...]] = {f: tuple(sorted(ts)) for f, ts in _FAMILY_TO_TYPES.items()}
ATTACK_FAMILIES: tuple[str, ...] = tuple(sorted(FAMILY_TO_TYPES))

# --------------------------------------------------------------------------- #
# Descriptor axes + bins (documented; low cardinality)
# --------------------------------------------------------------------------- #
FAMILY_BINS: tuple[str, ...] = ATTACK_FAMILIES + ("mixed",)   # 9 reachable families + "mixed"
DENSITY_BINS: tuple[str, ...] = ("sparse", "medium", "dense")
DENSITY_EDGES = (60, 120)                                     # realized vehicles: <60 / <120 / >=120
TOPO_BINS: tuple[str, ...] = ("grid", "ring", "spider")
ATTACKER_BINS: tuple[str, ...] = ("low", "med", "high")
ATTACKER_EDGES = (0.15, 0.35)                                 # attacker_pct: <0.15 / <0.35 / >=0.35


def grid_size() -> int:
    """Total number of descriptor cells (the coverage denominator)."""
    return len(FAMILY_BINS) * len(DENSITY_BINS) * len(TOPO_BINS) * len(ATTACKER_BINS)


# --------------------------------------------------------------------------- #
# Base genomes: a grid, a ring, a spider -- each a flow run with a multi-family attacker mix.
# (duration / seed / out_dir are injected by the driver, NOT stored here, so the loop can cap them.)
# --------------------------------------------------------------------------- #
_FLOW_DEFAULTS = dict(traffic_flow=True, car_following=True, n_lanes=2, arrival_rate=2.5,
                      grid_block_m=130.0)
BASE_GENOMES: tuple[dict, ...] = (
    {**_FLOW_DEFAULTS, "road_network": "grid", "grid_w": 5, "grid_h": 5, "attacker_pct": 0.25,
     "attack_types": ("ConstPosOffset", "SlowDrift", "Sybil", "HeadingOffset")},
    {**_FLOW_DEFAULTS, "road_network": "ring", "grid_w": 12, "grid_h": 6, "attacker_pct": 0.25,
     "attack_types": ("RandomSpeed", "AlongRoadOffset", "Sybil", "Teleport")},
    {**_FLOW_DEFAULTS, "road_network": "spider", "grid_w": 6, "grid_h": 4, "attacker_pct": 0.25,
     "attack_types": ("HeadingOffset", "SlowDrift", "InvalidSignature", "StopAndGo")},
)


# --------------------------------------------------------------------------- #
# Small helpers
# --------------------------------------------------------------------------- #
def _jsonable(obj):
    """Recursively convert tuples -> lists so a genome/config round-trips through JSON deterministically."""
    if isinstance(obj, dict):
        return {k: _jsonable(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_jsonable(v) for v in obj]
    return obj


def _clamp(value: float, name: str, lo_default: float, hi_default: float) -> float:
    """Clamp ``value`` to the field's schema [min,max] (falling back to the given defaults)."""
    meta = _SCHEMA.get(name, {})
    lo = meta.get("min")
    hi = meta.get("max")
    lo = lo_default if lo is None else lo
    hi = hi_default if hi is None else hi
    return max(lo, min(hi, value))


def _derive_seed(master_seed: int, counter: int) -> int:
    """Deterministic per-candidate engine seed (same arithmetic style as datagen/massive.py)."""
    return (int(master_seed) + counter * 100003) % 2_000_000_000


def _master_rng(seed: int, objective: str) -> random.Random:
    """Deterministic driver RNG (parent selection + mutation), keyed by seed+objective.

    Uses a hashlib-derived int seed so it is reproducible independent of PYTHONHASHSEED.
    """
    key = hashlib.sha256(f"foundry|{seed}|{objective}".encode()).hexdigest()
    return random.Random(int(key[:16], 16))


def _effective_attack_types(genome: dict) -> tuple[str, ...]:
    """The attack types this genome actually runs (mirrors run_pipeline's narrowing rule)."""
    types = tuple(genome.get("attack_types") or ())
    at = str(genome.get("attack_type") or "")
    if at and (not types or types == tuple(ATTACK_CATALOG)):
        return (at,)
    return types or tuple(ATTACK_CATALOG)


def _family_bin(genome: dict) -> str:
    """Descriptor axis (a): the attack family of the genome's effective types, or 'mixed'."""
    fams = sorted({_ATTACK_FAMILY.get(t, "other") for t in _effective_attack_types(genome)})
    return fams[0] if len(fams) == 1 else "mixed"


# --------------------------------------------------------------------------- #
# Objective / fitness + validity gate
# --------------------------------------------------------------------------- #
def _n_reports(summary: dict) -> int:
    """Total misbehavior reports the detectors actually filed this run (summed over all reason codes).

    Read from ``detector_reliability`` (validate.py), NOT ``ma_rows``: ``ma_rows`` counts EVERY ``ma/``
    row -- including one ``ma_cert_status`` row per observed certificate -- so it is ~always > 0 whenever
    any vehicle exists and cannot tell "the detectors engaged" from "nobody ever looked".
    """
    dr = summary.get("detector_reliability", {}) or {}
    return sum(int(d.get("reports", 0) or 0) for d in dr.values())


def _base_valid(summary: dict) -> bool:
    """Realism/validity gate shared by every objective: a real misbehavior scenario, not an empty one.

    Requires (a) at least one true attacker AND (b) the reporting pipeline actually fired at least one
    misbehavior report (:func:`_n_reports`, not the vacuous ``ma_rows``). This rejects the degenerate
    "attackers present but never observed/reported" scenario, which would otherwise score recall=0 ->
    ``evade`` fitness=1.0 and get archived as the "hardest" elite -- polluting the blind-spot map with
    "nobody looked" cells the objective would then actively chase. A GENUINE detector blind spot (reports
    DID fire but missed the attackers -> recall=0) is intentionally preserved: that is the jackpot the
    search is meant to find. NOTE: with only aggregate report counts we cannot distinguish "attackers
    unobserved but benign false-positives fired" from "attackers observed but unflagged"; a stricter
    observed-attacker gate would need an observation signal validate.py does not emit today.
    """
    return int(summary.get("attackers", 0) or 0) > 0 and _n_reports(summary) > 0


def realism_valid(dataset_dir: str) -> tuple[bool, dict]:
    """Physical-plausibility gate: reject a candidate whose TRAFFIC is kinematically absurd.

    The search maximises "hard to detect", and one cheap way to be hard to detect is to be
    unphysical -- vehicles that teleport, overlap, or brake at 30 m/s^2 produce evidence no real
    detector was ever calibrated against, so an elite won on that basis is a simulator artefact,
    not a blind spot. This is the realism sibling of :func:`_base_valid`'s report-activity gate:
    it rejects only ``datagen.realism_bench``'s HARD metrics (accel bounds, teleports, overlaps and
    the liveness fraction -- the first three are impossibility checks that a scenario in which
    nothing moves would pass trivially), never the SOFT distribution-shape ones, which legitimately
    vary with the scenario.

    Returns ``(valid, realism_summary)``. A dataset the harness cannot score (e.g. sub-sampled
    emissions leave every hard metric ``na``) is NOT rejected -- absence of evidence is not a
    failure -- so the gate is conservative by construction.
    """
    from . import realism_bench
    try:
        card = realism_bench.scorecard(dataset_dir)
    except Exception as exc:                    # noqa: BLE001 -- scoring must never kill the search
        return True, {"error": f"{type(exc).__name__}: {exc}"}
    s = dict(card["summary"])
    s["engine"] = card["probe"]["engine"]
    return not s.get("hard_failures"), s


def fitness(objective: str, summary: dict, duration_s: float) -> tuple[float, bool]:
    """Return ``(fitness, valid)``. Higher fitness == harder-to-detect (better adversarial) scenario.

    ``valid`` is False for scenarios the validity gate rejects (they are never archived).
    """
    if not _base_valid(summary):
        return 0.0, False
    if objective == "evade":
        return 1.0 - float(summary.get("recall", 0.0) or 0.0), True
    if objective.startswith("family:"):
        fam = objective.split(":", 1)[1]
        rbf = summary.get("recall_by_family", {}) or {}
        if fam not in rbf:                       # family not present -> not a test of F -> gated out
            return 0.0, False
        return 1.0 - float(rbf[fam]), True
    if objective == "latency":
        lat = summary.get("detection_latency_s", {}) or {}
        n = int(lat.get("n", 0) or 0)
        if n <= 0:                               # nobody caught -> no latency to measure -> gated out
            return 0.0, False
        denom = float(duration_s) if duration_s and duration_s > 0 else 60.0
        return max(0.0, min(1.0, float(lat.get("median_s", 0.0) or 0.0) / denom)), True
    raise ValueError(f"unknown objective {objective!r} (want evade | family:<F> | latency)")


def descriptor(config: dict, summary: dict) -> tuple:
    """Map a (config, run-summary) to its 4-D archive cell: (family, density, topology, attacker)."""
    fam = _family_bin(config)
    veh = int(summary.get("vehicles", 0) or 0)
    density = (DENSITY_BINS[0] if veh < DENSITY_EDGES[0]
               else DENSITY_BINS[1] if veh < DENSITY_EDGES[1] else DENSITY_BINS[2])
    road = str(config.get("road_network", "grid"))
    topo = road if road in TOPO_BINS else "grid"
    apct = float(config.get("attacker_pct", 0.0) or 0.0)
    atk = (ATTACKER_BINS[0] if apct < ATTACKER_EDGES[0]
           else ATTACKER_BINS[1] if apct < ATTACKER_EDGES[1] else ATTACKER_BINS[2])
    return (fam, density, topo, atk)


# --------------------------------------------------------------------------- #
# Config construction (genome -> validated PipelineConfig)
# --------------------------------------------------------------------------- #
def build_config(genome: dict, seed: int, duration_s: float, out_dir: str) -> PipelineConfig:
    """Build a *validated* PipelineConfig from a genome + run params. Raises on an infeasible genome.

    Injects the flow essentials and the driver-controlled seed/duration/out_dir; ``validate_config``
    is the cheap feasibility oracle (it raises on impossible knob combinations).
    """
    d = dict(genome)
    d.setdefault("traffic_flow", True)
    d.setdefault("car_following", True)
    if "attack_types" in d and isinstance(d["attack_types"], (list, tuple)):
        d["attack_types"] = tuple(d["attack_types"])
    d["seed"] = int(seed)
    d["duration_s"] = float(duration_s)
    d["out_dir"] = out_dir
    cfg = config_from_dict(d)
    validate_config(cfg)
    return cfg


def _config_dict(cfg: PipelineConfig) -> dict:
    """Full, replayable config dict (every field) with the absolute out_dir stripped for determinism."""
    d = _jsonable(dataclasses.asdict(cfg))
    d.pop("out_dir", None)
    return d


# --------------------------------------------------------------------------- #
# Mutation operator -- ONE perturbed knob per call, always re-validated (bounded resample/repair).
# Only adversary / environment / topology knobs are touched; the detector is never weakened.
# --------------------------------------------------------------------------- #
def _m_attacker_pct(g, rng):
    g["attacker_pct"] = round(_clamp(float(g.get("attacker_pct", 0.25)) + rng.uniform(-0.15, 0.15),
                                     "attacker_pct", 0.05, 0.6), 3)
    g["attacker_pct"] = max(0.05, min(0.6, g["attacker_pct"]))


def _m_arrival_rate(g, rng):
    g["arrival_rate"] = round(_clamp(float(g.get("arrival_rate", 2.5)) + rng.uniform(-1.5, 2.0),
                                     "arrival_rate", 0.5, 8.0), 2)
    g["arrival_rate"] = max(0.5, min(8.0, g["arrival_rate"]))


def _m_intensity(g, rng):
    # floor at 0.3 so attacks stay real (an intensity of ~0 makes "attackers" that never falsify,
    # a degenerate way to game recall that the archive must not reward).
    g["attack_intensity"] = round(max(0.3, min(3.0,
                                  float(g.get("attack_intensity", 1.0)) + rng.uniform(-0.5, 0.7))), 3)


def _m_duty_cycle(g, rng):
    g["attack_duty_cycle"] = round(max(0.2, min(1.0,
                                   float(g.get("attack_duty_cycle", 1.0)) + rng.uniform(-0.4, 0.2))), 3)


def _m_crl_aware(g, rng):
    g["crl_aware_pct"] = round(max(0.0, min(0.8,
                               float(g.get("crl_aware_pct", 0.0)) + rng.uniform(-0.2, 0.4))), 3)


def _m_radio_range(g, rng):
    g["radio_range_m"] = round(_clamp(float(g.get("radio_range_m", 500.0)) + rng.uniform(-200, 200),
                                      "radio_range_m", 150.0, 900.0), 1)
    g["radio_range_m"] = max(150.0, min(900.0, g["radio_range_m"]))


def _m_grid_dims(g, rng):
    g["grid_w"] = int(max(4, min(16, int(g.get("grid_w", 6)) + rng.choice([-2, -1, 1, 2]))))
    g["grid_h"] = int(max(3, min(10, int(g.get("grid_h", 6)) + rng.choice([-1, 1]))))


def _m_weather(g, rng):
    g["weather"] = rng.choice(list(_SCHEMA["weather"]["options"]))


def _m_family(g, rng):
    """Collapse to a SINGLE attack family -> a clean per-family archive cell (drives family coverage)."""
    fam = rng.choice(ATTACK_FAMILIES)
    pool = list(FAMILY_TO_TYPES[fam])
    k = min(len(pool), rng.randint(1, 2))
    g["attack_types"] = tuple(sorted(rng.sample(pool, k)))
    g.pop("attack_type", None)
    g.pop("attack_mix", None)


def _m_mix(g, rng):
    """Broaden to several families -> a 'mixed' cell (one representative type per family)."""
    fams = rng.sample(list(ATTACK_FAMILIES), min(len(ATTACK_FAMILIES), rng.randint(2, 3)))
    types = sorted({rng.choice(list(FAMILY_TO_TYPES[f])) for f in fams})
    g["attack_types"] = tuple(types)
    g.pop("attack_type", None)
    g.pop("attack_mix", None)


def _m_magnitude_scale(g, rng):
    """Per-type falsification-magnitude multiplier on top of the global intensity dial."""
    types = list(_effective_attack_types(g))
    if not types:
        return
    t = rng.choice(types)
    g["attack_magnitude_scale"] = f"{t}:{rng.choice([0.5, 0.75, 1.5, 2.0])}"


def _m_realism(g, rng):
    """Toggle/raise a BENIGN realism knob -- adds benign-difficulty noise that masks attacks."""
    knob = rng.choice(["turn_slowdown", "gap_acceptance", "lane_changes",
                       "faulty_pct", "gps_degrade_rate"])
    if knob == "lane_changes":
        g["n_lanes"] = max(2, int(g.get("n_lanes", 2)))     # lane changes need >1 lane + flow
        g["lane_changes"] = not bool(g.get("lane_changes", False))
    elif knob in ("turn_slowdown", "gap_acceptance"):
        g[knob] = not bool(g.get(knob, False))
    elif knob == "faulty_pct":
        g["faulty_pct"] = round(_clamp(float(g.get("faulty_pct", 0.05)) + rng.uniform(0.0, 0.15),
                                       "faulty_pct", 0.0, 0.5), 3)
    else:  # gps_degrade_rate
        g["gps_degrade_rate"] = round(_clamp(float(g.get("gps_degrade_rate", 0.006))
                                             + rng.uniform(0.0, 0.05), "gps_degrade_rate", 0.0, 0.2), 4)


def _m_topology(g, rng):
    """Switch the road topology (dims adjusted to sane ranges for the chosen layout)."""
    road = rng.choice(TOPO_BINS)
    g["road_network"] = road
    if road == "grid":
        g["grid_w"] = int(max(4, min(10, int(g.get("grid_w", 5)))))
        g["grid_h"] = int(max(4, min(10, int(g.get("grid_h", 5)))))
    elif road == "ring":
        g["grid_w"] = int(max(8, min(16, int(g.get("grid_w", 12)))))   # ring: loop intersections
        g["grid_h"] = int(max(4, min(8, int(g.get("grid_h", 6)))))
    else:  # spider
        g["grid_w"] = int(max(5, min(8, int(g.get("grid_w", 6)))))     # spider: arms
        g["grid_h"] = int(max(3, min(6, int(g.get("grid_h", 4)))))     # spider: rings


# (operator, weight). Cell-DEFINING operators (family / topology / band switches) are weighted
# higher so a modest budget spreads across descriptor cells (coverage); the remaining operators tune
# difficulty WITHIN a cell (QD depth). All selection is via rng, so it stays deterministic.
_MUTATOR_WEIGHTS = (
    (_m_family, 5),          # attack-family axis -- the main coverage driver
    (_m_topology, 3),        # topology axis
    (_m_attacker_pct, 3),    # attacker band (+ affects difficulty)
    (_m_arrival_rate, 3),    # density band (+ affects difficulty)
    (_m_mix, 2),             # -> 'mixed' family cell
    (_m_intensity, 2),       # difficulty within a cell
    (_m_duty_cycle, 2),      # difficulty within a cell (bursty evasion)
    (_m_crl_aware, 1),       # difficulty within a cell (feedback-aware evasion)
    (_m_radio_range, 1),
    (_m_grid_dims, 1),
    (_m_weather, 1),
    (_m_magnitude_scale, 1),
    (_m_realism, 1),         # benign realism noise that masks attacks
)
_MUTATORS = tuple(op for op, _ in _MUTATOR_WEIGHTS)
_MUTATOR_WEIGHT_VALUES = tuple(w for _, w in _MUTATOR_WEIGHTS)


def mutate(genome: dict, rng: random.Random, max_tries: int = 24) -> dict:
    """Return a NEW genome one perturbation away from ``genome``, guaranteed feasible.

    Applies one (weighted-random) mutation operator, then validates via ``validate_config``. On an
    infeasible result it resamples (up to ``max_tries``); if every try fails it returns a copy of the
    (valid) input. Deterministic given ``rng``.
    """
    for _ in range(max_tries):
        g = dict(genome)
        rng.choices(_MUTATORS, weights=_MUTATOR_WEIGHT_VALUES, k=1)[0](g, rng)
        if isinstance(g.get("attack_types"), list):
            g["attack_types"] = tuple(g["attack_types"])
        try:
            build_config(g, seed=1, duration_s=30.0, out_dir="__foundry_probe__")
        except Exception:  # noqa: BLE001 -- infeasible mutation; resample
            continue
        return g
    return dict(genome)


# --------------------------------------------------------------------------- #
# Archive (MAP-Elites)
# --------------------------------------------------------------------------- #
class Archive:
    """MAP-Elites archive: one elite scenario per descriptor cell, keeping the highest fitness."""

    def __init__(self):
        self.cells: dict[tuple, dict] = {}
        self.meta: dict = {}

    def insert_if_better(self, cell: tuple, candidate: dict) -> bool:
        """Keep ``candidate`` iff its cell is empty or it strictly beats the incumbent's fitness."""
        cur = self.cells.get(cell)
        if cur is None or candidate["fitness"] > cur["fitness"]:
            self.cells[cell] = candidate
            return True
        return False

    def coverage(self) -> int:
        return len(self.cells)

    def qd_score(self) -> float:
        return round(sum(c["fitness"] for c in self.cells.values()), 6)

    def summary(self, max_cells: int = 24) -> dict:
        """Compact, LLM/GUI-agnostic snapshot of archive state for a mutation operator.

        Reports the descriptor axes/bins, which cells are FILLED, which are EMPTY (the coverage gaps
        an operator should fill), and which are HARDEST (highest fitness == lowest detector recall,
        the failure clusters an operator should intensify). Each sample list is capped at
        ``max_cells``. Pure data -- carries no knowledge of how an operator uses it.
        """
        def _cell_dict(cell: tuple) -> dict:
            return {"attack_family": cell[0], "density_band": cell[1],
                    "topology": cell[2], "attacker_band": cell[3]}
        all_cells = [(f, d, t, a) for f in FAMILY_BINS for d in DENSITY_BINS
                     for t in TOPO_BINS for a in ATTACKER_BINS]
        empty = [c for c in all_cells if c not in self.cells]
        hardest = sorted(self.cells.items(), key=lambda kv: (-kv[1]["fitness"], kv[0]))
        return {
            "axes": {"attack_family": list(FAMILY_BINS), "density_band": list(DENSITY_BINS),
                     "topology": list(TOPO_BINS), "attacker_band": list(ATTACKER_BINS)},
            "grid_size": grid_size(),
            "coverage_cells": len(self.cells),
            "empty_count": len(empty),
            "filled_cells": [_cell_dict(c) for c in sorted(self.cells)[:max_cells]],
            "empty_cells": [_cell_dict(c) for c in empty[:max_cells]],
            "hardest_cells": [{**_cell_dict(cell), "fitness": round(float(rec["fitness"]), 6),
                               "recall": (rec.get("metrics") or {}).get("recall")}
                              for cell, rec in hardest[:max_cells]],
        }


def _candidate(cfg: PipelineConfig, seed: int, duration_s: float, summary: dict,
               fit: float, cell: tuple, genome: dict) -> dict:
    """Assemble the per-cell elite record stored in the archive (fully reproducible, no absolute paths)."""
    return {
        "descriptor": {"attack_family": cell[0], "density_band": cell[1],
                       "topology": cell[2], "attacker_band": cell[3]},
        "fitness": round(float(fit), 6),
        "seed": int(seed),
        "duration_s": float(duration_s),
        "genome": _jsonable(genome),                 # minimal overrides (compact reproducer)
        "config": _config_dict(cfg),                 # full replayable PipelineConfig (no out_dir)
        "metrics": {
            "recall": summary.get("recall"),
            "precision": summary.get("precision"),
            "attackers": summary.get("attackers"),
            "revoked": summary.get("revoked"),
            "false_revocations": summary.get("false_revocations"),
            "vehicles": summary.get("vehicles"),
            "ma_rows": summary.get("ma_rows"),
            "n_reports": _n_reports(summary),            # reports actually filed (the validity signal)
            "recall_by_family": summary.get("recall_by_family", {}),
            "recall_by_type": summary.get("recall_by_type", {}),
            "detection_latency_s": summary.get("detection_latency_s", {}),
        },
    }


# --------------------------------------------------------------------------- #
# Search driver
# --------------------------------------------------------------------------- #
def _evaluate(genome: dict, seed: int, duration_s: float, work_dir: str, objective: str,
              realism_gate: bool = False):
    """Run one candidate to completion and score it. Returns (cfg, summary, fitness, valid).

    ``realism_gate`` (opt-in) additionally runs :func:`realism_valid` over the candidate's dataset
    and vetoes archiving on any HARD physical-plausibility failure, recording the realism summary on
    the run summary either way. Default OFF so the pure-random search stays byte-identical.
    """
    cfg = build_config(genome, seed, duration_s, work_dir)      # may raise -> caller isolates
    run_pipeline(cfg)
    summary = validate_mod.validate(work_dir)[0]
    fit, valid = fitness(objective, summary, duration_s)
    if realism_gate:
        r_ok, r_sum = realism_valid(work_dir)
        summary["realism"] = r_sum
        if not r_ok:
            fit, valid = 0.0, False
    return cfg, summary, fit, valid


def _apply_mutation_fn(mutation_fn, parent: dict, archive: Archive, rng: random.Random) -> dict:
    """Call an injected mutation operator ``mutation_fn(parent, archive_summary, rng) -> genome``.

    Generic and dependency-free -- NO LLM/GUI knowledge lives here. The operator is handed the parent
    genome, a compact :meth:`Archive.summary` (filled / empty / hardest cells + the descriptor axes),
    and the driver RNG. Its returned genome is validated + scored + inserted downstream exactly like a
    random mutation (an infeasible genome is isolated per-candidate in ``_run_one``). If the operator
    returns a non-dict / empty result OR raises, we fall back to the built-in random :func:`mutate`,
    so an external operator can never break the (expensive) search loop.

    This path runs ONLY when a caller passes ``mutation_fn`` to :func:`run_foundry`; the default
    ``mutation_fn=None`` keeps the byte-identical pure-random behaviour. An operator that consults
    external state (e.g. an LLM) makes the run non-deterministic by design.
    """
    try:
        child = mutation_fn(parent, archive.summary(), rng)
        if isinstance(child, dict) and child:
            return child
    except Exception:  # noqa: BLE001 -- a misbehaving operator must not kill the search
        pass
    return mutate(parent, rng)


def run_foundry(budget: int = 60, seed: int = 7, base_duration: float = 40.0,
                out_dir: str = "datasets/foundry", objective: str = "evade",
                verbose: bool = False, mutation_fn=None, realism_gate: bool = False) -> Archive:
    """Run the MAP-Elites loop and write ``archive.json`` + ``FOUNDRY_REPORT.md`` to ``out_dir``.

    Init evaluates the 3 base genomes; then ``budget`` iterations each pick a parent (a base genome
    early on, a random elite later), mutate it, run the sim into a temp dir, score it, and
    ``insert_if_better``. Per-candidate run dirs are deleted to bound disk. Returns the Archive
    (``archive.meta`` carries base-vs-best fitness, coverage and QD-score for programmatic callers).

    ``mutation_fn`` is an OPTIONAL dependency-injected mutation operator with the signature
    ``mutation_fn(parent_genome, archive_summary, rng) -> genome`` (see :func:`_apply_mutation_fn`
    and :meth:`Archive.summary`). When ``None`` (the default) the built-in random :func:`mutate` is
    used and the run is DETERMINISTIC: same (budget, seed, base_duration, objective) -> byte-identical
    archive.json. When provided (e.g. an LLM semantic operator), candidates are proposed by the hook
    but still validated + scored + inserted identically; such runs are NOT byte-identical by design.

    ``realism_gate`` (default OFF, so the historical archive is byte-identical) adds
    :func:`realism_valid` next to the report-activity validity gate: an elite that only "evades"
    because its traffic teleports, overlaps or accelerates outside [-8, +4] m/s^2 is rejected
    instead of archived as a blind spot.
    """
    # Fail fast on a bad objective BEFORE spending the whole (expensive) budget. Without this an unknown
    # objective raises inside every _evaluate, is swallowed per-candidate, and leaves a SILENT empty
    # archive + exit 0; and a family:<F> with F absent from ATTACK_FAMILIES is a typo (not a legitimately
    # empty family run) -- reject it so the caller sees the mistake instead of a coverage=0 "success".
    if objective not in ("evade", "latency"):
        if objective.startswith("family:"):
            fam = objective.split(":", 1)[1]
            if fam not in ATTACK_FAMILIES:
                raise ValueError(f"unknown attack family {fam!r} in objective "
                                 f"(want family:<F> with F in {ATTACK_FAMILIES})")
        else:
            raise ValueError(f"unknown objective {objective!r} (want evade | family:<F> | latency)")
    os.makedirs(out_dir, exist_ok=True)
    work_root = os.path.join(out_dir, "_work")
    shutil.rmtree(work_root, ignore_errors=True)
    os.makedirs(work_root, exist_ok=True)

    archive = Archive()
    rng = _master_rng(seed, objective)
    counter = 0
    base_fitnesses: list[float] = []

    def _run_one(genome: dict):
        """Evaluate one candidate into a throwaway dir, insert if valid, clean up. Returns the
        fitness of an archived (valid) candidate, else None. Failures are isolated (search survives)."""
        nonlocal counter
        counter += 1
        cseed = _derive_seed(seed, counter)
        wdir = os.path.join(work_root, f"c{counter:05d}")
        try:
            cfg, summary, fit, valid = _evaluate(genome, cseed, base_duration, wdir, objective,
                                                 realism_gate=realism_gate)
            if valid:
                cell = descriptor(_config_dict(cfg), summary)
                archive.insert_if_better(cell, _candidate(cfg, cseed, base_duration, summary,
                                                          fit, cell, genome))
                return fit
        except Exception as exc:  # noqa: BLE001 -- isolate a bad candidate; keep the search alive
            if verbose:
                print(f"   [cand {counter}] skipped: {type(exc).__name__}: {exc}", flush=True)
        finally:
            shutil.rmtree(wdir, ignore_errors=True)
        return None

    # 1) init: evaluate the base genomes
    for g in BASE_GENOMES:
        fit = _run_one(g)
        if fit is not None:
            base_fitnesses.append(fit)
    if verbose:
        print(f"[foundry] bases evaluated: {archive.coverage()} cell(s) filled; "
              f"base fitness={[round(x, 3) for x in base_fitnesses]}", flush=True)

    # 2) search: budget mutations (base parent early on, random elite thereafter)
    n_base = len(BASE_GENOMES)
    for it in range(budget):
        if it < n_base or not archive.cells:
            parent = BASE_GENOMES[it % n_base]
        else:
            key = sorted(archive.cells.keys())[rng.randrange(len(archive.cells))]
            parent = archive.cells[key]["genome"]
        if mutation_fn is None:
            child = mutate(parent, rng)                    # default: byte-identical random operator
        else:
            child = _apply_mutation_fn(mutation_fn, parent, archive, rng)   # injected (e.g. LLM)
        _run_one(child)
        if verbose and (it + 1) % 20 == 0:
            print(f"   [{it + 1}/{budget}] coverage={archive.coverage()} "
                  f"qd={archive.qd_score():.3f}", flush=True)

    shutil.rmtree(work_root, ignore_errors=True)

    best = max((c["fitness"] for c in archive.cells.values()), default=0.0)
    base_best = max(base_fitnesses, default=0.0)
    archive.meta = {
        "objective": objective, "seed": seed, "budget": budget, "base_duration_s": base_duration,
        "grid_size": grid_size(), "coverage_cells": archive.coverage(),
        "coverage_pct": round(100.0 * archive.coverage() / grid_size(), 3),
        "qd_score": archive.qd_score(),
        "base_fitnesses": [round(x, 6) for x in base_fitnesses],
        "base_best_fitness": round(base_best, 6), "best_fitness": round(best, 6),
    }
    _write_archive(archive, out_dir)
    _write_report(archive, out_dir)
    return archive


# --------------------------------------------------------------------------- #
# Outputs
# --------------------------------------------------------------------------- #
def _axes_doc() -> dict:
    return {
        "attack_family": {"bins": list(FAMILY_BINS),
                          "from": "config attack types via _ATTACK_FAMILY; 'mixed' if >1 family"},
        "density_band": {"bins": list(DENSITY_BINS), "from": "realized vehicles",
                         "edges": f"sparse<{DENSITY_EDGES[0]}<=medium<{DENSITY_EDGES[1]}<=dense"},
        "topology": {"bins": list(TOPO_BINS), "from": "road_network"},
        "attacker_band": {"bins": list(ATTACKER_BINS), "from": "attacker_pct",
                          "edges": f"low<{ATTACKER_EDGES[0]}<=med<{ATTACKER_EDGES[1]}<=high"},
    }


def _write_archive(archive: Archive, out_dir: str) -> str:
    """Write archive.json (deterministic: sorted cells, no timestamps, no absolute paths)."""
    m = archive.meta
    doc = {
        "objective": m["objective"], "seed": m["seed"], "budget": m["budget"],
        "base_duration_s": m["base_duration_s"],
        "descriptor_axes": _axes_doc(),
        "fitness": {
            "evade": "1 - recall", "family:<F>": "1 - recall_by_family[F]",
            "latency": "clamp(median_detection_latency_s / duration_s, 0, 1)",
            "validity_gate": "attackers>0 AND ma_rows>0 (+ family present / latency n>0)",
            "realism_gate": ("realism_bench HARD metrics (accel bounds / teleports / overlaps) "
                             "when run_foundry(realism_gate=True); OFF by default"),
        },
        "grid_size": m["grid_size"], "coverage_cells": m["coverage_cells"],
        "coverage_pct": m["coverage_pct"], "qd_score": m["qd_score"],
        "base_best_fitness": m["base_best_fitness"], "best_fitness": m["best_fitness"],
        "cells": [dict(cell=list(k), **archive.cells[k]) for k in sorted(archive.cells)],
    }
    path = os.path.join(out_dir, "archive.json")
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(doc, fh, indent=2)
        fh.write("\n")
    return path


def _write_report(archive: Archive, out_dir: str) -> str:
    """Write FOUNDRY_REPORT.md: coverage/QD, hardest cells, base-vs-best, and the coverage gaps."""
    m = archive.meta
    cells = [dict(cell=k, **v) for k, v in archive.cells.items()]
    hardest = sorted(cells, key=lambda c: (-c["fitness"], c["cell"]))[:15]

    lines = []
    lines.append("# Foundry blind-spot map")
    lines.append("")
    lines.append(f"Detector-in-the-loop MAP-Elites archive for objective **{m['objective']}** "
                 f"(seed={m['seed']}, budget={m['budget']}, duration={m['base_duration_s']}s).")
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append(f"- Coverage: **{m['coverage_cells']} / {m['grid_size']} cells "
                 f"({m['coverage_pct']}%)**")
    lines.append(f"- QD-score (sum of elite fitnesses): **{m['qd_score']}**")
    lines.append(f"- Best base-genome fitness: **{m['base_best_fitness']}**  |  "
                 f"Best found fitness: **{m['best_fitness']}**  "
                 f"(search {'improved' if m['best_fitness'] > m['base_best_fitness'] else 'matched'} "
                 f"the objective)")
    lines.append("")
    lines.append("## Hardest cells (lowest detection == highest fitness)")
    lines.append("")
    lines.append("| family | density | topology | attacker | fitness | recall | "
                 "latency_med_s | attackers | revoked | seed |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|")
    for c in hardest:
        fam, dens, topo, atk = c["cell"]
        mt = c["metrics"]
        lat = (mt.get("detection_latency_s") or {}).get("median_s", "")
        lines.append(f"| {fam} | {dens} | {topo} | {atk} | {c['fitness']:.3f} | "
                     f"{mt.get('recall')} | {lat} | {mt.get('attackers')} | "
                     f"{mt.get('revoked')} | {c['seed']} |")
    lines.append("")

    # Coverage gaps: which attack families / topologies are filled vs missing (the blind-spot GAPS).
    filled_fams = {k[0] for k in archive.cells}
    filled_topo = {k[2] for k in archive.cells}
    lines.append("## Coverage gaps")
    lines.append("")
    lines.append(f"- Families filled: {sorted(filled_fams)}")
    lines.append(f"- Families empty:  {sorted(set(FAMILY_BINS) - filled_fams)}")
    lines.append(f"- Topologies filled: {sorted(filled_topo)}")
    lines.append(f"- Topologies empty:  {sorted(set(TOPO_BINS) - filled_topo)}")
    lines.append("")
    # a sample of empty cells (the under-explored corners a bigger budget should target)
    empty = [(f, d, t, a) for f in FAMILY_BINS for d in DENSITY_BINS
             for t in TOPO_BINS for a in ATTACKER_BINS if (f, d, t, a) not in archive.cells]
    lines.append(f"- Empty cells: {len(empty)} of {m['grid_size']} "
                 f"(first 20 shown)")
    for cell in empty[:20]:
        lines.append(f"  - {list(cell)}")
    lines.append("")
    lines.append("## Reproduce an elite")
    lines.append("")
    lines.append("Each cell in `archive.json` carries a full `config` dict. Replay it with:")
    lines.append("")
    lines.append("```python")
    lines.append("from scms_sim_ref.mock_pipeline import config_from_dict, run_pipeline")
    lines.append("cfg = config_from_dict({**cell['config'], 'out_dir': 'datasets/replay'})")
    lines.append("run_pipeline(cfg)   # regenerates that exact scenario")
    lines.append("```")
    lines.append("")

    path = os.path.join(out_dir, "FOUNDRY_REPORT.md")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines))
    return path


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="MAP-Elites quality-diversity foundry: a diverse-by-construction AND "
                    "hard-by-construction archive of V2X misbehavior scenarios (detector blind-spot map).")
    ap.add_argument("--budget", type=int, default=60, help="number of mutation/evaluation iterations")
    ap.add_argument("--seed", type=int, default=7, help="master seed (deterministic archive)")
    ap.add_argument("--duration", type=float, default=40.0, help="per-scenario sim seconds (flow)")
    ap.add_argument("--objective", default="evade",
                    help="evade | family:<F> (e.g. family:stealth) | latency")
    ap.add_argument("--out", default="datasets/foundry", help="output dir (archive.json + report)")
    ap.add_argument("--verbose", action="store_true", help="print progress heartbeat")
    ap.add_argument("--realism-gate", action="store_true",
                    help="also reject candidates that fail datagen.realism_bench's HARD "
                         "physical-plausibility metrics (teleports / overlaps / accel bounds), so an "
                         "elite cannot win by being kinematically absurd (default OFF)")
    a = ap.parse_args(argv)

    archive = run_foundry(budget=a.budget, seed=a.seed, base_duration=a.duration,
                          out_dir=a.out, objective=a.objective, verbose=a.verbose,
                          realism_gate=a.realism_gate)
    m = archive.meta
    print(f"[foundry] objective={m['objective']} seed={m['seed']} budget={m['budget']}")
    print(f"[foundry] coverage={m['coverage_cells']}/{m['grid_size']} ({m['coverage_pct']}%)  "
          f"QD-score={m['qd_score']}")
    print(f"[foundry] best base fitness={m['base_best_fitness']}  best found={m['best_fitness']}")
    if archive.cells:
        hardest = max(archive.cells.items(), key=lambda kv: (kv[1]["fitness"], kv[0]))
        cell, elite = hardest
        print(f"[foundry] hardest cell {list(cell)}  fitness={elite['fitness']:.3f}  "
              f"recall={elite['metrics'].get('recall')}")
    print(f"[foundry] wrote {os.path.join(a.out, 'archive.json')} and "
          f"{os.path.join(a.out, 'FOUNDRY_REPORT.md')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
