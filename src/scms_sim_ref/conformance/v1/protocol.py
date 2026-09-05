"""The v1 PROTOCOL-PROFILE and REPORT-FORMAT contracts -- `P1`-`P11` and `R1`-`R7`.

Three delivery routes, one implementation, exactly as the channel and detector contracts have::

    class TestMyStack(ProtocolProfileContract):
        REF = "myorg.stack:CV2XMode4Profile"
        PARAMS = {"cbr_target": 0.68}

    scms-poc conformance --slot protocol_profile --ref myorg.stack:CV2XMode4Profile
    plugins.protocol_profile = {"ref": "...", "conformance": "required"}   # the engine refuses it

**WHAT THIS SUITE IS FOR, and why a weak one would be worse than none.** A protocol profile sits on
the path of EVERY message -- not, like a detector, only on the ones it scores. It decides when a
station transmits, how big the frame is, how long it is on the air and how long it takes to arrive.
Every one of those is a lever on the whole dataset, and three of them are levers a hostile or merely
careless profile could pull in ways no digest would notice:

* **Nondeterminism** (``P2``/``P3``). `ma_reports.jsonl` is inside `data_digest`, and message timing
  reaches it. A profile that draws a random number does not produce a slightly different dataset --
  it produces one that cannot be replayed, at exit 0.
* **Oracle laundering** (``P4``). Transmitting more often for an attacker than for an honest station
  makes the message RATE a detector for the attack. It is invisible to every range check, every
  name-based lint and every plausibility eyeball, because the output looks like ordinary physics.
  The only thing that catches it is driving the profile twice over inputs that are field-for-field
  identical in everything the interface declares and differ ONLY in ground truth it has no business
  reading. That is what :class:`OracleGenerationInput` is, and it is the single most important
  object in this file.
* **A codec whose PDU does not decode** (``P6``). An encoder nobody round-trips is a stream of
  plausible-looking octets, and it would sail through every other check here -- the lengths are
  right, the airtime is right, the CBR is right. Only decoding it says whether it means anything.

**This project has already shipped one powerless gate**, so the suite is graded the same way the
channel contract's `C6b` was: `tests/test_protocol_profile.py` runs three profiles that SHOULD fail
-- one nondeterministic, one that reads oracle state, one whose codec emits an undecodable PDU --
and asserts each is refused, by name, on the check that is supposed to catch it. A suite that has
never been shown to fail a bad plugin has not been shown to do anything.

**No Hypothesis, no randomisation, no network, no clock.** Every sequence here comes from this
module's own seeded `random.Random`, and the whole suite is a pure function of
:attr:`ProtocolProfileContract.SEED`.
"""
from __future__ import annotations

import json
import math
import random

from ...api import codec as _api_codec
from ...api import profile as _profile
from ...api import registry as _registry
from ...api import report as _report
from ...api.errors import ConfigError
from ...api.rng import RngNamespace
from ...schemas.records import is_forbidden_feature_key
from .channel import CheckSkipped          # ONE skip type, so `runner._is_skip` recognises both
from .harness import DrawCounter, audit_guard

SUITE_VERSION = "v1"

#: The protocol-profile check ids, in the order they are run and reported.
PROTOCOL_CHECKS = (
    "P1_constructs_and_declares",
    "P2_repeatable",
    "P3_draws_no_random_number",
    "P4_no_oracle_influence",
    "P5_no_io",
    "P6_codec_round_trips",
    "P7_wire_size_is_pure_and_positive",
    "P8_airtime_and_cbr_are_sane",
    "P9_generation_respects_min_interval",
    "P10_congestion_never_speeds_up_under_load",
    "P11_latency_is_deterministic_and_finite",
)

#: The report-format check ids.
REPORT_CHECKS = (
    "R1_repeatable",
    "R2_row_carries_no_oracle_field",
    "R3_oracle_input_does_not_change_the_row",
    "R4_required_keys_present",
    "R5_json_serialisable",
    "R6_draws_no_random_number",
    "R7_no_io",
)


def _skip(reason: str):
    try:
        import pytest
    except ImportError:                                     # pragma: no cover - pytest optional
        raise CheckSkipped(reason) from None
    pytest.skip(reason)


def _engine():
    """Importing the engine is what populates the built-in registry."""
    from ...mock_pipeline import run as _run
    return _run


# --------------------------------------------------------------------------- #
# Scenarios -- fixed, replayable, and free of ground truth by construction
# --------------------------------------------------------------------------- #
class OracleGenerationInput(_profile.GenerationInput):
    """A :class:`~scms_sim_ref.api.profile.GenerationInput` carrying GROUND TRUTH the ABI does not
    declare.

    The declared fields are IDENTICAL between a plain input and one of these; the only difference is
    oracle state a conformant profile has no business reading. A profile that keys on it --
    ``if getattr(ego, "is_attacker", False): return "position"`` -- makes the transmit CADENCE a
    function of the attack, which is a leak dressed as a facilities-layer timer. Comparing the two
    traces is the only thing that catches it, and it is the protocol-side form of the same trap
    `OracleStation` (channel) and `OracleObservation` (detector) set.
    """

    __slots__ = ("is_attacker", "falsified", "attack_type", "true_x", "true_y", "veh")

    def __init__(self, base, **oracle):
        super().__init__(t=base.t, x=base.x, y=base.y, speed=base.speed, heading=base.heading,
                         dt=base.dt, min_interval_s=base.min_interval_s, is_rsu=base.is_rsu,
                         station_type=base.station_type)
        for k, v in oracle.items():
            object.__setattr__(self, k, v)


