"""Out-of-process detector execution -- the boundary an in-process plugin cannot be given.

**What this file pins, and why each one is here rather than in a document.**

The seam's own trust model (`docs/realism/DETECTOR-PLUGIN.md` section 2) says plainly that an
in-process Python plugin cannot be sandboxed: one line, `sys._getframe(1).f_locals["b"]`, reaches the
reception loop's broadcast dict, which carries `veh` (a `Vehicle` with `.is_attacker` /
`.attack_type`), the sender's TRUE `x`/`y`, `falsified` and `ghost`. Reading the oracle is perfectly
deterministic, so every reproducibility layer in this project -- the pinned goldens, the two-run
equality gate, the content-hash lock -- reproduces exactly while the labels leak into the features.
This repository exists to produce misbehaviour-detection datasets and let researchers compare
detectors, so that is not a theoretical concern: **a submitted detector that can read the labels it
is predicting makes any benchmark built on it worthless.**

So the assertions below are, in order:

1. **The same detector, the same seed, in process and isolated, produce the SAME data_digest.**
   Without this the mode is useless, because two detectors graded in different modes would not be
   comparable. It is the gate everything else hangs off.
2. **What crosses the boundary is exactly the `Observation`.** Asserted against `Observation`'s own
   declared field list rather than a transcription, and against the serialised BYTES, so an oracle
   value cannot arrive under some other name.
3. **The frame walk finds only the serialiser.** The identical hostile check is run BOTH ways; in
   process it reaches `run_pipeline`'s locals and files a report on every message from the label,
   isolated it reaches seven frames of `api/isolate.py` and files nothing.
4. **Failure is loud.** A crashing, hanging or lying worker fails the run. There is no path on which
   an isolated detector silently scores 0.0, because a dataset in which "the detector was broken"
   and "the detector saw nothing" are the same bytes is worse than no dataset.
5. **The residue is pinned too.** The child is an ordinary OS process: it reads the filesystem, this
   repository included. That is asserted here so the documentation cannot quietly stop saying it.
"""

import importlib
import json
import os
import sys

import pytest

from scms_sim_ref.api import isolate as ISO
from scms_sim_ref.api import registry as apireg
from scms_sim_ref.api.detect import OBSERVATION_FIELD_ORDER
from scms_sim_ref.api.errors import ConfigError
from scms_sim_ref.conformance.v1.detect import observation
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import run as RM

_CFG = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
            grid_w=5, grid_h=5, attacker_pct=0.25)

#: Names the engine's broadcast dict carries and the `Observation` does not. None of them may appear
#: in a frame the child sends, under any spelling.
ORACLE_NAMES = ("falsified", "ghost", "is_attacker", "attack_type", "veh", "victims", "tspd",
                "thdg")


