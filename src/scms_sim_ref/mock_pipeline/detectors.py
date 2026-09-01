"""The built-in misbehaviour checks and the built-in fusion, on the plugin interfaces.

**PLUGIN-ARCHITECTURE.md section 8.3, executed: code motion only.** Every expression below is the
one that used to sit inline in `run.py`'s reception loop, moved verbatim -- same operands, same
order, same parenthesisation -- so the extraction cannot move a float. What changes is only *who
holds it*: a registry entry with a name, a declared reason code, declared config fields and a
declared randomness namespace, instead of a statement in a 90-line block that could only be
extended by forking the engine.

The decomposition is F2MD's (checks -> score vector -> fusion -> report format); the registration is
not (see `api/registry.py`). The POLARITY IS THIS REPO'S, and it is the opposite of F2MD's:
``>= 1.0`` is violating. See `api/detect.py`.

Three properties of this module are load-bearing:

1. **Registration order IS evaluation order IS `DET_KEYS` order, and that order is digest-bearing.**
   Precisely: the report rows are canonicalised with SORTED keys, so key insertion order never
   reaches the bytes -- what does is the fusion's stable sort `sorted(fired, key=lambda k: -det[k])`
   over the tuple below. Two checks firing at the same score are ranked by their position here, and
   the winner becomes `reason_codes[0]` / `detector_outputs[0].check_id` / `detector_score`. The
   tuple at the bottom of this file is therefore the same sequence, in the same order, that the
   inline block evaluated.
2. **Two checks are GATED** (`vruImpersonation` on station types being in play, `denmPlausibility` on
   the DENM layer). They are registered unconditionally and enter the suite conditionally -- the
   in-tree "registering is not enabling" precedent that keeps the default digest byte-identical.
3. **The built-ins read the engine's own state dict directly** (`h`, `streak`, `kf`), because that is
   what they did before and moving that state would move the goldens. A third-party check -- and,
   since 2026-08-31, a third-party FUSION, which used to be handed the raw dict like a built-in --
   gets `NamespacedState` instead, whose mapping interface refuses those keys. Read that class's
   docstring for what the wrapper does and does not guarantee: it is a discipline, not a capability
   boundary, and `docs/realism/DETECTOR-PLUGIN.md` section 2 states why no in-process wrapper can be
   one.
"""
from __future__ import annotations

import math

from ..api import registry as _registry
from ..api.detect import (CAP_EVENT, CAP_HISTORY, CAP_LEGACY_GLOBAL_RNG, CAP_LEGACY_RAW_COMPARE,
                          CAP_MAP, CAP_NEIGHBOURHOOD, CAP_SOFT, CAP_STATEFUL, CheckBase,
                          FusionBase, ReportDecision)
from ..api.fields import FieldSpec


def _ang_diff(a: float, b: float) -> float:
    """Smallest absolute angle between two bearings, in degrees. (`run.py:1811`, transcribed
    character for character -- a helper this small is still an arithmetic site.)"""
    d = abs((a - b) % 360.0)
    return d if d <= 180.0 else 360.0 - d


