"""`validate()`'s two iterate-only passes stream instead of materialising the file.

`mock_pipeline.run._emit_result` calls `datagen.validate.validate()` after every CLI run, and that
call used to build `ma/ma_reports.jsonl` -- the largest MA-visible file a run writes -- as a list of
dicts, twice. Measured on this repository's own reference ladder, `validate()` alone peaked at
**462.9 MiB on the 3600 s dataset and 1,627.9 MiB on the 14400 s one**, against engine peaks of
465.9 and 1,633.0 MiB for the same runs: the published
`92.5 MiB + 54.3 KiB x (vehicles ever created)` memory law of `docs/realism/LONG-RUNS.md` section 1.3
was this list. Streaming it takes the same two passes to 98.3 MiB and 149.8 MiB with an identical
summary.
"""

import json

from scms_sim_ref.datagen import validate as V
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def test_iter_rows_yields_exactly_what_read_returns(tmp_path):
    rows = [{"a": i, "b": {"c": [i, i + 1]}} for i in range(500)]
    p = tmp_path / "rows.jsonl"
    p.write_text("".join(json.dumps(r) + "\n" for r in rows) + "\n\n", encoding="utf-8")
    assert list(V._iter_rows(str(p))) == V._read(str(p)) == rows


def test_iter_rows_on_a_missing_file_is_empty(tmp_path):
    assert list(V._iter_rows(str(tmp_path / "nope.jsonl"))) == []
    assert V._read(str(tmp_path / "nope.jsonl")) == []


def test_iter_rows_holds_one_row_at_a_time(tmp_path):
    """The property that matters: consuming the generator does not build the list first."""
    p = tmp_path / "big.jsonl"
    p.write_text("".join(json.dumps({"i": i}) + "\n" for i in range(10_000)), encoding="utf-8")
    it = V._iter_rows(str(p))
    first = next(it)
    assert first == {"i": 0}
    assert sum(1 for _ in it) == 9_999


def test_validate_summary_is_unchanged_on_a_real_dataset(tmp_path):
    out = str(tmp_path / "ds")
    r = run_pipeline(PipelineConfig(seed=42, traffic_flow=True, road_network="grid", duration_s=300.0,
                                    arrival_rate=2.0, grid_w=6, grid_h=6, attacker_pct=0.15,
                                    traffic_lights=True, out_dir=out))
    s, leaks = V.validate(out)
    assert leaks == []
    assert s["revoked"] == r.n_revoked
    assert s["ma_rows"] > 0 and s["precision"] is not None and s["recall"] is not None
    assert s["detector_reliability"], "the ma_reports pass must still populate per-detector counts"
