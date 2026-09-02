"""Requirement 1: butterfly-derived pseudonyms, unlinkable under the scheme's own criterion.

The criterion is CAMP SCP1's: **the PCA cannot link two certificates it issued to the same
device**, and a passive observer cannot either. The measurement is an adjusted Rand index of an
adversary's partition against the true device partition, with two mandatory positive controls so a
score of ~0 means "the adversary failed", not "the harness is blind".

Read the negative results together with `test_the_harness_is_not_blind`. Alone they prove nothing.
"""

from __future__ import annotations

import pytest

from scms_sim_ref.scms_core import unlinkability as U
from scms_sim_ref.scms_core.linkage import DeviceLinkageContext
from scms_sim_ref.scms_core.provisioning import ScmsProvisioning, seed_derive

N_DEVICES = 24
N_CERTS = 8
#: The threshold used throughout. ARI is chance-corrected, so a sound scheme sits at ~0 and
#: sampling noise on 192 certificates is well inside this.
CHANCE = 0.05


def _raw_windows(vid: int) -> list:
    """PER-DEVICE windows, exactly as run.py builds them: vf = spawn_time + k*rotate_period_s."""
    spawn = float(vid * 3)
    return [(k, (vid + k) % 20, spawn + 60.0 * k, spawn + 60.0 * (k + 1)) for k in range(N_CERTS)]


def _build(seed: int, align: bool) -> dict:
    d = seed_derive(seed)
    prov = ScmsProvisioning(d)
    devices = []
    for vid in range(N_DEVICES):
        ctx = DeviceLinkageContext(0x0001, 0x0002,
                                   d(f"ls1:{vid}", 16), d(f"ls2:{vid}", 16))
        w = _raw_windows(vid)
        devices.append((f"veh_{vid:03d}", ctx, U.align_windows(w, 60.0) if align else w))
    fleet = prov.provision_fleet(devices)
    creds = {label: dev.credentials for label, dev in fleet.items()}
    # The PCA's ledger carries no device column; attach the truth AFTER the fact, for scoring only.
    truth = {}
    for label, cl in creds.items():
        for c in cl:
            truth[c.digest] = label
    pca_rows = []
    for row in U.pca_view(prov.pca):
        r = dict(row)
        r["_device"] = truth[r["cert_digest"]]
        pca_rows.append(r)
    return {"prov": prov, "creds": creds, "pca_rows": pca_rows,
            "observer_rows": U.observer_view(creds), "derive": d,
            "devices": [d0 for d0, _c, _w in devices]}


@pytest.fixture(scope="module")
def world():
    """One fleet, provisioned as a single cross-device batch, plus every role's view of it."""
    return _build(20260901, align=False)


@pytest.fixture(scope="module")
def aligned_world():
    """The same fleet with period-aligned validity windows — the mitigation, measured."""
    return _build(20260901, align=True)


# ================================================= the controls: the harness can see linkage ==== #

def test_the_harness_is_not_blind(world):
    """POSITIVE CONTROL 1 — the RA holds `token -> device` and must link perfectly. If this ever
    drops, every negative result in this file is worthless."""
    rows = world["pca_rows"]
    pred = U.ra_oracle_adversary(rows, world["prov"].ra.token_map)
    rep = U.score("ra_private_table", "ra_oracle", rows, pred)
    assert rep.ari == pytest.approx(1.0), rep.as_dict()


def test_the_current_label_scheme_is_linked_outright(world):
    """POSITIVE CONTROL 2, and the baseline this whole task exists to replace.

    `run.py:4008` records `pca.issue(dig, req_hash, ...)` with the SAME `req_hash` for every
    pseudonym of a vehicle. The PCA's own table is therefore a complete device map, and the
    simplest adversary there is — equality of one field — recovers it exactly.
    """
    rows = U.legacy_label_view(world["devices"], N_CERTS, world["derive"])
    pred = U.shared_field_adversary(rows, "request_hash")
    rep = U.score("legacy_pca_ledger", "shared_field[request_hash]", rows, pred)
    assert rep.ari == pytest.approx(1.0), rep.as_dict()
    # and the automatic search finds it without being told which field to look at
    name, best = U.best_field_adversary(rows)
    assert name == "request_hash"
    assert U.adjusted_rand_index([r["_device"] for r in rows], best) == pytest.approx(1.0)


