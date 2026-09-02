"""Measuring pseudonym unlinkability — the scheme's own criterion, scored, not asserted.

The claim under test is CAMP SCP1's: *the PCA cannot link two certificates it issued to the same
device.* A claim like that is only worth as much as the adversary you tried, so this module is
built adversary-first:

* a **view** is what one role actually sees (the PCA's ledger, a radio observer's certificates,
  the RA's private table);
* an **adversary** is a function from a view to a partition of the certificates;
* the **score** is the adjusted Rand index of that partition against the true device partition —
  1.0 = perfect linking, 0.0 = chance, and it is chance-corrected so "one big cluster" and "all
  singletons" both score ~0 instead of looking good.

**Positive controls are mandatory here and are the reason to trust the negative results.** Two
adversaries in this module are *supposed* to score 1.0: `ra_oracle_adversary` (the RA holds the
token map by design, and misbehaviour investigation depends on it) and `shared_field_adversary`
applied to the current engine's `request_hash`. If a change ever made those score 0, the harness
would be blind and every other number in it meaningless. `test_unlinkability.py` asserts both.

## The finding this harness produced, which butterfly alone does not fix

Butterfly makes the *keys* unlinkable. It does nothing about the *validity windows*, and
`run.py`'s windows are per-device: `vf = spawn_time + k*rotate_period_s`, so consecutive pseudonyms
of one device abut exactly (`valid_to(k) == valid_from(k+1)`). `temporal_chaining_adversary` links
them at **ARI 1.0** through that channel while every key-material adversary scores ~0. The fix is
the one the real SCMS uses — i-periods are a shared calendar grid, so every device's certificates
for period i carry the *same* window — and `align_windows` implements it. Both are measured in the
task report. An unlinkability claim made without this measurement would have been wrong.
"""

from __future__ import annotations

import hashlib
import math
from collections import defaultdict
from dataclasses import dataclass
from typing import Callable, Mapping, Optional, Sequence


# ------------------------------------------------------------------------------- the metric ---- #

def adjusted_rand_index(truth: Sequence, pred: Sequence) -> float:
    """Chance-corrected agreement between two partitions. 1.0 = identical, ~0.0 = chance.

    Dependency-free (no scikit-learn in this repo's install closure) and exact: the standard
    contingency-table form, not an approximation.
    """
    if len(truth) != len(pred):
        raise ValueError("partitions must cover the same items")
    n = len(truth)
    if n < 2:
        return 1.0
    table: dict = defaultdict(int)
    a: dict = defaultdict(int)
    b: dict = defaultdict(int)
    for u, v in zip(truth, pred):
        table[(u, v)] += 1
        a[u] += 1
        b[v] += 1

    def c2(x: int) -> int:
        return x * (x - 1) // 2

    sum_ij = sum(c2(v) for v in table.values())
    sum_a = sum(c2(v) for v in a.values())
    sum_b = sum(c2(v) for v in b.values())
    total = c2(n)
    expected = sum_a * sum_b / total
    maximum = 0.5 * (sum_a + sum_b)
    if maximum == expected:
        return 1.0 if sum_ij == expected else 0.0
    return (sum_ij - expected) / (maximum - expected)


def pairwise_auc(score: Callable[[dict, dict], float], rows: Sequence[dict],
                 truth: Sequence, max_pairs: int = 200_000) -> float:
    """ROC-AUC of a pairwise *similarity* score at answering "same device?".

    This is the distinguishing game in its natural form: 0.5 is no advantage, 1.0 is a perfect
    distinguisher. Computed exactly by the Mann-Whitney form with tie correction; the pair set is
    truncated deterministically (by index order) when it would exceed `max_pairs`.
    """
    pos, neg = [], []
    stop = False
    for i in range(len(rows)):
        if stop:
            break
        for j in range(i + 1, len(rows)):
            s = score(rows[i], rows[j])
            (pos if truth[i] == truth[j] else neg).append(s)
            if len(pos) + len(neg) >= max_pairs:
                stop = True
                break
    if not pos or not neg:
        return 0.5
    order = sorted(range(len(pos) + len(neg)), key=lambda k: (pos + neg)[k])
    allv = pos + neg
    ranks = [0.0] * len(allv)
    k = 0
    while k < len(order):
        m = k
        while m + 1 < len(order) and allv[order[m + 1]] == allv[order[k]]:
            m += 1
        r = (k + m) / 2.0 + 1.0
        for q in range(k, m + 1):
            ranks[order[q]] = r
        k = m + 1
    rsum = sum(ranks[: len(pos)])
    return (rsum - len(pos) * (len(pos) + 1) / 2.0) / (len(pos) * len(neg))


