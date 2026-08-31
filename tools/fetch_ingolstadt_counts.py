#!/usr/bin/env python3
"""Fetch real Ingolstadt signal-loop counts and write a ``--ref-counts`` file.

Source
------
SAVeNoW / Stadt Ingolstadt open traffic counts, served by TU München as an OGC SensorThings
(FROST) endpoint: ``https://savenow.gis.lrg.tum.de/frost/v1.1/``. No key, no registration.
15-minute vehicle counts per signal-controller detector, 2023-05-17 .. 2025-10-28.

    Things            one per intersection (``aggregation_type=whole_intersection``) and one per
                      loop (``aggregation_type=single_detector``); ``properties.lsa_id`` is the
                      signal-controller id, ``properties.det_id`` the loop id.
    Datastreams       named by the bare ``det_id``; ``phenomenonTime`` is the archive extent.
    Observations      ``phenomenonTime`` = **start** of a 15 min bin, **in UTC**, ``result`` =
                      vehicles. ``parameters.countPeriodEndTime`` restates the bin end in LOCAL
                      time with an explicit offset, which is how the UTC reading was confirmed.
                      The caller is responsible for converting to the scenario clock: the InTAS
                      SUMO clock is local Ingolstadt time, so ``06:00-07:00Z`` in November is
                      SUMO ``25200-28800``. See docs/realism/GEH-VALIDATION.md pitfall 3.

THE ARCHIVE IS NOT CLEAN
------------------------
The advertised extent is a first/last pair, not a promise. Three defects are handled here:

  * **gaps** -- Tue 2024-11-12 has zero observations at all 25 controllers. Such stations are
    classified ``unusable`` and never emitted.
  * **duplicated bins** -- the archive repeats some observations (one bin 100x in 2024-01);
    ``dedupe_observations`` collapses repeated ``phenomenonTime`` values, because summing the raw
    rows inflated one test hour ~50x while every coverage check still looked healthy.
  * **dead controllers** -- station 5012 reports 0 across all 32 bins of the 2025-10-21 peak hour
    with complete coverage. A complete-but-zero station is refused as ``unusable``.

None of the three is present on 2023-11-14, the day used for the validation in
docs/realism/GEH-RESULT.md.

Mapping onto InTAS
------------------
The ``name=`` groups in ``InTAS_E1.add.xml`` are ``lsa_id`` values; an InTAS detector id is the
real ``det_id`` with its trailing parenthesised hardware label stripped
(``1010_4`` <-> ``1010_4(DAL1)``).

THE DETECTOR-SUBSET RULE
------------------------
Never take the ``whole_intersection`` stream as the station count. SAVeNoW instruments detectors
that InTAS does not model; using the intersection aggregate inflates the count -- measured
**+92.9% at 5060** and +75.9% at 6030 on the 2023-11-14 AM peak hour, see
``whole_intersection_inflation_pct`` in the output. This tool **only ever sums the InTAS-matched
``single_detector`` streams**, and reports the intersection aggregate solely as a diagnostic.

LICENCE -- READ THIS
--------------------
The licence of the count data is **not formally stated** (the TUM catalogue reads "License Not
Specified"). Counts fetched by this tool MUST NOT be committed to this repository. The cache and
the output file default to git-ignored locations and every run prints the caveat.

Usage
-----
    # 06:00-07:00Z is the LOCAL 07:00-08:00 AM peak == SUMO 25200-28800.
    python tools/fetch_ingolstadt_counts.py \
        --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml \
        --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z \
        --include exact \
        --out .cache/savenow/ref_counts_20231114_0600Z_exact.json

Exit codes: 0 ok, 2 usage error, 3 partial fetch (unless ``--allow-partial``).
"""
from __future__ import annotations

import argparse
import csv
import datetime as _dt
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

DEFAULT_ENDPOINT = "https://savenow.gis.lrg.tum.de/frost/v1.1/"
DEFAULT_CACHE = ".cache/savenow"
BIN_SECONDS = 900          # the API's own "frequency": "15 min"; matches InTAS freq="900.00"
USER_AGENT = "SCMS-Simulator/fetch_ingolstadt_counts (research; contact via repository)"

