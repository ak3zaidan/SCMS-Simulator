"""Conformance suite **v1** -- graded against `ChannelModel/1.x`.

Versioned WITH the interface on purpose: *"passed conformance"* is only a meaningful claim if it
names the contract it was graded against, which is why the suite id (`v1`) is recorded in every
report and in `manifest["plugins"]["loaded"][*]["conformance"]["suite"]`.
"""
from __future__ import annotations

from .channel import CHANNEL_CHECKS, SUITE_VERSION, ChannelModelContract, CheckSkipped
from .harness import DrawCounter, IoViolation, audit_guard, build_frames, build_ladder, trace
from .protocol import (PROTOCOL_CHECKS, REPORT_CHECKS, ProtocolProfileContract,
                       ReportFormatContract)

__all__ = ["CHANNEL_CHECKS", "PROTOCOL_CHECKS", "REPORT_CHECKS", "SUITE_VERSION",
           "ChannelModelContract", "CheckSkipped", "DrawCounter", "IoViolation",
           "ProtocolProfileContract", "ReportFormatContract",
           "audit_guard", "build_frames", "build_ladder", "trace"]