# --------------------------------------------------------------------------- #
# A third-party detector distribution, written outside `src/`, reached by dotted path.
# --------------------------------------------------------------------------- #
_PLUGIN_SRC = '''\
"""Third-party detectors for the isolation tests. Imports only the published api."""
import json
import math
import os
import sys
import time

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase
from scms_sim_ref.api.fields import FieldSpec


class ClaimedRangeThreshold(CheckBase):
    """An HONEST threshold detector, and a STATEFUL one -- the state has to survive the crossing.

    It also draws from the namespaced rng, so the two modes must agree on the stream as well as on
    the arithmetic: `RngNamespace(seed, plugin_id)` is a pure function of its inputs, and the child
    builds its own from the same two.
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "isodet"
    reason_code = "claimedRange"
    precision = 3

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.max_range_m = float(self.params["max_range_m"])
        self.tolerance_m = float(self.params["tolerance_m"])

    @classmethod
    def config_fields(cls):
        return {"max_range_m": FieldSpec("float", 250.0, "claimed distance treated as plausible",
                                         lo=10.0, hi=5000.0, unit="m"),
                "tolerance_m": FieldSpec("float", 100.0, "excess distance that scores 1.0",
                                         lo=1.0, hi=1000.0, unit="m")}

    def capabilities(self):
        return frozenset({"history", "stateful"})

    def evaluate(self, obs, state, params, rng):
        d = math.hypot(obs.claimed_x - obs.rx_x, obs.claimed_y - obs.rx_y)
        state["seen"] = state.get("seen", 0) + 1
        state["last"] = [obs.claimed_x, obs.claimed_y]
        jitter = rng.stream("jitter", obs.cert_digest).random() * 1e-9
        hist = state.get("h_len", 0)
        if "h" in state:
            hist = len(state["h"])
        state["h_len"] = hist
        return max(0.0, d - self.max_range_m) / self.tolerance_m + jitter


class Exploder(CheckBase):
    """Raises on the 5th message. A crash must fail the run, never score 0.0."""

    interface_version = INTERFACE_VERSION
    plugin_id = "isoboom"
    reason_code = "boom"

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.n = 0

    @classmethod
    def config_fields(cls):
        return {}

    def capabilities(self):
        return frozenset()

    def evaluate(self, obs, state, params, rng):
        self.n += 1
        if self.n >= 5:
            raise RuntimeError("detector exploded on purpose")
        return 0.0


class Sleeper(CheckBase):
    """Never returns. A hung detector must fail the run, never be waited out."""

    interface_version = INTERFACE_VERSION
    plugin_id = "isosleep"
    reason_code = "sleep"

    @classmethod
    def config_fields(cls):
        return {}

    def capabilities(self):
        return frozenset()

    def evaluate(self, obs, state, params, rng):
        time.sleep(3600.0)
        return 0.0


class Hoarder(CheckBase):
    """Puts an object that cannot cross a process boundary into its own state."""

    interface_version = INTERFACE_VERSION
    plugin_id = "isohoard"
    reason_code = "hoard"

    @classmethod
    def config_fields(cls):
        return {}

    def capabilities(self):
        return frozenset({"stateful"})

    def evaluate(self, obs, state, params, rng):
        state["thing"] = object()
        return 0.0


class Printer(CheckBase):
    """Writes to stdout on every message. That is the child's PROTOCOL channel."""

    interface_version = INTERFACE_VERSION
    plugin_id = "isoprint"
    reason_code = "printer"

    @classmethod
    def config_fields(cls):
        return {}

    def capabilities(self):
        return frozenset()

    def evaluate(self, obs, state, params, rng):
        print("chatter from a third-party detector", flush=True)
        sys.stdout.write("more chatter\\n")
        return 0.0
'''

#: The hostile detector lives in its OWN module, because the source gate parses a whole module and
#: would otherwise refuse the honest checks for sharing a file with it. That separation is also how
#: a real submission arrives.
_HOSTILE_SRC = '''\
"""A detector that reads the labels it is supposed to be predicting -- if it can reach them.

Writes what its frame walk found to `$ISO_TEST_REPORT`, so the test can COMPARE the two modes
instead of inferring the difference from a failure.
"""
import json
import os
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase


class FrameWalker(CheckBase):

    interface_version = INTERFACE_VERSION
    plugin_id = "isohostile"
    reason_code = "oracleRead"
    precision = 3

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.written = False
        self.calls = 0

    @classmethod
    def config_fields(cls):
        return {}

    def capabilities(self):
        return frozenset()

    def evaluate(self, obs, state, params, rng):
        frames, b, engine_rng = [], None, None
        f = sys._getframe(1)
        while f is not None and len(frames) < 40:
            loc = f.f_locals
            frames.append({"module": f.f_globals.get("__name__"),
                           "function": f.f_code.co_name,
                           "locals": sorted(str(k) for k in loc)})
            if b is None and isinstance(loc.get("b"), dict) and "veh" in loc["b"]:
                b = loc["b"]
            if engine_rng is None and type(loc.get("rng")).__name__ in ("Random",
                                                                       "WitnessedRandom"):
                engine_rng = type(loc["rng"]).__name__
            f = f.f_back
        self.calls = getattr(self, "calls", 0) + 1
        # Probed at message 1 AND at 20 000. The run's own streamed files are empty at message 1,
        # and "there was nothing to read yet" is a different statement from "it could not be read"
        # -- which is exactly the mistake the guide's first draft made.
        if self.calls in (1, 20000):
            self.written = True
            path = os.environ.get("ISO_TEST_REPORT")
            if path:
                prior = os.environ.get("ISO_TEST_PRIOR_GT", "")
                fs = {"probe_at_call": self.calls}
                if prior:
                    try:
                        with open(prior, encoding="utf-8") as fh:
                            fs["prior_gt_first_row"] = fh.readline().strip()[:200]
                    except OSError as e:
                        fs["prior_gt_first_row"] = "REFUSED: " + type(e).__name__
                out_dir = os.environ.get("ISO_TEST_OUT_DIR", "")
                if out_dir:
                    lp = os.path.join(out_dir, "ground_truth", "gt_report_labels.jsonl")
                    try:
                        with open(lp, "rb") as fh:
                            head = fh.read(400)
                        fs["run_labels"] = {"read": True, "size": os.path.getsize(lp),
                                            "head": head.decode("utf-8", "replace")}
                    except OSError as e:
                        fs["run_labels"] = {"read": False, "error": type(e).__name__}
                with open(path, "w", encoding="utf-8") as fh:
                    json.dump({"frames": frames, "oracle": b is not None,
                               "engine_rng": engine_rng, "filesystem": fs, "pid": os.getpid(),
                               "engine_modules": sorted(
                                   m for m in sys.modules if m.startswith("scms_sim_ref"))},
                              fh, indent=1, sort_keys=True)
        if b is not None:
            return 3.0 if b.get("veh") is not None and b["veh"].is_attacker else 0.0
        return 0.0
'''