# ================================================= the claim: butterfly, PCA view =============== #

def test_no_single_field_of_the_pca_view_links_a_device(world):
    """THE CRITERION. Every field the PCA sees, tried; the best of them must be at chance."""
    rows = world["pca_rows"]
    name, pred = U.best_field_adversary(rows)
    rep = U.score("butterfly_pca_ledger", f"best_field[{name}]", rows, pred)
    assert rep.ari < CHANCE, rep.as_dict()


def test_the_pca_ledger_contains_no_device_identifier_at_all(world):
    """Structural, not statistical: the strongest form of the claim is that the identifier is not
    there to mine. `request_token` is per-(device, i, j) and only the RA can invert it."""
    rows = world["pca_rows"]
    tokens = {r["request_token"] for r in rows}
    assert len(tokens) == len(rows)                      # unique per certificate, never per device
    visible = set(rows[0]) - {"_device"}
    assert "request_hash" not in visible and "device" not in visible and "vid" not in visible


def test_certified_keys_carry_no_structure_a_distinguisher_can_use(world):
    """The continuous form of the game: AUC of a byte-similarity distinguisher over the certified
    public keys. 0.5 is no advantage."""
    rows = world["pca_rows"]
    truth = [r["_device"] for r in rows]
    auc = U.pairwise_auc(U.key_similarity, rows, truth)
    assert abs(auc - 0.5) < 0.05, auc


def test_key_prefix_clustering_finds_nothing(world):
    rows = world["pca_rows"]
    rep = U.score("butterfly_pca_ledger", "key_prefix",
                  rows, U.key_prefix_adversary(rows))
    assert rep.ari < CHANCE, rep.as_dict()


def test_the_ra_never_sees_a_certified_key(world):
    """SCP1's other half: the RA computes cocoons but the PCA's secret `c` moves the key somewhere
    the RA cannot follow, so the RA's cocoon set and the issued-key set are disjoint."""
    from scms_sim_ref.scms_core import butterfly as bf
    from scms_sim_ref.scms_core.ecdsa_p256 import compress_point
    prov, creds, d = world["prov"], world["creds"], world["derive"]
    issued = {c.certificate.to_be_signed.verify_key_indicator
              for cl in creds.values() for c in cl}
    cocoons = set()
    for vid in range(N_DEVICES):
        ctx = DeviceLinkageContext(0x0001, 0x0002, d(f"ls1:{vid}", 16), d(f"ls2:{vid}", 16))
        cat = prov.ra.enrol(f"veh_{vid:03d}", ctx).caterpillar    # the RA's whole view
        for (i, j, _vf, _vt) in _raw_windows(vid):
            B, _Q = bf.ra_cocoon_keys(cat.A, cat.P, cat.ck, cat.ek, i, j)
            cocoons.add(compress_point(B))
    assert len(issued) == len(cocoons) == N_DEVICES * N_CERTS
    assert not (issued & cocoons)


# ================================================= the observer view ============================ #

def test_a_passive_observer_cannot_link_by_key_or_linkage_value(world):
    """Everything on the wire except the validity window — which is the next test, and is the one
    that fails."""
    rows = world["observer_rows"]
    for field in ("certified_key", "linkage_value", "cert_digest", "i_cert"):
        rep = U.score("observer", f"shared_field[{field}]", rows,
                      U.shared_field_adversary(rows, field))
        assert rep.ari < CHANCE, rep.as_dict()


# ================================================= the finding, and the fix ===================== #

def test_per_device_validity_windows_leak_the_link_that_butterfly_closed(world):
    """**The finding.** Butterfly makes the keys unlinkable and does nothing about the windows.

    `run.py` derives `valid_from` from the vehicle's own `spawn_time`, so a device's consecutive
    pseudonyms abut exactly: `valid_to(k) == valid_from(k+1)`. Chaining on that recovers the device
    partition completely, through certificates whose key material is provably unlinkable. Wiring
    butterfly WITHOUT fixing the windows would have produced an unlinkability claim that is false.
    """
    rows = world["observer_rows"]
    rep = U.score("observer", "temporal_chaining", rows, U.temporal_chaining_adversary(rows))
    # Not exactly 1.0: two devices whose windows happen to abut get merged, so 24 devices land in
    # 20 clusters. That is the adversary being slightly over-eager, not the leak being partial --
    # every device's own chain is recovered whole.
    assert rep.ari > 0.75, rep.as_dict()
    assert rep.linked and rep.n_clusters < rep.n_devices


