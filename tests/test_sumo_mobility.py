"""SUMO-backed mobility for the PYTHON engine: freeze, replay, and the coherence it has to hold.

WHAT IS BEING PROVEN, and why each half matters.

* **OFF is inert.** The knobs exist, flow the whole pipeline, and change NOTHING. The default
  golden is re-measured here rather than assumed, because the dataset-bearing engine is the one
  under change and a mobility seam is exactly where a stray rng draw hides.
* **ON is reproducible.** Replaying one frozen trajectory twice is byte-identical, and re-freezing
  under a different SUMO seed moves the recorded INPUT HASH -- which is the whole point of freezing
  rather than driving SUMO in-loop. A different `data_digest` is then explained by a different
  input, not by an unexplained engine regression.
* **The vehicles are on the engine's roads.** `dist_to_road` feeds the mapOffRoad detector and the
  geometric channel ray-casts building footprints registered to the same frame, so a replay that
  lands in a different frame silently poisons both. The gate is asserted positively (the measured
  distribution is lane-tight) and negatively (a deliberately misregistered frame is REFUSED).
* **Honest certificates outlive their trips.** Budgeting from the internal Trip is a guess; SUMO
  already drove the trip, so the budget is exact.

The SUMO-dependent tests skip when the toolchain is absent; the config-surface tests never do.

IMPORT-ORDER TRAP, measured here, and the reason the fixture shells out to freeze. `libsumo` and
`pyarrow` fight over native DLLs and whoever imports first wins: after `import pandas`, `import
libsumo` raises `DLL load failed while importing _libsumo`; after `import libsumo`, pandas' parquet
engine fails with "Unable to find a usable engine". In a FULL-SUITE run an earlier module has
already pulled pandas in through `datagen/featurize.py`, so an in-process `freeze()` here would
raise -- or, with an `import libsumo` skip guard, silently SKIP this entire file while the suite
stayed green and running the file alone passed. Both are unacceptable. So the fixture freezes
through `python -m scms_sim_ref.mock_pipeline.sumo_trace` in a FRESH interpreter (which also
exercises the shipped CLI), and everything in-process uses only `sumolib`, which is pure Python.
"""
import json
import math
import os
import shutil

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline
from scms_sim_ref.mock_pipeline import sumo_trace as st
from scms_sim_ref.mock_pipeline.run import validate_config
from scms_sim_ref.api import registry as _registry

GOLDEN_DEFAULT = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

#: the scenario the SUMO-dependent tests share: small enough to freeze in well under a second
GRID_N = 4
GRID_LEN = 150
STEPS = 120
DT = 1.0


REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _have_sumo() -> bool:
    """SUMO_HOME + sumolib only. `libsumo` is deliberately NOT imported here -- see the module
    docstring: importing it in a process that already has pandas raises, and importing it to decide
    whether to skip would make the skip a function of test ORDER."""
    if not os.environ.get("SUMO_HOME"):
        return False
    try:
        import sumolib  # noqa: F401,PLC0415
    except ImportError:
        return False
    return os.path.exists(os.path.join(os.environ["SUMO_HOME"], "bin"))


needs_sumo = pytest.mark.skipif(not _have_sumo(), reason="SUMO_HOME + sumolib required")


def _freeze_out_of_process(net, routes, out, run_seed, steps=None, dt=None, extra=()):
    """Freeze through the shipped CLI in a FRESH interpreter (see the module docstring)."""
    import subprocess
    import sys
    cmd = [sys.executable, "-m", "scms_sim_ref.mock_pipeline.sumo_trace",
           "--net", net, "--routes", routes, "--out", out,
           "--run-seed", str(run_seed), "--steps", str(steps or STEPS), "--dt", str(dt or DT),
           *[str(a) for a in extra]]
    r = subprocess.run(cmd, capture_output=True, text=True, cwd=REPO,
                       env={**os.environ, "PYTHONPATH": os.path.join(REPO, "src")})
    assert r.returncode == 0, f"freeze failed:\n{r.stdout}\n{r.stderr}"
    assert st.file_sha256(out) in r.stdout, "the CLI must print the artifact's sha256"
    return st.load(out)


