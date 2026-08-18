"""Unit tests for the grid road network: gravity OD trip-length law and turn-angle geometry."""

import random
import statistics

from scms_sim_ref.mock_pipeline.roads import GridNetwork, Trip


def _lengths(od_model, scale=2.0, n=400):
    net = GridNetwork(10, 10, 100.0)
    rng = random.Random(42)
    return [net.random_trip(rng, 12.0, 0.0, od_model=od_model, gravity_scale=scale).length
            for _ in range(n)]


def test_gravity_od_shortens_trips_and_is_right_skewed():
    """Gravity (distance-decay) trips are shorter on average and right-skewed vs uniform-random OD."""
    uni = _lengths("uniform")
    grav = _lengths("gravity", scale=1.5)
    assert statistics.mean(grav) < statistics.mean(uni), (statistics.mean(grav), statistics.mean(uni))
    # right-skew: mean > median (a long tail of a few far trips over many short ones)
    assert statistics.mean(grav) > statistics.median(grav)


def test_gravity_scale_controls_trip_length():
    """A larger gravity scale (weaker decay) yields longer mean trips, approaching uniform."""
    short = statistics.mean(_lengths("gravity", scale=1.0))
    long = statistics.mean(_lengths("gravity", scale=6.0))
    assert long > short


def test_gravity_od_is_deterministic():
    a = _lengths("gravity", scale=2.0)
    b = _lengths("gravity", scale=2.0)
    assert a == b


def test_uniform_od_unchanged_by_new_kwargs():
    """Passing the new kwargs with the default model must not perturb the uniform RNG sequence."""
    net = GridNetwork(8, 8, 100.0)
    a = [net.random_trip(random.Random(7), 10.0, 0.0).length for _ in range(50)]
    b = [net.random_trip(random.Random(7), 10.0, 0.0, od_model="uniform", gravity_scale=2.0).length
         for _ in range(50)]
    assert a == b


def test_boundary_origins_start_trips_at_the_perimeter():
    """With boundary_origins, every trip's first waypoint is on the grid perimeter."""
    net = GridNetwork(6, 6, 100.0)
    rng = random.Random(3)
    span = 5 * 100.0
    for _ in range(80):
        wp0 = net.random_trip(rng, 12.0, 0.0, boundary_origin=True).wp[0]
        on_edge = wp0[0] in (0.0, span) or wp0[1] in (0.0, span)
        assert on_edge, f"origin {wp0} not on the perimeter"
    # default (off) can start anywhere -> at least one interior origin over many draws
    interior = 0
    for _ in range(200):
        wp0 = net.random_trip(rng, 12.0, 0.0).wp[0]
        if wp0[0] not in (0.0, span) and wp0[1] not in (0.0, span):
            interior += 1
    assert interior > 0, "default origins should include interior nodes"


def test_next_turn_reports_grid_corner_angle():
    """A route that goes east then north bends ~90 degrees at the middle vertex."""
    trip = Trip([(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)], speed=10.0, spawn_time=0.0)
    dist, ang = trip.next_turn(0.0)
    assert abs(dist - 100.0) < 1e-6
    assert abs(ang - 90.0) < 1e-6
    # a straight (collinear) route has no bend
    straight = Trip([(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)], speed=10.0, spawn_time=0.0)
    _d, ang2 = straight.next_turn(0.0)
    assert abs(ang2) < 1e-6
