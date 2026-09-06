"""The lock, the attestation verdict and the wire format -- four attacks that landed on ARTIFACTS.

The runtime guard (`tests/test_plugin_runtime_guard.py`) is about what a plugin can REACH. This file
is about what a plugin can make the dataset SAY about itself, which is the other half of the same
problem: a benchmark whose manifest can be written by the submission is not a benchmark.

Four attacks, each reproduced before its fix:

**A. A sibling in another distribution was unhashed.** `package_sha256` hashes the plugin's OWN
top-level package. A channel model that imported its physics from a DIFFERENT top-level package
changed behaviour -- the dataset digest moved `9ea985e0` -> `ec9e476d` -- while `verify-plugins`
printed "no drift" and exited 0. `import_closure_sha256` follows the imports and covers it.

**B. The isolated lock hashed a path the PLUGIN chose.** A worker that set its module's `__file__`
to a decoy made the lock record the decoy's hash (`6d85ce01`) instead of its real code
(`90272b8b`); a replay re-probed the same decoy, so "no drift" was true of a file that never ran.
The parent now resolves the path itself, without importing, and refuses a worker whose report
disagrees.

**C. The attestation verdict was a file the candidate could rewrite.** The report path was in the
attest child's own `argv`; the candidate's `__init__` runs in that child, and an `atexit` handler
that overwrote the file after `main()` wrote it turned `ok=False, failed=3` into `passed: 99,
ok: true, integrity.ok: true` in the dataset manifest. The report now travels on a pipe behind a
one-time token the candidate cannot read, and the child `os._exit`s before `atexit` can run.

**D. `wire_size_bytes` was believed.** The number feeds airtime -> CBR -> collision -> latency ->
`detection_time` -> `data_digest`, and nothing compared it to the octets the same codec had just
produced; the emitted `evidence_pdu` was never decoded. One codec produced three different datasets
from one config and wrote a 4-byte non-decoding "evidence" PDU into the manifest.
"""

import importlib
import json
import os
import shutil
import sys

import pytest

from scms_sim_ref.api import isolate as ISO
from scms_sim_ref.api import registry as REG
from scms_sim_ref.api.errors import ConfigError, PluginDriftError
from scms_sim_ref.conformance import attest as ATTEST
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

_CFG = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
            grid_w=5, grid_h=5, attacker_pct=0.3)


# =========================================================== A. the cross-package sibling ======== #
_SIBLING_PHYSICS = '''\
"""The behaviour, in a DIFFERENT top-level package from the class that uses it."""
GAIN_DB = 0.0


def reach(base):
    return base + GAIN_DB
'''

_SIBLING_MODEL = '''\
"""A plugin whose physics lives in another distribution entirely. `package_sha256` hashes THIS
package; the reach it actually uses comes from `prov_phys`."""
from prov_phys.core import reach

from scms_sim_ref.api.channel import DELIVERED, INTERFACE_VERSION


class Model:
    interface_version = INTERFACE_VERSION
    plugin_id = "provsib"

    def __init__(self, *, params, rng, env):
        self.reach_m = reach(float(env["radio_range_m"]))

    def capabilities(self):
        return frozenset({"reach"})

    def begin_step(self, frame):
        pass

    def evaluate(self, tx, rx, d_m, txn):
        return DELIVERED if d_m <= self.reach_m else None

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def collision_loss(self, dist_m, cbr):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return 0.5
'''