# --------------------------------------------------------------------------- #
# scenario fixture
# --------------------------------------------------------------------------- #
@pytest.fixture(scope="module")
def scenario(tmp_path_factory):
    """A netgenerate grid + randomTrips demand + two frozen traces (two SUMO seeds).

    NOTE the directory: SUMO's tools embed their own command line in an XML comment and `--` is
    illegal inside one, so a scenario written into a path containing a double dash yields a SILENTLY
    EMPTY routes file and a run with zero vehicles. pytest's tmp base has no `--`; `freeze()` refuses
    such a path outright, which is asserted below."""
    import re
    import subprocess
    import sys

    d = tmp_path_factory.mktemp("sumoscen")
    assert "--" not in str(d), f"pytest tmp path has a double dash: {d}"
    home = os.environ["SUMO_HOME"]
    net = str(d / "grid.net.xml")
    rou = str(d / "grid.trips.xml")
    # `-j traffic_light` and NOT `--tls.guess`: the heuristic keys on traffic volume, so on a
    # demand-free procedural grid --tls.guess signalises exactly nothing.
    subprocess.run([os.path.join(home, "bin", "netgenerate.exe"), "--grid",
                    "--grid.number", str(GRID_N), "--grid.length", str(GRID_LEN),
                    "-j", "traffic_light", "-o", net], check=True, capture_output=True)
    # no --validate: it clobbers -o with a duarouter config dump instead of trips
    subprocess.run([sys.executable, os.path.join(home, "tools", "randomTrips.py"),
                    "-n", net, "-o", rou, "-e", str(STEPS), "-p", "0.5", "--seed", "11"],
                   check=True, capture_output=True)
    body = open(rou, encoding="utf-8").read()
    open(rou, "w", encoding="utf-8").write(re.sub(r"<!--.*?-->", "", body, flags=re.S))

    a = _freeze_out_of_process(net, rou, str(d / "a.trace"), 42)
    b = _freeze_out_of_process(net, rou, str(d / "b.trace"), 99)
    return {"dir": str(d), "net": net, "routes": rou, "a": a, "b": b,
            "a_path": str(d / "a.trace"), "b_path": str(d / "b.trace")}


def _cfg(scenario, out, **kw):
    base = dict(seed=42, traffic_flow=True, duration_s=STEPS * DT, dt=DT,
                road_network="sumo", sumo_net=scenario["net"],
                mobility_source="sumo_replay", sumo_trace=scenario["a_path"],
                attacker_pct=0.2, faulty_pct=0.05, verbose=False, out_dir=str(out))
    base.update(kw)
    return PipelineConfig(**base)


# --------------------------------------------------------------------------- #
# 1. the OFF path: the surface exists and changes nothing
# --------------------------------------------------------------------------- #
NEW_FIELDS = {
    "mobility_source": ("internal", "Mobility"),
    "sumo_trace": ("", "Mobility"),
    "sumo_trace_sha256": ("", "Mobility"),
    "sumo_cert_slack_s": (30.0, "Mobility"),
    "sumo_offroad_p95_max_m": (8.0, "Mobility"),
    "sumo_net": ("", "Network"),
    "sumo_frame_city": ("", "Network"),
}


def test_new_knobs_are_self_describing_and_default_inert():
    cfg = PipelineConfig()
    sch = config_schema()
    for name, (default, group) in NEW_FIELDS.items():
        assert getattr(cfg, name) == default, f"{name} default moved"
        assert name in sch, f"{name} missing from config_schema()"
        assert sch[name]["default"] == default
        assert sch[name]["group"] == group, f"{name} grouped as {sch[name]['group']}"
        assert sch[name]["help"], f"{name} has no help text"
    assert sch["mobility_source"]["widget"] == "select"
    assert sch["mobility_source"]["options"] == ["internal", "sumo_replay"]
    assert "sumo" in sch["road_network"]["options"]


def test_mobility_slot_is_no_longer_empty():
    """The registry shipped an EMPTY `mobility` slot; these are the built-ins that fill it, and the
    enum, the CLI choices and validate_config's message all read that one list."""
    assert _registry.builtin_names("mobility") == ("internal", "sumo_replay")
    assert _registry.builtin_names_sorted("mobility") == ("internal", "sumo_replay")
    for name in ("internal", "sumo_replay"):
        cls = _registry.builtin("mobility", name)
        assert cls.NAME == name
        assert cls.INTERFACE_NAME == "scms.mobility"
        assert callable(cls.capabilities)
    assert "car_following" in _registry.builtin("mobility", "internal").capabilities()
    assert _registry.builtin("mobility", "sumo_replay") is st.SumoReplayMobility


def test_cli_exposes_every_new_knob():
    import subprocess
    import sys
    env = {**os.environ, "PYTHONPATH": os.path.join(REPO, "src")}
    r = subprocess.run([sys.executable, "-m", "scms_sim_ref.mock_pipeline.run", "--help"],
                       capture_output=True, text=True, env=env, cwd=REPO)
    assert r.returncode == 0, r.stderr
    for flag in ("--mobility-source", "--sumo-net", "--sumo-frame-city", "--sumo-trace",
                 "--sumo-trace-sha256", "--sumo-cert-slack", "--sumo-offroad-p95-max"):
        assert flag in r.stdout, f"{flag} is not reachable from the CLI"


def test_default_golden_is_byte_identical_with_the_seam_present(tmp_path):
    """The pinned default. If a mobility seam leaks one rng draw onto the internal path, this is
    where it shows up."""
    res = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "golden")))
    assert res.data_digest == GOLDEN_DEFAULT


