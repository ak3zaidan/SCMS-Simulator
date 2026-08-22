"""Assess a merged multi-domain corpus for BALANCE and COVERAGE.

`scms_sim_ref.datagen.massive` merges many pipeline runs into ONE corpus directory: leakage-safe
``ml/*.csv`` tables (each row tagged with a ``domain_id``), a ``domain_catalog.json`` with per-domain
provenance (road_network / events / difficulty), and a ``manifest.json``. This module answers the
question massive itself never asks: *is that corpus a good training set?* -- i.e. is it class-balanced,
does it cover the whole attack family/type space, does it span multiple road topologies and an
easy->hard difficulty range, and is any single domain dominating the row budget?

The analysis is READ-ONLY (it never writes into the corpus) and DETERMINISTIC (no timestamps / no
randomness -- the same corpus always yields byte-identical output). Optional files
(``domain_catalog.json`` / ``manifest.json``) are handled gracefully: sections that need them degrade
to "unavailable" rather than crashing.

    python -m scms_sim_ref.datagen.corpus_report --corpus datasets/massive
    python -m scms_sim_ref.datagen.corpus_report --corpus datasets/massive --out report.md

Programmatic use::

    from scms_sim_ref.datagen.corpus_report import build_report, render_markdown
    rep = build_report("datasets/massive")     # -> dict
    print(render_markdown(rep))                 # -> Markdown string
    rep["warnings"]                             # [] means well-balanced

The attack family/type SPACE is imported from the code (single source of truth), never hardcoded:
``ATTACK_CATALOG`` + ``COMBINED_ATTACKS`` from the engine give the ~25 renderable types, and
``featurize._ATTACK_FAMILY`` maps them to the family set (7 base families + the opt-in "combined").
"""
from __future__ import annotations

import argparse
import json
import os
from typing import Any

import numpy as np
import pandas as pd

# --- SINGLE SOURCE OF TRUTH for the attack family/type space (imported, never hardcoded) ---
# The engine renders exactly ATTACK_CATALOG (the default round-robin) + COMBINED_ATTACKS (opt-in);
# featurize._ATTACK_FAMILY maps each base type to its family. We measure coverage against THESE, so
# the report tracks the code automatically -- add a type/family in run.py and it shows up as a gap.
from scms_sim_ref.mock_pipeline.run import ATTACK_CATALOG, COMBINED_ATTACKS
from scms_sim_ref.datagen.featurize import _ATTACK_FAMILY

ALL_TYPES: tuple[str, ...] = tuple(ATTACK_CATALOG) + tuple(COMBINED_ATTACKS)   # ~25 renderable types
TYPE_TO_FAMILY: dict[str, str] = {t: _ATTACK_FAMILY.get(t, "other") for t in ALL_TYPES}
ALL_FAMILIES: tuple[str, ...] = tuple(sorted({TYPE_TO_FAMILY[t] for t in ALL_TYPES}))
# Families reachable only via the opt-in COMBINED_ATTACKS (i.e. NOT produced by the default catalog):
# by construction this is the {"combined"} family. Derived, so it never drifts from the code.
_CATALOG_FAMILIES = {_ATTACK_FAMILY.get(t, "other") for t in ATTACK_CATALOG}
_COMBINED_FAMILIES = {_ATTACK_FAMILY.get(t, "other") for t in COMBINED_ATTACKS}
OPT_IN_FAMILIES: tuple[str, ...] = tuple(sorted(_COMBINED_FAMILIES - _CATALOG_FAMILIES))
BASE_FAMILIES: tuple[str, ...] = tuple(sorted(_CATALOG_FAMILIES))

# --- balance/coverage thresholds (module-level so they are documented + testable) ---
MIN_CLASS_FRAC = 0.02              # a class holding <2% of vehicles is severely underrepresented
ATTACKER_BENIGN_BAND = (0.05, 1.0)  # sane attacker:benign ratio (below -> too few positives; above -> attackers are the majority, unrealistic for V2X)
DIFFICULTY_SPREAD_MIN = 0.15       # max-min per-domain recall below this = corpus doesn't span easy->hard
DOMAIN_DOMINANCE_FACTOR = 4.0      # a domain with > this x the median row count dominates the corpus

