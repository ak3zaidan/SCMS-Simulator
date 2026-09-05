"""ETSI generation, congestion-control and access-layer timing rules -- the *behaviour* half of the
standards profile, next to `etsi.py`'s *encoding* half.

`etsi.py` answers "what do the octets look like". This module answers the three questions that
decide whether a run's traffic is a plausible ITS-G5 channel at all:

=========================================  ==================================================
EN 302 637-2 V1.4.1 clause 6.1.3           WHEN does a station emit a CAM
TS 102 687 V1.2.1 reactive DCC             HOW OFTEN is it then ALLOWED to
IEEE 802.11-2020 OFDM at 10 MHz            HOW LONG is that frame on the air, and how long
                                           does it take to get there
=========================================  ==================================================

**Stdlib only, and not one random number.** Every function here is a pure function of its
arguments; the two classes hold per-station state and mutate only their own fields. That is what
lets the whole layer be switched on without perturbing a single RNG stream, which is the hard
constraint every opt-in feature in this engine is held to.

**Every constant is transcribed from `datagen/refdata/`, never re-derived here.**
`etsi_cam_dcc.json` supplies the triggering thresholds, `T_GenCamMin` / `T_GenCamMax` and the
reactive-DCC state table; `phy_80211p_profile.json` supplies the OFDM timing, the MAC overhead and
the measured end-to-end latency band. `tests/test_etsi_rules.py` grades this file against those
JSONs rather than against a second copy of the numbers.

THE AIRTIME DISAGREEMENT THIS SETTLES
-------------------------------------
The two engines disagree by 2.05x on the airtime of one CAM, and both readings are pinned in the
same refdata file:

* Java (`Dcc.java`, `SignedCam.java`): a flat 300 B MPDU, PPDU only -> **448.0 us**
* Python (`run.py:PHY_FRAME_AIRTIME_S`): a ~500 B MPDU plus AIFS+backoff -> **919.5 us**

Neither is measured; both are assumptions about a frame nobody had encoded. A real UPER CAM under
`etsi.EtsiCamCodec` is **41 octets**, and TS 103 097's measured envelope
(`etsi.SECURITY_ENVELOPE_BYTES`) is 93 B digest-signed / 219 B certificate-attached. So the
MEASURED frame is 134 B or 260 B, and :func:`frame_airtime_s` returns 431.5 us / 599.5 us including
the MAC overhead -- BELOW the Java figure on the digest arm despite counting AIFS+backoff that the
Java figure omits, because the payload assumption was wrong by an order of magnitude, not the
timing arithmetic.
"""
from __future__ import annotations

import math
from typing import Optional

# --------------------------------------------------------------------------- #
# EN 302 637-2 V1.4.1 clause 6.1.3 -- CAM generation frequency management
# (refdata etsi_cam_dcc.cam_interval_s + cam_trigger_thresholds)
# --------------------------------------------------------------------------- #
#: `T_GenCamMin` -- the CAM service never generates two CAMs closer than this. 100 ms.
T_GEN_CAM_MIN_S = 0.1
#: `T_GenCamMax` -- the heart-beat: a CAM is generated at least this often regardless of dynamics.
T_GEN_CAM_MAX_S = 1.0
#: The three dynamics triggers, clause 6.1.3 conditions 2a/2b/2c.
CAM_TRIGGER_POSITION_M = 4.0
CAM_TRIGGER_HEADING_DEG = 4.0
CAM_TRIGGER_SPEED_MPS = 0.5
#: `N_GenCam`: after a dynamics trigger the shortened interval is kept for this many consecutive
#: CAMs before the heart-beat relaxes back to `T_GenCamMax`. Clause 6.1.3 sets the default to 3.
N_GEN_CAM = 3

#: Trigger reason labels. `""` means no CAM was generated at this evaluation instant.
TRIGGER_NONE = ""
TRIGGER_FIRST = "first"
TRIGGER_POSITION = "position"
TRIGGER_HEADING = "heading"
TRIGGER_SPEED = "speed"
TRIGGER_HEARTBEAT = "heartbeat"
#: The reasons that count as DYNAMICS-triggered in the Java engine's own 89.6 % measurement.
DYNAMICS_TRIGGERS = frozenset({TRIGGER_POSITION, TRIGGER_HEADING, TRIGGER_SPEED})