class _BuiltinCheck(CheckBase):
    """Shared plumbing for the in-tree checks.

    A built-in's knobs ARE the engine's own `PipelineConfig` knobs -- they were `cfg.<field>` reads
    in the inline block, they already appear in `manifest["config"]`, and they already have
    `_FIELD_META` entries, argparse flags and GUI widgets. `cfg_fields` maps the check's declared
    parameter name to the config field it mirrors, so `build_checks` can hand the run's ACTUAL value
    over as `params` without duplicating a single default. A third party declares `FieldSpec`s
    instead and gets its knobs into the schema, the CLI and the manifest the same way.

    `legacy_raw_compare` is declared by every built-in: section 4.1 has the engine round a returned
    score to the declared `precision` before the `>= 1.0` compare (that comparison is a cliff), but
    the goldens were pinned on the RAW comparison. Rounding the built-ins first would move
    `0bd93655...`, so the grandfathering is declared, refused from third parties, and recorded.
    """

    #: declared param name -> PipelineConfig field name
    cfg_fields: dict = {}
    #: extra capabilities beyond the grandfathering every built-in carries
    caps: tuple = ()

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        for name in self.cfg_fields:
            if name not in self.params:
                raise KeyError(f"{type(self).__name__}: missing param {name!r}")
        # Bound once, at construction: the values are fixed for the run, and a per-message dict
        # lookup on the hottest loop in the engine buys nothing. `params` is still passed to
        # `evaluate` because the CONTRACT passes it -- a third party may prefer to read it there.
        self.p = {k: self.params[k] for k in self.cfg_fields}

    def config_fields(self):
        return {name: FieldSpec("float", None, f"mirrors PipelineConfig.{field}",
                                group="Detection")
                for name, field in sorted(self.cfg_fields.items())}

    def capabilities(self):
        return frozenset((CAP_LEGACY_RAW_COMPARE,) + tuple(self.caps))


# --------------------------------------------------------------------------- #
# 1-6: the lagged-reference motion checks (`detectors()`, run.py:3179-3198)
# --------------------------------------------------------------------------- #
# The inline helper computed six residuals from ONE lagged reference in a single pass. Split into
# six checks, each recomputing the two or three shared intermediates it needs from the same
# `Observation` fields. That recomputation is float-exact -- `disp`, `dtt`, `tol`, `avg_v` are
# per-expression results of the same operands in the same order, not accumulations -- which is what
# makes the split code motion rather than a numerical change.
#
# `tol` is the uncertainty scale: the broadcast position confidence, floored at half the consistency
# threshold so a sender that claims implausibly tiny uncertainty cannot buy itself a free pass.

class PositionSpeedInconsistency(_BuiltinCheck):
    """Claimed displacement inconsistent with claimed speed over the interval."""

    plugin_id = "position_speed_inconsistency"
    reason_code = "positionSpeedInconsistency"
    vru_suppressed = True
    caps = (CAP_HISTORY,)
    cfg_fields = {"z": "detector_z_threshold", "consistency_threshold_m": "consistency_threshold_m"}

    def evaluate(self, obs, state, params, rng) -> float:
        if obs.first_sight:
            return 0.0
        p = self.p
        dtt = max(1e-6, obs.t - obs.ref_t)
        disp = math.hypot(obs.claimed_x - obs.ref_x, obs.claimed_y - obs.ref_y)
        tol = max(obs.pos_conf, 0.5 * p["consistency_threshold_m"])
        avg_v = 0.5 * (obs.claimed_speed + obs.ref_speed)   # accel/decel-safe interval average
        jerk_slack = 0.3 * abs(obs.claimed_speed - obs.ref_speed) * dtt  # stop-and-go tolerance
        return max(0.0, abs(disp - avg_v * dtt) - jerk_slack) / (p["z"] * tol)


class PositionJump(_BuiltinCheck):
    """Implausibly large position change between consecutive claims."""

    plugin_id = "position_jump"
    reason_code = "positionJump"
    vru_suppressed = True
    caps = (CAP_HISTORY,)
    cfg_fields = {"z": "detector_z_threshold", "consistency_threshold_m": "consistency_threshold_m"}

    def evaluate(self, obs, state, params, rng) -> float:
        if obs.first_sight:
            return 0.0
        p = self.p
        dtt = max(1e-6, obs.t - obs.ref_t)
        disp = math.hypot(obs.claimed_x - obs.ref_x, obs.claimed_y - obs.ref_y)
        tol = max(obs.pos_conf, 0.5 * p["consistency_threshold_m"])
        avg_v = 0.5 * (obs.claimed_speed + obs.ref_speed)
        return disp / (avg_v * dtt + p["z"] * tol + p["consistency_threshold_m"])


