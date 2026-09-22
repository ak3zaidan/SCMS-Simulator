"""The Python SDK, tested where it needs Python.

The Rust-side tests (`cargo test -p v2xw-py`) cover the bindings without a wheel. These
cover what only exists on this side: the ``pyarrow`` zero-copy path, the detector seam's
Arrow round trip, and the Python halves of the conformance kit.

Every plug-in test comes in a pair — a well-behaved plug-in passing, and the fault the
check exists for making the same check fail.
"""

from __future__ import annotations

import math as stdlib_math

import pytest

import v2xw
from v2xw.plugins import CarFollowing, Detector, card, equation, parameter, source

pa = pytest.importorskip("pyarrow", reason="the zero-copy Arrow path needs pyarrow")

BUILD_UTC = "2026-09-22T00:00:00Z"


# --------------------------------------------------------------------------------------
# scenarios and runs
# --------------------------------------------------------------------------------------


@pytest.fixture(scope="module")
def scenario():
    """Five seconds of Poisson traffic on the procedural grid."""
    doc = v2xw.Scenario.minimal().as_dict()
    doc["meta"]["name"] = "test-traffic"
    doc["time"]["duration_s"] = 5.0
    doc["actors"]["vehicles"]["demand"] = {
        "kind": "mobility/demand/poisson-thinned",
        "rate_veh_per_h": 600.0,
        "params": None,
    }
    return v2xw.Scenario.from_dict(doc)


@pytest.fixture(scope="module")
def run(scenario, tmp_path_factory):
    path = tmp_path_factory.mktemp("run") / "run.mcap"
    return (
        v2xw.run(
            scenario,
            build_utc=BUILD_UTC,
            recording=str(path),
            metric_window_s=1.0,
        ),
        str(path),
    )


def test_a_scenario_reports_every_problem_without_raising():
    doc = v2xw.Scenario.minimal().as_dict()
    doc["time"]["duration_s"] = -1.0
    doc["time"]["mobility_step_ms"] = 5  # outside ADR 0004's 10-100 ms
    with pytest.raises(v2xw.ScenarioError):
        v2xw.Scenario.from_dict(doc)


def test_the_manifest_timestamp_is_the_callers(run):
    result, _ = run
    assert result.manifest["build_utc"] == BUILD_UTC


def test_the_run_and_the_recording_agree(run):
    result, path = run
    assert result.records > 0, "a run with traffic on it must emit records"
    assert result.records_refused == 0
    rec = v2xw.Recording.open(path)
    report = rec.verify()
    assert report["records"] == result.records
    assert report["integrity_verified"] is True
    assert rec.manifest["scenario_hash"] == result.scenario_hash


def test_two_runs_of_one_scenario_agree(scenario):
    a, b = v2xw.run_twice(scenario, build_utc=BUILD_UTC)
    assert a == b
    assert len(a) == 64


# --------------------------------------------------------------------------------------
# the Arrow hand-off
# --------------------------------------------------------------------------------------


def test_metrics_cross_as_arrow_without_a_copy(run):
    result, _ = run
    batch = result.metrics.arrow()
    assert isinstance(batch, pa.RecordBatch)
    assert batch.num_rows == len(result.metrics)
    assert "metric" in batch.schema.names
    # The schema carries its own id, so a reader knows which version of the table it has.
    assert batch.schema.metadata[b"schema"].startswith(b"v2xw/metric-sample/")


def test_the_ipc_buffer_and_the_zero_copy_batch_hold_the_same_table(run):
    result, _ = run
    table = v2xw.read_ipc(result.metrics.ipc())
    batch = result.metrics.arrow()
    assert table.num_rows == batch.num_rows
    assert table.column_names == batch.schema.names
    assert table.column("metric").to_pylist() == batch.column("metric").to_pylist()


