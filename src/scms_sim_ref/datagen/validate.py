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
    revoked = {r["true_vehicle_id"] for r in _read(os.path.join(gt, "gt_linkage_revocation.jsonl"))}
    tp = attackers & revoked
    # A revoked faulty vehicle is a defensible removal of a malfunctioning unit, not the same as
    # revoking an innocent benign vehicle — report the two separately.
    faulty_rev = (revoked & faulty) - attackers
    benign_fp = revoked - attackers - faulty
    precision = len(tp) / len(revoked) if revoked else 1.0
    recall = len(tp) / len(attackers) if attackers else 0.0

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
