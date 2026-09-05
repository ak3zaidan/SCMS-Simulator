"""`ProtocolProfile` -- ONE declaration that says which protocol a deployment speaks.

The engine already had the pieces: a `message_codec` slot with four registered codecs, EN 302 637-2
generation rules, TS 102 687 reactive DCC, an 802.11p airtime model and a security envelope. What it
did NOT have is a way to say *"this run speaks ITS-G5"* or *"this run speaks something else"* as a
single, replayable statement. Five independent booleans are not a protocol; they are five knobs that
happen to be set the way ITS-G5 wants them, and a third party wanting a different stack had to hit
every one of them from the outside and could still not change the two things that are not knobs at
all -- how long a frame is on the air, and how the congestion controller reacts.

A profile bundles the five decisions a V2X stack actually makes:

======================  ==========================================================================
:meth:`~ProtocolProfile.codec`                 what the octets look like
:meth:`~ProtocolProfile.new_generation_state`  WHEN a station transmits
:meth:`~ProtocolProfile.new_congestion_state`  how often it is then ALLOWED to
:meth:`~ProtocolProfile.wire_size_bytes`       what a frame WEIGHS, security envelope included
:meth:`~ProtocolProfile.frame_airtime_s` /
:meth:`~ProtocolProfile.channel_busy_ratio` /
:meth:`~ProtocolProfile.link_latency_s`        what that weight COSTS on the channel
======================  ==========================================================================

**The built-in is not a special case.** `codecs.profiles.EtsiItsG5Profile` is resolved through this
seam by exactly the same three-tier resolver, gets exactly the same signature check, the same
capability screening, the same provenance lock entry and the same conformance contract as a
third-party profile. `run.py` holds no `if profile is the built-in` branch and calls
`scms_sim_ref.codecs.etsi_rules` from nowhere inside the step loop -- every ETSI constant reaches the
engine through a profile method. That is the test of whether a seam is real: delete the built-in and
the engine still runs whatever profile the config names.

THE FIREWALL APPLIES HERE TOO
-----------------------------
A profile decides message TIMING and message SIZE. Both are observable by an attacker and by every
receiver, so both are legitimate MA-visible quantities -- and both would be perfect laundering
vectors if a profile could see ground truth. :class:`GenerationInput` is therefore built to the same
rule as :class:`~scms_sim_ref.api.detect.Observation` and :class:`~scms_sim_ref.api.codec.Claim`:
**it carries the station's own ego state and nothing about anybody else's truthfulness.** A profile
that transmitted more often for an attacker than for an honest station would make the message RATE a
detector for the attack -- a leak dressed as physics, and precisely what conformance check
``P3_no_oracle_influence`` exists to catch.

TRUST MODEL, STATED THE WAY THE REST OF THIS PACKAGE STATES IT
--------------------------------------------------------------
Consistent with :mod:`scms_sim_ref.api` section 10 and `PLUGIN-ARCHITECTURE.md` section 10, because a
profile is more powerful than a detector -- it is on the path of EVERY message, not only of the ones
a detector scores.

**ENFORCED at load, before step 0, mechanically:**

* the interface name/major/minor (``ProtocolProfile/1.x``) -- a 1.1 profile against a 1.0 engine is
  refused at load, not at step k;
* the METHOD SIGNATURES, by name and position (:data:`PROFILE_SPEC`) -- neither `ABC` nor `Protocol`
  does this at runtime;
* declared capabilities, screened against :data:`KNOWN_CAPABILITIES`; an unknown one is a refusal;
* identity: a profile may not take a built-in's registry NAME (`run.py::_assert_not_hijacked`, the
  same refusal `message_codec`, `check` and `fusion` carry);
* provenance: `module_sha256` + `package_sha256` + the wheel RECORD hash land in the manifest lock,
  and `verify-plugins` re-resolves and compares them on replay;
* CAPABILITY BY OMISSION, which is the only strong in-process control there is: a profile is handed
  an :class:`~scms_sim_ref.api.rng.RngNamespace` and a read-only config view. It never receives the
  engine's global `random.Random(cfg.seed)`, the vehicle list, the attack schedule or any ground
  truth object -- there is no reference to reach them through.

**CONVENTION, checkable but not enforceable at load:** determinism, purity, no I/O, no clock, no
entropy, monotonicity of the congestion controller, and the anti-laundering property above. These
are what the ``P1``-``P9`` conformance contract grades
(:mod:`scms_sim_ref.conformance.v1.protocol`), what `plugins.protocol_profile.conformance =
"required"` refuses a run over, and what the pinned goldens plus the two-run equality gate catch
after the fact. A suite is a measurement, not a sandbox.

**NOT ENFORCEABLE IN-PROCESS AT ALL:** arbitrary code execution at import time, native
nondeterminism, resource exhaustion, and reading the ORACLE FILES OFF DISK. The last one is the
sharp one and it is measured, not hypothesised (`docs/realism/ISOLATION-ORACLE-LEAK.md`): a plugin
is an ordinary process with ordinary read access to `out_dir`. For code you do not trust the answer
is the same as it is for detectors -- run it out of process, where the address space it can reach
does not contain the answer key. The profile slot does not yet have that mode; until it does, a
profile is *attested and content-addressed, not sandboxed*, and this docstring says so rather than
letting a reader infer otherwise from the length of the ENFORCED list.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Mapping, Optional, Protocol, runtime_checkable

INTERFACE_NAME = "ProtocolProfile"
INTERFACE_VERSION = "ProtocolProfile/1.0"

#: Highest interface MINOR this engine understands.
MAX_MINOR = 0

# --------------------------------------------------------------------------- #
# Capabilities -- what the profile TAKES OVER from the engine
# --------------------------------------------------------------------------- #
#: :meth:`ProtocolProfile.codec` returns a real codec; every broadcast becomes octets.
CAP_CODEC = "codec"
#: :meth:`ProtocolProfile.new_generation_state` decides when a station transmits. Without it the
#: engine keeps its historic one-message-per-station-per-step cadence.
CAP_GENERATION = "generation"
#: :meth:`ProtocolProfile.new_congestion_state` rate-limits under load.
CAP_CONGESTION = "congestion"
#: :meth:`ProtocolProfile.link_latency_s` supplies a per-packet latency. Declaring it REPLACES the
#: engine's `rng.uniform(0, net_delay_max)` report-ingest draw, which moves every downstream number
#: -- so it is a declaration, never a default.
CAP_LATENCY = "latency"
#: :meth:`ProtocolProfile.frame_airtime_s` / :meth:`ProtocolProfile.channel_busy_ratio` own the
#: airtime accounting the CBR and the collision term are computed from.
CAP_AIRTIME = "airtime"
#: :meth:`ProtocolProfile.wire_size_bytes` accounts for the security envelope itself rather than
#: charging the codec's payload length.
CAP_SECURITY_ENVELOPE = "security_envelope"

KNOWN_CAPABILITIES = frozenset({
    CAP_AIRTIME, CAP_CODEC, CAP_CONGESTION, CAP_GENERATION, CAP_LATENCY, CAP_SECURITY_ENVELOPE,
})

#: Nothing is reserved. The channel and detector slots reserve capabilities because a built-in there
#: is grandfathered against a pinned digest (`legacy_global_rng`); every profile -- built-in
#: included -- is off by default, so there is nothing to grandfather.
RESERVED_CAPABILITIES: frozenset = frozenset()


# --------------------------------------------------------------------------- #
# Generation: the MA-visible input a transmit decision is allowed to see
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class GenerationInput:
    """One station's own EGO STATE at one evaluation instant. MA-VISIBLE ONLY.

    Deliberately NOT the falsified claim and NOT the GNSS-noisy fix, for two separate reasons that
    are both measured in `run.py`'s CAM-generation block: keying the cadence on the attack would make
    the message rate a detector for the attack, and keying it on per-step measurement noise fires the
    4 m trigger on the noise rather than on the movement (1548 of 2806 CAMs at dt=0.1, 5.94 Hz
    against the Java engine's 2.94 Hz on the same rules). A real station's ego position comes from a
    smoothed GNSS/INS fusion; this engine has no ego-state estimator, so the true position stands in
    for the output of one, and that residual is stated rather than hidden.

    There is no `veh`, no `is_attacker`, no `falsified` and no other station's anything here, and
    ``P3_no_oracle_influence`` asserts the profile's decisions do not move when those are present on
    a subclass carrying them.
    """

    t: float                    #: engine seconds
    x: float                    #: ego position, local metres east
    y: float                    #: ego position, local metres north
    speed: float                #: ego speed, m/s
    heading: float              #: ego heading, degrees CCW from East
    dt: float = 1.0             #: the evaluation interval
    #: The minimum inter-message interval the congestion controller permits right now, in seconds.
    #: `0.0` means "no congestion control is active"; a generation rule still applies its own floor.
    min_interval_s: float = 0.0
    #: `True` for a road-side unit. RSUs are on a different service cadence in most stacks.
    is_rsu: bool = False
    #: The self-DECLARED station type, exactly as it goes on the wire ("vehicle" | "vru").
    station_type: str = "vehicle"


#: Returned by a generation state when NO message is generated at this instant.
NO_MESSAGE = ""


@runtime_checkable
class GenerationState(Protocol):
    """One station's message-generation service. Constructed per station by the profile."""

    def evaluate(self, ego: "GenerationInput") -> str:
        """`""` for "no message now", otherwise a short TRIGGER REASON label.

        The label is manifest-only (it lands in `counts.protocol`), so a profile may name its own
        reasons freely. It must be a `str` and it must be falsey exactly when no message is sent.
        """


#: The load-time signature contract for a generation state.
GENERATION_SPEC = {"evaluate": ("ego",)}


@runtime_checkable
class CongestionState(Protocol):
    """One station's congestion-control entity. Constructed per station by the profile.

    Fed the CBR that station's OWN receiver measured on the PREVIOUS step, which is the causality
    rather than an approximation: a station cannot react to a load it has not yet heard.
    """

    def update(self, cbr: float) -> str:
        """Fold in a measured CBR and return a short STATE LABEL (manifest-only)."""

    def min_interval_s(self) -> float:
        """The minimum inter-message interval this controller currently permits, in seconds."""


CONGESTION_SPEC = {"update": ("cbr",), "min_interval_s": ()}


# --------------------------------------------------------------------------- #
# What the engine measured, handed back to the profile to report on
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class ProtocolMeasurements:
    """Everything the engine TALLIED about this run's protocol layer, for :meth:`ProtocolProfile.report`.

    The engine counts; the PROFILE says what the counts mean. That split is the reason the manifest
    block is not hard-coded to a DCC state table: a profile whose congestion controller has no states
    (LIMERIC has a continuous duty-cycle instead) reports its own quantity in its own words, and the
    engine needs no knowledge of it.

    Manifest-only by construction -- `_data_digest` excludes `manifest.json` -- so nothing here can
    move a digest no matter what a profile does with it.
    """

    #: Every live :class:`GenerationState`, in station order. Empty when generation is engine-driven.
    generation_states: tuple = ()
    #: Every live :class:`CongestionState`, in station order.
    congestion_states: tuple = ()
    #: trigger label -> count, over the whole run.
    triggers: Mapping = field(default_factory=dict)
    #: Inter-message gap tally: (n gaps, sum of gaps in s, max gap in s).
    gaps: tuple = (0, 0.0, 0.0)
    #: (sum, n, max) of the CBR every receiver measured, and the measurement window in seconds.
    cbr: tuple = (0.0, 0, 0.0, 1.0)
    #: (sum of wire bytes, n frames) actually put on the air.
    wire: tuple = (0, 0)
    #: Per-message-type PDU counts and payload bytes from the codec, or `{}` with no codec.
    codec_stats: Mapping = field(default_factory=dict)
    #: `latency in seconds -> count`, binned at 1 microsecond. Empty when no latency model is on.
    latency_hist: Mapping = field(default_factory=dict)
    #: (sum, n, min, max) of per-packet latency in seconds.
    latency: tuple = (0.0, 0, 0.0, 0.0)


@runtime_checkable
class ProtocolProfile(Protocol):
    """The seam. Nine methods; a profile that implements them IS a protocol stack to this engine."""

    interface_version: str
    plugin_id: str
    profile_id: str            #: "etsi_its_g5" | ...

    def capabilities(self) -> frozenset: ...

    def standards_claim(self) -> Mapping:
        """What this profile may honestly assert, verbatim into `manifest["standards_profile"]`.

        Same rule the codec slot is held to (section 6.5): name the standard AND the clause when you
        implement one; never say "conformance-tested", "certified" or "Plugtests-validated", because
        none of those follows from implementing a rule out of a document.
        """

    def codec(self):
        """The :class:`~scms_sim_ref.api.codec.MessageCodec` this profile puts on the wire, or None.

        Called ONCE per run. `None` means the engine keeps its bare-dict representation and charges
        the legacy frame size, which is what every run before this seam did.
        """

    def new_generation_state(self):
        """A fresh :class:`GenerationState` for one station, or None to leave generation to the engine."""

    def new_congestion_state(self):
        """A fresh :class:`CongestionState` for one station, or None for no congestion control."""

    def wire_size_bytes(self, claim, signer: str) -> int:
        """Octets on the air for one message under `signer`, SECURITY ENVELOPE INCLUDED.

        This -- not the codec's payload length -- is what airtime, CBR, the collision term and the
        latency model are computed from, so the security envelope is accounted for exactly once and
        by the layer that knows how big its own envelope is.
        """

    def frame_airtime_s(self, size_bytes: int) -> float:
        """Channel time one transmission attempt of `size_bytes` occupies, in seconds."""

    def channel_busy_ratio(self, offered_airtime_s: float, window_s: float) -> float:
        """CBR from integrated offered airtime over a measurement window. In [0, 1]."""

    def link_latency_s(self, distance_m: float, cbr: float, size_bytes: int) -> float:
        """End-to-end latency of ONE message on ONE link, in seconds.

        Must be DETERMINISTIC -- a pure function of its arguments. Drawing here would put a random
        number on a path whose whole value is that it has none, and would make every pinned digest
        unreachable with the model on.
        """

    def report(self, measured: "ProtocolMeasurements") -> Mapping:
        """`counts["protocol"]` for this profile -- what its layers actually did, in measured numbers.

        Manifest-only. Returning `{}` is legitimate and costs nothing.
        """


#: The load-time signature contract (`registry._check_signature`). Neither `Protocol` nor `ABC`
#: checks signatures at runtime; this is the third component that does.
PROFILE_SPEC = {
    "capabilities": (),
    "standards_claim": (),
    "codec": (),
    "new_generation_state": (),
    "new_congestion_state": (),
    "wire_size_bytes": ("claim", "signer"),
    "frame_airtime_s": ("size_bytes",),
    "channel_busy_ratio": ("offered_airtime_s", "window_s"),
    "link_latency_s": ("distance_m", "cbr", "size_bytes"),
    "report": ("measured",),
}


class ProtocolProfileBase:
    """Optional convenience base with the defaults every profile shares.

    Inheriting is never required -- the published contract is the `Protocol`, so an implementer needs
    no import of ours beyond the dataclasses. This exists for authors who prefer inheritance, exactly
    as `LinkChannelModelBase`, `CheckBase` and `MessageCodecBase` do on the other slots.

    Every default here is the ENGINE'S HISTORIC BEHAVIOUR, not ITS-G5's: no codec, no generation
    rules, no congestion control, and an airtime model that is a straight `size / rate`. A profile
    that overrides nothing therefore leaves the engine exactly where it was, which is the property
    that makes partial profiles safe.
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "protocol_profile"
    profile_id = "abstract"

    #: Bits per second on the air, for the default `frame_airtime_s`. 6 Mb/s is the 802.11p MCS the
    #: refdata pins; a profile on another radio overrides the method outright.
    bitrate_bps = 6_000_000.0

    def capabilities(self) -> frozenset:
        return frozenset()

    def standards_claim(self) -> Mapping:
        return {}

    def codec(self):
        return None

    def new_generation_state(self):
        return None

    def new_congestion_state(self):
        return None

    def wire_size_bytes(self, claim, signer: str = "none") -> int:
        c = self.codec()
        if c is None:
            raise NotImplementedError("a profile with no codec must implement wire_size_bytes()")
        return int(c.wire_size_bytes(claim, signer))

    def frame_airtime_s(self, size_bytes: int) -> float:
        return (8.0 * max(0, int(size_bytes))) / float(self.bitrate_bps)

    def channel_busy_ratio(self, offered_airtime_s: float, window_s: float) -> float:
        return min(1.0, max(0.0, float(offered_airtime_s)) / max(float(window_s), 1e-9))

    def link_latency_s(self, distance_m: float, cbr: float, size_bytes: int) -> float:
        raise NotImplementedError("declare CAP_LATENCY only with a link_latency_s() implementation")

    def report(self, measured: "ProtocolMeasurements") -> Mapping:
        return {}


def generation_report(measured: "ProtocolMeasurements", dynamics: frozenset = frozenset()) -> dict:
    """The MEASURED half of a generation block, shared by every profile that has one.

    Counts and gaps only -- never a restatement of a constant, which is what a profile's own
    :meth:`ProtocolProfile.report` adds around this. `dynamics` names the trigger labels that count
    as movement-triggered rather than periodic; empty means "do not claim a share".
    """
    triggers = dict(measured.triggers or {})
    total = sum(triggers.values())
    out: dict = {"messages": total, "triggers": dict(sorted(triggers.items()))}
    if dynamics:
        dyn = sum(v for k, v in triggers.items() if k in dynamics)
        out["dynamics_share"] = round(dyn / total, 6) if total else 0.0
    n, gsum, gmax = measured.gaps
    if n:
        out["mean_gap_s"] = round(gsum / n, 6)
        out["max_gap_s"] = round(gmax, 6)
        out["mean_rate_hz"] = round(n / gsum, 6) if gsum else 0.0
        out["gaps"] = n
    return out


__all__ = [
    "CAP_AIRTIME", "CAP_CODEC", "CAP_CONGESTION", "CAP_GENERATION", "CAP_LATENCY",
    "CAP_SECURITY_ENVELOPE", "CONGESTION_SPEC", "CongestionState", "GENERATION_SPEC",
    "GenerationInput", "GenerationState", "INTERFACE_NAME", "INTERFACE_VERSION",
    "KNOWN_CAPABILITIES", "MAX_MINOR", "NO_MESSAGE", "PROFILE_SPEC", "ProtocolMeasurements",
    "ProtocolProfile", "ProtocolProfileBase", "RESERVED_CAPABILITIES", "generation_report",
]