@pytest.fixture(scope="module")
def iso_plugin(tmp_path_factory):
    """The plugin distribution on `sys.path` -- which is also what the CHILD inherits.

    `api.isolate.child_env()` carries this process's `sys.path` over as `PYTHONPATH`, so a plugin
    the engine could import is a plugin the worker can import, and a fixture that inserts a temp
    directory needs no second mechanism.
    """
    root = tmp_path_factory.mktemp("iso_det")
    (root / "iso_det.py").write_text(_PLUGIN_SRC, encoding="utf-8")
    (root / "iso_hostile.py").write_text(_HOSTILE_SRC, encoding="utf-8")
    sys.path.insert(0, str(root))
    importlib.invalidate_caches()
    try:
        yield root
    finally:
        sys.path.remove(str(root))
        sys.modules.pop("iso_det", None)
        sys.modules.pop("iso_hostile", None)


REF = "iso_det:ClaimedRangeThreshold"
HOSTILE = "iso_hostile:FrameWalker"
PARAMS = {"max_range_m": 120.0, "tolerance_m": 40.0}


def _cfg(out, entries, **kw):
    return PipelineConfig(out_dir=str(out), plugins={"check": entries}, **dict(_CFG, **kw))


def _entry(ref, *, isolated=False, params=None, gate=None, timeout=None):
    e = {"ref": ref}
    if params is not None:
        e["params"] = params
    if isolated:
        e["isolated"] = True
    if gate is not None:
        e["source_gate"] = gate
    return e


def _reports(out):
    with open(os.path.join(str(out), "ma", "ma_reports.jsonl"), encoding="utf-8") as fh:
        return [json.loads(ln) for ln in fh if ln.strip()]


# ================================================================= 1. THE GATE ================= #
def test_isolated_and_in_process_agree_bit_for_bit(iso_plugin, tmp_path):
    """**The gate.** Same detector, same seed, same params -- one in the engine's interpreter, one in
    its own -- must produce the identical dataset.

    Everything else in this mode is worthless without it: a detector graded in isolation would not be
    comparable with one graded in process, and a benchmark cannot mix the two. The three things that
    make it hold are all deliberate: `json` renders a float through `float.__repr__` (the shortest
    string that round-trips), the child builds `RngNamespace(seed, plugin_id)` from the same two
    inputs the engine would have used, and the plugin's own state crosses back on every message so
    the engine keeps owning its lifetime.
    """
    a = run_pipeline(_cfg(tmp_path / "inproc", [_entry(REF, params=PARAMS)]))
    b = run_pipeline(_cfg(tmp_path / "isolated", [_entry(REF, params=PARAMS, isolated=True)]))
    assert a.data_digest == b.data_digest
    assert (a.n_reports, a.n_vehicles, a.n_revoked) == (b.n_reports, b.n_vehicles, b.n_revoked)
    assert a.n_reports > 0, "a detector that never fires would make this test vacuous"
    # ... including the per-message scores themselves, not merely the aggregate digest.
    col = "detnorm_x_isodet_claimedRange"
    ra, rb = _reports(tmp_path / "inproc"), _reports(tmp_path / "isolated")
    assert [r[col] for r in ra] == [r[col] for r in rb]
    assert any(r[col] > 0 for r in ra)