def test_a_recording_channel_crosses_as_arrow(run):
    _, path = run
    rec = v2xw.Recording.open(path)
    channels = [c["topic"].removeprefix("record/") for c in rec.channels if c["topic"].startswith("record/")]
    assert channels, "the recording must declare at least one record channel"
    channel = sorted(channels)[0]
    batch = rec.arrow(channel)
    assert isinstance(batch, pa.RecordBatch)
    assert batch.num_rows > 0
    assert "sim_time_ns" in batch.schema.names

    # The same channel as an IPC buffer holds the same rows.
    table = v2xw.read_ipc(rec.table(channel))
    assert table.num_rows == batch.num_rows


@pytest.mark.xfail(
    strict=True,
    reason=(
        "LEAK, in v2xw-record, not here. `export::schema::ground_truth_fields` names "
        "phy.rx's oracle columns `tx_node`, `distance_m` and `los_class`, but the record "
        "the engine actually writes calls them `tx` and `dist_m` and has no `los_class`. "
        "The names do not match, so `without_ground_truth()` drops nothing and a "
        "`ground_truth=False` export of phy.rx carries the true transmitter id and the "
        "true distance — which is exactly the state a NODE export must not have. Fixing "
        "the field list in v2xw-record makes this test XPASS, which is why it is strict."
    ),
)
def test_dropping_the_ground_truth_columns_drops_columns(run):
    """`ground_truth=False` removes the GT-tagged columns of a mixed channel.

    The channel has to be one that mixes: `phy.rx` is visible to the receiving node, but
    the transmitter's identity, the true distance and the line-of-sight class are oracle
    state that no node can see. A wholly-GT channel like `gt.kinematics` has no tagged
    columns at all — the whole channel is the tag — so stripping it is correctly a no-op
    and would make this test pass without checking anything.
    """
    _, path = run
    rec = v2xw.Recording.open(path)
    topics = [c["topic"].removeprefix("record/") for c in rec.channels]
    if "phy.rx" not in topics:
        pytest.skip("this run recorded no phy.rx")
    full = rec.arrow("phy.rx", ground_truth=True)
    stripped = rec.arrow("phy.rx", ground_truth=False)
    dropped = set(full.schema.names) - set(stripped.schema.names)
    assert dropped, (
        "phy.rx has GT-tagged columns; if nothing was dropped, the stripping is a no-op "
        "that would silently leak oracle state into a NODE export"
    )
    assert dropped <= {"tx_node", "distance_m", "los_class"}, (
        f"only the tagged columns may be dropped, got {dropped}"
    )


def test_the_ground_truth_field_list_does_not_match_the_channel(run):
    """Pins the mismatch the test above is expected to fail on.

    Written as an assertion rather than a comment so that the defect is data: when
    `v2xw-record` renames its field list to match the records, this test fails and says
    which name changed, and the `xfail` above turns into an `XPASS`. Two tests fail at once
    when the leak is fixed, which is the loudest possible signal that it was.
    """
    _, path = run
    rec = v2xw.Recording.open(path)
    if "record/phy.rx" not in [c["topic"] for c in rec.channels]:
        pytest.skip("this run recorded no phy.rx")
    names = set(rec.arrow("phy.rx").schema.names)
    declared = {"tx_node", "distance_m", "los_class"}
    assert not (declared & names), (
        "v2xw-record's declared GT field list for phy.rx now matches the channel — the "
        "leak is fixed, and the xfail above should be removed"
    )
    # The columns that actually carry the oracle state, under the names the engine writes.
    assert {"tx", "dist_m"} <= names


# --------------------------------------------------------------------------------------
# v2xw.math
# --------------------------------------------------------------------------------------


def test_v2xw_math_is_not_the_platform_libm_by_accident():
    # They agree to within the last bit or two on this platform, which is the point: the
    # difference is invisible until it moves a digest on another machine. What is asserted
    # is that `v2xw.math` is a distinct implementation that is close to, and need not
    # equal, the interpreter's.
    for x in (0.5, 1.0, 2.0, 10.0, 123.456):
        assert v2xw.math.exp(x) == pytest.approx(stdlib_math.exp(x), rel=1e-15)
        assert v2xw.math.ln(x) == pytest.approx(stdlib_math.log(x), rel=1e-15)
    assert v2xw.math.sqrt(2.0) == stdlib_math.sqrt(2.0), "sqrt is IEEE-exact both ways"


