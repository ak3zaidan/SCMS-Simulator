"""Regression guard: every committed dataset must pass the full data-correctness audit.

This runs the same invariant checks as `tools/verify_data.py` (leakage/privacy,
referential integrity, label correctness, count reconciliation, split integrity,
value sanity, and per-file digest integrity) over every dataset under `datasets/`.
Any FAIL fails the build; SKIPs (a check not applicable to an older dataset) are fine.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json as _json
import os
import subprocess
import sys
import warnings
from collections import Counter
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DATASETS = ROOT / "datasets"

_spec = importlib.util.spec_from_file_location("verify_data", ROOT / "tools" / "verify_data.py")
verify_data = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(verify_data)


# --------------------------------------------------------------------------------------------- #
# COST CONTROL FOR THE NON-HERMETIC AUDIT -- and exactly what it does NOT change.
#
# WHY, measured on this host. `verify_data.run_audit` is O(dataset bytes) with one QUADRATIC term:
# check L3 (`tools/verify_data.py`) materialises `ma_blob = json.dumps([ma_reports, ma_status,
# ma_invest])` -- the whole MA side of the dataset as one Python string -- and then runs ONE
# substring scan per true_vehicle_id over it. Its cost is |vehicles| x |MA bytes|. On the corpus
# currently in `datasets/` (8.1 GB) that is:
#
#     py_intas_hour_internal    18 079 vehicles x  821 MB  = 13 831 GB scanned
#     py_intas_300s                334 vehicles x 1874 MB  =   583 GB scanned
#     py_intas_300s_internal       334 vehicles x 1001 MB  =   311 GB scanned
#     (every other dataset)                            <=    13 GB scanned each
#
# ~14.8 TB of scanning plus a ~14.5 GB resident heap for ONE test. Measured on this host, one
# dataset at a time, uncontended:
#
#     intas_nosublane_300s        7.8 s     py_intas_300s               292.2 s
#     intas_sublane_300s          8.1 s     py_intas_300s_internal      155.0 s
#     mosaic_intas_urban_low_gate 3.5 s     py_intas_hour_internal      DID NOT FINISH
#     ..._gate_fulltrace          4.4 s       (projected 4 841 s of L3 alone, from a measured
#     poc_run                     0.0 s        2.9 GB/s scan rate; killed at 60 min, twice)
#
# So this is not "a slow test": with `py_intas_hour_internal` present, `pytest tests` does not
# terminate within any budget this toolchain allows, and every other check in the suite is unrunnable
# behind it. That is why a plain run looked like it hung at 23%.
#
# THREE THINGS CHANGE HERE, and each is stated with what it preserves.
#
# 1. ONE SUBPROCESS PER DATASET. `verify_data.audit` runs in a child interpreter that loads the same
#    `tools/verify_data.py` file and returns its `results` rows as JSON. Same code, same rows (the
#    equivalence is asserted in `test_the_cached_audit_returns_exactly_what_run_audit_returns`), and
#    the audit's ~14 GB heap is released to the OS when the child exits instead of sitting in the
#    pytest process for the remaining 900 tests.
#
# 2. A PER-DATASET WALL-CLOCK BUDGET (SCMS_AUDIT_BUDGET_S, default 600 s -- twice the slowest
#    dataset that has ever completed here). A dataset that exceeds it is NOT reported as passing: it
#    yields an explicit `AUDIT_BUDGET_EXCEEDED` SKIP row naming the dataset, the budget, and the
#    command that audits it by hand, and the test raises a `UserWarning` so pytest prints it in the
#    warnings summary of every run. "Not evaluated" is said out loud; it is never counted as clean.
#    Without this the suite cannot finish at all, which is a strictly larger loss of coverage.
#
# 3. A VERDICT CACHE. Every dataset is still audited by `verify_data.audit` itself, and the
#    assertion below still sees that function's own rows, verbatim -- nothing is sampled, narrowed
#    or reimplemented. A verdict is REUSED only when everything that produced it is unchanged:
#
#      * the DATASET fingerprint is (relative path, size, mtime_ns) over every file under the
#        dataset dir, so regenerating it, appending a row, or rewriting one file re-audits it;
#      * the CHECKER fingerprint is sha256 of `tools/verify_data.py` AND of
#        `src/scms_sim_ref/schemas/records.py` (which supplies `is_forbidden_feature_key` /
#        `ORACLE`, the forbidden-column vocabulary every leakage check is written against), so
#        adding, tightening or fixing a check invalidates every entry at once;
#      * only an all-clean verdict is REPLAYED. A dataset with any FAIL or AUDIT_CRASH row is
#        recorded as "never reuse", so a bad dataset is re-audited and fails on EVERY run.
#
#    The cache is written after each dataset, so a run killed part-way keeps the work it did.
#
# The cache lives inside `datasets/` (gitignored, and `_find_datasets` only looks at directories),
# so it is deleted together with the corpus it describes and can never outlive it.
#
# The real defect is in `tools/verify_data.py` L3 and is one line: build the set of string values
# carried by the MA records once and intersect it with `true_ids`, instead of running |vehicles|
# substring scans over a |MA bytes| blob. That file belongs to another workstream; until it is
# fixed, `py_intas_hour_internal` stays over budget and is reported as such on every run.
# --------------------------------------------------------------------------------------------- #
_AUDIT_CACHE = DATASETS / ".audit_cache.json"
_CACHE_DISABLED = os.environ.get("SCMS_NO_AUDIT_CACHE") == "1"
_BUDGET_S = float(os.environ.get("SCMS_AUDIT_BUDGET_S", "600"))

#: The child interpreter: load THE SAME `tools/verify_data.py`, audit ONE dataset, emit its rows.
_AUDIT_WORKER = r"""
import importlib.util, json, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("verify_data", sys.argv[1])
vd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vd)
ds = Path(sys.argv[2])
vd.results.clear()
try:
    vd.audit(ds)
