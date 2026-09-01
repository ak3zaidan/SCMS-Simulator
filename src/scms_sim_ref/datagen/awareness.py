"""Cooperative awareness, restated so it is measurable and like-for-like with its reference.

`realism_bench.comm_panel` publishes ``comm.awareness_ratio_200m`` and grades it against
``v2x_awareness.awareness_ratio_200m_urban_min`` = 0.90, credited to Boban & d'Orey. On real
Ingolstadt geometry (1519 OSM building polygons) that metric reads **0.115**, and
``docs/realism/PHASE2-GATE.md`` correctly refused to tune the channel to reach 0.90 until somebody
established what the reference had actually measured.

This module is that establishment, plus the arithmetic that makes the two sides comparable. The
conditions are transcribed and cited one file over, in
``refdata/v2x_awareness_conditions.json``; read it before reading this. In summary, the harness
metric and the published number differ in FOUR ways, three of which change the number by more than
the gap the gate was complaining about:

1. **Pair population.** The paper's MEASURED arm (Table III, from which "200 m urban" comes) has a
   denominator of 3-9 *instrumented* DRIVE-C2X vehicles driving a shared test route -- unequipped
   traffic emits nothing and is invisible to the log. Our denominator is every co-present pair in a
   city, most of which are separated by a block of buildings. The measured arm is NOT reproducible
   from our data (per-link LOS state, per-vehicle effective transmit power and route topology are
   all unpublished). The paper's SIMULATED arm (Section V: GEMV^2, 2410 vehicles over the core of
   Porto, real OSM buildings, LOS/NLOSv/NLOSb) IS all-pairs and IS the right anchor.
2. **Shot multiplicity.** NAR is a ">= 1 message received in t = 1 s" metric at 10 Hz CAM. The
   paper's own model is ``NAR = 1 - (1 - PDR)^Z`` with Z fitted between 2.14 and 8.29 (5.46 for the
   one urban dataset). So NAR = 0.90 corresponds to a PER-PACKET PDR of ``1 - 0.1^(1/Z)`` = 0.24 to
   0.66, centred near 0.34-0.42 -- **about one third, not nine tenths**. The mock engine emits one
   CAM per second (``dt = 1.0``), i.e. N = 1 and hence Z = 1, so its awareness ratio *is* a
   per-packet PDR and grading it at 0.90 asks for roughly 2.5x the delivery the reference did.
3. **Link budget.** The reference simulation assumed a **-95 dBm** receiver; ours is -81 dBm. Its
   urban "0.90 at 200 m" point is at 15 dBm, i.e. a **110 dB** budget. Ours at 23 dBm / -81 dBm is
   **104 dB** -- 6 dB short of the configuration that produced the anchor. The paper explicitly
   licenses trading sensitivity against power dB-for-dB, which is what makes its curve transferable.
4. **Level vs crossing.** Table III and Fig. 18 report *the distance at which NAR falls below 0.90*,
   not the awareness value at a fixed 200 m. The comparable harness quantity is a crossing distance.

(The annulus convention is the one thing the harness already had right: ``_curve_at`` averages the
bins overlapping ``[d - bin/2, d + bin/2]``, which is the paper's 50 m NAR bin.)

WHAT THIS MODULE COMPUTES
-------------------------
* :func:`link_state_composition` -- the fraction of co-present pairs per distance band that are
  LOS / NLOSv / NLOSb, on the scenario's own geometry, using **the same building raster and vehicle
  blocker index the channel itself used** (imported from ``mock_pipeline.run``, never re-implemented
  -- a second implementation of the ray march would be a second opinion, not a measurement). This is
  the quantity that explains the 0.115, and without it an awareness number is uninterpretable.
* :func:`propagation_pdr` -- the PROPAGATION-ONLY per-packet delivery probability for one link state
  at one distance, by exact quadrature over the TR 37.885 shadowing, the NLOSv censored-Gaussian
  blockage loss and the Nakagami-m fade against the hard decode floor. No congestion, no collision,
  no weather: the reference simulation has none either ("we do not consider interference ... the
  results in this section are an upper bound"), so including them would be the same class of
  mismatch this module exists to remove.
* :func:`mixed_pdr_curve` -- (1) x (2): an ABSOLUTE PDR-vs-distance curve for this scenario. The
  existing harness curve is *proportional* to PDR (normalised at the near band), so it cannot be
  compared to an absolute threshold at all; this one can.
* :func:`nar_from_pdr` / :func:`pdr_for_nar` -- the paper's eq. (4), so a per-packet curve can be
  read as NAR at any CAM rate, and a NAR threshold can be read as a per-packet threshold.
* :func:`reference_nar90_distance_m` -- the paper's own simulated awareness-vs-power curve,
  interpolated **at our link budget instead of its own**. That is the step the retired gate skipped.
  Urban only: the paper pins a single highway point, and one point is not a curve.
* :func:`gray_zone_ratio` -- the dimensionless d20/d90, which unlike an absolute gray-zone width
  cannot be passed by making the radio quieter.

TRUST FIREWALL: read-only over a dataset directory, aggregate output only, no per-entity value, no
RNG, no timestamps. Same contract as ``realism_bench``.

    python -m scms_sim_ref.datagen.awareness <dataset_dir> [--json out.json] [--markdown]
"""

from __future__ import annotations

import argparse
import json
import math
import os

import numpy as np

REFDATA_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "refdata")

# --- analysis tunables ---------------------------------------------------------------------------
DIST_BIN_M = 50.0             # the reference's NAR bin width (v2x_awareness_conditions.nar_definition)
MAX_DIST_M = 1000.0
T_BUCKET_S = 1.0              # co-presence snapshot width == the reference's t = 1 s window
MAX_SNAPSHOTS = 240           # deterministic even-spaced subsample of time buckets
MAX_PAIRS_CLASSIFIED = 400_000    # ray-march budget; pairs are taken in deterministic order
MIN_BAND_PAIRS = 30           # a band needs this many classified pairs before its mix is reported
AWARENESS_ANCHORS_M = (100.0, 200.0, 300.0)
NAR_LEVEL = 0.90              # the level Table III and Fig. 18 report crossings of

# Numerical quadrature (deterministic; no RNG anywhere in this module).
_FADE_LN_LO, _FADE_LN_HI, _FADE_N = math.log(1e-9), math.log(100.0), 1201
_NLOSV_N = 401                # grid points over the censored-Gaussian blockage loss

_STATES = ("LOS", "NLOSv", "NLOSb")


# =================================================================================================
# refdata
# =================================================================================================
def load_conditions(refdata_dir: str | None = None) -> dict:
    """Load ``refdata/v2x_awareness_conditions.json``; ``{}`` if it is not present."""
    p = os.path.join(os.fspath(refdata_dir or REFDATA_DIR), "v2x_awareness_conditions.json")
    if not os.path.exists(p):
        return {}
    with open(p, encoding="utf-8") as fh:
        return json.load(fh)


def _entry(cond: dict, key: str) -> dict:
    return ((cond.get("entries") or {}).get(key) or {})


# =================================================================================================
# the paper's awareness model -- eq. (3) and (4), Section IV
# =================================================================================================
def nar_from_pdr(pdr: float, z: float) -> float:
    """Boban & d'Orey eq. (4): ``NAR = 1 - (1 - PDR)^Z``.

    Z is the *effective* number of independent CAM transmissions inside the awareness window: it is
    bounded above by the literal count N, and the paper fits it between 2.14 and 8.29 because real
    CAM losses arrive in bursts, so N shots are worth fewer than N independent chances.
    """
    p = min(max(float(pdr), 0.0), 1.0)
    return 1.0 - (1.0 - p) ** max(float(z), 0.0)