# --------------------------------------------------------------------------- #
# 2. the refusal matrix -- every wrong combination says why
# --------------------------------------------------------------------------- #
@pytest.mark.parametrize("kw,needle", [
    (dict(mobility_source="nope"), "mobility_source must be"),
    (dict(road_network="sumo", traffic_flow=True, duration_s=10), "needs sumo_net"),
    (dict(road_network="sumo", sumo_net="no_such.net.xml", traffic_flow=True, duration_s=10),
     "sumo_net not found"),
    (dict(sumo_net="x.net.xml"), "sumo_net needs road_network"),
    (dict(sumo_frame_city="ingolstadt"), "sumo_frame_city needs road_network"),
    (dict(sumo_trace="x.trace"), "sumo_trace needs mobility_source"),
    (dict(sumo_trace_sha256="ab"), "sumo_trace_sha256 needs mobility_source"),
    (dict(mobility_source="sumo_replay", traffic_flow=False), "needs traffic_flow=true"),
    (dict(mobility_source="sumo_replay", traffic_flow=True, duration_s=10, road_network="grid"),
     "needs road_network='sumo'"),
    (dict(sumo_cert_slack_s=-1.0), "sumo_cert_slack_s must be"),
    (dict(sumo_offroad_p95_max_m=0.0), "sumo_offroad_p95_max_m must be"),
])
def test_bad_mobility_configs_are_refused_with_a_reason(kw, needle):
    with pytest.raises(ValueError, match=needle.replace("(", r"\(").replace("'", "'")):
        validate_config(PipelineConfig(**kw))


@needs_sumo
def test_replay_refuses_directed_lanes(scenario, tmp_path):
    """SUMO already put every vehicle on its own carriageway; offsetting the engine's centrelines
    again would move the roads off the replayed traffic."""
    with pytest.raises(ValueError, match="incompatible with directed_lanes"):
        validate_config(_cfg(scenario, tmp_path / "dl", directed_lanes=True))


def test_freeze_validates_arguments_before_loading_libsumo(tmp_path):
    """Two things at once.

    The trap: SUMO writes its own command line into an XML comment, where `--` is illegal, so a
    scenario written under such a path comes back SILENTLY EMPTY.

    And the ordering: this test is deliberately NOT marked `needs_sumo`, so it also runs in a
    full-suite process where `import libsumo` would raise (pandas got the DLLs first). A refusal
    that depends on an optional heavy dependency being importable is not a refusal."""
    with pytest.raises(ValueError, match="double dash"):
        st.freeze(net=str(tmp_path / "a--b" / "x.net.xml"), routes="r.xml",
                  out=str(tmp_path / "t.trace"), seed=1, steps=10)
    with pytest.raises(ValueError, match="needs either routes"):
        st.freeze(net=str(tmp_path / "x.net.xml"), out=str(tmp_path / "t.trace"), seed=1, steps=10)
    with pytest.raises(ValueError, match="steps must be"):
        st.freeze(net=str(tmp_path / "x.net.xml"), routes="r.xml",
                  out=str(tmp_path / "t.trace"), seed=1, steps=0)


# --------------------------------------------------------------------------- #
# 3. Phase A -- the frozen artifact is canonical and seed-controlled
# --------------------------------------------------------------------------- #
@needs_sumo
def test_freeze_is_deterministic_and_seed_controlled(scenario, tmp_path):
    again = _freeze_out_of_process(scenario["net"], scenario["routes"],
                                   str(tmp_path / "again.trace"), 42)
    assert again.sha256 == scenario["a"].sha256, "same seed must reproduce the artifact byte for byte"
    assert scenario["b"].sha256 != scenario["a"].sha256, "a different seed must move the hash"
    assert scenario["b"].meta["sumo_seed"] != scenario["a"].meta["sumo_seed"]
    assert scenario["a"].meta["sumo_seed"] == st.derive_sumo_seed(42)


@needs_sumo
@pytest.mark.parametrize("extra", [(), ("--warmup", "0"), ("--substeps", "1"),
                                   ("--time-to-teleport", "-1")])
def test_warmup_and_substeps_are_byte_identical_at_their_defaults(scenario, tmp_path, extra):
    """The artifact hash may not move because an option that is OFF now exists.

    `meta` is inside the hashed file, so an unconditional new key would re-hash every trace ever
    frozen and turn `--sumo-trace-sha256` pins into false alarms. Both keys are therefore emitted
    only when the option is actually engaged."""
    t = _freeze_out_of_process(scenario["net"], scenario["routes"],
                               str(tmp_path / f"d{len(extra)}{extra[-1] if extra else ''}.trace"),
                               42, extra=extra)
    assert t.sha256 == scenario["a"].sha256, f"{extra or 'no flags'} moved the artifact hash"
    for k in ("warmup_steps", "substeps", "time_to_teleport", "split_on_gap", "gap_splits"):
        assert k not in t.meta, f"{k} must not appear in meta at its default"
    inv = t.meta["invocation"]
    assert inv[inv.index("--time-to-teleport") + 1] == "-1", (
        "the default must still render as the integer '-1' SUMO's CLI was given before it became "
        "a parameter -- the invocation is inside the hashed artifact")