def test_isolated_alongside_the_builtins_is_also_identical(iso_plugin, tmp_path):
    """The realistic benchmark shape: the engine's own suite plus one submitted detector."""
    vec = ["@builtins", _entry(REF, params=PARAMS)]
    iso = ["@builtins", _entry(REF, params=PARAMS, isolated=True)]
    a = run_pipeline(_cfg(tmp_path / "bi", vec))
    b = run_pipeline(_cfg(tmp_path / "bi_iso", iso))
    assert a.data_digest == b.data_digest


def test_the_lock_records_that_the_plugin_ran_out_of_process(iso_plugin, tmp_path):
    """`isolated: true` is in the LOCK as well as in the config, and it changes how the entry is
    verified: a replay must not re-resolve it by importing it into the verifying process."""
    res = run_pipeline(_cfg(tmp_path / "lock", [_entry(REF, params=PARAMS, isolated=True)]))
    man = json.load(open(os.path.join(res.out_dir, "manifest.json"), encoding="utf-8"))
    entry = [e for e in man["plugins"]["loaded"] if e["ref"] == REF]
    assert len(entry) == 1 and entry[0]["isolated"] is True
    # The hashes were computed BY THE PARENT, by reading the file the worker named.
    assert entry[0]["module_sha256"] == apireg._file_sha256_cached(str(iso_plugin / "iso_det.py"))
    assert entry[0]["params"]["max_range_m"] == 120.0
    # No built-in entry grew the field, so every manifest written before it is byte-identical.
    assert all("isolated" not in e for e in man["plugins"]["loaded"] if e["resolved_via"] ==
               "builtin")
    # And the drift check passes without importing the plugin (it re-probes in a child).
    assert apireg.verify_lock(man["plugins"]) == []


def test_drift_in_an_isolated_plugin_is_still_caught(iso_plugin, tmp_path):
    """The content-hash lock does not weaken in isolated mode -- it just runs out of process too.

    `verify_lock` re-resolves every non-built-in entry, and re-resolving means IMPORTING. For an
    isolated entry that would run the plugin's module-level code in the VERIFYING process, which is
    exactly what the mode exists to avoid and is most likely to matter on a replay of somebody
    else's manifest. So an `isolated` entry is re-probed in a child, and the hashes are computed here
    from the files the child names.
    """
    res = run_pipeline(_cfg(tmp_path / "drift", [_entry(REF, params=PARAMS, isolated=True)]))
    man = json.load(open(os.path.join(res.out_dir, "manifest.json"), encoding="utf-8"))
    src = iso_plugin / "iso_det.py"
    original = src.read_text(encoding="utf-8")
    try:
        # ONE byte, inside a docstring: behaviour provably unchanged, identity provably moved.
        src.write_text(original.replace("An HONEST threshold detector", "An honest threshold "
                                        "detector"), encoding="utf-8")
        apireg._MODULE_HASH_CACHE.clear()
        with pytest.raises(Exception) as e:
            apireg.verify_lock(man["plugins"])
        assert "module_sha256" in str(e.value)
    finally:
        src.write_text(original, encoding="utf-8")
        apireg._MODULE_HASH_CACHE.clear()
        importlib.invalidate_caches()


