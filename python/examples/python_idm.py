#!/usr/bin/env python3
"""A car-following model written in Python, and the conformance kit run over it.

    python examples/python_idm.py

This is Treiber's Intelligent Driver Model — the same equation the engine's built-in
`mobility/car-following/idm` implements — written as a plug-in, so the two can be compared
and so the plug-in shape is visible end to end.

Three things in it are the point:

* every transcendental goes through ``v2xw.math``, not Python's ``math``, because the
  platform C library is not bit-identical between machines (ADR 0004 §4);
* the driver parameters come off ``ego``, not out of the card, because they are drawn per
  driver by the engine from the model's own calibration;
* the card cites the paper the equation is from, and the conformance kit refuses a card
  whose defaults cite nothing.

Then it does the same for a *deliberately broken* model, so you can see the kit fail.
"""

from __future__ import annotations

import v2xw
from v2xw.plugins import CarFollowing, card, equation, parameter, source

m = v2xw.math


class Idm(CarFollowing):
    """The Intelligent Driver Model.

    a = a_max · [ 1 − (v/v0)^δ − (s*/s)² ],  s* = s0 + v·T + v·Δv / (2·√(a_max·b))

    Everything on the right-hand side arrives on ``ego`` except the speed limit, which is
    the lane's. The model holds no state.
    """

    card = card(
        id="mobility/car-following/python-idm",
        family="mobility",
        version="1.0.0",
        tier=["abstract"],
        purpose="Treiber's Intelligent Driver Model, as a Python plug-in.",
        equations=[
            equation(
                "idm-acceleration",
                r"a = a_{max}\left[1 - (v/v_0)^\delta - (s^*/s)^2\right],"
                r"\quad s^* = s_0 + vT + v\Delta v / (2\sqrt{a_{max} b})",
                notes="Valid for a single leader on a single lane. Collision-free only "
                "while the leader's deceleration stays within b.",
            )
        ],
        sources=[
            source(
                "paper",
                "Treiber, Hennecke & Helbing (2000), Congested traffic states in empirical "
                "observations and microscopic simulations, Phys. Rev. E 62(2) 1805, "
                "doi:10.1103/PhysRevE.62.1805",
            )
        ],
        parameters=[
            parameter(
                "delta",
                "-",
                4.0,
                source(
                    "paper",
                    "Treiber et al. (2000) §II: the free-acceleration exponent delta is 4",
                ),
            )
        ],
        assumptions=[
            "One leader. The model does not look past the vehicle in front.",
            "No reaction time: the acceleration responds to the current gap.",
        ],
        limitations=[
            "Collision-free only while the leader's deceleration stays within b.",
            "The braking term is unbounded: at a gap far below the desired gap it returns "
            "hundreds of m/s^2, which no vehicle can produce. The engine clamps an "
            "acceleration to the vehicle's physical limits; the model does not, and a "
            "caller reading `accel` directly sees the raw value.",
        ],
        ignores=[
            "Lateral behaviour, which is the lane-change model's, and any stochastic "
            "driver variation, which the engine draws per driver.",
        ],
    )

    #: The free-acceleration exponent δ, cited on the card above.
    DELTA = 4.0

    def accel(self, ego, leader, lane, weather) -> float:
        v0 = min(ego.desired_speed_mps, lane.speed_limit_mps)
        a_max = ego.max_accel_mps2
        if v0 <= 0.0:
            return -a_max
        free = a_max * (1.0 - m.pow(ego.speed_mps / v0, self.DELTA))
        if leader is None:
            return free
        dv = ego.speed_mps - leader.speed_mps
        s_star = (
            ego.min_gap_m
            + ego.speed_mps * ego.time_headway_s
            + ego.speed_mps * dv / (2.0 * m.sqrt(a_max * ego.comfort_decel_mps2))
        )
        s = max(leader.gap_m, 0.01)
        return free - a_max * m.pow(max(s_star, 0.0) / s, 2.0)


class Jittery(Idm):
    """The same model with a random jitter added — the mistake the kit exists to catch."""

    card = dict(Idm.card, id="mobility/car-following/python-idm-jittery")

    def accel(self, ego, leader, lane, weather) -> float:
        import random

        return super().accel(ego, leader, lane, weather) + random.gauss(0.0, 0.1)


def main() -> int:
    model = Idm()
    handle = model.attach()

    print(f"model      {handle.id}")
    print("accelerations through the engine's trait object:")
    for label, kwargs in [
        ("free road at 50 km/h", {}),
        ("50 m behind a car at 50 km/h", {"gap_m": 50.0, "leader_speed_mps": 13.888889}),
        ("5 m behind a stopped car", {"gap_m": 5.0, "leader_speed_mps": 0.0}),
        ("2 m from a stop line", {"gap_m": 2.0, "leader_speed_mps": 0.0, "is_vehicle": False}),
    ]:
        a = handle.accel(13.888889, **kwargs)
        print(f"  {label:<32} {a:+10.3f} m/s^2")
    print("  (the last two are far below the IDM's desired gap, where its braking term is")
    print("   unbounded: a limitation stated on the card, and clamped by the engine.)")

    print("\nconformance:")
    report = v2xw.conformance.check(model)
    print(report)
    report.raise_for_failures()
    print("\npassed. Necessary, not sufficient: run v2xw.run_twice over a real scenario to")
    print("catch state carried between calls, which no static check can see.")

    print("\nthe same kit over the deliberately broken model:")
    broken = v2xw.conformance.check(Jittery())
    print(broken)
    assert not broken.passed, "the kit must fail a model that draws random numbers"
    print("\nfailed, as it must.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