def pdr_for_nar(nar: float, z: float) -> float:
    """Inverse of :func:`nar_from_pdr`: the per-packet delivery a NAR target implies at this Z.

    ``pdr_for_nar(0.90, 5.4579) = 0.3442`` -- the reference's "90% awareness" in the one urban
    dataset it fitted asks for about a third of packets to arrive, not nine tenths of them.
    """
    n = min(max(float(nar), 0.0), 1.0)
    zz = max(float(z), 1e-9)
    return 1.0 - (1.0 - n) ** (1.0 / zz)


def z_for_engine(dt_s: float, window_s: float = 1.0, z_cap: float | None = None) -> float:
    """Effective shot multiplicity available to a run that emits one CAM every ``dt_s``.

    ``Z <= N`` (eq. 4), and N is the number of CAMs inside the window. A 1 Hz engine in a 1 s window
    has N = 1, so Z = 1 no matter what the reference fitted: **its awareness ratio is a per-packet
    PDR**. ``z_cap`` (the reference's fitted Z) caps the result from above for a faster engine.
    """
    n = math.floor(max(float(window_s), 0.0) / max(float(dt_s), 1e-9) + 1e-9)
    n = max(1.0, float(n))
    return n if z_cap is None else min(n, float(z_cap))


def link_budget_db(tx_power_dbm: float, rx_sensitivity_dbm: float) -> float:
    """EIRP minus receiver sensitivity. The paper trades these dB-for-dB (Section V-E)."""
    return float(tx_power_dbm) - float(rx_sensitivity_dbm)


def reference_nar90_distance_m(budget_db: float, radio_env: str = "urban",
                               cond: dict | None = None) -> tuple[float | None, str]:
    """The reference's OWN simulated 90%-NAR crossing distance, at *our* link budget.

    URBAN: log-linear interpolation through the three pinned Fig. 18 points (100 dB -> 50 m,
    110 dB -> 200 m, 118 dB -> 300 m). ``None`` outside that range -- the paper's own 250-vs-300 m
    inconsistency at the top end is already as much extrapolation as the data will carry.

    HIGHWAY: the paper pins ONE simulated point (400 m at 20 dBm into a -95 dBm receiver = 115 dB).
    One point is not a curve, so nothing is interpolated and nothing is graded; the point is
    returned only when the budget matches it within 1 dB, and otherwise reported as context.
    Applying the urban curve to a highway run -- which an earlier draft of this module did -- is the
    same category error the module exists to remove.
    """
    cond = cond if cond is not None else load_conditions()
    env = str(radio_env or "urban").lower()
    if env != "urban":
        hw = _entry(cond, "simulated_highway_nar90").get("value") or {}
        hb, hd = hw.get("link_budget_db"), hw.get("distance_m")
        if hb is None or hd is None:
            return None, "no pinned highway reference"
        if abs(float(budget_db) - float(hb)) <= 1.0:
            return float(hd), (f"the single pinned highway point ({hd:.0f} m at {hb:.0f} dB); "
                               f"this run's budget matches it to within 1 dB")
        return None, (f"the reference pins ONE highway point ({hd:.0f} m at {hb:.0f} dB) and this "
                      f"run is at {float(budget_db):.1f} dB; a single point cannot be interpolated, "
                      f"and the urban curve does not transfer to a highway scene")
    pts = _entry(cond, "simulated_urban_nar90_by_link_budget").get("points") or []
    if len(pts) < 2:
        return None, "no pinned reference curve (refdata/v2x_awareness_conditions.json missing)"
    xs = np.array([float(p[1]) for p in pts], dtype=float)
    ys = np.log10(np.array([float(p[2]) for p in pts], dtype=float))
    b = float(budget_db)
    if b < xs.min() or b > xs.max():
        return None, (f"link budget {b:.1f} dB is outside the pinned reference range "
                      f"[{xs.min():.0f}, {xs.max():.0f}] dB; the curve is not extrapolated")
    return float(10.0 ** np.interp(b, xs, ys)), (
        f"log-linear interpolation of the pinned Fig. 18 urban points at {b:.1f} dB "
        f"(the reference's own -95 dBm receiver is folded into the budget)")


def gray_zone_ratio(d90: float | None, d20: float | None) -> float | None:
    """Dimensionless d20/d90. Unlike an absolute width in metres, lowering transmit power does not
    inflate it -- ``refdata/v2x_awareness.gray_zone_ratio_from_shadowing`` pins 2.41 urban LOS,
    2.08 highway LOS, 1.92 urban NLOS as shadowing-only LOWER bounds."""
    if not d90 or not d20 or d90 <= 0:
        return None
    return float(d20) / float(d90)


# =================================================================================================
# propagation-only delivery probability
# =================================================================================================
#: Abramowitz & Stegun 26.2.17 coefficients. numpy carries no erf and scipy is not a dependency of
#: this repository, so the standard normal CDF is evaluated with the A&S rational approximation
#: (|error| < 7.5e-8). `tests/test_awareness.py` pins that against `math.erf` on a dense grid, so
#: the approximation is a measured quantity here rather than an assumed one. A `np.vectorize` over
#: `math.erf` would be exact and ~40x slower on the 10^6-node quadrature grids below.
_AS_P = 0.2316419
_AS_B = (0.319381530, -0.356563782, 1.781477937, -1.821255978, 1.330274429)


def _norm_cdf(x):
    """Standard normal CDF, vectorised (A&S 26.2.17). Symmetric branch keeps the tail accurate."""
    z = np.asarray(x, dtype=float)
    a = np.abs(z)
    t = 1.0 / (1.0 + _AS_P * a)
    poly = t * (_AS_B[0] + t * (_AS_B[1] + t * (_AS_B[2] + t * (_AS_B[3] + t * _AS_B[4]))))
    upper = np.exp(-0.5 * a * a) / math.sqrt(2.0 * math.pi) * poly     # = 1 - Phi(|z|)
    return np.where(z >= 0.0, 1.0 - upper, upper)


def _fade_quadrature(m: float) -> tuple[np.ndarray, np.ndarray]:
    """Deterministic quadrature nodes/weights for the Nakagami power gain ``G ~ Gamma(m, 1/m)``.

    Integrating in ``u = ln g`` (uniform grid, integrand ``f(g)*g``) keeps the sharp low-g tail
    resolved without an adaptive scheme. Unit MEAN by construction, matching
    ``run.gammavariate(m, 1.0/m)``: fading redistributes power, it does not add any.
    """
    u = np.linspace(_FADE_LN_LO, _FADE_LN_HI, _FADE_N)
    g = np.exp(u)
    logf = m * math.log(m) - math.lgamma(m) + (m - 1.0) * u - m * g
    w = np.exp(logf + u)                      # f(g) * g  (the Jacobian of g = e^u)
    du = u[1] - u[0]
    w = w * du
    w[0] *= 0.5
    w[-1] *= 0.5
    s = w.sum()
    return g, (w / s if s > 0 else w)         # renormalise: quadrature error must not bias the PDR


