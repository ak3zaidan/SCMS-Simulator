"""Measure the MOSAIC/Java radio with the instruments the Python engine is already measured with.

WHY THIS FILE EXISTS
--------------------
The project has two engines with two different radios and they have never been compared. The Python
engine's channel is measured by ``src/scms_sim_ref/datagen/awareness.py``: link-state composition per
distance band, an ABSOLUTE per-packet PDR-vs-distance curve, the NAR-equivalent crossing distance
against Boban & d'Orey's own urban curve at OUR link budget, and the dimensionless gray-zone ratio.
Nothing had ever pointed those instruments at the MOSAIC path, so the two radios' agreement was
unknown and the ETSI DCC / EN 302 637-2 triggering that exist only on the Java side were an
unquantified realism inconsistency in the project's own output.

This module supplies the missing half, and it deliberately does NOT re-derive the reference
arithmetic: the shot-multiplicity conversion (``pdr_for_nar``), the reference curve interpolation
(``reference_nar90_distance_m``), the annulus crossing estimator (``crossing_m``) and the Python
engine's own propagation model (``propagation_pdr``) are all IMPORTED from ``datagen.awareness``. A
second copy of that arithmetic would be a second opinion, not a comparison.

WHAT IS MEASURED, AND FROM WHAT
-------------------------------
``measure``
    Reads ``link_trace.csv`` -- the per-(frame, receiver) reception decisions the Java radio actually
    took, written by ``org.scms.radio.LinkTrace`` -- plus the run manifest and the emission trace,
    and reports:

    * link-state composition per 50 m band (the Java ``BuildingIndex`` ray march's OWN verdict),
    * an EMPIRICAL absolute PDR-vs-distance curve, split propagation-only / with contention, so the
      propagation-only one is comparable with the reference (which models no interference),
    * a closed-form check of that curve against the Java model's own delivery rule, which is what
      turns "the trace looks plausible" into "the trace reproduces the model to within X",
    * the same curve as the PYTHON engine's physics would have produced ON THIS SAME SCENE, from
      ``awareness.propagation_pdr`` and the measured mix -- the scene is held fixed so the residual
      is physics and nothing else,
    * crossings (d90/d50/d20, gray-zone ratio, NAR-0.90-equivalent range) via ``awareness``,
    * modelled CBR from the run's own ETSI meter, and the CAM inter-packet-gap distribution from
      ``ground_truth/gt_emissions_sample.jsonl``.

``shim``
    Writes an ``awareness``-readable view of a MOSAIC dataset: a ``manifest.json`` whose ``config``
    carries the scenario's building rings (parsed from the SAME ``buildings.poly.xml`` the Java run
    rasterised) and the radio parameters under the key names ``awareness.load_scenario`` expects,
    beside the run's own emission trace. That lets the UNMODIFIED CLI

        python -m scms_sim_ref.datagen.awareness <shim_dir> --markdown

    run over MOSAIC data, so both engines are read by literally the same code path.

TRUST FIREWALL: read-only over dataset directories, aggregate output only, no RNG, no per-entity
value in any output.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if os.path.join(REPO, "src") not in sys.path:
    sys.path.insert(0, os.path.join(REPO, "src"))

from scms_sim_ref.datagen import awareness as aw   # noqa: E402  (path set above)

BIN_M = aw.DIST_BIN_M          # 50 m: the reference's own NAR bin
MAX_DIST_M = aw.MAX_DIST_M
MIN_BAND_N = 200               # a band needs this many traced decisions before its PDR is quoted
# The Java receiver's own vocabulary, and the tokens it writes into link_trace.csv. NLOSv joined the
# set when RxChannel gained the vehicle-blockage branch; a trace written before that carries no
# NLOSv rows at all, so an old run still bins correctly and simply reports fraction 0.
STATES = ("LOS", "NLOSv", "NLOSb")


# =================================================================================================
# the Java radio's own delivery rule, in closed form
# =================================================================================================
# org.scms.radio.PathLoss: PL = a + b*log10(d) + c*log10(fc). Same TR 37.885 constants the Python
# engine uses -- transcribed here ONLY to evaluate the Java rule; any divergence between the two
# engines' path loss would show up as a disagreement between this and awareness.propagation_pdr at
# LOS, where the two models are otherwise identical.
# NLOSv uses the LOS formula plus its own censored-Gaussian blockage term, exactly as
# PathLoss.State encodes it: sigma 3.0 dB (NOT the NLOSb 4.0, which would double-count blockage).
JAVA_PATHLOSS = {"LOS": (38.77, 16.7, 18.2), "NLOSv": (38.77, 16.7, 18.2),
                 "NLOSb": (36.85, 30.0, 18.9)}
JAVA_SHADOW_SIGMA = {"LOS": 3.0, "NLOSv": 3.0, "NLOSb": 4.0}
JAVA_FC_GHZ = 5.9
# org.scms.radio.PathLoss.NLOSV_MU_BASE_DB / NLOSV_SIGMA_DB, indexed by antennas below the blocker.
# Index 2 (both below) is the vehicle-to-vehicle case: OBU antennas sit at 1.5 m and the shortest
# blocker is a 1.6 m car, so every V2V NLOSv link takes it. An RSU link (5 m pole) takes index 1.
JAVA_NLOSV_BRANCH = {2: (9.0, 4.5), 1: (5.0, 4.0), 0: (0.0, 0.0)}
# nakagami_fading.m_by_distance_adopted, mirrored in PathLoss.nakagamiM.
JAVA_NAKAGAMI_BANDS = ((50.0, 3.0), (150.0, 1.5), (float("inf"), 1.0))


def java_pathloss_db(state: str, d_m: float, fc_ghz: float = JAVA_FC_GHZ) -> float:
    a, b, c = JAVA_PATHLOSS[state]
    return a + b * math.log10(max(float(d_m), 1.0)) + c * math.log10(fc_ghz)


def java_nakagami_m(d_m: float) -> float:
    for upper, m in JAVA_NAKAGAMI_BANDS:
        if d_m <= upper:
            return m
    return JAVA_NAKAGAMI_BANDS[-1][1]


def java_pdr(state: str, d_m: float, *, tx_power_dbm: float, rx_sensitivity_dbm: float,
             antenna_gain_dbi: float = 0.0, fading: bool = True, nlosv: bool = True,
             nlosv_branch: int = 2) -> float:
    """P(rssi >= sensitivity) under the Java model, in closed form.

    ``RxChannel.geometricDeliver`` computes, per frame::

        rssi = tx + 2*gain - PL(state, d) - sigma*z [- max(0, N(mu, sig))] [+ 10*log10(Gamma(m,1/m))]

    where ``z`` is the AR(1) shadowing state, whose MARGINAL is N(0,1) whatever the correlation, so
    integrating over it is exact and not an approximation. The two bracketed terms are the ones the
    receiver gained with the NLOSv/fading work and each is gated by its own env knob; ``fading`` and
    ``nlosv`` here mirror ``SCMS_FADING`` / ``SCMS_NLOSV`` as the run's manifest reports them, so the
    SAME function scores a before-run and an after-run and the comparison is like for like.

    The quadrature nodes come from ``awareness`` (one audited integrator, shared with
    ``propagation_pdr``); every CONSTANT is the Java one. That split is deliberate: reusing the
    reference's integrator keeps the two engines numerically comparable, while keeping the Java
    constants local means a future divergence in either engine still shows up here as a difference
    rather than being defined away.

    NOTE the decode floor. Java's is the bare sensitivity; the Python engine's is
    ``max(sensitivity, noise + SNIR) = max(sens, -106)``. They coincide at -81 dBm and diverge only
    for a sensitivity below -106 dBm, where the Java side would be the optimistic one.
    """
    margin = (float(tx_power_dbm) + 2.0 * float(antenna_gain_dbi)
              - java_pathloss_db(state, d_m) - float(rx_sensitivity_dbm))
    sigma = JAVA_SHADOW_SIGMA[state]
    if fading:
        g, wg = aw._fade_quadrature(java_nakagami_m(float(d_m)))
        fade_db = 10.0 * np.log10(np.maximum(g, 1e-300))
    else:
        fade_db, wg = np.array([0.0]), np.array([1.0])
    if state == "NLOSv" and nlosv:
        mu_base, sig_v = JAVA_NLOSV_BRANCH[int(nlosv_branch)]
        mu = mu_base + max(0.0, 15.0 * math.log10(max(float(d_m), 1.0)) - 41.0)
        nodes, wl = aw._nlosv_quadrature(mu, sig_v)
        z = (margin + fade_db[:, None] - nodes[None, :]) / sigma
        return float((aw._norm_cdf(z) * wg[:, None] * wl[None, :]).sum())
    return float((aw._norm_cdf((margin + fade_db) / sigma) * wg).sum())


# =================================================================================================
# the run's own artefacts
# =================================================================================================
def load_manifest(run_dir: str) -> dict:
    with open(os.path.join(run_dir, "manifest.json"), encoding="utf-8") as fh:
        return json.load(fh)


def radio_config(man: dict) -> dict:
    """Radio knobs as the Java JVM resolved them (manifest.effective_params)."""
    ep = man.get("effective_params") or {}
    tx = float(ep.get("SCMS_TX_POWER_DBM", 10.0 * math.log10(20.0)))
    sens = float(ep.get("SCMS_RX_SENSITIVITY_DBM", -81.0))
    gain = float(ep.get("SCMS_ANTENNA_GAIN_DBI", 0.0))
    return {
        "radio_model": str(ep.get("SCMS_RADIO_MODEL", "sns")),
        "radio_env": "urban" if str(ep.get("SCMS_RADIO_REGIME", "urban")) == "urban" else "highway",
        "tx_power_dbm": tx,
        "rx_sensitivity_dbm": sens,
        "antenna_gain_dbi": gain,
        # Java: budget = tx + 2*gain - sensitivity. There is no separate noise/SNIR floor on this
        # side; the Python engine's floor is max(sens, noise + snir) = max(sens, -106).
        "link_budget_db": round(tx + 2.0 * gain - sens, 3),
        "dcc_enabled": bool(ep.get("SCMS_DCC", False)),
        "buildings": bool(ep.get("SCMS_BUILDINGS", False)),
        # Absent from every manifest written before RxChannel gained the two terms, and the default
        # therefore has to be False: an old panel must keep scoring the radio that produced it.
        "nlosv": bool(ep.get("SCMS_NLOSV", False)),
        "fading": bool(ep.get("SCMS_FADING", False)),
        "nlosv_half_width_m": ep.get("SCMS_NLOSV_HALF_WIDTH_M"),
        "nlosv_rebuild_s": ep.get("SCMS_NLOSV_REBUILD_S"),
        "nlosv_max_age_s": ep.get("SCMS_NLOSV_MAX_AGE_S"),
        "chan_capacity": ep.get("SCMS_CHAN_CAPACITY"),
        "weather_drop": ep.get("SCMS_WEATHER_RADIO_LOSS"),
        "sns_singlehop_radius_m": (man.get("config") or {}).get("radio_range_m"),
        "trace_prob": ep.get("SCMS_LINK_TRACE_PROB"),
        "dcc_frame_airtime_s": ep.get("dcc_frame_airtime_s"),
    }


def read_trace(path: str, bin_m: float = BIN_M, max_dist_m: float = MAX_DIST_M) -> dict:
    """Bin ``link_trace.csv`` into per-(band, state) outcome counts.

    One row per reception decision the radio took: ``t_s,rx,dist_m,state,rssi_dbm,outcome`` with
    outcome in {ok, geom, cong, wx}. Rows are a uniform random sample of the decisions (probability
    ``SCMS_LINK_TRACE_PROB``), drawn from a stream the simulation never touches, so every ratio
    below is unbiased and only its variance depends on the sampling rate.
    """
    nb = int(max_dist_m // bin_m)
    zero = lambda: np.zeros(nb, dtype=np.int64)       # noqa: E731
    tot = {s: zero() for s in STATES}
    ok = {s: zero() for s in STATES}
    cong = {s: zero() for s in STATES}
    rssi_sum = {s: np.zeros(nb) for s in STATES}
    n_rows = n_far = n_na = 0
    t_lo, t_hi = math.inf, -math.inf
    with open(path, encoding="utf-8", newline="") as fh:
        header = fh.readline()
        if not header.startswith("t_s,"):
            raise ValueError(f"{path}: unexpected header {header!r}")
        for line in fh:
            n_rows += 1
            f = line.rstrip("\n").split(",")
            if len(f) < 6:
                continue
            st = f[3]
            if st not in tot:
                n_na += 1
                continue
            try:
                d = float(f[2])
                t = float(f[0])
            except ValueError:
                n_na += 1
                continue
            if t < t_lo:
                t_lo = t
            if t > t_hi:
                t_hi = t
            b = int(d // bin_m)
            if b >= nb:
                n_far += 1
                continue
            tot[st][b] += 1
            if f[5] == "ok":
                ok[st][b] += 1
            elif f[5] == "cong":
                cong[st][b] += 1
            if f[4]:
                rssi_sum[st][b] += float(f[4])
    edges = np.arange(0.0, max_dist_m + bin_m, bin_m)
    n_tot = sum(tot[s] for s in STATES)
    n_ok = sum(ok[s] for s in STATES)
    n_cong = sum(cong[s] for s in STATES)
    with np.errstate(invalid="ignore", divide="ignore"):
        frac = {s: np.where(n_tot > 0, tot[s] / np.maximum(n_tot, 1), np.nan) for s in STATES}
        pdr_prop = np.where(n_tot >= MIN_BAND_N, (n_ok + n_cong) / np.maximum(n_tot, 1), np.nan)
        pdr_full = np.where(n_tot >= MIN_BAND_N, n_ok / np.maximum(n_tot, 1), np.nan)
        per_state = {s: np.where(tot[s] >= MIN_BAND_N,
                                 (ok[s] + cong[s]) / np.maximum(tot[s], 1), np.nan)
                     for s in STATES}
        mean_rssi = {s: np.where(tot[s] > 0, rssi_sum[s] / np.maximum(tot[s], 1), np.nan)
                     for s in STATES}
    return {"edges": edges, "centres": (edges[:-1] + edges[1:]) / 2.0,
            "n_total": n_tot, "n_ok": n_ok, "n_cong": n_cong,
            "n_by_state": tot, "fraction": frac,
            "pdr_propagation": pdr_prop, "pdr_with_contention": pdr_full,
            "pdr_by_state": per_state, "mean_rssi_dbm": mean_rssi,
            "rows_read": n_rows, "rows_beyond_max_dist": n_far, "rows_unclassified": n_na,
            "t_first_s": (None if t_lo is math.inf else t_lo),
            "t_last_s": (None if t_hi == -math.inf else t_hi),
            "bin_m": float(bin_m), "max_dist_m": float(max_dist_m)}


def cam_gaps(run_dir: str, honest_only: bool = True) -> dict:
    """CAM inter-packet gaps per vehicle, from the full emission trace.

    Requires ``SCMS_EMIT_SAMPLE=1.0``; a sub-sampled trace would report the gaps between the
    emissions that happened to be recorded, which is a different quantity. Attacker vehicles are
    excluded by default because a DoS flood emits ``SCMS_FLOOD_BURST`` frames at one timestamp, i.e.
    a spike of exactly-zero gaps that has nothing to do with the ETSI generation rules.
    """
    per: dict[str, list[float]] = {}
    n_att = 0
    path = os.path.join(run_dir, "ground_truth", "gt_emissions_sample.jsonl")
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            e = json.loads(line)
            if honest_only and e.get("is_attacker"):
                n_att += 1
                continue
            per.setdefault(str(e.get("true_vehicle_id")), []).append(float(e["t"]))
    gaps = []
    for ts in per.values():
        ts.sort()
        gaps.extend(np.diff(np.asarray(ts, dtype=float)).tolist())
    g = np.asarray([x for x in gaps if x > 0.0], dtype=float)
    zero_gaps = len(gaps) - g.size
    if g.size == 0:
        return {"n_gaps": 0}
    q = np.percentile(g, [5, 25, 50, 75, 95, 99])
    # ETSI EN 302 637-2 buckets: at the floor (T_GenCamMin), at the heartbeat (T_GenCamMax), between.
    return {
        "n_vehicles": len(per), "n_gaps": int(g.size), "n_zero_gaps": int(zero_gaps),
        "attacker_emissions_excluded": n_att,
        "mean_s": float(g.mean()), "std_s": float(g.std()),
        "min_s": float(g.min()), "max_s": float(g.max()),
        "p05_s": float(q[0]), "p25_s": float(q[1]), "median_s": float(q[2]),
        "p75_s": float(q[3]), "p95_s": float(q[4]), "p99_s": float(q[5]),
        "mean_rate_hz": float(1.0 / g.mean()),
        "frac_at_t_gencam_min_0p1s": float(np.mean(g <= 0.1005)),
        "frac_at_t_gencam_max_1p0s": float(np.mean(g >= 0.9995)),
        "frac_between": float(np.mean((g > 0.1005) & (g < 0.9995))),
        "histogram_s": {f"{lo:g}-{hi:g}": int(((g >= lo) & (g < hi)).sum())
                        for lo, hi in ((0.0, 0.15), (0.15, 0.25), (0.25, 0.35), (0.35, 0.55),
                                       (0.55, 0.95), (0.95, 1.05), (1.05, 1e9))},
    }


# =================================================================================================
# curves and crossings, read with the reference's own estimator
# =================================================================================================
def _curve(centres, pdr, weights) -> dict:
    return {"centres": np.asarray(centres, dtype=float), "pdr": np.asarray(pdr, dtype=float),
            "edges": np.concatenate(([0.0], np.asarray(centres, dtype=float) + BIN_M / 2.0)),
            "n_pairs": np.asarray(weights)}


def crossings(centres, pdr, weights, budget_db: float, radio_env: str, cond: dict) -> dict:
    """d90/d50/d20, gray-zone ratio and the NAR-0.90-equivalent range, all via ``awareness``."""
    c = _curve(centres, pdr, weights)
    zref = (aw._entry(cond, "nar_shot_multiplicity_z").get("value") or {})
    z_urban = float(zref.get("z_urban", 5.4579))
    z_lo, z_hi = [float(v) for v in (zref.get("z_range") or [2.1365, 8.2886])]
    p_star = aw.pdr_for_nar(aw.NAR_LEVEL, z_urban)
    d90, d50, d20 = (aw.crossing_m(c, lv) for lv in (0.90, 0.50, 0.20))
    ref_d, ref_note = aw.reference_nar90_distance_m(budget_db, radio_env, cond)
    return {
        "pdr_0p90_m": d90, "pdr_0p50_m": d50, "pdr_0p20_m": d20,
        "gray_zone_width_m": (None if (d20 is None or d90 is None) else d20 - d90),
        "gray_zone_ratio_d20_over_d90": aw.gray_zone_ratio(d90, d20),
        "nar90_equivalent_range_m": aw.crossing_m(c, p_star),
        "nar90_equivalent_z_range_m": [aw.crossing_m(c, aw.pdr_for_nar(aw.NAR_LEVEL, z_lo)),
                                       aw.crossing_m(c, aw.pdr_for_nar(aw.NAR_LEVEL, z_hi))],
        "per_packet_pdr_of_nar_0p90": p_star,
        "z_reference_urban": z_urban, "z_reference_range": [z_lo, z_hi],
        "reference_nar90_distance_m": ref_d, "reference_note": ref_note,
        "ratio_model_over_reference": (None if not (ref_d and aw.crossing_m(c, p_star))
                                       else aw.crossing_m(c, p_star) / ref_d),
    }


def counterfactual_curves(centres, frac, cfg: dict) -> dict:
    """The two engines' physics evaluated on the SAME measured link-state mix.

    Holding the scene fixed is the whole point: any difference between these two curves is the
    RADIO, because the composition weighting them is one measurement used twice.
    """
    floor_py = aw.decode_floor_dbm(cfg["rx_sensitivity_dbm"])
    jkw = dict(tx_power_dbm=cfg["tx_power_dbm"], rx_sensitivity_dbm=cfg["rx_sensitivity_dbm"],
               antenna_gain_dbi=cfg["antenna_gain_dbi"], fading=cfg["fading"], nlosv=cfg["nlosv"])
    java = np.array([sum((frac[s][i] if np.isfinite(frac[s][i]) else 0.0)
                         * java_pdr(s, float(c), **jkw)
                         for s in STATES)
                    for i, c in enumerate(centres)])
    py = np.array([sum((frac[s][i] if np.isfinite(frac[s][i]) else 0.0)
                       * aw.propagation_pdr(s, float(c), tx_power_dbm=cfg["tx_power_dbm"],
                                            decode_floor_dbm=floor_py,
                                            radio_env=cfg["radio_env"])
                       for s in STATES)
                  for i, c in enumerate(centres)])
    per_state = {}
    for s in STATES:
        # A run with SCMS_NLOSV=0 emits no NLOSv rows at all, so quoting a Java NLOSv curve for it
        # would be quoting a branch that did not run. Report None, exactly as this tool did for
        # every state before the branch existed.
        java_state = ([None] * len(centres) if (s == "NLOSv" and not cfg["nlosv"])
                      else [java_pdr(s, float(c), **jkw) for c in centres])
        per_state[s] = {
            "java": java_state,
            "python": [aw.propagation_pdr(s, float(c), tx_power_dbm=cfg["tx_power_dbm"],
                                          decode_floor_dbm=floor_py, radio_env=cfg["radio_env"])
                       for c in centres],
        }
    return {"java_closed_form": java, "python_physics_same_scene": py, "per_state": per_state,
            "python_decode_floor_dbm": floor_py}


# =================================================================================================
# the whole panel
# =================================================================================================
def measure(run_dir: str, bin_m: float = BIN_M, max_dist_m: float = MAX_DIST_M) -> dict:
    man = load_manifest(run_dir)
    cfg = radio_config(man)
    cond = aw.load_conditions()
    counts = man.get("counts") or {}
    ch = counts.get("channel") or {}
    dcc = counts.get("dcc") or {}
    gaps = cam_gaps(run_dir)

    trace_path = os.path.join(run_dir, "link_trace.csv")
    tr = read_trace(trace_path, bin_m, max_dist_m) if os.path.exists(trace_path) else None

    out = {
        "run_dir": os.path.abspath(run_dir).replace("\\", "/"),
        "engine": "mosaic-java",
        "config": cfg,
        "aggregate_channel": {
            "frames_sensed": ch.get("frames_sensed"),
            "frames_delivered": ch.get("frames_delivered"),
            "delivery_ratio_over_sns_disc": ch.get("delivery_ratio"),
            "dropped_obstruction": ch.get("dropped_obstruction"),
            "dropped_congestion": ch.get("dropped_congestion"),
            "dropped_weather": ch.get("dropped_weather"),
            "nlosb_links": ch.get("nlosb_links"),
            "nlosv_links": ch.get("nlosv_links"),
            "nlosv_fraction_all_links": ch.get("nlosv_fraction"),
            "blockers_declared": ch.get("blockers_declared"),
            "blockers_truck_height": ch.get("blockers_truck_height"),
            "blocker_snapshots": ch.get("blocker_snapshots"),
            "blocker_peak_live": ch.get("blocker_peak_live"),
            "faded_frames": ch.get("faded_frames"),
            "nlosb_fraction_all_links": ((ch.get("buildings") or {}).get("nlosb_fraction")),
            "buildings_indexed": ((ch.get("buildings") or {}).get("buildings")),
            "buildings_aligned": ((ch.get("buildings") or {}).get("aligned")),
            "note": "the denominator is every frame MOSAIC's SNS handed the app, i.e. every "
                    "transmitter within the singlehop radius; it is not a co-presence population",
        },
        "cbr": {
            "cbr_mean": dcc.get("cbr_mean"), "cbr_max": dcc.get("cbr_max"),
            "cbr_samples": dcc.get("cbr_samples"),
            "cams_allowed": dcc.get("cams_allowed"), "cams_suppressed": dcc.get("cams_suppressed"),
            "dcc_applied": cfg["dcc_enabled"],
            "frame_airtime_s": cfg["dcc_frame_airtime_s"],
            "definition": "ETSI TS 102 687 channel busy ratio, modelled as frames sensed x PPDU "
                          "airtime / window (org.scms.radio.Dcc), 100 ms probes over a 1 s window",
            "upper_bound_note": "every frame inside the SNS singlehop radius is counted as sensed "
                                "regardless of its RSSI, so this OVERSTATES the busy ratio a real "
                                "-85 dBm carrier-sense would report on building-blocked links",
        },
        "cam_generation": gaps,
        "counts": {"vehicles": counts.get("vehicles"), "rsus": counts.get("rsus"),
                   "emissions": (dcc.get("cams_allowed"))},
    }

    if tr is None:
        out["link_trace"] = {"present": False,
                             "reason": "no link_trace.csv (run without SCMS_LINK_TRACE)"}
        return out

    centres = tr["centres"]
    cf = counterfactual_curves(centres, tr["fraction"], cfg)
    budget = cfg["link_budget_db"]
    out["link_trace"] = {
        "present": True, "rows": tr["rows_read"], "sample_probability": cfg["trace_prob"],
        "rows_beyond_max_dist": tr["rows_beyond_max_dist"],
        "rows_unclassified": tr["rows_unclassified"],
        "t_span_s": [tr["t_first_s"], tr["t_last_s"]],
        "decisions_binned": int(tr["n_total"].sum()),
    }
    # Empirical-vs-closed-form residual: the trace must reproduce the model it came from, or the
    # measurement is measuring the instrument.
    m = np.isfinite(tr["pdr_propagation"]) & np.isfinite(cf["java_closed_form"])
    resid = np.abs(tr["pdr_propagation"][m] - cf["java_closed_form"][m])
    w = tr["n_total"][m].astype(float)
    out["trace_vs_closed_form"] = {
        "bands_compared": int(m.sum()),
        "max_abs_diff": float(resid.max()) if resid.size else None,
        "weighted_mean_abs_diff": float(np.average(resid, weights=w)) if resid.size else None,
        "note": "empirical per-band PDR from the trace against the Java model's own delivery rule "
                "evaluated at the band centre with the band's measured LOS/NLOSb mix. A residual is "
                "expected: distance is not uniform inside a 50 m band and the mix is measured, not "
                "assumed.",
    }
    out["by_band"] = [
        {
            "d_lo_m": float(tr["edges"][i]), "d_hi_m": float(tr["edges"][i + 1]),
            "n_decisions": int(tr["n_total"][i]),
            "los": _r(tr["fraction"]["LOS"][i]), "nlosv": _r(tr["fraction"]["NLOSv"][i]),
            "nlosb": _r(tr["fraction"]["NLOSb"][i]),
            "pdr_propagation": _r(tr["pdr_propagation"][i]),
            "pdr_with_contention": _r(tr["pdr_with_contention"][i]),
            "pdr_los": _r(tr["pdr_by_state"]["LOS"][i]),
            "pdr_nlosv": _r(tr["pdr_by_state"]["NLOSv"][i]),
            "pdr_nlosb": _r(tr["pdr_by_state"]["NLOSb"][i]),
            "mean_rssi_dbm": _r(tr["mean_rssi_dbm"]["LOS"][i], 2),
            "java_closed_form_pdr": _r(cf["java_closed_form"][i]),
            "python_physics_same_scene_pdr": _r(cf["python_physics_same_scene"][i]),
        }
        for i in range(len(centres)) if tr["n_total"][i] > 0
    ]
    out["overall_link_state"] = {
        s: _r(float(np.average(np.nan_to_num(tr["fraction"][s]),
                               weights=tr["n_total"].astype(float))))
        for s in STATES}
    out["crossings_measured"] = crossings(centres, tr["pdr_propagation"], tr["n_total"],
                                          budget, cfg["radio_env"], cond)
    out["crossings_java_closed_form"] = crossings(centres, cf["java_closed_form"], tr["n_total"],
                                                  budget, cfg["radio_env"], cond)
    out["crossings_python_physics_same_scene"] = crossings(
        centres, cf["python_physics_same_scene"], tr["n_total"], budget, cfg["radio_env"], cond)
    out["per_state_pdr_model"] = {
        s: {"d_m": [float(c) for c in centres],
            "java": [_r(v) for v in cf["per_state"][s]["java"]],
            "python": [_r(v) for v in cf["per_state"][s]["python"]]}
        for s in cf["per_state"]}
    # Anchors, in the same places awareness.py reports them.
    out["anchors"] = {}
    for a in aw.AWARENESS_ANCHORS_M:
        c_meas = _curve(centres, tr["pdr_propagation"], tr["n_total"])
        c_java = _curve(centres, cf["java_closed_form"], tr["n_total"])
        c_py = _curve(centres, cf["python_physics_same_scene"], tr["n_total"])
        mix = {}
        for s in STATES:
            cs = _curve(centres, np.nan_to_num(tr["fraction"][s], nan=np.nan), tr["n_total"])
            mix[s] = aw.curve_value_at(cs, a, bin_m)
        out["anchors"][int(a)] = {
            "pdr_measured": _r(aw.curve_value_at(c_meas, a, bin_m)),
            "pdr_java_closed_form": _r(aw.curve_value_at(c_java, a, bin_m)),
            "pdr_python_physics_same_scene": _r(aw.curve_value_at(c_py, a, bin_m)),
            "link_state_mix": {k: _r(v) for k, v in mix.items()},
        }
    return out


def _r(x, nd: int = 4):
    if x is None:
        return None
    v = float(x)
    if not math.isfinite(v):
        return None
    return round(v, nd)


# =================================================================================================
# awareness shim
# =================================================================================================
_POLY_RE = re.compile(r"<poly\b[^>]*?/>", re.S)
_TYPE_RE = re.compile(r'type="([^"]*)"')
_SHAPE_RE = re.compile(r'shape="([^"]*)"')


def parse_buildings(poly_xml: str, off_x: float = 0.0, off_y: float = 0.0) -> list:
    """Building rings from a SUMO polygon additional-file.

    The same selection ``org.scms.radio.BuildingIndex.parse`` makes -- ``type="building"`` only,
    z ignored, the duplicated closing vertex dropped -- so the Python instrument rasterises the same
    footprint set the Java run classified against, not a different subset of the same file.
    """
    with open(poly_xml, encoding="utf-8") as fh:
        xml = fh.read()
    rings = []
    for el in _POLY_RE.findall(xml):
        t = _TYPE_RE.search(el)
        if not t or t.group(1) != "building":
            continue
        sh = _SHAPE_RE.search(el)
        if not sh:
            continue
        pts = []
        for part in sh.group(1).split():
            bits = part.split(",")
            if len(bits) < 2:
                continue
            try:
                pts.append((float(bits[0]) + off_x, float(bits[1]) + off_y))
            except ValueError:
                pts = []
                break
        if len(pts) >= 2 and pts[0] == pts[-1]:
            pts = pts[:-1]
        if len(pts) >= 3:
            rings.append(pts)
    return rings


def write_shim(run_dir: str, poly_xml: str, out_dir: str, dt_s: float | None = None,
               bbox: tuple[float, float, float, float] | None = None) -> dict:
    """Write an ``awareness``-readable view of a MOSAIC run (see the module docstring).

    ``bbox`` = ``(min_x, min_y, max_x, max_y)`` restricts BOTH the footprints and the emissions to a
    sub-area. That is what makes the city-wide MOSAIC scene comparable with the Python engine's
    2 km OSM core extract: the two runs' link-state compositions differ by an order of magnitude,
    and the only way to tell "different radio" from "different piece of Ingolstadt" is to cut the
    same size of city out of both. It also drops the footprint bounding box far enough for
    ``_BuildingRaster`` to keep its native 3 m cell instead of auto-coarsening to 6 m, which removes
    the raster-resolution confound from the same comparison.
    """
    man = load_manifest(run_dir)
    cfg = radio_config(man)
    rings = parse_buildings(poly_xml)
    n_all = len(rings)
    if bbox is not None:
        x0, y0, x1, y1 = bbox
        rings = [r for r in rings
                 if any(x0 <= px <= x1 and y0 <= py <= y1 for px, py in r)]
    if dt_s is None:
        dt_s = cam_gaps(run_dir).get("mean_s") or 1.0
    os.makedirs(os.path.join(out_dir, "ground_truth"), exist_ok=True)
    n_emit = n_kept = 0
    for name in ("gt_emissions_sample.jsonl", "gt_vehicle.jsonl"):
        src = os.path.join(run_dir, "ground_truth", name)
        dst = os.path.join(out_dir, "ground_truth", name)
        if not os.path.exists(src):
            continue
        if bbox is None or name != "gt_emissions_sample.jsonl":
            with open(src, "rb") as a, open(dst, "wb") as b:
                while True:
                    chunk = a.read(1 << 20)
                    if not chunk:
                        break
                    b.write(chunk)
            continue
        x0, y0, x1, y1 = bbox
        with open(src, encoding="utf-8") as a, open(dst, "w", encoding="utf-8", newline="\n") as b:
            for line in a:
                if not line.strip():
                    continue
                n_emit += 1
                e = json.loads(line)
                if x0 <= float(e["true_x"]) <= x1 and y0 <= float(e["true_y"]) <= y1:
                    n_kept += 1
                    b.write(line)
    shim = {
        "dataset_version": "shim/1",
        "generator": "tools/xengine_radio.py write_shim -- an awareness.load_scenario VIEW of a "
                     "MOSAIC dataset. NOT a dataset: no ma/, no ml/, no labels, nothing here is "
                     "training data.",
        "source_run": os.path.abspath(run_dir).replace("\\", "/"),
        "source_buildings": os.path.abspath(poly_xml).replace("\\", "/"),
        "config": {
            "radio_model": "geometric" if cfg["radio_model"] == "geometric" else cfg["radio_model"],
            "radio_env": cfg["radio_env"],
            "radio_tx_power_dbm": cfg["tx_power_dbm"],
            "radio_rx_sensitivity_dbm": cfg["rx_sensitivity_dbm"],
            "radio_nlosb_density_per_km": 0.0,
            "dt": float(dt_s),
            "emit_sample_prob": float((man.get("config") or {}).get("emit_sample_prob", 1.0)),
            "custom_network": json.dumps({"buildings": rings}),
        },
    }
    shim["bbox"] = list(bbox) if bbox else None
    with open(os.path.join(out_dir, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(shim, fh)
        fh.write("\n")
    return {"out_dir": out_dir, "buildings": len(rings), "buildings_before_bbox": n_all,
            "dt_s": float(dt_s), "bbox": list(bbox) if bbox else None,
            "emissions_read": n_emit, "emissions_kept": n_kept}


# =================================================================================================
# classifier A/B: the two engines' LOS tests on the SAME segments
# =================================================================================================
def classify_ab(run_dir: str, poly_xml: str, bbox=None, max_links: int = 400_000,
                cell_m: float = 3.0, bin_m: float = BIN_M,
                max_dist_m: float = MAX_DIST_M) -> dict:
    """Run the Python engine's ``_BuildingRaster`` over the links the Java ``BuildingIndex`` judged.

    The trace records both endpoints of every sampled reception decision, so the two geometry tests
    can be put on IDENTICAL segments. That is the only way to separate "the two engines disagree
    about Ingolstadt" from "the two engines were shown different pieces of Ingolstadt", and it
    isolates the two known implementation differences:

    * the Python side rasterises footprints at ``GEO_BUILDING_CELL_M`` and marks a whole cell
      occupied, so a wall is at least one cell thick; the Java side intersects the polygon EDGES
      exactly. The raster must therefore over-block, by more as the cell grows.
    * the Python side ignores raster hits within ``GEO_ENDPOINT_CLEAR_M`` = 6 m of either antenna
      (the road graph is RDP-simplified and an antenna can land on a building cell); the Java side
      has no such clearance. The clearance must therefore under-block near buildings.

    Both are evaluated: ``python_nlosb`` uses the engine's own 6 m clearance, ``python_nlosb_noclear``
    turns it off, so the two effects are reported separately rather than as one lumped residual.
    """
    from scms_sim_ref.mock_pipeline.run import GEO_ENDPOINT_CLEAR_M, _BuildingRaster

    rings = parse_buildings(poly_xml)
    n_rings_all = len(rings)
    if bbox is not None:
        x0, y0, x1, y1 = bbox
        pad = 50.0
        rings = [r for r in rings
                 if any(x0 - pad <= px <= x1 + pad and y0 - pad <= py <= y1 + pad for px, py in r)]
    raster = _BuildingRaster(rings, cell_m=cell_m)

    nb = int(max_dist_m // bin_m)
    keys = ("both_los", "both_nlosb", "java_los_py_nlosb", "java_nlosb_py_los")
    band = {k: np.zeros(nb, dtype=np.int64) for k in keys}
    n_java_nlosb = np.zeros(nb, dtype=np.int64)
    n_py_nlosb = np.zeros(nb, dtype=np.int64)
    n_py_nlosb_nc = np.zeros(nb, dtype=np.int64)
    n_band = np.zeros(nb, dtype=np.int64)
    n = 0
    with open(os.path.join(run_dir, "link_trace.csv"), encoding="utf-8", newline="") as fh:
        head = fh.readline().strip().split(",")
        if "rx_x" not in head:
            raise ValueError("link_trace.csv has no endpoint columns; re-run with the current app")
        ix = {k: head.index(k) for k in ("dist_m", "state", "rx_x", "rx_y", "tx_x", "tx_y")}
        for line in fh:
            f = line.rstrip("\n").split(",")
            if len(f) <= ix["tx_y"] or f[ix["state"]] not in STATES or not f[ix["tx_x"]]:
                continue
            rx = (float(f[ix["rx_x"]]), float(f[ix["rx_y"]]))
            tx = (float(f[ix["tx_x"]]), float(f[ix["tx_y"]]))
            if bbox is not None:
                x0, y0, x1, y1 = bbox
                if not (x0 <= rx[0] <= x1 and y0 <= rx[1] <= y1
                        and x0 <= tx[0] <= x1 and y0 <= tx[1] <= y1):
                    continue
            d = float(f[ix["dist_m"]])
            b = int(d // bin_m)
            if b >= nb:
                continue
            jb = f[ix["state"]] == "NLOSb"
            pb = raster.blocked(tx[0], tx[1], rx[0], rx[1], GEO_ENDPOINT_CLEAR_M)
            pbn = raster.blocked(tx[0], tx[1], rx[0], rx[1], 0.0)
            n_band[b] += 1
            n_java_nlosb[b] += jb
            n_py_nlosb[b] += pb
            n_py_nlosb_nc[b] += pbn
            band["both_nlosb" if (jb and pb) else
                 "both_los" if (not jb and not pb) else
                 "java_nlosb_py_los" if jb else "java_los_py_nlosb"][b] += 1
            n += 1
            if n >= max_links:
                break
    tot = int(n_band.sum())
    edges = np.arange(0.0, max_dist_m + bin_m, bin_m)
    agree = int(band["both_los"].sum() + band["both_nlosb"].sum())
    return {
        "run_dir": os.path.abspath(run_dir).replace("\\", "/"),
        "bbox": list(bbox) if bbox else None,
        "raster_cell_m": raster.cell, "raster_cells": raster.nx * raster.ny,
        "raster_requested_cell_m": cell_m,
        "buildings_used": len(rings), "buildings_in_file": n_rings_all,
        "links_compared": tot,
        "java_nlosb_fraction": _r(n_java_nlosb.sum() / tot) if tot else None,
        "python_nlosb_fraction": _r(n_py_nlosb.sum() / tot) if tot else None,
        "python_nlosb_fraction_no_endpoint_clearance": _r(n_py_nlosb_nc.sum() / tot) if tot else None,
        "agreement": _r(agree / tot) if tot else None,
        "confusion": {k: int(band[k].sum()) for k in keys},
        "by_band": [
            {"d_lo_m": float(edges[i]), "d_hi_m": float(edges[i + 1]), "n": int(n_band[i]),
             "java_nlosb": _r(n_java_nlosb[i] / n_band[i]),
             "python_nlosb": _r(n_py_nlosb[i] / n_band[i]),
             "python_nlosb_noclear": _r(n_py_nlosb_nc[i] / n_band[i]),
             "agreement": _r((band["both_los"][i] + band["both_nlosb"][i]) / n_band[i])}
            for i in range(nb) if n_band[i] >= MIN_BAND_N],
    }


# =================================================================================================
# cross-engine comparison
# =================================================================================================
COMPARE_ANCHORS_M = (50.0, 100.0, 200.0, 300.0, 500.0)


def compare(mosaic_panels: dict, py_awareness_json: str | None,
            py_on_mosaic_scene_json: str | None = None) -> list[str]:
    """Render the cross-engine tables. ``mosaic_panels`` is ``{label: measure() output}``."""
    L: list[str] = []
    pya = None
    if py_awareness_json and os.path.exists(py_awareness_json):
        with open(py_awareness_json, encoding="utf-8") as fh:
            pya = json.load(fh)
    pym = None
    if py_on_mosaic_scene_json and os.path.exists(py_on_mosaic_scene_json):
        with open(py_on_mosaic_scene_json, encoding="utf-8") as fh:
            pym = json.load(fh)

    L += ["## Link budget and radio configuration", "",
          "| | " + " | ".join(mosaic_panels) + (" | python-engine |" if pya else " |"),
          "|---|" + "---|" * (len(mosaic_panels) + (1 if pya else 0))]
    rows = [
        ("engine", lambda r: "mosaic-java", lambda a: "python"),
        ("radio model", lambda r: r["config"]["radio_model"], lambda a: a["config"]["radio_model"]),
        ("tx power dBm", lambda r: r["config"]["tx_power_dbm"], lambda a: a["config"]["tx_power_dbm"]),
        ("rx sensitivity dBm", lambda r: r["config"]["rx_sensitivity_dbm"],
         lambda a: a["config"]["rx_sensitivity_dbm"]),
        ("decode floor dBm", lambda r: r["config"]["rx_sensitivity_dbm"],
         lambda a: a["config"]["decode_floor_dbm"]),
        ("link budget dB", lambda r: r["config"]["link_budget_db"],
         lambda a: a["config"]["link_budget_db"]),
        ("range cap m", lambda r: r["config"]["sns_singlehop_radius_m"], lambda a: "none (curve to 1 km)"),
        ("DCC", lambda r: "on" if r["config"]["dcc_enabled"] else "off", lambda a: "absent"),
        ("CAM rate Hz", lambda r: round(r["cam_generation"].get("mean_rate_hz", 0), 3),
         lambda a: round(1.0 / a["config"]["dt_s"], 3)),
    ]
    for name, fm, fp in rows:
        cells = [str(fm(r)) for r in mosaic_panels.values()]
        if pya:
            cells.append(str(fp(pya)))
        L.append(f"| {name} | " + " | ".join(cells) + " |")

    # --- per-state PDR: the SCENE-FREE radio comparison ---------------------------------------
    any_panel = next(iter(mosaic_panels.values()), None)
    if any_panel:
        c = any_panel["config"]
        floor = aw.decode_floor_dbm(c["rx_sensitivity_dbm"])
        # .get with a False default: a radio_panel.json written before the NLOSv/fading work has
        # neither key, and must keep being scored against the radio that produced it.
        c_nlosv, c_fading = bool(c.get("nlosv")), bool(c.get("fading"))
        jkw = dict(tx_power_dbm=c["tx_power_dbm"], rx_sensitivity_dbm=c["rx_sensitivity_dbm"],
                   antenna_gain_dbi=c["antenna_gain_dbi"], fading=c_fading, nlosv=c_nlosv)
        L += ["", f"## Per-state per-packet PDR at a matched {c['link_budget_db']} dB budget "
                  f"-- the radio with no scene in it", "",
              f"Java physics on this arm: NLOSv {'on' if c_nlosv else 'OFF'}, "
              f"Nakagami fading {'on' if c_fading else 'OFF'}.", "",
              "| d (m) | LOS java | LOS python | NLOSb java | NLOSb python | NLOSv java | NLOSv python |",
              "|---|---|---|---|---|---|---|"]
        for a in COMPARE_ANCHORS_M:
            jl = java_pdr("LOS", a, **jkw)
            jn = java_pdr("NLOSb", a, **jkw)
            jv = java_pdr("NLOSv", a, **jkw) if c_nlosv else None
            pl_ = aw.propagation_pdr("LOS", a, tx_power_dbm=c["tx_power_dbm"],
                                     decode_floor_dbm=floor, radio_env=c["radio_env"])
            pn = aw.propagation_pdr("NLOSb", a, tx_power_dbm=c["tx_power_dbm"],
                                    decode_floor_dbm=floor, radio_env=c["radio_env"])
            pv = aw.propagation_pdr("NLOSv", a, tx_power_dbm=c["tx_power_dbm"],
                                    decode_floor_dbm=floor, radio_env=c["radio_env"])
            L.append("| {:g} | {:.4f} | {:.4f} | {:.4f} | {:.4f} | {} | {:.4f} |".format(
                a, jl, pl_, jn, pn,
                "n/a -- no NLOSv branch" if jv is None else f"{jv:.4f}", pv))

    # --- link-state composition, both engines, each on its own scene and on the shared one -----
    if pya or pym:
        L += ["", "## Link-state composition of the pair population", "",
              "| band | java on InTAS (transmission-weighted) | python on InTAS (co-presence) | "
              "python on its own OSM extract |", "|---|---|---|---|"]
        panel = next((p for p in mosaic_panels.values() if p.get("by_band")), None)
        jb = {int(b["d_lo_m"]): b for b in (panel or {}).get("by_band", [])}
        pm = {int(r["d_lo_m"]): r for r in (pym or {}).get("link_state_mix_by_band", [])}
        pp = {int(r["d_lo_m"]): r for r in (pya or {}).get("link_state_mix_by_band", [])}
        for lo in range(0, 500, 50):
            j = jb.get(lo)
            m = pm.get(lo)
            q = pp.get(lo)
            L.append("| {}-{} m | {} | {} | {} |".format(
                lo, lo + 50,
                (f"LOS {j['los']} / NLOSv {j['nlosv']} / NLOSb {j['nlosb']}"
                 if j and j.get("nlosv") else f"LOS {j['los']} / NLOSb {j['nlosb']}") if j else "-",
                f"LOS {m['los']} / NLOSv {m['nlosv']} / NLOSb {m['nlosb']}" if m else "-",
                f"LOS {q['los']} / NLOSv {q['nlosv']} / NLOSb {q['nlosb']}" if q else "-"))
    return L


# =================================================================================================
# rendering
# =================================================================================================
def render(rep: dict) -> list[str]:
    c, ag, cb, cg = rep["config"], rep["aggregate_channel"], rep["cbr"], rep["cam_generation"]
    L = [
        f"# MOSAIC/Java radio  ({rep['run_dir']})",
        "",
        f"model={c['radio_model']} env={c['radio_env']} tx={c['tx_power_dbm']} dBm "
        f"sens={c['rx_sensitivity_dbm']} dBm gain={c['antenna_gain_dbi']} dBi "
        f"budget={c['link_budget_db']} dB  DCC={'on' if c['dcc_enabled'] else 'off'}  "
        f"SNS singlehop radius={c['sns_singlehop_radius_m']} m  chan_capacity={c['chan_capacity']}",
        f"physics: NLOSv={'on' if c.get('nlosv') else 'OFF'}  "
        f"Nakagami fading={'on' if c.get('fading') else 'OFF'}",
        "",
        "AGGREGATE (every frame SNS handed the app)",
        f"  sensed {ag['frames_sensed']}  delivered {ag['frames_delivered']}  "
        f"ratio {ag['delivery_ratio_over_sns_disc']}",
        f"  dropped: obstruction {ag['dropped_obstruction']}  congestion {ag['dropped_congestion']}"
        f"  weather {ag['dropped_weather']}",
        f"  NLOSb fraction over all links {ag['nlosb_fraction_all_links']}  "
        f"({ag['buildings_indexed']} footprints, aligned={ag['buildings_aligned']})",
        f"  NLOSv links {ag['nlosv_links']} (fraction {ag['nlosv_fraction_all_links']}) from "
        f"{ag['blockers_declared']} declared blockers, {ag['blockers_truck_height']} at truck "
        f"height; peak live {ag['blocker_peak_live']} over {ag['blocker_snapshots']} snapshots",
        "",
        "CBR (ETSI TS 102 687 meter, always measured)",
        f"  mean {cb['cbr_mean']}  max {cb['cbr_max']}  samples {cb['cbr_samples']}  "
        f"airtime {cb['frame_airtime_s']} s",
        f"  CAMs allowed {cb['cams_allowed']}  suppressed by DCC {cb['cams_suppressed']}",
        "",
        "CAM GENERATION (EN 302 637-2 rules, honest vehicles only)",
        f"  gaps n={cg.get('n_gaps')}  mean {cg.get('mean_s'):.4f} s -> "
        f"{cg.get('mean_rate_hz'):.3f} Hz  median {cg.get('median_s')} s  "
        f"p05 {cg.get('p05_s')} p95 {cg.get('p95_s')} max {cg.get('max_s')}",
        f"  at T_GenCamMin (0.1 s) {cg.get('frac_at_t_gencam_min_0p1s'):.4f}   "
        f"between {cg.get('frac_between'):.4f}   "
        f"at T_GenCamMax (1.0 s) {cg.get('frac_at_t_gencam_max_1p0s'):.4f}",
    ]
    lt = rep.get("link_trace") or {}
    if not lt.get("present"):
        L += ["", "LINK TRACE: " + str(lt.get("reason"))]
        return L
    L += ["", f"LINK TRACE  {lt['decisions_binned']} binned decisions "
              f"(sample p={lt['sample_probability']})",
          "",
          "  band       n     LOS  NLOSv  NLOSb |  PDR(prop) PDR(+cont) | java-cf  py-phys",
          "  " + "-" * 81]
    for b in rep["by_band"]:
        if b["n_decisions"] < MIN_BAND_N:
            continue
        L.append("  {:>4.0f}-{:<4.0f} {:>8d}  {:>6} {:>6} {:>6}  |  {:>7} {:>9}  | {:>7} {:>8}".format(
            b["d_lo_m"], b["d_hi_m"], b["n_decisions"], b["los"], b["nlosv"], b["nlosb"],
            b["pdr_propagation"], b["pdr_with_contention"],
            b["java_closed_form_pdr"], b["python_physics_same_scene_pdr"]))
    tv = rep["trace_vs_closed_form"]
    L += ["", f"  trace vs the Java model's own rule: max |diff| {tv['max_abs_diff']:.4f}, "
              f"weighted mean {tv['weighted_mean_abs_diff']:.4f} over {tv['bands_compared']} bands"]
    for key, title in (("crossings_measured", "MEASURED (trace, propagation only)"),
                       ("crossings_java_closed_form", "JAVA closed form on the measured mix"),
                       ("crossings_python_physics_same_scene", "PYTHON physics on the SAME scene")):
        x = rep[key]
        L += ["", title,
              f"  d90/d50/d20 = {x['pdr_0p90_m']} / {x['pdr_0p50_m']} / {x['pdr_0p20_m']} m   "
              f"gray-zone ratio {x['gray_zone_ratio_d20_over_d90']}",
              f"  NAR-0.90-equivalent range {x['nar90_equivalent_range_m']} m "
              f"(Z bracket {x['nar90_equivalent_z_range_m']})",
              f"  reference at this budget  {x['reference_nar90_distance_m']} m -> ratio "
              f"{x['ratio_model_over_reference']}"]
    L += ["", "ANCHORS (annulus of +/- 25 m)"]
    for a, r in rep["anchors"].items():
        L.append(f"  @{a:>4} m  measured {r['pdr_measured']}  java-cf {r['pdr_java_closed_form']}  "
                 f"py-phys {r['pdr_python_physics_same_scene']}  mix {r['link_state_mix']}")
    return L


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    m = sub.add_parser("measure", help="measure one MOSAIC run's radio")
    m.add_argument("run_dir")
    m.add_argument("--json", dest="json_out", default=None)
    m.add_argument("--markdown", action="store_true")
    m.add_argument("--bin-m", type=float, default=BIN_M)
    m.add_argument("--max-dist-m", type=float, default=MAX_DIST_M)

    s = sub.add_parser("shim", help="write an awareness-readable view of a MOSAIC run")
    s.add_argument("run_dir")
    s.add_argument("--buildings", required=True, help="path to sumo/buildings.poly.xml")
    s.add_argument("--out", required=True)
    s.add_argument("--dt", type=float, default=None)
    s.add_argument("--bbox", default=None,
                   help="min_x,min_y,max_x,max_y -- restrict footprints AND emissions to a sub-area")

    ab = sub.add_parser("classify-ab",
                        help="run the Python raster over the links the Java index judged")
    ab.add_argument("run_dir")
    ab.add_argument("--buildings", required=True)
    ab.add_argument("--bbox", default=None)
    ab.add_argument("--max-links", type=int, default=400_000)
    ab.add_argument("--cell", type=float, default=3.0)
    ab.add_argument("--json", dest="json_out", default=None)

    cp = sub.add_parser("compare", help="cross-engine tables from measured panels")
    cp.add_argument("runs", nargs="+", help="label=run_dir")
    cp.add_argument("--python-awareness", default=None,
                    help="awareness.py --json output for the PYTHON engine's own dataset")
    cp.add_argument("--python-on-mosaic-scene", default=None,
                    help="awareness.py --json output for the MOSAIC shim (python instrument, "
                         "MOSAIC scene)")

    a = p.parse_args(argv)
    if a.cmd == "compare":
        panels = {}
        for spec in a.runs:
            label, _, path = spec.partition("=")
            if not path:
                label, path = os.path.basename(spec.rstrip("/\\")), spec
            pj = os.path.join(path, "radio_panel.json")
            if os.path.exists(pj):
                with open(pj, encoding="utf-8") as fh:
                    panels[label] = json.load(fh)
            else:
                panels[label] = measure(path)
        print("\n".join(compare(panels, a.python_awareness, a.python_on_mosaic_scene)))
        return 0
    if a.cmd == "classify-ab":
        bb = None
        if a.bbox:
            v = [float(x) for x in a.bbox.split(",")]
            if len(v) != 4:
                p.error("--bbox wants min_x,min_y,max_x,max_y")
            bb = tuple(v)
        rep = classify_ab(a.run_dir, a.buildings, bb, a.max_links, a.cell)
        print(json.dumps(rep, indent=2, default=str))
        if a.json_out:
            with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
                json.dump(rep, fh, indent=2, default=str)
                fh.write("\n")
        return 0
    if a.cmd == "measure":
        rep = measure(a.run_dir, a.bin_m, a.max_dist_m)
        if a.markdown:
            print("\n".join(render(rep)))
        else:
            print(json.dumps(rep, indent=2, default=str))
        if a.json_out:
            with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
                json.dump(rep, fh, indent=2, default=str)
                fh.write("\n")
            print(f"\n[wrote {a.json_out}]")
        return 0
    if a.cmd == "shim":
        bb = None
        if a.bbox:
            v = [float(x) for x in a.bbox.split(",")]
            if len(v) != 4:
                p.error("--bbox wants min_x,min_y,max_x,max_y")
            bb = (v[0], v[1], v[2], v[3])
        print(json.dumps(write_shim(a.run_dir, a.buildings, a.out, a.dt, bb), indent=2))
        return 0
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