LICENCE_CAVEAT = (
    "Licence NOT formally stated for this dataset (TUM catalogue: 'License Not Specified'). "
    "Served openly without registration and described by the city/THI/SAVeNoW as Open Data, but "
    "no explicit or machine-readable licence is attached to the data itself. DO NOT redistribute "
    "and DO NOT commit these counts into any repository. Request a written statement from "
    "b.willenborg@tum.de (TUM Geoinformatics, catalogue maintainer), info@savenow.de, or Stadt "
    "Ingolstadt Amt fuer Verkehrsmanagement und Geoinformation before vendoring."
)
ATTRIBUTION = ("Stadt Ingolstadt, Amt fuer Verkehrsmanagement und Geoinformation; "
               "published via the SAVeNoW project, hosted by TU Muenchen (FROST SensorThings).")

# Hardware label suffix on a real det_id: "1010_4(DAL1)" -> "1010_4".
_HW_LABEL = re.compile(r"\([^()]*\)\s*$")


# ------------------------------------------------------------------------------------------------
# time helpers
# ------------------------------------------------------------------------------------------------
def parse_instant(text: str, what: str) -> _dt.datetime:
    """ISO-8601 instant -> aware UTC datetime. A naive value is read as UTC."""
    s = text.strip()
    if s.endswith(("Z", "z")):
        s = s[:-1] + "+00:00"
    try:
        d = _dt.datetime.fromisoformat(s)
    except ValueError:
        raise SystemExit(f"[usage] --{what}: not an ISO-8601 instant: {text!r} "
                         f"(want e.g. 2023-11-14T07:00:00Z)")
    if d.tzinfo is None:
        d = d.replace(tzinfo=_dt.timezone.utc)
    return d.astimezone(_dt.timezone.utc)


def iso_z(d: _dt.datetime) -> str:
    return d.astimezone(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def utc_now_iso() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ------------------------------------------------------------------------------------------------
# InTAS detector layout
# ------------------------------------------------------------------------------------------------
def parse_intas_layout(path: str) -> dict[str, list[str]]:
    """``InTAS_E1.add.xml`` -> ``{station_name: [detector_id, ...]}`` for *named* stations only.

    The 2 unnamed loops (``income``/``outgoing``, file ``gate.xml``) are scenario-gate counters,
    not InTAS stations, and are deliberately dropped: only the 25 ``name=`` groups are valid
    ``--ref-counts`` keys (see refdata/geh_reference_counts.README.md section 1).
    """
    stations: dict[str, list[str]] = {}
    n_total = n_unnamed = 0
    for _ev, el in ET.iterparse(path, events=("end",)):
        if el.tag in ("e1Detector", "inductionLoop"):
            n_total += 1
            name, did = el.get("name"), el.get("id")
            if name and did:
                stations.setdefault(name, []).append(did)
            else:
                n_unnamed += 1
        el.clear()
    if not stations:
        raise SystemExit(f"[usage] --det-add: no named <e1Detector> groups found in {path}")
    for k in stations:
        stations[k] = sorted(set(stations[k]))
    stations["__layout__"] = [str(n_total), str(n_unnamed)]   # carried out-of-band, popped below
    return stations


def normalise_det_id(det_id: str) -> str:
    """Real ``det_id`` -> InTAS detector id: strip the trailing hardware label."""
    return _HW_LABEL.sub("", det_id.strip())


# ------------------------------------------------------------------------------------------------
# HTTP with an on-disk cache
# ------------------------------------------------------------------------------------------------
class Api:
    """Cached, polite, retrying GET against a FROST endpoint."""

    def __init__(self, endpoint: str, cache_dir: str, *, refresh: bool = False,
                 offline: bool = False, sleep_s: float = 0.5, timeout_s: float = 180.0,
                 retries: int = 3, verbose: bool = True):
        self.endpoint = endpoint if endpoint.endswith("/") else endpoint + "/"
        self.cache_dir = cache_dir
        self.refresh = refresh
        self.offline = offline
        self.sleep_s = sleep_s
        self.timeout_s = timeout_s
        self.retries = retries
        self.verbose = verbose
        self.n_http = 0
        self.n_cache = 0
        self.bytes_http = 0
        self.errors: list[dict] = []
        self._last_call = 0.0
        os.makedirs(self.cache_dir, exist_ok=True)

    def _cache_path(self, url: str) -> str:
        return os.path.join(self.cache_dir, hashlib.sha256(url.encode("utf-8")).hexdigest() + ".json")

    def _note_cache(self, url: str, path: str) -> None:
        idx = os.path.join(self.cache_dir, "index.jsonl")
        with open(idx, "a", encoding="utf-8") as fh:
            fh.write(json.dumps({"retrieved_utc": utc_now_iso(),
                                 "file": os.path.basename(path), "url": url}) + "\n")

    def get(self, url: str) -> dict:
        """Return the parsed JSON body. Raises ``ApiError`` after exhausting retries."""
        cp = self._cache_path(url)
        if os.path.exists(cp) and not self.refresh:
            try:
                with open(cp, encoding="utf-8") as fh:
                    body = json.load(fh)
                self.n_cache += 1
                return body
            except (OSError, json.JSONDecodeError):
                pass                                    # corrupt cache entry -> refetch
        if self.offline:
            raise ApiError(url, "not in cache and --offline was given")

        gap = self.sleep_s - (time.monotonic() - self._last_call)
        if gap > 0:
            time.sleep(gap)
        last = None
        for attempt in range(1, self.retries + 1):
            try:
                req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT,
                                                           "Accept": "application/json"})
                with urllib.request.urlopen(req, timeout=self.timeout_s) as resp:
                    raw = resp.read()
                self._last_call = time.monotonic()
                self.n_http += 1
                self.bytes_http += len(raw)
                body = json.loads(raw.decode("utf-8"))
                with open(cp, "w", encoding="utf-8") as fh:
                    json.dump(body, fh)
                self._note_cache(url, cp)
                return body
            except Exception as exc:                    # noqa: BLE001 - network/parse alike
                last = exc
                self._last_call = time.monotonic()
                if self.verbose:
                    print(f"  ! attempt {attempt}/{self.retries} failed: {type(exc).__name__}: {exc}",
                          file=sys.stderr)
                if attempt < self.retries:
                    time.sleep(min(30.0, 2.0 ** attempt))
        raise ApiError(url, f"{type(last).__name__}: {last}")

    def get_all(self, url: str, key: str = "value") -> list:
        """Follow top-level ``@iot.nextLink`` paging and concatenate ``value``."""
        out: list = []
        seen = 0
        while url:
            body = self.get(url)
            out.extend(body.get(key) or [])
            url = body.get("@iot.nextLink")
            seen += 1
            if seen > 500:
                raise ApiError(url or "", "paging did not terminate after 500 pages")
        return out

    def q(self, path: str, params: dict[str, str]) -> str:
        return self.endpoint + path + "?" + urllib.parse.urlencode(params, safe="", quote_via=urllib.parse.quote)