# ============================================== 2. WHAT CROSSES THE BOUNDARY =================== #
def test_the_payload_is_the_observation_and_nothing_else():
    """Asserted against `Observation`'s OWN declared field list, so a field added to the DTO is
    covered automatically and a field that is not on the DTO cannot be smuggled across."""
    obs = observation()
    payload = ISO._eval_payload(obs, {"mine": 1}, {"h": [[1.0, 2.0]], "touch": 3}, step=7, seq=11)
    assert set(payload) == {"t", "seq", "step", "obs", "own", "res"}
    assert len(payload["obs"]) == len(OBSERVATION_FIELD_ORDER)
    assert payload["obs"] == [dict(zip(OBSERVATION_FIELD_ORDER, payload["obs"]))[n]
                              for n in OBSERVATION_FIELD_ORDER]
    # The BYTES, because a name-based check is only as good as the names it knows. Quoted, so
    # `"veh"` is not spuriously matched inside the legitimate station_type value `"vehicle"`.
    wire = ISO._dump(payload).decode("utf-8")
    for name in ORACLE_NAMES:
        assert f'"{name}"' not in wire, f"{name!r} reached the wire"
    assert "PipelineConfig" not in wire and "Vehicle" not in wire
    # The observation crosses POSITIONALLY, so the only keys on the wire are the protocol's own.
    assert sorted(json.loads(wire)) == ["obs", "own", "res", "seq", "step", "t"]


def test_a_frozen_observation_survives_the_crossing_unchanged(iso_plugin):
    """Round-trip the DTO through the wire and back: same values, still sealed, still a proxy."""
    obs = observation(offset_m=0.1 + 0.2, rssi_dbm=-71.25, conf=1.0 / 3.0, speed=12.345678901234567)
    payload = ISO._eval_payload(obs, {}, {}, step=0, seq=1)
    back = json.loads(ISO._dump(payload))["obs"]
    rebuilt = dict(zip(OBSERVATION_FIELD_ORDER, back))
    for name in OBSERVATION_FIELD_ORDER:
        want = getattr(obs, name)
        got = rebuilt[name]
        if name == "neighbourhood":
            assert dict(want) == dict(got)
            continue
        # `repr` rather than `==`: the claim is BIT-exact float round-tripping, and `==` would pass
        # for two doubles that merely printed the same.
        assert repr(want) == repr(got), name


# ==================================================== 3. THE HOSTILE DETECTOR ================== #
def test_a_frame_walk_reaches_the_engine_in_process(iso_plugin, tmp_path, monkeypatch):
    """The measurement the whole mode exists for, half one: **in process the walk reaches the
    oracle.** The source gate refuses this class by name, so the config says `source_gate: off` --
    which is what a reviewer who waved a submission through would have written."""
    report = tmp_path / "inproc_findings.json"
    monkeypatch.setenv("ISO_TEST_REPORT", str(report))
    res = run_pipeline(_cfg(tmp_path / "hostile_in",
                            [_entry(HOSTILE, gate="off")], duration_s=20))
    found = json.loads(report.read_text(encoding="utf-8"))
    assert found["oracle"] is True
    assert found["engine_rng"] in ("Random", "WitnessedRandom")
    assert any(f["module"] == "scms_sim_ref.mock_pipeline.run" and f["function"] == "run_pipeline"
               for f in found["frames"])
    engine_frame = next(f for f in found["frames"]
                        if f["module"] == "scms_sim_ref.mock_pipeline.run")
    for name in ("b", "cfg", "rng", "vehicles", "gt_vehicle", "last_claimed"):
        assert name in engine_frame["locals"]
    # It scored the label, and the engine believed it: every report row it produced came from the
    # oracle, and the run is perfectly reproducible while doing it.
    rows = _reports(res.out_dir)
    assert rows and all(r["detnorm_x_isohostile_oracleRead"] >= 1.0 for r in rows)


def test_a_frame_walk_in_the_child_reaches_only_the_serialiser(iso_plugin, tmp_path, monkeypatch):
    """Half two: **isolated, the identical class finds nothing.**

    The walk terminates in `api/isolate.py`'s own frames. There is no engine frame to reach, because
    there is no engine in the process -- `scms_sim_ref.mock_pipeline` is not even imported there.
    """
    report = tmp_path / "iso_findings.json"
    monkeypatch.setenv("ISO_TEST_REPORT", str(report))
    res = run_pipeline(_cfg(tmp_path / "hostile_iso",
                            [_entry(HOSTILE, isolated=True)], duration_s=20))
    found = json.loads(report.read_text(encoding="utf-8"))
    assert found["oracle"] is False
    assert found["engine_rng"] is None
    modules = {f["module"] for f in found["frames"]}
    assert not any(str(m).startswith("scms_sim_ref.mock_pipeline") for m in modules)
    assert "scms_sim_ref.mock_pipeline.run" not in found["engine_modules"]
    # The walk starts at the plugin's CALLER, and the caller is the serialiser: its locals are the
    # payload it just decoded and nothing else. Above it: `serve`, `main`, the module, `runpy`.
    assert found["frames"][0]["locals"] == ["_types", "fields", "msg", "obs", "self", "state"]
    assert [f["function"] for f in found["frames"]][:3] == ["evaluate", "serve", "main"]
    # It is a DIFFERENT PROCESS, stated as an assertion rather than as a design intention.
    assert found["pid"] != os.getpid()
    # And it filed nothing: with no labels to read, the label-reading detector has no signal.
    rows = _reports(res.out_dir)
    assert not any(r.get("detnorm_x_isohostile_oracleRead", 0.0) >= 1.0 for r in rows)