ML_TABLES = ("report_features", "report_labels", "subject_features", "subject_labels",
             "vehicle_features", "vehicle_labels", "vehicle_features_ma", "vehicle_labels_ma",
             "graph_edges", "subject_windows")


# --------------------------------------------------------------------------------------------------
# small IO helpers (all read-only; missing/empty files degrade to None / empty frame)
# --------------------------------------------------------------------------------------------------
def _read_csv(path: str, usecols: list[str] | None = None) -> pd.DataFrame | None:
    if not os.path.exists(path):
        return None
    try:
        return pd.read_csv(path, usecols=usecols)
    except (pd.errors.EmptyDataError, ValueError):
        # empty file, or requested usecols absent -> retry without usecols, else treat as empty
        if usecols is not None:
            try:
                return pd.read_csv(path)
            except pd.errors.EmptyDataError:
                return pd.DataFrame()
        return pd.DataFrame()


def _read_json(path: str) -> Any | None:
    if not os.path.exists(path):
        return None
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except (json.JSONDecodeError, OSError):
        return None


def _count_data_rows(path: str) -> int:
    """Count data rows (excluding the header) of a csv without loading it into memory."""
    if not os.path.exists(path):
        return 0
    with open(path, "rb") as fh:
        n = sum(1 for _ in fh)
    return max(0, n - 1)


def _stats(values: list[float]) -> dict[str, float] | None:
    if not values:
        return None
    arr = np.asarray(values, dtype=float)
    return {"min": round(float(arr.min()), 4),
            "median": round(float(np.median(arr)), 4),
            "max": round(float(arr.max()), 4)}


# --------------------------------------------------------------------------------------------------
# section analyzers -- each returns (section_dict, warnings_list)
# --------------------------------------------------------------------------------------------------
def _analyze_class_balance(veh: pd.DataFrame | None,
                           rep_labels: pd.DataFrame | None) -> tuple[dict, list[str]]:
    """Benign vs attacker vs faulty at the vehicle level (three-way partition) and attacker vs
    non-attacker at the report level; flag any severely-underrepresented class or a lopsided ratio."""
    warnings: list[str] = []
    out: dict[str, Any] = {"vehicle": None, "report": None}

    if veh is not None and len(veh) and "label_is_attacker" in veh.columns:
        atk = veh["label_is_attacker"].fillna(0).astype(int) == 1
        flt = (veh.get("label_is_faulty", 0)
               if "label_is_faulty" in veh.columns else pd.Series(0, index=veh.index))
        faulty = (~atk) & (flt.fillna(0).astype(int) == 1)   # faulty-but-not-attacker
        benign = (~atk) & (~faulty)
        total = int(len(veh))
        n_atk, n_flt, n_ben = int(atk.sum()), int(faulty.sum()), int(benign.sum())
        vb = {
            "total": total,
            "benign": n_ben, "attacker": n_atk, "faulty": n_flt,
            "benign_frac": round(n_ben / total, 4),
            "attacker_frac": round(n_atk / total, 4),
            "faulty_frac": round(n_flt / total, 4),
            "attacker_benign_ratio": (round(n_atk / n_ben, 4) if n_ben else None),
        }
        out["vehicle"] = vb
        for cls, n in (("benign", n_ben), ("attacker", n_atk), ("faulty", n_flt)):
            frac = n / total
            if n == 0:
                warnings.append(f"Class '{cls}' is entirely ABSENT at the vehicle level (0 vehicles).")
            elif frac < MIN_CLASS_FRAC:
                warnings.append(f"Class '{cls}' is severely underrepresented: {n}/{total} vehicles "
                                f"({frac:.2%} < {MIN_CLASS_FRAC:.0%}).")
        if n_ben:
            ratio = n_atk / n_ben
            lo, hi = ATTACKER_BENIGN_BAND
            if ratio < lo:
                warnings.append(f"Attacker:benign ratio {ratio:.3f} is below the sane band "
                                f"[{lo}, {hi}] -- too few attacker examples to train on.")
            elif ratio > hi:
                warnings.append(f"Attacker:benign ratio {ratio:.3f} is above the sane band "
                                f"[{lo}, {hi}] -- attackers outnumber benign, unrealistic for V2X.")
    else:
        warnings.append("Class balance unavailable: ml/vehicle_labels.csv is missing or empty.")

    if rep_labels is not None and len(rep_labels) and "label_subject_is_attacker" in rep_labels.columns:
        r_atk = int((rep_labels["label_subject_is_attacker"].fillna(0).astype(int) == 1).sum())
        r_tot = int(len(rep_labels))
        out["report"] = {
            "total": r_tot,
            "attacker": r_atk, "non_attacker": r_tot - r_atk,
            "attacker_frac": round(r_atk / r_tot, 4) if r_tot else 0.0,
            "note": "report level distinguishes attacker vs non-attacker only (no faulty label).",
        }
        if r_tot and (r_atk / r_tot) < MIN_CLASS_FRAC:
            warnings.append(f"Report-level attacker signal is very sparse: {r_atk}/{r_tot} reports "
                            f"target an attacker ({r_atk / r_tot:.2%}).")

    return out, warnings