class ApiError(RuntimeError):
    def __init__(self, url: str, msg: str):
        super().__init__(msg)
        self.url = url
        self.msg = msg


# ------------------------------------------------------------------------------------------------
# catalogue + observations
# ------------------------------------------------------------------------------------------------
def fetch_stations(api: Api, lsa_ids: list[str], begin: _dt.datetime, end: _dt.datetime,
                   batch: int) -> tuple[dict[str, list[dict]], dict[int, list[tuple[str, float]]],
                                        list[dict]]:
    """One query per batch of controllers: catalogue **and** windowed observations together.

    Returns ``({lsa_id: [datastream_record, ...]}, {datastream_id: [(time, result), ...]}, errors)``.

    Batching on ``Thing/properties/lsa_id`` (rather than on datastream ids) is what keeps the
    endpoint responsive: the server indexes that predicate, while a long ``id eq .. or ..`` chain
    times out. Each record carries the archive extent (``Datastreams/phenomenonTime``); a *null*
    extent is the API's way of saying the stream has never carried a single observation, which is
    how ``3120``/``3130`` are distinguished from a genuine measured zero.

    Both the top-level ``@iot.nextLink`` and the nested ``Observations@iot.nextLink`` are followed,
    so a long window is never silently truncated.
    """
    cat: dict[str, list[dict]] = {s: [] for s in lsa_ids}
    obs: dict[int, list[tuple[str, float]]] = {}
    errs: list[dict] = []
    tfilt = f"phenomenonTime ge {iso_z(begin)} and phenomenonTime lt {iso_z(end)}"
    for i in range(0, len(lsa_ids), batch):
        chunk = lsa_ids[i:i + batch]
        url = api.q("Datastreams", {
            "$filter": " or ".join(f"Thing/properties/lsa_id eq '{s}'" for s in chunk),
            "$select": "id,name,phenomenonTime",
            "$expand": ("Thing($select=name,properties),"
                        f"Observations($filter={tfilt};$select=phenomenonTime,result;"
                        f"$orderby=phenomenonTime asc;$top=1000)"),
            "$top": "1000",
        })
        print(f"      stations {','.join(chunk)} ...", flush=True)
        try:
            rows = api.get_all(url)
        except ApiError as exc:
            errs.append({"stage": "station_batch", "stations": chunk,
                         "url": exc.url, "error": exc.msg})
            continue
        for r in rows:
            props = ((r.get("Thing") or {}).get("properties") or {})
            lsa = props.get("lsa_id")
            if lsa not in cat:
                continue                                # weather station / unrelated controller
            det = props.get("det_id") or r.get("name") or ""
            did = r.get("@iot.id")
            cat[lsa].append({
                "datastream_id": did,
                "det_id": det,
                "intas_id": normalise_det_id(det),
                "aggregation_type": props.get("aggregation_type"),
                "frequency": props.get("frequency"),
                "intersection_name": props.get("intersection_name"),
                "archive_extent": r.get("phenomenonTime"),
            })
            vals = list(r.get("Observations") or [])
            nxt = r.get("Observations@iot.nextLink")
            try:
                while nxt:
                    body = api.get(nxt)
                    vals.extend(body.get("value") or [])
                    nxt = body.get("@iot.nextLink")
            except ApiError as exc:
                errs.append({"stage": "observation_paging", "stations": [lsa],
                             "datastream_id": did, "url": exc.url, "error": exc.msg})
            bucket = obs.setdefault(did, [])
            for o in vals:
                try:
                    bucket.append((o["phenomenonTime"], float(o["result"])))
                except (KeyError, TypeError, ValueError):
                    continue
    return cat, obs, errs