def test_the_child_still_reads_the_filesystem_and_the_docs_say_so(iso_plugin, tmp_path,
                                                                  monkeypatch):
    """**The residual hole, pinned so it cannot quietly stop being documented.**

    Process isolation takes the run's ground truth out of the detector's ADDRESS SPACE. It does not
    take it off the DISK: the child runs as the same user, so a completed earlier run's
    `ground_truth/*.jsonl` in a directory it can guess is readable. The honest mitigations are
    operational (run the benchmark against datasets the submitter's process cannot reach, or under
    an OS-level sandbox), and the honest thing to do here is assert the hole rather than imply it is
    closed.
    """
    prior = tmp_path / "prior_gt.jsonl"
    prior.write_text('{"_visibility":"ORACLE","is_attacker":true}\n', encoding="utf-8")
    report = tmp_path / "fs_findings.json"
    monkeypatch.setenv("ISO_TEST_REPORT", str(report))
    monkeypatch.setenv("ISO_TEST_PRIOR_GT", str(prior))
    run_pipeline(_cfg(tmp_path / "fs", [_entry(HOSTILE, isolated=True)], duration_s=20))
    found = json.loads(report.read_text(encoding="utf-8"))
    assert found["filesystem"]["prior_gt_first_row"].startswith('{"_visibility":"ORACLE"')
    doc = os.path.join(os.path.dirname(__file__), os.pardir, "docs", "realism",
                       "DETECTOR-PLUGIN.md")
    text = open(doc, encoding="utf-8").read().lower()
    assert "filesystem" in text, "the guide must name the residual hole this test measures"
    assert "filesystem" in ISO.__doc__


def test_the_child_can_read_this_runs_streamed_oracle_labels(iso_plugin, tmp_path, monkeypatch):
    """**The worse half of the same hole, and the reason this test exists at all.**

    The first draft of the guide's §2.8 said the run's OWN oracle files were unreadable while the
    loop was going, so a detector could not see the labels of the run it was being graded on. That is
    false, and measuring it is what showed it: `ground_truth/gt_report_labels.jsonl` is STREAMED, so
    by message 20 000 it already holds the oracle verdict — plus `reporter_true_id` and
    `subject_true_id` — for every report filed so far.

    The detector has to be declared alongside `@builtins` for that to be visible, because a detector
    that fires nothing produces nothing to stream, and the first measurement was quietly reading an
    empty file and calling it a closed hole.

    What survives is the narrower claim: the ground truth is not in the child's ADDRESS SPACE
    (`oracle["oracle"] is False` here too). What does not survive is any claim about the disk.
    """
    report = tmp_path / "stream_findings.json"
    monkeypatch.setenv("ISO_TEST_REPORT", str(report))
    out = tmp_path / "stream"
    monkeypatch.setenv("ISO_TEST_OUT_DIR", str(out))
    run_pipeline(_cfg(out, ["@builtins", _entry(HOSTILE, isolated=True)], duration_s=40))
    found = json.loads(report.read_text(encoding="utf-8"))
    assert found["oracle"] is False              # the address-space claim still holds
    labels = found["filesystem"].get("run_labels", {})
    assert labels.get("read") is True and labels.get("size", 0) > 0, (
        "the streamed oracle label file was expected to be readable AND non-empty from the child; "
        "if this now fails because the engine stopped streaming it, the guide's §2.8 must be "
        "rewritten rather than this assertion relaxed")
    assert '"_visibility":"ORACLE"' in labels["head"]
    assert "report_correctness" in labels["head"] and "subject_true_id" in labels["head"]