def _analyze_attack_coverage(veh: pd.DataFrame | None,
                             catalog: list | None) -> tuple[dict, list[str]]:
    """Which of the full family set + ~25 types are PRESENT vs ABSENT.

    Families come from vehicle_labels.attack_family (an ML-table column). Per-TYPE labels are NOT in
    the ml tables, so type coverage is measured from domain_catalog.json (recall_by_type keys, which
    are derived from ground-truth attackers, plus each domain's configured scenario). Absent
    domain_catalog -> type coverage degrades to 'unavailable'.
    """
    warnings: list[str] = []

    # -- families: measured from the ml tables --
    present_fams: set[str] = set()
    if veh is not None and len(veh) and "attack_family" in veh.columns:
        present_fams = {str(f) for f in veh["attack_family"].dropna().unique()} & set(ALL_FAMILIES)
    absent_fams = [f for f in ALL_FAMILIES if f not in present_fams]
    families = {
        "all": list(ALL_FAMILIES), "present": sorted(present_fams), "absent": absent_fams,
        "n_present": len(present_fams), "n_total": len(ALL_FAMILIES),
        "base_families": list(BASE_FAMILIES), "opt_in_families": list(OPT_IN_FAMILIES),
    }
    if veh is None or "attack_family" not in (veh.columns if veh is not None else []):
        warnings.append("Attack-family coverage unavailable: ml/vehicle_labels.csv missing its "
                        "attack_family column.")
    elif absent_fams:
        warnings.append(f"Attack families ABSENT ({len(absent_fams)}/{len(ALL_FAMILIES)}): "
                        f"{', '.join(absent_fams)}.")
    for opt in OPT_IN_FAMILIES:
        if opt in absent_fams:
            warnings.append(f"The opt-in '{opt}' attack family (multi-field falsification) is absent; "
                            f"pass COMBINED_ATTACKS via attack_types to cover it.")

    # -- types: measured from domain_catalog (recall_by_type keys + configured scenarios) --
    types: dict[str, Any]
    if catalog:
        present_types: set[str] = set()
        for d in catalog:
            for t in (d.get("recall_by_type") or {}):
                base = str(t).split("/")[0]
                if base in TYPE_TO_FAMILY:
                    present_types.add(base)
            scen = str(d.get("scenario", ""))
            if scen in TYPE_TO_FAMILY:            # a domain configured to a single specific type
                present_types.add(scen)
        absent_types = [t for t in ALL_TYPES if t not in present_types]
        types = {
            "available": True, "source": "domain_catalog.json (recall_by_type + scenario)",
            "all": list(ALL_TYPES), "present": [t for t in ALL_TYPES if t in present_types],
            "absent": absent_types, "n_present": len(present_types), "n_total": len(ALL_TYPES),
        }
        if absent_types:
            shown = ", ".join(absent_types[:12]) + (", ..." if len(absent_types) > 12 else "")
            warnings.append(f"Attack types ABSENT ({len(absent_types)}/{len(ALL_TYPES)}): {shown}")
    else:
        types = {"available": False, "n_total": len(ALL_TYPES), "all": list(ALL_TYPES),
                 "reason": "no domain_catalog.json; the ml tables carry attack_family, not attack_type."}
        warnings.append("Attack-type coverage unavailable: no domain_catalog.json (ml tables carry "
                        "family, not type).")

    return {"families": families, "types": types}, warnings