# ------------------------------------------------------------------------------------------------
# station assembly
# ------------------------------------------------------------------------------------------------
def dedupe_observations(
        rows: list[tuple[str, float]]) -> tuple[list[tuple[str, float]], int, list]:
    """Collapse repeated ``phenomenonTime`` values. Returns ``(unique, n_discarded, conflicts)``.

    The archive really does contain duplicates: on datastream 35 (``1010_4(DAL1)``), January 2024
    holds 3389 observations over only 2798 distinct timestamps, one of them repeated **100 times**
    (measured 2026-08-30). Summing the raw rows would inflate that bin 100-fold while every
    coverage check still looked healthy, because the old code only ever tested for *too few* bins.

    A 15-min bin is by definition one value, so repeated timestamps are an archive artefact, not
    extra traffic. The first row in ``phenomenonTime asc`` order wins. When the copies disagree the
    true value is unknowable, so the disagreement is returned and the caller must degrade the
    station's ``comparability`` -- a conflicted bin may never be part of an ``exact`` headline.

    Identical copies collapse silently (this is the real archive's shape -- every duplicate observed
    so far carries the same value), and the naive sum is what the count would wrongly have been:

    >>> rows = [("t1", 10.0), ("t1", 10.0), ("t1", 10.0), ("t2", 5.0)]
    >>> uniq, discarded, conflicts = dedupe_observations(rows)
    >>> uniq, discarded, conflicts
    ([('t1', 10.0), ('t2', 5.0)], 2, [])
    >>> sum(v for _, v in rows), sum(v for _, v in uniq)   # naive vs deduped
    (35.0, 15.0)

    A disagreement is reported and never silently averaged or summed:

    >>> dedupe_observations([("t3", 7.0), ("t3", 9.0)])
    ([('t3', 7.0)], 1, [('t3', [7.0, 9.0])])

    An empty window stays empty, which is what keeps a "no data" hole distinguishable from a zero:

    >>> dedupe_observations([])
    ([], 0, [])
    """
    seen: dict[str, float] = {}
    conflict: dict[str, list[float]] = {}
    discarded = 0
    for t, v in rows:
        if t not in seen:
            seen[t] = v
            continue
        discarded += 1
        if v != seen[t]:
            conflict.setdefault(t, [seen[t]]).append(v)
    return (sorted(seen.items()), discarded,
            sorted((t, vs) for t, vs in conflict.items()))