def _nlosv_quadrature(mu: float, sigma: float) -> tuple[np.ndarray, np.ndarray]:
    """Nodes/weights for the TR 37.885 NLOSv extra loss ``L = max(0, N(mu, sigma))``.

    A censored Gaussian: an atom of mass ``Phi(-mu/sigma)`` at L = 0 plus the positive tail. The
    engine draws exactly this (``max(0.0, prng.gauss(mu, sigma))``) rather than shortcutting to the
    mean, so the quadrature has to carry the atom or it will over-attenuate the LOS-like fraction.
    """
    if sigma <= 0.0:
        return np.array([max(0.0, mu)]), np.array([1.0])
    atom = float(_norm_cdf(np.array([-mu / sigma]))[0])
    lo, hi = 0.0, mu + 10.0 * sigma
    l = np.linspace(lo, hi, _NLOSV_N)
    dens = np.exp(-0.5 * ((l - mu) / sigma) ** 2) / (sigma * math.sqrt(2.0 * math.pi))
    dl = l[1] - l[0]
    w = dens * dl
    w[0] *= 0.5
    w[-1] *= 0.5
    tail = w.sum()
    target = 1.0 - atom
    if tail > 0:
        w = w * (target / tail)
    nodes = np.concatenate(([0.0], l))
    weights = np.concatenate(([atom], w))
    return nodes, weights


def propagation_pdr(state: str, d_m: float, *, tx_power_dbm: float, decode_floor_dbm: float,
                    radio_env: str = "urban") -> float:
    """PROPAGATION-ONLY probability that one packet decodes on one link of this state at this range.

    Exactly the engine's own delivery test -- ``rx_dbm >= decode_floor`` after TR 37.885 mean path
    loss, the per-state Gudmundson shadowing (whose *marginal* is N(0, sigma) regardless of the AR(1)
    correlation, so this is exact and not an approximation), the censored-Gaussian NLOSv blockage,
    and the Nakagami-m fade -- integrated instead of sampled.

    Deliberately excludes congestion, hidden-terminal collision and weather. The reference this feeds
    ("Boban & d'Orey Section V") states it models no interference and is therefore an upper bound;
    comparing a congestion-inclusive number against it would re-introduce the very mismatch this
    module removes.
    """
    from ..mock_pipeline.run import (TR37885_NLOSV, TR37885_SHADOW_SIGMA_DB,
                                     nakagami_m_for_distance, tr37885_nlosv_mu_db,
                                     tr37885_pathloss_db)
    if state not in _STATES:
        raise ValueError(f"unknown link state {state!r} (have {list(_STATES)})")
    d = max(float(d_m), 1.0)
    los_state = "urban_los" if radio_env == "urban" else "highway_los"
    pl = tr37885_pathloss_db("urban_nlos" if state == "NLOSb" else los_state, d)
    sigma = TR37885_SHADOW_SIGMA_DB[state]
    margin = float(tx_power_dbm) - pl - float(decode_floor_dbm)

    g, wg = _fade_quadrature(nakagami_m_for_distance(d))
    fade_db = 10.0 * np.log10(np.maximum(g, 1e-300))

    if state == "NLOSv":
        # V2V antennas sit at 1.5 m and the shortest blocker is 1.6 m, so both endpoints are below
        # the blocker: TR37885_NLOSV["both_below"]. run.py picks the same branch.
        mu_base, sig_v = TR37885_NLOSV["both_below"]
        nodes, wl = _nlosv_quadrature(tr37885_nlosv_mu_db(mu_base, d), sig_v)
        z = (margin + fade_db[:, None] - nodes[None, :]) / sigma
        return float((_norm_cdf(z) * wg[:, None] * wl[None, :]).sum())
    return float((_norm_cdf((margin + fade_db) / sigma) * wg).sum())


def decode_floor_dbm(rx_sensitivity_dbm: float) -> float:
    """The engine's actual decode floor: ``max(sensitivity, noise + SNIR threshold)``."""
    from ..mock_pipeline.run import PHY_NOISE_DBM, PHY_SNIR_THRESHOLD_DB
    return max(float(rx_sensitivity_dbm), PHY_NOISE_DBM + PHY_SNIR_THRESHOLD_DB)


def mixed_pdr_curve(composition: dict, *, tx_power_dbm: float, rx_sensitivity_dbm: float,
                    radio_env: str = "urban") -> dict:
    """Absolute PDR-vs-distance for this scenario: per-state delivery weighted by the MEASURED mix.

    ``composition`` is :func:`link_state_composition`'s output. Each band's PDR is evaluated at the
    band centre and mixed as ``sum_s f_s(d) * PDR(d | s)``. Bands with too few classified pairs are
    left as ``None`` rather than being filled from a neighbour.
    """
    floor = decode_floor_dbm(rx_sensitivity_dbm)
    edges = np.asarray(composition["edges"], dtype=float)
    centres = (edges[:-1] + edges[1:]) / 2.0
    frac = composition["fraction"]
    n = composition["n_pairs"]
    pdr, per_state = [], {s: [] for s in _STATES}
    for i, c in enumerate(centres):
        ps = {s: propagation_pdr(s, float(c), tx_power_dbm=tx_power_dbm,
                                 decode_floor_dbm=floor, radio_env=radio_env) for s in _STATES}
        for s in _STATES:
            per_state[s].append(ps[s])
        if n[i] < MIN_BAND_PAIRS:
            pdr.append(float("nan"))
            continue
        pdr.append(sum(frac[s][i] * ps[s] for s in _STATES))
    return {"edges": edges, "centres": centres, "pdr": np.array(pdr, dtype=float),
            "per_state_pdr": {s: np.array(v, dtype=float) for s, v in per_state.items()},
            "n_pairs": np.asarray(n), "tx_power_dbm": float(tx_power_dbm),
            "rx_sensitivity_dbm": float(rx_sensitivity_dbm), "decode_floor_dbm": floor,
            "link_budget_db": link_budget_db(tx_power_dbm, floor), "radio_env": radio_env}


def curve_value_at(curve: dict, dist_m: float, bin_m: float = DIST_BIN_M) -> float | None:
    """PDR over the band of half a bin either side of ``dist_m`` -- the reference's annulus."""
    edges, y = curve["edges"], curve["pdr"]
    lo, hi = dist_m - bin_m / 2.0, dist_m + bin_m / 2.0
    sel = (edges[:-1] < hi - 1e-9) & (edges[1:] > lo + 1e-9) & np.isfinite(y)
    if not sel.any():
        return None
    w = np.asarray(curve["n_pairs"], dtype=float)[sel]
    if w.sum() <= 0:
        return float(np.mean(y[sel]))
    return float(np.average(y[sel], weights=w))


def crossing_m(curve: dict, level: float) -> float | None:
    """Distance at which the curve drops below ``level`` for good (linear interpolation).

    Read off the least non-increasing majorant (running maximum from the far end), for the same
    reason ``realism_bench._crossing`` does: PDR is non-increasing in distance by construction, so a
    lone noisy dip must not be allowed to move the crossing inward. Here the curve is analytic and
    already monotone in practice, which makes the majorant a no-op and the two estimators
    directly comparable.
    """
    x, y = np.asarray(curve["centres"], dtype=float), np.asarray(curve["pdr"], dtype=float)
    ok = np.isfinite(y)
    if ok.sum() < 2:
        return None
    xs = x[ok]
    ys = np.maximum.accumulate(y[ok][::-1])[::-1]
    if ys[0] < level:
        return None
    for i in range(1, xs.size):
        if ys[i - 1] >= level > ys[i]:
            span = ys[i - 1] - ys[i]
            if span <= 0:
                return float(xs[i])
            return float(xs[i - 1] + (xs[i] - xs[i - 1]) * (ys[i - 1] - level) / span)
    return None


# =================================================================================================
# geometry: the LOS / NLOSv / NLOSb composition of the pair population
# =================================================================================================
def _jsonl(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(x) for x in fh if x.strip()]


