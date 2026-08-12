"""Domain-randomization campaign: generate a SOTA training dataset that generalizes to reality.

The single biggest lever for sim-to-real transfer is DOMAIN RANDOMIZATION: instead of one dataset
at fixed parameters, generate many scenarios ("domains") each with a WIDELY randomized environment
(map, traffic, sensor noise, radio, faults, attacker mix, detector thresholds), then merge them into
one dataset with a `domain_id` per sample. A model trained across this envelope cannot rely on any
fixed simulator constant and is forced onto the invariant misbehaviour signal — which is what makes
it hold up on real-world conditions it never saw.

    python -m scms_sim_ref.datagen.campaign --domains 20 --seed 7 --out datasets/campaign

Runs each domain through run.ps1 (build once, then -SkipBuild), namespaces ids per domain, merges
the ML tables, writes a campaign manifest with the per-domain parameters + metrics, and benchmarks
the merged dataset (including leave-one-attack-family-out generalization).
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
from pathlib import Path

import pandas as pd

REPO = Path(__file__).resolve().parents[3]
RUN_PS1 = REPO / "run.ps1"
TABLES = ["report_features", "report_labels", "subject_features", "subject_labels",
          "vehicle_features", "vehicle_labels"]
ID_COLS = {"report_id", "subject_cert_digest", "reporter_cert_digest", "entity_id",
           "true_vehicle_id", "subject_true_id", "reporter_true_id"}
OSM_SAMPLE = ["osm_munich", "osm_manhattan", "osm_tokyo", "osm_paris", "osm_singapore"]


def sample_domain(rng: random.Random, idx: int, base_seed: int, osm: bool) -> dict:
    """Draw one domain: a map + a wide random draw over every environment parameter."""
    p = rng.random()
    if p < 0.42:
        scen = f"grid_{rng.choice([4, 5, 6, 8])}x{rng.choice([4, 5, 6, 8])}_s{rng.randint(1, 3)}"
    elif p < 0.60:
        scen = f"spider_{rng.choice([4, 6, 8])}a{rng.choice([3, 4, 5])}c_s{rng.randint(1, 3)}"
    elif p < 0.75:
        scen = f"rand_{rng.choice([80, 120, 160, 200])}_s{rng.randint(1, 3)}"
    elif p < 0.93 or not osm:
        scen = rng.choice(["highway", "barnim", "tiergarten", "intas_urban_low", "intas_highway_low"])
    else:
        scen = rng.choice(OSM_SAMPLE)
    env = {
        # population
        "SCMS_ATTACKER_PCT": rng.randint(8, 35),
        "SCMS_FAULTY_PCT": rng.randint(0, 12),
        # sensor / GNSS
        "SCMS_GPS_SIGMA": round(rng.uniform(0.8, 4.0), 2),
        "SCMS_GPS_BIAS": round(rng.uniform(1.0, 6.0), 2),
        "SCMS_GPS_OUTLIER_RATE": round(rng.uniform(0.002, 0.02), 4),
        "SCMS_GPS_DEGRADE_RATE": round(rng.uniform(0.0, 0.010), 4),
        "SCMS_HEADING_SIGMA": round(rng.uniform(1.0, 4.0), 2),
        # radio / channel
        "SCMS_RADIO_RANGE": rng.choice([300, 500, 709, 1000, 1500]),
        "SCMS_RADIO_LOSS": round(rng.uniform(0.0, 0.15), 2),
        "SCMS_CHAN_CAPACITY": rng.choice([15, 25, 40, 80]),
        "SCMS_CAM_INTERVAL": rng.choice([0.5, 1.0]),
        "SCMS_WEATHER": rng.choice(["clear", "clear", "rain", "fog", "snow"]),
        "SCMS_NLOS": rng.choice([0.0, 0.0, 0.3, 0.6]),
        # SCMS policy
        "SCMS_REPORT_PROB": round(rng.uniform(0.6, 1.0), 2),
        "SCMS_REPORT_K": rng.choice([2, 3, 4]),
        "SCMS_ROTATE_PERIOD": rng.choice([0, 60, 90, 120, 300]),
        # collusion / false-accusation threat present in ~40% of domains
        "SCMS_COLLUDE_PCT": rng.choice([0, 0, 0, 15, 30, 45]),
        "SCMS_VICTIM_PCT": rng.choice([2, 3, 4]),
        # traffic / fleet
        "SCMS_VEH_SPEEDFACTOR": round(rng.uniform(0.85, 1.2), 2),
        "SCMS_FLEET": rng.choice(["mixed", "car"]),
        "SCMS_DEMAND": rng.choice(["uniform", "rush", "night"]),
        # detector thresholds jittered, so the model can't memorise one fixed operating point
        "SCMS_ART_MAX_M": rng.choice([600, 1000, 1500]),
        "SCMS_STALE_MAX": rng.choice([3, 5, 8]),
        "SCMS_SPEED_TOL": rng.choice([8, 10, 15]),
        "SCMS_SYBIL_MIN": rng.choice([4, 5, 6]),
        "SCMS_ATTACKS": "all",
    }
    return {"idx": idx, "scenario": scen, "duration": rng.choice(["120s", "150s", "180s"]),
            # keep the per-domain seed inside Int32 (run.ps1 -Seed and MOSAIC randomSeed are 32-bit)
            "seed": (base_seed + idx * 100003) % 2_000_000_000, "env": env}


def run_domain(dom: dict, out_dir: Path, skip_build: bool) -> dict:
    env = {**os.environ, **{k: str(v) for k, v in dom["env"].items()}, "PYTHONPATH": str(REPO / "src")}
    cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(RUN_PS1),
           "-Scenario", dom["scenario"], "-Duration", dom["duration"], "-Seed", str(dom["seed"]),
           "-OutDir", str(out_dir)]
    if skip_build:
        cmd.append("-SkipBuild")
    r = subprocess.run(cmd, cwd=str(REPO), env=env, capture_output=True, text=True)
    metrics = {}
    man = out_dir / "manifest.json"
    if man.exists():
        try:
            sys.path.insert(0, str(REPO / "src"))
            from scms_sim_ref.datagen import validate as vmod
            metrics, _ = vmod.validate(str(out_dir))
            metrics = {k: metrics[k] for k in ("vehicles", "attackers", "precision", "recall") if k in metrics}
        except Exception:
            pass
    return {"ok": man.exists(), "returncode": r.returncode, "metrics": metrics}


def merge(domains: list[dict], base: Path):
    """Concatenate every domain's ML tables, namespacing ids + adding domain_id."""
    out = base / "merged" / "ml"
    out.mkdir(parents=True, exist_ok=True)
    for tbl in TABLES:
        frames = []
        for dom in domains:
            csv = base / f"dom_{dom['idx']:03d}" / "ml" / f"{tbl}.csv"
            if not csv.exists():
                continue
            df = pd.read_csv(csv)
            if len(df) == 0:
                continue
            for c in ID_COLS & set(df.columns):
                df[c] = f"d{dom['idx']}_" + df[c].astype(str)
            df.insert(0, "domain_id", dom["idx"])
            frames.append(df)
        if frames:
            merged = pd.concat(frames, ignore_index=True)
            merged.to_csv(out / f"{tbl}.csv", index=False)
            merged.to_parquet(out / f"{tbl}.parquet", index=False)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Domain-randomization dataset campaign.")
    ap.add_argument("--domains", type=int, default=12)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--out", default=str(REPO / "datasets" / "campaign"))
    ap.add_argument("--osm", action="store_true", help="include a few real-city OSM domains (needs internet)")
    a = ap.parse_args(argv)

    base = Path(a.out)
    if base.exists():
        shutil.rmtree(base)
    base.mkdir(parents=True)
    rng = random.Random(a.seed)
    domains = [sample_domain(rng, i, a.seed, a.osm) for i in range(a.domains)]

    print(f"== domain-randomization campaign: {a.domains} domains, seed {a.seed} ==", flush=True)
    for i, dom in enumerate(domains):
        out_dir = base / f"dom_{dom['idx']:03d}"
        print(f"-- domain {i + 1}/{a.domains}: {dom['scenario']} ({dom['duration']}) ...", flush=True)
        res = run_domain(dom, out_dir, skip_build=(i > 0))
        dom["result"] = res
        print(f"   ok={res['ok']} metrics={res['metrics']}", flush=True)

    merge([d for d in domains if d.get("result", {}).get("ok")], base)
    manifest = {"n_domains": a.domains, "seed": a.seed,
                "domains": [{"idx": d["idx"], "scenario": d["scenario"], "duration": d["duration"],
                             "seed": d["seed"], "env": d["env"], "result": d.get("result", {})}
                            for d in domains]}
    (base / "campaign_manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")

    # benchmark the merged dataset (incl. leave-one-attack-family-out generalization)
    merged = base / "merged"
    if (merged / "ml" / "vehicle_features.csv").exists():
        sys.path.insert(0, str(REPO / "src"))
        from scms_sim_ref.datagen import benchmark as bmod
        bench = bmod.run(str(merged))
        (base / "merged_benchmark.json").write_text(json.dumps(bench, indent=2, default=str), encoding="utf-8")
        print("\n== merged dataset benchmark ==")
        for k, t in bench.get("tasks", {}).items():
            if "note" not in t:
                print(f"  {k:26s} roc_auc={t.get('roc_auc')}  n_test={t.get('n_test')}")
        gen = bench.get("generalization", {}).get("vehicle_novel_attack")
        if gen:
            print(f"  novel-attack generalization: mean_auc={gen.get('mean_novel_attack_auc')}  "
                  f"per_family={gen.get('per_family_auc')}")
    print(f"\nDONE. Campaign at {base}  (merged/ml/*, campaign_manifest.json, merged_benchmark.json)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