def ego_track(*, n: int = 240, dt: float = 0.1, min_interval_s: float = 0.0,
              seed: int = 20260905) -> tuple:
    """A fixed 24-second drive: accelerate, cruise, turn, brake, stop, pull away.

    Deterministic and hand-shaped rather than random-walked, because every phase is there to make a
    specific trigger fire: the cruise clears a 4 m position threshold, the turn clears a 4-degree
    heading threshold, the braking phase clears a 0.5 m/s speed threshold, and the stop is the only
    place a heart-beat can be the reason anything is sent. A profile graded on a straight line at
    constant speed would have three of its four triggers untested.
    """
    rng = random.Random(seed)
    out, x, y, speed, heading = [], 0.0, 0.0, 0.0, 0.0
    for k in range(n):
        t = k * dt
        phase = k / n
        if phase < 0.25:
            speed = min(16.0, speed + 2.0 * dt)
        elif phase < 0.45:
            heading = (heading + 22.0 * dt) % 360.0
        elif phase < 0.62:
            speed = max(0.0, speed - 6.0 * dt)
        elif phase < 0.78:
            speed = 0.0
        else:
            speed = min(14.0, speed + 3.0 * dt)
        # A metre of GNSS-free jitter so a profile cannot pass by keying on an exactly linear track.
        jitter = (rng.random() - 0.5) * 0.02
        rad = math.radians(heading)
        x += speed * dt * math.cos(rad) + jitter
        y += speed * dt * math.sin(rad)
        out.append(_profile.GenerationInput(t=t, x=x, y=y, speed=speed, heading=heading, dt=dt,
                                            min_interval_s=min_interval_s))
    return tuple(out)


#: The CBR ladder every congestion check is graded on: below the reactive table's first breakpoint,
#: at it, through every band, and past saturation.
CBR_LADDER = (0.0, 0.05, 0.20, 0.2999, 0.30, 0.35, 0.45, 0.55, 0.65, 0.80, 0.95, 1.0)

#: Frame sizes the airtime and latency checks sweep. 41 B is a real UPER CAM; 134 B and 260 B are
#: that CAM under the two measured TS 103 097 envelopes; 300 B is what both engines used to assume.
FRAME_SIZES = (1, 41, 100, 134, 200, 260, 300, 500, 1500)

#: Offered channel time, in seconds per one-second window, that `channel_busy_ratio` is swept over:
#: idle, sparse, through the band, and past saturation (where the estimator must clamp rather than
#: return a ratio above 1).
OFFERED_AIRTIME_LADDER = (0.0, 0.0005, 0.01, 0.1, 0.3, 0.5, 0.68, 0.9, 1.0, 1.5, 100.0)


def sample_claim(**kw) -> _api_codec.Claim:
    """One HONEST claim, in engine units. Every field is inside its ETSI-encodable range, so a codec
    that refuses it is refusing a legal message rather than an out-of-range one."""
    base = dict(station_id=0x1234ABCD, cert_digest="0123456789abcdef", msg_type="cam",
                gen_time=12.5, x=137.25, y=-84.5, speed=13.75, heading=42.0, pos_conf=1.5,
                station_type="vehicle", msg_count=1, sig_ok=True,
                cert_valid_from=0.0, cert_valid_to=3600.0)
    base.update(kw)
    return _api_codec.Claim(**base)


