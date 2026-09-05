"""`etsi_its_g5` -- the shipped protocol profile, expressed THROUGH the seam rather than beside it.

Everything ETSI ITS-G5 decides, in one object: the wire format (EN 302 637-2 / -3, TS 103 300-3 via
:mod:`scms_sim_ref.codecs.etsi`), when a station transmits (EN 302 637-2 clause 6.1.3), how often it
is then allowed to (TS 102 687 reactive DCC), what a frame weighs including its TS 103 097 envelope,
and what that weight costs on an IEEE 802.11-2020 OFDM channel at 10 MHz.

**This file contains no new physics and no new constants.** Every number comes from
:mod:`scms_sim_ref.codecs.etsi_rules`, which is itself graded against `datagen/refdata/`. What this
file adds is the SHAPE: two thin adapters that give the standard's own state machines the seam's
signatures, and one class that answers the nine :data:`~scms_sim_ref.api.profile.PROFILE_SPEC`
methods. It is resolved by the same three-tier resolver as a third-party stack, gets the same
signature check, the same capability screening, the same content-hash lock entry and the same
conformance contract -- which is the whole test of whether the seam is real.

Nothing here draws a random number and nothing here reads a clock, a file or an environment
variable. That is what lets the built-in profile be switched on without perturbing a single RNG
stream, which is the constraint every opt-in feature in this engine is held to.
"""
from __future__ import annotations

from collections import Counter

from ..api import profile as _p
from ..api.codec import SIGNER_FORMS
from ..api.fields import FieldSpec
from . import etsi_rules as ER

#: What the engine charges a frame when no codec is on the path (`run.py::NATIVE_WIRE_SIZE_BYTES`).
LEGACY_WIRE_SIZE_BYTES = 300


class EtsiCamGenerationState(ER.CamGenerationState):
    """EN 302 637-2 clause 6.1.3, given the seam's `evaluate(ego)` signature.

    A subclass rather than a wrapper so the standard's own state machine -- and the tests that grade
    it against `etsi_cam_dcc.json` -- stay exactly where they are. The DCC floor arrives on
    :attr:`~scms_sim_ref.api.profile.GenerationInput.min_interval_s`, and `0.0` (the seam's "no
    congestion control") lands on `max(T_GenCamMin, 0.0)` inside the parent, i.e. on the CAM
    service's own floor. That equivalence is why the engine can stop knowing what `T_GenCamMin` is.
    """

    __slots__ = ()

    def evaluate(self, ego) -> str:                       # type: ignore[override]
        return super().evaluate(ego.t, ego.x, ego.y, ego.speed, ego.heading,
                                max(0.0, float(ego.min_interval_s)))


class EtsiReactiveDccState(ER.ReactiveDcc):
    """TS 102 687 reactive DCC, given the seam's `min_interval_s()` signature."""

    __slots__ = ()

    def min_interval_s(self) -> float:
        return self.t_gen_cam_floor