def test_quantisation_round_trips_on_its_grid():
    q = v2xw.math.quantize_to(1.23456789, 1e-3)
    assert q == 1.235
    assert v2xw.math.is_on_grid(q, 1e-3)
    assert not v2xw.math.is_on_grid(1.23456789, 1e-3)
    assert v2xw.math.grid_index(q, 1e-3) == 1235


def test_ordered_summation_ignores_the_input_order():
    xs = [1e-16, 1.0, 1e16, 2.0, -1e16]
    assert v2xw.math.sum_ordered(xs) == v2xw.math.sum_ordered(list(reversed(xs)))


# --------------------------------------------------------------------------------------
# the car-following plug-in
# --------------------------------------------------------------------------------------

m = v2xw.math


class Linear(CarFollowing):
    """The simplest longitudinal model that is not wrong: proportional to the speed error.

    Not a research model, and its card says so. It exists because a conformance test should
    not depend on the correctness of the model it is testing.
    """

    card = card(
        id="mobility/car-following/test-linear",
        family="mobility",
        version="0.1.0",
        purpose="Accelerate proportionally to the speed error; brake on a short gap. A "
        "fixture, not a model of driving.",
        parameters=[
            parameter(
                "gain",
                "1/s",
                0.5,
                source(
                    "todo-calibrate",
                    "a fixture value; nothing is calibrated against it",
                ),
                calibration="Nothing: this is a test fixture and must not be calibrated "
                "or used as a model of driving.",
            )
        ],
        equations=[equation("linear", "a = k (v0 - v)")],
        limitations=["It is not a model of human driving and must not be used as one."],
    )

    GAIN = 0.5

    def accel(self, ego, leader, lane, weather):
        v0 = min(ego.desired_speed_mps, lane.speed_limit_mps)
        a = self.GAIN * (v0 - ego.speed_mps)
        if leader is not None and leader.gap_m < ego.min_gap_m:
            return -ego.comfort_decel_mps2
        return a


def test_a_well_behaved_plug_in_passes_the_whole_kit():
    report = v2xw.conformance.check(Linear())
    assert report.passed, str(report)
    assert not report.failures
    report.raise_for_failures()


def test_the_plug_in_is_called_through_the_engines_trait():
    handle = Linear().attach()
    assert handle.id == "mobility/car-following/test-linear"
    # At the limit, no acceleration. Below it, positive. Inside the jam distance, braking.
    assert handle.accel(13.888889) == pytest.approx(0.0, abs=1e-9)
    assert handle.accel(0.0) > 0.0
    assert handle.accel(10.0, gap_m=0.5, leader_speed_mps=0.0) < 0.0


def test_a_plug_in_that_draws_random_numbers_fails():
    class Sloppy(Linear):
        card = dict(Linear.card, id="mobility/car-following/test-sloppy")

        def accel(self, ego, leader, lane, weather):
            import random

            return super().accel(ego, leader, lane, weather) + random.random()

    report = v2xw.conformance.check(Sloppy())
    assert not report.passed
    failed = {c.check for c in report.failures}
    assert "determinism" in failed
    assert "purity" in failed, "the purity guard must catch the draw, not only the digest"
    assert any("random" in c.detail for c in report.warnings), (
        "the source scan must also see it"
    )
    with pytest.raises(v2xw.DeterminismError):
        report.raise_for_failures()


def test_a_plug_in_that_reads_the_wall_clock_fails():
    class Clocky(Linear):
        card = dict(Linear.card, id="mobility/car-following/test-clocky")

        def accel(self, ego, leader, lane, weather):
            import time

            return super().accel(ego, leader, lane, weather) + (time.time() % 1e-9)

    report = v2xw.conformance.check(Clocky())
    assert not report.passed
    assert "purity" in {c.check for c in report.failures}
    assert any("time.time" in c.detail for c in report.failures)


def test_the_purity_guard_puts_everything_back():
    import random
    import time

    before = (random.random, time.time)
    with v2xw.conformance.purity_guard():
        assert random.random is not before[0]
    assert (random.random, time.time) == before, (
        "the guard must restore the interpreter it borrowed"
    )
    random.random()  # would raise if it had not