# --------------------------------------------------------------------------- #
# The protocol-profile contract
# --------------------------------------------------------------------------- #
class ProtocolProfileContract:
    """The v1 protocol-profile contract. Subclass, set :attr:`REF` or override :meth:`make`."""

    SLOT = "protocol_profile"
    INTERFACE_VERSION = _profile.INTERFACE_VERSION
    REF: str = None                       #: dotted path, entry-point name or built-in registry key
    PARAMS: dict = {}                     #: params handed to the plugin, as a config would
    SEED = 20260905                       #: the ONLY source of variation in this whole suite
    PRECISION = 9                         #: decimals used when comparing two float traces

    #: check_id -> WRITTEN justification (Django's `django_test_skips` doctrine). A waiver with an
    #: empty justification is REFUSED by the runner.
    waivers: dict = {}

    def declared_waivers(self) -> dict:
        if self.REF is None:
            return {}
        try:
            _engine()
            cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        except Exception:                                   # unresolvable -> the checks will say so
            return {}
        return dict(getattr(cls, "conformance_waivers", None) or {})

    def waiver_for(self, check_id: str):
        return dict(self.declared_waivers(), **(self.waivers or {})).get(check_id)

    # -- construction ------------------------------------------------------------------------- #
    def _config(self):
        return _engine().PipelineConfig(seed=self.SEED)

    #: The codec the contract INJECTS through `env`, exactly as `run.build_profile` does. A profile
    #: that brings its own returns it from `codec()` and this is ignored; a profile that consumes
    #: the injected one (the built-in `etsi_its_g5` does) is then graded by `P6` on a real
    #: round trip instead of skipping it. `None` injects nothing.
    CODEC_REF: str = "native_v1"

    def env(self) -> dict:
        """The construction environment, shaped exactly like `run.build_profile`'s.

        The injected codec is the one difference from a bare `{"config": ...}`, and it is there
        because the engine really does inject one: a contract that omitted it would grade a
        construction environment the profile will never see.
        """
        cfg = self._config()
        codec = None
        if self.CODEC_REF:
            _engine()
            ccls, _how, _iv, _shape = _registry.resolve("message_codec", self.CODEC_REF)
            codec = _registry.instantiate(
                ccls, params={}, rng=RngNamespace(self.SEED, "conformance_codec"), env={})
        return {"config": _engine().ReadOnlyConfig(cfg), "message_codec": codec}

    def make(self, **params):
        """Build one fresh instance. Every check that needs one builds its own -- a contract check
        may never observe state another check left behind."""
        if self.REF is None:
            raise NotImplementedError(
                f"{type(self).__name__}: set REF = 'package.module:Class' (or a built-in registry "
                f"key), or override make()")
        p = dict(self.PARAMS)
        p.update(params)
        _engine()
        cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        _validate_params(cls, p)
        pid = _registry.plugin_id_of(cls, self.REF.rsplit(":", 1)[-1].lower())
        self._ns = RngNamespace(self.SEED, pid)
        return _registry.instantiate(cls, params=p, rng=self._ns, env=self.env())

    def capabilities(self) -> frozenset:
        return frozenset(self.make().capabilities())

    # -- driving a profile --------------------------------------------------------------------- #
    def drive(self, prof, track=None) -> tuple:
        """Run one station through `track` and return a canonical, comparable trace.

        One row per evaluated instant: `(step, trigger label, min interval the controller permitted,
        congestion state label)`. Everything a profile DECIDES is in the row; nothing a profile
        merely holds is, so an internal representation change cannot make two runs compare unequal.
        """
        track = track or self.track()
        gen = prof.new_generation_state()
        cong = prof.new_congestion_state()
        rows, cbr = [], 0.0
        for k, ego in enumerate(track):
            state = ""
            floor = 0.0
            if cong is not None:
                state = str(cong.update(cbr))
                floor = float(cong.min_interval_s())
            if gen is None:
                rows.append((k, "engine", round(floor, self.PRECISION), state))
            else:
                ego = _with_floor(ego, floor)
                reason = gen.evaluate(ego)
                rows.append((k, str(reason), round(floor, self.PRECISION), state))
                # A crude, deterministic load feedback so the congestion controller is driven over a
                # real range rather than at a constant: a station that transmitted adds airtime.
                if reason:
                    cbr = min(1.0, cbr + 0.07)
                else:
                    cbr = max(0.0, cbr - 0.01)
        return tuple(rows)

    def track(self):
        return ego_track(seed=self.SEED)

    def size_trace(self, prof) -> tuple:
        """Everything the profile says about SIZE and COST, as one comparable tuple."""
        rows = []
        for n in FRAME_SIZES:
            rows.append(("airtime", n, round(float(prof.frame_airtime_s(n)), 12)))
        # The CBR estimator is swept over OFFERED AIRTIME, which is its actual argument -- a sweep
        # keyed on the CBR ladder would have recomputed one value twelve times under twelve
        # different labels and compared equal no matter what the estimator did.
        for offered in OFFERED_AIRTIME_LADDER:
            rows.append(("cbr", offered, round(float(prof.channel_busy_ratio(offered, 1.0)), 12)))
        if _profile.CAP_LATENCY in frozenset(prof.capabilities()):
            for n in FRAME_SIZES:
                for c in CBR_LADDER:
                    rows.append(("lat", n, c,
                                 round(float(prof.link_latency_s(120.0, c, n)), 12)))
        for signer in _api_codec.SIGNER_FORMS:
            try:
                rows.append(("size", signer, int(prof.wire_size_bytes(sample_claim(), signer))))
            except Exception as e:                          # noqa: BLE001 - recorded, not raised
                rows.append(("size", signer, f"{type(e).__name__}"))
        return tuple(rows)

    # -- P1 --------------------------------------------------------------------------------- #
    def check_P1_constructs_and_declares(self):
        """Constructs, declares a known interface version, and declares only known capabilities.

        Everything here is reachable BEFORE step 0 and is the whole reason the resolver refuses at
        load: a stack that fails at step k > 0 has already produced a partial dataset nobody can
        interpret.
        """
        prof = self.make()
        iv = getattr(prof, "interface_version", None)
        assert iv, f"{type(prof).__name__} declares no interface_version"
        name, _, ver = str(iv).partition("/")
        assert name == _profile.INTERFACE_NAME, f"interface {name!r} != {_profile.INTERFACE_NAME!r}"
        caps = frozenset(prof.capabilities())
        unknown = sorted(caps - _profile.KNOWN_CAPABILITIES)
        assert not unknown, (f"declares unknown capability {unknown}; "
                             f"known: {sorted(_profile.KNOWN_CAPABILITIES)}")
        assert isinstance(prof.standards_claim(), dict) or hasattr(prof.standards_claim(), "keys"), \
            "standards_claim() must be a mapping"
        asserted = _asserts_conformance(prof.standards_claim())
        assert not asserted, (
            f"standards_claim() ASSERTS {asserted}; none of that follows from implementing a rule "
            f"out of a document (section 6.5). Saying 'NOT conformance-tested' is fine and is what "
            f"the built-ins say -- claiming it is not")
        # A declared layer must actually be there. A profile claiming `generation` and returning
        # None from `new_generation_state()` would make the engine silently keep its own cadence
        # while the manifest recorded a generation rule that never ran.
        if _profile.CAP_GENERATION in caps:
            assert prof.new_generation_state() is not None, \
                "declares CAP_GENERATION but new_generation_state() returned None"
        if _profile.CAP_CONGESTION in caps:
            assert prof.new_congestion_state() is not None, \
                "declares CAP_CONGESTION but new_congestion_state() returned None"
        return f"{iv} caps={sorted(caps)}"

    # -- P2 --------------------------------------------------------------------------------- #
    def check_P2_repeatable(self):
        """Two fresh instances, the same track, identical traces -- decisions AND sizes.

        This is the check a profile that reads a clock, hashes an object address, iterates a `set`
        or draws a random number fails, and it is the one that matters most: report timing reaches
        `data_digest`, so a profile that is not repeatable makes the dataset unreplayable at exit 0.
        """
        a = self.drive(self.make())
        b = self.drive(self.make())
        assert a == b, f"two identical drives differ at row {_first_diff(a, b)}"
        sa, sb = self.size_trace(self.make()), self.size_trace(self.make())
        assert sa == sb, f"two identical size traces differ at row {_first_diff(sa, sb)}"
        return f"{len(a)} decisions + {len(sa)} size rows reproduced exactly"

    # -- P3 --------------------------------------------------------------------------------- #
    def check_P3_draws_no_random_number(self):
        """ZERO `random.Random` draws over a full drive, by TWO independent instruments.

        Stronger than the channel contract's C3, and deliberately. A channel model is EXPECTED to
        draw (its keyed streams are how a fade is modelled); a protocol profile is a set of timers
        and arithmetic, and the engine's ability to switch a whole stack on without moving a single
        pinned digest rests on it drawing nothing at all. A stack that genuinely needs a random
        number -- a randomised backoff, a jittered start-up offset -- must waive this check with a
        written justification, which is exactly what the waiver mechanism is for.

        **Two instruments, because one of them has a hole and it is the hole a real implementer
        falls into.** `DrawCounter` patches the METHODS ON `random.Random`, but the module-level
        `random.random()` is a bound method of the hidden global instance, captured at import time
        -- so `import random; random.random()`, which is what everybody actually writes, is
        INVISIBLE to the counter. Measured on `scms_cv2x_profile.bad:NondeterministicProfile`: 240
        module-level draws, counter reads 0. Comparing the global generator's STATE across the drive
        catches it, and catching it matters more than either instrument alone, because a profile
        that consumes from the process-global stream does not merely randomise itself -- it shifts
        every subsequent draw the ENGINE makes, and the engine's draw order is load-bearing for both
        pinned goldens.
        """
        prof = self.make()
        # The track is built OUTSIDE the counter: `ego_track` seeds its own `random.Random` for the
        # jitter, and counting the harness's own draws would make this check unfailable-by-anyone
        # rather than unfailable-by-an-honest-profile.
        track = self.track()
        before = random.getstate()
        with DrawCounter() as c:
            self.drive(prof, track)
            self.size_trace(prof)
        after = random.getstate()
        assert c.count == 0, (f"drew {c.count} random number(s); a protocol profile must be a pure "
                              f"function of its inputs (waive this check with a written "
                              f"justification if the stack genuinely randomises)")
        assert after == before, (
            "advanced the PROCESS-GLOBAL `random` generator -- module-level `random.random()`, "
            "`random.uniform()` and friends are bound methods of a hidden global instance, so they "
            "are invisible to a method-level draw counter. A profile that consumes from that stream "
            "shifts every subsequent draw the engine makes")
        return "0 draws, global generator state unmoved"

    # -- P4 --------------------------------------------------------------------------------- #
    def check_P4_no_oracle_influence(self):
        """The anti-laundering check, and the reason this file exists.

        Two drives whose DECLARED fields are identical field-for-field, one of which additionally
        carries ground truth on the ego object -- `is_attacker`, `falsified`, `attack_type`, the
        true coordinates, the vehicle itself. Identical traces, or the profile is keying the
        transmit cadence on the answer key and the message RATE has become a detector for the
        attack.

        It also refuses a profile that reaches for an attribute the DTO does not declare, by name,
        through a recording proxy -- the capability form of the check, which a name-based linter
        provably cannot do.
        """
        plain = self.track()
        oracle = tuple(OracleGenerationInput(e, is_attacker=(i % 3 == 0),
                                             falsified=(i % 3 == 0),
                                             attack_type="ConstPos" if i % 3 == 0 else "",
                                             true_x=e.x + 250.0, true_y=e.y - 250.0, veh=object())
                       for i, e in enumerate(plain))
        a = self.drive(self.make(), plain)
        b = self.drive(self.make(), oracle)
        assert a == b, (f"the trace MOVED when ground truth was attached to an otherwise identical "
                        f"ego state -- first difference at row {_first_diff(a, b)}. A profile that "
                        f"transmits differently for an attacker makes the message rate a detector "
                        f"for the attack")
        # And by name: what did it actually touch?
        prof = self.make()
        gen = prof.new_generation_state()
        if gen is not None:
            touched = set()
            for ego in plain[:24]:
                gen.evaluate(_RecordingEgo(ego, touched))
            bad = sorted(n for n in touched if is_forbidden_feature_key(n))
            assert not bad, f"read oracle attribute(s) {bad} off the ego state"
            undeclared = sorted(n for n in touched
                                if not n.startswith("_") and n not in _EGO_FIELDS)
            assert not undeclared, (f"read attribute(s) {undeclared} that GenerationInput does not "
                                    f"declare; the DTO is the whole vocabulary")
        return "identical under an oracle-carrying ego state"

    # -- P5 --------------------------------------------------------------------------------- #
    def check_P5_no_io(self):
        """No filesystem write, network connection or subprocess while deciding.

        Stated honestly, exactly as the channel contract states it: PEP 578 says in as many words
        that it *"is not sandboxing"*, and hooks fire only in this interpreter, so a subprocess
        escapes entirely. This detects ACCIDENTAL I/O -- a profile that logs to a file, phones a
        licence server, or reads the ORACLE files out of `out_dir`. It does not contain hostile I/O,
        and nothing in-process can.
        """
        with audit_guard():
            prof = self.make()
            self.drive(prof)
            self.size_trace(prof)
        return "no denied I/O"

    # -- P6 --------------------------------------------------------------------------------- #
    def check_P6_codec_round_trips(self):
        """A PDU the profile's own codec cannot decode is not a PDU.

        An encoder nobody round-trips is a stream of plausible-looking octets: the lengths are
        right, the airtime is right, the CBR computed from it is right, and every other check in
        this file passes. Decoding is the only thing that says whether the bytes mean anything --
        which is why a profile whose codec emits an undecodable PDU is one of the three traps
        `tests/test_protocol_profile.py` proves this suite catches.

        Graded UP TO QUANTISATION: an ETSI profile encodes position to 1/10 microdegree and speed to
        0.01 m/s, so exact equality is the wrong test. The tolerances below are the quantisation
        steps themselves plus a decade of headroom, and a codec claiming `lossless` is held to
        exact equality instead.
        """
        prof = self.make()
        if _profile.CAP_CODEC not in frozenset(prof.capabilities()):
            _skip("profile declares no codec")
        codec = prof.codec()
        assert codec is not None, "declares CAP_CODEC but codec() returned None"
        station = _api_codec.StationView()
        lossless = _api_codec.CAP_LOSSLESS in frozenset(codec.capabilities())
        checked = 0
        for claim in (sample_claim(), sample_claim(speed=0.0, heading=0.0, x=0.0, y=0.0),
                      sample_claim(speed=61.0, heading=359.9, x=-990.0, y=990.0, pos_conf=12.0)):
            blob = codec.encode_cam(claim, station)
            assert isinstance(blob, (bytes, bytearray)) and len(blob) > 0, \
                f"encode_cam returned {type(blob).__name__} of length {len(blob or b'')}"
            back = codec.decode_cam(bytes(blob))
            for field, tol in _ROUND_TRIP_TOLERANCE.items():
                want, got = getattr(claim, field), getattr(back, field)
                if want is None or got is None:
                    continue
                lim = 0.0 if lossless else tol
                if field == "heading":
                    delta = abs((float(want) - float(got) + 180.0) % 360.0 - 180.0)
                else:
                    delta = abs(float(want) - float(got))
                assert delta <= lim, (
                    f"decode(encode(claim)).{field} = {got!r}, expected {want!r} "
                    f"(delta {delta:.6g} > tolerance {lim:.6g}"
                    f"{'; codec declares lossless' if lossless else ''})")
            checked += 1
        return f"{checked} claims round-tripped{' losslessly' if lossless else ''}"

    # -- P7 --------------------------------------------------------------------------------- #
    def check_P7_wire_size_is_pure_and_positive(self):
        """A frame length is a positive integer, and asking twice gives the same answer.

        Airtime, CBR, the collision term and the latency model are all computed from this number, so
        a size that is zero, negative, fractional or drifting corrupts four downstream quantities at
        once -- and every one of them would still look like plausible physics.
        """
        prof = self.make()
        seen = {}
        for signer in _api_codec.SIGNER_FORMS:
            try:
                n = prof.wire_size_bytes(sample_claim(), signer)
            except NotImplementedError:
                _skip("profile implements no wire_size_bytes (no codec and no envelope)")
            assert isinstance(n, int) and not isinstance(n, bool), \
                f"wire_size_bytes(..., {signer!r}) returned {type(n).__name__}, expected int"
            assert 0 < n <= 8192, f"wire_size_bytes(..., {signer!r}) = {n}, outside (0, 8192]"
            again = prof.wire_size_bytes(sample_claim(), signer)
            assert again == n, (f"wire_size_bytes(..., {signer!r}) returned {n} then {again} for "
                                f"the same claim")
            seen[signer] = n
        return " ".join(f"{k}={v}B" for k, v in sorted(seen.items()))

    # -- P8 --------------------------------------------------------------------------------- #
    def check_P8_airtime_and_cbr_are_sane(self):
        """Airtime grows with the frame; CBR is a ratio in [0, 1] and grows with offered load.

        Both are monotonicity statements rather than value statements, deliberately: this suite has
        no business telling a C-V2X profile what a subframe costs. What it CAN say is that a bigger
        frame is never cheaper and a busier channel never reads emptier, and a profile that fails
        either is not modelling an access layer.
        """
        prof = self.make()
        prev = -1.0
        for n in FRAME_SIZES:
            a = float(prof.frame_airtime_s(n))
            assert math.isfinite(a) and a > 0.0, f"frame_airtime_s({n}) = {a!r}"
            assert a >= prev, f"frame_airtime_s({n}) = {a} < frame_airtime_s(previous) = {prev}"
            prev = a
        assert float(prof.channel_busy_ratio(0.0, 1.0)) == 0.0, \
            "channel_busy_ratio(0, 1) must be 0: an idle channel is not busy"
        prevc = -1.0
        for offered in (0.0, 0.01, 0.1, 0.3, 0.5, 0.9, 1.0, 2.0, 100.0):
            c = float(prof.channel_busy_ratio(offered, 1.0))
            assert math.isfinite(c) and 0.0 <= c <= 1.0, \
                f"channel_busy_ratio({offered}, 1.0) = {c!r}, outside [0, 1]"
            assert c >= prevc, f"channel_busy_ratio fell from {prevc} to {c} as load ROSE"
            prevc = c
        return f"airtime {prof.frame_airtime_s(41)*1e6:.1f}us@41B -> " \
               f"{prof.frame_airtime_s(1500)*1e6:.1f}us@1500B"

    # -- P9 --------------------------------------------------------------------------------- #
    def check_P9_generation_respects_min_interval(self):
        """A generation rule must honour the floor the congestion controller hands it.

        This is the check that makes the two layers one protocol rather than two features in the
        same object: a rate limiter whose limit the generator ignores is not a rate limiter. Graded
        by driving the SAME track twice with two different floors and asserting (a) no two
        transmissions are closer than the floor, and (b) a larger floor never produces MORE
        transmissions.
        """
        prof = self.make()
        if _profile.CAP_GENERATION not in frozenset(prof.capabilities()):
            _skip("profile supplies no generation rules")
        counts = []
        for floor in (0.0, 0.5, 2.0):
            gen = prof.new_generation_state()
            last_t, n = None, 0
            for ego in self.track():
                if gen.evaluate(_with_floor(ego, floor)):
                    if last_t is not None and floor > 0.0:
                        gap = ego.t - last_t
                        assert gap + 1e-9 >= floor, (
                            f"transmitted {gap:.4f} s apart under a {floor} s floor -- the "
                            f"congestion controller's limit was ignored")
                    last_t = ego.t
                    n += 1
            counts.append(n)
        assert counts[0] >= counts[1] >= counts[2], (
            f"raising the floor did not reduce the message count: {counts} for floors "
            f"(0.0, 0.5, 2.0)")
        return f"messages at floors 0/0.5/2.0 s: {counts}"

    # -- P10 -------------------------------------------------------------------------------- #
    def check_P10_congestion_never_speeds_up_under_load(self):
        """`min_interval_s()` is non-decreasing in the measured CBR.

        The one property every congestion controller in existence has, whatever its control law:
        reactive DCC's state table, LIMERIC's duty cycle, a fixed-rate limiter. A controller that
        permits a SHORTER interval on a busier channel is a congestion AMPLIFIER, and it would look
        entirely plausible in a manifest -- the states would cycle, the numbers would be finite, and
        the CBR would simply be worse than it should be.
        """
        prof = self.make()
        if _profile.CAP_CONGESTION not in frozenset(prof.capabilities()):
            _skip("profile supplies no congestion control")
        # A fresh entity per rung, so this measures the MAPPING and not a trajectory through it.
        rows = []
        for cbr in CBR_LADDER:
            c = prof.new_congestion_state()
            state = c.update(cbr)
            interval = float(c.min_interval_s())
            assert math.isfinite(interval) and interval >= 0.0, \
                f"min_interval_s() = {interval!r} at CBR {cbr}"
            assert isinstance(state, str), \
                f"update({cbr}) returned {type(state).__name__}, expected a str state label"
            rows.append((cbr, state, interval))
        for (c0, _s0, i0), (c1, _s1, i1) in zip(rows, rows[1:]):
            assert i1 + 1e-12 >= i0, (
                f"min_interval_s FELL from {i0} at CBR {c0} to {i1} at CBR {c1} -- a controller "
                f"that transmits MORE under load is a congestion amplifier")
        return f"{rows[0][1]}@{rows[0][2]:.3f}s -> {rows[-1][1]}@{rows[-1][2]:.3f}s"

    # -- P11 -------------------------------------------------------------------------------- #
    def check_P11_latency_is_deterministic_and_finite(self):
        """Per-packet latency is a pure, finite, non-negative function of (distance, CBR, size).

        And it is non-decreasing in each: a longer link, a busier channel and a bigger frame can
        never make a packet arrive sooner. Purity matters more here than anywhere else in this
        contract, because `net_latency_model` REMOVES the engine's uniform ingest draw -- the whole
        reason that swap is affordable is that what replaces it draws nothing.
        """
        prof = self.make()
        if _profile.CAP_LATENCY not in frozenset(prof.capabilities()):
            _skip("profile supplies no latency model")
        base = float(prof.link_latency_s(100.0, 0.2, 200))
        assert math.isfinite(base) and base >= 0.0, f"link_latency_s = {base!r}"
        assert float(prof.link_latency_s(100.0, 0.2, 200)) == base, \
            "link_latency_s is not a pure function: two identical calls disagreed"
        for lo, hi, label in ((0.0, 800.0, "distance"), (0.0, 0.9, "cbr"), (41, 1500, "size")):
            args_lo = {"distance_m": 100.0, "cbr": 0.2, "size_bytes": 200}
            args_hi = dict(args_lo)
            key = {"distance": "distance_m", "cbr": "cbr", "size": "size_bytes"}[label]
            args_lo[key], args_hi[key] = lo, hi
            a, b = float(prof.link_latency_s(**args_lo)), float(prof.link_latency_s(**args_hi))
            assert b + 1e-15 >= a, (f"latency FELL from {a} to {b} as {label} rose from {lo} to "
                                    f"{hi}: a longer/busier/bigger link cannot arrive sooner")
        return f"{base*1e3:.4f} ms at 100 m / CBR 0.2 / 200 B"


