"""Out-of-process detector execution -- the REAL boundary, and the only one this project claims.

**Why this module exists, in one paragraph.** An in-process Python plugin cannot be sandboxed.
``sys._getframe``, ``gc.get_referrers``, ``inspect``, module globals and ``__subclasses__`` are all
reachable from any callable in the process, and one line --
``sys._getframe(1).f_locals["b"]`` -- hands a detector the engine's broadcast dict, which carries
``veh`` (a whole `Vehicle` with ``.is_attacker`` / ``.attack_type`` / ``.victims``), the sender's
TRUE ``x`` / ``y``, ``falsified`` and ``ghost``. Every reproducibility layer in this project is
structurally blind to that: reading the oracle is perfectly deterministic, so the digests, the
goldens, the content-hash lock and the two-run equality gate all reproduce while the dataset's labels
leak into its features. `docs/realism/DETECTOR-PLUGIN.md` section 2 measures it. The `Observation`
DTO, the `NamespacedState` wrapper, the source gate and the integrity monitor raise the bar and make
the reach deliberate and reviewable; **none of them is containment**, and this repository exists to
produce misbehaviour-detection datasets and let researchers compare detectors, so a submitted
detector that can read the labels it is predicting makes any benchmark built on it worthless.

The answer is not another wrapper. It is a process in which the ground truth **is not present**.

::

    engine process                              detector child (`python -m ...api.isolate --serve`)
      |-- HELLO   {api, slot, ref, seed} ------------->|   resolve() only -- nothing constructed yet
      |<-- META   {plugin_id, reason_code, ...} -------|
      |    (parent validates params, hashes the        |
      |     reported source paths, source-gates)       |
      |-- CONSTRUCT {params, env} -------------------->|   the plugin's __init__ runs HERE
      |<-- READY  {capabilities} ----------------------|
      |   per received message:                        |
      |-- EVAL {seq, step, obs[34], state} ----------->|   evaluate(obs, state, params, rng)
      |<-- SCORE {seq, score, state} ------------------|
      |-- FINISH ------------------------------------->|
      |<-- SUMMARY {declared_streams, evaluated} ------|

**What crosses the boundary is exactly the `Observation` and the plugin's own state.** The broadcast
dict, the `Vehicle`, the `PipelineConfig` and the engine's global `random.Random(cfg.seed)` are not
serialised, are not referenced by anything that is serialised, and do not exist in the child's
address space at all. That is a structural property of the payload builder (:func:`_eval_payload`),
not a rule someone has to remember, and `tests/test_detector_isolation.py` asserts it against
`Observation`'s own declared field list.

TRANSPORT, and why this one
---------------------------
**Length-prefixed JSON over the child's stdin/stdout pipes.** Four bytes big-endian unsigned, then
that many bytes of UTF-8 JSON. The three properties that chose it, in order of weight:

1. **JSON is data, not code.** The reply comes from a process the untrusted plugin runs inside, so
   the parent must be unable to be harmed by a hostile reply. `pickle` -- and therefore
   `multiprocessing`, whose queues pickle -- would hand that process arbitrary code execution in the
   ENGINE, which is the exact thing this mode exists to prevent. A crafted JSON document can at worst
   be a wrong number, and every number is range-checked on arrival.
2. **Determinism.** `json.dumps` renders a float through `float.__repr__`, the shortest string that
   round-trips, so a `float` survives the crossing bit-exactly; the child derives randomness from
   `RngNamespace(seed, plugin_id)`, a pure function of (seed, plugin, label, ids, step), so it draws
   the *same words* as the in-process path; and the exchange is strict lockstep with an echoed
   sequence number, so nothing in the engine's data can depend on the child's timing or on the OS
   scheduler. The one clock in this module is a fail-stop watchdog, and a run that trips it produces
   no data at all.
3. **No ambient resources.** No port to allocate (so no ephemeral-port collision, no loopback
   firewall prompt on Windows, and no chance of a second process connecting), no temp file to
   race, no shared memory to unmap. The pipe dies with the parent, so an abandoned child is reaped
   by the OS rather than by a cleanup path that might not run.

Rejected, and why: a loopback socket (an ambient, connectable resource, and Windows-hostile);
`multiprocessing` (pickle, plus a feeder thread whose scheduling the parent would then depend on);
shared memory (needs a lock discipline, and a hostile child can corrupt the parent's view);
per-STEP batching as `PLUGIN-ARCHITECTURE.md` D1 specifies for the CHANNEL slot (the score of one
message is consumed by the fusion before the next message is scored, so batching a step would mean
restructuring the reception loop and re-pinning every golden -- see `docs/realism/DETECTOR-PLUGIN.md`
for the measured cost of not doing it).

THE RUN'S OWN GROUND TRUTH IS NOT WRITTEN WHILE THE CHILD IS ALIVE
-----------------------------------------------------------------
Isolation began by closing the *address space* and leaving the *filesystem* open, and that was not
enough: the engine STREAMED ``ground_truth/gt_report_labels.jsonl`` as the loop ran, so a detector
that opened it at its 20 000th message read the oracle verdict -- plus ``reporter_true_id`` and
``subject_true_id`` -- for every report filed so far, in the very run it was being graded on.
Measured, and then closed.

The engine now WITHHOLDS every ORACLE output while any isolated check is loaded
(`mock_pipeline/run.py`, ``_WithheldStream``): the rows are buffered and the files are created only
after the last worker has been reaped. So during the run there is nothing for the child to open --
not an empty file, not a partial one, no ``ground_truth/`` directory at all. Measured cost on the
InTAS AM peak (1 188 vehicles, 158 767 vehicle-steps): 3.2 MiB held, **+4.1 MB of peak working set**,
same ``data_digest``.

**Where that degrades, stated rather than buried.** Past ``run.WITHHELD_MEMORY_BYTES`` (384 MiB of
withheld output across both tables) the overflow spills to ``<out_dir>/.withheld/*.sealed``, XORed
with an ``os.urandom`` key held only in the engine's memory; the real file is unsealed from it once
the worker is gone. So the claim has two tiers and both are true as written: **under the ceiling
there is nothing on disk to read; over it there is a file that holds nothing readable.**
``docs/realism/ISOLATION-ORACLE-LEAK.md`` has the measurement of both.

``ma/ma_reports.jsonl`` is MA-visible and is still streamed, so a child can read the built-in suite's
scores for messages already processed. That is not the answer key -- no ``_visibility: ORACLE`` row,
no true id, no ``falsified`` flag -- and withholding it too would cost every long run its memory
bound, so it is a deliberate choice and it is named here rather than left to be discovered.

Defence in depth, and it is only that: the child is started in a **fresh empty working directory**
(so a relative path cannot land in the dataset), and its environment is scrubbed of every variable
and every ``PYTHONPATH`` entry whose value names the output directory (:func:`child_env`).

WHAT THIS STILL DOES NOT CLOSE
------------------------------
The child remains an ordinary OS process running as the same user. It cannot reach the engine's
objects and it cannot read this run's labels, because they do not exist yet -- but it CAN read the
filesystem: this repository (``scms_sim_ref.__file__`` names it), and any OTHER dataset directory on
the machine, including a completed earlier run's ``ground_truth/*.jsonl``. A determined child can
walk the disk looking for one. So the claim this module supports is exactly these two sentences, and
not a word more:

    **The run's ground truth is not in the child's ADDRESS SPACE -- the frame walk that reads it
    in-process finds only this module's own frames -- and it is not on DISK while the child is
    alive.** Other datasets on the same machine are readable by the child, and keeping them out of
    reach is an operational matter (a separate user, ACLs, or an OS-level sandbox around the worker,
    which this mode makes possible -- one process, one pipe -- and does not itself provide).

Measured, and still true: a child that walks the temp tree finds COMPLETED earlier runs and reads
their ``ground_truth/*.jsonl`` -- ``reporter_true_id``, ``subject_true_id``,
``report_correctness`` -- in full. That is a different dataset's answer key, not this one's, and a
detector trained on it is a detector that has seen labels it was never given; for a benchmark, one
is as disqualifying as the other.

**Why there is no filesystem allow-list here.** A PEP 578 audit hook in the child could deny
``open`` / ``os.listdir`` / ``os.scandir`` outside a set of roots, and it would stop the walk
described above -- :mod:`~scms_sim_ref.api.guard` is the same machinery, used for the reflective
events. It is deliberately not offered, because the child is the plugin's OWN process: whatever
policy state that hook consulted would be an object the plugin can reach and edit, exactly as
`guard.py`'s own docstring concedes for the in-process case, and it is the same interpreter
throughout. In the in-process case that residue is worth accepting because the alternative is
nothing at all; here it is not, because it would put a "the worker cannot read your disk" sentence
next to a mechanism that a determined worker turns off, and this project has already withdrawn two
containment claims. **What actually keeps another dataset away from the worker is an OS boundary --
a separate user account, a directory ACL, a container -- and this mode is the shape that makes one
possible: one process, one pipe, no shared state.** That is the whole claim, and it is the
operator's to complete.

`docs/realism/DETECTOR-PLUGIN.md` section 2.8 has the full measurement.
"""
from __future__ import annotations