def load_scenario(dataset_dir: str) -> dict:
    """Everything the geometry needs, read out of one dataset directory.

    Returns the radio config, the building polygons (from the custom-network document the run was
    built from -- byte-identical to what the channel rasterised), the per-vehicle blocker heights,
    and the emission records.
    """
    dataset_dir = os.fspath(dataset_dir)
    man: dict = {}
    mp = os.path.join(dataset_dir, "manifest.json")
    if os.path.exists(mp):
        try:
            with open(mp, encoding="utf-8") as fh:
                man = json.load(fh)
        except (OSError, json.JSONDecodeError):
            man = {}
    cfg = man.get("config") if isinstance(man.get("config"), dict) else {}

    buildings: list = []
    src = "none"
    raw = cfg.get("custom_network")
    if raw:
        try:
            doc = json.loads(raw) if isinstance(raw, str) else raw
            polys = doc.get("buildings") if isinstance(doc, dict) else None
            if polys:
                buildings = [[(float(p[0]), float(p[1])) for p in ring] for ring in polys]
                src = "custom_network.buildings"
        except (TypeError, ValueError, KeyError):
            buildings, src = [], "custom_network unparseable"

    from ..mock_pipeline.run import TR37885_BLOCKER_HEIGHT_M
    heights: dict[str, float] = {}
    for v in _jsonl(os.path.join(dataset_dir, "ground_truth", "gt_vehicle.jsonl")):
        vid = v.get("true_vehicle_id")
        if vid is None:
            continue
        heights[str(vid)] = float(TR37885_BLOCKER_HEIGHT_M.get(str(v.get("veh_type")), 1.6))

    return {
        "dataset_dir": dataset_dir,
        "config": cfg,
        "buildings": buildings,
        "buildings_source": src,
        "blocker_height_m": heights,
        # Emissions only. Reports are deliberately NOT read here: this path reconstructs awareness
        # from GEOMETRY and the configured physics, with no dependence on the detector's report
        # stream at all. That independence is what makes it a second opinion on
        # comm.awareness_ratio_* rather than a restatement of it.
        "emissions": _jsonl(os.path.join(dataset_dir, "ground_truth",
                                         "gt_emissions_sample.jsonl")),
        "radio_model": str(cfg.get("radio_model") or ""),
        "tx_power_dbm": float(cfg.get("radio_tx_power_dbm", 23.0) or 23.0),
        "rx_sensitivity_dbm": float(cfg.get("radio_rx_sensitivity_dbm", -81.0) or -81.0),
        "radio_env": str(cfg.get("radio_env") or "urban"),
        "nlosb_density_per_km": float(cfg.get("radio_nlosb_density_per_km", 0.0) or 0.0),
        "dt_s": float(cfg.get("dt", 1.0) or 1.0),
        "emit_sample_prob": float(cfg.get("emit_sample_prob", 1.0) or 0.0),
    }


def snapshots(emissions: list[dict], bucket_s: float = T_BUCKET_S,
              max_snaps: int = MAX_SNAPSHOTS) -> list[dict]:
    """Co-presence snapshots: one true position per vehicle per ``bucket_s`` window.

    ``bucket_s`` defaults to 1.0 s, which is the reference's own awareness window t. Buckets are
    subsampled evenly across the run and vehicles kept in sorted-id order, so nothing here is
    random and the same dataset always yields the same snapshots. Unlike
    ``realism_bench._snapshots`` no per-bucket vehicle cap is applied: every vehicle present has to
    be in the blocker index or the NLOSv classification is wrong.
    """
    buckets: dict[int, dict[str, tuple[float, float, float]]] = {}
    for e in emissions:
        vid = e.get("true_vehicle_id")
        if vid is None:
            continue
        try:
            t = float(e["t"]); x = float(e["true_x"]); y = float(e["true_y"])
        except (KeyError, TypeError, ValueError):
            continue
        b = int(math.floor(t / bucket_s))
        cur = buckets.setdefault(b, {}).get(str(vid))
        if cur is None or t >= cur[0]:
            buckets[b][str(vid)] = (t, x, y)
    keys = sorted(buckets)
    if len(keys) > max_snaps:
        step = len(keys) / float(max_snaps)
        keys = [keys[int(i * step)] for i in range(max_snaps)]
    out = []
    for b in keys:
        vids = sorted(buckets[b])
        if len(vids) < 2:
            continue
        out.append({"bucket": b, "vids": vids,
                    "x": np.array([buckets[b][v][1] for v in vids], dtype=float),
                    "y": np.array([buckets[b][v][2] for v in vids], dtype=float)})
    return out