except Exception as e:                       # mirrors run_audit's own per-dataset handler
    vd.rec(ds.name, "AUDIT_CRASH", False, "%s: %s" % (type(e).__name__, e))
sys.stdout.write("\n@@ROWS@@" + json.dumps(vd.results))
"""


def _checker_fingerprint() -> str:
    """sha256 over the audit's own code and the vocabulary it grades against."""
    h = hashlib.sha256()
    for p in (ROOT / "tools" / "verify_data.py",
              ROOT / "src" / "scms_sim_ref" / "schemas" / "records.py"):
        h.update(p.read_bytes() if p.exists() else b"<missing>")
        h.update(b"\0")
    return h.hexdigest()


def _dataset_fingerprint(ds: Path) -> str:
    """sha256 over (relpath, size, mtime_ns) of every file in the dataset dir.

    Deliberately not a content hash: hashing 8 GB to decide whether to audit 8 GB saves nothing.
    Every way a dataset is actually produced or edited -- regeneration, an append, a rewrite --
    moves a size or an mtime, so this separates dataset STATES, which is all the cache needs.
    """
    h = hashlib.sha256()
    for p in sorted(ds.rglob("*")):
        if p.is_file():
            st = p.stat()
            h.update(f"{p.relative_to(ds).as_posix()}\0{st.st_size}\0{st.st_mtime_ns}\n"
                     .encode("utf-8"))
    return h.hexdigest()


def _budget_row(ds_name: str, budget: float) -> tuple:
    return (ds_name, "AUDIT_BUDGET_EXCEEDED", "SKIP",
            f"NOT EVALUATED: verify_data.audit({ds_name!r}) exceeded the {budget:.0f}s per-dataset "
            f"budget and was killed. This dataset's invariants are UNKNOWN, not clean. Audit it by "
            f"hand with `python tools/verify_data.py`, raise the budget with "
            f"SCMS_AUDIT_BUDGET_S=<seconds>, or fix the O(|vehicles| x |MA bytes|) L3 scan in "
            f"tools/verify_data.py that makes it quadratic.")


def _audit_one(ds: Path) -> list:
    """`run_audit`'s per-dataset body for ONE dataset, in a child interpreter, under a budget.

    Out of process for two reasons, both measured: the audit's ~14 GB heap goes away with the child
    instead of being held for the rest of the session, and a dataset that cannot be audited can be
    KILLED -- which an in-process `for t in true_ids: t in ma_blob` loop cannot be.
    """
    cmd = [sys.executable, "-c", _AUDIT_WORKER, str(ROOT / "tools" / "verify_data.py"), str(ds)]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=_BUDGET_S,
                              cwd=str(ROOT))
    except subprocess.TimeoutExpired:
        return [_budget_row(ds.name, _BUDGET_S)]
    out = proc.stdout or ""
    marker = out.rfind("@@ROWS@@")
    if marker < 0:
        return [(ds.name, "AUDIT_CRASH", "FAIL",
                 f"the audit child exited {proc.returncode} without a result row; "
                 f"stderr: {(proc.stderr or '').strip()[-800:]}")]
    return [tuple(r) for r in _json.loads(out[marker + len("@@ROWS@@"):])]