class HeadingInconsistency(_BuiltinCheck):
    """Claimed heading vs the bearing implied by consecutive positions.

    Deliberately measured over a ONE-STEP baseline (the most recent prior fix), not the lagged
    reference: a 1.5 s baseline spans road turns and reads them as heading lies.
    """

    plugin_id = "heading_inconsistency"
    reason_code = "headingInconsistency"
    vru_suppressed = True
    caps = (CAP_HISTORY,)
    cfg_fields = {"heading_threshold_deg": "heading_threshold_deg", "dt": "dt"}

    def evaluate(self, obs, state, params, rng) -> float:
        if obs.first_sight:
            return 0.0
        p = self.p
        dprev = math.hypot(obs.claimed_x - obs.prev_x, obs.claimed_y - obs.prev_y)
        if (obs.claimed_speed > 3.0 and (obs.t - obs.prev_t) <= 2.0 * p["dt"]
                and dprev > max(5.0, 2.5 * obs.pos_conf)):
            bearing = math.degrees(math.atan2(obs.claimed_y - obs.prev_y,
                                              obs.claimed_x - obs.prev_x)) % 360.0
            return _ang_diff(obs.claimed_heading, bearing) / p["heading_threshold_deg"]
        return 0.0


class StaleOrReplay(_BuiltinCheck):
    """Claim generation-time older than the staleness threshold (stale / replayed).

    TWO ARMS, `max`-combined exactly as the inline code combined them: the frozen-position arm
    (a position that has not moved while the sender still claims to be moving is, among other
    things, a replay signature) and the generation-time arm.
    """

    plugin_id = "stale_or_replay"
    reason_code = "staleOrReplay"
    caps = (CAP_HISTORY,)
    cfg_fields = {"stale_max_s": "stale_max_s"}

    def evaluate(self, obs, state, params, rng) -> float:
        frozen = 0.0
        if (not obs.first_sight and obs.claimed_x == obs.ref_x and obs.claimed_y == obs.ref_y
                and obs.claimed_speed > 0.5):
            frozen = 1.2
        return max(frozen, (obs.t - obs.gen_time) / self.p["stale_max_s"])


class ConstantPositionFrozen(_BuiltinCheck):
    """Position frozen across consecutive CAMs while still claiming to move."""

    plugin_id = "constant_position_frozen"
    reason_code = "constantPositionFrozen"
    vru_suppressed = True
    caps = (CAP_HISTORY,)

    def evaluate(self, obs, state, params, rng) -> float:
        if (not obs.first_sight and obs.claimed_x == obs.ref_x and obs.claimed_y == obs.ref_y
                and obs.claimed_speed > 0.5):
            return 1.5
        return 0.0


class ImplausibleAcceleration(_BuiltinCheck):
    """Implied acceleration exceeds a physical limit."""

    plugin_id = "implausible_acceleration"
    reason_code = "implausibleAcceleration"
    vru_suppressed = True
    caps = (CAP_HISTORY,)
    cfg_fields = {"max_accel_mps2": "max_accel_mps2"}

    def evaluate(self, obs, state, params, rng) -> float:
        if obs.first_sight:
            return 0.0
        dtt = max(1e-6, obs.t - obs.ref_t)
        return (abs(obs.claimed_speed - obs.ref_speed) / dtt) / self.p["max_accel_mps2"]


# --------------------------------------------------------------------------- #
# 7-12: the radio / credential checks (run.py:3458-3475, inline)
# --------------------------------------------------------------------------- #
class SybilCoLocation(_BuiltinCheck):
    """Many distinct certificates at the same point+heading (ghost cluster).

    The cell count is aggregate MA-visible context computed by the engine over the step's claimed
    positions -- `obs.neighbourhood["cell_cert_count"]`, never a per-vehicle truth.
    """

    plugin_id = "sybil_colocation"
    reason_code = "sybilCoLocation"
    caps = (CAP_NEIGHBOURHOOD,)
    cfg_fields = {"sybil_min_certs": "sybil_min_certs"}

    def evaluate(self, obs, state, params, rng) -> float:
        return obs.neighbourhood["cell_cert_count"] / self.p["sybil_min_certs"]


