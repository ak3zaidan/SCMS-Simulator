"""Tests for the SCMS linkage-value engine.

These assert the three properties that make linkage-based revocation both correct
and privacy-preserving, per CAMP SCP2 / Brecht et al.
"""

import os

import pytest

from scms_sim_ref.scms_core import linkage as lk
from scms_sim_ref.scms_core.linkage import CrlLinkageEntry, DeviceLinkageContext


def _device(seed=b"\x01" * 16, seed2=b"\x02" * 16, la1=0x0001, la2=0x0002):
    return DeviceLinkageContext(la_id1=la1, la_id2=la2, ls1_0=seed, ls2_0=seed2)


def test_field_widths():
    dev = _device()
    plv1 = lk.pre_linkage_value(dev.la_id1, dev.ls1_0, 0)
    assert len(plv1) == lk.PLV_BYTES == 9
    assert len(lk.linkage_seed_next(dev.la_id1, dev.ls1_0)) == lk.LS_BYTES == 16
    assert len(dev.linkage_value_for(0, 0)) == 9


def test_two_LA_xor_reconstruction():
    """The lv in a cert is exactly plv1 XOR plv2 -- neither LA alone yields it."""
    dev = _device()
    i, j = 5, 3
    plv1 = lk.pre_linkage_value(dev.la_id1, lk.linkage_seed_at(dev.la_id1, dev.ls1_0, i), j)
    plv2 = lk.pre_linkage_value(dev.la_id2, lk.linkage_seed_at(dev.la_id2, dev.ls2_0, i), j)
    lv = dev.linkage_value_for(i, j)
    assert lv == lk.linkage_value(plv1, plv2)
    # A single LA's pre-linkage value must not equal the final linkage value.
    assert lv != plv1 and lv != plv2


def test_determinism():
    dev = _device()
    assert dev.linkage_value_for(7, 2) == dev.linkage_value_for(7, 2)


def test_distinct_across_j_and_i():
    dev = _device()
    lvs = {dev.linkage_value_for(i, j) for i in range(3) for j in range(5)}
    # 15 (i, j) slots should give 15 distinct linkage values (no trivial collisions).
    assert len(lvs) == 15


def test_crl_forward_match_and_backward_privacy():
    dev = _device()
    revoke_i = 10
    entry = CrlLinkageEntry.from_device(dev, i=revoke_i, jmax=20)

    # Revoked from period i forward (same and later periods match).
    for cert_i in (revoke_i, revoke_i + 1, revoke_i + 5):
        for j in (0, 7, 19):
            assert entry.matches(cert_i, j, dev.linkage_value_for(cert_i, j))

    # Backward privacy: certificates from BEFORE the revocation period never match.
    for cert_i in (0, revoke_i - 1):
        for j in (0, 7):
            assert not entry.matches(cert_i, j, dev.linkage_value_for(cert_i, j))


def test_crl_does_not_match_other_device():
    victim = _device()
    other = DeviceLinkageContext(la_id1=0x0001, la_id2=0x0002,
                                 ls1_0=os.urandom(16), ls2_0=os.urandom(16))
    entry = CrlLinkageEntry.from_device(victim, i=4)
    for cert_i in range(4, 9):
        for j in range(3):
            assert not entry.matches(cert_i, j, other.linkage_value_for(cert_i, j))


def test_crl_contains_helper():
    d1, d2 = _device(), DeviceLinkageContext(0x0001, 0x0002, os.urandom(16), os.urandom(16))
    crl = [CrlLinkageEntry.from_device(d1, i=2)]
    assert lk.crl_contains(crl, 3, 1, d1.linkage_value_for(3, 1))
    assert not lk.crl_contains(crl, 3, 1, d2.linkage_value_for(3, 1))


def test_out_of_range_j_rejected_by_entry():
    dev = _device()
    entry = CrlLinkageEntry.from_device(dev, i=0, jmax=20)
    assert not entry.matches(0, 20, dev.linkage_value_for(0, 0))
