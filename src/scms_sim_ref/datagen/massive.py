"""Massive grid campaign: generate ONE big training dataset across every scenario x permutation.

Where `campaign.py` randomly *samples* domains (and drives the Java/MOSAIC stack), this enumerates a
full FACTORIAL grid over the pure-Python generator: every attack scenario (each attack type in
isolation, plus a mixed "ALL" scenario) crossed with every permutation of the environment axes
(weather x rotation x collusion x faults x attacker density x fleet size). Each cell is one
deterministic pipeline run; all cells are featurized and MERGED into a single dataset with a
`domain_id` per row -- ready for cross-domain training and the leave-one-domain-out benchmark.

    python -m scms_sim_ref.datagen.massive --grid full   --out datasets/massive
    python -m scms_sim_ref.datagen.massive --grid medium --out datasets/massive   # tractable middle ground
    python -m scms_sim_ref.datagen.massive --grid quick  --out datasets/massive_quick
    python -m scms_sim_ref.datagen.massive --grid full --flow --dry-run           # preview size, generate nothing

Under --flow, every domain additionally samples a "randomized world" from a per-domain seeded rng
(random.Random(f"{seed}:world:{idx}")): a road topology (grid / ring / spider with varied
dimensions) and, for roughly half the domains, a small scenario-event timeline (demand surges,
weather fronts, attack waves; road closures on grid domains). The sampled parameters are recorded
per domain in domain_catalog.json, so the corpus stays byte-reproducible end to end.

Grids: quick (smoke) < medium (broad, tractable) < full (exhaustive product). Scales to thousands of
cells: each domain is streamed into the merged CSVs and its per-domain directory is deleted immediately
(unless --keep-domains), so disk and memory stay bounded. A failing domain is isolated and recorded in
manifest.failed_domains (never aborts the campaign). Nothing is truncated silently -- if a cap drops
cells, it is logged; --dry-run prints the plan without generating.
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
# corpus_report is the SINGLE SOURCE OF TRUTH for the attack family/type space (it derives
# ALL_FAMILIES / ALL_TYPES / TYPE_TO_FAMILY from run.py's ATTACK_CATALOG+COMBINED_ATTACKS and
# featurize._ATTACK_FAMILY). We reuse those + its build_report() rather than re-deriving anything.
from scms_sim_ref.datagen import corpus_report as corpus_report_mod   # noqa: E402

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
    # a tractable middle ground: broad attack + condition coverage without the full product's blow-up
    "medium": {
        "scenario": ["ConstPos", "ConstPosOffset", "RandomSpeed", "HeadingOffset", "ReversedHeading",
                     "SlowDrift", "AlongRoadOffset", "DoS", "Sybil", "InvalidSignature", "ALL"],
        "weather": ["clear", "rain"],
        "rotate_period_s": [0.0, 60.0],
        "collude_pct": [0.0, 0.5],
        "faulty_pct": [0.05],
        "attacker_pct": [0.2],
        "n_vehicles": [80],
    },
}


def enumerate_cells(grid: dict) -> list[dict]:
    """Full Cartesian product of the grid axes, in a stable order."""
    keys = list(grid)
    cells = []
    for combo in itertools.product(*(grid[k] for k in keys)):
        cells.append(dict(zip(keys, combo)))
    return cells


# "Randomized worlds" (--flow): instead of enumerating topology as a grid axis, every flow domain
# deterministically SAMPLES its world -- road topology + dimensions + a small scenario-event
# timeline -- from a per-domain seeded rng. Same corpus seed -> byte-identical parameters -> a
# byte-identical dataset.
WORLD_TOPOLOGIES = ("grid", "ring", "spider")


def sample_world(base_seed: int, domain_idx: int, duration_s: float) -> dict:
    """Deterministically sample one flow domain's world: topology, dims, and scenario events.

    Every draw comes from a single string-keyed random.Random (no global/os randomness), so
    re-enumerating the corpus reproduces identical parameters. Topology spans grid / ring /
    spider with dims varied per family (ring uses grid_w >= 8 loop intersections; spider is
    4-8 arms x 2-4 rings). Roughly half the domains additionally get a timeline of 0-2 events
    from {demand surge, weather front, attack wave} -- plus road closures, but only on grid
    domains (grid close_edge edges are adjacent-intersection pairs [[i,j],[i2,j2]]). All event
    times fit inside duration_s.
    """
    rng = random.Random(f"{base_seed}:world:{domain_idx}")
    road = rng.choice(WORLD_TOPOLOGIES)
    if road == "grid":
        gw, gh = rng.randint(4, 8), rng.randint(4, 8)
        block = float(rng.choice((100, 120, 140, 160, 180)))
    elif road == "ring":                       # grid_w = number of intersections on the loop
        gw, gh = rng.randint(8, 20), 6
        block = float(rng.choice((100, 140, 180, 220)))
    else:                                      # spider: grid_w arms x grid_h concentric rings
        gw, gh = rng.randint(4, 8), rng.randint(2, 4)
        block = float(rng.choice((100, 130, 160)))
    events: list[dict] = []
    if rng.random() < 0.5:                     # EVENTS axis: ~half the domains get a timeline
        pool = ["demand", "weather", "attack_wave"] + (["close_edge"] if road == "grid" else [])
        for etype in rng.sample(pool, rng.randint(0, 2)):
            t0 = round(rng.uniform(0.10, 0.50) * duration_s, 1)
            until = round(min(float(duration_s), t0 + rng.uniform(0.20, 0.45) * duration_s), 1)
            if etype == "demand":
                events.append({"t": t0, "until": until, "type": "demand",
                               "mult": round(rng.uniform(1.5, 4.0), 2)})
            elif etype == "weather":
                events.append({"t": t0, "type": "weather",
                               "value": rng.choice(("rain", "fog", "snow", "clear"))})
            elif etype == "attack_wave":
                events.append({"t": t0, "until": until, "type": "attack_wave"})
            else:                              # close_edge: an existing adjacent street segment
                if rng.random() < 0.5:         # horizontal
                    i, j = rng.randrange(gw - 1), rng.randrange(gh)
                    edge = [[i, j], [i + 1, j]]
                else:                          # vertical
                    i, j = rng.randrange(gw), rng.randrange(gh - 1)
                    edge = [[i, j], [i, j + 1]]
                events.append({"t": t0, "until": until, "type": "close_edge", "edge": edge})
        events.sort(key=lambda e: (e["t"], e["type"]))
    return {"road": road, "grid_w": gw, "grid_h": gh, "grid_block_m": block, "events": events}


# ==================================================================================================
# OPT-IN dataset-robustness planning (stratified attacks / class balancing / curriculum ordering).
# Everything here is PURE + DETERMINISTIC and produces a per-domain "plan" that main() then runs.
# With every opt-in OFF, build_domain_plan() returns the cells unchanged, so the default corpus (and
# every existing massive test) is byte-identical. The family/type space is imported, never hardcoded.
# ==================================================================================================

# Difficulty proxies for the curriculum (cheap, a-priori, computed BEFORE any pipeline runs):
# stealth = hard (low-and-slow, evades detectors); speed/timing/combined = medium; the "blatant"
# families (position/heading/identity/credential) and the mixed "ALL" scenario = easy.
_HARD_FAMILIES = frozenset({"stealth"})
_MEDIUM_FAMILIES = frozenset({"speed", "timing", "combined"})


def _stratified_schedule() -> list[str]:
    """A families-first ordering of every renderable attack type (single source of truth:
    corpus_report.ALL_TYPES / TYPE_TO_FAMILY / ALL_FAMILIES).

    Round-robins across families, so the FIRST ``len(ALL_FAMILIES)`` entries hit each family exactly
    once (including the opt-in "combined" family) and continuing on covers every individual type. Any
    corpus with at least that many domains therefore covers every attack family; a larger corpus
    additionally covers every type. Deterministic (sorted family order, stable within family).
    """
    by_fam: dict[str, list[str]] = {}
    for t in corpus_report_mod.ALL_TYPES:
        by_fam.setdefault(corpus_report_mod.TYPE_TO_FAMILY[t], []).append(t)
    fams = list(corpus_report_mod.ALL_FAMILIES)            # sorted -> deterministic
    schedule: list[str] = []
    depth = 0
    while len(schedule) < len(corpus_report_mod.ALL_TYPES):
        for f in fams:
            if depth < len(by_fam[f]):
                schedule.append(by_fam[f][depth])
        depth += 1
    return schedule


def _balanced_attacker_pct(base_seed: int, slot: int, target: float,
                           jitter: float = 0.05, lo: float = 0.02, hi: float = 0.6) -> float:
    """Draw one domain's attacker_pct centered on ``target`` (a small symmetric jitter so domains
    still vary), from a per-domain seeded rng (a sibling of the world stream). Because the jitter is
    zero-mean, the MEAN attacker_pct over the corpus -> ``target``, so the merged class balance lands
    near the target instead of whatever the uniform grid axis yields. Deterministic; clamped sane."""
    rng = random.Random(f"{base_seed}:balance:{slot}")
    return round(max(lo, min(hi, target + rng.uniform(-jitter, jitter))), 4)


def _plan_attack_family(entry: dict) -> str | None:
    """The attack family a planned domain will actually produce: the stratified type's family if
    stratifying, else the family of a single-type scenario. "ALL"/mixed/unknown -> None (treated as
    blatant/easy for difficulty)."""
    strat = entry.get("strat_type")
    if strat:
        return corpus_report_mod.TYPE_TO_FAMILY.get(strat)
    scen = entry.get("scenario")
    if scen and scen != "ALL":
        return corpus_report_mod.TYPE_TO_FAMILY.get(scen)
    return None


def _difficulty_score(entry: dict, flow: bool) -> float:
    """Cheap a-priori difficulty proxy (higher = harder), from fields known before running:
    stealth family (+2) > speed/timing/combined (+0.5) > blatant/mixed (+0); FEWER attackers are
    harder (attacker_pct < 0.15 -> +1, < 0.25 -> +0.5); under --flow, subtle (intensity < 1) and
    pulsed/evasive (duty < 1) attackers each add +1."""
    fam = _plan_attack_family(entry)
    s = 0.0
    if fam in _HARD_FAMILIES:
        s += 2.0
    elif fam in _MEDIUM_FAMILIES:
        s += 0.5
    ap = float(entry.get("attacker_pct", 0.0) or 0.0)
    if ap < 0.15:
        s += 1.0
    elif ap < 0.25:
        s += 0.5
    if flow:
        if float(entry.get("intensity", 1.0) or 1.0) < 1.0:
            s += 1.0
        if float(entry.get("duty", 1.0) or 1.0) < 1.0:
            s += 1.0
    return s


def _difficulty_tier(score: float) -> int:
    """Bin a difficulty score into a tier 0..3 (0 = easiest, 3 = hardest)."""
    if score <= 0.0:
        return 0
    if score <= 1.0:
        return 1
    if score <= 2.0:
        return 2
    return 3


def build_domain_plan(cells: list[dict], base_seed: int, *, flow: bool = False,
                      stratify: bool = False, balance_target: float | None = None,
                      curriculum: bool = False) -> list[dict]:
    """Turn enumerated grid cells into the per-domain PLAN, applying the opt-in robustness controls.

    Pure + deterministic (runs no pipeline). Each returned entry is a copy of a cell, optionally
    augmented with:
      * ``strat_type`` / ``strat_family`` -- when ``stratify`` is on (attack forced per domain so
        every family, then every type, is covered; source of truth = corpus_report's type space);
      * a biased ``attacker_pct`` -- when ``balance_target`` is set (draws centered on the target);
      * ``difficulty_tier`` (0..3), with the plan REORDERED easy->hard -- when ``curriculum`` is on.

    With all three off it returns ``[dict(cell) for cell in cells]`` -- identical values, identical
    order, no extra keys -- so the default corpus is unchanged.
    """
    schedule = _stratified_schedule() if stratify else None
    entries: list[dict] = []
    for slot, cell in enumerate(cells):
        e = dict(cell)
        if schedule is not None:
            t = schedule[slot % len(schedule)]
            e["strat_type"] = t
            e["strat_family"] = corpus_report_mod.TYPE_TO_FAMILY[t]
        if balance_target is not None:
            e["attacker_pct"] = _balanced_attacker_pct(base_seed, slot, balance_target)
        entries.append(e)
    if curriculum:
        # order easy->hard (stable: ties keep enumeration order) and stamp each domain's tier
        scored = sorted(((_difficulty_score(e, flow), slot, e)
                         for slot, e in enumerate(entries)), key=lambda x: (x[0], x[1]))
        for score, _slot, e in scored:
            e["difficulty_tier"] = _difficulty_tier(score)
        entries = [e for _score, _slot, e in scored]
    return entries


def cell_config(cell: dict, idx: int, base_seed: int, n_steps: int, out_dir: Path,
                flow: bool = False, flow_duration: float = 0.0,
                world: dict | None = None) -> PipelineConfig:
    scen = cell["scenario"]
    strat = cell.get("strat_type")                       # --stratify-attacks: forced per-domain type
    if strat:
        attack_types = (strat,)                          # 1-tuple != default -> pipeline uses it as-is
    else:
        attack_types = tuple(ATTACK_TYPES) if scen == "ALL" else (scen,)
    kw = dict(
        seed=(base_seed + idx * 100003) % 2_000_000_000,
        n_vehicles=cell["n_vehicles"], n_steps=n_steps,
        attacker_pct=cell["attacker_pct"], attack_types=attack_types,
        # "ALL" must keep the FULL round-robin catalog: attack_type MUST be the sentinel "" here,
        # else the F2 narrowing (attack_type truthy AND attack_types==default) collapses these rich
        # mixed-family cells to a single type. Single-scenario cells set attack_types=(scen,) which is
        # already non-default, so attack_type is ignored there.
        attack_type=(strat if strat else (scen if scen != "ALL" else "")),
        faulty_pct=cell["faulty_pct"], weather=cell["weather"],
        rotate_period_s=cell["rotate_period_s"],
        collude_pct=cell["collude_pct"], victim_pct=0.12,
        out_dir=str(out_dir))
    if flow:
        # each domain is a long routed simulation with car-following + a demand profile, and
        # (as permutation axes) topology, signals, fleet, attack difficulty, RSUs. When a sampled
        # `world` is supplied (randomized-worlds corpus), it decides topology/dims/events instead
        # of the legacy fixed grid/ring cell axis.
        w = world or {}
        road = w.get("road", cell.get("road", "grid"))
        gw = w.get("grid_w", 16 if road == "ring" else 6)  # ring: grid_w = loop intersections
        gh = w.get("grid_h", 6)
        block = w.get("grid_block_m", 140.0)
        events = w.get("events") or []
        kw.update(traffic_flow=True, road_network=road, car_following=True,
                  duration_s=flow_duration, arrival_rate=2.0, grid_w=gw, grid_h=gh, n_lanes=2,
                  grid_block_m=block, events=(json.dumps(events) if events else ""),
                  demand_profile=cell.get("demand", "uniform"),
                  traffic_lights=bool(cell.get("lights", False)), fleet=cell.get("fleet", "mixed"),
                  attack_intensity=cell.get("intensity", 1.0),
                  attack_duty_cycle=cell.get("duty", 1.0),        # pulsed-attack difficulty axis
                  od_model=cell.get("od", "uniform"),             # trip-length realism axis
                  n_rsus=int(cell.get("rsus", 0)))                # infrastructure-assisted axis
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
    ap.add_argument("--dry-run", action="store_true", help="print the plan (cell count + axes) and exit without generating")
    ap.add_argument("--keep-domains", action="store_true", help="keep each per-domain dir (default: delete after merge)")
    ap.add_argument("--parquet", action="store_true", help="also write merged parquet (memory-heavy at scale)")
    ap.add_argument("--flow", action="store_true", help="each domain is a long routed flow simulation")
    ap.add_argument("--flow-duration", type=float, default=300.0, help="flow: seconds per domain")
    # --- OPT-IN dataset-robustness controls (all default OFF -> default corpus is byte-identical) ---
    ap.add_argument("--stratify-attacks", action="store_true",
                    help="deterministically force one attack type per domain so EVERY family (then "
                         "every type) is covered, incl. the opt-in 'combined' family (default OFF)")
    ap.add_argument("--balance-classes", type=float, default=None, metavar="TARGET",
                    help="bias per-domain attacker_pct draws (seeded) so the merged attacker fraction "
                         "lands near TARGET instead of the uniform grid value (default OFF)")
    ap.add_argument("--curriculum", action="store_true",
                    help="order domains easy->hard by cheap a-priori proxies and record a per-domain "
                         "difficulty_tier (0..3) in domain_catalog.json (default OFF)")
    ap.add_argument("--report", action=argparse.BooleanOptionalAction, default=True,
                    help="after generation, write CORPUS_REPORT.md (balance/coverage) next to the "
                         "manifest and print its warnings summary (read-only; default ON, "
                         "--no-report disables)")
    ap.add_argument("--realism", action="store_true",
                    help="score each domain with datagen.realism_bench before its directory is "
                         "deleted and record the scorecard summary in domain_catalog.json "
                         "(read-only, deterministic; default OFF because it re-reads every "
                         "domain's ground truth)")
    ap.add_argument("--out", default=str(REPO / "datasets" / "massive"))
    a = ap.parse_args(argv)

    grid = dict(GRIDS[a.grid])
    if a.flow:
        # under flow, demand / signals / fleet / attack difficulty become permutation axes; n_vehicles
        # is irrelevant. "duty" spans continuous vs pulsed (evasive) attackers -> a difficulty axis.
        # Topology + scenario events are NOT grid axes: each domain samples its own "world"
        # (road_network/dims/events) from a per-domain seeded rng -- see sample_world().
        grid = {**grid, "demand": ["uniform", "rush", "night"], "lights": [False, True],
                "fleet": ["mixed", "car"], "intensity": [1.0, 0.5], "duty": [1.0, 0.4],
                "rsus": [0, 8],                             # RSU (infrastructure) coverage axis
                "n_vehicles": [0]}
    cells = enumerate_cells(grid)
    total = len(cells)
    dropped = 0
    if a.max_domains and total > a.max_domains:
        if a.sample:
            cells = random.Random(a.seed).sample(cells, a.max_domains)
        else:
            cells = cells[:a.max_domains]
        dropped = total - len(cells)

    # OPT-IN robustness planning: stratified attacks / class balancing / curriculum ordering.
    # With all three off this is `[dict(c) for c in cells]` -- identical values + order -> unchanged.
    plan = build_domain_plan(cells, a.seed, flow=a.flow, stratify=a.stratify_attacks,
                             balance_target=a.balance_classes, curriculum=a.curriculum)

    print(f"== massive grid '{a.grid}': {total} cells in the product, running {len(plan)}"
          + (f" (CAP dropped {dropped})" if dropped else "") + f", seed {a.seed} ==", flush=True)
    print(f"   axes: " + ", ".join(f"{k}({len(v)})" for k, v in grid.items()), flush=True)
    if a.stratify_attacks:
        fams = sorted({e["strat_family"] for e in plan})
        print(f"   stratify-attacks ON: forcing per-domain types over {len(corpus_report_mod.ALL_TYPES)} "
              f"types / {len(corpus_report_mod.ALL_FAMILIES)} families; plan covers families: "
              f"{', '.join(fams)}", flush=True)
    if a.balance_classes is not None:
        print(f"   balance-classes ON: per-domain attacker_pct centered on target "
              f"{a.balance_classes} (mean planned = "
              f"{sum(e['attacker_pct'] for e in plan) / len(plan):.4f})", flush=True)
    if a.curriculum:
        tiers = sorted({e.get("difficulty_tier") for e in plan})
        print(f"   curriculum ON: domains ordered easy->hard; difficulty tiers present: {tiers}",
              flush=True)
    if a.flow:
        print(f"   worlds: road_network sampled per domain from {'/'.join(WORLD_TOPOLOGIES)} "
              f"(+ dims); ~half get 0-2 scenario events (seeded, byte-reproducible)", flush=True)
    if a.dry_run:                                    # preview the plan without generating anything
        print(f"   [dry-run] would generate {len(cells)} domain(s) into {a.out}; no output written.",
              flush=True)
        return 0

    base = Path(a.out)
    if base.exists():
        shutil.rmtree(base)
    (base / "ml").mkdir(parents=True)
    (base / "domains").mkdir(parents=True)

    header_written: set = set()
    catalog, row_counts, failed = [], {t: 0 for t in TABLES}, []
    for idx, entry in enumerate(plan):
        dom_dir = base / "domains" / f"d{idx:04d}"
        # flow: each domain gets a deterministically sampled world (topology + dims + events);
        # its parameters ride along into the catalog / failed record as per-domain provenance.
        world = sample_world(a.seed, idx, a.flow_duration) if a.flow else None
        wrec = ({"road": world["road"], "road_network": world["road"],
                 "grid_w": world["grid_w"], "grid_h": world["grid_h"],
                 "grid_block_m": world["grid_block_m"], "events": world["events"]}
                if world else {})
        # Isolate each cell: one bad domain (e.g. a degenerate config) must not throw away the
        # thousands of good ones already merged. Failures are RECORDED (not silently dropped).
        try:
            cfg = cell_config(entry, idx, a.seed, a.steps, dom_dir, flow=a.flow,
                              flow_duration=a.flow_duration, world=world)
            res = run_pipeline(cfg)
            featmod.build(str(dom_dir), split_seed=1234)
            for tbl in TABLES:
                csv = dom_dir / "ml" / f"{tbl}.csv"
                if csv.exists():
                    df = pd.read_csv(csv)
                    row_counts[tbl] += _append(df, idx, base / "ml" / f"{tbl}.csv", header_written)
            # per-domain difficulty labels (from its own ground truth, before the dir is deleted):
            # lets a trainer curriculum-weight or stratify the merged corpus by how hard each domain is.
            # --realism additionally scores the domain's TRAFFIC/COMM realism here, i.e. while the
            # domain dir still exists (it is deleted in the finally: below).
            vs = valmod.validate(str(dom_dir), realism=a.realism)[0]
            rec = {"domain_id": idx, **entry, **wrec, "seed": cfg.seed,
                   "reports": res.n_reports, "revoked": res.n_revoked,
                   "precision": vs.get("precision"), "recall": vs.get("recall"),
                   "recall_by_family": vs.get("recall_by_family", {}),
                   "recall_by_type": vs.get("recall_by_type", {})}
            if a.realism:
                rec["realism"] = vs.get("realism", {})
            catalog.append(rec)
        except Exception as e:                       # noqa: BLE001 -- keep the campaign alive
            failed.append({"domain_id": idx, **entry, **wrec, "error": f"{type(e).__name__}: {e}"})
            print(f"   [{idx + 1}/{len(plan)}] FAILED domain {idx}: {type(e).__name__}: {e}", flush=True)
        finally:
            if not a.keep_domains:
                shutil.rmtree(dom_dir, ignore_errors=True)
        if (idx + 1) % 25 == 0 or idx + 1 == len(plan):
            print(f"   [{idx + 1}/{len(plan)}] merged; report rows so far={row_counts['report_features']}"
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
    if a.flow:
        # randomized-worlds provenance: per-domain road_network/dims/events live in
        # domain_catalog.json; this records HOW they were sampled (reproducibility contract).
        manifest["world_sampling"] = {
            "topologies": list(WORLD_TOPOLOGIES),
            "events": "~half of domains get 0-2 events (demand/weather/attack_wave; "
                      "close_edge on grid domains only), times within flow_duration",
            "rng": "random.Random(f'{seed}:world:{domain_idx}')",
        }
    # OPT-IN robustness provenance: recorded ONLY when a control is active, so the DEFAULT manifest
    # is byte-identical to before this feature (existing digests/tests unaffected).
    if a.stratify_attacks or a.balance_classes is not None or a.curriculum:
        manifest["dataset_robustness"] = {
            "stratify_attacks": bool(a.stratify_attacks),
            "balance_classes_target": a.balance_classes,
            "curriculum": bool(a.curriculum),
            "families_covered": sorted({e["strat_family"] for e in plan
                                        if "strat_family" in e}) or None,
            "difficulty_tiers": sorted({e["difficulty_tier"] for e in plan
                                        if "difficulty_tier" in e}) or None,
        }
    (base / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True), encoding="utf-8")
    (base / "domain_catalog.json").write_text(json.dumps(catalog, indent=2), encoding="utf-8")

    # ADDITIVE, read-only balance/coverage report (default ON; --no-report disables). Runs on the
    # merged corpus AFTER manifest + catalog are written so build_report() sees full provenance; it
    # never mutates the corpus. Guarded so reporting can never abort a large campaign.
    if a.report:
        try:
            rep = corpus_report_mod.build_report(str(base))
            (base / "CORPUS_REPORT.md").write_text(
                corpus_report_mod.render_markdown(rep), encoding="utf-8", newline="\n")
            warns = rep.get("warnings", [])
            print(f"\n== corpus report ({'no warnings -- well-balanced' if not warns else str(len(warns)) + ' warning(s)'}) "
                  f"-> {base / 'CORPUS_REPORT.md'} ==", flush=True)
            for w in warns:
                print(f"   !! {w}", flush=True)
        except Exception as e:                       # noqa: BLE001 -- reporting is additive, never fatal
            print(f"   [report] skipped: {type(e).__name__}: {e}", flush=True)

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
    print(f"\nDONE. Massive dataset at {base}  (ml/*, manifest.json, domain_catalog.json, "
          f"merged_benchmark.json" + (", CORPUS_REPORT.md" if a.report else "") + ")")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