def angle_delta_deg(a: float, b: float) -> float:
    """Smallest absolute angle between two bearings, in degrees [0, 180].

    Convention-free by construction (it is a difference), so the engine's degrees-CCW-from-East and
    ETSI's 0.1-degrees-CW-from-North give the identical answer -- which is why the 4-degree
    threshold can be applied to engine headings without converting first.
    """
    return abs((float(a) - float(b) + 180.0) % 360.0 - 180.0)


class CamGenerationState:
    """One station's CAM service state, clause 6.1.3.

    Held per vehicle by the engine. `evaluate` is called once per simulation step and answers "does
    this station put a CAM on the air right now, and why". It draws nothing and allocates nothing
    per call beyond the returned string.

    `t_gen_cam` is the currently permitted heart-beat interval: `T_GenCamMax` normally, shortened to
    the elapsed time after a dynamics trigger and held there for `N_GenCam` consecutive CAMs, then
    released. `t_gen_cam_dcc` is the floor DCC imposes (see :class:`ReactiveDcc`); the effective
    minimum inter-CAM interval is `max(T_GenCamMin, t_gen_cam_dcc)`.
    """

    __slots__ = ("last_t", "last_x", "last_y", "last_speed", "last_heading",
                 "t_gen_cam", "n_left", "n_cams", "n_dynamics")

    def __init__(self) -> None:
        self.last_t: Optional[float] = None
        self.last_x = 0.0
        self.last_y = 0.0
        self.last_speed = 0.0
        self.last_heading = 0.0
        self.t_gen_cam = T_GEN_CAM_MAX_S
        self.n_left = 0                 # remaining CAMs to keep the shortened interval for
        self.n_cams = 0                 # CAMs this station generated (a counter, not a rate)
        self.n_dynamics = 0             # of which dynamics-triggered

    def evaluate(self, t: float, x: float, y: float, speed: float, heading: float,
                 t_gen_cam_dcc: float = T_GEN_CAM_MIN_S) -> str:
        """Decide, and -- if a CAM is generated -- commit the new reference state.

        Returns one of the `TRIGGER_*` labels; `TRIGGER_NONE` means no CAM this instant. Committing
        inside the decision is deliberate: clause 6.1.3 defines every threshold against *the last
        CAM actually sent*, so a caller that evaluated without sending would corrupt the reference.
        """
        if self.last_t is None:
            self._commit(t, x, y, speed, heading, dynamics=False)
            return TRIGGER_FIRST
        dt = t - self.last_t
        # Condition 1: never faster than T_GenCamMin, and never faster than DCC permits.
        if dt + 1e-9 < max(T_GEN_CAM_MIN_S, t_gen_cam_dcc):
            return TRIGGER_NONE
        # Conditions 2a/2b/2c, evaluated in the standard's own order.
        if math.hypot(x - self.last_x, y - self.last_y) > CAM_TRIGGER_POSITION_M:
            reason = TRIGGER_POSITION
        elif angle_delta_deg(heading, self.last_heading) > CAM_TRIGGER_HEADING_DEG:
            reason = TRIGGER_HEADING
        elif abs(speed - self.last_speed) > CAM_TRIGGER_SPEED_MPS:
            reason = TRIGGER_SPEED
        elif dt + 1e-9 >= self.t_gen_cam:
            reason = TRIGGER_HEARTBEAT
        else:
            return TRIGGER_NONE
        dynamics = reason in DYNAMICS_TRIGGERS
        if dynamics:
            # Clause 6.1.3: the elapsed time becomes the new T_GenCam, kept for N_GenCam CAMs.
            self.t_gen_cam = min(max(dt, T_GEN_CAM_MIN_S), T_GEN_CAM_MAX_S)
            self.n_left = N_GEN_CAM
        elif self.n_left > 0:
            self.n_left -= 1
            if self.n_left == 0:
                self.t_gen_cam = T_GEN_CAM_MAX_S
        self._commit(t, x, y, speed, heading, dynamics)
        return reason

    def _commit(self, t, x, y, speed, heading, dynamics: bool) -> None:
        self.last_t, self.last_x, self.last_y = t, x, y
        self.last_speed, self.last_heading = speed, heading
        self.n_cams += 1
        if dynamics:
            self.n_dynamics += 1