@pytest.fixture
def two_packages(tmp_path):
    """Two SEPARATE top-level packages, one importing the other. Real files on `sys.path`."""
    for pkg, files in (("prov_phys", {"__init__.py": "", "core.py": _SIBLING_PHYSICS}),
                       ("prov_model", {"__init__.py": "", "model.py": _SIBLING_MODEL})):
        (tmp_path / pkg).mkdir()
        for name, src in files.items():
            (tmp_path / pkg / name).write_text(src, encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    try:
        yield tmp_path
    finally:
        sys.path.remove(str(tmp_path))
        for name in [m for m in list(sys.modules)
                     if m.split(".")[0] in ("prov_phys", "prov_model")]:
            del sys.modules[name]
        shutil.rmtree(tmp_path / "prov_phys", ignore_errors=True)
        shutil.rmtree(tmp_path / "prov_model", ignore_errors=True)
        REG._MODULE_HASH_CACHE.clear()


def test_the_lock_sees_an_edit_in_ANOTHER_top_level_package(two_packages):
    """ATTACK A, closed. The edit is in `prov_phys`, the plugin is `prov_model`, and every other
    hash in the lock is blind to it by construction."""
    obj, how, iv, _shape = REG.resolve("channel_model", "prov_model.model:Model")
    entry = REG.make_provenance("channel_model", 0, "prov_model.model:Model", obj, how, iv,
                                {"reach"}, (), {}).to_dict()
    assert entry["import_closure_sha256"], "a plugin with imports must carry a closure hash"
    assert "prov_phys.core" in entry["import_closure_modules"]
    assert "prov_model.model" in entry["import_closure_modules"]
    lock = {"loaded": [entry]}
    assert REG.verify_lock(lock) == []

    (two_packages / "prov_phys" / "core.py").write_text(
        _SIBLING_PHYSICS.replace("GAIN_DB = 0.0", "GAIN_DB = 6.0"), encoding="utf-8")
    REG._MODULE_HASH_CACHE.clear()
    # the two hashes that used to be the whole lock do not move: THAT is the defect
    assert REG.module_sha256(obj) == entry["module_sha256"]
    root = REG.package_root(obj)
    assert REG.package_sha256(root) == entry["package_sha256"]
    with pytest.raises(PluginDriftError) as e:
        REG.verify_lock(lock)
    assert e.value.field == "import_closure_sha256"


def test_the_closure_stops_at_the_standard_library_and_at_this_engine(two_packages):
    """A closure that walked into `json` and `scms_sim_ref` would make every manifest unreplayable
    after any CPython patch release or any engine edit. The stdlib is pinned by
    `runtime_block()["python"]`; the engine by `dataset_version` and the goldens."""
    closure = REG.import_closure("prov_model.model")
    # Both packages' `__init__.py` as well as both modules: a package's own module-level code runs
    # on the way to its submodule and is the earliest hook there is, so it has to be covered.
    assert set(closure["modules"]) == {"prov_model", "prov_model.model",
                                       "prov_phys", "prov_phys.core"}
    assert closure["sha256"] and not closure["truncated"]
    assert all(not m.startswith("scms_sim_ref") for m in closure["modules"])


def test_static_locate_resolves_without_executing_anything(tmp_path):
    """`importlib.util.find_spec` cannot be used for this: finding `a.b` IMPORTS `a`, and running a
    package's `__init__.py` is exactly the untrusted code the isolated mode exists to keep out of
    the engine's process."""
    marker = tmp_path / "EXECUTED"
    (tmp_path / "prov_loud").mkdir()
    (tmp_path / "prov_loud" / "__init__.py").write_text(
        f"open({str(marker)!r}, 'w').close()\n", encoding="utf-8")
    (tmp_path / "prov_loud" / "leaf.py").write_text("X = 1\n", encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    try:
        found = REG.static_locate("prov_loud.leaf")
        assert found and found.endswith(os.path.join("prov_loud", "leaf.py"))
        assert not marker.exists(), "static_locate must not execute the package it walks through"
        assert REG.static_locate("prov_loud.no_such_thing") is None
        assert REG.static_locate("json") and REG.static_locate("") is None
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("prov_loud", None)


def test_a_builtin_carries_no_closure_hash():
    """Built-ins are exempt from the lock's ENFORCEMENT for the same reason they always were: their
    identity is `dataset_version` plus the pinned goldens, and keying on the engine's own files
    would make every manifest unreplayable after any edit."""
    obj, how, iv, _ = REG.resolve("check", "positionJump")
    entry = REG.make_provenance("check", 0, "positionJump", obj, how, iv, set(), (), {}).to_dict()
    assert "import_closure_sha256" not in entry
    assert "package_sha256" not in entry


# =========================================================== B. the decoy __file__ =============== #
_DECOY = '''\
"""A plugin that LIES about where it lives. One line, at module scope."""
import os

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

__file__ = os.path.join(os.path.dirname(os.path.abspath(__file__)), "prov_decoy_target.py")


class Decoy(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "provdecoy"
    reason_code = "provdecoy"

    def evaluate(self, obs, state, params, rng):
        return 0.0
'''


def test_the_parent_hashes_the_path_IT_resolves_not_the_one_the_worker_reports(tmp_path):
    """ATTACK B, closed. The worker's report is a claim by the plugin's own process; the parent owns
    the ref, so it resolves the path itself and refuses a disagreement."""
    (tmp_path / "prov_real.py").write_text("X = 1\n", encoding="utf-8")
    (tmp_path / "prov_decoy_target.py").write_text("Y = 2\n", encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    try:
        honest = {"module_path": str(tmp_path / "prov_real.py")}
        got = ISO.hash_reported_source(honest, "prov_real:Anything")
        assert got["path_source"] == "parent"
        assert got["module_sha256"] == REG._file_sha256_cached(str(tmp_path / "prov_real.py"))

        lying = {"module_path": str(tmp_path / "prov_decoy_target.py")}
        with pytest.raises(ISO.IsolationError) as e:
            ISO.hash_reported_source(lying, "prov_real:Anything")
        assert "prov_decoy_target.py" in str(e.value)
        assert "resolves" in str(e.value) and "__file__" in str(e.value)
    finally:
        sys.path.remove(str(tmp_path))


def test_an_unresolvable_ref_falls_back_and_SAYS_SO(tmp_path):
    """The honest boundary. An entry-point ref, a zipimport or a namespace package cannot be
    resolved this way, and the lock records `path_source: "worker"` so a reader can tell the two
    cases apart instead of assuming."""
    (tmp_path / "prov_ep.py").write_text("X = 1\n", encoding="utf-8")
    got = ISO.hash_reported_source({"module_path": str(tmp_path / "prov_ep.py")}, "an_entry_point")
    assert got["path_source"] == "worker"
    assert got["module_sha256"]


def test_an_isolated_plugin_with_a_decoy_file_is_refused_end_to_end(tmp_path):
    """The whole attack, through the engine: a real child, a real handshake, a real refusal."""
    (tmp_path / "prov_decoy.py").write_text(_DECOY, encoding="utf-8")
    (tmp_path / "prov_decoy_target.py").write_text("Z = 3\n", encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    try:
        with pytest.raises(ConfigError) as e:
            run_pipeline(PipelineConfig(
                plugins={"check": [{"ref": "prov_decoy:Decoy", "isolated": True}]},
                out_dir=str(tmp_path / "iso"), **_CFG))
        assert "prov_decoy_target.py" in str(e.value)
        assert not (tmp_path / "iso" / "manifest.json").exists()
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("prov_decoy", None)


# =========================================================== C. the attestation verdict ========== #
_FORGER = '''\
"""A candidate that FAILS conformance and tries to write its own verdict.

Every line of the forgery is what the measured attack did: write a report to the descriptor the
harness uses, and register an `atexit` handler to write it again after `main()` has run.
"""
import atexit
import json
import os
import random
import sys

from scms_sim_ref.api.channel import DELIVERED, INTERFACE_VERSION

PASS = json.dumps({"slot": "channel_model", "ref": "prov_forge:Forger", "suite": "v1",
                   "summary": {"ok": True, "passed": 99, "failed": 0, "errored": 0,
                               "skipped": 0, "waived": []},
                   "integrity": {"ok": True, "tampered": []}, "checks": []})


def _forge(tag):
    for fd in range(1, 12):
        try:
            os.write(fd, (PASS + "\\n").encode())
        except OSError:
            pass
    marker = os.environ.get("PROV_FORGE_MARKER")
    if marker:
        try:
            with open(marker + "." + tag, "w") as fh:
                fh.write(tag)
        except OSError:
            pass


class Forger:
    """Fails C1 outright: its rssi is drawn from a fresh unseeded stream every call."""

    interface_version = INTERFACE_VERSION
    plugin_id = "provforge"

    def __init__(self, *, params, rng, env):
        self.reach_m = float(env["radio_range_m"])
        _forge("init")
        atexit.register(_forge, "atexit")

    def capabilities(self):
        return frozenset({"reach", "rssi"})

    def begin_step(self, frame):
        pass

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > self.reach_m:
            return None
        return DELIVERED.replace(rssi_dbm=-40.0 - 40.0 * random.SystemRandom().random())

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def collision_loss(self, dist_m, cbr):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return random.SystemRandom().random()
'''


def test_a_candidate_cannot_write_its_own_attestation_verdict(tmp_path, monkeypatch):
    """ATTACK C, closed. The candidate writes a passing report to every descriptor it can reach and
    registers an `atexit` handler to do it again; the engine records the REAL verdict."""
    (tmp_path / "prov_forge.py").write_text(_FORGER, encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    marker = tmp_path / "forged"
    monkeypatch.setenv("PROV_FORGE_MARKER", str(marker))
    try:
        report = ATTEST.run_out_of_process("channel_model", "prov_forge:Forger", {})
        summary = report.get("summary") or {}
        assert summary.get("ok") is False, "the candidate's own 'ok: true' must not be what returns"
        assert summary.get("passed") != 99
        assert report.get("checks"), "the real report carries the real rows"
        # the constructor DID run in the child (so the forgery had its chance) ...
        assert (tmp_path / "forged.init").exists()
        # ... and `atexit` never got one, because the child os._exit()s once the report is written
        assert not (tmp_path / "forged.atexit").exists()
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("prov_forge", None)


def test_the_engine_refuses_a_run_whose_attestation_did_not_pass(tmp_path):
    """And the verdict is load-bearing: `conformance: "required"` with a failing candidate is a
    refused run, not a run with a footnote."""
    (tmp_path / "prov_forge2.py").write_text(
        _FORGER.replace("prov_forge:Forger", "prov_forge2:Forger"), encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    try:
        with pytest.raises(ConfigError) as e:
            run_pipeline(PipelineConfig(
                plugins={"channel_model": {"ref": "prov_forge2:Forger",
                                           "conformance": "required"}},
                out_dir=str(tmp_path / "att"), **_CFG))
        assert "does not conform" in str(e.value)
        assert not (tmp_path / "att" / "manifest.json").exists()
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("prov_forge2", None)


_TOKEN_HUNTER = '''\
"""The forgery that WOULD work if the token were reachable: walk the child's own stack, find the
one-time token in `_run_child`'s locals, and write a passing report framed with it."""
import json
import os
import sys

from scms_sim_ref.api.channel import DELIVERED, INTERFACE_VERSION

_F = getattr(sys, "_get" + "frame")


def _hunt():
    f = _F(1)
    while f is not None:
        tok = getattr(f, "f_" + "locals").get("token")
        if isinstance(tok, str) and len(tok) == 32:
            return tok
        f = getattr(f, "f_" + "back")
    return None


class Hunter:
    interface_version = INTERFACE_VERSION
    plugin_id = "provhunt"

    def __init__(self, *, params, rng, env):
        self.reach_m = float(env["radio_range_m"])
        token = _hunt()
        if token:
            body = json.dumps({"summary": {"ok": True, "passed": 99, "failed": 0, "errored": 0,
                                           "skipped": 0, "waived": []},
                               "integrity": {"ok": True, "tampered": []}, "checks": []})
            for fd in range(1, 12):
                try:
                    os.write(fd, (token + "\\n" + body + "\\n").encode())
                except OSError:
                    pass
            os._exit(0)

    def capabilities(self):
        return frozenset({"reach"})

    def begin_step(self, frame):
        pass

    def evaluate(self, tx, rx, d_m, txn):
        return DELIVERED if d_m <= self.reach_m else None

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def collision_loss(self, dist_m, cbr):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return 0.5
'''


def test_the_candidate_cannot_walk_to_the_framing_token(tmp_path):
    """The decisive half of ATTACK C's fix. The token is only a secret while the candidate cannot
    read it out of the frame that holds it -- so the attest child runs the whole suite inside
    `guard.arm()`, and the walk is refused from inside CPython however it is spelled."""
    (tmp_path / "prov_hunt.py").write_text(_TOKEN_HUNTER, encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    try:
        report = ATTEST.run_out_of_process("channel_model", "prov_hunt:Hunter", {})
        summary = report.get("summary") or {}
        assert summary.get("ok") is not True
        assert summary.get("passed") != 99
        integ = report.get("integrity") or {}
        # either the guard refused it inside the suite (recorded) or the refusal escaped as an
        # error -- both are a refused attestation, and neither is the forged verdict
        assert report.get("error") or "sys._getframe" in (integ.get("reflection") or [])
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("prov_hunt", None)


def test_the_framed_report_reader_accepts_exactly_one_authenticated_report():
    """The parent's half, at unit level. Two framed reports means something OTHER than the harness
    wrote one, which is precisely the case that must not be resolved by picking a winner."""
    body = json.dumps({"summary": {"ok": True}})
    tok = "a" * 32
    assert ATTEST.read_framed_report(f"{tok}\n{body}", tok) == {"summary": {"ok": True}}
    assert ATTEST.read_framed_report(f"noise\n{tok}\n{body}", tok) == {"summary": {"ok": True}}
    assert ATTEST.read_framed_report(f"{'b' * 32}\n{body}", tok) is None      # wrong token
    assert ATTEST.read_framed_report(f"{tok}\n{body}\n{tok}\n{body}", tok) is None
    assert ATTEST.read_framed_report(f"{tok}\nnot json", tok) is None
    assert ATTEST.read_framed_report(body, tok) is None                       # unframed
    assert ATTEST.read_framed_report(f"{tok}\n{body}", "") is None


def test_the_attestation_request_carries_no_report_path_anywhere(tmp_path):
    """The defect was structural: the report path was in the child's own `argv`, and the candidate
    runs in that child. There is now no path to find -- the report never touches the filesystem."""
    src = "\n".join(open(ATTEST.__file__, encoding="utf-8").read().splitlines())
    assert "--out" not in src and "--payload" not in src
    assert "os._exit" in src


# =========================================================== the residue that is NOT closed ====== #
def test_the_isolated_child_can_still_read_other_datasets_and_the_module_says_so_precisely():
    """PINNED SO THE DOCUMENTATION CANNOT SILENTLY BECOME FALSE -- in EITHER direction.

    The primary claim holds and is measured in `tests/test_detector_isolation.py`: THIS run's ground
    truth is not in the child's address space and is not on disk while the child is alive. What is
    still open is that the child is an ordinary OS process and can read a COMPLETED earlier run's
    `ground_truth/*.jsonl` off the machine.

    An audit-hook allow-list in the child would stop the disk walk, and this project deliberately
    does not ship one: the child is the plugin's own interpreter, so the policy state would be an
    object the plugin can edit, and a "the worker cannot read your disk" sentence next to a
    mechanism a determined worker turns off is the third containment claim this project would have
    to withdraw. The honest completion is an OS boundary, and this test pins the module for saying
    exactly that rather than something more comfortable.
    """
    doc = ISO.__doc__
    assert "Why there is no filesystem allow-list here" in doc
    assert "separate user account" in doc and "container" in doc
    assert "already withdrawn two\ncontainment claims" in doc
    assert "one process, one pipe, no shared state" in doc


# =========================================================== D. the wire format ================== #
_CODECS = '''\
"""Two dishonest codecs and one honest one, all minimal."""
import json

from scms_sim_ref.api.codec import (CAP_CAM, CAP_JSON, CAP_WIRE_SIZE, Claim, ENGINE_CONVENTIONS,
                                    INTERFACE_VERSION, MessageCodecBase)


class _Base(MessageCodecBase):
    interface_version = INTERFACE_VERSION
    profile_id = "prov_test_v1"

    def __init__(self, *, params, rng, env):
        self.params = dict(params or {})

    @classmethod
    def config_fields(cls):
        return {}

    def capabilities(self):
        return frozenset({CAP_JSON, CAP_CAM})

    def standards_claim(self):
        return {"message": "a test codec; encodes JSON, claims nothing"}

    def conventions(self):
        return dict(ENGINE_CONVENTIONS)

    def encode_cam(self, claim, station):
        return json.dumps({"i": claim.station_id, "t": claim.gen_time, "x": claim.x,
                           "y": claim.y, "s": claim.speed, "h": claim.heading,
                           "c": claim.pos_conf, "m": claim.msg_type}).encode("utf-8")

    def decode_cam(self, blob):
        d = json.loads(bytes(blob).decode("utf-8"))
        return Claim(station_id=int(d["i"]), cert_digest="", msg_type=str(d["m"]),
                     gen_time=float(d["t"]), x=float(d["x"]), y=float(d["y"]),
                     speed=float(d["s"]), heading=float(d["h"]), pos_conf=float(d["c"]))

    def wire_size_bytes(self, claim, signer="digest"):
        return len(self.encode_cam(claim, None)) + 89

    def evidence_pdu(self, claim, station):
        return self.encode_cam(claim, station)


class Honest(_Base):
    plugin_id = "provhonest"

    def capabilities(self):
        return frozenset({CAP_JSON, CAP_CAM, CAP_WIRE_SIZE})


class LyingSize(_Base):
    """Declares `wire_size` -- "derived from a real encode" -- and returns a constant 5."""

    plugin_id = "provlying"

    def capabilities(self):
        return frozenset({CAP_JSON, CAP_CAM, CAP_WIRE_SIZE})

    def wire_size_bytes(self, claim, signer="digest"):
        return 5


class DriftingEnvelope(_Base):
    """Declares `wire_size` and charges a length that does not track its own payload."""

    plugin_id = "provdrift"

    def capabilities(self):
        return frozenset({CAP_JSON, CAP_CAM, CAP_WIRE_SIZE})

    def wire_size_bytes(self, claim, signer="digest"):
        return 300 + (int(claim.station_id) % 7)


class FakeEvidence(_Base):
    """An honest length and a four-byte blob for evidence."""

    plugin_id = "provfake"

    def evidence_pdu(self, claim, station):
        return b"\\x00\\x01\\x02\\x03"

    def wire_size_bytes(self, claim, signer="digest"):
        return 300
'''


@pytest.fixture
def codecs(tmp_path):
    (tmp_path / "prov_codecs.py").write_text(_CODECS, encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    try:
        yield tmp_path
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("prov_codecs", None)


def _codec_run(tmp_path, name, cls):
    return run_pipeline(PipelineConfig(
        plugins={"message_codec": {"ref": f"prov_codecs:{cls}"}},
        out_dir=str(tmp_path / name), **_CFG))


def test_an_honest_codec_runs(codecs):
    """The case the checks exist to protect: a codec whose declared length is its own payload plus
    a constant envelope, and whose evidence PDU decodes back to the claim."""
    res = _codec_run(codecs, "ok", "Honest")
    assert res.n_reports >= 0
    man = json.loads((codecs / "ok" / "manifest.json").read_text(encoding="utf-8"))
    assert man["config"]["plugins"]["message_codec"]["ref"] == "prov_codecs:Honest"


def test_a_codec_that_declares_wire_size_and_fabricates_it_is_refused(codecs):
    """ATTACK D, first half. `wire_size` means "derived from a real encode"; a 5-byte frame carrying
    a hundred-byte payload is not that, and the engine can do the arithmetic itself."""
    with pytest.raises(ConfigError) as e:
        _codec_run(codecs, "lying", "LyingSize")
    assert "cannot be shorter than the payload" in str(e.value)
    assert not (codecs / "lying" / "manifest.json").exists()


def test_a_declared_envelope_that_does_not_track_the_encoder_is_refused(codecs):
    """The subtler fabrication: a plausible ~300 B that drifts per message. The envelope for one
    (msg_type, signer) is a CONSTANT, so a length that wanders is a fabricated CBR."""
    with pytest.raises(ConfigError) as e:
        _codec_run(codecs, "drift", "DriftingEnvelope")
    assert "CONSTANT" in str(e.value)


def test_an_evidence_pdu_that_does_not_decode_is_refused(codecs):
    """ATTACK D, second half. These octets are what the dataset records as evidence and what a
    TS 103 759 `v2xPduEvidence` entry would carry; a blob nobody can decode is not evidence."""
    with pytest.raises(ConfigError) as e:
        _codec_run(codecs, "fake", "FakeEvidence")
    assert "does not decode" in str(e.value) or "decodes to a position" in str(e.value)
    assert not (codecs / "fake" / "manifest.json").exists()


def test_a_codec_that_does_not_declare_wire_size_may_model_its_length(codecs):
    """THE HONEST BOUNDARY, pinned. The built-in `native_v1` deliberately charges the engine's
    legacy 300 B regardless of content, because its job is to be what the engine does today. Holding
    a MODELLED constant to the length of its own payload would be refusing the thing it declared --
    so the envelope rule applies only to a codec that declares `wire_size`, and the declaration is
    in the manifest where a reader can see it.
    """
    res = run_pipeline(PipelineConfig(message_codec="native_v1",
                                      out_dir=str(codecs / "native"), **_CFG))
    assert res.n_reports >= 0