# --------------------------------------------------------------------------- #
# The report-format contract
# --------------------------------------------------------------------------- #
class OracleReportInput(_report.ReportInput):
    """A :class:`~scms_sim_ref.api.report.ReportInput` carrying GROUND TRUTH the ABI does not declare.

    The detector suite's `OracleObservation` for the backhaul. It matters MORE here than there: a
    detector that reads the answer produces a wrong score, while a report FORMAT that reads the
    answer writes it into `ma/ma_reports.jsonl` -- a file the dataset ships and a supervised
    experiment trains on. The leak would be undetectable by every digest in the project, because the
    digest would simply be the digest of the leaked data.
    """

    __slots__ = ("is_attacker", "attack_type", "falsified", "true_x", "true_y",
                 "reporter_true_id", "subject_true_id", "report_correctness")

    def __init__(self, base, **oracle):
        super().__init__(**{f: getattr(base, f) for f in _REPORT_FIELDS})
        for k, v in oracle.items():
            object.__setattr__(self, k, v)


def sample_report(**kw) -> _report.ReportInput:
    """One report, with every optional column populated -- so a format cannot pass by ignoring the
    fields that are hardest to render."""
    base = dict(
        report_id="rpt_00042", ingest_time=41.123456789, detection_time=39.000000123,
        generation_time=39.0, reporter_cert_digest="aaaabbbbccccdddd",
        subject_cert_digest="1111222233334444",
        reason_codes=("positionJump", "speedPlausibility"), round_detection=True,
        detector_scores={"positionJump": 4.25, "speedPlausibility": 1.5, "headingConsistency": 0.2},
        detector_keys=("positionJump", "speedPlausibility", "headingConsistency"),
        score=4.25, score_norm=4.25, subject_pos_confidence=1.4999,
        sig_valid=True, cert_crl_status="active", station_type="vehicle",
        rssi_dbm=-78.246, emit_rssi=True, st_bbox=(1.0, 2.0, 3.0, 4.0),
        st_tstart=39.0, st_tend=39.0, duplicate_flag=False,
        evidence_msg_refs=("rpt_00042-m",),
        evidence_pdus=(bytes(range(41)),), evidence_profile_id="etsi_cam_en302637_2",
        cert_validity={"sig_valid": True, "not_expired": True, "not_revoked": True,
                       "chain_ok": True})
    base.update(kw)
    return _report.ReportInput(**base)