# --------------------------------------------------------------------------- #
# TS 102 687 V1.2.1 -- reactive DCC
# (refdata etsi_cam_dcc.dcc_reactive_states, the AUTHORITATIVE encoding)
# --------------------------------------------------------------------------- #
#: (state, CBR lower bound INCLUSIVE, CBR upper bound EXCLUSIVE, permitted CAM rate Hz, T_off s).
#: Half-open bands, exactly as the refdata `derivation` field spells out: CBR 0.30 selects
#: `active_1`, CBR 0.60 selects `restrictive`, and there is no gap and no overlap at a breakpoint.
DCC_REACTIVE_STATES = (
    ("relaxed",     0.00, 0.30, 10.0, 0.100),
    ("active_1",    0.30, 0.40,  5.0, 0.200),
    ("active_2",    0.40, 0.50,  2.5, 0.400),
    ("active_3",    0.50, 0.60,  2.0, 0.500),
    ("restrictive", 0.60, 1.01,  1.0, 1.000),
)
#: The state a station sits in with an idle channel -- and the one the low-density measurement must
#: show, because on InTAS the measured CBR is 0.027 against a 0.30 breakpoint.
DCC_DEFAULT_STATE = DCC_REACTIVE_STATES[0][0]
#: TS 103 175 satisfactory operating range and the reactive machine's control point.
DCC_SATISFACTORY_RANGE = (0.55, 0.75)
DCC_CONTROL_TARGET = 0.60


def dcc_state_for(cbr: float) -> tuple:
    """`(state, rate_hz, t_off_s)` for a measured CBR. Never raises; clamps outside [0, 1]."""
    c = min(1.0, max(0.0, float(cbr)))
    for name, lo, hi, rate, t_off in DCC_REACTIVE_STATES:
        if lo <= c < hi:
            return name, rate, t_off
    return DCC_REACTIVE_STATES[-1][0], DCC_REACTIVE_STATES[-1][3], DCC_REACTIVE_STATES[-1][4]


class ReactiveDcc:
    """One station's reactive-DCC entity.

    Fed the CBR the station's own receiver measured on the PREVIOUS step, which is the correct
    causality: DCC is a feedback loop over a quantity that is measured, not predicted. A station
    that has heard nothing yet sits in `relaxed`, whose `T_off` of 100 ms equals `T_GenCamMin` -- so
    an idle channel means DCC imposes no constraint the CAM service was not already under, which is
    exactly the "correctly does nothing at low load" property.

    Smoothing is deliberately absent. TS 102 687's reactive machine is a memoryless mapping from
    the current CBR to a state; the exponential smoothing some implementations add is a local
    choice, and adding one here would make the reported CBR-to-rate relation this file's invention
    rather than the standard's.
    """

    __slots__ = ("cbr", "state", "rate_hz", "t_off", "steps_in_state", "state_counts")

    def __init__(self) -> None:
        self.cbr = 0.0
        self.state, self.rate_hz, self.t_off = dcc_state_for(0.0)
        self.steps_in_state = 0
        self.state_counts: dict = {}

    def update(self, cbr: float) -> str:
        """Fold in a measured CBR and return the new state name."""
        self.cbr = min(1.0, max(0.0, float(cbr)))
        prev = self.state
        self.state, self.rate_hz, self.t_off = dcc_state_for(self.cbr)
        self.steps_in_state = self.steps_in_state + 1 if self.state == prev else 0
        self.state_counts[self.state] = self.state_counts.get(self.state, 0) + 1
        return self.state

    @property
    def t_gen_cam_floor(self) -> float:
        """The minimum inter-CAM interval DCC permits, as the CAM service consumes it.

        `T_off` from the state table, clamped into the CAM service's own `[T_GenCamMin,
        T_GenCamMax]` band -- the refdata's own note that "the DCC rates span exactly this 10 Hz -
        1 Hz interval, so DCC never asks for a rate outside the CAM service's own limits".
        """
        return min(max(self.t_off, T_GEN_CAM_MIN_S), T_GEN_CAM_MAX_S)


# --------------------------------------------------------------------------- #
# IEEE 802.11 OFDM timing at 10 MHz, 6 Mb/s -- airtime and access delay
# (refdata phy_80211p_profile.frame_airtime_us + mac_overhead_us)
# --------------------------------------------------------------------------- #
OFDM_PREAMBLE_US = 32.0            #: short + long training symbols at 10 MHz
OFDM_SIGNAL_US = 8.0               #: the SIGNAL field is one symbol
OFDM_SYMBOL_US = 8.0               #: 10 MHz doubles the 20 MHz 4 us symbol
OFDM_N_DBPS = 48                   #: 6 Mb/s * 8 us = 48 data bits per symbol (QPSK 1/2)
OFDM_SERVICE_BITS = 16
OFDM_TAIL_BITS = 6

