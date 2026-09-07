"""Flow-mode memory is bounded by the CONCURRENT population, not the cumulative one.

`docs/realism/LONG-RUNS.md` section 1.3: `run_pipeline` drew the whole arrival process and
constructed every vehicle before step 0, then held all of them for the run, so peak working set
fitted `92.5 MiB + 54.3 KiB x (vehicles EVER created)` -- 3,159 MiB over 8 hours at a concurrent
population that never left ~170. Arrivals are now drawn on demand (`_pump`) and finished stations are
released (`_retire`).

These tests pin the behaviour, not the byte count: the population is built late, released early, and
the dataset it produces is unchanged down to the pinned digest.
"""

import gc
import json
import os

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import run as runmod
from scms_sim_ref.mock_pipeline.run import Vehicle, _RetiredDevice


def _live_vehicles():
    gc.collect()
    return sum(1 for o in gc.get_objects() if type(o) is Vehicle)


def _flow_cfg(out_dir, duration, **kw):
    return PipelineConfig(seed=42, traffic_flow=True, road_network="grid", duration_s=duration,
                          arrival_rate=2.0, grid_w=6, grid_h=6, attacker_pct=0.15,
                          traffic_lights=True, out_dir=out_dir, **kw)


# --------------------------------------------------------------------------- #
# Built late.
# --------------------------------------------------------------------------- #
def test_the_population_is_not_materialised_before_step_zero(tmp_path):
    """At step 0 only the vehicles that have actually arrived exist.

    This is the defect itself: the arrival loop used to run to `tt >= total_time` before the first
    vehicle moved, so `_live_vehicles()` at step 0 was the WHOLE population.
    """
    seen = {}

    def hook(step):
        if step in (0, 5):
            seen[step] = _live_vehicles()

    runmod.PER_STEP_HOOK = hook
    try:
        r = run_pipeline(_flow_cfg(str(tmp_path / "late"), 300.0))
    finally:
        runmod.PER_STEP_HOOK = None

    assert r.n_vehicles > 400, r.n_vehicles          # a real population to be lazy about
    assert seen[0] <= 20, f"{seen[0]} vehicles already built at step 0 of {r.n_vehicles}"
    assert seen[5] < seen[0] + 40                    # and it grows with the clock, not all at once


# --------------------------------------------------------------------------- #
# Released early.
# --------------------------------------------------------------------------- #
def test_finished_vehicles_are_released_so_the_live_set_tracks_concurrency(tmp_path):
    """Live `Vehicle` objects near the end of a run track the ACTIVE population, not the total."""
    peak = {"live": 0, "active": 0}

    def hook(step):
        if step % 25:
            return
        peak["live"] = max(peak["live"], _live_vehicles())

    runmod.PER_STEP_HOOK = hook
    try:
        r = run_pipeline(_flow_cfg(str(tmp_path / "release"), 600.0))
    finally:
        runmod.PER_STEP_HOOK = None

    # The reference arm saturates at ~170 concurrent whatever its duration; a 600 s run creates
    # ~1200 vehicles. Live objects must follow the first number, with slack for the spawn queue.
    assert r.n_vehicles > 800, r.n_vehicles
    assert peak["live"] < r.n_vehicles / 2, (
        f"{peak['live']} live Vehicle objects for {r.n_vehicles} ever created -- nothing was released")
    assert peak["live"] < 400


def test_a_retired_station_still_resolves_for_the_misbehaviour_authority(tmp_path):
    """`digest_to_vehicle` keeps answering after despawn, because `trusted()` depends on it.

    The MA's trusted-reporter gate is consulted for reporters inside a `revoke_window_s` sliding
    window, and a reporter can despawn inside that window. Resolving it to `None` would change the
    gate's verdict, so a released station leaves a `_RetiredDevice` behind rather than a hole.
    """
    box = {}

    def hook(step):
        if step == 599:
            loc = __import__("sys")._getframe(1).f_locals
            d2v = loc["digest_to_vehicle"]
            box["tombstones"] = sum(1 for v in d2v.values() if isinstance(v, _RetiredDevice))
            box["total"] = len(d2v)
            box["ids_ok"] = all(isinstance(v.vid, int) and v.is_rsu is False
                                for v in d2v.values() if isinstance(v, _RetiredDevice))

    runmod.PER_STEP_HOOK = hook
    try:
        run_pipeline(_flow_cfg(str(tmp_path / "tomb"), 600.0))
    finally:
        runmod.PER_STEP_HOOK = None

    assert box["tombstones"] > 100, box
    assert box["total"] > box["tombstones"]          # the driving ones are still real Vehicles
    assert box["ids_ok"]


# --------------------------------------------------------------------------- #
# Same dataset.
# --------------------------------------------------------------------------- #
def test_the_pinned_reference_digest_is_unchanged(tmp_path):
    r = run_pipeline(_flow_cfg(str(tmp_path / "ref"), 300.0))
    assert r.data_digest == "b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815", \
        r.data_digest


def test_the_arrival_process_is_drained_so_late_arrivals_still_reach_ground_truth(tmp_path):
    """An arrival landing in the run's final `dt` is created even though it never activates.

    The eager pre-pass drew every arrival with `tt < total_time`, including the ones past the last
    step, and they appear in `gt_vehicle` / `gt_identity_map`. The lazy pump is drained after the
    loop for exactly that reason; without the drain those rows would silently disappear.
    """
    out = str(tmp_path / "drain")
    r = run_pipeline(_flow_cfg(out, 300.0))
    rows = [json.loads(ln) for ln in open(os.path.join(out, "ground_truth", "gt_vehicle.jsonl"),
                                          encoding="utf-8") if ln.strip()]
    assert len(rows) == r.n_vehicles
    last_step_t = 299 * 1.0                          # dt = 1.0 s, so the last step is at t = 299
    assert any(v["spawn_time"] > last_step_t for v in rows), \
        "no arrival past the last step -- the drain is untested by this seed"
    idmap = sum(1 for ln in open(os.path.join(out, "ground_truth", "gt_identity_map.jsonl"),
                                 encoding="utf-8") if ln.strip())
    assert idmap >= len(rows)


def test_every_attacker_still_has_exactly_one_ground_truth_attack_row(tmp_path):
    """`gt_attacks` is now written as attackers retire, plus whoever is still live at the end."""
    out = str(tmp_path / "atk")
    run_pipeline(_flow_cfg(out, 600.0))
    veh = [json.loads(ln) for ln in open(os.path.join(out, "ground_truth", "gt_vehicle.jsonl"),
                                         encoding="utf-8") if ln.strip()]
    atk = [json.loads(ln) for ln in open(os.path.join(out, "ground_truth", "gt_attacks.jsonl"),
                                         encoding="utf-8") if ln.strip()]
    attackers = {v["true_vehicle_id"] for v in veh if v["is_attacker"]}
    assert attackers
    assert {a["true_vehicle_id"] for a in atk} == attackers
    assert len(atk) == len(attackers)                # no duplicates from the retire/finalise split
    assert [a["attack_id"] for a in atk] == sorted(a["attack_id"] for a in atk)