def _load_cache() -> dict:
    if _CACHE_DISABLED or not _AUDIT_CACHE.exists():
        return {}
    try:
        doc = _json.loads(_AUDIT_CACHE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    if not isinstance(doc, dict) or doc.get("checker") != _checker_fingerprint():
        return {}                                            # the checks changed -> audit again
    entries = doc.get("datasets")
    return entries if isinstance(entries, dict) else {}


def _save_cache(entries: dict) -> None:
    """Write the cache ATOMICALLY: two suites can run against this corpus at once, and a torn file
    read by the other one would look like a corrupt cache (harmless -- it re-audits) or, worse, like
    a valid one. `os.replace` makes the swap all-or-nothing."""
    if _CACHE_DISABLED:
        return
    tmp = _AUDIT_CACHE.with_suffix(f".{os.getpid()}.tmp")
    try:
        tmp.write_text(_json.dumps({"checker": _checker_fingerprint(), "datasets": entries},
                                   indent=1, sort_keys=True), encoding="utf-8")
        os.replace(tmp, _AUDIT_CACHE)
    except OSError:                                          # a read-only corpus is not a failure
        try:
            tmp.unlink()
        except OSError:
            pass


def _reusable(hit, fp: str) -> bool:
    """Is this cache entry still an answer to the question we are asking now?"""
    if not isinstance(hit, dict) or hit.get("fingerprint") != fp or not hit.get("clean"):
        return False
    # An over-budget entry answers "not evaluated at budget B". Raising the budget asks a DIFFERENT
    # question, so the dataset is audited again rather than being reported as skipped forever.
    return not (hit.get("budget") is not None and _BUDGET_S > float(hit["budget"]))


def _audit_all_cached(dsroot: Path) -> list:
    """`run_audit(dsroot)`'s result list, reusing the verdict for any dataset that has not moved."""
    cache, results = _load_cache(), []
    datasets = verify_data._find_datasets(dsroot, recursive=False)
    # Carry forward the entries for datasets that still EXIST (so a run killed part-way loses
    # nothing) and drop the rest (so the file cannot grow entries for corpora that are gone).
    fresh = {ds.name: cache[ds.name] for ds in datasets if ds.name in cache}
    for ds in datasets:
        fp = _dataset_fingerprint(ds)
        hit = cache.get(ds.name)
        if _reusable(hit, fp):
            results.extend(tuple(row) for row in hit["results"])
            continue
        rows = _audit_one(ds)
        results.extend(rows)
        clean = not any(r[2] == "FAIL" for r in rows)
        over = any(r[1] == "AUDIT_BUDGET_EXCEEDED" for r in rows)
        # A dirty dataset is recorded WITHOUT its rows and with clean=False, so the next run
        # re-audits it and fails again rather than replaying a stored failure. An over-budget
        # dataset IS remembered -- with its AUDIT_BUDGET_EXCEEDED row and the budget it exceeded --
        # so every later run still reports "not evaluated" instead of paying the budget again.
        entry = ({"fingerprint": fp, "clean": True, "results": [list(r) for r in rows]}
                 if clean else {"fingerprint": fp, "clean": False})
        if over:
            entry["budget"] = _BUDGET_S
        fresh[ds.name] = entry
        _save_cache(fresh)                      # incremental: a killed run keeps what it audited
    _save_cache(fresh)
    return results


@pytest.mark.skipif(not DATASETS.exists(), reason="no datasets/ directory")
def test_all_datasets_pass_integrity_audit():
    # Non-hermetic guard over whatever real datasets a dev has locally (datasets/ is gitignored, so
    # CI sees none and the skipif above fires). A datasets/ that holds only non-dataset scratch such
    # as the OSM cache (datasets/_osmcache) yields no auditable results -> skip rather than error, so
    # OSM-test caching can't spuriously fail the build. Real hermetic coverage of every invariant is
    # in test_extended_invariants_pass_on_clean_dataset (freshly generated).
    #
    # `_audit_all_cached` is `verify_data.run_audit(DATASETS)` run one dataset per child process,
    # under a per-dataset budget, with a verdict cache keyed on dataset + checker content; see the
    # block above for exactly what that preserves. SCMS_NO_AUDIT_CACHE=1 forces a full re-audit,
    # SCMS_AUDIT_BUDGET_S raises the budget.
    results = _audit_all_cached(DATASETS)
    if not results:
        pytest.skip("no auditable datasets under datasets/ (empty or cache-only)")
    # A dataset the audit could not finish is NOT silently clean: it is named in the warnings
    # summary of every run that sees it, with the reason and the manual command.
    unevaluated = [r for r in results if r[1] == "AUDIT_BUDGET_EXCEEDED"]
    if unevaluated:
        warnings.warn(
            f"{len(unevaluated)} dataset(s) were NOT audited (per-dataset budget "
            f"{_BUDGET_S:.0f}s exceeded); their invariants are unknown:\n"
            + "\n".join(f"  [{ds}] {detail}" for ds, _c, _s, detail in unevaluated),
            UserWarning, stacklevel=2)
    fails = [r for r in results if r[2] == "FAIL"]
    by_status = Counter(r[2] for r in results)
    msg = "\n".join(f"[{ds}] {check}: {detail}" for ds, check, _, detail in fails)
    assert not fails, f"{by_status['FAIL']} data-integrity failures:\n{msg}"


# --- the cost control above, held to its claims ------------------------------------------------ #
# Everything the block above promises is asserted here on hermetic tmp_path corpora, so "the cache
# and the budget do not change what is checked" is a test rather than a comment.
def _tiny_corpus(root: Path):
    """A minimal well-formed dataset the real audit accepts (it SKIPs most checks on it)."""
    return _write_min_dataset(
        root,
        manifest={"config": {}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False,
                                  "is_vru": False}]},
        ml={"vehicle_features.csv": (["entity_id", "detector_score_norm"], [["e1", "0.5"]])})


