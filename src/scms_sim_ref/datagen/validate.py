"""Validate a generated dataset: leakage-free MA data + revocation precision/recall.

Runs the same leakage linter used everywhere over every MA-visible row, and scores
the MA's revocations against ground truth. Returns a summary and the list of any leaks.
"""

from __future__ import annotations

import glob
import json
import os

from .leakage_linter import find_forbidden_keys


def _read(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def validate(dataset_dir: str) -> tuple[dict, list]:
    ma = os.path.join(dataset_dir, "ma")
    gt = os.path.join(dataset_dir, "ground_truth")

    ma_rows = 0
    leaks: list = []
    for f in sorted(glob.glob(os.path.join(ma, "*.jsonl"))):
        for row in _read(f):
            ma_rows += 1
            hits = find_forbidden_keys(row)
            if hits:
                leaks.append({"file": os.path.basename(f), "keys": hits})

    vehicles = _read(os.path.join(gt, "gt_vehicle.jsonl"))
    attackers = {v["true_vehicle_id"] for v in vehicles if v.get("is_attacker")}
    faulty = {v["true_vehicle_id"] for v in vehicles if v.get("is_faulty")}
    rev_rows = _read(os.path.join(gt, "gt_linkage_revocation.jsonl"))
    revoked = {r["true_vehicle_id"] for r in rev_rows}
    tp = attackers & revoked

    # detection latency: time from attack ONSET (first falsified message) to revocation, for
    # correctly-revoked attackers. The operational metric for a global MA (how fast is a real
    # attack caught?), reported alongside precision/recall.
    onset = {a["true_vehicle_id"]: a.get("attack_onset_time")
             for a in _read(os.path.join(gt, "gt_attacks.jsonl"))}
    rev_time = {r["true_vehicle_id"]: r.get("true_revocation_time") for r in rev_rows}
    lat = []
    for v in tp:
        o, rt = onset.get(v), rev_time.get(v)
        if o is not None and rt is not None and rt >= o:
            lat.append(float(rt) - float(o))
    lat.sort()
    latency = {}
    if lat:
        latency = {"n": len(lat), "median_s": round(lat[len(lat) // 2], 2),
                   "p90_s": round(lat[min(len(lat) - 1, int(0.9 * len(lat)))], 2),
                   "max_s": round(lat[-1], 2)}
    # A revoked faulty vehicle is a defensible removal of a malfunctioning unit, not the same as
    # revoking an innocent benign vehicle — report the two separately.
    faulty_rev = (revoked & faulty) - attackers
    benign_fp = revoked - attackers - faulty
    precision = len(tp) / len(revoked) if revoked else 1.0
    recall = len(tp) / len(attackers) if attackers else 0.0

    # per-family recall: which attack families the MA actually catches (the dataset's difficulty
    # profile). Families that evade -> the hard, research-relevant cases.
    from .featurize import _ATTACK_FAMILY
    gt_atk_rows = _read(os.path.join(gt, "gt_attacks.jsonl"))
    fam_of = {a["true_vehicle_id"]: _ATTACK_FAMILY.get(str(a.get("attack_type", "")).split("/")[0], "other")
              for a in gt_atk_rows}
    type_of = {a["true_vehicle_id"]: str(a.get("attack_type", "")).split("/")[0] for a in gt_atk_rows}
    fam_tot: dict[str, int] = {}
    fam_caught: dict[str, int] = {}
    typ_tot: dict[str, int] = {}
    typ_caught: dict[str, int] = {}
    for v in attackers:
        f = fam_of.get(v, "other")
        fam_tot[f] = fam_tot.get(f, 0) + 1
        fam_caught[f] = fam_caught.get(f, 0) + (1 if v in revoked else 0)
        ty = type_of.get(v, "?")
        typ_tot[ty] = typ_tot.get(ty, 0) + 1
        typ_caught[ty] = typ_caught.get(ty, 0) + (1 if v in revoked else 0)
    recall_by_family = {f: round(fam_caught[f] / fam_tot[f], 3) for f in sorted(fam_tot)}
    # finest difficulty signal: which specific attack TYPES evade (e.g. SlowDrift vs Teleport)
    recall_by_type = {ty: {"recall": round(typ_caught[ty] / typ_tot[ty], 3), "n": typ_tot[ty]}
                      for ty in sorted(typ_tot)}

    # per-detector reliability: of the reports citing each reason code, what fraction actually target
    # a true attacker (report-level precision) and how many reports it fires on (coverage). Shows
    # which of the detectors carry trustworthy signal vs which are noisy / gameable by false reports.
    idmap = _read(os.path.join(gt, "gt_identity_map.jsonl"))
    d2v = {m.get("pseudonym_cert_digest"): m.get("true_vehicle_id") for m in idmap}
    det_hits: dict[str, int] = {}
    det_true: dict[str, int] = {}
    for r in _read(os.path.join(ma, "ma_reports.jsonl")):
        subj_atk = d2v.get(r.get("subject_cert_digest")) in attackers
        for code in r.get("reason_codes", []):
            det_hits[code] = det_hits.get(code, 0) + 1
            det_true[code] = det_true.get(code, 0) + (1 if subj_atk else 0)
    detector_reliability = {c: {"reports": det_hits[c], "precision": round(det_true[c] / det_hits[c], 3)}
                            for c in sorted(det_hits)}

    summary = {
        "dataset_dir": dataset_dir,
        "ma_rows": ma_rows,
        "leakage_violations": len(leaks),
        "vehicles": len(vehicles),
        "attackers": len(attackers),
        "faulty": len(faulty),
        "revoked": len(revoked),
        "true_attacker_revocations": len(tp),
        "faulty_revocations": len(faulty_rev),
        "false_revocations": len(benign_fp),
        "precision": round(precision, 3),
        "recall": round(recall, 3),
        "recall_by_family": recall_by_family,
        "recall_by_type": recall_by_type,
        "detector_reliability": detector_reliability,
        "detection_latency_s": latency,
    }
    return summary, leaks


def main(argv: list[str] | None = None) -> int:
    import argparse
    p = argparse.ArgumentParser(description="Validate a generated SCMS dataset.")
    p.add_argument("dataset_dir")
    args = p.parse_args(argv)
    summary, leaks = validate(args.dataset_dir)
    print(json.dumps(summary, indent=2))
    if leaks:
        print("LEAKAGE DETECTED:", json.dumps(leaks, indent=2))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
