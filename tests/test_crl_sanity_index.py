"""The end-of-run CRL linkage self-check: same invariant, indexed instead of quadratic.

`docs/realism/LONG-RUNS.md` section 1.2 measured the old form -- `for vid in revoked_vehicles: for d
in cert_first_seen: assert any(e.matches(...) for e in crl_entries)` -- as R(R+1)/2 `matches` calls
at 8.7-10.3 us each, i.e. ~4.5 us x R^2, which was 70% of a 1,715 s finalisation on an 8-hour run and
the reason finalisation's scaling exponent was 1.6 rather than 1.0.

The check now asserts that the CRL entry the MA appended FOR THAT DEVICE recomputes the pseudonym's
linkage value. These tests pin the two properties that makes it safe to rely on:

* it is **not weaker** -- on a real run the indexed verdict and the old whole-CRL scan agree, cert
  for cert (`SCMS_CRL_AUDIT=exhaustive` runs both and asserts it, and the run still reproduces the
  pinned reference digest);
* it is **not vacuous** -- it fires on a real body of certificates, and an entry belonging to a
  different device does not satisfy it.
"""

import hashlib
import os

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import run as runmod
from scms_sim_ref.scms_core.linkage import CrlLinkageEntry, DeviceLinkageContext


#: The repository's pinned reference run, verbatim (docs/realism/PROGRESS.md).
REFERENCE_DIGEST = "b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815"


def _reference_cfg(out_dir, duration_s=300.0, **kw):
    return PipelineConfig(seed=42, traffic_flow=True, road_network="grid", duration_s=duration_s,
                          arrival_rate=2.0, grid_w=6, grid_h=6, attacker_pct=0.15,
                          traffic_lights=True, out_dir=out_dir, **kw)


# --------------------------------------------------------------------------- #
# It has teeth: the entry that matches is the target device's own, not just any entry.
# --------------------------------------------------------------------------- #
def test_a_crl_entry_does_not_match_another_devices_pseudonym():
    """The indexed assert would be worthless if any entry matched any certificate."""
    a = DeviceLinkageContext(0x0001, 0x0002, hashlib.sha256(b"A1").digest()[:16],
                             hashlib.sha256(b"A2").digest()[:16])
    b = DeviceLinkageContext(0x0001, 0x0002, hashlib.sha256(b"B1").digest()[:16],
                             hashlib.sha256(b"B2").digest()[:16])
    ea = CrlLinkageEntry.from_device(a, i=0, jmax=20)
    for j in range(20):
        assert ea.matches(0, j, a.linkage_value_for(0, j)), "own pseudonym must be revoked"
        assert not ea.matches(0, j, b.linkage_value_for(0, j)), "another device must NOT be"


# --------------------------------------------------------------------------- #
# It is not weaker: indexed and exhaustive agree on a real run, which still reproduces the golden.
# --------------------------------------------------------------------------- #
def test_indexed_check_agrees_with_the_exhaustive_scan_on_the_reference_run(tmp_path):
    """`CRL_AUDIT_EXHAUSTIVE` runs BOTH forms and asserts they agree for every observed cert.

    Passing here is the equivalence proof on real data: 152 revocations, 623 certificates and the
    pinned reference digest unchanged.
    """
    runmod.CRL_AUDIT_EXHAUSTIVE = True
    try:
        r = run_pipeline(_reference_cfg(str(tmp_path / "audit")))
    finally:
        runmod.CRL_AUDIT_EXHAUSTIVE = False
    assert r.data_digest == REFERENCE_DIGEST, r.data_digest
    assert r.counts["crl_linkage_checks"] > 100, r.counts


def test_environment_variable_arms_the_exhaustive_audit(tmp_path, monkeypatch):
    """The audit is reachable without importing the module, for a probe running the CLI."""
    monkeypatch.setenv("SCMS_CRL_AUDIT", "exhaustive")
    r = run_pipeline(_reference_cfg(str(tmp_path / "env"), duration_s=120.0))
    assert r.counts["crl_linkage_checks"] > 0
    assert os.environ["SCMS_CRL_AUDIT"] == "exhaustive"


# --------------------------------------------------------------------------- #
# It is not vacuous, and it is LINEAR: one `matches` call per observed cert of a revoked device.
# --------------------------------------------------------------------------- #
def test_check_covers_every_observed_pseudonym_of_every_revoked_device(tmp_path):
    """The number of certificates checked is exactly the number the invariant is about.

    `ma_cert_status.jsonl` carries one row per certificate the MA ever observed, with
    `crl_status = revoked` for the ones belonging to a revoked device. That count must equal the
    number of certificates the self-check re-derived from the CRL.
    """
    import json
    out = str(tmp_path / "cover")
    r = run_pipeline(_reference_cfg(out))
    revoked_rows = sum(1 for ln in open(os.path.join(out, "ma", "ma_cert_status.jsonl"),
                                        encoding="utf-8")
                       if ln.strip() and json.loads(ln)["crl_status"] == "revoked")
    assert revoked_rows > 0
    assert r.counts["crl_linkage_checks"] == revoked_rows


@pytest.mark.parametrize("duration", [150.0, 300.0])
def test_matches_calls_are_linear_not_quadratic(tmp_path, duration, monkeypatch):
    """Count real `CrlLinkageEntry.matches` calls; the default path makes them ONLY here.

    The old form made R(R+1)/2 of them. The indexed form makes exactly one per observed pseudonym of
    a revoked device, so the count must equal `crl_linkage_checks` and must stay far below the
    quadratic figure the same run would have produced.
    """
    calls = {"n": 0}
    real = CrlLinkageEntry.matches

    def counting(self, i, j, lv):
        calls["n"] += 1
        return real(self, i, j, lv)

    monkeypatch.setattr(CrlLinkageEntry, "matches", counting)
    r = run_pipeline(_reference_cfg(str(tmp_path / f"lin{int(duration)}"), duration_s=duration))
    checks = r.counts["crl_linkage_checks"]
    assert calls["n"] == checks > 0, (calls, checks)
    quadratic = r.n_revoked * (r.n_revoked + 1) // 2
    assert calls["n"] < quadratic / 4, (
        f"{calls['n']} matches calls for {r.n_revoked} revocations; the scan this replaced would "
        f"have made {quadratic}")