import atexit
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import weakref

from typing import MutableMapping

from .detect import (OBSERVATION_FIELD_ORDER, RESERVED_STATE_KEYS, Observation)
from .errors import ConfigError
from .rng import RESERVED_PREFIX, RngNamespace

#: Bumped when the message vocabulary changes incompatibly. Echoed in HELLO and asserted by the
#: child, so an engine and a worker from different versions fail at the handshake and never at
#: message 40 000.
PROTOCOL_VERSION = "DetectorIsolate/1.0"

#: Refuse an absurd length header rather than trying to allocate it. A single `Observation` frame is
#: a few hundred bytes; the ceiling exists because the header comes from an untrusted process.
MAX_FRAME_BYTES = 8 << 20

#: Wall-clock ceiling for ONE exchange. This is the module's only clock, and it is a FAIL-STOP: a
#: run that trips it raises and writes no manifest. It is never control flow -- there is no path on
#: which a slow child yields a different score, a zero, or a skipped message.
DEFAULT_TIMEOUT_S = 60.0

#: Ceiling for the handshake, which imports and constructs the plugin. Generous: importing a
#: distribution that pulls numpy/torch is seconds, and a benchmark host should not have to tune this.
DEFAULT_START_TIMEOUT_S = 180.0


#: Every worker that has been spawned and not yet closed, weakly held.
#:
#: `CheckSuite.close()` is the intended reaping path and `__del__` is the second. This is the third,
#: and it exists because of the IN-PROCESS MULTI-RUN DRIVERS (`datagen/foundry.py`, `campaign.py`,
#: `massive.py`, `gui/agent.py`): there the engine process outlives the run, so a run that raised
#: between spawning a worker and reaching `close()` would leave a child holding a pipe for the
#: lifetime of the driver, and thousands of runs would leave thousands of them. Weak, so it never
#: keeps a worker alive; `atexit`, so it costs nothing until the process ends.
_LIVE: "weakref.WeakSet" = weakref.WeakSet()


@atexit.register
def _reap_workers() -> None:                             # pragma: no cover - interpreter shutdown
    for worker in list(_LIVE):
        try:
            worker.close()
        except BaseException:                            # noqa: BLE001 - shutdown is best-effort
            pass


class IsolationError(ConfigError):
    """The isolated detector could not be run, or did not answer as the protocol requires.

    A `ConfigError`, so it takes the same fatal-before-step-0 path every other plugin failure takes,
    and so `--check-config`, the GUI and the copilot report it the same way. **There is no path on
    which a child that crashed, hung, desynchronised or answered nonsense produces a score.** A
    detector that cannot be run is a failed run, never a run in which it scored 0.0.
    """


# --------------------------------------------------------------------------- #
# Framing
# --------------------------------------------------------------------------- #
def _dump(obj) -> bytes:
    """Canonical-ish JSON bytes. `separators` trims the wire; floats go through `float.__repr__`,
    which is the shortest string that round-trips to the same double."""
    return json.dumps(obj, separators=(",", ":")).encode("utf-8")


def write_frame(fh, obj, *, body=None) -> None:
    """`body` lets a caller that has already encoded the payload avoid encoding it twice -- the hot
    path does, because it encodes once to find out whether the payload is serialisable at all."""
    if body is None:
        body = _dump(obj)
    fh.write(len(body).to_bytes(4, "big"))
    fh.write(body)
    fh.flush()