# ============================================================== 4. LOUD FAILURE ================ #
def test_a_crashing_detector_fails_the_run(iso_plugin, tmp_path):
    """A detector that raises out of process must fail the run with the plugin's OWN traceback --
    never be swallowed into a 0.0 that reads as "this message looked fine"."""
    with pytest.raises(ConfigError) as e:
        run_pipeline(_cfg(tmp_path / "boom", [_entry("iso_det:Exploder", isolated=True)]))
    msg = str(e.value)
    assert "detector exploded on purpose" in msg
    assert "iso_det:Exploder" in msg and "RuntimeError" in msg


def test_a_hanging_detector_fails_the_run_rather_than_being_waited_out(iso_plugin, tmp_path,
                                                                      monkeypatch):
    """The timeout is a FAIL-STOP, and this test is what makes that statement checkable.

    It is the only clock in the mode. A slow child can turn a run into a failure; it can never turn
    it into a different run, because there is no branch on which the parent proceeds without the
    score it asked for.
    """
    monkeypatch.setattr(ISO, "DEFAULT_TIMEOUT_S", 3.0)
    with pytest.raises(ConfigError) as e:
        run_pipeline(_cfg(tmp_path / "hang", [_entry("iso_det:Sleeper", isolated=True)]))
    msg = str(e.value)
    assert "did not answer within" in msg and "FAILS THE RUN" in msg


def test_state_that_cannot_cross_is_refused_by_name(iso_plugin, tmp_path):
    """An isolated check's state is JSON. A value that will not serialise is named at the first
    message rather than dropped -- a silently dropped key makes the two modes disagree on message 2,
    which is the worst possible way to learn about this constraint."""
    with pytest.raises(ConfigError) as e:
        run_pipeline(_cfg(tmp_path / "hoard", [_entry("iso_det:Hoarder", isolated=True)]))
    assert "not JSON-serialisable" in str(e.value) and "'thing'" in str(e.value)


def test_a_chatty_detector_cannot_corrupt_the_protocol(iso_plugin, tmp_path):
    """`print()` in a submitted detector is not an attack; it is an accident that would otherwise
    desynchronise the frame stream and look like an engine bug. The worker takes fd 1 away from the
    plugin before it runs, so the chatter goes to stderr and the run completes."""
    res = run_pipeline(_cfg(tmp_path / "chatty", [_entry("iso_det:Printer", isolated=True)],
                            duration_s=20))
    assert res.n_vehicles > 0


def test_a_desynchronised_worker_is_refused(iso_plugin):
    """Strict lockstep, asserted directly on the proxy: a reply carrying the wrong sequence number
    is refused rather than accepted one message late, which would shift the whole dataset."""
    w = ISO.IsolatedCheck(REF, {}, seed=17, env={})
    w.plugin_id, w._ns = "isodet", "plugin:isodet"
    w._closed = False

    class _Pipe:
        closed = True

        def write(self, _b):
            pass

        def flush(self):
            pass

    class _FakeProc:
        stdin = stdout = stderr = None

        def poll(self):
            return 0

        def wait(self, timeout=None):
            return 0

        def kill(self):
            pass

    w._proc = _FakeProc()
    w._in = _Pipe()
    body = ISO._dump({"t": "SCORE", "seq": 999, "score": 0.0, "own": {}})
    w._out = __import__("io").BytesIO(len(body).to_bytes(4, "big") + body)
    with pytest.raises(ConfigError) as e:
        w.evaluate(observation(), {}, {}, ISO.IsolatedRng(17, "isodet"))
    assert "out of sequence" in str(e.value) and "lockstep" in str(e.value)


def test_an_unresolvable_ref_fails_before_step_0(iso_plugin, tmp_path):
    """The worker starts, fails to resolve, and reports it. No output directory, no partial run."""
    out = tmp_path / "missing"
    with pytest.raises(ConfigError) as e:
        run_pipeline(_cfg(out, [_entry("nosuchpkg.nomodule:Nope", isolated=True)]))
    assert "nosuchpkg" in str(e.value)
    assert not out.exists()


