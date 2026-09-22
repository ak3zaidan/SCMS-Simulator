"""Run the FROZEN legacy Python engine over the attack catalog and report its detector
decisions, in two regimes.

This is the yardstick half of the cross-engine comparison. Nothing here re-implements a
legacy formula: `run_pipeline` is called as it stands, and the numbers are read off the
tables it writes (`ma/ma_reports.jsonl`, `ground_truth/gt_report_labels.jsonl`,
`ground_truth/gt_vehicle.jsonl`).

Per ADR 0004 the legacy fixed digests are retired, so what is compared is *decisions and
rates*, never a digest.

    ideal        gps noise off, faulty sensors off, hard reception disc, report_prob 1.0
    realistic    the legacy defaults

Usage:  legacy_rates.py <out.json> [ideal|realistic] [duration_s] [dt] [n_vehicles]
"""

import json
import os
import shutil
import sys
import collections

# The frozen reference lives at <repo>/legacy; this file is <repo>/crates/v2xw-threat/
# tests/legacy/, so the path is relative to it and the script moves with the repository.
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                "..", "..", "..", "..", "legacy")))

from scms_sim_ref.mock_pipeline.run import (  # noqa: E402
    ATTACK_CATALOG,
    PipelineConfig,
    run_pipeline,
)

OUT_ROOT = os.environ.get("V2XW_LEGACY_OUT",
                          os.path.join(os.path.dirname(os.path.abspath(sys.argv[1])),
                                       "legacy_runs"))


def config(regime: str, attack: str, duration_s: float, dt: float, n_vehicles: int,
           attacker_pct: float, seed: int, out_dir: str) -> PipelineConfig:
    common = dict(
        seed=seed,
        n_vehicles=n_vehicles,
        attacker_pct=attacker_pct,
        n_steps=int(round(duration_s / dt)),
        duration_s=duration_s,
        dt=dt,
        attack_type=attack,
        attack_start=5.0,
        attack_end=1.0e9,
        out_dir=out_dir,
        emit_sample_prob=0.0,
        # The road network: a grid, which is the legacy topology closest to the
        # Midtown-shaped procedural lattice the Rust harness runs on. 13x34 junctions at
        # 274 m x 80 m is not expressible here (grid_block_m is one number), so the
        # comparable choice is a grid of the same junction count and a block size between
        # the two spacings. This is one of the differences the report names.
        road_network="grid",
        grid_w=13,
        grid_h=13,
        grid_block_m=120.0,
        # ONE lane per road, and routed car-following.
        #
        # Both are load-bearing and both were found by running it. With
        # `traffic_flow=False` (the legacy default) vehicles are NOT bound to the road
        # network -- the fixed-fleet model moves them along straight lines -- so the
        # legacy engine's own `mapOffRoad` check fires on honest traffic: a benign vehicle
        # 19 m from the nearest gridline scores 19/15 = 1.27 and is reported. Measured at
        # `n_lanes=2, traffic_flow=False`: 37 543 of 53 010 reports were `mapOffRoad`
        # against benign subjects, and the benign false-positive rate was 0.91. Routed
        # flow puts the vehicles on the roads the map knows about, which is the condition
        # the check was written for and the condition the Rust harness runs in.
        n_lanes=1,
        traffic_flow=True,
        arrival_rate=1.0,
        max_total_vehicles=60,
        radio_range_m=500.0,
        # The detector operating point: the numbers v2xw-threat's DetectorParams defaults
        # are ported from. Stated rather than defaulted, so the two engines are known to
        # be at the same operating point.
        consistency_threshold_m=5.0,
        heading_threshold_deg=35.0,
        detector_lag_s=1.5,
        detector_z_threshold=3.0,
        detector_min_consec=2,
        sybil_min_certs=4,
        sybil_cell_m=3.0,
        art_max_m=150.0,
        offroad_tol_m=15.0,
        max_accel_mps2=12.0,
        freq_max=6.0,
        stale_max_s=5.0,
        dos_burst=12,
        delay_s=6.0,
        sybil_ghosts=6,
        report_threshold_k=3,
        revoke_min_seconds=4,
        revoke_persist_s=3.0,
        revoke_window_s=15.0,
        ma_defense=True,
        rotate_period_s=0.0,
        collude_pct=0.0,
    )
    if regime == "ideal":
        common.update(
            gps_sigma_m=0.0,
            gps_bias_sigma_m=0.0,
            gps_outlier_rate=0.0,
            gps_degrade_rate=0.0,
            gps_jam_rate=0.0,
            faulty_pct=0.0,
            report_prob=1.0,
            radio_model="disc",
            packet_loss_base=0.0,
            nlos_loss=0.0,
            net_delay_max=0.0,
        )
    else:
        common.update(
            gps_sigma_m=1.2,
            gps_bias_sigma_m=1.5,
            gps_outlier_rate=0.01,
            gps_degrade_rate=0.006,
            faulty_pct=0.05,
            report_prob=0.9,
            radio_model="logdistance",
            pathloss_exponent=2.7,
            shadowing_sigma_db=4.0,
            net_delay_max=2.0,
        )
    return PipelineConfig(**common)