@dataclass(frozen=True, slots=True)
class LinkabilityReport:
    view: str
    adversary: str
    ari: float
    n_certificates: int
    n_devices: int
    n_clusters: int
    #: What this number means for the claim. Set by the caller, carried so a report cannot be
    #: quoted without its interpretation.
    expectation: str = ""

    @property
    def linked(self) -> bool:
        """A pragmatic threshold: anything above 0.1 is a real channel, not noise."""
        return self.ari > 0.1

    def as_dict(self) -> dict:
        return {"view": self.view, "adversary": self.adversary, "ari": round(self.ari, 6),
                "n_certificates": self.n_certificates, "n_devices": self.n_devices,
                "n_clusters": self.n_clusters, "linked": self.linked,
                "expectation": self.expectation}


# --------------------------------------------------------------------------------- the views --- #

def pca_view(pca) -> list[dict]:
    """Exactly `PseudonymCertificateAuthority.ledger` — everything the PCA saw, nothing else."""
    return [dict(r) for r in pca.ledger]


def observer_view(credentials_by_device: Mapping[str, Sequence]) -> list[dict]:
    """What a passive radio observer collects: the certificates themselves, in no useful order."""
    rows = []
    for label, creds in sorted(credentials_by_device.items()):
        for c in creds:
            cert = c.certificate
            rows.append({
                "cert_digest": cert.digest_hex(),
                "certified_key": cert.to_be_signed.verify_key_indicator.hex(),
                "linkage_value": cert.linkage.linkage_value.hex(),
                "i_cert": cert.linkage.i_cert,
                "issuer": (cert.issuer or b"").hex(),
                "valid_from": cert.validity.start,
                "valid_to": cert.validity.end,
                "crl_series": cert.to_be_signed.crl_series,
                "assurance_level": cert.to_be_signed.assurance_level,
                "_device": label,
            })
    return rows


def legacy_label_view(devices: Sequence[str], rotations: int, derive) -> list[dict]:
    """The CURRENT engine's provisioning, reconstructed for contrast.

    `run.py:4006-4008`: one Ed25519 key per `key:{vid}:{k}` label, and a `pca.issue(dig, req_hash,
    ...)` record whose `request_hash` is **the same for every pseudonym of a vehicle**. This
    function reproduces both, so the two schemes are scored by the identical adversaries.
    """
    rows = []
    for label in devices:
        req_hash = hashlib.sha256(f"req|{label}".encode()).hexdigest()[:16]
        for k in range(rotations):
            material = derive(f"key:{label}:{k}", 32)
            rows.append({
                "cert_digest": hashlib.sha256(material).hexdigest()[-16:],
                "request_hash": req_hash,
                "i_cert": k // 20,
                "j_index": k % 20,
                "_device": label,
            })
    return rows


# ---------------------------------------------------------------------------- the adversaries -- #

def shared_field_adversary(rows: Sequence[dict], field: str) -> list:
    """Partition by equality of one visible field. The simplest attack there is, and the one that
    breaks the current scheme outright when `field = "request_hash"`."""
    return [r.get(field) for r in rows]


def best_field_adversary(rows: Sequence[dict], skip: Sequence[str] = ()) -> tuple[str, list]:
    """Try EVERY visible field and keep the one that links best.

    This is what makes a negative result mean something: not "the field I thought of does not
    link", but "no single field in the view links". Fields whose values are unique per row are
    still tried — an adversary is entitled to them, they simply produce all-singletons and score ~0.
    """
    truth = [r.get("_device") for r in rows]
    best_name, best_pred, best = "", [i for i in range(len(rows))], -2.0
    skip_set = set(skip) | {"_device"}
    names = sorted({k for r in rows for k in r} - skip_set)
    for name in names:
        pred = shared_field_adversary(rows, name)
        score = adjusted_rand_index(truth, pred)
        if score > best:
            best_name, best_pred, best = name, pred, score
    return best_name, best_pred


def temporal_chaining_adversary(rows: Sequence[dict], tolerance: int = 0) -> list:
    """Chain certificates whose validity windows abut: `valid_to(A) ~= valid_from(B)`.

    A union-find over the abutment relation. This is the adversary that finds the residual channel
    `run.py`'s per-device windows leave open — see the module docstring. Deterministic: candidates
    are considered in sorted-window order, so the partition is a function of the data alone.
    """
    n = len(rows)
    parent = list(range(n))

    def find(x: int) -> int:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(x: int, y: int) -> None:
        rx, ry = find(x), find(y)
        if rx != ry:
            parent[max(rx, ry)] = min(rx, ry)

    by_start: dict = defaultdict(list)
    for idx, r in enumerate(rows):
        vf = r.get("valid_from")
        if vf is not None:
            by_start[int(vf)].append(idx)
    for idx, r in enumerate(rows):
        vt = r.get("valid_to")
        if vt is None:
            continue
        for delta in range(-tolerance, tolerance + 1):
            for other in by_start.get(int(vt) + delta, ()):
                if other != idx:
                    union(idx, other)
    return [find(i) for i in range(n)]