def _analyze_domain_diversity(catalog: list | None) -> tuple[dict, list[str]]:
    """Distribution of domains over road-network topologies and scenario-event presence."""
    warnings: list[str] = []
    if not catalog:
        return ({"available": False,
                 "reason": "no domain_catalog.json"}, [])   # silent: covered by size/other sections

    topo: dict[str, int] = {}
    event_domains = 0
    for d in catalog:
        net = d.get("road_network") or d.get("road")
        if net is not None:
            topo[str(net)] = topo.get(str(net), 0) + 1
        if d.get("events"):
            event_domains += 1
    out = {
        "available": True, "n_domains": len(catalog),
        "topologies": dict(sorted(topo.items())), "n_topologies": len(topo),
        "event_domains": event_domains, "no_event_domains": len(catalog) - event_domains,
    }
    if topo and len(topo) == 1:
        warnings.append(f"Only one road-network topology is represented "
                        f"({next(iter(topo))}); the corpus lacks topological diversity.")
    if catalog and event_domains == 0:
        warnings.append("No domain carries a scenario-event timeline (demand/weather/attack waves); "
                        "the corpus covers static conditions only.")
    return out, warnings


def _analyze_difficulty_spread(catalog: list | None) -> tuple[dict, list[str]]:
    """Per-domain recall spread (does the corpus span easy->hard?) + hardest families."""
    warnings: list[str] = []
    if not catalog:
        return ({"available": False, "reason": "no domain_catalog.json"}, [])

    recalls = [float(d["recall"]) for d in catalog if d.get("recall") is not None]
    precisions = [float(d["precision"]) for d in catalog if d.get("precision") is not None]
    if not recalls:
        return ({"available": False, "reason": "domain_catalog carries no per-domain recall"}, [])

    fam_recalls: dict[str, list[float]] = {}
    for d in catalog:
        for fam, r in (d.get("recall_by_family") or {}).items():
            if r is not None:
                fam_recalls.setdefault(str(fam), []).append(float(r))
    by_family = {f: {"mean": round(float(np.mean(v)), 4),
                     "min": round(float(np.min(v)), 4),
                     "max": round(float(np.max(v)), 4),
                     "n_domains": len(v)}
                 for f, v in sorted(fam_recalls.items())}
    hardest = sorted(by_family.items(), key=lambda kv: kv[1]["mean"])[:3]

    rec_stats = _stats(recalls)
    spread = round(rec_stats["max"] - rec_stats["min"], 4)
    out = {
        "available": True, "n_domains_scored": len(recalls),
        "recall": {**rec_stats, "spread": spread},
        "precision": _stats(precisions),
        "recall_by_family": by_family,
        "hardest_families": [{"family": f, **s} for f, s in hardest],
    }
    if spread < DIFFICULTY_SPREAD_MIN:
        warnings.append(f"Narrow difficulty spread: per-domain recall ranges only "
                        f"{rec_stats['min']}..{rec_stats['max']} (spread {spread} < "
                        f"{DIFFICULTY_SPREAD_MIN}); the corpus may not span easy->hard.")
    return out, warnings


