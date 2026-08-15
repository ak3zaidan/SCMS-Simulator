"""Full data-correctness audit over every dataset on disk.

Checks: leakage/privacy, referential integrity, label correctness, count
reconciliation, split integrity, value sanity, and cryptographic file integrity.
Exit code 0 iff every non-skipped check passes across every dataset.
"""
from __future__ import annotations
import json, sys, csv, hashlib
from pathlib import Path
from collections import Counter, defaultdict

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))
from scms_sim_ref.schemas.records import is_forbidden_feature_key, ORACLE  # noqa

FEATURE_FILES = ["report_features", "subject_features", "vehicle_features",
                 "vehicle_features_ma", "subject_windows"]
LABEL_FILES = ["report_labels", "subject_labels", "vehicle_labels"]
VALID_SPLITS = {"train", "val", "test"}
VALID_CORRECTNESS = {"correct", "false_positive", "malicious", "duplicate",
                     "collusive", "faulty_detection", "malicious_false_report"}

results = []  # (dataset, check, status, detail)
def rec(ds, check, ok, detail=""):
    results.append((ds, check, "PASS" if ok else ("SKIP" if ok is None else "FAIL"), detail))

def run_audit(datasets_dir=None):
    """Audit every dataset under `datasets_dir`; return the results list.
    Each item is (dataset, check, status in {PASS,FAIL,SKIP}, detail)."""
    results.clear()
    dsroot = Path(datasets_dir) if datasets_dir else (ROOT / "datasets")
    targets = sorted(p for p in dsroot.iterdir() if p.is_dir() and (p / "manifest.json").exists())
    for p in targets:
        try:
            audit(p)
        except Exception as e:
            rec(p.name, "AUDIT_CRASH", False, f"{type(e).__name__}: {e}")
    return list(results)

def read_jsonl(p):
    if not p.exists(): return []
    out = []
    for line in p.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line:
            out.append(json.loads(line))
    return out

def read_csv(p):
    if not p.exists(): return [], []
    with open(p, encoding="utf-8", newline="") as fh:
        r = csv.DictReader(fh)
        rows = list(r)
        return r.fieldnames or [], rows

def file_sha256(p):
    h = hashlib.sha256()
    h.update(Path(p).read_bytes())
    return h.hexdigest()