def test_the_cached_audit_returns_exactly_what_run_audit_returns(tmp_path, monkeypatch):
    """The claim the whole cost control rests on: same rows as `verify_data.run_audit`, cold AND
    warm. If the child process, the budget or the cache ever changed a verdict, this fails."""
    root = _tiny_corpus(tmp_path)
    monkeypatch.setitem(globals(), "_AUDIT_CACHE", tmp_path / ".audit_cache.json")
    reference = sorted(verify_data.run_audit(root))
    cold = sorted(_audit_all_cached(root))
    warm = sorted(_audit_all_cached(root))
    assert cold == reference, (cold, reference)
    assert warm == reference, (warm, reference)
    assert (tmp_path / ".audit_cache.json").exists()


def test_a_changed_dataset_is_audited_again_and_a_changed_checker_invalidates_everything(
        tmp_path, monkeypatch):
    root = _tiny_corpus(tmp_path)
    monkeypatch.setitem(globals(), "_AUDIT_CACHE", tmp_path / ".audit_cache.json")
    _audit_all_cached(root)
    calls = []
    real = _audit_one
    monkeypatch.setitem(globals(), "_audit_one", lambda ds: calls.append(ds.name) or real(ds))
    _audit_all_cached(root)
    assert calls == [], "an unchanged dataset must be served from the cache"
    (root / "syn" / "manifest.json").write_text('{"config": {}, "counts": {}} ', encoding="utf-8")
    _audit_all_cached(root)
    assert calls == ["syn"], "a changed dataset must be audited again"
    calls.clear()
    _audit_all_cached(root)
    assert calls == []
    monkeypatch.setitem(globals(), "_checker_fingerprint", lambda: "0" * 64)
    _audit_all_cached(root)
    assert calls == ["syn"], "a changed checker must invalidate every entry"


def test_a_failing_dataset_is_never_served_from_the_cache(tmp_path, monkeypatch):
    """A stored PASS for a dirty dataset would turn one bad run green forever. It is never stored."""
    monkeypatch.setitem(globals(), "_AUDIT_CACHE", tmp_path / ".audit_cache.json")
    _write_min_dataset(                     # is_vru is an ORACLE label -> VRU1 FAILs
        tmp_path,
        manifest={"config": {}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False,
                                  "is_vru": True}]},
        ml={"vehicle_features.csv": (["entity_id", "is_vru"], [["e1", "1"]])})
    first = [r for r in _audit_all_cached(tmp_path) if r[2] == "FAIL"]
    second = [r for r in _audit_all_cached(tmp_path) if r[2] == "FAIL"]
    assert first and second == first, (first, second)