class ReportFormatContract:
    """The v1 report-format contract. Subclass, set :attr:`REF` or override :meth:`make`."""

    SLOT = "report_format"
    INTERFACE_VERSION = _report.INTERFACE_VERSION
    REF: str = None
    PARAMS: dict = {}
    SEED = 20260905

    waivers: dict = {}

    def declared_waivers(self) -> dict:
        if self.REF is None:
            return {}
        try:
            _engine()
            cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        except Exception:
            return {}
        return dict(getattr(cls, "conformance_waivers", None) or {})

    def waiver_for(self, check_id: str):
        return dict(self.declared_waivers(), **(self.waivers or {})).get(check_id)

    def env(self) -> dict:
        return {"config": _engine().ReadOnlyConfig(_engine().PipelineConfig(seed=self.SEED))}

    def make(self, **params):
        if self.REF is None:
            raise NotImplementedError(
                f"{type(self).__name__}: set REF = 'package.module:Class' (or a built-in registry "
                f"key), or override make()")
        p = dict(self.PARAMS)
        p.update(params)
        _engine()
        cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        _validate_params(cls, p)
        pid = _registry.plugin_id_of(cls, self.REF.rsplit(":", 1)[-1].lower())
        self._ns = RngNamespace(self.SEED, pid)
        return _registry.instantiate(cls, params=p, rng=self._ns, env=self.env())

    def reports(self) -> tuple:
        """The report set every check renders: a full one, a minimal one, an unsigned one, one with
        no evidence, and one where the receiver measured no RSSI."""
        return (sample_report(),
                sample_report(reason_codes=("certValidity",), detector_scores={"certValidity": 1.0},
                              detector_keys=("certValidity",), evidence_pdus=(), station_type=None,
                              emit_rssi=False, round_detection=False),
                sample_report(sig_valid=False, cert_crl_status="revoked"),
                sample_report(evidence_pdus=(), evidence_profile_id=None),
                sample_report(rssi_dbm=None))

    def render_all(self, fmt) -> tuple:
        return tuple(_canonical(fmt.render(r)) for r in self.reports())

    # -- R1 --------------------------------------------------------------------------------- #
    def check_R1_repeatable(self):
        """Two fresh instances render the same reports to identical canonical bytes.

        `ma/ma_reports.jsonl` is INSIDE `data_digest`, so this is not a style preference: a format
        that is not repeatable makes the run unreplayable while every exit code stays 0.
        """
        a, b = self.render_all(self.make()), self.render_all(self.make())
        assert a == b, f"two identical renders differ at report {_first_diff(a, b)}"
        return f"{len(a)} reports reproduced byte-identically"

    # -- R2 --------------------------------------------------------------------------------- #
    def check_R2_row_carries_no_oracle_field(self):
        """No rendered key is in `FORBIDDEN_FEATURE_KEYS`, at any nesting depth.

        The machine-checkable definition of leakage this repository already uses for its feature
        tables, applied to the file those tables are built from.
        """
        fmt = self.make()
        bad = set()
        for report in self.reports():
            _walk_keys(fmt.render(report), bad)
        offenders = sorted(k for k in bad if is_forbidden_feature_key(k))
        assert not offenders, (f"rendered row carries ground-truth key(s) {offenders}; "
                               f"ma/ma_reports.jsonl is MA-visible and ships with the dataset")
        return f"{len(bad)} distinct keys, none forbidden"

    # -- R3 --------------------------------------------------------------------------------- #
    def check_R3_oracle_input_does_not_change_the_row(self):
        """The anti-laundering arm: identical declared fields plus ground truth -> identical rows.

        Key names are not enough. A format that wrote the subject's true position into a field
        called `st_bbox`, or the attack type into `cert_crl_status`, passes `R2` perfectly. Only
        driving it over two inputs that differ ONLY in oracle attributes catches that.
        """
        fmt_a, fmt_b = self.make(), self.make()
        plain = self.reports()
        oracle = tuple(OracleReportInput(r, is_attacker=True, attack_type="ConstPos",
                                         falsified=True, true_x=r.st_bbox[0] + 500.0,
                                         true_y=r.st_bbox[1] - 500.0,
                                         reporter_true_id="veh_007", subject_true_id="veh_013",
                                         report_correctness="correct")
                       for r in plain)
        a = tuple(_canonical(fmt_a.render(r)) for r in plain)
        b = tuple(_canonical(fmt_b.render(r)) for r in oracle)
        assert a == b, (f"the rendered row MOVED when ground truth was attached to an otherwise "
                        f"identical input -- first difference at report {_first_diff(a, b)}")
        return "identical under an oracle-carrying input"

    # -- R4 --------------------------------------------------------------------------------- #
    def check_R4_required_keys_present(self):
        """Every row carries `REQUIRED_ROW_KEYS`, on every report shape.

        The engine reads these back to sort and stream, so a format that omits one does not produce
        an unusual dataset -- it produces a `KeyError` at write time, at the end of a run.
        """
        fmt = self.make()
        for i, report in enumerate(self.reports()):
            row = fmt.render(report)
            missing = [k for k in _report.REQUIRED_ROW_KEYS if k not in row]
            assert not missing, f"report {i} rendered without {missing}"
            assert row["report_id"] == report.report_id, \
                f"report_id was rewritten: {row['report_id']!r} != {report.report_id!r}"
        return f"all of {list(_report.REQUIRED_ROW_KEYS)} on {len(self.reports())} shapes"

    # -- R5 --------------------------------------------------------------------------------- #
    def check_R5_json_serialisable(self):
        """Rows survive the engine's own canonicalisation -- sorted keys, compact separators.

        `bytes`, `set`, `Decimal`, a dataclass and NaN all fail here, and every one of them is a
        plausible thing to put in a report row. NaN is called out separately because
        `json.dumps` ACCEPTS it by default and emits bare `NaN`, which is not JSON and which no
        strict parser downstream will read.
        """
        fmt = self.make()
        for i, report in enumerate(self.reports()):
            row = fmt.render(report)
            try:
                blob = json.dumps(row, sort_keys=True, separators=(",", ":"), allow_nan=False)
            except (TypeError, ValueError) as e:
                raise AssertionError(f"report {i} did not serialise: {type(e).__name__}: {e}") \
                    from None
            assert json.loads(blob) is not None
        return "canonical JSON on every shape, no NaN"

    # -- R6 --------------------------------------------------------------------------------- #
    def check_R6_draws_no_random_number(self):
        """ZERO draws while rendering, by both instruments. Same reasoning as `P3`, one slot over --
        including the module-level hole, which the global-state comparison is what closes."""
        fmt = self.make()
        before = random.getstate()
        with DrawCounter() as c:
            self.render_all(fmt)
        after = random.getstate()
        assert c.count == 0, (f"drew {c.count} random number(s) while rendering a report; the row "
                              f"lands inside data_digest")
        assert after == before, ("advanced the PROCESS-GLOBAL `random` generator while rendering a "
                                 "report; every subsequent engine draw shifts with it")
        return "0 draws, global generator state unmoved"

    # -- R7 --------------------------------------------------------------------------------- #
    def check_R7_no_io(self):
        """No filesystem write, network connection or subprocess while rendering. Detects accidental
        I/O; does not contain hostile I/O (PEP 578 is not sandboxing)."""
        with audit_guard():
            self.render_all(self.make())
        return "no denied I/O"


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #
#: Per-field round-trip tolerance for `P6`, in engine units. Position: 1/10 microdegree is ~1.1 cm of
#: latitude, so 5 cm is the quantisation plus a decade of headroom. Speed: ETSI's 0.01 m/s step.
#: Heading: ETSI's 0.1 degree step. A codec declaring `lossless` is held to exact equality instead.
_ROUND_TRIP_TOLERANCE = {"x": 0.05, "y": 0.05, "speed": 0.02, "heading": 0.2}

