"""Config save/load: a saved config (or a run's manifest) replays byte-for-byte."""

import json

from scms_sim_ref.mock_pipeline import (PipelineConfig, config_from_dict, run_pipeline)
from scms_sim_ref.mock_pipeline import run as runmod


def _cfg(tmp, **over):
    kw = dict(seed=7, n_vehicles=20, n_steps=25, attacker_pct=0.2, weather="rain",
              attack_types=("ConstPos", "RandomSpeed"))
    kw.update(over)
    return PipelineConfig(out_dir=str(tmp), **kw)


def test_dict_roundtrip_preserves_tuple_fields():
    cfg = _cfg("x")
    d = {k: (list(v) if isinstance(v, tuple) else v) for k, v in cfg.__dict__.items()}
    d = json.loads(json.dumps(d))                       # through JSON: tuples -> lists
    back = config_from_dict(d)
    assert isinstance(back.attack_types, tuple), "list must be coerced back to tuple"
    assert back.attack_types == cfg.attack_types
    assert back.__dict__ == cfg.__dict__, "round-trip must be lossless"


def test_config_from_manifest_replays_byte_identical(tmp_path):
    a = run_pipeline(_cfg(tmp_path / "a"))
    manifest = json.loads((tmp_path / "a" / "manifest.json").read_text())
    # rebuild from the saved manifest (nested "config"), point at a fresh dir, and rerun
    cfg2 = config_from_dict(manifest)
    cfg2.out_dir = str(tmp_path / "b")
    b = run_pipeline(cfg2)
    assert a.data_digest == b.data_digest, "replay from manifest must reproduce the dataset exactly"


def test_config_from_dict_ignores_unknown_keys(capsys):
    cfg = config_from_dict({"seed": 3, "n_vehicles": 9, "not_a_real_field": 123})
    assert cfg.seed == 3 and cfg.n_vehicles == 9
    assert "not_a_real_field" in capsys.readouterr().err


def test_dump_config_then_reload(tmp_path):
    cfg = _cfg(tmp_path / "run", n_lanes=2, traffic_lights=True)
    path = tmp_path / "cfg.json"
    runmod._dump_config(cfg, str(path))
    back = config_from_dict(json.loads(path.read_text()))
    assert back.n_lanes == 2 and back.traffic_lights is True
    assert back.__dict__ == cfg.__dict__


def test_config_schema_covers_all_fields_and_is_json_safe():
    import json
    from scms_sim_ref.mock_pipeline import config_schema, PipelineConfig
    import dataclasses

    sch = config_schema()
    names = {f.name for f in dataclasses.fields(PipelineConfig)}
    assert set(sch) == names, "schema must list every config field"
    for name, meta in sch.items():
        assert "type" in meta and "default" in meta
    json.dumps(sch)                                  # must be JSON-serializable (no tuples/MISSING)
    # a few spot checks
    assert sch["attack_types"]["default"] == list(PipelineConfig().attack_types)
    assert sch["seed"]["default"] == PipelineConfig().seed


def test_cli_dump_config_schema(tmp_path):
    import json
    from scms_sim_ref.mock_pipeline.run import main
    path = tmp_path / "schema.json"
    assert main(["--dump-config-schema", str(path)]) == 0
    sch = json.loads(path.read_text())
    assert "attack_duty_cycle" in sch and "od_model" in sch and "turn_slowdown" in sch


def test_cli_check_config_valid_and_invalid(tmp_path):
    import json
    from scms_sim_ref.mock_pipeline.run import main
    good = tmp_path / "good.json"
    good.write_text(json.dumps({"seed": 5, "n_vehicles": 10, "weather": "rain"}))
    assert main(["--check-config", str(good)]) == 0
    bad = tmp_path / "bad.json"
    bad.write_text(json.dumps({"weather": "hail"}))          # invalid enum -> validate_config raises
    assert main(["--check-config", str(bad)]) == 1
    missing = tmp_path / "nope.json"
    assert main(["--check-config", str(missing)]) == 1       # unreadable -> 1, not a crash


def test_cli_config_flag_runs(tmp_path):
    src = _cfg(tmp_path / "src")
    cfgpath = tmp_path / "c.json"
    runmod._dump_config(src, str(cfgpath))
    rc = runmod.main(["--config", str(cfgpath), "--out", str(tmp_path / "out")])
    assert rc == 0
    assert (tmp_path / "out" / "manifest.json").exists()