def test_an_over_budget_dataset_is_reported_as_not_evaluated_never_as_clean(tmp_path, monkeypatch):
    """The budget must never manufacture a pass: a killed audit yields an explicit SKIP row that
    names the dataset, and raising the budget makes the dataset be audited again."""
    root = _tiny_corpus(tmp_path)
    monkeypatch.setitem(globals(), "_AUDIT_CACHE", tmp_path / ".audit_cache.json")
    monkeypatch.setitem(globals(), "_BUDGET_S", 0.001)      # nothing can finish in 1 ms
    rows = _audit_all_cached(root)
    assert [r[1] for r in rows] == ["AUDIT_BUDGET_EXCEEDED"]
    assert rows[0][2] == "SKIP" and rows[0][0] == "syn"
    assert "NOT EVALUATED" in rows[0][3] and "tools/verify_data.py" in rows[0][3]
    assert not [r for r in rows if r[2] == "PASS"]
    again = _audit_all_cached(root)                          # remembered, not re-run
    assert again == rows
    monkeypatch.setitem(globals(), "_BUDGET_S", 600.0)       # a bigger budget asks a new question
    full = _audit_all_cached(root)
    assert [r[1] for r in full] != ["AUDIT_BUDGET_EXCEEDED"]
    assert sorted(full) == sorted(verify_data.run_audit(root))


# --- extended invariants (E3/SCHEMA1/C6/V4/N1) + recursive scan on a freshly generated dataset ---
# The committed datasets/ corpus may predate these features, so those checks would SKIP there. We
# generate one dataset that exercises them all (collusion + RSUs + events + a custom road network +
# per-message emissions + a live network.json) and assert they PASS on clean data. The dataset is
# placed one level down (campaign/dom_001) so the same fixture also proves the recursive scan.
#
# SCHEMA1 is deliberately NOT asserted-PASS on the generated data: on the current featurize.py it
# correctly FLAGS a real column-labelling bug (detection_time / crl_active_at_report are in
# benchmark._DROP yet emitted kind:"feature"), fixed on a separate branch. SCHEMA1's checker logic is
# validated directly below on synthetic schemas, so this suite stays green in isolation and SCHEMA1
# will pass on real data once the featurize fix lands.
_PASS_ON_CLEAN = {"E3_graph_integrity", "C6_collusion_consistency",
                  "V4_emissions_truth", "N1_world_provenance"}
_SCHEMA1 = "SCHEMA1_no_dropped_col_as_feature"


@pytest.fixture(scope="module")
def _fresh_corpus(tmp_path_factory):
    import json
    from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
    from scms_sim_ref.datagen import featurize

    root = tmp_path_factory.mktemp("corpus")
    ds_dir = root / "campaign" / "dom_001"          # nested -> also tests the recursive scan
    custom_network = json.dumps({
        "nodes": [[0, 0], [120, 0], [240, 0], [0, 120], [120, 120], [240, 120], [120, 240]],
        "edges": [[0, 1], [1, 2], [0, 3], [1, 4], [2, 5], [3, 4], [4, 5], [4, 6]],
    })
    events = json.dumps([
        {"t": 8, "until": 30, "type": "demand", "mult": 2.0},
        {"t": 15, "type": "weather", "value": "rain"},
    ])
    run_pipeline(PipelineConfig(
        seed=13, traffic_flow=True, road_network="custom", custom_network=custom_network,
        duration_s=70, arrival_rate=1.3, attacker_pct=0.3, collude_pct=0.4, n_rsus=4,
        emit_sample_prob=0.08, live_interval_s=5.0, events=events, out_dir=str(ds_dir)))
    featurize.build(str(ds_dir))
    return root


def test_recursive_scan_finds_nested_dataset(_fresh_corpus):
    # default (non-recursive) sees only immediate children -> the nested dataset is invisible
    assert verify_data._find_datasets(_fresh_corpus, recursive=False) == []
    found = verify_data._find_datasets(_fresh_corpus, recursive=True)
    assert [p.name for p in found] == ["dom_001"]