def _analyze_size(corpus_dir: str, veh: pd.DataFrame | None,
                  catalog: list | None, manifest: dict | None) -> tuple[dict, list[str]]:
    """Rows per ml table, #domains, and per-domain row spread (to spot a dominating domain)."""
    warnings: list[str] = []
    ml = os.path.join(corpus_dir, "ml")

    # rows per table: trust the manifest's exact counts if present, else count file lines
    row_counts: dict[str, int] = {}
    man_counts = (manifest or {}).get("row_counts") or {}
    for tbl in ML_TABLES:
        p = os.path.join(ml, f"{tbl}.csv")
        if os.path.exists(p):
            row_counts[tbl] = int(man_counts.get(tbl, _count_data_rows(p)))

    # per-domain volume: prefer the largest per-report table, then labels, then vehicles
    per_domain: dict[int, int] = {}
    domain_source = None
    for tbl in ("report_features", "report_labels", "vehicle_labels"):
        p = os.path.join(ml, f"{tbl}.csv")
        df = _read_csv(p, usecols=["domain_id"])
        if df is not None and len(df) and "domain_id" in df.columns:
            per_domain = df["domain_id"].value_counts().to_dict()
            domain_source = tbl
            break

    n_domains = None
    if per_domain:
        n_domains = len(per_domain)
    elif catalog:
        n_domains = len(catalog)
    elif manifest and manifest.get("n_domains_ok") is not None:
        n_domains = int(manifest["n_domains_ok"])

    out: dict[str, Any] = {
        "row_counts": row_counts,
        "n_domains": n_domains,
        "per_domain_rows": None,
        "per_domain_source": domain_source,
    }
    if manifest is not None:
        out["manifest"] = {"grid": manifest.get("grid"), "seed": manifest.get("seed"),
                           "n_domains_ok": manifest.get("n_domains_ok"),
                           "n_domains_failed": manifest.get("n_domains_failed")}
        if manifest.get("n_domains_failed"):
            warnings.append(f"{manifest['n_domains_failed']} domain(s) FAILED during generation "
                            f"(see manifest.failed_domains); the corpus merged only the successes.")

    if per_domain:
        counts = [int(v) for v in per_domain.values()]
        pd_stats = _stats([float(c) for c in counts])
        out["per_domain_rows"] = {**pd_stats, "table": domain_source, "n_domains": len(counts)}
        med = pd_stats["median"]
        if med and (pd_stats["max"] / med) > DOMAIN_DOMINANCE_FACTOR:
            dom_id = max(per_domain, key=per_domain.get)
            share = pd_stats["max"] / sum(counts)
            warnings.append(f"Domain {dom_id} dominates the corpus: {int(pd_stats['max'])} rows "
                            f"({share:.1%} of {domain_source}) vs a median of {int(med)} "
                            f"(> {DOMAIN_DOMINANCE_FACTOR}x).")
    if not row_counts:
        warnings.append("No ml/*.csv tables found under the corpus directory.")

    return out, warnings