def _read_exact(fh, n: int):
    """Exactly `n` bytes, or None at a clean EOF. A SHORT read is EOF, never a partial message."""
    buf = bytearray()
    while len(buf) < n:
        chunk = fh.read(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


def read_frame(fh):
    head = _read_exact(fh, 4)
    if head is None:
        return None
    n = int.from_bytes(head, "big")
    if n <= 0 or n > MAX_FRAME_BYTES:
        raise IsolationError(f"isolated detector: frame length {n} is out of range "
                             f"(1..{MAX_FRAME_BYTES})")
    body = _read_exact(fh, n)
    if body is None:
        raise IsolationError(f"isolated detector: truncated frame (wanted {n} bytes, got EOF)")
    try:
        return json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as e:
        raise IsolationError(f"isolated detector: reply is not valid JSON ({e})") from None


# --------------------------------------------------------------------------- #
# Payloads -- the boundary, stated as code
# --------------------------------------------------------------------------- #
def _eval_payload(obs, own, reserved, step, seq) -> dict:
    """The EVAL frame. **Read this function to know what crosses the boundary.**

    `obs` is flattened POSITIONALLY through `Observation`'s own declared field order -- so a field
    added to the DTO crosses automatically and a field that is not on the DTO cannot cross at all.
    Nothing else about the message, the sender, the receiver or the run is included: no `veh`, no
    true position, no `falsified`, no `ghost`, no config object and no engine rng.
    """
    fields = [getattr(obs, name) for name in OBSERVATION_FIELD_ORDER]
    # `neighbourhood` is the one Mapping field and the engine hands it over as a MappingProxyType,
    # which `json` will not encode. Flattened here and rebuilt as a proxy on the far side, so the
    # plugin sees the same read-only mapping it sees in process.
    fields[_NEIGHBOURHOOD_AT] = dict(fields[_NEIGHBOURHOOD_AT] or {})
    return {"t": "EVAL", "seq": seq, "step": step, "obs": fields, "own": own, "res": reserved}


#: Index of `Observation.neighbourhood` in the declared field order. Derived, never hard-coded, so
#: reordering the DTO cannot silently point this at a float.
_NEIGHBOURHOOD_AT = OBSERVATION_FIELD_ORDER.index("neighbourhood")


#: Sorted once. The engine's reserved per-link state keys, in the order they go on the wire.
_RESERVED_ORDER = tuple(sorted(RESERVED_STATE_KEYS))


def _reserved_raw(st) -> dict:
    """The engine's RESERVED per-link keys, verbatim.

    A third-party check may READ `h` (the claim history), `streak`, `touch` and `kf` in process --
    `NamespacedState.__getitem__` hands them back through `_read_only`. Withholding them out of
    process would make the same detector score differently in the two modes, which would defeat the
    equal-scores property this mode is graded on. None of the four is ground truth: `h` is the
    sequence of CLAIMS this receiver decoded, `streak` and `kf` are MA-side derived counters, `touch`
    is a step number.
    """
    return {k: st[k] for k in _RESERVED_ORDER if k in st}


def _reserved_marked(st) -> dict:
    """:func:`_reserved_raw`, with any value that will not serialise replaced by a marker.

    The SLOW path, taken only after a whole-payload encode has already failed. Testing each value
    with its own `json.dumps` on every message costs four extra encodes per delivered link on the
    hottest path in the run, to guard a case that is empty for every state shape the engine actually
    builds -- so the fast path encodes once and this runs only to say WHICH key was the problem.
    A marked value makes the child raise when the plugin READS it, rather than letting it score
    against a state the in-process path would have shown differently.
    """
    out = {}
    for key, value in _reserved_raw(st).items():
        try:
            json.dumps(value)
        except (TypeError, ValueError):
            out[key] = {"__unserialisable__": type(value).__name__}
            continue
        out[key] = value
    return out


def unserialisable_message(own) -> str:
    """Name the state key that could not cross, for a reply the child has already failed to encode.

    In process a check's own namespace may hold any object; out of process it must be JSON. The
    honest way to say so is an error naming the KEY -- never a silent drop, which would make the two
    modes disagree from the next message on.

    Reached only AFTER a whole-reply encode has failed, so the per-key probing costs nothing on the
    hot path. Testing every key on every message would be one `json.dumps` per state entry per
    delivered link, to guard a case that is empty for every well-formed check.
    """
    for key, value in (own or {}).items():
        try:
            json.dumps(value)
        except (TypeError, ValueError) as e:
            return (f"state[{key!r}] is not JSON-serialisable ({e}). An isolated check's state "
                    f"crosses a process boundary on every message, so `state[...]` must hold JSON "
                    f"values (numbers, strings, booleans, null, lists, dicts). Keep derived objects "
                    f"on `self`, which lives in the worker for the whole run.")
    return "the worker's reply could not be encoded as JSON"


# --------------------------------------------------------------------------- #
# Parent side
# --------------------------------------------------------------------------- #
class IsolatedRng:
    """Stands in for the `RngNamespace` an in-process check is handed.

    The REAL namespace lives in the child and is constructed there from `(seed, plugin_id)` -- the
    same pure function of the same two inputs, so it produces the identical stream. This shim only
    carries the step the engine last announced (the engine calls `begin_step` on every check's
    namespace once per step) and reports back the labels the child said it actually used, which is
    what `manifest["plugins"]["loaded"][*].declared_streams` records.

    It deliberately has no `stream()` / `persistent()`: nothing in the parent may draw from a plugin
    stream, and a missing method is a better statement of that than a raising one.
    """

    __slots__ = ("_seed", "_pid", "_step", "_streams")

    def __init__(self, seed: int, plugin_id: str):
        self._seed, self._pid = int(seed), str(plugin_id)
        self._step = -1
        self._streams: tuple = ()

    @property
    def plugin_id(self) -> str:
        return self._pid

    @property
    def step(self) -> int:
        return self._step

    def begin_step(self, step: int) -> None:
        self._step = int(step)

    def declared_streams(self) -> tuple:
        return self._streams

    def advance_counts(self) -> dict:
        return {}


class IsolatedCheck:
    """A third-party `check` running in its own interpreter, with the engine's API on this side.

    Constructed in the engine, it exposes the SAME `evaluate(obs, state, params, rng)` the reception
    loop calls on an in-process check, so the hot loop is unchanged: the call plan holds this
    object's bound method exactly as it holds a plugin's. Everything else about it is different --
    the plugin's module is never imported here, its `__init__` never runs here, and the only thing
    it is ever told is one `Observation` at a time.
    """

    __slots__ = ("slot", "ref", "declared", "seed", "env", "timeout", "start_timeout", "meta",
                 "plugin_id", "reason_code", "precision", "soft", "msg_types", "vru_suppressed",
                 "capabilities", "interface_version", "resolved_via", "params", "rng", "deny",
                 "_proc", "_in", "_out", "_seq", "_deadline", "_watchdog", "_stop", "_stderr",
                 "_stderr_thread", "_timed_out", "_closed", "_evaluated", "_ns", "_workdir",
                 "__weakref__")

    def __init__(self, ref: str, declared: dict, *, seed: int, env: dict,
                 timeout: float = DEFAULT_TIMEOUT_S,
                 start_timeout: float = DEFAULT_START_TIMEOUT_S, slot: str = "check",
                 deny=()):
        self.slot, self.ref = str(slot), str(ref)
        self.declared = dict(declared or {})
        #: Directories the child must not be POINTED AT -- the run's output directory. See
        #: :func:`child_env`; defence in depth, never the primary containment.
        self.deny = tuple(str(d) for d in (deny or ()))
        self.seed, self.env = int(seed), dict(env or {})
        self.timeout = float(timeout)
        self.start_timeout = float(start_timeout)
        self.meta: dict = {}
        self.params: dict = {}
        self.plugin_id = self.reason_code = ""
        self.precision, self.soft = 3, False
        self.msg_types: tuple = ("cam",)
        self.vru_suppressed = False
        self.capabilities: frozenset = frozenset()
        self.interface_version = self.resolved_via = ""
        self.rng = None
        self._proc = self._in = self._out = None
        self._seq = 0
        self._evaluated = 0
        self._deadline = None
        self._watchdog = self._stderr_thread = None
        self._stop = threading.Event()
        self._stderr: list = []
        self._timed_out = False
        self._closed = False
        self._ns = ""
        self._workdir = None

    # -- lifecycle ---------------------------------------------------------------------------- #
    def spawn(self) -> dict:
        """Start the child and RESOLVE the plugin there. Returns its metadata; constructs nothing.

        Split from :meth:`construct` on purpose: the parent validates the declared params against the
        `FieldSpec`s the child reports, and screens the source file the child names, BEFORE the
        plugin's ``__init__`` is allowed to run anywhere.
        """
        cmd = [sys.executable, "-X", "utf8", "-m", "scms_sim_ref.api.isolate", "--serve"]
        # A FRESH EMPTY working directory, not the engine's. Defence in depth: `out/run7` typed by a
        # plugin resolves against a directory that holds nothing, and `os.listdir(".")` is empty. It
        # is NOT containment -- an absolute path still works and the child can walk the disk -- so it
        # is stated as a second line and never as the claim. `child_env` re-adds the parent's cwd to
        # `PYTHONPATH`, so nothing that used to import stops importing.
        self._workdir = tempfile.mkdtemp(prefix="scms-detector-")
        try:
            # BUFFERED pipes on purpose. With `bufsize=0` the handles are raw `FileIO`, whose
            # `write()` is allowed to be SHORT on a pipe -- a truncated frame that would surface as
            # a protocol error under load and nowhere else. A `BufferedWriter` writes it all, and a
            # `BufferedReader.read(n)` returns exactly n bytes or EOF, which is what the framing
            # wants. Every write is explicitly flushed.
            self._proc = subprocess.Popen(
                cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env=child_env(self.deny), cwd=self._workdir)
        except OSError as e:
            self._drop_workdir()
            raise IsolationError(
                f"plugins.check {self.ref!r}: isolated=true, but the detector child could not be "
                f"started ({' '.join(cmd[:4])}...): {e}") from None
        self._in, self._out = self._proc.stdin, self._proc.stdout
        _LIVE.add(self)
        self._stderr_thread = threading.Thread(target=self._drain_stderr, daemon=True)
        self._stderr_thread.start()
        self._watchdog = threading.Thread(target=self._watch, daemon=True)
        self._watchdog.start()
        meta = self._exchange({"t": "HELLO", "api": PROTOCOL_VERSION, "slot": self.slot,
                               "ref": self.ref, "seed": self.seed,
                               "plugin_id_fallback": _fallback_pid(self.ref)},
                              "META", timeout=self.start_timeout)
        self.meta = meta
        self.plugin_id = str(meta["plugin_id"])
        self.reason_code = str(meta["reason_code"])
        self.precision = int(meta["precision"])
        self.soft = bool(meta["soft"])
        self.msg_types = tuple(str(m) for m in meta["msg_types"])
        self.vru_suppressed = bool(meta["vru_suppressed"])
        self.interface_version = str(meta["interface_version"])
        self.resolved_via = str(meta["resolved_via"])
        self.rng = IsolatedRng(self.seed, self.plugin_id)
        self._ns = f"{RESERVED_PREFIX}:{self.plugin_id}"
        return meta

    def construct(self, params: dict) -> frozenset:
        """Build the plugin IN THE CHILD with the params the parent resolved and validated."""
        self.params = dict(params or {})
        reply = self._exchange({"t": "CONSTRUCT", "params": self.params, "env": self.env},
                               "READY", timeout=self.start_timeout)
        self.capabilities = frozenset(str(c) for c in reply.get("capabilities") or ())
        got = reply.get("params_sha256")
        want = _params_sha256(self.params)
        if got != want:
            # The child echoes a hash of what it ACTUALLY constructed with. A mismatch means the
            # worker used a different parameter set than the one the manifest is about to record --
            # i.e. the artifact would describe a run that did not happen.
            raise IsolationError(
                f"plugins.check {self.ref!r}: the isolated worker constructed the plugin with a "
                f"different parameter set than the engine resolved (params_sha256 {got!r} != "
                f"{want!r}); the manifest would record parameters that did not run")
        return self.capabilities

    # -- the hot path ------------------------------------------------------------------------- #
    def evaluate(self, obs, state, params, rng) -> float:
        """One message, one round trip. Signature-identical to `Check.evaluate`.

        `state` is the engine's RAW per-(receiver, sender) dict -- the namespacing an in-process
        third party gets from `NamespacedState` is done here instead, because the plugin's own
        namespace has to be extracted, shipped and put back anyway.
        """
        own = state.get(self._ns)
        if own is None:
            own = state[self._ns] = {}
        self._seq += 1
        step = rng.step if rng is not None else -1
        payload = _eval_payload(obs, own, _reserved_raw(state), step, self._seq)
        try:
            body = _dump(payload)
        except (TypeError, ValueError):
            # One of the engine's reserved values is not JSON. Rebuild with the offender marked, so
            # the child raises with the key's NAME if the plugin reads it and stays silent if it
            # does not -- rather than the whole run failing on state this check never touches.
            payload = _eval_payload(obs, own, _reserved_marked(state), step, self._seq)
            body = _dump(payload)
        reply = self._exchange(payload, "SCORE", timeout=self.timeout, body=body)
        if reply.get("seq") != self._seq:
            raise IsolationError(
                f"plugins.check {self.ref!r}: isolated worker replied out of sequence "
                f"(expected {self._seq}, got {reply.get('seq')!r}). The exchange is strict "
                f"lockstep; a desynchronised worker is refused rather than allowed to shift the "
                f"dataset by one message.")
        score = reply.get("score")
        if not isinstance(score, (int, float)) or isinstance(score, bool):
            raise IsolationError(
                f"plugins.check {self.ref!r}: isolated worker returned score {score!r}, which is "
                f"not a number. A detnorm must be a finite float (>= 1.0 == violating).")
        new_own = reply.get("own")
        if not isinstance(new_own, dict):
            raise IsolationError(f"plugins.check {self.ref!r}: isolated worker returned state "
                                 f"{type(new_own).__name__}, expected an object")
        state[self._ns] = new_own
        self._evaluated += 1
        return float(score)

    # -- teardown ----------------------------------------------------------------------------- #
    def finish(self) -> dict:
        """FINISH/SUMMARY. Collects the stream labels the child actually used for the manifest."""
        if self._closed or self._proc is None:
            return {}
        try:
            summary = self._exchange({"t": "FINISH"}, "SUMMARY", timeout=self.timeout)
        except IsolationError:
            return {}
        if self.rng is not None:
            self.rng._streams = tuple(str(s) for s in summary.get("declared_streams") or ())
        got = summary.get("evaluated")
        if got != self._evaluated:
            raise IsolationError(
                f"plugins.check {self.ref!r}: isolated worker scored {got!r} messages, the engine "
                f"asked for {self._evaluated}. A worker that did not see every message it was sent "
                f"produced a dataset nobody can reproduce.")
        return summary

    def close(self) -> None:
        """Always safe, always idempotent. Called on the success path AND on every error path."""
        if self._closed:
            return
        self._closed = True
        self._stop.set()
        _LIVE.discard(self)
        proc = self._proc
        if proc is None:
            self._drop_workdir()
            return
        try:
            if proc.stdin is not None and not proc.stdin.closed:
                proc.stdin.close()               # EOF -> the child's serve loop returns
        except OSError:
            pass
        try:
            proc.wait(timeout=5.0)
        except subprocess.TimeoutExpired:        # pragma: no cover - a wedged child
            proc.kill()
            try:
                proc.wait(timeout=5.0)
            except subprocess.TimeoutExpired:
                pass
        for stream in (proc.stdout, proc.stderr):
            try:
                if stream is not None and not stream.closed:
                    stream.close()
            except OSError:
                pass
        self._drop_workdir()

    def _drop_workdir(self) -> None:
        """Remove the child's sandbox directory. Best-effort: a plugin may have left files in it,
        and on Windows a handle the dying child has not released yet makes the unlink fail. Never
        fatal -- a leftover temp directory is a tidiness problem, not a correctness one."""
        path, self._workdir = self._workdir, None
        if path:
            shutil.rmtree(path, ignore_errors=True)

    def workdir(self):
        """The child's working directory while it is running, or None. Test/diagnostic only."""
        return self._workdir

    def __del__(self):                                 # pragma: no cover - GC timing
        """Last-resort reaping. `CheckSuite.close()` is the intended path; this catches a run that
        raised before reaching it, which matters for the in-process multi-run drivers where the
        engine process outlives the run and the pipe would otherwise stay open."""
        try:
            self.close()
        except BaseException:                            # noqa: BLE001 - never raise from __del__
            pass

    # -- plumbing ----------------------------------------------------------------------------- #
    def _exchange(self, payload: dict, expect: str, *, timeout: float, body=None) -> dict:
        if self._proc is None or self._closed:
            raise IsolationError(f"plugins.check {self.ref!r}: isolated worker is not running")
        self._deadline = time.monotonic() + float(timeout)
        try:
            try:
                write_frame(self._in, payload, body=body)
            except (OSError, ValueError):
                raise self._died(f"the worker's stdin closed while sending {payload['t']}") from None
            reply = read_frame(self._out)
        finally:
            self._deadline = None
        if reply is None:
            raise self._died(f"the worker exited without answering {payload['t']}")
        kind = reply.get("t")
        if kind == "ERROR":
            raise IsolationError(
                f"plugins.check {self.ref!r}: the isolated worker refused {payload['t']}.\n"
                f"  {reply.get('error')}\n"
                + _indent(reply.get("traceback") or "")
                + self._stderr_tail())
        if kind != expect:
            raise IsolationError(f"plugins.check {self.ref!r}: isolated worker replied {kind!r}, "
                                 f"expected {expect!r}")
        return reply

    def _died(self, what: str) -> IsolationError:
        rc = None
        if self._proc is not None:
            try:
                rc = self._proc.poll()
                if rc is None:
                    self._proc.kill()
                    rc = self._proc.wait(timeout=5.0)
            except (OSError, subprocess.TimeoutExpired):  # pragma: no cover
                pass
        if self._timed_out:
            return IsolationError(
                f"plugins.check {self.ref!r}: the isolated detector did not answer within "
                f"{self.timeout:.0f}s and was killed. A hung detector FAILS THE RUN -- there is no "
                f"path on which a timeout becomes a score, because a score that depends on the "
                f"scheduler is not reproducible." + self._stderr_tail())
        return IsolationError(
            f"plugins.check {self.ref!r}: {what} (exit {rc!r}). A crashed isolated detector fails "
            f"the run; it never silently scores 0.0." + self._stderr_tail())

    def _stderr_tail(self) -> str:
        text = "".join(self._stderr)[-4000:].strip()
        return f"\n--- worker stderr ---\n{text}" if text else ""

    def _drain_stderr(self) -> None:
        """Drain the child's stderr continuously. Without this a child that writes more than the pipe
        buffer blocks forever inside `print`, and the parent would attribute that to the plugin."""
        stream = self._proc.stderr
        try:
            for line in iter(stream.readline, b""):
                if len(self._stderr) < 2000:
                    self._stderr.append(line.decode("utf-8", "replace"))
        except (OSError, ValueError):                   # pragma: no cover - closed under us
            pass

    def _watch(self) -> None:
        """The fail-stop. Polls a deadline set around each blocking read; on expiry it kills the
        child, which turns the parent's blocking read into an EOF and then into a loud error.

        This is the ONLY use of a clock in this module, and it can only ever turn a run into a
        failure -- never into a different run."""
        while not self._stop.wait(0.25):
            deadline = self._deadline
            if deadline is not None and time.monotonic() > deadline:
                self._timed_out = True
                try:
                    self._proc.kill()
                except OSError:                          # pragma: no cover
                    pass
                return


def _indent(text: str) -> str:
    tail = (text or "").strip()
    if not tail:
        return ""
    return "".join(f"  {line}\n" for line in tail.splitlines()[-25:])


def _params_sha256(params) -> str:
    from . import registry
    return registry.params_sha256(params)


def _fallback_pid(ref: str) -> str:
    tail = ref.rsplit(":", 1)[-1] if ":" in ref else ref
    out = "".join(ch if ch.isalnum() else "_" for ch in tail).lower().strip("_")
    return out[:32] or "plugin"


def _norm(path) -> str:
    """An absolute, case-folded, separator-normalised path for CONTAINMENT tests only."""
    try:
        return os.path.normcase(os.path.abspath(os.fspath(path)))
    except (TypeError, ValueError):                      # pragma: no cover - unusable path
        return ""


def _is_within(path: str, root: str) -> bool:
    """True when `path` IS `root` or lives under it. Both already through :func:`_norm`."""
    if not path or not root:
        return False
    if path == root:
        return True
    return path.startswith(root.rstrip(os.sep) + os.sep)


def child_env(deny=()) -> dict:
    """The child's environment: this interpreter's `sys.path`, carried over as `PYTHONPATH`.

    The same decision `conformance/attest.py` documents, for the same reason: what the child must be
    able to import is exactly what this process could import, and `-I`/`-E` would ignore
    `PYTHONPATH` and make an out-of-repo plugin unresolvable for reasons that have nothing to do with
    the plugin. `PYTHONHASHSEED` is pinned for the same reason the engine pins it for itself: not for
    the RNG (`random.Random(<str>)` is seeded from sha512 of the key and is hash-seed independent),
    but so set and dict iteration order inside the PLUGIN is stable across processes and runs.

    **`deny` names directories the child must not be POINTED AT** -- in practice the run's output
    directory. Every `PYTHONPATH` entry inside one, and every environment variable whose value names
    one, is dropped. This is defence in depth and nothing more: it removes the paths the child is
    *handed*, not the paths it could *find*. `scms_sim_ref.__file__` still names this repository and
    the child can still walk the disk. What actually keeps this run's labels away from it is that
    they are not written until it has exited (see the module docstring).

    The parent's own working directory is carried over EXPLICITLY, because the child is started in a
    fresh empty one (:meth:`IsolatedCheck.spawn`) and `python -m` would otherwise have put the
    sandbox on `sys.path` in its place -- which would make a plugin that resolves only from the cwd
    unresolvable for a reason that has nothing to do with the plugin.
    """
    denied = tuple(d for d in (_norm(p) for p in deny) if d)
    env = dict(os.environ)
    if denied:
        # A benchmark host that exported the dataset path (`SCMS_OUT=...`) would otherwise hand the
        # child the one path this mode is trying not to hand it. Values are treated as
        # `os.pathsep`-separated lists so a PATH-like variable is FILTERED rather than deleted --
        # deleting `PATH` on Windows would break the child for reasons unrelated to the dataset.
        for name, value in list(env.items()):
            if not isinstance(value, str) or not value:
                continue
            parts = value.split(os.pathsep)
            # ABSOLUTE components only. `_norm` resolves a relative string against the parent's cwd,
            # so testing `PROCESSOR_LEVEL=6` would ask whether `<cwd>/6` is inside the dataset -- a
            # question with a surprising answer for any run whose out_dir happens to BE the cwd. A
            # relative name is also useless to the child, whose cwd is an empty sandbox.
            kept = [p for p in parts
                    if not (os.path.isabs(p) and any(_is_within(_norm(p), d) for d in denied))]
            if len(kept) == len(parts):
                continue
            if kept:
                env[name] = os.pathsep.join(kept)
            else:
                env.pop(name, None)
    entries, seen = [], set()
    for p in list(sys.path) + [os.getcwd()]:
        if not isinstance(p, str) or not p:
            continue
        absolute = os.path.abspath(p)
        if absolute in seen or any(_is_within(_norm(absolute), d) for d in denied):
            continue
        seen.add(absolute)
        entries.append(absolute)
    env["PYTHONPATH"] = os.pathsep.join(entries)
    env.setdefault("PYTHONHASHSEED", "0")
    env.pop("PYTHONSTARTUP", None)
    env.pop("PWD", None)                                 # would name the parent's cwd, not the child's
    return env


# --------------------------------------------------------------------------- #
# Parent side: identity, without importing the plugin
# --------------------------------------------------------------------------- #
def hash_reported_source(meta: dict, ref=None) -> dict:
    """Content hashes for a plugin the parent deliberately did NOT import.

    **THE PARENT RESOLVES THE PATH ITSELF.** It used to hash whatever path the child NAMED, and the
    child is the plugin's own process: a plugin that sets its module's ``__file__`` to a decoy made
    the lock record the decoy's hash (measured: ``6d85ce01`` recorded for real code hashing
    ``90272b8b``), and a replay re-probed the same decoy, so "no drift" was true of a file that was
    never executed while the real code was free to change. So for a dotted ``pkg.mod:Class`` ref --
    which the PARENT owns, because it comes out of the config -- the parent walks `sys.path` itself
    (:func:`~scms_sim_ref.api.registry.static_locate`, which executes nothing, unlike `find_spec`,
    which imports parent packages) and **refuses a worker whose reported path is not the one this
    process resolves.** The package root and the import closure are derived from the parent's path
    too, never from the child's report.

    **The residue, stated rather than discovered.** When the parent cannot resolve the name that way
    -- a zipimport, a namespace package, an entry-point ref, an editable install behind a custom
    finder -- it falls back to the child's path AND SAYS SO in `path_source: "worker"`, and in that
    case the old caveat still applies: a worker that loads module A and names module B produces a
    self-consistent lock over the wrong files. `path_source` is in the lock so a reader can tell the
    two cases apart instead of having to assume.
    """
    from . import registry
    reported = meta.get("module_path")
    module_name = str(ref).partition(":")[0] if ref and ":" in str(ref) else None
    own = registry.static_locate(module_name) if module_name else None
    if own is not None and reported and _norm(reported) != _norm(own):
        raise IsolationError(
            f"plugins.check {ref!r}: the isolated worker reported that it loaded the plugin from\n"
            f"    {reported}\n"
            f"but this process resolves {module_name!r} on the SAME sys.path to\n"
            f"    {own}\n"
            f"The lock's content hashes are computed from the file the ENGINE resolves, because a "
            f"module can set its own __file__ and a worker that names a decoy would produce a "
            f"self-consistent lock over code that never ran. A path this process cannot confirm is "
            f"refused rather than recorded.")
    module_path = own or reported
    package_root = (registry.package_root_of_path(own) if own is not None
                    else meta.get("package_root"))
    msha = None
    if module_path and os.path.isfile(module_path):
        try:
            msha = registry._file_sha256_cached(module_path)
        except OSError:                                  # pragma: no cover
            msha = None
    psha = registry.package_sha256(package_root) if package_root else None
    closure = registry.import_closure_sha256(module_name) if own is not None else None
    out = {"module_sha256": msha, "package_sha256": psha,
           "dist_sha256": meta.get("dist_sha256"),
           "distribution": meta.get("distribution"), "version": meta.get("version"),
           "path_source": "parent" if own is not None else "worker"}
    if closure is not None:
        out["import_closure_sha256"] = closure
    return out


def provenance_record(slot, order, worker, params, conformance=None):
    """The lock entry for an isolated plugin, built WITHOUT importing it.

    Identity comes from the child's report; every hash in it is recomputed HERE by reading the files
    the child named (:func:`hash_reported_source`). `isolated: true` goes into the entry so a replay
    knows to re-probe rather than re-import.
    """
    from . import registry
    h = hash_reported_source(worker.meta, worker.ref)
    incomplete = (h["dist_sha256"] is None and h["module_sha256"] is None
                  and h["package_sha256"] is None)
    closure = ({"sha256": h["import_closure_sha256"], "modules": {}}
               if h.get("import_closure_sha256") else None)
    return registry.ProvenanceRecord(
        slot=slot, order=order, ref=worker.ref, resolved_via=worker.resolved_via,
        distribution=h["distribution"], version=h["version"], dist_sha256=h["dist_sha256"],
        module_sha256=h["module_sha256"], package_sha256=h["package_sha256"],
        interface_version=worker.interface_version, capabilities=frozenset(worker.capabilities),
        declared_streams=tuple(worker.rng.declared_streams() if worker.rng else ()),
        params=dict(params or {}), params_sha256=registry.params_sha256(params),
        provenance_incomplete=incomplete, conformance=conformance, isolated=True,
        import_closure=closure)


def probe(slot: str, ref: str, *, timeout: float = DEFAULT_START_TIMEOUT_S) -> dict:
    """Resolve `ref` in a child and return its metadata plus parent-computed hashes.

    Used by the drift check on replay: a manifest entry recorded as `isolated` must not be
    re-resolved by IMPORTING the plugin into the verifying process, because that is the one thing
    the mode exists to avoid -- and a replay is exactly when an unreviewed plugin is most likely to
    be present.
    """
    worker = IsolatedCheck(ref, {}, seed=0, env={}, start_timeout=timeout, slot=slot)
    try:
        meta = worker.spawn()
        return dict(meta, **hash_reported_source(meta, ref))
    finally:
        worker.close()


# --------------------------------------------------------------------------- #
# Child side
# --------------------------------------------------------------------------- #
class _Worker:
    """The child's whole state. Deliberately tiny: resolve, construct, score, report."""

    def __init__(self):
        self.cls = None
        self.instance = None
        self.plugin_id = ""
        self.rng = None
        self.params: dict = {}
        self.evaluated = 0

    def hello(self, msg: dict) -> dict:
        from . import registry
        from .rng import check_plugin_id
        api = msg.get("api")
        if api != PROTOCOL_VERSION:
            raise IsolationError(f"worker speaks {PROTOCOL_VERSION}, engine offered {api!r}")
        slot, ref = str(msg["slot"]), str(msg["ref"])
        cls, how, iv, _shape = registry.resolve(slot, ref)
        if how == "builtin" and registry.is_builtin(slot, cls):
            raise IsolationError(
                f"{ref!r} is a BUILT-IN check: it IS the engine, its knobs are engine config fields, "
                f"and running it out of process would buy nothing and cost a round trip per message. "
                f"Isolation is for third-party code.")
        self.cls = cls
        self.plugin_id = check_plugin_id(str(getattr(cls, "plugin_id", None)
                                             or msg.get("plugin_id_fallback") or "plugin"))
        self.rng = RngNamespace(int(msg.get("seed", 0)), self.plugin_id)
        dist, version, dsha = registry._distribution_for(cls)
        return {"t": "META", "api": PROTOCOL_VERSION,
                "plugin_id": self.plugin_id,
                "reason_code": str(getattr(cls, "reason_code", "")),
                "precision": int(getattr(cls, "precision", 3)),
                "soft": bool(getattr(cls, "soft", False)),
                "msg_types": [str(m) for m in (getattr(cls, "msg_types", None) or ("cam",))],
                "vru_suppressed": bool(getattr(cls, "vru_suppressed", False)),
                "gate": getattr(cls, "gate", None),
                "interface_version": str(iv), "resolved_via": str(how),
                "config_fields": _spec_wire(cls),
                "module_path": _module_path(cls),
                "package_root": registry.package_root(cls),
                "distribution": dist, "version": version, "dist_sha256": dsha}

    def construct(self, msg: dict) -> dict:
        from . import registry
        self.params = dict(msg.get("params") or {})
        env = dict(msg.get("env") or {})
        self.instance = registry.instantiate(self.cls, params=self.params, rng=self.rng, env=env)
        caps = self.instance.capabilities()
        return {"t": "READY", "capabilities": sorted(str(c) for c in caps),
                "params_sha256": registry.params_sha256(self.params)}

    def evaluate(self, msg: dict) -> dict:
        if self.instance is None:
            raise IsolationError("EVAL before CONSTRUCT")
        import types as _types
        fields = list(msg["obs"])
        if len(fields) != len(OBSERVATION_FIELD_ORDER):
            raise IsolationError(f"EVAL carried {len(fields)} observation fields, "
                                 f"expected {len(OBSERVATION_FIELD_ORDER)}")
        # `neighbourhood` is the one Mapping field; the engine hands the in-process path a
        # MappingProxyType, so the isolated path does too. Anything else would let a check that
        # writes to it succeed here and raise there.
        fields[_NEIGHBOURHOOD_AT] = _types.MappingProxyType(dict(fields[_NEIGHBOURHOOD_AT] or {}))
        obs = Observation(*fields)
        self.rng.begin_step(int(msg.get("step", -1)))
        state = _ChildState(msg.get("own") or {}, dict(msg.get("res") or {}))
        score = self.instance.evaluate(obs, state, self.params, self.rng)
        self.evaluated += 1
        try:
            value = float(score)
        except (TypeError, ValueError):
            raise IsolationError(f"evaluate() returned {score!r}, which is not a float") from None
        # What the plugin left in its OWN namespace goes back to the engine, which owns the lifetime
        # of per-(receiver, sender) state and prunes it. Keeping it here instead would leak for the
        # length of the run and would not survive the engine's pruning, so the two modes would
        # diverge for any stateful check.
        return {"t": "SCORE", "seq": msg.get("seq"), "score": value, "own": state.own()}

    def finish(self, _msg: dict) -> dict:
        return {"t": "SUMMARY", "evaluated": self.evaluated,
                "declared_streams": list(self.rng.declared_streams()) if self.rng else []}


class _ChildState(MutableMapping):
    """The plugin's own namespace, with the engine's reserved keys READABLE exactly as in process.

    **A `MutableMapping` over a plain dict, not a `dict` subclass**, and the difference is not
    stylistic. `NamespacedState` (the in-process wrapper) is a `MutableMapping`, so `setdefault`,
    `pop`, `update` and everything else the ABC derives all route through `__getitem__` /
    `__setitem__` and therefore all honour the reserved keys. A `dict` subclass inherits C
    implementations that bypass the overrides, so `state.setdefault("h", ...)` would raise in process
    and silently create a shadow `h` here -- the two modes disagreeing on a corner, which is the one
    thing this class must never do.

    On this side the namespacing itself has already happened: the parent extracted
    `state["plugin:<id>"]` before serialising, so what the plugin holds IS its own namespace.
    """

    __slots__ = ("_own", "_res")

    def __init__(self, own, reserved):
        self._own = dict(own)
        self._res = reserved

    def __getitem__(self, key):
        if key in RESERVED_STATE_KEYS:
            if key not in self._res:
                raise KeyError(key)
            return _reserved_value(self._res[key], key)
        return self._own[key]

    def __setitem__(self, key, value):
        if key in RESERVED_STATE_KEYS:
            raise ConfigError(f"state key {key!r} is reserved for the built-in detectors; a plugin "
                              f"writes only inside its own namespace")
        self._own[key] = value

    def __delitem__(self, key):
        if key in RESERVED_STATE_KEYS:
            raise ConfigError(f"state key {key!r} is reserved for the built-in detectors")
        del self._own[key]

    def __iter__(self):
        return iter(self._own)

    def __len__(self):
        return len(self._own)

    def __contains__(self, key):
        return key in self._own or (key in RESERVED_STATE_KEYS and key in self._res)

    def own(self) -> dict:
        """What goes back to the engine, which owns this state's lifetime."""
        return self._own


def _reserved_value(value, key: str):
    """A reserved value in the shape `NamespacedState._read_only` would have produced.

    JSON has no tuple, so `h` -- a list of 5-tuples in process -- arrives as a list of lists. It is
    re-tupled RECURSIVELY here so a check that unpacks or compares it behaves identically in the two
    modes; a `dict` becomes a read-only proxy, as it does in process.
    """
    import types as _types
    if isinstance(value, dict):
        if "__unserialisable__" in value and len(value) == 1:
            raise IsolationError(
                f"state[{key!r}] holds a {value['__unserialisable__']} the engine could not send "
                f"across the process boundary. An isolated check cannot read it; run this detector "
                f"in process, or stop reading the engine's reserved state.")
        return _types.MappingProxyType(dict(value))
    if isinstance(value, list):
        return tuple(_retuple(v) for v in value)
    return value


def _retuple(value):
    if isinstance(value, list):
        return tuple(_retuple(v) for v in value)
    return value


def _spec_wire(cls) -> dict:
    """`config_fields()` on the wire: the exact `FieldSpec` constructor arguments, so the PARENT can
    rebuild the spec and do the range checking itself rather than trusting a child-side verdict."""
    fn = getattr(cls, "config_fields", None)
    if not callable(fn):
        return {}
    try:
        spec = fn()
    except TypeError:
        return {}
    out = {}
    for name, fs in dict(spec or {}).items():
        out[str(name)] = {"type": fs.type, "default": fs.default, "help": fs.help,
                          "lo": fs.lo, "hi": fs.hi, "step": fs.step, "unit": fs.unit,
                          "options": list(fs.options) if fs.options else None,
                          "group": fs.group}
    return out


def spec_from_wire(wire: dict) -> dict:
    """The parent's inverse of :func:`_spec_wire`: `{name: FieldSpec}`."""
    from .fields import FieldSpec
    out = {}
    for name, d in sorted((wire or {}).items()):
        out[str(name)] = FieldSpec(
            type=str(d.get("type", "float")), default=d.get("default"), help=str(d.get("help", "")),
            lo=d.get("lo"), hi=d.get("hi"), step=d.get("step"), unit=d.get("unit"),
            options=tuple(d["options"]) if d.get("options") else None, group=d.get("group"))
    return out


def _module_path(cls):
    mod = sys.modules.get(getattr(cls, "__module__", "") or "")
    path = getattr(mod, "__file__", None) if mod is not None else None
    return os.path.abspath(path) if path else None


def serve(inp, out) -> int:
    """The child's message loop. Strict lockstep, one reply per request, EOF ends it."""
    worker = _Worker()
    handlers = {"HELLO": worker.hello, "CONSTRUCT": worker.construct,
                "EVAL": worker.evaluate, "FINISH": worker.finish}
    while True:
        try:
            msg = read_frame(inp)
        except IsolationError:
            return 4
        if msg is None:
            return 0
        kind = str(msg.get("t", ""))
        handler = handlers.get(kind)
        try:
            if handler is None:
                raise IsolationError(f"unknown message {kind!r}")
            reply = handler(msg)
        except BaseException as e:                       # noqa: BLE001 - the child reports anything
            import traceback
            reply = {"t": "ERROR", "error": f"{type(e).__name__}: {e}",
                     "traceback": traceback.format_exc()[-4000:]}
        try:
            write_frame(out, reply)
        except OSError:                                  # pragma: no cover - parent went away
            return 5
        except (TypeError, ValueError):
            # The plugin put something in its state that will not cross. Encoding the whole reply
            # once and probing per key only on failure keeps the hot path to a single encode.
            reply = {"t": "ERROR", "error": unserialisable_message(reply.get("own")),
                     "traceback": ""}
            try:
                write_frame(out, reply)
            except OSError:                              # pragma: no cover
                return 5
        if kind == "FINISH":
            return 0


def main(argv=None) -> int:
    """`python -m scms_sim_ref.api.isolate --serve`.

    **The first thing it does is take the protocol's file descriptors away from the plugin.** fd 1 is
    duplicated to a private descriptor and then pointed at stderr, and fd 0 at the null device, so a
    plugin that `print()`s -- or that reads `input()` -- cannot corrupt or steal the frame stream.
    Without this, one stray `print` in a submitted detector desynchronises the exchange and the
    failure looks like a protocol bug in the engine.
    """
    argv = list(sys.argv[1:] if argv is None else argv)
    if "--serve" not in argv:
        print("usage: python -m scms_sim_ref.api.isolate --serve", file=sys.stderr)
        return 2
    out_fd = os.dup(1)
    in_fd = os.dup(0)
    os.dup2(2, 1)
    devnull = os.open(os.devnull, os.O_RDONLY)
    os.dup2(devnull, 0)
    os.close(devnull)
    sys.stdout = sys.stderr
    # Buffered, for the reason given in `IsolatedCheck.spawn`: a raw `FileIO.write` may be short on
    # a pipe. `write_frame` flushes after every message, so lockstep is unaffected.
    with os.fdopen(out_fd, "wb") as out, os.fdopen(in_fd, "rb") as inp:
        return serve(inp, out)


if __name__ == "__main__":                               # pragma: no cover - process entry point
    raise SystemExit(main())
