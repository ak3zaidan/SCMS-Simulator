"""Massive grid campaign: generate ONE big training dataset across every scenario x permutation.

Where `campaign.py` randomly *samples* domains (and drives the Java/MOSAIC stack), this enumerates a
full FACTORIAL grid over the pure-Python generator: every attack scenario (each attack type in
isolation, plus a mixed "ALL" scenario) crossed with every permutation of the environment axes
(weather x rotation x collusion x faults x attacker density x fleet size). Each cell is one
deterministic pipeline run; all cells are featurized and MERGED into a single dataset with a
`domain_id` per row -- ready for cross-domain training and the leave-one-domain-out benchmark.

    python -m scms_sim_ref.datagen.massive --grid full  --out datasets/massive
    python -m scms_sim_ref.datagen.massive --grid quick --out datasets/massive_quick

Scales to thousands of cells: each domain is streamed into the merged CSVs and its per-domain
directory is deleted immediately (unless --keep-domains), so disk and memory stay bounded. Nothing is
truncated silently -- if a cap drops cells, it is logged.
"""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import random
import shutil
import sys
from pathlib import Path

import pandas as pd

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "src"))
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline   # noqa: E402
from scms_sim_ref.datagen import featurize as featmod                 # noqa: E402
from scms_sim_ref.datagen import validate as valmod                   # noqa: E402

TABLES = ["report_features", "report_labels", "subject_features", "subject_labels",
          "vehicle_features", "vehicle_labels", "vehicle_features_ma", "vehicle_labels_ma",
          "graph_edges", "subject_windows"]
ID_COLS = {"report_id", "subject_cert_digest", "reporter_cert_digest", "entity_id",
           "true_vehicle_id", "subject_true_id", "reporter_true_id", "src_entity", "dst_entity"}

# The 13 falsification attacks + Sybil; "ALL" is the mixed scenario (every type at once).
ATTACK_TYPES = list(PipelineConfig().attack_types)
SCENARIOS = ATTACK_TYPES + ["ALL"]

# Grid axes. `full` enumerates the entire Cartesian product; `quick` is a small smoke grid.
GRIDS = {
    "full": {
        "scenario": SCENARIOS,
        "weather": ["clear", "rain", "fog", "snow"],
        "rotate_period_s": [0.0, 60.0],
        "collude_pct": [0.0, 0.5],
        "faulty_pct": [0.0, 0.1],
        "attacker_pct": [0.15, 0.3],
        "n_vehicles": [60, 120],
    },
    "quick": {
        "scenario": ["ConstPos", "RandomSpeed", "ReversedHeading", "SlowDrift", "Sybil", "ALL"],
        "weather": ["clear", "rain"],
        "rotate_period_s": [0.0],
        "collude_pct": [0.0, 0.5],
        "faulty_pct": [0.05],
        "attacker_pct": [0.2],
        "n_vehicles": [60],
    },
}


def enumerate_cells(grid: dict) -> list[dict]:
    """Full Cartesian product of the grid axes, in a stable order."""
    keys = list(grid)
    cells = []
    for combo in itertools.product(*(grid[k] for k in keys)):
        cells.append(dict(zip(keys, combo)))
    return cells


def cell_config(cell: dict, idx: int, base_seed: int, n_steps: int, out_dir: Path,
                flow: bool = False, flow_duration: float = 0.0) -> PipelineConfig:
    scen = cell["scenario"]
    attack_types = tuple(ATTACK_TYPES) if scen == "ALL" else (scen,)
    kw = dict(
        seed=(base_seed + idx * 100003) % 2_000_000_000,
        n_vehicles=cell["n_vehicles"], n_steps=n_steps,
        attacker_pct=cell["attacker_pct"], attack_types=attack_types,
        attack_type=(scen if scen != "ALL" else "ConstPos"),
        faulty_pct=cell["faulty_pct"], weather=cell["weather"],
        rotate_period_s=cell["rotate_period_s"],
        collude_pct=cell["collude_pct"], victim_pct=0.12,
        out_dir=str(out_dir))
    if flow:
        # each domain is a long routed simulation with car-following + a demand profile, and
        # (as permutation axes) signalized/unsignalized intersections and mixed/car fleets
        kw.update(traffic_flow=True, road_network="grid", car_following=True,
                  duration_s=flow_duration, arrival_rate=2.0, grid_w=6, grid_h=6, n_lanes=2,
                  grid_block_m=140.0, demand_profile=cell.get("demand", "uniform"),
                  traffic_lights=bool(cell.get("lights", False)), fleet=cell.get("fleet", "mixed"),
                  attack_intensity=cell.get("intensity", 1.0))
    return PipelineConfig(**kw)