# --------------------------------------------------------------------------------------------------
# public API
# --------------------------------------------------------------------------------------------------
def build_report(corpus_dir: str) -> dict:
    """Analyze a corpus directory and return a balance/coverage report as a dict.

    Read-only and deterministic. Missing optional files (domain_catalog.json / manifest.json) degrade
    gracefully. ``report["warnings"]`` aggregates every section's warnings; an empty list means the
    corpus is well-balanced and well-covered by these checks.
    """
    corpus_dir = os.fspath(corpus_dir)
    ml = os.path.join(corpus_dir, "ml")
    veh = _read_csv(os.path.join(ml, "vehicle_labels.csv"))
    rep_labels = _read_csv(os.path.join(ml, "report_labels.csv"),
                           usecols=["domain_id", "label_subject_is_attacker"])
    catalog = _read_json(os.path.join(corpus_dir, "domain_catalog.json"))
    if not isinstance(catalog, list):
        catalog = None
    manifest = _read_json(os.path.join(corpus_dir, "manifest.json"))
    if not isinstance(manifest, dict):
        manifest = None

    class_balance, w_cb = _analyze_class_balance(veh, rep_labels)
    attack_coverage, w_ac = _analyze_attack_coverage(veh, catalog)
    domain_diversity, w_dd = _analyze_domain_diversity(catalog)
    difficulty_spread, w_ds = _analyze_difficulty_spread(catalog)
    size, w_sz = _analyze_size(corpus_dir, veh, catalog, manifest)

    warnings_by_section = {
        "class_balance": w_cb, "attack_coverage": w_ac, "domain_diversity": w_dd,
        "difficulty_spread": w_ds, "size": w_sz,
    }
    warnings = [w for sect in ("class_balance", "attack_coverage", "domain_diversity",
                               "difficulty_spread", "size") for w in warnings_by_section[sect]]

    return {
        "corpus_dir": corpus_dir,
        "has_domain_catalog": catalog is not None,
        "has_manifest": manifest is not None,
        "class_balance": class_balance,
        "attack_coverage": attack_coverage,
        "domain_diversity": domain_diversity,
        "difficulty_spread": difficulty_spread,
        "size": size,
        "warnings": warnings,
        "warnings_by_section": warnings_by_section,
    }


# --------------------------------------------------------------------------------------------------
# markdown rendering
# --------------------------------------------------------------------------------------------------
def _fmt_pct(x: float | None) -> str:
    return "-" if x is None else f"{x:.1%}"


