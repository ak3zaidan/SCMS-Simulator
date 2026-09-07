"""Output files are hashed by streaming, once, and shared between the digest and the manifest.

`_file_sha256` was `h.update(fh.read())` -- the entire file into one `bytes` object -- and both
`_data_digest` and `_write_manifest` called it per file, so every output file was read whole, twice.
On the 8-hour reference run the largest output file is 997 MB, which made that single allocation the
biggest term in the run's peak working set and linear in run length
(`docs/realism/LONG-RUNS.md` section 1.2).
"""

import hashlib
import json
import os

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import _data_digest, _file_sha256


def test_streamed_hash_equals_the_whole_file_hash(tmp_path):
    p = tmp_path / "blob.bin"
    p.write_bytes(bytes(range(256)) * 40_000)        # > 10 MiB, several read blocks
    assert _file_sha256(str(p)) == hashlib.sha256(p.read_bytes()).hexdigest()


def test_the_file_is_read_once_per_run_not_twice(tmp_path, monkeypatch):
    """`_data_digest` fills the cache and `_write_manifest` consumes it: one pass per file."""
    reads = {"n": 0}
    real_open = open

    def counting_open(path, mode="r", *a, **kw):
        if "b" in mode and str(path).endswith(".jsonl"):
            reads["n"] += 1
        return real_open(path, mode, *a, **kw)

    out = str(tmp_path / "once")
    monkeypatch.setattr("builtins.open", counting_open)
    run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                out_dir=out))
    monkeypatch.undo()
    n_files = len(json.loads(open(os.path.join(out, "manifest.json"),
                                  encoding="utf-8").read())["outputs"])
    assert n_files > 5
    assert reads["n"] == n_files, (
        f"{reads['n']} binary reads for {n_files} output files -- each should be hashed once")


def test_the_cache_is_optional_and_correct(tmp_path):
    a = tmp_path / "a.txt"
    a.write_text("hello")
    cache: dict = {}
    first = _file_sha256(str(a), cache)
    assert cache == {str(a): first}
    a.write_text("goodbye")                          # cached value wins, as the one-run contract says
    assert _file_sha256(str(a), cache) == first
    assert _file_sha256(str(a)) != first             # ... and without a cache it re-reads


def test_data_digest_is_unchanged_by_the_chunking(tmp_path):
    """The digest is a hash of per-file hashes; chunked reads feed hashlib the same bytes."""
    out = str(tmp_path / "d")
    r = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=out))
    man = json.loads(open(os.path.join(out, "manifest.json"), encoding="utf-8").read())
    files = {o["path"]: os.path.join(out, o["path"]) for o in man["outputs"]}
    assert _data_digest(out, files) == r.data_digest == man["data_digest_sha256"]
    for o in man["outputs"]:
        whole = hashlib.sha256(open(os.path.join(out, o["path"]), "rb").read()).hexdigest()
        assert o["sha256"] == whole, o["path"]