def test_period_aligned_windows_close_it(aligned_world):
    """**The fix**, which is just what the real SCMS does: i-periods are a shared calendar grid, so
    every device's period-i certificate carries the same window and there is nothing to chain."""
    rows = aligned_world["observer_rows"]
    chained = U.score("observer_aligned", "temporal_chaining", rows,
                      U.temporal_chaining_adversary(rows))
    assert chained.ari < CHANCE, chained.as_dict()
    name, pred = U.best_field_adversary(rows)
    best = U.score("observer_aligned", f"best_field[{name}]", rows, pred)
    assert best.ari < CHANCE, best.as_dict()


def test_align_windows_keeps_the_linkage_period_and_the_validity_period_in_step():
    """They are the same period in CAMP SCP2. If `align_windows` let them drift, a CRL entry for
    period i would stop matching the certificate that claims period i."""
    raw = [(0, 3, 5.0, 65.0), (1, 4, 65.0, 125.0), (2, 5, 125.0, 185.0)]
    out = U.align_windows(raw, 60.0)
    assert [i for i, _j, _f, _t in out] == [0, 1, 2]
    assert [(f, t) for _i, _j, f, t in out] == [(0.0, 60.0), (60.0, 120.0), (120.0, 180.0)]
    assert [j for _i, j, _f, _t in out] == [3, 4, 5]      # j is untouched


# ================================================= identity resolution still works ============== #

def test_unlinkable_does_not_mean_uninvestigable(world):
    """The property that makes the whole thing usable for THIS project: the MA can still resolve a
    misbehaving pseudonym to a device, via the RA, exactly as `ra.bind(request_hash, true_id)` does
    today. Unlinkability that also defeated investigation would be a regression, not a feature."""
    prov, rows = world["prov"], world["pca_rows"]
    resolved = 0
    for r in rows:
        token = bytes.fromhex(r["request_token"])
        got = prov.ra.resolve(token)
        assert got is not None
        assert got[0] == r["_device"]
        resolved += 1
    assert resolved == N_DEVICES * N_CERTS


def test_audit_reports_are_self_describing(world, aligned_world):
    """`audit()` must carry its own interpretation, so a number cannot be quoted without it — and
    the full audit is what turns "butterfly is wired in" into a defensible sentence.

    Per-device windows: exactly TWO adversaries link — the RA oracle (by design) and temporal
    chaining (the defect). Aligned windows: only the RA oracle.
    """
    per_device = U.audit(world["pca_rows"], "butterfly_pca_ledger",
                         token_map=world["prov"].ra.token_map)
    assert all(r.expectation for r in per_device)
    assert {r.adversary for r in per_device if r.linked} == {"ra_oracle", "temporal_chaining"}

    aligned = U.audit(aligned_world["pca_rows"], "butterfly_pca_ledger_aligned",
                      token_map=aligned_world["prov"].ra.token_map)
    assert aligned[0].adversary == "ra_oracle" and aligned[0].ari == pytest.approx(1.0)
    assert {r.adversary for r in aligned if r.linked} == {"ra_oracle"}


# ================================================= the metric itself ============================ #

def test_adjusted_rand_index_edges():
    assert U.adjusted_rand_index([0, 0, 1, 1], [0, 0, 1, 1]) == pytest.approx(1.0)
    assert U.adjusted_rand_index([0, 0, 1, 1], [1, 1, 0, 0]) == pytest.approx(1.0)
    # both degenerate guesses must score ~0, which is the whole point of chance correction
    assert abs(U.adjusted_rand_index([0, 0, 1, 1, 2, 2], [0] * 6)) < 1e-9
    assert abs(U.adjusted_rand_index([0, 0, 1, 1, 2, 2], list(range(6)))) < 1e-9


def test_pairwise_auc_edges():
    rows = [{"k": "00"}, {"k": "00"}, {"k": "ff"}, {"k": "ff"}]
    truth = ["a", "a", "b", "b"]
    perfect = U.pairwise_auc(lambda r1, r2: 1.0 if r1["k"] == r2["k"] else 0.0, rows, truth)
    assert perfect == pytest.approx(1.0)
    blind = U.pairwise_auc(lambda r1, r2: 0.0, rows, truth)
    assert blind == pytest.approx(0.5)