def render_markdown(report: dict) -> str:
    """Render a build_report() dict as human-readable Markdown."""
    L: list[str] = []
    L.append("# Corpus balance & coverage report")
    L.append("")
    L.append(f"- **Corpus:** `{report['corpus_dir']}`")
    L.append(f"- **domain_catalog.json:** {'present' if report['has_domain_catalog'] else 'ABSENT'}"
             f"  |  **manifest.json:** {'present' if report['has_manifest'] else 'ABSENT'}")
    n_warn = len(report["warnings"])
    L.append(f"- **Verdict:** {'WELL-BALANCED (no warnings)' if n_warn == 0 else f'{n_warn} warning(s) -- see below'}")
    L.append("")

    # -- class balance --
    L.append("## Class balance")
    cb = report["class_balance"]
    vb = cb.get("vehicle")
    if vb:
        L.append(f"Vehicle level ({vb['total']} vehicles):")
        L.append("")
        L.append("| class | count | fraction |")
        L.append("| --- | ---: | ---: |")
        for cls in ("benign", "attacker", "faulty"):
            L.append(f"| {cls} | {vb[cls]} | {_fmt_pct(vb[f'{cls}_frac'])} |")
        L.append("")
        L.append(f"Attacker:benign ratio = "
                 f"{vb['attacker_benign_ratio'] if vb['attacker_benign_ratio'] is not None else '-'} "
                 f"(sane band {ATTACKER_BENIGN_BAND}).")
    else:
        L.append("_Vehicle-level class balance unavailable (vehicle_labels.csv missing)._")
    rb = cb.get("report")
    if rb:
        L.append("")
        L.append(f"Report level ({rb['total']} reports): {rb['attacker']} target an attacker "
                 f"({_fmt_pct(rb['attacker_frac'])}), {rb['non_attacker']} do not.")
    L.append("")

    # -- attack coverage --
    L.append("## Attack coverage")
    fams = report["attack_coverage"]["families"]
    L.append(f"**Families:** {fams['n_present']}/{fams['n_total']} present.")
    L.append("")
    L.append(f"- Present: {', '.join(fams['present']) or '(none)'}")
    L.append(f"- Absent: {', '.join(fams['absent']) or '(none)'}")
    L.append(f"- (7 base families: {', '.join(fams['base_families'])}; opt-in: "
             f"{', '.join(fams['opt_in_families'])})")
    L.append("")
    types = report["attack_coverage"]["types"]
    if types.get("available"):
        L.append(f"**Types:** {types['n_present']}/{types['n_total']} present "
                 f"(source: {types['source']}).")
        L.append("")
        L.append(f"- Absent ({len(types['absent'])}): {', '.join(types['absent']) or '(none)'}")
    else:
        L.append(f"**Types:** coverage unavailable -- {types.get('reason', '')}")
    L.append("")

    # -- domain diversity --
    L.append("## Domain diversity")
    dd = report["domain_diversity"]
    if dd.get("available"):
        L.append(f"{dd['n_domains']} domains across {dd['n_topologies']} topolog(ies): "
                 f"{', '.join(f'{k}={v}' for k, v in dd['topologies'].items()) or '(unknown)'}.")
        L.append("")
        L.append(f"Scenario events: {dd['event_domains']} domain(s) carry an event timeline, "
                 f"{dd['no_event_domains']} do not.")
    else:
        L.append(f"_Unavailable -- {dd.get('reason', 'no domain_catalog.json')}._")
    L.append("")

    # -- difficulty spread --
    L.append("## Difficulty spread")
    ds = report["difficulty_spread"]
    if ds.get("available"):
        r = ds["recall"]
        L.append(f"Per-domain recall over {ds['n_domains_scored']} scored domains: "
                 f"min={r['min']}, median={r['median']}, max={r['max']} (spread {r['spread']}).")
        if ds.get("hardest_families"):
            L.append("")
            L.append("Hardest families (lowest mean recall):")
            for h in ds["hardest_families"]:
                L.append(f"- {h['family']}: mean recall {h['mean']} "
                         f"(min {h['min']}, max {h['max']}, over {h['n_domains']} domains)")
    else:
        L.append(f"_Unavailable -- {ds.get('reason', 'no per-domain difficulty in domain_catalog.json')}._")
    L.append("")

    # -- size --
    L.append("## Size")
    sz = report["size"]
    L.append(f"Domains: {sz.get('n_domains', '?')}.")
    L.append("")
    if sz.get("row_counts"):
        L.append("| ml table | rows |")
        L.append("| --- | ---: |")
        for tbl, n in sz["row_counts"].items():
            L.append(f"| {tbl} | {n:,} |")
    pdr = sz.get("per_domain_rows")
    if pdr:
        L.append("")
        L.append(f"Rows per domain (from {pdr['table']}, {pdr['n_domains']} domains): "
                 f"min={int(pdr['min'])}, median={int(pdr['median'])}, max={int(pdr['max'])}.")
    L.append("")

    # -- warnings --
    L.append("## Warnings")
    if report["warnings"]:
        for w in report["warnings"]:
            L.append(f"- {w}")
    else:
        L.append("None -- the corpus is well-balanced and well-covered by these checks.")
    L.append("")

    return "\n".join(L)


# --------------------------------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------------------------------
def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Assess a merged corpus for class balance and attack/domain/difficulty coverage.")
    ap.add_argument("--corpus", required=True, help="corpus directory (as written by datagen.massive)")
    ap.add_argument("--out", default=None, help="write the Markdown report to this path")
    ap.add_argument("--json", action="store_true", help="also print the report dict as JSON")
    a = ap.parse_args(argv)

    if not os.path.isdir(a.corpus):
        ap.error(f"corpus directory not found: {a.corpus}")

    report = build_report(a.corpus)
    md = render_markdown(report)
    print(md)
    if a.json:
        print("\n" + json.dumps(report, indent=2, default=str))
    if a.out:
        with open(a.out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(md)
        print(f"\n[wrote {a.out}]")
    # exit non-zero when there are warnings, so the tool is usable as a CI balance gate
    return 1 if report["warnings"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