def _append(df: pd.DataFrame, idx: int, path: Path, header_written: set) -> int:
    if df is None or len(df) == 0:
        return 0
    for c in ID_COLS & set(df.columns):
        df[c] = f"d{idx}_" + df[c].astype(str)
    df.insert(0, "domain_id", idx)
    df.to_csv(path, mode="a", header=(path.name not in header_written), index=False)
    header_written.add(path.name)
    return len(df)


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Massive factorial grid dataset (every scenario x permutation).")
    ap.add_argument("--grid", choices=list(GRIDS), default="quick")
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--steps", type=int, default=80)
    ap.add_argument("--max-domains", type=int, default=0, help="cap cells (0 = no cap / full grid)")
    ap.add_argument("--sample", action="store_true", help="if capped, randomly sample cells instead of taking the first")
    ap.add_argument("--keep-domains", action="store_true", help="keep each per-domain dir (default: delete after merge)")
    ap.add_argument("--parquet", action="store_true", help="also write merged parquet (memory-heavy at scale)")
    ap.add_argument("--flow", action="store_true", help="each domain is a long routed flow simulation")
    ap.add_argument("--flow-duration", type=float, default=300.0, help="flow: seconds per domain")
    ap.add_argument("--out", default=str(REPO / "datasets" / "massive"))
    a = ap.parse_args(argv)

    grid = dict(GRIDS[a.grid])
    if a.flow:
        # under flow, demand / signals / fleet become permutation axes; n_vehicles is irrelevant
        grid = {**grid, "demand": ["uniform", "rush", "night"], "lights": [False, True],
                "fleet": ["mixed", "car"], "intensity": [1.0, 0.5], "n_vehicles": [0]}
    cells = enumerate_cells(grid)
    total = len(cells)
    dropped = 0
    if a.max_domains and total > a.max_domains:
        if a.sample:
            cells = random.Random(a.seed).sample(cells, a.max_domains)
        else:
            cells = cells[:a.max_domains]
        dropped = total - len(cells)

    base = Path(a.out)
    if base.exists():
        shutil.rmtree(base)
    (base / "ml").mkdir(parents=True)
    (base / "domains").mkdir(parents=True)

    print(f"== massive grid '{a.grid}': {total} cells in the product, running {len(cells)}"
          + (f" (CAP dropped {dropped})" if dropped else "") + f", seed {a.seed} ==", flush=True)
    print(f"   axes: " + ", ".join(f"{k}({len(v)})" for k, v in grid.items()), flush=True)

    header_written: set = set()
    catalog, row_counts, failed = [], {t: 0 for t in TABLES}, []
    for idx, cell in enumerate(cells):
        dom_dir = base / "domains" / f"d{idx:04d}"
        # Isolate each cell: one bad domain (e.g. a degenerate config) must not throw away the
        # thousands of good ones already merged. Failures are RECORDED (not silently dropped).
        try:
            cfg = cell_config(cell, idx, a.seed, a.steps, dom_dir, flow=a.flow, flow_duration=a.flow_duration)
            res = run_pipeline(cfg)
            featmod.build(str(dom_dir), split_seed=1234)
            for tbl in TABLES:
                csv = dom_dir / "ml" / f"{tbl}.csv"
                if csv.exists():
                    df = pd.read_csv(csv)
                    row_counts[tbl] += _append(df, idx, base / "ml" / f"{tbl}.csv", header_written)
            # per-domain difficulty labels (from its own ground truth, before the dir is deleted):
            # lets a trainer curriculum-weight or stratify the merged corpus by how hard each domain is.
            vs = valmod.validate(str(dom_dir))[0]
            catalog.append({"domain_id": idx, **cell, "seed": cfg.seed,
                            "reports": res.n_reports, "revoked": res.n_revoked,
                            "precision": vs.get("precision"), "recall": vs.get("recall"),
                            "recall_by_family": vs.get("recall_by_family", {})})
        except Exception as e:                       # noqa: BLE001 -- keep the campaign alive
            failed.append({"domain_id": idx, **cell, "error": f"{type(e).__name__}: {e}"})
            print(f"   [{idx + 1}/{len(cells)}] FAILED domain {idx}: {type(e).__name__}: {e}", flush=True)
        finally:
            if not a.keep_domains:
                shutil.rmtree(dom_dir, ignore_errors=True)
        if (idx + 1) % 25 == 0 or idx + 1 == len(cells):
            print(f"   [{idx + 1}/{len(cells)}] merged; report rows so far={row_counts['report_features']}"
                  + (f"; {len(failed)} failed" if failed else ""), flush=True)

    if not a.keep_domains:
        shutil.rmtree(base / "domains", ignore_errors=True)

    if a.parquet:
        for tbl in TABLES:
            csv = base / "ml" / f"{tbl}.csv"
            if csv.exists():
                pd.read_csv(csv).to_parquet(base / "ml" / f"{tbl}.parquet", index=False)

    # merged manifest + grid catalog + data digest over the merged tables
    outputs = {}
    for tbl in TABLES:
        p = base / "ml" / f"{tbl}.csv"
        if p.exists():
            outputs[f"ml/{tbl}.csv"] = _sha256(p)
    dh = hashlib.sha256()
    for rel in sorted(outputs):
        dh.update(rel.encode()); dh.update(outputs[rel].encode())
    manifest = {
        "generator": "scms_sim_ref.datagen.massive (factorial grid)",
        "grid": a.grid, "seed": a.seed, "n_cells_in_product": total,
        "n_domains_run": len(cells), "n_domains_ok": len(catalog),
        "n_domains_failed": len(failed), "n_dropped_by_cap": dropped,
        "axes": {k: v for k, v in grid.items()},
        "row_counts": row_counts, "data_digest_sha256": dh.hexdigest(),
        "failed_domains": failed,
        "outputs": [{"path": k, "sha256": v} for k, v in sorted(outputs.items())],
    }
    (base / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True), encoding="utf-8")
    (base / "domain_catalog.json").write_text(json.dumps(catalog, indent=2), encoding="utf-8")

    print(f"\n== merged dataset row counts ==")
    for t in TABLES:
        print(f"   {t:24s} {row_counts[t]:>10,}")
    if failed:
        print(f"\n!! {len(failed)}/{len(cells)} domains FAILED (recorded in manifest.failed_domains); "
              f"the campaign merged the {len(catalog)} that succeeded.", flush=True)

    if not catalog or row_counts["report_features"] == 0:
        print("\nNo domains produced data -- skipping merged benchmark.", flush=True)
        print(f"\nDONE (with failures). Massive dataset at {base}", flush=True)
        return 1 if failed else 0

    # benchmark the merged dataset (incl. leave-one-domain-out generalization)
    from scms_sim_ref.datagen import benchmark as bmod
    bench = bmod.run(str(base))
    (base / "merged_benchmark.json").write_text(json.dumps(bench, indent=2, default=str), encoding="utf-8")
    print("\n== merged benchmark ==")
    for k, t in bench.get("tasks", {}).items():
        if isinstance(t, dict) and t.get("roc_auc") is not None:
            g = (t.get("gbdt") or {}).get("roc_auc")
            print(f"   {k:30s} logreg={t.get('roc_auc')} gbdt={g} n_test={t.get('n_test')}")
    dg = bench.get("generalization", {}).get("domain_leave_one_out")
    if dg:
        print(f"   domain leave-one-out: mean_auc={dg.get('mean_auc')} n_domains={dg.get('n_domains_evaluated')}")
    nov = bench.get("generalization", {}).get("vehicle_novel_attack")
    if nov:
        print(f"   novel-attack (leave-one-family-out): mean_auc={nov.get('mean_novel_attack_auc')}")
    print(f"\nDONE. Massive dataset at {base}  (ml/*, manifest.json, domain_catalog.json, merged_benchmark.json)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