class AcceptanceRangeThreshold(_BuiltinCheck):
    """Claimed position lies beyond the receiver's declared radio reach.

    `obs.rx_reach_m` is the CHANNEL MODEL's declared reach for this receiver, which is what makes it
    a declared input rather than a leak -- and what stops a model that understates its reach from
    silently making this check wrong for every honest long link (conformance C8).
    """

    plugin_id = "acceptance_range"
    reason_code = "acceptanceRangeThreshold"
    cfg_fields = {"art_max_m": "art_max_m"}

    def evaluate(self, obs, state, params, rng) -> float:
        return max(0.0, math.hypot(obs.claimed_x - obs.rx_x, obs.claimed_y - obs.rx_y)
                   - obs.rx_reach_m) / self.p["art_max_m"]


class BeaconFrequency(_BuiltinCheck):
    """CAM rate above the plausible beacon rate (flooding / DoS)."""

    plugin_id = "beacon_frequency"
    reason_code = "beaconFrequency"
    cfg_fields = {"freq_max": "freq_max"}

    def evaluate(self, obs, state, params, rng) -> float:
        return obs.msg_count / self.p["freq_max"]


class SignatureVerification(_BuiltinCheck):
    """Message signature failed to verify.

    Scored by the engine's signature-failure arm rather than here: when a signature fails, the
    content cannot be trusted at all, so every plausibility score is suppressed and only this one is
    raised. Keeping the arm in the engine keeps that suppression a single, auditable statement; this
    class owns the reason code, the column, the documentation and the registry slot.
    """

    plugin_id = "signature_verification"
    reason_code = "signatureVerification"

    def evaluate(self, obs, state, params, rng) -> float:
        return 0.0 if obs.sig_ok else 1.5


class CertValidity(_BuiltinCheck):
    """Certificate presented outside its validity window."""

    plugin_id = "cert_validity"
    reason_code = "certValidity"

    def evaluate(self, obs, state, params, rng) -> float:
        return 1.5 if (obs.t > obs.cert_valid_to + 1.0 or obs.t < obs.cert_valid_from - 1.0) else 0.0


class MapOffRoad(_BuiltinCheck):
    """Claimed position far from any road (HD-map plausibility check)."""

    plugin_id = "map_offroad"
    reason_code = "mapOffRoad"
    vru_suppressed = True                    # VRUs legitimately travel off the road centreline
    caps = (CAP_MAP,)
    cfg_fields = {"offroad_tol_m": "offroad_tol_m"}

    def evaluate(self, obs, state, params, rng) -> float:
        return obs.map_offroad_m / self.p["offroad_tol_m"]


# --------------------------------------------------------------------------- #
# 13-14: the two GATED checks
# --------------------------------------------------------------------------- #
class VruImpersonation(_BuiltinCheck):
    """Beacon self-declares VRU yet moves at vehicle speed (VRU-impersonation spoof).

    Scored ONLY for a beacon that declares `station_type="vru"` -- for anything else it is 0.0 and
    the vehicle-kinematic checks above do the work. TWO ARMS, `max`-combined:

    * SPEED -- a plausible VRU beacon claims a few m/s, so a CLAIMED speed above the cyclist bound
      is a vehicle. Catches an impersonator that otherwise drives honestly.
    * POSITION -- a genuine VRU moves smoothly, so the displacement of its claimed position since
      the lagged reference implies at most a VRU-grade speed. Catches the slow-and-teleporting
      impersonator the speed arm alone misses. The tolerance carries the VRU bound over the
      interval PLUS the broadcast confidence PLUS a full multipath-outlier magnitude, so GNSS
      jitter on a real VRU can never push it over.
    """

    plugin_id = "vru_impersonation"
    reason_code = "vruImpersonation"
    gate = "station_type"
    caps = (CAP_HISTORY,)
    cfg_fields = {"vru_max_plausible_speed_mps": "vru_max_plausible_speed_mps",
                  "z": "detector_z_threshold",
                  "consistency_threshold_m": "consistency_threshold_m",
                  "gps_outlier_mag_m": "gps_outlier_mag_m", "dt": "dt"}

    def evaluate(self, obs, state, params, rng) -> float:
        if obs.station_type != "vru":
            return 0.0
        p = self.p
        vmax = p["vru_max_plausible_speed_mps"]
        speed_arm = max(0.0, obs.claimed_speed) / vmax
        dtt_v = max(p["dt"], obs.t - obs.ref_t)
        vru_allow = (vmax * dtt_v
                     + p["z"] * max(obs.pos_conf, 0.5 * p["consistency_threshold_m"])
                     + p["gps_outlier_mag_m"])
        jump_arm = math.hypot(obs.claimed_x - obs.ref_x,
                              obs.claimed_y - obs.ref_y) / max(1e-6, vru_allow)
        return max(speed_arm, jump_arm)


