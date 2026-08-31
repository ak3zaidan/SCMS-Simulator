"""The conformance suite (PLUGIN-ARCHITECTURE.md section 5), versioned WITH the interface.

`"passed conformance"` has to be a checkable statement about a SPECIFIC contract, so the suite is
shipped as importable base classes under a version package (`v1` speaks `ChannelModel/1.x`) rather
than as a floating pile of tests. Three delivery routes, one implementation behind all of them:

1. **Subclass it in your own test suite** -- `class TestRayleigh(ChannelModelContract)`, set `REF`
   (or override `make()`), and pytest collects C1-C12 as twelve real tests.
2. **Run it from the CLI** -- `scms-poc conformance --slot channel_model --ref myorg.radio:Rayleigh`,
   which needs no pytest and writes a `conformance_report.json`.
3. **Embed the report** -- that summary is what belongs in
   `manifest["plugins"]["loaded"][*]["conformance"]`, which turns *"this dataset was produced by a
   conformant plugin"* into a machine-checkable property of the artifact.

Prior art, and the specific thing taken from each. **Django**: third-party database backends run
Django's OWN suite and declare what they legitimately cannot pass as DATA
(`DatabaseFeatures.django_test_skips`) rather than by editing the suite -- so :attr:`waivers` is a
mapping of check id to a WRITTEN JUSTIFICATION, and the justification lands in the report.
**pytest**: ship the harness, not just the docs. **This repo**: `tests/test_geometric_channel.py`
grades the implementation against the audited `datagen/refdata/*.json` rather than a second
transcription of the same formula -- *"which is what makes them non-tautological"*.

**No Hypothesis.** It is randomised and keeps an example database; a nondeterministic conformance
suite in a determinism project is self-defeating. Every sequence here comes from our own seeded
`random.Random`, and the whole suite is a pure function of :attr:`ChannelModelContract.SEED`.
"""
from __future__ import annotations

from .v1 import (CHANNEL_CHECKS, SUITE_VERSION, ChannelModelContract, CheckSkipped,
                 IoViolation)
from .runner import ConformanceReport, run_contract, run_ref

__all__ = ["CHANNEL_CHECKS", "SUITE_VERSION", "ChannelModelContract", "CheckSkipped",
           "ConformanceReport", "IoViolation", "run_contract", "run_ref"]