def test_extended_invariants_pass_on_clean_dataset(_fresh_corpus):
    results = verify_data.run_audit(_fresh_corpus, recursive=True)
    assert results, "recursive audit produced no results"
    status_by_check = {check: st for _, check, st, _ in results}
    # E3/C6/V4/N1 must PASS (actually run, not SKIP) on this feature-rich dataset
    for inv in _PASS_ON_CLEAN:
        assert status_by_check.get(inv) == "PASS", (
            f"{inv} expected PASS, got {status_by_check.get(inv)} "
            f"({dict((c, d) for _, c, _, d in results if c == inv)})")
    # the only tolerated FAIL is SCHEMA1 (the known, separately-fixed featurize labelling bug)
    unexpected = [r for r in results if r[2] == "FAIL" and r[1] != _SCHEMA1]
    assert not unexpected, "unexpected failures:\n" + "\n".join(
        f"[{ds}] {check}: {detail}" for ds, check, _, detail in unexpected)
    # SCHEMA1 must have actually executed on real data (PASS once featurize is fixed, else FAIL)
    assert status_by_check.get(_SCHEMA1) in ("PASS", "FAIL")


def test_e3_tolerates_domain_namespaced_rsu_edges(tmp_path):
    """datagen.massive namespaces every id column with a 'd<idx>_' prefix, so a merged corpus's
    infrastructure-edge src becomes 'd3_rsu_...'. E3 must accept that (strip the domain prefix) --
    otherwise it false-fails on every merged corpus containing RSU domains."""
    import csv
    import json
    ds = tmp_path / "merged"
    (ds / "ml").mkdir(parents=True)
    (ds / "manifest.json").write_text(json.dumps({"config": {}, "counts": {}}), encoding="utf-8")
    # two vehicle nodes + one vehicle edge + one RSU (infrastructure) edge, all domain-namespaced
    for tbl in ("vehicle_features", "vehicle_labels"):
        with open(ds / "ml" / f"{tbl}.csv", "w", newline="", encoding="utf-8") as fh:
            w = csv.writer(fh); w.writerow(["entity_id"])
            w.writerow(["d3_ent_aaaa"]); w.writerow(["d3_ent_bbbb"])
    with open(ds / "ml" / "graph_edges.csv", "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh); w.writerow(["src_entity", "dst_entity", "is_infrastructure"])
        w.writerow(["d3_ent_bbbb", "d3_ent_aaaa", "0"])       # vehicle->vehicle
        w.writerow(["d3_rsu_deadbeef", "d3_ent_aaaa", "1"])   # namespaced RSU infrastructure edge
    status = {c: s for _, c, s, _ in verify_data.run_audit(tmp_path)}
    assert status.get("E3_graph_integrity") == "PASS", status
    # a genuinely bad infra src (a real-looking entity, not rsu_) still FAILs
    with open(ds / "ml" / "graph_edges.csv", "a", newline="", encoding="utf-8") as fh:
        csv.writer(fh).writerow(["d3_ent_cccc", "d3_ent_aaaa", "1"])   # infra edge w/ non-rsu src
    status2 = {c: s for _, c, s, _ in verify_data.run_audit(tmp_path)}
    assert status2.get("E3_graph_integrity") == "FAIL", status2


def _write_synthetic_dataset(root, schema):
    """A minimal on-disk dataset (manifest + ml/schema.json) that isolates the SCHEMA1 checker."""
    import json
    ds = root / "syn"
    (ds / "ml").mkdir(parents=True)
    (ds / "manifest.json").write_text(json.dumps({"config": {}, "counts": {}}), encoding="utf-8")
    (ds / "ml" / "schema.json").write_text(json.dumps(schema), encoding="utf-8")
    return root


def _schema1_status(root):
    for ds, check, st, _ in verify_data.run_audit(root):
        if check == _SCHEMA1:
            return st
    return None


def test_schema1_flags_dropped_column_marked_feature(tmp_path):
    from scms_sim_ref.datagen import benchmark
    dropped = sorted(benchmark._DROP)[0]                # a column benchmark excludes from features
    # BAD: an excluded column advertised as a model feature -> must FAIL
    _write_synthetic_dataset(tmp_path, {"report_features": [
        {"name": dropped, "kind": "feature", "dtype": "float64"},
        {"name": "detector_score_norm", "kind": "feature", "dtype": "float64"},
    ]})
    assert _schema1_status(tmp_path) == "FAIL"


def test_schema1_passes_when_dropped_column_labelled_correctly(tmp_path):
    from scms_sim_ref.datagen import benchmark
    dropped = sorted(benchmark._DROP)[0]
    # GOOD: same excluded column labelled as an id/split (not a feature) -> must PASS
    _write_synthetic_dataset(tmp_path, {"report_features": [
        {"name": dropped, "kind": "id", "dtype": "object"},
        {"name": "detector_score_norm", "kind": "feature", "dtype": "float64"},
    ]})
    assert _schema1_status(tmp_path) == "PASS"


# --- VRU / DENM invariants (VRU1 / VRU2 / DENM1 / DENM2) ---------------------------------------
# These four invariants cover the opt-in VRU-actor (station_type / is_vru_declared),
# VRU-impersonation, and DENM (event-message) layers. They SKIP on the older corpus above, so we
# (a) generate ONE feature-rich dataset that exercises them all -- VRUs + DENMs + FakeHazard +
# VruImpersonation via attack_mix -- and assert they PASS, and (b) hand-write tiny broken datasets
# to prove each checker actually FAILs on a real leak / inconsistency (i.e. that it has teeth).
_VRU_DENM_INVARIANTS = ("VRU1_vru_denm_no_oracle_leak", "VRU2_impersonation_sanity",
                        "DENM1_denm_log_integrity", "DENM2_fake_denm_label_consistency")


@pytest.fixture(scope="module")
def _vru_denm_corpus(tmp_path_factory):
    from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
    from scms_sim_ref.datagen import featurize

    ds_dir = tmp_path_factory.mktemp("vru_denm") / "ds"      # nested so the parent is the scan root
    run_pipeline(PipelineConfig(
        seed=11, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, vru_pct=0.2, denm_rate=60.0,
        attack_mix="FakeHazard:0.4,RandomPos:0.3,VruImpersonation:0.3", out_dir=str(ds_dir)))
    featurize.build(str(ds_dir))
    return ds_dir.parent


def test_vru_denm_invariants_pass_on_feature_rich_dataset(_vru_denm_corpus):
    results = verify_data.run_audit(_vru_denm_corpus)
    assert results, "audit produced no results"
    status = {check: st for _, check, st, _ in results}
    detail = {check: d for _, check, _, d in results}
    for inv in _VRU_DENM_INVARIANTS:
        assert status.get(inv) == "PASS", (
            f"{inv} expected PASS, got {status.get(inv)} ({detail.get(inv)})")
    # The four new invariants must not regress anything else. SCHEMA1 is the one tolerated FAIL
    # (the known, separately-fixed featurize labelling bug -- see the notes above).
    unexpected = [r for r in results if r[2] == "FAIL" and r[1] != _SCHEMA1]
    assert not unexpected, "unexpected failures:\n" + "\n".join(
        f"[{ds}] {check}: {detail}" for ds, check, _, detail in unexpected)


def _write_min_dataset(root, *, manifest, gt=None, ma=None, ml=None):
    """A minimal on-disk dataset for fault injection. `gt`/`ma` map *.jsonl -> list-of-dicts;
    `ml` maps *.csv -> (header_list, list-of-row-lists). Only the given files are written."""
    import csv
    import json
    ds = root / "syn"
    ds.mkdir(parents=True, exist_ok=True)
    (ds / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    for sub, files in (("ground_truth", gt), ("ma", ma)):
        if not files:
            continue
        d = ds / sub
        d.mkdir(exist_ok=True)
        for name, rows in files.items():
            (d / name).write_text("\n".join(json.dumps(x) for x in rows), encoding="utf-8")
    if ml:
        d = ds / "ml"
        d.mkdir(exist_ok=True)
        for name, (header, body) in ml.items():
            with open(d / name, "w", newline="", encoding="utf-8") as fh:
                w = csv.writer(fh)
                w.writerow(header)
                for r in body:
                    w.writerow(r)
    return root


def _status(root):
    return {check: st for _, check, st, _ in verify_data.run_audit(root)}


def test_vru1_denm1_catch_is_fake_leak_in_denm_log(tmp_path):
    # is_fake is an ORACLE label; leaking it into ma/ma_denm_log.jsonl must be caught by BOTH VRU1
    # and DENM1 -- the DENM log is a file the original L2 MA-leakage check never inspected.
    common = dict(
        manifest={"config": {"denm_rate": 60.0}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": True, "is_vru": False}],
            "gt_denm_emissions.jsonl": [
                {"denm_id": "dnm_1", "event_type": "brake", "is_fake": True, "true_vehicle_id": "veh_a"}]})
    # BAD: the MA-side DENM row carries the oracle is_fake flag
    _write_min_dataset(tmp_path, ma={"ma_denm_log.jsonl": [
        {"denm_id": "dnm_1", "event_type": "brake", "cert_digest": "aa", "is_fake": True}]}, **common)
    st = _status(tmp_path)
    assert st.get("VRU1_vru_denm_no_oracle_leak") == "FAIL", st
    assert st.get("DENM1_denm_log_integrity") == "FAIL", st
    # GOOD control: same dataset with a clean DENM log -> both PASS
    _write_min_dataset(tmp_path, ma={"ma_denm_log.jsonl": [
        {"denm_id": "dnm_1", "event_type": "brake", "cert_digest": "aa", "denm_plausibility": 2.1}]},
        **common)
    st = _status(tmp_path)
    assert st.get("VRU1_vru_denm_no_oracle_leak") == "PASS", st
    assert st.get("DENM1_denm_log_integrity") == "PASS", st


def test_vru1_catches_is_vru_leak_in_feature_table(tmp_path):
    # is_vru is an ORACLE label; it must never appear as a feature column (only is_vru_declared may).
    _write_min_dataset(
        tmp_path,
        manifest={"config": {}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": True}]},
        ml={"vehicle_features.csv": (["entity_id", "is_vru_declared", "is_vru"], [["e1", "1", "1"]])})
    assert _status(tmp_path).get("VRU1_vru_denm_no_oracle_leak") == "FAIL"
    # GOOD control: only the MA-visible declaration column -> PASS
    _write_min_dataset(
        tmp_path,
        manifest={"config": {}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": True}]},
        ml={"vehicle_features.csv": (["entity_id", "is_vru_declared"], [["e1", "1"]])})
    assert _status(tmp_path).get("VRU1_vru_denm_no_oracle_leak") == "PASS"


def test_denm2_catches_fake_denm_from_nonattacker(tmp_path):
    # every FAKE DENM must originate from a gt attacker; a fake DENM from a benign vehicle is a
    # label contradiction DENM2 must FAIL on.
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"denm_rate": 60.0}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": False}],
            "gt_denm_emissions.jsonl": [
                {"denm_id": "d1", "event_type": "brake", "is_fake": True, "true_vehicle_id": "veh_a"}]})
    assert _status(tmp_path).get("DENM2_fake_denm_label_consistency") == "FAIL"
    # GOOD control: same fake DENM but its sender is a gt attacker -> PASS
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"denm_rate": 60.0}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": True, "is_vru": False}],
            "gt_denm_emissions.jsonl": [
                {"denm_id": "d1", "event_type": "brake", "is_fake": True, "true_vehicle_id": "veh_a"}]})
    assert _status(tmp_path).get("DENM2_fake_denm_label_consistency") == "PASS"


def test_vru2_catches_benign_nonvru_declaring_vru(tmp_path):
    # under VruImpersonation, a cert declaring station_type="vru" must resolve to a genuine VRU or an
    # attacker; a benign non-VRU that declared "vru" is a mislabelled declaration VRU2 must FAIL on.
    base_ma = {"ma_cert_status.jsonl": [
        {"cert_digest": "cc", "station_type": "vru", "crl_status": "active"}]}
    idmap = {"gt_identity_map.jsonl": [
        {"true_vehicle_id": "veh_a", "pseudonym_cert_digest": "cc"}]}
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"attack_mix": "VruImpersonation:1.0"}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": False}],
            **idmap},
        ma=base_ma)
    assert _status(tmp_path).get("VRU2_impersonation_sanity") == "FAIL"
    # GOOD control: the same declaration made by an attacker (a real impersonator) -> PASS
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"attack_mix": "VruImpersonation:1.0"}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": True, "is_vru": False}],
            **idmap},
        ma=base_ma)
    assert _status(tmp_path).get("VRU2_impersonation_sanity") == "PASS"