#: The declared field vocabulary of `GenerationInput`, for `P4`'s undeclared-attribute arm.
_EGO_FIELDS = frozenset(_profile.GenerationInput.__slots__)

#: The declared field vocabulary of `ReportInput`, for `OracleReportInput`'s reconstruction.
_REPORT_FIELDS = tuple(_report.ReportInput.__slots__)


class _RecordingEgo:
    """Records every attribute name a generation rule touches, and refuses every write.

    A CAPABILITY check, not a name check -- the same instrument the detector contract's
    `RecordingProxy` is, one seam over.
    """

    __slots__ = ("_ego", "_touched")

    def __init__(self, ego, touched):
        object.__setattr__(self, "_ego", ego)
        object.__setattr__(self, "_touched", touched)

    def __getattr__(self, name):
        object.__getattribute__(self, "_touched").add(name)
        return getattr(object.__getattribute__(self, "_ego"), name)

    def __setattr__(self, name, value):
        raise AttributeError(f"GenerationInput is frozen; a profile may not write {name!r}")


def _with_floor(ego, floor: float):
    """The same ego state with a different congestion floor. Rebuilt rather than mutated: the DTO is
    frozen, and a contract that mutated it would be grading a different object from the engine's."""
    if isinstance(ego, OracleGenerationInput):
        out = OracleGenerationInput(ego)
        object.__setattr__(out, "min_interval_s", float(floor))
        for k in OracleGenerationInput.__slots__:
            if hasattr(ego, k):
                object.__setattr__(out, k, getattr(ego, k))
        return out
    return _profile.GenerationInput(t=ego.t, x=ego.x, y=ego.y, speed=ego.speed,
                                    heading=ego.heading, dt=ego.dt, min_interval_s=float(floor),
                                    is_rsu=ego.is_rsu, station_type=ego.station_type)