def build_station(station: str, intas_dets: list[str], api_rows: list[dict],
                  obs: dict[int, list[tuple[str, float]]], expected_bins: int) -> dict:
    """Match, sum, and classify one station. Never invents a value."""
    singles = [r for r in api_rows if r["aggregation_type"] == "single_detector"]
    whole = [r for r in api_rows if r["aggregation_type"] == "whole_intersection"]

    by_intas: dict[str, list[dict]] = {}
    for r in singles:
        by_intas.setdefault(r["intas_id"], []).append(r)
    collisions = sorted(k for k, v in by_intas.items() if len(v) > 1)

    matched, unmatched = [], []
    for d in intas_dets:
        (matched if d in by_intas else unmatched).append(d)
    extra_api = sorted(set(by_intas) - set(intas_dets))

    det_detail: dict[str, dict] = {}
    total = 0.0
    bins_seen = 0
    bins_possible = 0
    dets_no_data = []
    dets_partial = []
    dets_conflicting = []
    n_dup_total = 0
    n_conflict_total = 0
    for d in matched:
        cnt = 0.0
        nb = 0
        ds_ids = []
        extent = None
        n_dup = 0
        conflicts: list[dict] = []
        for r in by_intas[d]:
            ds_ids.append(r["datastream_id"])
            extent = extent or r["archive_extent"]
            uniq, dup, conf = dedupe_observations(obs.get(r["datastream_id"], []))
            n_dup += dup
            for t, vs in conf:
                conflicts.append({"datastream_id": r["datastream_id"], "phenomenonTime": t,
                                  "results": vs})
            for _t, v in uniq:
                cnt += v
                nb += 1
        n_streams = len(by_intas[d])
        bins_possible += expected_bins * n_streams
        bins_seen += nb
        total += cnt
        n_dup_total += n_dup
        n_conflict_total += len(conflicts)
        det_detail[d] = {"det_id": [r["det_id"] for r in by_intas[d]],
                         "datastream_id": ds_ids, "count": cnt, "bins": nb,
                         "bins_expected": expected_bins * n_streams,
                         "duplicate_observations_discarded": n_dup,
                         "conflicting_duplicates": conflicts[:10],
                         "archive_extent": extent}
        if nb == 0:
            dets_no_data.append(d)
        elif nb < expected_bins * n_streams:
            dets_partial.append(d)
        if conflicts:
            dets_conflicting.append(d)

    # whole-intersection aggregate: diagnostic only, never the emitted count.
    whole_cnt = None
    whole_bins = 0
    for r in whole:
        uniq, _dup, _conf = dedupe_observations(obs.get(r["datastream_id"], []))
        for _t, v in uniq:
            whole_cnt = (whole_cnt or 0.0) + v
            whole_bins += 1

    reasons = []
    if not singles:
        reasons.append("no_detectors_registered")
    if not matched:
        reasons.append("no_matching_detectors")
    if matched and bins_seen == 0:
        reasons.append("no_observations_in_window")
    if unmatched:
        reasons.append("unmatched_intas_detectors")
    if dets_no_data and bins_seen > 0:
        reasons.append("detectors_without_data")
    if dets_partial:
        reasons.append("incomplete_bins")
    if collisions:
        reasons.append("det_id_collision")
    if n_dup_total:
        reasons.append("duplicate_observations_discarded")
    if dets_conflicting:
        reasons.append("conflicting_duplicate_observations")
    # Complete bin coverage that sums to exactly zero is a dead controller, not a measurement:
    # every detector at station 5012 reported 0 across all 32 bins of the 2025-10-21 AM peak while
    # the station still classified as `exact`. A signalised urban intersection does not carry zero
    # vehicles for a peak hour, and a zero that really means "no data" corrupts a GEH in exactly the
    # way the `unusable` class exists to prevent -- so it is refused, not emitted.
    zero_flow = bool(matched) and bins_seen > 0 and total == 0.0
    if zero_flow:
        reasons.append("all_matched_detectors_report_zero")
    if all(r["archive_extent"] is None for r in singles) and singles:
        reasons.append("empty_archive")

    if not matched or bins_seen == 0 or zero_flow:
        flag = "unusable"
    elif unmatched or dets_no_data or dets_partial or dets_conflicting:
        flag = "subset"
    else:
        flag = "exact"

    sets_identical = bool(matched) and not unmatched and not extra_api and not collisions

    infl = None
    if whole_cnt is not None and total > 0:
        infl = round(100.0 * (whole_cnt - total) / total, 2)

    return {
        "station": station,
        "intersection_name": (api_rows[0]["intersection_name"] if api_rows else None),
        "api_frequency": sorted({r["frequency"] for r in api_rows if r.get("frequency")}) or None,
        "comparability": flag,
        "comparability_reasons": reasons,
        "detector_sets_identical": sets_identical,
        "intas_detectors": len(intas_dets),
        "api_detectors": len(singles),
        "matched_detectors": len(matched),
        "matched_fraction": (round(len(matched) / len(intas_dets), 4) if intas_dets else None),
        "matched_detector_ids": matched,
        "unmatched_intas_detector_ids": unmatched,
        "api_only_detector_ids": extra_api,
        "det_id_collisions": collisions,
        "bins_expected": bins_possible,
        "bins_observed": bins_seen,
        "bin_coverage": (round(bins_seen / bins_possible, 4) if bins_possible else None),
        "detectors_without_data": dets_no_data,
        "detectors_partial_bins": dets_partial,
        "duplicate_observations_discarded": n_dup_total,
        "detectors_with_conflicting_duplicates": dets_conflicting,
        "n_conflicting_bins": n_conflict_total,
        # complete bins but a zero sum: legitimate-looking, almost never legitimate. Diagnostic
        # only at detector level (a rarely used turning lane really can read zero); a whole station
        # reading zero is refused above.
        "detectors_reporting_zero": sorted(d for d in matched
                                           if det_detail[d]["bins"] > 0
                                           and det_detail[d]["count"] == 0.0),
        "count": (total if flag != "unusable" else None),
        "archive_extent": sorted({r["archive_extent"] for r in singles
                                  if r["archive_extent"]}) or None,
        "whole_intersection_count": whole_cnt,
        "whole_intersection_bins": whole_bins,
        "whole_intersection_inflation_pct": infl,
        "per_detector": det_detail,
    }