class EtsiItsG5Profile(_p.ProtocolProfileBase):
    """The built-in stack. Every layer is independently off, and all of them are off by default.

    The five parameters are not five features bolted together -- they are which LAYERS of one
    profile this run turns on, and the profile refuses an incoherent combination the way the standard
    does: reactive DCC acts by lengthening the CAM service's minimum interval, so `congestion`
    without `generation` has no interval to lengthen and is a refusal rather than a silent no-op.
    """

    interface_version = _p.INTERFACE_VERSION
    plugin_id = "etsi_its_g5"
    profile_id = "etsi_its_g5"

    #: Declared knobs, in exactly the shape `config_schema()` emits, so `--dump-config-schema`, the
    #: GUI's advanced panel and the copilot can all show and validate them with no code per field.
    PARAM_FIELDS = {
        "signer": FieldSpec("str", "digest", "TS 103 097 signer form used for the frame's wire "
                            "size", options=tuple(SIGNER_FORMS), group="Protocol"),
        "generation": FieldSpec("bool", False, "EN 302 637-2 clause 6.1.3 CAM generation rules",
                                group="Protocol"),
        "congestion": FieldSpec("bool", False, "TS 102 687 reactive DCC over the measured CBR "
                                "(requires generation)", group="Protocol"),
        "latency": FieldSpec("bool", False, "per-packet propagation + access + stack latency",
                             group="Protocol"),
        "stack_latency_s": FieldSpec("float", ER.STACK_LATENCY_S, "facilities+networking+security "
                                     "stack latency, derived from the DLR Cohda MK5 field campaign",
                                     lo=0.0, hi=1.0, unit="s", group="Protocol"),
        "legacy_wire_size_bytes": FieldSpec("int", LEGACY_WIRE_SIZE_BYTES, "frame length charged "
                                            "when no codec is on the path", lo=1, hi=4096,
                                            unit="B", group="Protocol"),
    }

    def __init__(self, *, params=None, rng=None, env=None):
        p = dict(params or {})
        unknown = sorted(set(p) - set(self.PARAM_FIELDS))
        if unknown:
            raise ValueError(f"etsi_its_g5 does not accept params {unknown}; "
                             f"known: {sorted(self.PARAM_FIELDS)}")
        for k, v in sorted(p.items()):
            self.PARAM_FIELDS[k].validate(f"plugins.protocol_profile.params.{k}", v)
        self.params = p
        self.signer = str(p.get("signer", "digest"))
        if self.signer not in SIGNER_FORMS:
            raise ValueError(f"etsi_its_g5: signer must be one of {SIGNER_FORMS}, "
                             f"got {self.signer!r}")
        self._generation = bool(p.get("generation", False))
        self._congestion = bool(p.get("congestion", False))
        self._latency = bool(p.get("latency", False))
        self._stack_s = float(p.get("stack_latency_s", ER.STACK_LATENCY_S))
        self._legacy_size = int(p.get("legacy_wire_size_bytes", LEGACY_WIRE_SIZE_BYTES))
        if self._congestion and not self._generation:
            raise ValueError(
                "etsi_its_g5: congestion=True requires generation=True -- reactive DCC acts by "
                "lengthening the CAM service's minimum inter-CAM interval, and with no generation "
                "rules there is no interval to lengthen. Accepting it would put a congestion "
                "controller in the manifest of a run where it did nothing.")
        # THE CODEC IS INJECTED, NOT RESOLVED HERE. `run.py::build_codec` owns the codec slot's
        # integrity sentinel, its name-hijack refusal and its provenance lock entry; a profile that
        # resolved codecs itself would move all three inside a plugin. A THIRD-PARTY profile is of
        # course free to construct its own codec object -- it owns its own code, and its package
        # hash covers it -- which is exactly the difference in trust the two cases deserve.
        self._codec = (env or {}).get("message_codec")

    @classmethod
    def from_plugin(cls, *, params=None, rng=None, env=None):
        return cls(params=params, rng=rng, env=env)

    @classmethod
    def config_fields(cls) -> dict:
        return dict(cls.PARAM_FIELDS)

    # -- declarations ------------------------------------------------------------------ #
    def capabilities(self) -> frozenset:
        caps = {_p.CAP_AIRTIME}
        if self._codec is not None:
            caps.add(_p.CAP_CODEC)
            caps.add(_p.CAP_SECURITY_ENVELOPE)
        if self._generation:
            caps.add(_p.CAP_GENERATION)
        if self._congestion:
            caps.add(_p.CAP_CONGESTION)
        if self._latency:
            caps.add(_p.CAP_LATENCY)
        return frozenset(caps)

    def standards_claim(self) -> dict:
        """Named standards and clauses; no claim of conformance testing, because none was done."""
        layers = {
            "access": "IEEE 802.11-2020 OFDM at 10 MHz, 6 Mb/s (QPSK 1/2), AIFS(AC_BE) + mean "
                      "initial backoff over CWmin=15; airtime and access delay only, not a MAC "
                      "simulation",
            "generation": ("ETSI EN 302 637-2 V1.4.1 clause 6.1.3 CAM generation frequency "
                           "management (4 m / 4 deg / 0.5 m/s, T_GenCamMin 0.1 s, T_GenCamMax "
                           "1.0 s, N_GenCam 3)" if self._generation else None),
            "congestion": ("ETSI TS 102 687 V1.2.1 reactive DCC, the five-state half-open table "
                           "over the station's own measured CBR" if self._congestion else None),
            "security_envelope": ("ETSI TS 103 097 signer alternation, MEASURED envelope lengths "
                                  "only -- the serialisation is not COER"
                                  if self._codec is not None else None),
        }
        claim = {"profile": "ETSI ITS-G5 (EN 302 665 architecture), partial",
                 "layers": {k: v for k, v in sorted(layers.items()) if v},
                 "conformance": "NOT conformance-tested, NOT certified, NOT Plugtests-validated; "
                                "these are implementations of published rules, graded against "
                                "datagen/refdata/ transcriptions of them"}
        if self._codec is not None:
            claim["message"] = dict(self._codec.standards_claim())
        return claim

    # -- the five layers ----------------------------------------------------------------- #
    def codec(self):
        return self._codec

    def new_generation_state(self):
        return EtsiCamGenerationState() if self._generation else None

    def new_congestion_state(self):
        return EtsiReactiveDccState() if self._congestion else None

    def wire_size_bytes(self, claim, signer: str = "digest") -> int:
        """Payload octets plus the TS 103 097 envelope for `signer`.

        Delegated to the codec, which measured its own envelope against a real COER encoder
        (`etsi.SECURITY_ENVELOPE_BYTES`). With no codec there is nothing better to ask and the
        engine's own legacy constant stands -- including its wrongness, which is the point of
        being able to compare the two.
        """
        if self._codec is None:
            return self._legacy_size
        return int(self._codec.wire_size_bytes(claim, signer))

    def frame_airtime_s(self, size_bytes: int) -> float:
        return ER.frame_airtime_s(size_bytes)

    def channel_busy_ratio(self, offered_airtime_s: float, window_s: float) -> float:
        return ER.channel_busy_ratio(offered_airtime_s, window_s)

    def link_latency_s(self, distance_m: float, cbr: float, size_bytes: int) -> float:
        return ER.link_latency_s(distance_m, cbr, size_bytes, self._stack_s)

    # -- what it actually did -------------------------------------------------------------- #
    def report(self, measured) -> dict:
        """`counts["protocol"]` for the layers THIS profile owns.

        The engine counts, the profile says what the counts mean. Everything here is an OBSERVATION
        of the run -- the CAM rate is counted triggers over counted gaps, the latency quantiles are
        exact over every delivered frame -- with the profile's own declared constants alongside, so
        a reader can see both the rule and what it produced. Manifest-only by construction.
        """
        out: dict = {}
        if measured.codec_stats:
            out["wire"] = self._wire_block(measured)
        if measured.generation_states:
            cam = _p.generation_report(measured, ER.DYNAMICS_TRIGGERS)
            cam["cams"] = cam.pop("messages")
            cam["t_gen_cam_min_s"] = ER.T_GEN_CAM_MIN_S
            cam["t_gen_cam_max_s"] = ER.T_GEN_CAM_MAX_S
            cam["thresholds"] = {"position_m": ER.CAM_TRIGGER_POSITION_M,
                                 "heading_deg": ER.CAM_TRIGGER_HEADING_DEG,
                                 "speed_mps": ER.CAM_TRIGGER_SPEED_MPS}
            out["cam_generation"] = cam
        csum, cn, cmax, window = measured.cbr
        if cn:
            # The engine MEASURED this; the profile is what can say what the number means, which is
            # why the reactive machine's first breakpoint travels with it. A run under a different
            # profile gets the engine's fallback block WITHOUT this key -- publishing a TS 102 687
            # breakpoint beside a C-V2X run's CBR would be a constant pretending to be a finding.
            out["cbr"] = {"mean": round(csum / cn, 6), "max": round(cmax, 6), "samples": cn,
                          "window_s": window,
                          "dcc_breakpoint": ER.DCC_REACTIVE_STATES[0][2]}
        if measured.congestion_states:
            agg: Counter = Counter()
            for d in measured.congestion_states:
                agg.update(getattr(d, "state_counts", {}) or {})
            out["dcc"] = {"states": dict(sorted(agg.items())),
                          "stations": len(measured.congestion_states),
                          "table": [list(r) for r in ER.DCC_REACTIVE_STATES]}
        lat = self._latency_block(measured)
        if lat:
            out["latency_ms"] = lat
        return out

    def _wire_block(self, measured) -> dict:
        w = dict(measured.codec_stats)
        w["profile"] = getattr(self._codec, "profile_id", "")
        w["signer"] = self.signer
        wsum, wn = measured.wire
        if wn:
            mean = wsum / wn
            w["mean_wire_bytes"] = round(mean, 3)
            w["frames_sized"] = wn
            # The number that settles the 2.05x airtime disagreement, computed on the MEASURED
            # length rather than on either engine's assumption.
            w["mean_frame_airtime_us"] = round(self.frame_airtime_s(round(mean)) * 1e6, 3)
            w["java_assumed_airtime_us"] = round(
                ER.ppdu_airtime_s(ER.JAVA_ASSUMED_MPDU_BYTES) * 1e6, 1)
            w["python_assumed_airtime_us"] = round(ER.PYTHON_ASSUMED_FRAME_AIRTIME_S * 1e6, 1)
        return w

    def _latency_block(self, measured) -> dict:
        lsum, ln, lmin, lmax = measured.latency
        if not ln:
            return {}
        # Quantiles straight off the microsecond histogram: walk the sorted bins accumulating counts
        # until the target rank is passed. Exact to 1 us, allocation-free.
        bins = sorted((measured.latency_hist or {}).items())

        def _pct(q):
            target, seen = q * ln, 0
            for us, n in bins:
                seen += n
                if seen >= target:
                    return round(us / 1e3, 6)
            return round(bins[-1][0] / 1e3, 6) if bins else 0.0

        return {"mean": round(lsum / ln * 1e3, 6), "min": round(lmin * 1e3, 6),
                "p50": _pct(0.50), "p90": _pct(0.90), "p99": _pct(0.99),
                "max": round(lmax * 1e3, 6), "samples": ln,
                "reference_band_ms": [b * 1e3 for b in ER.LATENCY_REFERENCE_BAND_S],
                "reference": "v2x_awareness.latency_p50_ms_80211p (DLR Cohda MK5, ITS WC 2021)",
                "stack_constant_ms": round(self._stack_s * 1e3, 6)}


__all__ = ["EtsiCamGenerationState", "EtsiItsG5Profile", "EtsiReactiveDccState",
           "LEGACY_WIRE_SIZE_BYTES"]