def audit(ds_dir: Path):
    ds = ds_dir.name
    man_p = ds_dir / "manifest.json"
    if not man_p.exists():
        rec(ds, "manifest_exists", False, "no manifest.json"); return
    man = json.loads(man_p.read_text(encoding="utf-8"))
    gt = ds_dir / "ground_truth"; ma = ds_dir / "ma"; ml = ds_dir / "ml"

    # ---- load ----
    gt_vehicle = read_jsonl(gt / "gt_vehicle.jsonl")
    gt_idmap = read_jsonl(gt / "gt_identity_map.jsonl")
    gt_attacks = read_jsonl(gt / "gt_attacks.jsonl")
    gt_labels = read_jsonl(gt / "gt_report_labels.jsonl")
    gt_revoc = read_jsonl(gt / "gt_linkage_revocation.jsonl")
    ma_reports = read_jsonl(ma / "ma_reports.jsonl")
    ma_status = read_jsonl(ma / "ma_cert_status.jsonl")
    ma_invest = read_jsonl(ma / "ma_investigations.jsonl")

    digest2true = {r["pseudonym_cert_digest"]: r["true_vehicle_id"] for r in gt_idmap}
    veh_attacker = {r["true_vehicle_id"]: r.get("is_attacker") for r in gt_vehicle}
    veh_faulty = {r["true_vehicle_id"]: r.get("is_faulty") for r in gt_vehicle}

    # ============ A. LEAKAGE / PRIVACY ============
    # L1: feature CSVs have no forbidden columns
    leak_cols = {}
    for name in FEATURE_FILES:
        cols, _ = read_csv(ml / f"{name}.csv")
        bad = [c for c in cols if is_forbidden_feature_key(c)]
        if bad: leak_cols[name] = bad
    rec(ds, "L1_feature_cols_clean", not leak_cols, str(leak_cols))

    # L2: MA jsonl have no forbidden keys and no ORACLE visibility
    ma_leaks = []
    for fname, recs in [("ma_reports", ma_reports), ("ma_cert_status", ma_status),
                        ("ma_investigations", ma_invest)]:
        for i, r in enumerate(recs):
            if r.get("_visibility") == ORACLE:
                ma_leaks.append(f"{fname}[{i}] ORACLE visibility")
            bad = [k for k in _all_keys(r) if is_forbidden_feature_key(k)]
            if bad: ma_leaks.append(f"{fname}[{i}] keys {bad}")
    rec(ds, "L2_ma_no_forbidden_keys", not ma_leaks, "; ".join(ma_leaks[:5]))

    # L3: no true_vehicle_id string value leaks into MA files (privacy separation)
    true_ids = set(veh_attacker) | set(digest2true.values())
    ma_blob = json.dumps([ma_reports, ma_status, ma_invest])
    leaked_ids = sorted({t for t in true_ids if f'"{t}"' in ma_blob})
    rec(ds, "L3_no_true_id_in_ma", not leaked_ids, str(leaked_ids[:5]))

    # ============ B. REFERENTIAL INTEGRITY ============
    rep_ids_ma = {r["report_id"] for r in ma_reports}
    rep_ids_lbl = {r["report_id"] for r in gt_labels}
    if ma_reports and gt_labels:
        rec(ds, "R1_reportid_bijection_ma_vs_gtlabels", rep_ids_ma == rep_ids_lbl,
            f"ma-only={len(rep_ids_ma-rep_ids_lbl)} gt-only={len(rep_ids_lbl-rep_ids_ma)}")
    else:
        rec(ds, "R1_reportid_bijection_ma_vs_gtlabels", None, "missing files")

    _, rf_rows = read_csv(ml / "report_features.csv")
    if rf_rows and ma_reports:
        rf_ids = {r["report_id"] for r in rf_rows}
        rec(ds, "R2_report_features_subset_of_ma", rf_ids <= rep_ids_ma,
            f"orphan feature rows={len(rf_ids-rep_ids_ma)}")
    else:
        rec(ds, "R2_report_features_subset_of_ma", None, "missing files")

    # R3: every cert digest referenced in MA reports resolves in the identity map
    if ma_reports and digest2true:
        refd = set()
        for r in ma_reports:
            refd.add(r.get("subject_cert_digest")); refd.add(r.get("reporter_cert_digest"))
        refd.discard(None)
        unresolved = refd - set(digest2true)
        rec(ds, "R3_cert_digests_resolve", not unresolved,
            f"{len(unresolved)}/{len(refd)} unresolved")
    else:
        rec(ds, "R3_cert_digests_resolve", None, "missing files")

    # ============ C. LABEL CORRECTNESS ============
    _, sl_rows = read_csv(ml / "subject_labels.csv")
    mism_att = mism_flt = 0
    for r in sl_rows:
        tid = r.get("true_vehicle_id")
        if tid in veh_attacker:
            if str(veh_attacker[tid]).lower() != _boolstr(r.get("label_is_attacker")):
                mism_att += 1
            if tid in veh_faulty and veh_faulty[tid] is not None and \
               str(veh_faulty[tid]).lower() != _boolstr(r.get("label_is_faulty")):
                mism_flt += 1
    if sl_rows and veh_attacker:
        rec(ds, "C1_subject_attacker_label_matches_gt", mism_att == 0, f"{mism_att} mismatches")
        rec(ds, "C2_subject_faulty_label_matches_gt", mism_flt == 0, f"{mism_flt} mismatches")
    else:
        rec(ds, "C1_subject_attacker_label_matches_gt", None, "missing files")
        rec(ds, "C2_subject_faulty_label_matches_gt", None, "missing files")

    # C3: report_correctness values valid
    if gt_labels:
        bad_corr = sorted({r.get("report_correctness") for r in gt_labels} - VALID_CORRECTNESS)
        rec(ds, "C3_report_correctness_vocab", not bad_corr, str(bad_corr))
    else:
        rec(ds, "C3_report_correctness_vocab", None, "missing files")

    # C4: 'correct' report => subject truly attacker or faulty; 'false_positive' => benign & not faulty
    c4_bad = 0
    for r in gt_labels:
        sub = r.get("subject_true_id"); corr = r.get("report_correctness")
        if sub not in veh_attacker: continue
        is_bad_actor = bool(veh_attacker.get(sub)) or bool(veh_faulty.get(sub))
        if corr == "correct" and not is_bad_actor: c4_bad += 1
        if corr == "false_positive" and is_bad_actor: c4_bad += 1
    if gt_labels and veh_attacker:
        rec(ds, "C4_correctness_semantics", c4_bad == 0, f"{c4_bad} contradictions")
    else:
        rec(ds, "C4_correctness_semantics", None, "missing files")

    # C5: vehicle_labels attack_family consistency
    vcols, vl_rows = read_csv(ml / "vehicle_labels.csv")
    if vl_rows and "attack_family" in vcols:
        c5_bad = 0
        for r in vl_rows:
            att = _boolstr(r.get("label_is_attacker")) == "true"
            fam = (r.get("attack_family") or "none")
            if att and fam in ("none", "", None): c5_bad += 1
            if not att and fam not in ("none", "", None): c5_bad += 1
        rec(ds, "C5_family_matches_attacker_flag", c5_bad == 0, f"{c5_bad} mismatches")
    else:
        rec(ds, "C5_family_matches_attacker_flag", None, "no attack_family col")

    # ============ D. COUNT RECONCILIATION ============
    counts = man.get("counts", {})
    n_veh = counts.get("vehicles")
    if n_veh is not None and gt_vehicle:
        rec(ds, "CNT1_vehicles", n_veh == len(gt_vehicle),
            f"manifest={n_veh} gt_vehicle_lines={len(gt_vehicle)}")
    else:
        rec(ds, "CNT1_vehicles", None, "")
    n_rep = counts.get("reports")
    if n_rep is not None and ma_reports:
        ok = n_rep == len(ma_reports) == len(gt_labels)
        rec(ds, "CNT2_reports", ok,
            f"manifest={n_rep} ma={len(ma_reports)} gtlabels={len(gt_labels)} feat={len(rf_rows)}")
    else:
        rec(ds, "CNT2_reports", None, "")
    n_rev = counts.get("revoked")
    if n_rev is not None:
        gt_rev = sum(1 for r in gt_revoc if r.get("should_have_been_revoked"))
        ma_rev = sum(1 for r in ma_status if r.get("crl_status") == "revoked") or \
                 sum(1 for r in ma_invest if r.get("revocation_decision") == "revoke")
        rec(ds, "CNT3_revoked", n_rev in (gt_rev, ma_rev),
            f"manifest={n_rev} gt_should_revoke={gt_rev} ma_revoke={ma_rev}")
    else:
        rec(ds, "CNT3_revoked", None, "")
    n_inv = counts.get("investigations")
    if n_inv is not None and ma_invest:
        rec(ds, "CNT4_investigations", n_inv == len(ma_invest),
            f"manifest={n_inv} ma_invest={len(ma_invest)}")
    else:
        rec(ds, "CNT4_investigations", None, "")

    # ============ E. SPLIT INTEGRITY ============
    # each subject digest maps to a single split across subject_features/subject_labels
    split_map = defaultdict(set)
    for name in ("subject_features", "subject_labels"):
        cols, rows = read_csv(ml / f"{name}.csv")
        if "subject_cert_digest" in cols and "split" in cols:
            for r in rows:
                split_map[r["subject_cert_digest"]].add(r["split"])
    conflict = {k: v for k, v in split_map.items() if len(v) > 1}
    all_splits = {s for v in split_map.values() for s in v}
    if split_map:
        rec(ds, "S1_subject_single_split", not conflict, f"{len(conflict)} entities in >1 split")
        rec(ds, "S2_split_vocab", all_splits <= VALID_SPLITS, str(sorted(all_splits - VALID_SPLITS)))
    else:
        rec(ds, "S1_subject_single_split", None, "")
        rec(ds, "S2_split_vocab", None, "")

    # ============ F. VALUE SANITY ============
    # V1: ingest_time >= detection_time
    bad_t = sum(1 for r in ma_reports
                if r.get("ingest_time") is not None and r.get("detection_time") is not None
                and r["ingest_time"] < r["detection_time"] - 1e-9)
    if ma_reports:
        rec(ds, "V1_ingest_after_detection", bad_t == 0, f"{bad_t} reports ingest<detect")
    else:
        rec(ds, "V1_ingest_after_detection", None, "")
    # V2: detnorm_* non-negative
    neg = 0
    for r in ma_reports:
        for k, v in r.items():
            if k.startswith("detnorm_") and isinstance(v, (int, float)) and v < 0:
                neg += 1
    if ma_reports:
        rec(ds, "V2_detnorm_nonneg", neg == 0, f"{neg} negative detnorm values")
    else:
        rec(ds, "V2_detnorm_nonneg", None, "")
    # V3: attacker fraction ~ attacker_pct (config may express it as a percent [20] or fraction [0.2];
    # when it is 0 the fleet is driven by an explicit attacker_ids list, so the check is not applicable)
    pct = man.get("config", {}).get("attacker_pct")
    if pct and gt_vehicle:
        pct_percent = pct * 100.0 if pct <= 1.0 else float(pct)
        frac = 100.0 * sum(1 for r in gt_vehicle if r.get("is_attacker")) / len(gt_vehicle)
        rec(ds, "V3_attacker_pct_plausible", abs(frac - pct_percent) <= max(8.0, 0.5 * pct_percent),
            f"config={pct_percent:.0f}% actual={frac:.1f}%")
    else:
        rec(ds, "V3_attacker_pct_plausible", None, "attacker_ids mode" if pct == 0 else "")

    # ============ G. FILE INTEGRITY (digests) ============
    outputs = man.get("outputs")
    pairs = []
    if isinstance(outputs, dict):
        pairs = list(outputs.items())
    elif isinstance(outputs, list):
        pairs = [(o["path"], o["sha256"]) for o in outputs if isinstance(o, dict)]
    if pairs:
        mismatched = []
        data_files = {}
        for rel, sha in pairs:
            fp = ds_dir / rel
            if not fp.exists():
                mismatched.append(f"{rel}:missing"); continue
            actual = file_sha256(fp)
            data_files[rel] = actual
            if actual != sha:
                mismatched.append(rel)
        rec(ds, "I1_file_digests_match_manifest", not mismatched, "; ".join(mismatched[:4]))
        # recompute aggregate data_digest
        h = hashlib.sha256()
        for rel in sorted(data_files):
            h.update(rel.encode()); h.update(data_files[rel].encode())
        want = man.get("data_digest_sha256")
        if want:
            rec(ds, "I2_aggregate_data_digest", h.hexdigest() == want,
                f"recomputed={h.hexdigest()[:12]} manifest={want[:12]}")
        else:
            rec(ds, "I2_aggregate_data_digest", None, "no digest in manifest")
    else:
        rec(ds, "I1_file_digests_match_manifest", None, "no outputs list")
        rec(ds, "I2_aggregate_data_digest", None, "")