def link_state_composition(scenario: dict, *, bin_m: float = DIST_BIN_M,
                           max_dist_m: float = MAX_DIST_M,
                           max_pairs: int = MAX_PAIRS_CLASSIFIED,
                           snaps: list[dict] | None = None) -> dict:
    """LOS / NLOSv / NLOSb fraction of the co-present pair population, per distance band.

    The classification is the channel's own, imported not re-implemented: ``_BuildingRaster`` for
    the NLOSb ray march and ``_VehicleBlockerIndex`` for the NLOSv body test, with the same
    endpoint-clearance and half-width constants. A separate implementation here would be a second
    opinion about the geometry, and the whole point of this module is to stop comparing two
    different measurements.

    With no building polygons the run used the synthetic urban-canyon fallback -- a Poisson blockage
    process with ``P(LOS) = exp(-lambda*d)``. That is not reproducible pair-by-pair without the
    engine's RNG stream, but its EXPECTATION is exact, so those bands report the analytic
    ``1 - exp(-lambda*d)`` NLOSb fraction and say so in ``nlosb_method``.
    """
    from ..mock_pipeline.run import (GEO_ENDPOINT_CLEAR_M, V2X_ANTENNA_HEIGHT_M, _BuildingRaster,
                                     _VehicleBlockerIndex)
    snaps = snapshots(scenario["emissions"]) if snaps is None else snaps
    edges = np.arange(0.0, max_dist_m + bin_m, bin_m)
    nb = edges.size - 1
    counts = {s: np.zeros(nb, dtype=np.int64) for s in _STATES}
    heights = scenario["blocker_height_m"]
    raster = _BuildingRaster(scenario["buildings"]) if scenario["buildings"] else None
    lam = max(0.0, scenario.get("nlosb_density_per_km", 0.0)) / 1000.0
    ant = V2X_ANTENNA_HEIGHT_M
    nlosb_method = ("building_raster" if raster is not None else
                    ("canyon_density_expectation" if lam > 0 else "no_blockage_model"))

    classified = 0
    truncated = False
    for s in snaps:
        vids, xs, ys = s["vids"], s["x"], s["y"]
        n = len(vids)
        if n < 2:
            continue
        idx = _VehicleBlockerIndex()
        idx.rebuild([(vids[k], float(xs[k]), float(ys[k]), heights.get(vids[k], 1.6))
                     for k in range(n)])
        for i in range(n - 1):
            if truncated:
                break
            xi, yi = float(xs[i]), float(ys[i])
            for j in range(i + 1, n):
                d = math.hypot(xi - float(xs[j]), yi - float(ys[j]))
                if d >= max_dist_m:
                    continue
                b = int(d // bin_m)
                if b >= nb:
                    continue
                if raster is not None:
                    blocked = raster.blocked(xi, yi, float(xs[j]), float(ys[j]),
                                             GEO_ENDPOINT_CLEAR_M)
                else:
                    blocked = False           # canyon expectation is applied after the loop
                if blocked:
                    counts["NLOSb"][b] += 1
                else:
                    h = idx.tallest_blocker(xi, yi, float(xs[j]), float(ys[j]), vids[i], vids[j])
                    # run.py: `below = (tx_h < h) + (rx_h < h)`; NLOSv iff at least one antenna is
                    # under the blocker. Both V2V antennas sit at V2X_ANTENNA_HEIGHT_M, so the two
                    # terms are the same test -- kept as one to make that explicit.
                    counts["NLOSv" if (h > 0.0 and ant < h) else "LOS"][b] += 1
                classified += 1
                if classified >= max_pairs:
                    truncated = True
                    break

    n_pairs = sum(counts[s] for s in _STATES)
    frac = {s: np.zeros(nb, dtype=float) for s in _STATES}
    for b in range(nb):
        tot = float(n_pairs[b])
        if tot <= 0:
            continue
        for s in _STATES:
            frac[s][b] = counts[s][b] / tot
    if raster is None and lam > 0.0:
        # Synthetic map: split each band's non-NLOSv/LOS-classified pairs by the analytic canyon
        # blockage probability at the band centre. Applied to the WHOLE band population, since the
        # engine draws the canyon verdict before it ever looks at a vehicle blocker.
        centres = (edges[:-1] + edges[1:]) / 2.0
        pb = 1.0 - np.exp(-lam * centres)
        for b in range(nb):
            if n_pairs[b] <= 0:
                continue
            keep = 1.0 - pb[b]
            frac["NLOSb"][b] = pb[b]
            frac["LOS"][b] *= keep
            frac["NLOSv"][b] *= keep

    # POPULATION-WEIGHTED overall mix, not a sum of `count`. Under the canyon fallback the NLOSb
    # share lives only in `fraction` (it is an expectation, never an incremented counter), so
    # aggregating the counters would report NLOSb = 0 for every synthetic-map run.
    w = np.asarray(n_pairs, dtype=float)
    tot_w = float(w.sum())
    overall = ({s: float(np.average(frac[s], weights=w)) for s in _STATES} if tot_w > 0
               else {s: 0.0 for s in _STATES})
    return {"edges": edges, "count": {s: counts[s] for s in _STATES},
            "fraction": frac, "overall": overall, "n_pairs": n_pairs,
            "n_classified": int(classified),
            "truncated": bool(truncated), "snapshots": len(snaps),
            "nlosb_method": nlosb_method, "n_buildings": len(scenario["buildings"]),
            "bin_m": float(bin_m), "max_dist_m": float(max_dist_m)}


def composition_at(comp: dict, dist_m: float, bin_m: float | None = None) -> dict | None:
    """State mix over the annulus of half a bin either side of ``dist_m``."""
    bin_m = comp["bin_m"] if bin_m is None else bin_m
    edges = comp["edges"]
    lo, hi = dist_m - bin_m / 2.0, dist_m + bin_m / 2.0
    sel = (edges[:-1] < hi - 1e-9) & (edges[1:] > lo + 1e-9)
    if not sel.any():
        return None
    tot = float(np.asarray(comp["n_pairs"])[sel].sum())
    if tot <= 0:
        return None
    out = {}
    for s in _STATES:
        # weight the per-band fractions by band population so a canyon-expectation split (which
        # lives in `fraction`, not in `count`) is honoured
        out[s] = float(np.average(comp["fraction"][s][sel],
                                  weights=np.asarray(comp["n_pairs"], dtype=float)[sel]))
    out["n_pairs"] = int(tot)
    return out


# =================================================================================================
# the whole comparison
# =================================================================================================
def awareness_report(dataset_dir: str, *, refdata_dir: str | None = None,
                     bin_m: float = DIST_BIN_M, max_dist_m: float = MAX_DIST_M,
                     max_pairs: int = MAX_PAIRS_CLASSIFIED) -> dict:
    """Measure this dataset's awareness under the reference's own conditions.

    Everything reported here is either measured on the dataset's geometry or derived from the
    pinned reference conditions; nothing is normalised against an unknown constant.
    """
    cond = load_conditions(refdata_dir)
    sc = load_scenario(dataset_dir)
    comp = link_state_composition(sc, bin_m=bin_m, max_dist_m=max_dist_m, max_pairs=max_pairs)
    curve = mixed_pdr_curve(comp, tx_power_dbm=sc["tx_power_dbm"],
                            rx_sensitivity_dbm=sc["rx_sensitivity_dbm"],
                            radio_env=sc["radio_env"])

    zref = _entry(cond, "nar_shot_multiplicity_z").get("value") or {}
    z_urban = float(zref.get("z_urban", 5.4579))
    z_lo, z_hi = [float(v) for v in (zref.get("z_range") or [2.1365, 8.2886])]
    z_engine = z_for_engine(sc["dt_s"], 1.0)
    env = str(sc["radio_env"] or "urban").lower()
    # A HEADLINE Z exists only where the paper fitted one for that environment. It fitted exactly
    # one urban dataset (5.4579). It fitted TWO highway datasets, 2.1365 and 8.2886 -- a 4x spread
    # -- and averaging them would be inventing a number the source does not contain, so a non-urban
    # run reports the bracket and no headline.
    z_headline = z_urban if env == "urban" else None

    budget = curve["link_budget_db"]
    ref_d90, ref_note = reference_nar90_distance_m(budget, env, cond)

    # The per-packet level the reference's NAR = 0.90 actually asks for. At dt = 1.0 s the engine
    # gets ONE shot per 1 s window, so its own awareness ratio is already per-packet. A faster
    # engine is capped by the source's own fitted Z -- eq. (4) says Z <= N, and N shots at 10 Hz are
    # worth 2-8 independent chances, not 10, because CAM losses arrive in bursts. Where the source
    # fitted no Z for this environment the cap is the LARGEST fitted value, which makes the
    # resulting NAR an upper bound rather than a guess.
    z_used = min(z_engine, z_headline if z_headline else z_hi) if z_engine > 1 else 1.0
    p_star = pdr_for_nar(NAR_LEVEL, z_headline) if z_headline else None
    p_star_lo = pdr_for_nar(NAR_LEVEL, z_hi)          # most generous Z in the paper
    p_star_hi = pdr_for_nar(NAR_LEVEL, z_lo)          # least generous Z in the paper

    d_star = crossing_m(curve, p_star) if p_star is not None else None
    d_star_lo = crossing_m(curve, p_star_hi)          # tighter level -> shorter distance
    d_star_hi = crossing_m(curve, p_star_lo)
    d90 = crossing_m(curve, 0.90)
    d50 = crossing_m(curve, 0.50)
    d20 = crossing_m(curve, 0.20)
    # The near-band PDR the harness's proportional-to-PDR curve is normalised BY. Publishing it is
    # what lets comm.awareness_ratio_* (a normalised ratio) be reconciled against an absolute one.
    near = None
    for i in range(curve["pdr"].size):
        if np.isfinite(curve["pdr"][i]) and comp["n_pairs"][i] > 0:
            near = float(curve["pdr"][i])
            break

    anchors = {}
    for a in AWARENESS_ANCHORS_M:
        p = curve_value_at(curve, a, bin_m)
        mix = composition_at(comp, a, bin_m)
        anchors[int(a)] = {
            "pdr_per_packet": _r(p),
            "normalised_like_realism_bench": _r(p / near if (p is not None and near) else None),
            "nar_at_engine_rate": _r(nar_from_pdr(p, z_used) if p is not None else None),
            "nar_at_reference_10hz_rate": (_r(nar_from_pdr(p, z_headline))
                                           if (p is not None and z_headline) else None),
            "link_state_mix": ({k: _r(v) for k, v in mix.items() if k in _STATES} if mix else None),
            "n_pairs": (mix or {}).get("n_pairs"),
        }

    return {
        "dataset_dir": os.fspath(dataset_dir),
        "config": {
            "radio_model": sc["radio_model"], "radio_env": sc["radio_env"],
            "tx_power_dbm": sc["tx_power_dbm"], "rx_sensitivity_dbm": sc["rx_sensitivity_dbm"],
            "decode_floor_dbm": curve["decode_floor_dbm"], "link_budget_db": _r(budget, 2),
            "dt_s": sc["dt_s"], "emit_sample_prob": sc["emit_sample_prob"],
            "nlosb_density_per_km": sc["nlosb_density_per_km"],
        },
        "geometry": {
            "n_buildings": comp["n_buildings"], "buildings_source": sc["buildings_source"],
            "nlosb_method": comp["nlosb_method"], "snapshots": comp["snapshots"],
            "pairs_classified": comp["n_classified"], "pair_budget_reached": comp["truncated"],
        },
        "link_state_mix_overall": {s: _r(comp["overall"][s]) for s in _STATES},
        "pdr_model": {
            "model": "TR 37.885 per-state path loss + Gudmundson shadowing marginal + censored "
                     "NLOSv blockage + Nakagami-m fade, against the engine's hard decode floor",
            "applies_to_this_run": sc["radio_model"] == "geometric",
            "note": ("the run's own radio_model is %r. The composition above is a property of the "
                     "SCENE and is valid either way; the PDR curve is what the configured geometric "
                     "physics WOULD deliver on it, which for a disc/logdistance run is a "
                     "counterfactual and must be labelled as one." % sc["radio_model"]),
            "near_band_pdr": _r(near),
            "excludes": ["congestion/CBR", "hidden-terminal collision", "weather loss"],
            "excludes_why": "the reference simulation models no interference and is explicitly an "
                            "upper bound; adding our congestion term would recreate the mismatch",
        },
        "link_state_mix_by_band": [
            {"d_lo_m": _r(comp["edges"][i], 1), "d_hi_m": _r(comp["edges"][i + 1], 1),
             "n_pairs": int(comp["n_pairs"][i]),
             **{s.lower(): _r(comp["fraction"][s][i]) for s in _STATES},
             "pdr_per_packet": _r(curve["pdr"][i])}
            for i in range(comp["edges"].size - 1) if comp["n_pairs"][i] > 0],
        "shot_multiplicity": {
            "engine_cams_per_1s_window": _r(z_engine, 1),
            "z_engine_effective": _r(z_used, 4),
            "z_headline": z_headline,
            "z_reference_urban": z_urban,
            "z_reference_range": [z_lo, z_hi],
            "per_packet_pdr_equivalent_of_nar_0p90": {
                "at_z_headline": _r(p_star), "at_z_min": _r(p_star_hi), "at_z_max": _r(p_star_lo)},
            "note": ("the engine emits one CAM every dt_s, so a 1 s awareness window gives it "
                     "floor(1/dt_s) shots; at dt_s = 1.0 that is ONE and its awareness ratio IS a "
                     "per-packet PDR"),
        },
        "crossings_m": {
            "nar90_equivalent": _r(d_star, 1),
            "nar90_equivalent_z_range": [_r(d_star_lo, 1), _r(d_star_hi, 1)],
            "pdr_0p90": _r(d90, 1), "pdr_0p50": _r(d50, 1), "pdr_0p20": _r(d20, 1),
            "gray_zone_width_m": _r((d20 - d90) if (d20 and d90) else None, 1),
            "gray_zone_ratio_d20_over_d90": _r(gray_zone_ratio(d90, d20)),
        },
        "reference": {
            "regime": env,
            "nar90_distance_m_at_our_budget": _r(ref_d90, 1),
            "interpolation": ref_note,
            "cite": "Boban & d'Orey, IEEE TVT 65(6):3904-3916, 2016 (arXiv:1503.06590v3), "
                    "Section V-E.1 Figs. 18-19; conditions pinned in "
                    "refdata/v2x_awareness_conditions.json",
            "measured_arm_reproducible": False,
            "measured_arm_reason": (
                "Table III's urban 200 m is one test site (Tampere) with a THREE-vehicle "
                "instrumented fleet on a shared 22 km route; the denominator counts only "
                "instrumented vehicles, per-link LOS state and per-vehicle effective transmit "
                "power are unpublished, and the same table's highway column spans 100-400 m across "
                "four sites. It cannot be reproduced from a simulation's all-pairs population."),
        },
        "verdict": _verdict(d_star, ref_d90, anchors, ref_note),
        "anchors": anchors,
    }


def _verdict(d_star, ref_d90, anchors, ref_note: str = "") -> dict:
    """Plain statement of whether the channel is over-attenuating, and by how much.

    The dB figure converts a range ratio through the TR 37.885 urban-NLOS slope of 30 dB/decade,
    which is the slope that governs the dominant link state in a built-up scene. It is an
    EQUIVALENT, not a measured power error: a scene whose LOS fraction differs from the reference
    scene's will show a range ratio with no power error at all, which is exactly why the LOS
    composition is reported next to it.
    """
    if d_star is None or ref_d90 is None:
        return {"status": "na", "text": "no comparable crossing: " + (ref_note or "insufficient data")}
    a200 = (anchors.get(200) or {}).get("link_state_mix") or {}
    los_note = (f" LOS composition at 200 m: {a200.get('LOS', float('nan')):.3f} LOS / "
                f"{a200.get('NLOSv', float('nan')):.3f} NLOSv / "
                f"{a200.get('NLOSb', float('nan')):.3f} NLOSb." if a200 else "")
    ratio = d_star / ref_d90
    db_30 = 30.0 * math.log10(max(ratio, 1e-9))
    if 0.75 <= ratio <= 1.35:
        text = (f"CONSISTENT: the model's 90%-awareness-equivalent range is {d_star:.0f} m against "
                f"{ref_d90:.0f} m predicted by the reference's own curve at this link budget "
                f"({ratio:.2f}x). The channel is not over-attenuating; the retired >= 0.90 gate "
                f"was comparing a per-packet ratio against a >=1-of-Z metric measured at a richer "
                f"link budget.{los_note}")
        st = "consistent"
    elif ratio < 0.75:
        text = (f"SHORT of the reference by {abs(db_30):.1f} dB equivalent: {d_star:.0f} m against "
                f"{ref_d90:.0f} m ({ratio:.2f}x). Read this against the scene, not only the "
                f"channel: TR 37.885's urban-NLOS term is a statistical fit applied to every "
                f"blocked link, whereas the reference's GEMV^2 NLOSb finds actual reflected and "
                f"diffracted paths around corners, so a denser scene and a more pessimistic NLOS "
                f"term are both live explanations.{los_note}")
        st = "short"
    else:
        text = (f"LONGER than the reference by {db_30:.1f} dB equivalent: {d_star:.0f} m against "
                f"{ref_d90:.0f} m ({ratio:.2f}x).{los_note}")
        st = "long"
    return {"status": st, "ratio": _r(ratio), "equivalent_db": _r(db_30, 2), "text": text}


def _r(x, nd: int = 4):
    if x is None:
        return None
    v = float(x)
    if not math.isfinite(v):
        return None
    return round(v, nd)


# =================================================================================================
# scorecard rows (consumed by realism_bench.comm_panel)
# =================================================================================================
def panel_rows(report: dict, metric_fn, ref_lookup) -> list[dict]:
    """Build ``comm.*`` scorecard rows from :func:`awareness_report`'s output.

    ``metric_fn`` is ``realism_bench._metric`` and ``ref_lookup`` is a callable ``ref_id -> entry``;
    they are injected rather than imported so this module stays free of a circular dependency and
    can be unit-tested without the harness.
    """
    P = "comm"
    out: list[dict] = []
    geo, cfg = report["geometry"], report["config"]
    mix = report["link_state_mix_overall"]
    shots = report["shot_multiplicity"]
    cross = report["crossings_m"]
    ref = report["reference"]

    n_cls = geo["pairs_classified"]
    thin = None if n_cls >= MIN_BAND_PAIRS else f"only {n_cls} classifiable co-present pairs"
    # A LOS share is only comparable across scenes when it was measured the same way in both.
    # Saying so here is what stops a 0.76 from a footprint-free import being read against a 0.03
    # from real Ingolstadt geometry as if the model had changed.
    _NO_GEOM = {
        "no_blockage_model":
            " NOTE: this dataset carries NO building polygons and no canyon density, so NLOSb is "
            "structurally zero and the LOS share is an UPPER BOUND counting vehicle blockage only. "
            "It is not comparable with a scene that has footprints.",
        "canyon_density_expectation":
            " NOTE: this dataset carries no building polygons, so NLOSb is the analytic "
            "urban-canyon expectation 1 - exp(-lambda*d) at each band centre, not a per-pair "
            "geometric test. It is a scenario-level average, not a measurement of these pairs.",
    }
    no_geom = _NO_GEOM.get(geo["nlosb_method"], "")

    out.append(metric_fn(
        "comm.link_state_los_fraction", P,
        "LOS fraction of the co-present pair population (all bands)",
        mix.get("LOS"), "fraction", n_cls, None, "soft",
        reason=thin or ("informational: the explanatory variable behind every awareness number. "
                        "Without it an awareness ratio cannot be interpreted at all." + no_geom),
        extra={"nlosv_fraction": mix.get("NLOSv"), "nlosb_fraction": mix.get("NLOSb"),
               "nlosb_method": geo["nlosb_method"], "n_buildings": geo["n_buildings"],
               "classification": "the channel's own _BuildingRaster ray march and "
                                 "_VehicleBlockerIndex body test, imported not re-implemented",
               "by_band": report["link_state_mix_by_band"]}))

    for a in AWARENESS_ANCHORS_M:
        row = report["anchors"].get(int(a)) or {}
        out.append(metric_fn(
            f"comm.link_state_los_fraction_{int(a)}m", P,
            f"LOS fraction of pairs at {int(a)} m",
            (row.get("link_state_mix") or {}).get("LOS"), "fraction", row.get("n_pairs"),
            None, "soft",
            reason="informational: LOS/NLOSv/NLOSb composition of the annulus the awareness "
                   "ratio at this distance averages over",
            extra={"mix": row.get("link_state_mix"),
                   "pdr_per_packet": row.get("pdr_per_packet"),
                   "normalised_like_realism_bench": row.get("normalised_like_realism_bench"),
                   "nar_at_reference_10hz_rate": row.get("nar_at_reference_10hz_rate")}))

    out.append(metric_fn(
        "comm.pdr_absolute_200m", P,
        "Absolute per-packet PDR at 200 m (propagation only)",
        (report["anchors"].get(200) or {}).get("pdr_per_packet"), "fraction",
        (report["anchors"].get(200) or {}).get("n_pairs"),
        None, "soft",
        reason=thin or ("informational: ABSOLUTE, not normalised at the near band, so unlike "
                        "comm.awareness_ratio_200m it can be compared to a threshold at all. "
                        "Excludes congestion/collision/weather because the reference simulation "
                        "models no interference and is explicitly an upper bound."),
        extra={"per_packet_equivalent_of_reference_nar_0p90":
               shots["per_packet_pdr_equivalent_of_nar_0p90"],
               "engine_shots_per_1s_window": shots["engine_cams_per_1s_window"],
               "z_source": "v2x_awareness_conditions.nar_shot_multiplicity_z "
                           "(Boban & d'Orey 2016 eq. 4, Figs. 11/12/14)",
               "near_band_pdr": report["pdr_model"]["near_band_pdr"],
               "normalised_like_realism_bench":
                   (report["anchors"].get(200) or {}).get("normalised_like_realism_bench"),
               "applies_to_this_run": report["pdr_model"]["applies_to_this_run"]}))

    # GRADE ONLY WHAT THE RUN ACTUALLY USED. On a `disc`/`logdistance`/MOSAIC-SNS run the modelled
    # PDR curve is what the geometric physics WOULD deliver on this scene, which is a useful
    # counterfactual and a useless gate -- grading it would be the same category error this module
    # exists to remove, one level down.
    counterfactual = (None if report["pdr_model"]["applies_to_this_run"] else
                      f"COUNTERFACTUAL, so reported but not graded: this run used "
                      f"radio_model={cfg['radio_model']!r}. The link-state composition above is a "
                      f"property of the scene and stands; the modelled PDR curve is what the "
                      f"configured geometric physics would deliver on it, not what this run did.")
    if cross["nar90_equivalent"] is None:
        crossing_reason = ("the modelled curve never falls to the reference's per-packet "
                           f"equivalent ({shots['per_packet_pdr_equivalent_of_nar_0p90']['at_z_headline']}) "
                           "inside the measured range"
                           if shots.get("z_headline") else
                           "no environment-matched Z is fitted in the source for this regime "
                           "(the paper fits highway Z at both 2.1365 and 8.2886), so no single "
                           "crossing is quoted -- see z_sensitivity_range_m")
    elif ref["nar90_distance_m_at_our_budget"] is None:
        crossing_reason = ref["interpolation"]
    else:
        crossing_reason = None
    out.append(metric_fn(
        "comm.nar90_equivalent_range_m", P,
        "Range at which awareness falls below the reference's 0.90 (Table III's own quantity)",
        cross["nar90_equivalent"], "m", n_cls,
        _synth_ref(ref["nar90_distance_m_at_our_budget"], ref["cite"], ref["interpolation"],
                   cross["nar90_equivalent"], cross["nar90_equivalent_z_range"],
                   shots["z_reference_range"]),
        "soft",
        reason=thin or counterfactual or crossing_reason,
        extra={"z_sensitivity_range_m": cross["nar90_equivalent_z_range"],
               "link_budget_db": cfg["link_budget_db"],
               "nlosb_method": geo["nlosb_method"], "n_buildings": geo["n_buildings"],
               "reference_budget_note": "the reference's own -95 dBm receiver is folded into its "
                                        "budget, so this compares like with like",
               "verdict": report["verdict"]["text"]}))

    out.append(metric_fn(
        "comm.pdr_gray_zone_ratio", P,
        "PDR gray-zone RATIO d20/d90 (dimensionless)",
        cross["gray_zone_ratio_d20_over_d90"], "ratio", n_cls,
        _gray_ratio_ref(ref_lookup("v2x_awareness.gray_zone_ratio_from_shadowing")), "soft",
        reason=thin or counterfactual,
        extra={"d_at_0p90_m": cross["pdr_0p90"], "d_at_0p20_m": cross["pdr_0p20"],
               "width_m": cross["gray_zone_width_m"],
               "why_a_ratio": "unlike the absolute width in metres, this cannot be passed by "
                              "lowering transmit power until the curve is broad and low -- the "
                              "canyon-0 run scored a 509 m width while failing awareness",
               "shadowing_only_lower_bounds": {"urban_los": 2.4066, "highway_los": 2.0820,
                                               "urban_nlos": 1.9191}}))
    return out


def _gray_ratio_ref(pinned: dict | None) -> dict | None:
    """Turn the pinned ``[state, sigma, b, ratio, min_d90]`` rows into an actual threshold.

    ``v2x_awareness.gray_zone_ratio_from_shadowing`` carries only ``points``, so the harness read it
    as "reference entry carries no numeric threshold" and reported ``na``. But its own derivation
    note says the three ratios are LOWER BOUNDS -- "small-scale fading widens the zone further" and
    state mixing widens it further still -- so the smallest of them is a genuine floor for any link
    population, and a model below it is applying too small a shadowing sigma or re-drawing shadowing
    per packet (which averages the zone away). No upper bound is claimed by the source and none is
    invented here.
    """
    if not pinned or not pinned.get("points"):
        return None
    floor = min(float(p[3]) for p in pinned["points"])
    out = dict(pinned)
    out["min"] = floor
    out["note"] = (f"floor = min of the three pinned shadowing-only ratios ({floor:.4f}, urban "
                   f"NLOS). The source states these are LOWER bounds because fading and LOS/NLOS "
                   f"mixing both widen the zone; no upper bound is claimed, so none is gated. "
                   + str(pinned.get("note") or ""))
    return out


def _synth_ref(value, cite, note, headline, z_bracket, z_range) -> dict | None:
    """A reference entry built from the interpolated curve, with a band NOBODY CHOSE.

    The obvious thing -- a round +/- percentage -- would be a number invented here, and inventing the
    tolerance is how a gate stops being a measurement. Instead the band comes entirely from the
    source's own admitted uncertainty: it fits the shot multiplicity Z anywhere in [2.1365, 8.2886],
    and NAR = 0.90 therefore corresponds to a per-packet PDR anywhere in [0.243, 0.660]. Reading OUR
    curve at both ends of that gives ``z_bracket`` = [d_lo, d_hi], the range of crossing distances
    consistent with the reference's own definition.

    The test that should be applied is simply "does the reference's predicted distance fall inside
    that bracket". Written as a band on the measured value -- which is the shape the scorecard
    grades -- that is exactly

        value * (ref / d_hi)  <=  value  <=  value * (ref / d_lo)

    so ``min`` and ``max`` below are ``ref * headline / d_hi`` and ``ref * headline / d_lo``. Every
    term is measured or transcribed; none is a tolerance somebody picked.
    """
    if value is None or not headline or not z_bracket:
        return None
    lo, hi = z_bracket
    if not lo or not hi or lo <= 0 or hi <= 0:
        return None
    v, h = float(value), float(headline)
    return {"ref_id": "v2x_awareness_conditions.simulated_urban_nar90_by_link_budget",
            "unit": "m", "confidence": "anchored", "cite": cite, "source": note,
            "min": round(v * h / float(hi), 1), "max": round(v * h / float(lo), 1),
            "derivation": (f"the reference predicts {v:.1f} m at this link budget; the band is the "
                           f"source's own fitted Z range {z_range} read through THIS curve "
                           f"({lo:.1f}-{hi:.1f} m), so the test is equivalent to 'the reference "
                           f"distance lies inside the model's Z-sensitivity bracket'"),
            "note": ("The source is additionally inconsistent at the top of its own curve -- it "
                     "reports 300 m at 23 dBm and 250 m at the 33 dBm ceiling -- which this band "
                     "does not attempt to absorb.")}


# =================================================================================================
# CLI
# =================================================================================================
def render_lines(rep: dict) -> list[str]:
    c, g, x = rep["config"], rep["geometry"], rep["crossings_m"]
    mix = rep["link_state_mix_overall"]
    s = rep["shot_multiplicity"]
    lines = [
        f"# awareness, like-for-like  ({rep['dataset_dir']})",
        "",
        f"radio_model={c['radio_model']} env={c['radio_env']} tx={c['tx_power_dbm']} dBm "
        f"sens={c['rx_sensitivity_dbm']} dBm floor={c['decode_floor_dbm']} dBm "
        f"budget={c['link_budget_db']} dB dt={c['dt_s']} s",
        f"geometry: {g['n_buildings']} buildings ({g['nlosb_method']}), {g['snapshots']} snapshots, "
        f"{g['pairs_classified']} pairs classified",
        "",
        "LINK-STATE COMPOSITION (the quantity that explains the awareness number)",
        f"  overall   LOS {mix['LOS']:.4f}   NLOSv {mix['NLOSv']:.4f}   NLOSb {mix['NLOSb']:.4f}",
    ]
    for a in AWARENESS_ANCHORS_M:
        r = rep["anchors"].get(int(a)) or {}
        m = r.get("link_state_mix") or {}
        if not m:
            continue
        lines.append(f"  @{int(a):>4d} m  LOS {m['LOS']:.4f}   NLOSv {m['NLOSv']:.4f}   "
                     f"NLOSb {m['NLOSb']:.4f}   (n={r.get('n_pairs')})")
    pm = rep["pdr_model"]
    lines += ["", "ABSOLUTE PDR (propagation only, no congestion -- as in the reference)"
                  + ("" if pm["applies_to_this_run"]
                     else "   [COUNTERFACTUAL: run used radio_model=" + c["radio_model"] + "]")]
    for a in AWARENESS_ANCHORS_M:
        r = rep["anchors"].get(int(a)) or {}
        if r.get("pdr_per_packet") is None:
            continue
        nar = r.get("nar_at_reference_10hz_rate")
        lines.append(f"  @{int(a):>4d} m  per-packet {r['pdr_per_packet']:.4f}   "
                     f"normalised-like-realism_bench {r['normalised_like_realism_bench']:.4f}   "
                     + (f"NAR at the reference's 10 Hz {nar:.4f}" if nar is not None else ""))
    eq = s["per_packet_pdr_equivalent_of_nar_0p90"]
    hz = eq["at_z_headline"]
    lines += [
        "",
        "SHOT MULTIPLICITY (Boban & d'Orey eq. 4)",
        f"  engine gets {s['engine_cams_per_1s_window']:.0f} CAM(s) per 1 s window -> Z = "
        f"{s['z_engine_effective']}",
        (f"  reference NAR 0.90 == per-packet PDR {hz:.4f} at Z = {s['z_headline']}"
         if hz is not None else
         "  no environment-matched Z is fitted in the source for this regime; bracket only"),
        f"  Z bracket: per-packet {eq['at_z_max']:.4f} at Z = {s['z_reference_range'][1]} .. "
        f"{eq['at_z_min']:.4f} at Z = {s['z_reference_range'][0]}",
        "",
        "CROSSINGS",
        f"  0.90-awareness-equivalent range : {x['nar90_equivalent']} m "
        f"(Z range {x['nar90_equivalent_z_range']})",
        f"  reference at OUR budget         : {rep['reference']['nar90_distance_m_at_our_budget']} m",
        f"  PDR 0.90 / 0.50 / 0.20          : {x['pdr_0p90']} / {x['pdr_0p50']} / {x['pdr_0p20']} m",
        f"  gray zone                       : {x['gray_zone_width_m']} m, "
        f"ratio d20/d90 = {x['gray_zone_ratio_d20_over_d90']}",
        "",
        "VERDICT",
        "  " + rep["verdict"]["text"],
    ]
    return lines


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        description="Measure cooperative awareness under the conditions its reference measured it.")
    p.add_argument("dataset_dir")
    p.add_argument("--refdata", default=None)
    p.add_argument("--json", dest="json_out", default=None)
    p.add_argument("--markdown", action="store_true")
    p.add_argument("--bin-m", type=float, default=DIST_BIN_M)
    p.add_argument("--max-dist-m", type=float, default=MAX_DIST_M)
    p.add_argument("--max-pairs", type=int, default=MAX_PAIRS_CLASSIFIED)
    a = p.parse_args(argv)
    if not os.path.isdir(a.dataset_dir):
        p.error(f"dataset directory not found: {a.dataset_dir}")
    rep = awareness_report(a.dataset_dir, refdata_dir=a.refdata, bin_m=a.bin_m,
                           max_dist_m=a.max_dist_m, max_pairs=a.max_pairs)
    if a.markdown:
        print("\n".join(render_lines(rep)))
    else:
        print(json.dumps(rep, indent=2, default=str))
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(rep, fh, indent=2, default=str)
            fh.write("\n")
        print(f"\n[wrote {a.json_out}]")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