SLOT_US = 13.0
SIFS_US = 32.0
DIFS_US = SIFS_US + 2.0 * SLOT_US                 # 58.0
AIFS_AC_BE_US = SIFS_US + 6.0 * SLOT_US           # AIFSN(AC_BE) = 6 -> 110.0
CW_MIN = 15                                        # vendored INET default (omnetpp.ini:70)
MEAN_BACKOFF_US = (CW_MIN / 2.0) * SLOT_US        # 97.5
MAC_OVERHEAD_US = AIFS_AC_BE_US + MEAN_BACKOFF_US  # 207.5

#: Speed of light, m/s -- the propagation term. 0.33 us at 100 m: negligible against the MAC, and
#: included anyway because a latency model that omits propagation is not a latency model.
C_M_S = 299_792_458.0

#: The facilities + networking + SECURITY stack latency, in seconds, DERIVED from the one anchored
#: measurement this repository holds rather than chosen.
#:
#: `v2x_awareness.latency_p50_ms_80211p` is the DLR Cohda MK5 field campaign: **5-9 ms** end to end,
#: 5 ms at a 200 B payload. The air interface at 200 B is `frame_airtime_s(200)` = 519.5 us
#: (312 us PPDU + 207.5 us AIFS+backoff), and the refdata's own note says the measured figure is
#: "dominated by stack/queueing, not by the air interface". Everything the air interface does not
#: account for is therefore this constant:
#:
#:     5.000 ms - 0.5195 ms = 4.4805 ms
#:
#: It is a CONSTANT, not a distribution: nothing in this repository measures the SHAPE of that
#: 4.48 ms, so drawing it from an invented distribution would be fabrication -- and it would cost a
#: random number on a path whose whole point is that it costs none. The consequence is stated
#: rather than hidden: this model's latency spread comes entirely from frame size, distance and
#: contention, so it reproduces the band's LOWER edge and cannot reproduce its 9 ms tail.
STACK_LATENCY_S = 0.0044805


def ppdu_bits(mpdu_bytes: int) -> int:
    """PSDU bit count including the SERVICE field and the tail: `16 + 8*MPDU + 6`."""
    return OFDM_SERVICE_BITS + 8 * int(mpdu_bytes) + OFDM_TAIL_BITS


def ppdu_symbols(mpdu_bytes: int) -> int:
    """OFDM data symbols: `ceil(bits / N_DBPS)`. The rounding is why 300 B is 448 us, not 400 us."""
    return int(math.ceil(ppdu_bits(mpdu_bytes) / OFDM_N_DBPS))


def ppdu_airtime_s(mpdu_bytes: int) -> float:
    """PPDU airtime in seconds: preamble + SIGNAL + data symbols. Excludes AIFS and backoff.

    Reproduces `phy_80211p_profile.frame_airtime_us` exactly at every one of its five pinned rows.
    """
    us = OFDM_PREAMBLE_US + OFDM_SIGNAL_US + OFDM_SYMBOL_US * ppdu_symbols(mpdu_bytes)
    return us * 1e-6


def frame_airtime_s(mpdu_bytes: int) -> float:
    """Channel time one broadcast attempt occupies: PPDU + AIFS(AC_BE) + mean initial backoff.

    This -- not the bare PPDU -- is what a CBR estimate must integrate, because a station that is
    deferring is a station whose channel is busy. It is also the term whose omission is, in the
    refdata's own words, "the difference between meeting and missing the CBR gate".
    """
    return ppdu_airtime_s(mpdu_bytes) + MAC_OVERHEAD_US * 1e-6


def channel_busy_ratio(offered_airtime_s: float, window_s: float) -> float:
    """CBR: offered channel time over the measurement window, clamped to [0, 1].

    The zeroth-order estimator `phy_80211p_profile.cbr_from_load` pins, evaluated on REAL frame
    lengths instead of an assumed one. Its stated limits are inherited: it counts every frame in
    sensing range as heard, adds airtime linearly, and therefore over-counts overlapping
    transmissions at high load.
    """
    return min(1.0, max(0.0, float(offered_airtime_s)) / max(float(window_s), 1e-9))


