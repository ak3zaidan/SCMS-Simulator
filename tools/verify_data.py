"""Full data-correctness audit over every dataset on disk.

Checks: leakage/privacy, referential integrity, label correctness, count
reconciliation, split integrity, value sanity, and cryptographic file integrity.
Exit code 0 iff every non-skipped check passes across every dataset.
"""
from __future__ import annotations
import json, sys, csv, hashlib, re
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

def _find_datasets(dsroot: Path, recursive: bool = False):
    """Dataset dirs (those containing a manifest.json) under `dsroot`.
    Non-recursive (default): immediate children only -- the historical behaviour.
    Recursive: any nested dataset dir, so nested corpora (datasets/campaign/merged,
    datasets/<x>/dom_00N) are audited too."""
    if recursive:
        if not dsroot.exists():
            return []
        return sorted({m.parent for m in dsroot.rglob("manifest.json") if m.is_file()})
    return sorted(p for p in dsroot.iterdir() if p.is_dir() and (p / "manifest.json").exists())


def run_audit(datasets_dir=None, recursive=False):
    """Audit every dataset under `datasets_dir`; return the results list.
    Each item is (dataset, check, status in {PASS,FAIL,SKIP}, detail).
    `recursive` (default False -> unchanged behaviour) also descends into nested corpora."""
    results.clear()
    dsroot = Path(datasets_dir) if datasets_dir else (ROOT / "datasets")
    for p in _find_datasets(dsroot, recursive):
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
    gt_emissions = read_jsonl(gt / "gt_emissions_sample.jsonl")
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

    # R3: every SUBJECT cert digest resolves to a vehicle (subjects are vehicles). REPORTER digests
    # may additionally be Road-Side-Unit infrastructure certs, which are NOT vehicle pseudonyms and so
    # are absent from the identity map -- allow up to n_rsus distinct unresolved reporters.
    if ma_reports and digest2true:
        known = set(digest2true)
        subj_unresolved = {r.get("subject_cert_digest") for r in ma_reports} - known - {None}
        rep_unresolved = {r.get("reporter_cert_digest") for r in ma_reports} - known - {None}
        cfg = man.get("config") or {}
        n_rsus = int(cfg.get("n_rsus", 0) or 0)
        coords = str(cfg.get("rsu_coords", "") or "").strip()
        if coords:                                   # explicit placement may exceed n_rsus
            n_rsus = max(n_rsus, sum(1 for p in coords.split(";") if p.strip()))
        ok = (not subj_unresolved) and (len(rep_unresolved) <= n_rsus)
        rec(ds, "R3_cert_digests_resolve", ok,
            f"subj_unresolved={len(subj_unresolved)} reporter_unresolved={len(rep_unresolved)} "
            f"(RSU allowance={n_rsus})")
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
    # revoked count = distinct TRUE vehicles with a revoked cert. Robust to pseudonym rotation
    # (many revoked certs per vehicle) and to false revocations (a framed benign counts as revoked).
    n_rev = counts.get("revoked")
    if n_rev is not None and ma_status and digest2true:
        revoked_true = {digest2true[c["cert_digest"]] for c in ma_status
                        if c.get("crl_status") == "revoked" and c["cert_digest"] in digest2true}
        rec(ds, "CNT3_revoked", len(revoked_true) == n_rev,
            f"manifest={n_rev} distinct_revoked_vehicles={len(revoked_true)}")
    elif n_rev is not None:
        rec(ds, "CNT3_revoked", n_rev == len(gt_revoc), f"manifest={n_rev} gt_rows={len(gt_revoc)}")
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

    # ============ H. GRAPH / SCHEMA / COLLUSION / EMISSIONS / WORLD ============
    cfg = man.get("config") or {}
    attackers = {tid for tid, a in veh_attacker.items() if a}
    attackers |= {a.get("true_vehicle_id") for a in gt_attacks if a.get("true_vehicle_id")}
    faulty = {tid for tid, f in veh_faulty.items() if f}

    # E3: graph integrity -- every edge endpoint resolves, and the edge count reconciles with the
    # reports that survived featurization. featurize emits a reporter->subject edge for each report
    # whose SUBJECT cert resolves to a true vehicle. A VEHICLE reporter also resolves (its src is a
    # known entity id); an RSU/infrastructure reporter does NOT resolve -- it is emitted as an opaque
    # "rsu_" node flagged is_infrastructure=1, so its src is legitimately outside the vehicle node set.
    # Datasets predating the RSU-graph feature have no is_infrastructure column and dropped RSU reports
    # entirely (edge iff BOTH resolve); we reconcile against whichever regime the columns indicate.
    ge_path = ml / "graph_edges.csv"
    edge_cols, edge_rows = read_csv(ge_path)
    node_ids = set()
    for tbl in ("vehicle_features", "vehicle_labels"):
        ncols, nrows = read_csv(ml / f"{tbl}.csv")
        if "entity_id" in ncols:
            node_ids |= {r["entity_id"] for r in nrows}
    if not ge_path.exists() or not node_ids:
        rec(ds, "E3_graph_integrity", None, "no graph_edges/node tables")
    else:
        def _is_infra(r):
            return str(r.get("is_infrastructure", "0")) in ("1", "True", "true")
        # every dst (subject) resolves; every NON-infra src (vehicle reporter) resolves
        need_resolve = {r.get("dst_entity") for r in edge_rows}
        need_resolve |= {r.get("src_entity") for r in edge_rows if not _is_infra(r)}
        orphan = (need_resolve - {None}) - node_ids
        # infrastructure edges must carry an opaque rsu_ src (never a real/enrolled id). A merged
        # corpus (datagen.massive) namespaces every id column with a "d<idx>_" domain prefix, so the
        # rsu_ marker may be prefixed -- strip an optional leading domain namespace before checking.
        def _is_rsu_src(s):
            return re.match(r"^(d\d+_)?rsu_", str(s or "")) is not None
        infra_bad = {r.get("src_entity") for r in edge_rows
                     if _is_infra(r) and not _is_rsu_src(r.get("src_entity"))}
        if ma_reports and digest2true:
            known = set(digest2true)
            if "is_infrastructure" in edge_cols:         # RSU-graph regime: edge per subject-resolving report
                expected = sum(1 for r in ma_reports if r.get("subject_cert_digest") in known)
            else:                                        # legacy regime: edge iff both resolve
                expected = sum(1 for r in ma_reports
                               if r.get("subject_cert_digest") in known
                               and r.get("reporter_cert_digest") in known)
            count_ok = len(edge_rows) == expected
        else:
            expected, count_ok = None, True
        rec(ds, "E3_graph_integrity", (not orphan) and (not infra_bad) and count_ok,
            f"orphan_edge_entities={len(orphan)} infra_bad={len(infra_bad)} "
            f"edges={len(edge_rows)} expected_from_reports={expected}")

    # SCHEMA1: ml/schema.json must not advertise as a model feature any column the benchmark drops.
    # benchmark._feature_matrix() excludes benchmark._DROP from the feature matrix, so schema.json
    # labelling any of those columns kind:"feature" is a contract violation -- it tells an ML consumer
    # to train on a column the shipped benchmark deliberately ignores. Kept strict on purpose: on the
    # current featurize.py this correctly FLAGS detection_time & crl_active_at_report, which are in
    # _DROP yet emitted as kind:"feature" (a real featurize labelling bug, fixed on a separate branch).
    sch_path = ml / "schema.json"
    if not sch_path.exists():
        rec(ds, "SCHEMA1_no_dropped_col_as_feature", None, "no schema.json")
    else:
        try:
            from scms_sim_ref.datagen import benchmark as _bench
            drop = set(_bench._DROP)
        except Exception as e:
            drop = None
            rec(ds, "SCHEMA1_no_dropped_col_as_feature", None, f"benchmark import failed: {e}")
        if drop is not None:
            schema = json.loads(sch_path.read_text(encoding="utf-8"))
            bad = []
            for tbl, cols in schema.items():
                if tbl == "_legend" or not isinstance(cols, list):
                    continue
                for c in cols:
                    if (isinstance(c, dict) and c.get("kind") == "feature"
                            and c.get("name") in drop):
                        bad.append(f"{tbl}.{c.get('name')}")
            rec(ds, "SCHEMA1_no_dropped_col_as_feature", not bad, "; ".join(bad[:6]))

    # C6: collusion consistency -- with collude_pct>0 some attackers file false reports; every
    # malicious_false_report was PRODUCED by its reporter, which must therefore be a true attacker.
    coll = cfg.get("collude_pct")
    try:
        coll = float(coll) if coll not in (None, "") else 0.0
    except (TypeError, ValueError):
        coll = 0.0
    if coll <= 0:
        rec(ds, "C6_collusion_consistency", None, "no collusion (collude_pct=0)")
    elif not gt_labels:
        rec(ds, "C6_collusion_consistency", None, "no gt_report_labels")
    else:
        mfr = [r for r in gt_labels if r.get("report_correctness") == "malicious_false_report"]
        bad = [r.get("report_id") for r in mfr if r.get("reporter_true_id") not in attackers]
        rec(ds, "C6_collusion_consistency", not bad,
            f"malicious_false_report={len(mfr)} reporter_not_attacker={len(bad)}")

    # V4: emissions truth -- a falsified sampled CAM must belong to an attacker (never to a benign,
    # non-faulty vehicle) and, when its attack window is recoverable from gt_attacks, fall inside it.
    if not gt_emissions:
        rec(ds, "V4_emissions_truth", None, "no emissions sample")
    else:
        fals = [e for e in gt_emissions if e.get("falsified")]
        win = {a.get("true_vehicle_id"): (a.get("start_time"), a.get("end_time")) for a in gt_attacks}
        non_attacker = benign_nonfaulty = outside = 0
        EPS = 0.05
        for e in fals:
            v = e.get("true_vehicle_id")
            if v not in attackers:
                non_attacker += 1
                if v not in faulty:
                    benign_nonfaulty += 1
            w = win.get(v)
            if w and w[0] is not None and w[1] is not None:
                t = float(e.get("t", 0) or 0)
                if not (float(w[0]) - EPS <= t <= float(w[1]) + EPS):
                    outside += 1
        rec(ds, "V4_emissions_truth", non_attacker == 0 and outside == 0,
            f"falsified={len(fals)} non_attacker={non_attacker} "
            f"benign_nonfaulty={benign_nonfaulty} outside_window={outside}")

    # N1: world provenance -- network.json (written for live/GUI runs) must be internally consistent
    # (every edge endpoint indexes an existing node); and for a custom map, the manifest's
    # custom_network must parse and its node/edge counts must match the exported geometry (edges are
    # canonicalised the way CustomNetwork does: undirected, de-duplicated, self-loops dropped).
    net_path = ds_dir / "network.json"
    if not net_path.exists():
        rec(ds, "N1_world_provenance", None, "no network.json")
    else:
        try:
            net = json.loads(net_path.read_text(encoding="utf-8"))
            nodes = net.get("nodes") or []
            edges = net.get("edges") or []
            problems = []
            if not isinstance(nodes, list) or len(nodes) < 1:
                problems.append("nodes missing/empty")
            n = len(nodes)
            bad_ep = 0
            for e in edges:
                try:
                    a, b = int(e[0]), int(e[1])
                except (TypeError, ValueError, IndexError):
                    bad_ep += 1
                    continue
                if not (0 <= a < n and 0 <= b < n):
                    bad_ep += 1
            if bad_ep:
                problems.append(f"{bad_ep} edges with out-of-range endpoints")
            cnet_raw = cfg.get("custom_network")
            if cfg.get("road_network") == "custom" and str(cnet_raw or "").strip():
                try:
                    doc = json.loads(cnet_raw) if isinstance(cnet_raw, str) else cnet_raw
                    cnodes, cedges = doc.get("nodes"), doc.get("edges")
                    if not (isinstance(cnodes, list) and isinstance(cedges, list)):
                        problems.append("custom_network missing nodes/edges")
                    else:
                        if len(cnodes) != n:
                            problems.append(f"custom nodes {len(cnodes)} != network.json {n}")
                        canon = {(min(int(x[0]), int(x[1])), max(int(x[0]), int(x[1])))
                                 for x in cedges if int(x[0]) != int(x[1])}
                        if len(canon) != len(edges):
                            problems.append(f"custom edges {len(canon)} != network.json {len(edges)}")
                except Exception as ce:
                    problems.append(f"custom_network parse: {type(ce).__name__}: {ce}")
            rec(ds, "N1_world_provenance", not problems,
                f"nodes={len(nodes)} edges={len(edges)}" + ("; " + "; ".join(problems[:4]) if problems else ""))
        except Exception as e:
            rec(ds, "N1_world_provenance", False, f"network.json error: {type(e).__name__}: {e}")

    # ============ VRU / DENM: LEAKAGE + CONSISTENCY (all SKIP-graceful) ============
    # The VRU-actor (station_type / is_vru_declared), VRU-impersonation, and DENM (event-message)
    # layers are opt-in; a dataset predating them has none of these files and every check below
    # SKIPs so pre-feature corpora stay green.
    gt_denm = read_jsonl(gt / "gt_denm_emissions.jsonl")
    ma_denm = read_jsonl(ma / "ma_denm_log.jsonl")
    veh_is_vru = {r["true_vehicle_id"]: r.get("is_vru") for r in gt_vehicle if "is_vru" in r}

    # VRU1: the ORACLE VRU/DENM labels -- is_vru (gt_vehicle) and is_fake (gt_denm_emissions) --
    # must NEVER reach the MA side or a feature table; only the MA-visible *declarations*
    # (station_type / is_vru_declared) may. Reuses is_forbidden_feature_key, and -- unlike L1/L2 --
    # also sweeps ma/ma_denm_log.jsonl (and any other ma/*.jsonl), which the original MA-leakage
    # check never inspected. FAIL on any forbidden key or ORACLE-visibility record on the MA/feature
    # side. Applicable whenever ANY VRU/DENM artefact is present.
    vru_feat_cols = set()
    for name in FEATURE_FILES:
        cols, _ = read_csv(ml / f"{name}.csv")
        vru_feat_cols |= {c for c in cols
                          if c in ("is_vru_declared", "n_denms_sent", "n_denms_implausible")}
    vru_active = bool(veh_is_vru) or bool(gt_denm) or bool(ma_denm) or bool(vru_feat_cols)
    if not vru_active:
        rec(ds, "VRU1_vru_denm_no_oracle_leak", None, "no VRU/DENM feature in dataset")
    else:
        vleaks = []
        ma_files = sorted(ma.glob("*.jsonl")) if ma.exists() else []
        for fp in ma_files:
            for i, r in enumerate(read_jsonl(fp)):
                if isinstance(r, dict) and r.get("_visibility") == ORACLE:
                    vleaks.append(f"{fp.name}[{i}] ORACLE visibility")
                bad = [k for k in _all_keys(r) if is_forbidden_feature_key(k)]
                if bad:
                    vleaks.append(f"{fp.name}[{i}] keys {bad}")
        for name in FEATURE_FILES:
            cols, _ = read_csv(ml / f"{name}.csv")
            bad = [c for c in cols if is_forbidden_feature_key(c)]
            if bad:
                vleaks.append(f"ml/{name} cols {bad}")
        rec(ds, "VRU1_vru_denm_no_oracle_leak", not vleaks, "; ".join(vleaks[:6]))

    # VRU2: if VruImpersonation was configured, (a) gt marks those vehicles attackers (they belong to
    # the identity family) and (b) every cert that DECLARED station_type="vru" resolves to a genuine
    # VRU (gt is_vru) or an attacker -- never a benign, non-VRU, non-attacker. A benign non-VRU that
    # declared "vru" would betray a mislabelled/leaked declaration. Unresolved digests are R3's remit.
    def _cfg_uses(cfg_, name):
        if str(cfg_.get("attack_type") or "") == name:
            return True
        if name in str(cfg_.get("attack_mix") or ""):
            return True
        ats = cfg_.get("attack_types")
        return isinstance(ats, (list, tuple)) and name in ats
    if not _cfg_uses(cfg, "VruImpersonation"):
        rec(ds, "VRU2_impersonation_sanity", None, "no VruImpersonation configured")
    elif not (gt_vehicle and digest2true):
        rec(ds, "VRU2_impersonation_sanity", None, "no gt_vehicle/identity_map")
    else:
        imp_veh = {a.get("true_vehicle_id") for a in gt_attacks
                   if a.get("attack_type") == "VruImpersonation"}
        imp_not_attacker = sorted(v for v in imp_veh if v and not veh_attacker.get(v))
        vru_digests = {r.get("cert_digest") for r in ma_status if r.get("station_type") == "vru"}
        vru_digests |= {r.get("subject_cert_digest") for r in ma_reports
                        if r.get("station_type") == "vru"}
        bad_decl = []
        for dg in vru_digests - {None}:
            tid = digest2true.get(dg)
            if tid is None:
                continue
            if not (veh_is_vru.get(tid) or veh_attacker.get(tid)):
                bad_decl.append(tid)
        rec(ds, "VRU2_impersonation_sanity",
            (not imp_not_attacker) and (not bad_decl),
            f"imp_vehicles={len(imp_veh)} imp_not_attacker={len(imp_not_attacker)} "
            f"vru_declaring_certs={len(vru_digests - {None})} "
            f"benign_nonvru_declared_vru={len(bad_decl)}")

    # DENM1: the MA DENM log is well-formed evidence -- each row carries an event type, a sender
    # handle (cert_digest) and a denm id, holds NO oracle flag (is_fake / true id / attack label),
    # and -- when the oracle emissions exist -- its observed denm ids reconcile (are a subset) with
    # them. SKIP when no DENM layer is present.
    if not ma_denm and not gt_denm:
        rec(ds, "DENM1_denm_log_integrity", None, "no DENM layer")
    elif not ma_denm:
        rec(ds, "DENM1_denm_log_integrity", None, "no ma_denm_log")
    else:
        dproblems = []
        malformed = 0
        for r in ma_denm:
            if not isinstance(r, dict):
                malformed += 1
                continue
            has_event = bool(r.get("event_type") or r.get("msg_type"))
            has_sender = bool(r.get("cert_digest") or r.get("sender") or r.get("sender_cert_digest"))
            has_id = r.get("denm_id") is not None
            if not (has_event and has_sender and has_id):
                malformed += 1
        if malformed:
            dproblems.append(f"{malformed} malformed rows")
        leak_keys = sorted({k for r in ma_denm if isinstance(r, dict)
                            for k in _all_keys(r) if is_forbidden_feature_key(k)})
        if leak_keys:
            dproblems.append(f"oracle keys {leak_keys}")
        if gt_denm:
            ma_ids = {r.get("denm_id") for r in ma_denm if isinstance(r, dict)} - {None}
            gt_ids = {r.get("denm_id") for r in gt_denm if isinstance(r, dict)} - {None}
            extra = ma_ids - gt_ids
            if extra:
                dproblems.append(f"{len(extra)} ma denm ids absent from emissions")
        rec(ds, "DENM1_denm_log_integrity", not dproblems,
            f"ma_denm={len(ma_denm)} " + ("; ".join(dproblems[:4]) if dproblems else "clean"))

    # DENM2: when a DENM layer is active (denm_rate>0 or a DENM attack such as FakeHazard),
    # gt_denm_emissions exists, is_fake is boolean, and every FAKE DENM was sent by a gt attacker
    # (attacker set = gt_vehicle.is_attacker | gt_attacks). The converse (benign DENM => non-attacker)
    # is intentionally NOT asserted: an attacker vehicle legitimately emits genuine DENMs outside its
    # attack window, so that direction would false-FAIL a valid dataset.
    denm_rate = cfg.get("denm_rate")
    try:
        denm_rate = float(denm_rate) if denm_rate not in (None, "") else 0.0
    except (TypeError, ValueError):
        denm_rate = 0.0
    if denm_rate <= 0 and not _cfg_uses(cfg, "FakeHazard"):
        rec(ds, "DENM2_fake_denm_label_consistency", None, "no DENM layer configured")
    elif not gt_denm:
        rec(ds, "DENM2_fake_denm_label_consistency",
            None if not ma_denm else False,
            "no gt_denm_emissions" + ("" if not ma_denm else " but ma_denm_log present"))
    else:
        non_bool = sum(1 for r in gt_denm if not isinstance(r.get("is_fake"), bool))
        fake_from_nonattacker = [r.get("denm_id") for r in gt_denm
                                 if r.get("is_fake") and r.get("true_vehicle_id") is not None
                                 and r.get("true_vehicle_id") not in attackers]
        rec(ds, "DENM2_fake_denm_label_consistency",
            non_bool == 0 and not fake_from_nonattacker,
            f"emissions={len(gt_denm)} non_bool_is_fake={non_bool} "
            f"fake_from_nonattacker={len(fake_from_nonattacker)}")

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

def main(argv=None):
    import argparse
    ap = argparse.ArgumentParser(description="Full data-correctness audit over datasets on disk.")
    ap.add_argument("--datasets-root", default=str(ROOT / "datasets"),
                    help="root directory to scan for datasets (default: ./datasets)")
    ap.add_argument("--recursive", action="store_true",
                    help="descend into nested corpora (default: immediate child datasets only)")
    args = ap.parse_args(argv)
    dsroot = Path(args.datasets_root)
    targets = _find_datasets(dsroot, args.recursive)
    run_audit(dsroot, recursive=args.recursive)

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