def _canonical(row) -> bytes:
    """The engine's own canonicalisation, so two rows compare exactly as the dataset writer would."""
    return json.dumps(dict(row), sort_keys=True, separators=(",", ":"),
                      default=str).encode("utf-8")


def _walk_keys(obj, out: set) -> None:
    if isinstance(obj, dict):
        for k, v in obj.items():
            out.add(str(k))
            _walk_keys(v, out)
    elif isinstance(obj, (list, tuple)):
        for v in obj:
            _walk_keys(v, out)


#: Claims section 6.5 forbids. A profile may SAY "NOT conformance-tested" -- that is the honest
#: sentence and it is what every built-in says -- so the check is for a POSITIVE assertion, which
#: means looking at what precedes the term rather than merely finding it.
_FORBIDDEN_CLAIMS = ("conformance-tested", "conformance tested", "certified",
                     "plugtests-validated", "plugtests validated", "fully compliant")
_NEGATIONS = ("not ", "no ", "never ", "isn't ", "is not ", "neither ", "nor ")


def _asserts_conformance(claim) -> list:
    """The forbidden terms a claim ASSERTS, negated occurrences excluded."""
    text = json.dumps(claim, sort_keys=True, default=str).lower()
    out = []
    for term in _FORBIDDEN_CLAIMS:
        start = 0
        while True:
            i = text.find(term, start)
            if i < 0:
                break
            start = i + len(term)
            before = text[max(0, i - 12):i]
            if not any(n in before for n in _NEGATIONS):
                out.append(term)
                break
    return sorted(set(out))