def measure(out_dir: str, revoked_digests) -> dict:
    def rows(rel):
        p = os.path.join(out_dir, rel)
        if not os.path.exists(p):
            return []
        with open(p, encoding="utf-8") as fh:
            return [json.loads(l) for l in fh if l.strip()]

    vehicles = rows("ground_truth/gt_vehicle.jsonl")
    labels = rows("ground_truth/gt_report_labels.jsonl")
    reports = rows("ma/ma_reports.jsonl")
    crl = rows("ma/ma_crl_events.jsonl")
    idmap = rows("ground_truth/gt_identity_map.jsonl")

    attacker = {v["true_vehicle_id"]: bool(v["is_attacker"]) for v in vehicles}
    faulty = {v["true_vehicle_id"]: bool(v["is_faulty"]) for v in vehicles}
    reported = set(l["subject_true_id"] for l in labels)

    tp = sum(1 for v, a in attacker.items() if a and v in reported)
    fn = sum(1 for v, a in attacker.items() if a and v not in reported)
    fp = sum(1 for v, a in attacker.items() if not a and v in reported)
    tn = sum(1 for v, a in attacker.items() if not a and v not in reported)

    digest_to_vehicle = {r["pseudonym_cert_digest"]: r["true_vehicle_id"] for r in idmap}
    # `ma_crl_events` carries counts rather than digests, so the revoked set comes from
    # the run result's own `revoked_cert_digests`, resolved through the oracle id map.
    revoked = {digest_to_vehicle[d] for d in (revoked_digests or [])
               if d in digest_to_vehicle}
    _ = crl
    rev_tp = sum(1 for v in revoked if attacker.get(v))
    rev_fp = sum(1 for v in revoked if not attacker.get(v))

    lead = collections.Counter()
    for r in reports:
        codes = r.get("reason_codes") or []
        if codes:
            lead[codes[0]] += 1

    # Every check's peak score over every report, for the per-detector comparison.
    peak = {}
    for r in reports:
        for k, v in r.items():
            if k.startswith("detnorm_") and isinstance(v, (int, float)):
                name = k[len("detnorm_"):]
                if v > peak.get(name, 0.0):
                    peak[name] = v

    return dict(
        vehicles=len(vehicles),
        attackers=sum(1 for a in attacker.values() if a),
        faulty=sum(1 for f in faulty.values() if f),
        reports=len(reports),
        tp=tp, fp=fp, fn=fn, tn=tn,
        recall=(tp / (tp + fn)) if (tp + fn) else None,
        fpr=(fp / (fp + tn)) if (fp + tn) else None,
        precision=(tp / (tp + fp)) if (tp + fp) else None,
        revoked=len(revoked), revoked_tp=rev_tp, revoked_fp=rev_fp,
        leading=dict(lead),
        peak=peak,
    )


def main() -> int:
    out_json = sys.argv[1]
    regime = sys.argv[2] if len(sys.argv) > 2 else "ideal"
    duration_s = float(sys.argv[3]) if len(sys.argv) > 3 else 60.0
    dt = float(sys.argv[4]) if len(sys.argv) > 4 else 0.1
    n_vehicles = int(sys.argv[5]) if len(sys.argv) > 5 else 60
    attacker_pct = float(sys.argv[6]) if len(sys.argv) > 6 else 0.25
    seed = int(sys.argv[7]) if len(sys.argv) > 7 else 1001

    results = {}
    for attack in ATTACK_CATALOG:
        out_dir = os.path.join(OUT_ROOT, f"{regime}_{attack}")
        shutil.rmtree(out_dir, ignore_errors=True)
        cfg = config(regime, attack, duration_s, dt, n_vehicles, attacker_pct, seed, out_dir)
        res = run_pipeline(cfg)
        m = measure(out_dir, res.revoked_cert_digests)
        m["data_digest"] = res.data_digest
        results[attack] = m
        print(f"{attack:20} attackers {m['attackers']:3} reports {m['reports']:6} "
              f"recall {m['recall']} fpr {m['fpr']} revoked {m['revoked']} "
              f"lead {sorted(m['leading'].items(), key=lambda kv: -kv[1])[:3]}",
              flush=True)
        shutil.rmtree(out_dir, ignore_errors=True)

    payload = dict(
        regime=regime, duration_s=duration_s, dt=dt, n_vehicles=n_vehicles,
        attacker_pct=attacker_pct, seed=seed, results=results,
    )
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(payload, fh, indent=1, sort_keys=True)
    print(f"wrote {out_json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