@needs_sumo
def test_warmup_starts_the_recording_on_a_loaded_network(scenario, tmp_path):
    """A warm-up is not a longer trace: it is the SAME window, entered already full.

    `--begin T` makes SUMO discard every vehicle departing before T, so a window opened cold starts
    on an empty city and its first minutes are a fill transient rather than the traffic being
    studied. The measured consequence on InTAS's AM peak is 36 vehicles ten seconds after
    `--begin 25200` against ~3,400 with an hour of warm-up ahead of it."""
    cold = _freeze_out_of_process(scenario["net"], scenario["routes"],
                                  str(tmp_path / "cold.trace"), 42, steps=10)
    warm = _freeze_out_of_process(scenario["net"], scenario["routes"],
                                  str(tmp_path / "warm.trace"), 42, steps=10,
                                  extra=("--warmup", str(STEPS // 2)))
    assert warm.meta["warmup_steps"] == STEPS // 2
    assert warm.meta["step0_sim_time"] == pytest.approx(warm.meta["begin"]
                                                        + (STEPS // 2 + 1) * DT)
    assert warm.meta["end"] == pytest.approx(warm.meta["begin"] + (STEPS // 2 + 10) * DT)
    assert len(warm.vehicles) > len(cold.vehicles), (
        "a warmed-up window must open on more vehicles than a cold one "
        f"({len(warm.vehicles)} vs {len(cold.vehicles)})")
    # every vehicle already on the network is written from step 0, contiguously
    assert sum(1 for v in warm.vehicles if v.first_step == 0) > 0
    assert warm.meta["steps"] == 10, "steps counts RECORDED steps, not warm-up ones"


@needs_sumo
def test_substeps_keeps_the_integration_step_and_the_sample_rate_apart(scenario, tmp_path):
    """SUMO integrates at dt/N; the artifact -- and so the engine -- still steps at dt."""
    sub = _freeze_out_of_process(scenario["net"], scenario["routes"],
                                 str(tmp_path / "sub.trace"), 42, extra=("--substeps", "5"))
    assert sub.meta["substeps"] == 5
    assert sub.meta["dt"] == pytest.approx(DT), "the ARTIFACT's dt is untouched"
    assert sub.meta["sumo_step_length"] == pytest.approx(DT / 5)
    inv = sub.meta["invocation"]
    assert inv[inv.index("--step-length") + 1] == repr(DT / 5), "SUMO must get the sub-step"
    assert inv[inv.index("--end") + 1] == repr(float(sub.meta["end"]))
    assert sub.meta["steps"] == scenario["a"].meta["steps"]
    # a different integration step is a different simulation, so the trajectory must differ --
    # otherwise the flag would be doing nothing at all
    assert sub.sha256 != scenario["a"].sha256
    reloaded = st.load(str(tmp_path / "sub.trace"))
    assert len(reloaded.vehicles) == len(sub.vehicles)


@needs_sumo
def test_a_vehicle_that_leaves_and_returns_is_refused_by_default_and_split_on_request(
        scenario, tmp_path):
    """SUMO's teleport (and its parking manoeuvre) take a vehicle OUT of the network and put it back
    somewhere else. The trace format indexes state by `step - first_step`, so a gap written as a
    contiguous run mislabels every later step -- it must be refused, or recorded as a second
    trajectory, never papered over.

    A very short `--time-to-teleport` is used to MAKE the event happen on a procedural grid; on a
    real congested scenario it happens on its own -- InTAS's AM peak teleports 365 times in 25,271
    vehicles under its own 300 s policy, so a freeze of it is unwritable without this.
    """
    import subprocess
    import sys
    base = [sys.executable, "-m", "scms_sim_ref.mock_pipeline.sumo_trace",
            "--net", scenario["net"], "--routes", scenario["routes"],
            "--run-seed", "42", "--steps", str(STEPS), "--dt", str(DT),
            "--time-to-teleport", "1"]
    env = {**os.environ, "PYTHONPATH": os.path.join(REPO, "src")}
    r = subprocess.run(base + ["--out", str(tmp_path / "refused.trace")],
                       capture_output=True, text=True, cwd=REPO, env=env)
    assert r.returncode != 0 and "disappeared and came back" in r.stderr, (
        "a gap must be refused by default")
    assert "--split-on-gap" in r.stderr, "the refusal must name the option that handles it"

    ok = _freeze_out_of_process(scenario["net"], scenario["routes"],
                                str(tmp_path / "split.trace"), 42,
                                extra=("--time-to-teleport", "1", "--split-on-gap"))
    assert ok.meta["split_on_gap"] is True
    assert ok.meta["gap_splits"] >= 1, "the control must actually produce a gap to split"
    assert ok.meta["time_to_teleport"] == 1.0
    assert ok.meta["teleports"] >= 1
    seg = [v for v in ok.vehicles if "#" in v.sumo_id]
    assert len(seg) == ok.meta["gap_splits"]
    # every trajectory, split or not, is still contiguous -- which is what `load()` verifies
    st.load(str(tmp_path / "split.trace"))
    for v in seg:
        assert v.last_step >= v.first_step
        assert v.route_length_m >= 0.0, "a segment's own driven length, not the trip's cumulative"


@needs_sumo
def test_artifact_records_version_invocation_and_the_id_mapping(scenario):
    meta = scenario["a"].meta
    assert st.SUMO_VERSION_PINNED in meta["sumo_version"]
    inv = meta["invocation"]
    assert inv[0].startswith("sumo")
    # single-threaded, teleport-free, seeded -- the flags a reproduction depends on
    for flag, value in (("--seed", str(meta["sumo_seed"])), ("--threads", "1"),
                        ("--time-to-teleport", "-1"), ("--step-length", repr(DT))):
        assert flag in inv and inv[inv.index(flag) + 1] == value, f"{flag} not recorded"
    assert all(os.path.sep not in tok for tok in inv), "invocation must be path-independent"
    assert meta["net_sha256"] == st.file_sha256(scenario["net"])
    assert meta["routes_sha256"] == st.file_sha256(scenario["routes"])
    # the stable vehicle-id mapping is IN the file, not re-derived
    body = open(scenario["a_path"], encoding="utf-8").read().splitlines()
    vrows = [ln for ln in body if ln.startswith("V ")]
    assert len(vrows) == len(scenario["a"].vehicles) > 0
    for i, ln in enumerate(vrows):
        assert int(ln.split()[1]) == i


@needs_sumo
def test_artifact_rows_are_sorted_and_fixed_rounded(scenario):
    prev = (-1, -1)
    n = 0
    with open(scenario["a_path"], encoding="utf-8") as fh:
        for line in fh:
            if line[0] == "#" or line.startswith("V "):
                continue
            p = line.split()
            key = (int(p[0]), int(p[1]))
            assert key > prev, "rows must be sorted by (step, vehicle)"
            prev = key
            for tok in p[2:]:
                assert len(tok.split(".")[1]) == 3, f"unrounded value {tok!r}"
                assert tok != "-0.000", "negative zero must be normalised"
            n += 1
    assert n == scenario["a"].n_rows


@needs_sumo
def test_load_round_trips(scenario):
    back = st.load(scenario["a_path"])
    assert back.sha256 == scenario["a"].sha256
    assert back.n_rows == scenario["a"].n_rows
    assert [v.sumo_id for v in back.vehicles] == [v.sumo_id for v in scenario["a"].vehicles]
    v0 = back.vehicles[0]
    assert back.state(0, v0.first_step) is not None
    assert back.state(0, v0.last_step + 1) is None


# --------------------------------------------------------------------------- #
# 4. Phase B -- replay reproduces, and the input hash is the change channel
# --------------------------------------------------------------------------- #
@needs_sumo
def test_replaying_the_same_trace_twice_is_byte_identical(scenario, tmp_path):
    a = run_pipeline(_cfg(scenario, tmp_path / "r1"))
    b = run_pipeline(_cfg(scenario, tmp_path / "r2"))
    assert a.data_digest == b.data_digest
    assert a.n_vehicles == b.n_vehicles > 0
    assert a.n_reports == b.n_reports


@needs_sumo
def test_a_different_sumo_seed_moves_the_recorded_input_hash(scenario, tmp_path):
    a = run_pipeline(_cfg(scenario, tmp_path / "sa"))
    b = run_pipeline(_cfg(scenario, tmp_path / "sb", sumo_trace=scenario["b_path"]))
    ma = json.load(open(tmp_path / "sa" / "manifest.json"))
    mb = json.load(open(tmp_path / "sb" / "manifest.json"))
    assert ma["config"]["sumo_trace_sha256"] == scenario["a"].sha256
    assert mb["config"]["sumo_trace_sha256"] == scenario["b"].sha256
    assert ma["config"]["sumo_trace_sha256"] != mb["config"]["sumo_trace_sha256"]
    assert a.data_digest != b.data_digest, "different traffic must produce a different dataset"
    # ... and the manifest says which SUMO produced it
    assert ma["mobility"]["provider"]["sumo_version"].endswith(st.SUMO_VERSION_PINNED)
    assert ma["mobility"]["provider"]["sumo_seed"] == st.derive_sumo_seed(42)


@needs_sumo
def test_a_pinned_hash_refuses_a_re_frozen_trajectory(scenario, tmp_path):
    """The reason freeze-and-replay beats live TraCI: SUMO's nondeterminism becomes a DETECTABLE
    input change instead of a silent digest break that reads as an engine regression."""
    cfg = _cfg(scenario, tmp_path / "pin", sumo_trace=scenario["b_path"],
               sumo_trace_sha256=scenario["a"].sha256)
    with pytest.raises(ValueError, match="does not match the sha256 this config pins"):
        run_pipeline(cfg)


@needs_sumo
def test_a_matching_pin_is_accepted_and_recorded(scenario, tmp_path):
    cfg = _cfg(scenario, tmp_path / "pinok", sumo_trace_sha256=scenario["a"].sha256)
    res = run_pipeline(cfg)
    man = json.load(open(tmp_path / "pinok" / "manifest.json"))
    assert man["config"]["sumo_trace_sha256"] == scenario["a"].sha256
    assert res.n_vehicles > 0


# --------------------------------------------------------------------------- #
# 5. network coherence -- the replayed vehicles are ON the engine's roads
# --------------------------------------------------------------------------- #
@needs_sumo
def test_replayed_positions_follow_the_engine_network(scenario, tmp_path):
    run_pipeline(_cfg(scenario, tmp_path / "coh"))
    coh = json.load(open(tmp_path / "coh" / "manifest.json"))["mobility"]["coherence"]
    d = coh["dist_to_road_m"]
    assert d["n"] > 500
    # A road-following vehicle sits on a LANE centre, i.e. within about half a carriageway of the
    # engine's junction-to-junction centreline. Scattered positions (a misregistered frame) blow
    # straight through this.
    assert d["p50"] <= 3.0, d
    assert d["p95"] <= 4.0, d
    assert d["max"] <= 8.0, d


@needs_sumo
def test_a_misregistered_frame_is_REFUSED(scenario, tmp_path):
    """The negative arm, and the one that matters: the gate must actually fire. A 250 m translation
    is the kind of error a wrong projection origin produces -- every street still looks plausible,
    and dist_to_road / mapOffRoad / the geometric channel's building blockage all become nonsense."""
    from scms_sim_ref.mock_pipeline.roads import CustomNetwork
    from scms_sim_ref.mock_pipeline.osm import network_document
    from scms_sim_ref.mock_pipeline.run import _parse_custom_network

    nodes, edges, info, tf = st.engine_network(scenario["net"])
    net = CustomNetwork(*_parse_custom_network(network_document(nodes, edges, info)))
    good = st.SumoReplayMobility(scenario["a"], dt=DT, transform=tf)
    bad = st.SumoReplayMobility(scenario["a"], dt=DT,
                                transform=lambda x, y: (x + 250.0, y - 130.0))
    g = good.offroad_stats(net.dist_to_road)
    b = bad.offroad_stats(net.dist_to_road)
    assert g["p95"] <= 4.0 < b["p95"], (g, b)
    assert b["p95"] > PipelineConfig().sumo_offroad_p95_max_m, b


@needs_sumo
def test_engine_truth_equals_the_frozen_trace(scenario, tmp_path):
    """The replay actually replays. Ground-truth positions the engine wrote must BE the frozen
    trajectory, transformed once -- not something re-integrated on top of it."""
    run_pipeline(_cfg(scenario, tmp_path / "truth", emit_sample_prob=1.0))
    trace = st.load(scenario["a_path"])
    _n, _e, _i, tf = st.engine_network(scenario["net"])
    prov = st.SumoReplayMobility(trace, dt=DT, transform=tf)
    spans = prov.plan(total_time=STEPS * DT)
    by_vid = {i: sp.idx for i, sp in enumerate(spans)}
    checked = 0
    path = tmp_path / "truth" / "ground_truth" / "gt_emissions_sample.jsonl"
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            vid = int(r["true_vehicle_id"].split("_")[1])
            idx = by_vid.get(vid)
            if idx is None:
                continue
            step = int(round(r["t"] / DT))
            k = step - trace.vehicles[idx].first_step
            if k < 0 or k >= len(prov._x[idx]):
                continue
            assert abs(r["true_x"] - prov._x[idx][k]) < 1e-6, (vid, step)
            assert abs(r["true_y"] - prov._y[idx][k]) < 1e-6, (vid, step)
            assert abs(r["true_speed"] - prov._v[idx][k]) < 1e-3, (vid, step)
            assert abs(r["true_heading"] - prov._h[idx][k]) < 1e-3, (vid, step)
            checked += 1
    assert checked > 200, f"only {checked} ground-truth rows checked"


@needs_sumo
def test_heading_is_converted_out_of_sumos_convention(scenario):
    """SUMO reports degrees CLOCKWISE FROM NORTH; the engine's manifest declares
    `heading: deg_ccw_from_east`. Getting this wrong mirrors every heading and would quietly poison
    the headingInconsistency detector."""
    prov = st.SumoReplayMobility(scenario["a"], dt=DT)
    for veh in scenario["a"].vehicles[:20]:
        X, Y, V, A = scenario["a"].series(veh.idx)
        for k in range(0, len(A), 7):
            # the provider rounds to the artifact's own 3-decimal quantisation
            assert abs(prov._h[veh.idx][k] - (90.0 - A[k]) % 360.0) < 5e-4
            assert 0.0 <= prov._h[veh.idx][k] < 360.0


# --------------------------------------------------------------------------- #
# 6. certificate lifetime from the SUMO route
# --------------------------------------------------------------------------- #
@needs_sumo
def test_honest_certificates_cover_the_whole_trip(scenario, tmp_path):
    """Requirement 5. Under the internal model the trip duration is a GUESS (3x free-flow plus a
    signal-wait budget), and a short guess expires an honest certificate mid-trip -- a precision
    collapse that is an artifact of the budget, not of any attack. SUMO already drove the trip."""
    out = tmp_path / "cert"
    run_pipeline(_cfg(scenario, out, rotate_period_s=30.0))
    gt = {}
    with open(out / "ground_truth" / "gt_vehicle.jsonl", encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            gt[r["true_vehicle_id"]] = r
    windows: dict = {}
    with open(out / "ground_truth" / "gt_identity_map.jsonl", encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            windows.setdefault(r["true_vehicle_id"], []).append((r["valid_from"], r["valid_to"]))
    trace = st.load(scenario["a_path"])
    prov = st.SumoReplayMobility(trace, dt=DT)
    spans = prov.plan(total_time=STEPS * DT)
    assert len(spans) == len(gt) > 0
    for i, sp in enumerate(spans):
        w = sorted(windows[f"veh_{i:03d}"])
        assert w[0][0] <= sp.spawn_time + 1e-9, f"veh {i} has no valid cert at spawn"
        # the union of the windows is contiguous and reaches past the vehicle's LAST step
        end = w[0][1]
        for lo, hi in w[1:]:
            assert lo <= end + 1e-9, f"veh {i} has a certificate gap at {lo}"
            end = max(end, hi)
        assert end >= sp.finish_time - 1e-9, f"veh {i} cert expires at {end} < despawn {sp.finish_time}"

    # and no honest vehicle is ever reported for certValidity
    honest = 0
    idmap = {}
    with open(out / "ground_truth" / "gt_identity_map.jsonl", encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            idmap[r["pseudonym_cert_digest"]] = r["true_vehicle_id"]
    with open(out / "ma" / "ma_reports.jsonl", encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            tv = idmap.get(r.get("subject_cert_digest", ""))
            if tv and not gt[tv]["is_attacker"]:
                honest += sum(1 for d in r.get("detector_outputs", ())
                              if d["check_id"] == "certValidity" and d["verdict"] == "fail")
    assert honest == 0, f"{honest} certValidity reports against honest vehicles"


@needs_sumo
def test_trip_length_comes_from_the_sumo_route(scenario):
    """`Vehicle.trip` still exists for a replayed vehicle, built from the DRIVEN polyline, so
    everything that reads it keeps working -- and its length agrees with SUMO's own odometer."""
    from scms_sim_ref.mock_pipeline.roads import Trip
    prov = st.SumoReplayMobility(scenario["a"], dt=DT)
    err = []
    for sp in prov.plan(total_time=STEPS * DT):
        if sp.route_length_m < 20.0:
            continue
        trip = Trip(sp.polyline, sp.mean_speed, sp.spawn_time)
        err.append(abs(trip.length - sp.route_length_m) / sp.route_length_m)
        assert abs(trip.t1 - (sp.spawn_time + trip.length / trip.speed)) < 1e-6
    assert err and max(err) < 0.05, f"worst polyline/odometer disagreement {max(err):.4f}"


# --------------------------------------------------------------------------- #
# 7. the provider draws no randomness at all
# --------------------------------------------------------------------------- #
@needs_sumo
def test_replay_draws_no_randomness(scenario, monkeypatch):
    """Not one draw from any generator: that is what lets the mode exist beside a digest pinned to
    the COUNT and ORDER of the engine's global rng draws."""
    import random

    trace = st.load(scenario["a_path"])
    prov = st.SumoReplayMobility(trace, dt=DT)
    spans = prov.plan(total_time=STEPS * DT)

    def boom(*a, **k):                     # pragma: no cover - must never be reached
        raise AssertionError("the mobility provider drew randomness")

    monkeypatch.setattr(random.Random, "random", boom)
    monkeypatch.setattr(random.Random, "getrandbits", boom)

    class _V:
        __slots__ = ("vid", "cur_x", "cur_y", "cur_v", "cur_h", "s_pos")

        def __init__(self, vid):
            self.vid = vid
            self.cur_x = self.cur_y = self.cur_v = self.cur_h = self.s_pos = 0.0

    vs = [_V(i) for i in range(len(spans))]
    for i, sp in enumerate(spans):
        prov.bind(i, sp.idx)
    for step in range(STEPS):
        prov.advance(vs, step, step * DT)
    assert any(v.cur_x for v in vs)


@needs_sumo
def test_replay_ignores_a_dt_mismatch_loudly(scenario):
    with pytest.raises(ValueError, match="step grids must agree"):
        st.SumoReplayMobility(scenario["a"], dt=0.1)


# --------------------------------------------------------------------------- #
# 8. the whole SCMS stack still works on top of replayed mobility
# --------------------------------------------------------------------------- #
@needs_sumo
def test_the_full_stack_still_runs_over_replayed_mobility(scenario, tmp_path):
    """Attacks, detectors, the MA and the report writer are untouched code; this asserts they are
    still producing a complete, self-consistent dataset when the movement is SUMO's."""
    out = tmp_path / "stack"
    res = run_pipeline(_cfg(scenario, out, attacker_pct=0.3, collude_pct=0.4, victim_pct=0.2,
                            n_rsus=2, radio_model="geometric", rotate_period_s=45.0))
    assert res.n_vehicles > 0 and res.n_reports > 0 and res.n_revoked > 0
    man = json.load(open(out / "manifest.json"))
    assert man["mobility"]["source"] == "sumo_replay"
    assert man["mobility"]["network"]["n_signal_nodes"] == GRID_N * GRID_N
    for rel, meta in ((o["path"], o) for o in man["outputs"]):
        assert os.path.exists(os.path.join(out, rel)), rel
    assert man["data_digest_sha256"] == res.data_digest
    # the ORACLE/MA firewall still holds: no ma_reports row carries an oracle-only key
    with open(out / "ma" / "ma_reports.jsonl", encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            assert r["_visibility"] == "MA"
            assert not {"true_x", "true_y", "is_attacker", "true_vehicle_id"} & set(r)


@needs_sumo
def test_manifest_mobility_block_is_absent_when_the_mode_is_off(tmp_path):
    run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=30,
                                arrival_rate=1.5, grid_w=4, grid_h=4,
                                out_dir=str(tmp_path / "off")))
    man = json.load(open(tmp_path / "off" / "manifest.json"))
    assert "mobility" not in man, "the manifest must be byte-identical on the default path"


@needs_sumo
def test_config_round_trips_through_the_manifest(scenario, tmp_path):
    from scms_sim_ref.mock_pipeline.run import config_from_dict
    out = tmp_path / "rt"
    run_pipeline(_cfg(scenario, out))
    man = json.load(open(out / "manifest.json"))
    back = config_from_dict(man["config"])
    assert back.mobility_source == "sumo_replay"
    assert back.sumo_trace_sha256 == scenario["a"].sha256
    assert back.sumo_net == scenario["net"]
    res = run_pipeline(config_from_dict({**man["config"], "out_dir": str(tmp_path / "rt2")}))
    assert res.data_digest == man["data_digest_sha256"], "a manifest must replay to itself"


# --------------------------------------------------------------------------- #
# 5b. the coherence gate measures against the MAP, not against the routing graph
# --------------------------------------------------------------------------- #
@needs_sumo
def test_the_engine_measures_against_the_drivable_surface(scenario, tmp_path):
    """WHY THE GATE USED TO FIRE ON AN HONEST TRACE. `dist_to_road` was answering a map question
    with the routing graph -- one centreline per physical road, nothing at all inside a junction --
    so a vehicle crossing a signalised junction, or driving the other carriageway of a two-way
    street, measured as off-road. Measured on InTAS (1,188 vehicles, 4,071 sampled positions):
    graph chords p50 2.203 / p95 17.434 / max 95.227 m; with this surface p50 0.351 / p95 3.195 /
    max 4.804 m, against a raw-SUMO-network reference of p95 3.201 / max 6.400 m."""
    run_pipeline(_cfg(scenario, tmp_path / "surf"))
    mob = json.load(open(tmp_path / "surf" / "manifest.json"))["mobility"]
    surf = mob["network"]["road_surface"]
    assert surf["surface_segments"] > 0 and surf["junction_discs"] > 0
    assert mob["coherence"]["measured_against"] == "road_surface"
    # The surface is GEOMETRY, not topology: it carries one polyline per DIRECTED carriageway plus
    # the junction polygons, so it is strictly richer than the graph's one segment per undirected
    # edge -- while the graph itself (n_nodes) is untouched by installing it.
    assert surf["surface_segments"] > surf["graph_segments"] > 0
    assert mob["network"]["n_nodes"] == mob["network"]["strong_component_nodes"]
    assert surf["junction_discs"] >= mob["network"]["n_nodes"]


@needs_sumo
def test_a_misregistered_frame_is_refused_WITH_the_surface_installed(scenario):
    """The negative arm has to survive the fix. A more generous road surface must buy coherence for
    honest vehicles without buying it for a wrong projection origin -- the failure mode where every
    street still looks plausible and the whole city is 250 m from where the engine thinks it is."""
    from scms_sim_ref.mock_pipeline.roads import CustomNetwork
    from scms_sim_ref.mock_pipeline.osm import network_document
    from scms_sim_ref.mock_pipeline.run import _parse_custom_network

    nodes, edges, info, tf = st.engine_network(scenario["net"])
    net = CustomNetwork(*_parse_custom_network(network_document(nodes, edges, info)))
    stats = net.set_road_surface(**info["road_surface"])
    assert stats["junction_discs"] > 0
    good = st.SumoReplayMobility(scenario["a"], dt=DT, transform=tf)
    bad = st.SumoReplayMobility(scenario["a"], dt=DT,
                                transform=lambda x, y: (x + 250.0, y - 130.0))
    g = good.offroad_stats(net.dist_to_road)
    b = bad.offroad_stats(net.dist_to_road)
    assert g["p95"] <= PipelineConfig().sumo_offroad_p95_max_m, g
    assert b["p95"] > PipelineConfig().sumo_offroad_p95_max_m, b
    # and the surface must not have flattened the distribution into "everything is on a road"
    assert b["p50"] > 10.0, b


@needs_sumo
def test_the_engine_graph_is_the_strongly_connected_core_in_every_configuration(scenario):
    """`strongly_connected: false` is not a diagnostic to carry around: those junctions can be
    entered and never left, so a trip routed onto one strands. Before, whether they were dropped
    depended on `custom_network_directed` -- off, the router ignored one-ways and never saw them;
    on, `_parse_custom_network` trimmed them silently. Two configurations, two different maps, one
    manifest schema describing both. InTAS: 39 of 3,328 junctions, 1.2% of the graph."""
    _n, _e, info, _tf = st.engine_network(scenario["net"])
    assert info["strongly_connected"] is True
    assert info["kept_nodes"] == info["strong_component_nodes"]
    assert info["strong_trimmed_nodes"] >= 0
    # the roads those junctions carried are still MAP: a vehicle SUMO drove down a road the router
    # declined to use is on a road, and calling it mapOffRoad is the false positive being removed
    assert len(info["road_surface"]["junctions"]) >= info["kept_nodes"]
