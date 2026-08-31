#!/usr/bin/env python3
"""Render a `tools/sumo_realism.py --ref-counts` report as the tables used in GEH-RESULT.md.

Reads the report JSON (and, optionally, the rich reference file from
``tools/fetch_ingolstadt_counts.py`` for intersection names, comparability and detector coverage)
and prints Markdown: a per-station modelled-vs-measured table, the FHWA gate block, and the
aggregate lines.

Nothing here re-derives GEH: every GEH value is read from the report, which computes it through the
shipped ``geh_summary``. Only the presentation columns (difference, percentage difference, FHWA
band) are computed here, and the FHWA band logic mirrors ``sumo_realism._flow_tolerance_pass``.

LICENCE: reference counts are SAVeNoW / Stadt Ingolstadt data whose licence is NOT formally stated.
Rendered tables contain those measured values -- treat the output as unredistributable and keep it
out of the repository unless a licence has been obtained.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

BANDS_NOTE = "FHWA flow band: <700 -> +/-100 veh/h; 700..2700 -> +/-15%; >2700 -> +/-400 veh/h"


def band_pass(m: float, c: float) -> tuple[bool, str]:
    if c < 700.0:
        return abs(m - c) <= 100.0, "<700"
    if c <= 2700.0:
        return abs(m - c) <= 0.15 * c, "700-2700"
    return abs(m - c) <= 400.0, ">2700"


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="Markdown tables for a sumo_realism validation report.")
    p.add_argument("--report", required=True, help="sumo_realism --json output")
    p.add_argument("--ref-counts", help="rich reference JSON (names/comparability/coverage)")
    p.add_argument("--title", default=None)
    a = p.parse_args(argv)

    rep = json.loads(Path(a.report).read_text(encoding="utf-8"))
    geh = rep["sections"]["geh"]
    if rep.get("comparison_kind") != "fhwa_validation":
        raise SystemExit(f"{a.report}: comparison_kind is {rep.get('comparison_kind')!r}, not "
                         f"'fhwa_validation'. Only a --ref-counts report may be rendered here.")
    det = {}
    if a.ref_counts:
        det = (json.loads(Path(a.ref_counts).read_text(encoding="utf-8"))
               .get("station_details") or {})

    if a.title:
        print(f"### {a.title}\n")
    rows = geh["geh"]["stations"]
    print("| Station | Intersection | Cmp | Loops | Modelled | Measured | Diff | Diff % | GEH | "
          "GEH<5 | FHWA band |")
    print("|---|---|---|---|---:|---:|---:|---:|---:|:---:|:---:|")
    for r in rows:
        sid = r["station"]
        m, c, g = float(r["modelled"]), float(r["counted"]), float(r["geh"])
        d = det.get(sid, {})
        name = (d.get("intersection_name") or "").replace("|", "/")
        cmp_ = d.get("comparability", "?")
        loops = (f'{d.get("matched_detectors", "?")}/{d.get("intas_detectors", "?")}'
                 if d else "?")
        ok_band, band = band_pass(m, c)
        pct = (m - c) / c * 100.0 if c else float("nan")
        print(f"| `{sid}` | {name} | {cmp_} | {loops} | {m:,.0f} | {c:,.0f} | {m - c:+,.0f} | "
              f"{pct:+.1f}% | **{g:.2f}** | {'yes' if g < 5 else 'NO'} | "
              f"{'yes' if ok_band else 'NO'} ({band}) |")
    s = geh["geh"]
    print(f"| **total** | | | | **{s['total_modelled']:,.0f}** | **{s['total_counted']:,.0f}** | "
          f"**{s['total_modelled'] - s['total_counted']:+,.0f}** | "
          f"**{s['total_rel_error'] * 100:+.1f}%** | **{s['total_geh']:.2f}** | | |")
    print(f"\n{BANDS_NOTE}\n")

    print(f"- stations compared: **{s['n_stations']}**  "
          f"(modelled keys {rep['modelled']['n_keys']}, reference keys {rep['reference']['n_keys']}, "
          f"shared {geh['n_stations_shared']})")
    print(f"- GEH median **{s['geh_median']:.3f}**, p85 **{s['geh_p85']:.3f}**, "
          f"max **{max(float(r['geh']) for r in rows):.3f}** "
          f"(at `{max(rows, key=lambda r: float(r['geh']))['station']}`), "
          f"min **{min(float(r['geh']) for r in rows):.3f}** "
          f"(at `{min(rows, key=lambda r: float(r['geh']))['station']}`)")
    print(f"- window {rep['modelled']['begin_s']:.0f}..{rep['modelled']['end_s']:.0f} s "
          f"({rep['modelled']['duration_s']:.0f} s, {rep['modelled']['n_intervals']} E1 intervals), "
          f"seed {rep['modelled'].get('seed')}")
    print(f"- GEH < {geh['calibration_target_geh']:g} (stricter calibration target) on "
          f"**{geh['calibration_target_pass_fraction'] * 100:.1f}%** of stations")

    print("\n| FHWA gate | value | threshold | verdict |")
    print("|---|---:|---|:---:|")
    for gt in geh["gates"]:
        thr = ((gt.get("reference") or {}).get("threshold")) or "-"
        val = "-" if gt.get("value") is None else f"{gt['value']:.4f}"
        print(f"| {gt['title']} | {val} | {thr} | **{gt['status'].upper()}** |")
    for n in geh.get("notes") or []:
        print(f"\n> NOTE: {n}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