class DenmPlausibility(_BuiltinCheck):
    """A received event message (DENM) announces a brake/stationary hazard while its sender's own
    claimed speed shows it is still moving fast (phantom hazard).

    Reads MA-VISIBLE evidence only: the announced cause code and the sender's own claimed speed. It
    never consults the oracle real/fake flag, and a benign DENM (a genuinely slow or stopped sender)
    stays below its bound, so it can never cause a false revocation.

    The bound is EVENT-TYPE aware: an `emergencyElectronicBrakeLight` sender that has truly braked
    is at or below the post-brake bound, so one still moving normally above it announces a brake its
    own kinematics contradict even below the generic line.
    """

    plugin_id = "denm_plausibility"
    reason_code = "denmPlausibility"
    gate = "denm"
    msg_types = ("denm",)
    caps = (CAP_EVENT,)
    cfg_fields = {"denm_benign_max_speed_mps": "denm_benign_max_speed_mps",
                  "denm_implausible_speed_mps": "denm_implausible_speed_mps"}

    def evaluate(self, obs, state, params, rng) -> float:
        p = self.p
        thresh = (p["denm_benign_max_speed_mps"] + 0.5
                  if obs.event_type == "emergencyElectronicBrakeLight"
                  else p["denm_implausible_speed_mps"])
        return max(0.0, obs.claimed_speed) / thresh


# --------------------------------------------------------------------------- #
# 15: the SOFT check
# --------------------------------------------------------------------------- #
class KalmanConsistency(_BuiltinCheck):
    """Constant-velocity (alpha-beta) tracker residual -- a soft fusion feature, never a reason.

    SOFT by declaration (`soft = True`): it is scored and emitted into the `detnorm_*` fingerprint an
    ML fusion consumes, but it can never fire a report on its own, because a constant-velocity
    tracker false-positives on every curve.

    The only built-in that keeps its own per-link state. It writes `st["kf"]` -- a reserved key -- so
    the state advances for EVERY message including one whose signature failed, exactly as before.
    """

    plugin_id = "kalman_consistency"
    reason_code = "kalmanConsistency"
    soft = True
    caps = (CAP_STATEFUL, CAP_SOFT)
    cfg_fields = {"consistency_threshold_m": "consistency_threshold_m"}

    #: Django's `django_test_skips`, as the conformance runner implements it: a limitation the
    #: implementation DECLARES, with a written justification that travels into the report and from
    #: there into the manifest -- not an edit to the suite, and not a mute button.
    conformance_waivers = {
        "D7_state_namespacing":
            "BUILT-IN: `kf` is one of the four RESERVED per-link state keys, and this check is the "
            "component that owns it. The engine hands a built-in the raw state dict and a third "
            "party the NamespacedState wrapper, so D7 is grading this check against a boundary it "
            "is on the inside of. The property D7 exists to protect -- that a THIRD PARTY going "
            "through the state MAPPING cannot write `h`/`streak`/`touch`/`kf` -- is unaffected and "
            "is asserted separately against the wrapper itself.",
    }

    def evaluate(self, obs, state, params, rng) -> float:
        kf = state.get("kf")
        cx, cy, t = obs.claimed_x, obs.claimed_y, obs.t
        if kf is None:
            state["kf"] = (cx, cy, 0.0, 0.0, t)
            return 0.0
        ex, ey, evx, evy, et = kf
        dtk = max(1e-3, t - et)
        predx, predy = ex + evx * dtk, ey + evy * dtk
        rxk, ryk = cx - predx, cy - predy
        score = math.hypot(rxk, ryk) / (2 * self.p["consistency_threshold_m"] + obs.pos_conf)
        state["kf"] = (predx + 0.5 * rxk, predy + 0.5 * ryk,
                       evx + 0.3 * rxk / dtk, evy + 0.3 * ryk / dtk, t)
        return score