# ====================================================== 5. THE MODE'S OWN RULES ================ #
def test_a_builtin_may_not_be_isolated(iso_plugin, tmp_path):
    """A built-in IS the engine: isolating it would buy nothing and cost a round trip per message."""
    with pytest.raises(ConfigError) as e:
        run_pipeline(_cfg(tmp_path / "bi", [_entry("positionJump", isolated=True)]))
    assert "BUILT-IN" in str(e.value) and "cannot be isolated" in str(e.value)
    # Refused in the ENGINE, not by the worker: the child imports `scms_sim_ref.api` and nothing
    # else, so it has never heard of the built-in registry and would only say "unknown check".
    with pytest.raises(ConfigError):
        RM._checks_selection(PipelineConfig(plugins={"check": [{"ref": "positionJump",
                                                                "isolated": True}]}),
                             station_types=False, denm=False)


def test_isolated_is_a_replayable_config_key(iso_plugin, tmp_path):
    """It lands verbatim in `manifest["config"]["plugins"]`, so a dataset produced by an isolated
    detector says so in its own manifest and the decision replays."""
    res = run_pipeline(_cfg(tmp_path / "cfg", [_entry(REF, params=PARAMS, isolated=True)]))
    man = json.load(open(os.path.join(res.out_dir, "manifest.json"), encoding="utf-8"))
    assert man["config"]["plugins"]["check"][0]["isolated"] is True
    cfg2 = RM.config_from_dict(man["config"])
    assert RM._checks_selection(cfg2, station_types=False, denm=False)[0][4] is True


def test_isolated_defaults_the_source_gate_off_and_says_why(iso_plugin):
    """The gate refuses `sys._getframe` because in process it reaches the engine's frame. Out of
    process it does not, so refusing a submission for containing the name would be theatre -- and
    turning a real submission away for a construct that is now harmless is how a mode gets
    disabled."""
    cfg = PipelineConfig(plugins={"check": [{"ref": HOSTILE, "isolated": True}]})
    sel = RM._checks_selection(cfg, station_types=False, denm=False)
    assert sel[0][3] == "off" and sel[0][4] is True
    # ... and explicitly asking for it still screens the file, WITHOUT importing it.
    cfg_on = PipelineConfig(plugins={"check": [{"ref": HOSTILE, "isolated": True,
                                                "source_gate": "on"}]})
    assert RM._checks_selection(cfg_on, station_types=False, denm=False)[0][3] == "on"


def test_isolated_entries_are_not_resolved_at_config_time(iso_plugin, monkeypatch):
    """Config-time validation resolves every declared ref, and resolving means IMPORTING -- which
    module-level code makes an earlier hook than `__init__`. An isolated entry must be skipped
    there and validated against the worker's reported `FieldSpec`s instead."""
    calls = []
    real = apireg.resolve

    def spy(slot, ref):
        calls.append((slot, ref))
        return real(slot, ref)

    monkeypatch.setattr(apireg, "resolve", spy)
    cfg = PipelineConfig(plugins={"check": [_entry(REF, params=PARAMS, isolated=True)]})
    RM.validate_config(cfg)
    assert not any(r == REF for _s, r in calls)


def test_a_bad_param_is_still_refused_before_step_0(iso_plugin, tmp_path):
    """One phase later than the in-process path, but still before step 0 and before an output
    directory exists -- and with the plugin author's OWN bounds and message, taken off the wire."""
    out = tmp_path / "badparam"
    with pytest.raises(ConfigError) as e:
        run_pipeline(_cfg(out, [_entry(REF, params={"max_range_m": 1.0}, isolated=True)]))
    assert "below minimum 10.0" in str(e.value)
    assert not out.exists()
    with pytest.raises(ConfigError) as e:
        run_pipeline(_cfg(out, [_entry(REF, params={"nope": 1.0}, isolated=True)]))
    assert "declares no field 'nope'" in str(e.value)


def test_the_module_states_what_it_does_not_close():
    """The mode is honest or it is worse than nothing: a boundary that is oversold gets trusted for
    things it does not do. Pinned here so a future edit cannot quietly delete the caveat."""
    doc = ISO.__doc__
    assert "cannot be sandboxed" in doc
    assert "filesystem" in doc
    assert "pickle" in doc and "code" in doc
    assert "address space" in doc and "ordinary OS process" in doc