# ------------------------------------------------------------------------------------------------
# main
# ------------------------------------------------------------------------------------------------
def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="fetch_ingolstadt_counts.py",
        description="Fetch real Ingolstadt loop counts (SAVeNoW/FROST) into a --ref-counts file.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=("LICENCE: not formally stated for this dataset. Never commit the fetched counts. "
                "See docs/realism/GEH-VALIDATION.md."))
    ap.add_argument("--det-add", required=True,
                    help="InTAS_E1.add.xml -- supplies the station groups and detector ids")
    ap.add_argument("--begin", required=True, help="window start, ISO-8601 (e.g. 2023-11-14T07:00:00Z)")
    ap.add_argument("--end", required=True, help="window end, exclusive, ISO-8601")
    ap.add_argument("--out", help="output JSON path (--ref-counts schema). Default: under --cache-dir")
    ap.add_argument("--csv", help="also write the documented CSV form (station,count,duration_s)")
    ap.add_argument("--unit", choices=("count", "veh_per_h"), default="count",
                    help="emit raw counts + duration_s (default) or pre-divided veh/h")
    ap.add_argument("--stations", help="comma-separated subset of station ids (default: all named)")
    ap.add_argument("--include", choices=("exact", "exact+subset"), default="exact+subset",
                    help="which comparability classes land in 'stations' (default exact+subset). "
                         "A 'subset' count is a LOWER BOUND on the InTAS station. 'unusable' "
                         "stations are NEVER emitted -- a zero would silently corrupt a GEH.")
    ap.add_argument("--endpoint", default=DEFAULT_ENDPOINT)
    ap.add_argument("--cache-dir", default=DEFAULT_CACHE, help=f"default {DEFAULT_CACHE} (git-ignored)")
    ap.add_argument("--refresh", action="store_true", help="ignore cached responses and refetch")
    ap.add_argument("--offline", action="store_true", help="cache only; fail on a cache miss")
    ap.add_argument("--batch-stations", type=int, default=5,
                    help="controllers per query (catalogue + observations come back together)")
    ap.add_argument("--sleep", type=float, default=0.5, help="minimum seconds between HTTP calls")
    ap.add_argument("--timeout", type=float, default=180.0)
    ap.add_argument("--retries", type=int, default=3)
    ap.add_argument("--allow-partial", action="store_true",
                    help="exit 0 even when some queries failed (the failure is still recorded)")
    args = ap.parse_args(argv)
    try:
        sys.stdout.reconfigure(line_buffering=True)      # progress is useful when redirected
    except Exception:                                    # noqa: BLE001
        pass

    begin = parse_instant(args.begin, "begin")
    end = parse_instant(args.end, "end")
    if end <= begin:
        raise SystemExit("[usage] --end must be after --begin")
    duration_s = int((end - begin).total_seconds())
    misaligned = (begin.timestamp() % BIN_SECONDS != 0) or (duration_s % BIN_SECONDS != 0)
    expected_bins = duration_s // BIN_SECONDS
    if misaligned:
        print(f"[warn] window is not aligned to the API's {BIN_SECONDS}s bins; bin coverage and "
              f"the 'exact' flag will be unreliable.", file=sys.stderr)

    layout = parse_intas_layout(args.det_add)
    n_total_det, n_unnamed = (int(x) for x in layout.pop("__layout__"))
    all_stations = sorted(layout)
    if args.stations:
        want = [s.strip() for s in args.stations.split(",") if s.strip()]
        bad = [s for s in want if s not in layout]
        if bad:
            raise SystemExit(f"[usage] --stations: not in {args.det_add}: {', '.join(bad)}")
        stations = sorted(want)
    else:
        stations = all_stations

    print(f"SAVeNoW / Stadt Ingolstadt loop counts via {args.endpoint}")
    print(f"  layout   {args.det_add}")
    print(f"           {n_total_det} e1Detectors, {len(all_stations)} named stations, "
          f"{sum(len(v) for v in layout.values())} named loops, {n_unnamed} unnamed (gate counters)")
    print(f"  window   {iso_z(begin)} .. {iso_z(end)}  ({duration_s}s = {expected_bins} x 15min bins)")
    print(f"  cache    {os.path.abspath(args.cache_dir)}"
          f"{'  [refresh]' if args.refresh else ''}{'  [offline]' if args.offline else ''}")
    print(f"  LICENCE  {LICENCE_CAVEAT}")
    print()

    api = Api(args.endpoint, args.cache_dir, refresh=args.refresh, offline=args.offline,
              sleep_s=args.sleep, timeout_s=args.timeout, retries=args.retries)

    print(f"fetching {len(stations)} controllers in batches of {args.batch_stations} ...", flush=True)
    catalogue, obs, errs = fetch_stations(api, stations, begin, end, args.batch_stations)
    ds_ids = sorted({r["datastream_id"] for rows in catalogue.values() for r in rows
                     if r["datastream_id"] is not None})
    n_single = sum(1 for rows in catalogue.values() for r in rows
                   if r["aggregation_type"] == "single_detector")
    dead = sorted({r["datastream_id"] for rows in catalogue.values() for r in rows
                   if not r["archive_extent"]})
    n_obs = sum(len(v) for v in obs.values())
    print(f"      {len(ds_ids)} datastreams ({n_single} single_detector, "
          f"{len(ds_ids) - n_single} whole_intersection); "
          f"{len(dead)} with a null archive extent (no observation ever recorded)")
    print(f"      {n_obs} observations in window; {api.n_http} HTTP call(s), "
          f"{api.n_cache} from cache, {api.bytes_http/1024:.0f} KiB fetched")
    print()

    details = {s: build_station(s, layout[s], catalogue.get(s, []), obs, expected_bins)
               for s in stations}

    # A station whose query failed must never be mistaken for a station with no data. Force it
    # unusable and say why, so a partial fetch can never masquerade as a complete one.
    failed_stations = {s for e in errs for s in (e.get("stations") or [])}
    for s in sorted(failed_stations & set(details)):
        d = details[s]
        d["fetch_failed"] = True
        d["comparability"] = "unusable"
        d["comparability_reasons"] = ["fetch_failed"] + d["comparability_reasons"]
        d["count"] = None

    keep = {"exact"} if args.include == "exact" else {"exact", "subset"}
    emitted, held, unusable = {}, {}, {}
    for s in stations:
        d = details[s]
        if d["comparability"] == "unusable":
            unusable[s] = d["comparability_reasons"]
        elif d["comparability"] in keep:
            v = d["count"]
            emitted[s] = v if args.unit == "count" else v * 3600.0 / duration_s
        else:
            held[s] = d["comparability"]

    partial = bool(errs)
    provenance = {
        "tool": "tools/fetch_ingolstadt_counts.py",
        "retrieved_utc": utc_now_iso(),
        "endpoint": api.endpoint,
        "dataset": "SAVeNoW / Stadt Ingolstadt signal-loop vehicle counts (OGC SensorThings/FROST)",
        "catalogue_url": "https://catalog.savenow.gis.lrg.tum.de/en/dataset/verkehrsdaten-von-ingolstadt",
        "attribution": ATTRIBUTION,
        "licence": "NOT FORMALLY STATED",
        "licence_caveat": LICENCE_CAVEAT,
        "redistribution": "prohibited-pending-written-licence; do not commit these counts",
        "query_window_utc": {"begin": iso_z(begin), "end_exclusive": iso_z(end),
                             "duration_s": duration_s, "weekday": begin.strftime("%A"),
                             "bin_seconds": BIN_SECONDS, "bins_expected_per_detector": expected_bins,
                             "aligned_to_bins": not misaligned},
        "api_frequency_field": sorted({f for d in details.values()
                                       for f in (d["api_frequency"] or [])}),
        "phenomenon_time_semantics": "phenomenonTime is the START of a 15 min bin; "
                                     "window filter is 'ge begin and lt end'",
        "detector_layout_file": os.path.abspath(args.det_add),
        "detector_layout": {"e1_detectors": n_total_det, "named_stations": len(all_stations),
                            "named_detectors": sum(len(v) for v in layout.values()),
                            "unnamed_gate_detectors": n_unnamed},
        "id_mapping_rule": "InTAS detector id == real det_id with the trailing "
                           "parenthesised hardware label stripped, e.g. 1010_4 <-> 1010_4(DAL1)",
        "aggregation_rule": "Per station, sum ONLY the InTAS-matched single_detector streams. "
                            "The whole_intersection stream is recorded as a diagnostic and is "
                            "NEVER used as the count (it inflates by up to ~46%).",
        "http": {"calls": api.n_http, "cache_hits": api.n_cache, "bytes": api.bytes_http,
                 "cache_dir": os.path.abspath(args.cache_dir)},
        "complete": not partial,
        "fetch_errors": errs,
        "summary": {
            "stations_requested": len(stations),
            "emitted": len(emitted),
            "held_back_by_include_filter": len(held),
            "unusable": len(unusable),
            "exact": sum(1 for d in details.values() if d["comparability"] == "exact"),
            "subset": sum(1 for d in details.values() if d["comparability"] == "subset"),
            "detector_sets_identical": sorted(s for s, d in details.items()
                                              if d["detector_sets_identical"]),
        },
    }

    doc: dict = {"unit": args.unit, "stations": emitted}
    if args.unit == "count":
        doc["duration_s"] = duration_s
    doc["comparability"] = {s: details[s]["comparability"] for s in stations}
    doc["unusable_stations"] = unusable
    doc["held_back_stations"] = held
    doc["station_details"] = details
    doc["provenance"] = provenance
    doc["WARNING"] = LICENCE_CAVEAT

    out_path = args.out or os.path.join(
        args.cache_dir, f"ref_counts_{begin.strftime('%Y%m%dT%H%M')}Z_{duration_s}s.json")
    os.makedirs(os.path.dirname(os.path.abspath(out_path)) or ".", exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(doc, fh, indent=2, sort_keys=False)
        fh.write("\n")

    if args.csv:
        os.makedirs(os.path.dirname(os.path.abspath(args.csv)) or ".", exist_ok=True)
        with open(args.csv, "w", encoding="utf-8", newline="") as fh:
            w = csv.writer(fh)
            if args.unit == "count":
                w.writerow(["station", "count", "duration_s", "comparability",
                            "matched_detectors", "intas_detectors"])
                for s, v in emitted.items():
                    w.writerow([s, v, duration_s, details[s]["comparability"],
                                details[s]["matched_detectors"], details[s]["intas_detectors"]])
            else:
                w.writerow(["station", "flow", "comparability",
                            "matched_detectors", "intas_detectors"])
                for s, v in emitted.items():
                    w.writerow([s, v, details[s]["comparability"],
                                details[s]["matched_detectors"], details[s]["intas_detectors"]])

    # ------------------------------------------------------------------ report
    hdr = f"{'station':<8} {'flag':<9} {'matched':>9} {'bins':>9} {'count':>9} {'veh/h':>9}  intersection"
    print(hdr)
    print("-" * len(hdr))
    for s in stations:
        d = details[s]
        c = d["count"]
        print(f"{s:<8} {d['comparability']:<9} "
              f"{str(d['matched_detectors']) + '/' + str(d['intas_detectors']):>9} "
              f"{str(d['bins_observed']) + '/' + str(d['bins_expected']):>9} "
              f"{('-' if c is None else f'{c:.0f}'):>9} "
              f"{('-' if c is None else f'{c * 3600.0 / duration_s:.0f}'):>9}  "
              f"{(d['intersection_name'] or '')[:44]}")
    print()
    print(f"emitted {len(emitted)} station(s); {len(unusable)} unusable "
          f"({', '.join(sorted(unusable)) or 'none'}); "
          f"{len(held)} held back by --include {args.include}")
    tot = sum(v for v in emitted.values())
    print(f"total over emitted stations: {tot:.0f} "
          f"{'veh' if args.unit == 'count' else 'veh/h'}")
    print(f"wrote {os.path.abspath(out_path)}")
    if args.csv:
        print(f"wrote {os.path.abspath(args.csv)}")
    print()
    print("PROVENANCE " + ATTRIBUTION)
    print("LICENCE    " + LICENCE_CAVEAT)

    if partial:
        print(f"\n[PARTIAL] {len(errs)} quer(y|ies) failed; the output records them under "
              f"provenance.fetch_errors and provenance.complete=false.", file=sys.stderr)
        for e in errs[:10]:
            print(f"  - {e['stage']}: {e['error']}", file=sys.stderr)
        if not args.allow_partial:
            return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