def _all_keys(obj, out=None):
    out = [] if out is None else out
    if isinstance(obj, dict):
        for k, v in obj.items():
            out.append(k); _all_keys(v, out)
    elif isinstance(obj, (list, tuple)):
        for v in obj: _all_keys(v, out)
    return out

def _boolstr(v):
    s = str(v).strip().lower()
    if s in ("1", "true", "yes"): return "true"
    if s in ("0", "false", "no", ""): return "false"
    return s

def main():
    dsroot = ROOT / "datasets"
    targets = sorted(p for p in dsroot.iterdir() if p.is_dir() and (p / "manifest.json").exists())
    run_audit(dsroot)

    # ---- report ----
    by_status = Counter(r[2] for r in results)
    fails = [r for r in results if r[2] == "FAIL"]
    print(f"\nDatasets audited: {len(targets)}   Checks: {len(results)}   "
          f"PASS={by_status['PASS']} FAIL={by_status['FAIL']} SKIP={by_status['SKIP']}")
    if fails:
        print("\n=== FAILURES ===")
        for ds, check, _, detail in fails:
            print(f"  [{ds}] {check}: {detail}")
    else:
        print("\nAll non-skipped checks PASS.")
    # per-check summary
    print("\n=== PER-CHECK (fail/total across datasets) ===")
    checks = {}
    for ds, check, st, _ in results:
        d = checks.setdefault(check, [0, 0, 0])
        d[0 if st == "PASS" else (1 if st == "FAIL" else 2)] += 1
    for check in sorted(checks):
        p, f, s = checks[check]
        flag = "  <-- FAIL" if f else ""
        print(f"  {check:42s} pass={p:2d} fail={f:2d} skip={s:2d}{flag}")
    return 1 if fails else 0

if __name__ == "__main__":
    sys.exit(main())