# --------------------------------------------------------------------------- #
# LAYER 2 -- the built-in fusion (run.py:3530-3538 + the DENM arm at 3411-3419)
# --------------------------------------------------------------------------- #
class StreakFusion(FusionBase):
    """`streak_v1` -- F2MD's `ThresholdApp`, as this engine has always spelled it.

    TWO ARMS, keyed on the MA-visible message type, because the engine has always had two:

    * **CAM** -- a reason fires only after `detector_min_consec` CONSECUTIVE violating messages from
      the same certificate at the same receiver (a single GNSS outlier must not be reportable), and
      then only with probability `report_prob`. Reasons are ordered most-severe-first by a STABLE
      sort over the declared key order, which is why that order is digest-bearing.
    * **DENM** -- an event message is a one-shot claim with no history to streak over, so it reports
      immediately on a violating score, again gated by `report_prob`.

    **`legacy_global_rng`.** The `report_prob` Bernoulli is drawn from the engine's single global
    `random.Random(cfg.seed)`, whose draw COUNT AND ORDER are load-bearing across packet loss,
    collusion and net_delay. That is exactly what D3 forbids a plugin from touching. Moving it to a
    keyed namespace would move every pinned golden, so the built-in declares the grandfathering, the
    resolver refuses that capability from any third party, and the manifest records it. A
    third-party fusion draws from its own `RngNamespace` and changes the digest -- which is correct,
    and is what declaring a plugin is supposed to do.
    """

    plugin_id = "streak_v1"
    cfg_fields = {"min_consec": "detector_min_consec", "report_prob": "report_prob"}

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.min_consec = self.params["min_consec"]
        self.report_prob = self.params["report_prob"]
        #: The global stream, handed over ONLY because `legacy_global_rng` is declared and this is a
        #: built-in. `env` carries it; a third-party fusion's env does not.
        self._legacy_rng = self.env.get("legacy_rng")

    def config_fields(self):
        return {name: FieldSpec("float", None, f"mirrors PipelineConfig.{field}",
                                group="Detection")
                for name, field in sorted(self.cfg_fields.items())}

    def capabilities(self):
        return frozenset({CAP_LEGACY_GLOBAL_RNG, CAP_LEGACY_RAW_COMPARE, CAP_STATEFUL})

    def decide(self, scores, state, obs, params, rng):
        if obs.msg_type == "denm":
            # No hard-coded reason code here: a check that does not apply to this message type
            # scored exactly 0.0, so "which checks fired" is the same question on both arms. That
            # is what lets a THIRD-PARTY event check work with the shipped fusion, and with one
            # event check declared (the built-in `denmPlausibility`) it is the identical branch on
            # the identical draw the inline code made.
            fired = [k for k in self.keys if scores[k] >= 1.0]
            if not fired:
                return None
            if self._legacy_rng.random() > self.report_prob:
                return None
            fired.sort(key=lambda k: -scores[k])
            return ReportDecision(True, fired, scores[fired[0]],
                                  max(scores.values()) if scores else 1.0)
        streak = state["streak"]
        min_consec = self.min_consec
        fired = {}
        for k in self.keys:
            n = streak.get(k, 0) + 1 if scores[k] >= 1.0 else 0
            streak[k] = n
            if n >= min_consec:
                fired[k] = scores[k]
        if not fired:
            return None
        if self._legacy_rng.random() > self.report_prob:
            return None
        reasons = sorted(fired, key=lambda k: -scores[k])
        return ReportDecision(True, reasons, scores.get(reasons[0], 1.0),
                              max(scores.values()) if scores else 1.0)