def key_prefix_adversary(rows: Sequence[dict], nibbles: int = 2,
                         field: str = "certified_key") -> list:
    """Cluster by a shared prefix of the public key. Looks for residual structure in the key
    material itself — the thing butterfly's pseudorandomness is supposed to destroy."""
    return [str(r.get(field, ""))[:nibbles] for r in rows]


def ra_oracle_adversary(rows: Sequence[dict], token_map: Mapping[bytes, tuple]) -> list:
    """**Positive control.** The RA holds `token -> (device, i, j)` and links perfectly.

    This is not a break; it is the SCMS design, and the same power `run.py`'s RA already has as the
    sole holder of `request_hash -> true_id`. Its purpose here is to prove the measurement can see
    linkage when linkage exists.
    """
    hexmap = {tok.hex(): dev for tok, (dev, _i, _j) in token_map.items()}
    return [hexmap.get(r.get("request_token"), f"?{n}") for n, r in enumerate(rows)]


# -------------------------------------------------------------------------------- the harness -- #

def _hamming_hex(a: str, b: str) -> float:
    ba, bb = bytes.fromhex(a), bytes.fromhex(b)
    n = min(len(ba), len(bb))
    return -sum(bin(ba[i] ^ bb[i]).count("1") for i in range(n))


def key_similarity(r1: dict, r2: dict, field: str = "certified_key") -> float:
    """Negative Hamming distance between two public keys — a continuous distinguisher for
    `pairwise_auc`. Under a sound scheme its AUC is 0.5."""
    a, b = r1.get(field), r2.get(field)
    if not a or not b:
        return 0.0
    return _hamming_hex(a, b)


def score(view_name: str, adversary_name: str, rows: Sequence[dict], pred: Sequence,
          expectation: str = "") -> LinkabilityReport:
    truth = [r.get("_device") for r in rows]
    return LinkabilityReport(view_name, adversary_name,
                             adjusted_rand_index(truth, pred), len(rows),
                             len(set(truth)), len(set(map(str, pred))), expectation)


def audit(rows: Sequence[dict], view_name: str, *,
          token_map: Optional[Mapping[bytes, tuple]] = None,
          skip_fields: Sequence[str] = ()) -> list[LinkabilityReport]:
    """Run every adversary in this module against one view. Returns reports, most linked first."""
    out = []
    name, pred = best_field_adversary(rows, skip=skip_fields)
    out.append(score(view_name, f"best_field[{name}]", rows, pred,
                     "no single visible field may group by device"))
    out.append(score(view_name, "temporal_chaining", rows,
                     temporal_chaining_adversary(rows),
                     "abutting validity windows must not chain"))
    if any("certified_key" in r for r in rows):
        out.append(score(view_name, "key_prefix", rows, key_prefix_adversary(rows),
                         "key material must carry no device structure"))
    if token_map is not None:
        out.append(score(view_name, "ra_oracle", rows,
                         ra_oracle_adversary(rows, token_map),
                         "POSITIVE CONTROL: must be 1.0, or the harness is blind"))
    return sorted(out, key=lambda r: -r.ari)


# ------------------------------------------------------------------- the mitigation, as an API -- #

def align_windows(windows: Sequence[tuple], period_s: float, *,
                  overlap_s: float = 0.0, epoch_s: float = 0.0) -> list[tuple]:
    """Snap per-device validity windows onto the **shared** i-period grid.

    The real SCMS issues certificates against a global calendar: every device's period-i
    certificate carries the same `[start, end)`, which is precisely why an observer cannot chain a
    device's certificates through their windows. `run.py` instead derives the window from the
    vehicle's own `spawn_time`, making it a per-device fingerprint (measured: ARI 1.0).

    `windows` = [(i, j, valid_from_s, valid_to_s)] in, same shape out, with `i` recomputed from the
    grid so the linkage i-period and the validity period agree — they are the same period in CAMP
    SCP2 and must not be allowed to drift apart.

    The cost is stated rather than hidden: aligned windows are *coarser*, so a vehicle's first
    certificate is valid from the start of the period it spawned in, not from its spawn. That
    widens the window in which a revoked-but-not-yet-enforced credential is usable, and it removes
    the engine's ability to expire a certificate exactly at a trip's end. Both are properties of
    the real system, not artifacts.
    """
    if period_s <= 0:
        raise ValueError("period_s must be > 0")
    out = []
    for (_i, j, vf, vt) in windows:
        idx = int(math.floor((vf - epoch_s) / period_s))
        start = epoch_s + idx * period_s
        out.append((idx, j, start, start + period_s + overlap_s))
    return out