def test_a_card_declaring_an_rng_domain_is_refused():
    doc = dict(Linear.card)
    doc["id"] = "mobility/car-following/test-rng"
    doc["determinism"] = {"uses_rng": True, "rng_domains": ["fading"]}

    class Claims(Linear):
        card = doc

    report = v2xw.conformance.check(Claims())
    assert "rng domains" in {c.check for c in report.failures}


def test_a_card_with_an_uncited_default_and_no_plan_is_reported():
    doc = dict(Linear.card)
    doc["id"] = "mobility/car-following/test-uncited"
    doc["parameters"] = [
        {
            "name": "fudge",
            "unit": "-",
            "default": 1.7,
            "source": {"kind": "todo-calibrate", "ref": "picked to make the plot look right"},
            "calibration": "",
        }
    ]
    assert v2xw.plugins.uncited_parameters(doc) == ["fudge"]

    doc2 = dict(doc)
    doc2["parameters"] = [dict(doc["parameters"][0], calibration="Fit to the NGSIM set.")]
    assert v2xw.plugins.uncited_parameters(doc2) == []


# --------------------------------------------------------------------------------------
# the detector plug-in
# --------------------------------------------------------------------------------------


def a_batch() -> "pa.RecordBatch":
    """A window of received messages, as a detector sees one."""
    return pa.record_batch(
        {
            "node": pa.array([1, 2, 3, 4], type=pa.uint32()),
            "speed_mps": pa.array([13.9, 300.0, 25.0, 91.0], type=pa.float64()),
        }
    )


class SpeedDetector(Detector):
    """The same check the Rust reference detector makes, in Python, for comparison."""

    card = card(
        id="detect/local/test-speed",
        family="detector",
        version="0.1.0",
        purpose="Flag a claimed speed above a threshold. A fixture for the plug-in seam.",
        parameters=[
            parameter(
                "max_speed_mps",
                "m/s",
                90.0,
                source("todo-calibrate", "not fitted: 'faster than any road vehicle'"),
                calibration="Fit to the benign speed distribution of the target scenario.",
            )
        ],
    )

    MAX_SPEED_MPS = 90.0

    def on_messages(self, ctx, batch):
        # Columnwise, which is the reason the batch is a batch. A row loop here would
        # throw away what the Arrow hand-off bought.
        nodes = batch.column("node").to_pylist()
        speeds = batch.column("speed_mps").to_pylist()
        return [
            v2xw.plugins.Observation(node, "speed-plausibility", 1.0, ctx.t_ns)
            for node, speed in zip(nodes, speeds)
            if speed > self.MAX_SPEED_MPS
        ]


def test_a_python_detector_and_the_rust_reference_agree():
    batch = a_batch()
    python = SpeedDetector().attach().on_messages(5000, 9, batch)
    rust = v2xw.plugins.ReferenceDetector().on_messages(5000, 9, batch)

    assert [(o.subject, o.kind, o.confidence, o.t_ns) for o in python] == [
        (o.subject, o.kind, o.confidence, o.t_ns) for o in rust
    ], "a Python detector and the Rust reference must agree on an obvious case"
    assert [o.subject for o in python] == [2, 4]


def test_a_detector_that_raises_is_not_silently_empty():
    class Broken(SpeedDetector):
        card = dict(SpeedDetector.card, id="detect/local/test-broken")

        def on_messages(self, ctx, batch):
            raise KeyError("subject_id")

    handle = Broken().attach()
    with pytest.raises(v2xw.V2xwError, match="subject_id"):
        handle.on_messages(0, 0, a_batch())


def test_an_observations_confidence_is_quantised():
    obs = v2xw.plugins.Observation(7, "k", 0.123456789, 1000)
    assert obs.confidence == 0.123457
    assert v2xw.math.is_on_grid(obs.confidence, v2xw.plugins.PROBABILITY_QUANTUM)
    for bad in (-0.1, 1.5, float("nan")):
        with pytest.raises(v2xw.V2xwError):
            v2xw.plugins.Observation(7, "k", bad, 0)