# --------------------------------------------------------------------------- #
# THE REGISTRY. Registration order IS evaluation order IS `DET_KEYS` order.
# --------------------------------------------------------------------------- #
#: The order the inline block evaluated these in, preserved exactly. The first twelve are the
#: unconditional `DET_KEYS`; `vruImpersonation` and `denmPlausibility` are the two gated appends, in
#: the order the gates appended them; `kalmanConsistency` is the soft tail, which lands after every
#: hard key in the report row (`for k in (*DET_KEYS, *SOFT_KEYS)`).
BUILTIN_CHECKS = (
    PositionSpeedInconsistency,
    PositionJump,
    HeadingInconsistency,
    StaleOrReplay,
    ConstantPositionFrozen,
    ImplausibleAcceleration,
    SybilCoLocation,
    AcceptanceRangeThreshold,
    BeaconFrequency,
    SignatureVerification,
    CertValidity,
    MapOffRoad,
    VruImpersonation,
    DenmPlausibility,
    KalmanConsistency,
)

for _cls in BUILTIN_CHECKS:
    _registry.register_builtin("check", _cls.reason_code, _cls)
del _cls

_registry.register_builtin("fusion", "streak_v1", StreakFusion)

#: name -> class for the SHIPPED suite, as a fixed in-tree mapping, NOT a view of the registry.
#:
#: The distinction is load-bearing and was a real hole. `register_builtin` writes into a process-
#: global dict and is idempotent per (slot, name), so it OVERWRITES; and the resolver's built-in
#: tier reads that same dict. Any installed distribution that calls `register_builtin` at import
#: time -- which nothing prevents, since importing it is enough -- could therefore (a) add itself to
#: whatever `@builtins` expanded to, in a run whose config never named it, and (b) replace
#: `positionJump` with its own class, which then resolves as a BUILT-IN: engine config fields for
#: params, the raw (unrounded) cliff compare, no `x_` column namespacing, no source gate.
#:
#: `run.default_check_refs` expands `@builtins` from :data:`BUILTIN_CHECKS` above -- a tuple written
#: in this file -- and `run.build_checks` re-checks every resolved built-in against this mapping by
#: IDENTITY, so a hijacked registry entry is a named refusal before step 0 instead of a silent
#: substitution. Registering remains the way a NEW in-tree check joins the suite; it is no longer a
#: way for anything else to.
BUILTIN_CHECK_BY_CODE = {cls.reason_code: cls for cls in BUILTIN_CHECKS}

#: The same fixed mapping for the fusion slot.
BUILTIN_FUSION_BY_NAME = {"streak_v1": StreakFusion}


def builtin_check(name):
    """The registered class for a built-in reason code, or None."""
    return _registry.builtin("check", name)


def check_docs() -> dict:
    """reason code -> one-line description, derived from each check's own docstring.

    This is what `datagen/featurize.py` emits into `schema.json`, so a detector's documentation is
    written once, next to its arithmetic, instead of in a hand-maintained dict three modules away
    that nothing forces anyone to update.
    """
    out = {}
    for name in _registry.builtin_names("check"):
        cls = _registry.builtin("check", name)
        doc = " ".join((cls.__doc__ or "").split("\n\n")[0].split()).rstrip(".")
        out[name] = doc
    return out