def _first_diff(a, b):
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return f"{i}: {x!r} != {y!r}"
    return f"length {len(a)} != {len(b)}"


def _config_fields(cls) -> dict:
    fn = getattr(cls, "config_fields", None)
    try:
        return dict(fn() or {}) if callable(fn) else {}
    except Exception:                                       # pragma: no cover - defensive
        return {}


def _validate_params(cls, params) -> None:
    """The same gate `run._validate_slot_plugin` applies, so `make()` and the engine agree on what
    is a legal parameter set -- a contract that accepted params the engine refuses would be lying."""
    spec = _config_fields(cls)
    for k, v in (params or {}).items():
        fs = spec.get(k)
        if fs is None:
            if not spec:
                continue
            raise ConfigError(f"{cls.__name__} declares no field {k!r} (declared: {sorted(spec)})")
        fs.validate(k, v)
    own = getattr(cls, "validate_params", None)
    if callable(own):
        try:
            own(dict(params or {}))
        except TypeError:                                   # pragma: no cover - instance method
            pass


__all__ = ["OracleGenerationInput", "OracleReportInput", "OFFERED_AIRTIME_LADDER",
           "PROTOCOL_CHECKS", "REPORT_CHECKS", "ProtocolProfileContract", "ReportFormatContract",
           "SUITE_VERSION", "CBR_LADDER", "FRAME_SIZES", "ego_track", "sample_claim",
           "sample_report"]