def access_delay_s(cbr: float, mpdu_bytes: int) -> float:
    """Time from "the frame is ready" to "the last bit is on the air", for one broadcast.

    `AIFS + E[backoff]/(1 - CBR) + PPDU`. The `1/(1 - CBR)` factor is the standard mean-deferral
    scaling: a station that finds the medium busy a fraction CBR of the time waits, in expectation,
    that many times longer to count its backoff slots down. It is 1.0 on an idle channel, so the
    delay of an uncontended frame is exactly AIFS + mean backoff + airtime and the congestion term
    contributes nothing -- the same "correctly does nothing at low load" property DCC has.

    CBR is capped at 0.95 so the model stays finite at saturation. That cap IS the model's ceiling
    and is stated rather than hidden: at CBR 0.95 the access delay is 2.1 ms, and a real 802.11p
    station under that load would be doing considerably worse.
    """
    c = min(0.95, max(0.0, float(cbr)))
    backoff = MEAN_BACKOFF_US * 1e-6 / (1.0 - c)
    return AIFS_AC_BE_US * 1e-6 + backoff + ppdu_airtime_s(mpdu_bytes)


def link_latency_s(distance_m: float, cbr: float, mpdu_bytes: int,
                   stack_s: float = STACK_LATENCY_S) -> float:
    """End-to-end latency of one PDU on one link: propagation + access + stack.

    Deterministic -- a pure function of geometry, load and frame length. That is what lets the
    latency model be switched on without drawing a single random number, which is the property that
    keeps every pinned digest reachable with it off and every replay exact with it on.
    """
    return (max(0.0, float(distance_m)) / C_M_S
            + access_delay_s(cbr, mpdu_bytes)
            + max(0.0, float(stack_s)))


#: The measured band `link_latency_s` is checked against: DLR Cohda MK5, ITS World Congress 2021,
#: transcribed in `v2x_awareness.latency_p50_ms_80211p`. Seconds.
LATENCY_REFERENCE_BAND_S = (0.005, 0.009)

# --------------------------------------------------------------------------- #
# The two ASSUMED airtimes, restated so the disagreement can be REPORTED without importing the
# engine. `codecs/` is imported by `run.py`, never the other way round, so a profile that wants to
# publish "here is what each engine assumed, and here is what the measured frame actually costs"
# needs its own copy of both numbers. `tests/test_protocol_profile.py` asserts they are still the
# engine's, so the copies cannot drift.
# --------------------------------------------------------------------------- #
#: `Dcc.java` / `SignedCam.java`: a flat 300 B MPDU, PPDU only -> 448.0 us.
JAVA_ASSUMED_MPDU_BYTES = 300
#: `run.py::PHY_FRAME_AIRTIME_S`: a ~500 B MPDU (712 us of PPDU) plus 207.5 us of AIFS+backoff.
PYTHON_ASSUMED_FRAME_AIRTIME_S = (712.0 + 207.5) * 1e-6


__all__ = [
    "AIFS_AC_BE_US", "CAM_TRIGGER_HEADING_DEG", "CAM_TRIGGER_POSITION_M", "CAM_TRIGGER_SPEED_MPS",
    "C_M_S", "CW_MIN", "CamGenerationState", "DCC_CONTROL_TARGET", "DCC_DEFAULT_STATE",
    "DCC_REACTIVE_STATES", "DCC_SATISFACTORY_RANGE", "DIFS_US", "DYNAMICS_TRIGGERS",
    "JAVA_ASSUMED_MPDU_BYTES", "PYTHON_ASSUMED_FRAME_AIRTIME_S",
    "LATENCY_REFERENCE_BAND_S", "MAC_OVERHEAD_US", "MEAN_BACKOFF_US", "N_GEN_CAM", "OFDM_N_DBPS",
    "OFDM_PREAMBLE_US", "OFDM_SIGNAL_US", "OFDM_SYMBOL_US", "ReactiveDcc", "SIFS_US", "SLOT_US",
    "STACK_LATENCY_S", "TRIGGER_FIRST", "TRIGGER_HEADING", "TRIGGER_HEARTBEAT", "TRIGGER_NONE",
    "TRIGGER_POSITION", "TRIGGER_SPEED", "T_GEN_CAM_MAX_S", "T_GEN_CAM_MIN_S", "access_delay_s",
    "angle_delta_deg", "channel_busy_ratio", "dcc_state_for", "frame_airtime_s", "link_latency_s",
    "ppdu_airtime_s", "ppdu_bits", "ppdu_symbols",
]
