"""The traffic panel must measure TRAFFIC, not enforcement.

``ground_truth/gt_emissions_sample.jsonl`` is written inside the engine's broadcast pre-pass, whose
first statement is ``if enforced(tx, t): continue`` -- so a revoked vehicle's kinematic record ends
at revocation while the vehicle keeps driving. Over a full InTAS AM peak hour that deleted 56.22% of
the vehicle-steps, and because detection precision there is 0.308 it was mostly BENIGN vehicles
being deleted. The loss grows with run length and with the false-positive rate, so it cannot be
corrected by a constant, and at the 60-300 s durations the traffic panel was previously measured at
it was small enough to look like noise.

Three contracts are asserted here, and each one is the reason a specific number in
``docs/realism/PYTHON-ENGINE-VALIDATION.md`` had to be re-measured:

  * **the engine can emit an UN-ENFORCED mobility record** (``emit_mobility_oracle``) that is one
    row per active station per step regardless of revocation -- and switching it on adds exactly one
    file and moves no other byte, which is what keeps every pinned digest valid;
  * **survivorship is a published number**, unconditionally, in ``manifest["counts"]`` (outside
    ``data_digest`` by construction) and as first-class scorecard metrics;
  * **the panel prefers an unbiased source** and, when it cannot get one, degrades the
    density-dependent metrics to ``na`` rather than reporting a confidently wrong figure.

The ORACLE/MA firewall is asserted too: the un-enforced record is strictly more revealing than the
truncated one, so it must be unusable as a feature and unusable by an MA-visible consumer.
"""
from __future__ import annotations

import json
import os

import pytest

from scms_sim_ref.datagen import realism_bench as rb
from scms_sim_ref.datagen.leakage_linter import (LeakageViolation, assert_ma_visible,
                                                 find_forbidden_keys)
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

ORACLE_REL = os.path.join("ground_truth", "gt_mobility_oracle.jsonl")
EMIT_REL = os.path.join("ground_truth", "gt_emissions_sample.jsonl")


def _jsonl(path):
    with open(path, encoding="utf-8") as fh:
        return [json.loads(ln) for ln in fh if ln.strip()]


def _manifest(root):
    with open(os.path.join(root, "manifest.json"), encoding="utf-8") as fh:
        return json.load(fh)


def _flow_cfg(out, **over):
    """A small flow run that actually revokes vehicles, so truncation is observable at all."""
    kw = dict(seed=7, traffic_flow=True, road_network="grid", grid_w=5, grid_h=5, duration_s=60,
              arrival_rate=1.5, attacker_pct=0.25, emit_sample_prob=1.0, out_dir=str(out))
    kw.update(over)
    return PipelineConfig(**kw)


@pytest.fixture(scope="module")
def oracle_run(tmp_path_factory):
    """Flow (streamed) run WITH the un-enforced record."""
    out = tmp_path_factory.mktemp("orc") / "run"
    run_pipeline(_flow_cfg(out, emit_mobility_oracle=True))
    return str(out)


@pytest.fixture(scope="module")
def plain_run(tmp_path_factory):
    """The identical run WITHOUT it -- the byte-for-byte control."""
    out = tmp_path_factory.mktemp("plain") / "run"
    res = run_pipeline(_flow_cfg(out))
    return str(out), res


# ==================================================================================================
# 1. the engine's un-enforced mobility record
# ==================================================================================================
def test_opt_in_adds_exactly_one_file_and_moves_no_other_byte(oracle_run, plain_run):
    """The whole digest argument in one assertion.

    Every pinned golden in this repository is a sha256 over the dataset's files. An unconditional
    new stream would move all of them; a stream that changed one RNG draw would move them without
    even adding a file. So the contract is: same seed, same config plus one boolean -> every
    pre-existing file identical, exactly one file added.
    """
    import hashlib

    def files(root):
        out = {}
        for sub in ("ma", "ground_truth"):
            d = os.path.join(root, sub)
            for name in sorted(os.listdir(d)):
                with open(os.path.join(d, name), "rb") as fh:
                    out[f"{sub}/{name}"] = hashlib.sha256(fh.read()).hexdigest()
        return out

    plain_dir, _ = plain_run
    a, b = files(plain_dir), files(oracle_run)
    assert set(b) - set(a) == {"ground_truth/gt_mobility_oracle.jsonl"}, "added more than the record"
    assert not set(a) - set(b), "the opt-in removed a file"
    moved = {k for k in a if a[k] != b[k]}
    assert moved == set(), f"the opt-in perturbed existing output: {sorted(moved)}"


def test_default_run_writes_no_oracle_record(plain_run):
    plain_dir, _ = plain_run
    assert not os.path.exists(os.path.join(plain_dir, ORACLE_REL))
    outputs = {o["path"] for o in _manifest(plain_dir)["outputs"]}
    assert "ground_truth/gt_mobility_oracle.jsonl" not in outputs


def test_oracle_record_is_one_row_per_active_station_per_step(oracle_run):
    """Un-enforced means un-enforced: the row count IS the simulated vehicle-step count."""
    surv = _manifest(oracle_run)["counts"]["mobility_survivorship"]
    rows = _jsonl(os.path.join(oracle_run, ORACLE_REL))
    assert len(rows) == surv["vehicle_steps_simulated"] == surv["oracle_rows"]
    # ... and no (vehicle, t) is written twice, which is what "one row per station per step" means
    keys = {(r["true_vehicle_id"], r["t"]) for r in rows}
    assert len(keys) == len(rows)


def test_emission_stream_is_exactly_the_broadcasting_subset_of_the_oracle(oracle_run):
    """The two streams must agree wherever both exist, or the comparison below means nothing.

    This is the assertion that makes 'the emission stream is the same motion, truncated' a measured
    claim rather than a description: every emitted sample is present in the un-enforced record at
    the same instant with IDENTICAL kinematics, and the emitted set is precisely the subset the
    pre-pass let onto the air.
    """
    orc = _jsonl(os.path.join(oracle_run, ORACLE_REL))
    emi = _jsonl(os.path.join(oracle_run, EMIT_REL))
    by_key = {(r["true_vehicle_id"], r["t"]): r for r in orc}
    for e in emi:
        r = by_key.get((e["true_vehicle_id"], e["t"]))
        assert r is not None, f"emitted sample missing from the un-enforced record: {e['emit_id']}"
        assert (r["true_x"], r["true_y"], r["true_speed"], r["true_heading"]) == (
            e["true_x"], e["true_y"], e["true_speed"], e["true_heading"])
        assert r["broadcasting"] is True
    assert sum(1 for r in orc if r["broadcasting"]) == len(emi), (
        "emit_sample_prob=1.0: every broadcasting station-step must be an emitted sample")
    assert len(orc) > len(emi), "this run revoked nobody, so it cannot demonstrate truncation"


def test_the_missing_steps_belong_to_revoked_vehicles(oracle_run):
    """Attribution, not correlation: the rows the emission stream lacks are enforcement's."""
    orc = _jsonl(os.path.join(oracle_run, ORACLE_REL))
    revoked = {r["true_vehicle_id"] for r in
               _jsonl(os.path.join(oracle_run, "ground_truth", "gt_linkage_revocation.jsonl"))}
    silent = [r for r in orc if not r["broadcasting"]]
    assert silent, "no silenced station-steps in this run"
    assert {r["true_vehicle_id"] for r in silent} <= revoked, (
        "a station stopped broadcasting without being revoked")


def test_fixed_fleet_path_also_writes_the_record(tmp_path):
    """The non-streamed (fixed-fleet) branch writes it from memory; both branches are covered."""
    out = tmp_path / "ff"
    run_pipeline(PipelineConfig(out_dir=str(out), seed=7, n_vehicles=24, n_steps=40,
                                attacker_pct=0.25, emit_sample_prob=1.0,
                                emit_mobility_oracle=True))
    surv = _manifest(str(out))["counts"]["mobility_survivorship"]
    rows = _jsonl(os.path.join(str(out), ORACLE_REL))
    assert len(rows) == surv["vehicle_steps_simulated"] > 0
    assert [r["mob_id"] for r in rows] == sorted(r["mob_id"] for r in rows), "not canonically sorted"


# ==================================================================================================
# 2. survivorship is published whether or not the record is
# ==================================================================================================
def test_survivorship_counts_are_present_without_the_opt_in(plain_run):
    """`manifest["counts"]` is outside `data_digest`, so this costs no digest and is unconditional."""
    plain_dir, _ = plain_run
    s = _manifest(plain_dir)["counts"]["mobility_survivorship"]
    assert s["vehicle_steps_simulated"] > s["vehicle_steps_broadcast"] > 0
    assert s["vehicle_steps_survival_frac"] == pytest.approx(
        s["vehicle_steps_broadcast"] / s["vehicle_steps_simulated"], abs=1e-6)
    assert s["vehicles_revoked"] > 0 and s["oracle_record"] is None


def test_survivorship_is_identical_with_and_without_the_record(oracle_run, plain_run):
    """The tallies are a property of the RUN, not of what was written -- so they must not move."""
    plain_dir, _ = plain_run
    a = _manifest(plain_dir)["counts"]["mobility_survivorship"]
    b = _manifest(oracle_run)["counts"]["mobility_survivorship"]
    for k in ("vehicle_steps_simulated", "vehicle_steps_broadcast", "vehicle_steps_enforced_out",
              "vehicles", "vehicles_revoked", "vehicle_steps_survival_frac",
              "mean_record_span_s_revoked", "mean_record_span_s_never_revoked"):
        assert a[k] == b[k], f"{k} moved when the opt-in was switched on"


def test_revoked_vehicles_are_the_longer_trip_ones(plain_run):
    """Why the loss cannot be estimated from the surviving records, as a measured fact.

    A never-revoked vehicle is a SHORT-trip vehicle: exposure is what earns a false positive. So the
    obvious in-dataset estimator -- scale the revoked vehicles' records up to the never-revoked mean
    -- understates the loss, and it does so systematically rather than noisily. (Measured on the
    300 s grid reference run: 82.3 s simulated span for revoked vehicles against 68.2 s; on the
    InTAS peak hour the naive estimate reads 0.71 against a true 0.4378.)
    """
    plain_dir, _ = plain_run
    s = _manifest(plain_dir)["counts"]["mobility_survivorship"]
    assert s["mean_simulated_span_s_revoked"] > s["mean_simulated_span_s_never_revoked"]
    assert s["mean_record_span_s_revoked"] < s["mean_record_span_s_never_revoked"]


# ==================================================================================================
# 3. the harness: source resolution, survivorship metrics, and the degradation rule
# ==================================================================================================
def test_panel_prefers_the_unenforced_record_and_reports_full_survivorship(oracle_run):
    card = rb.scorecard(oracle_run, regime="urban")
    assert card["traffic_source"]["source"] == "oracle"
    assert card["traffic_source"]["truncated"] is False
    assert card["survivorship"]["vehicle_steps_survival_frac"] == 1.0
    assert card["survivorship"]["basis"] == "full_record"
    m = {x["id"]: x for x in card["panels"]["traffic"]}
    assert m["traffic.survivorship_vehicle_steps_frac"]["value"] == 1.0
    assert m["traffic.survivorship_vehicle_steps_frac"]["status"] == "pass"
    # every traffic row names its source, so no number in this panel is readable without it
    for row in card["panels"]["traffic"]:
        if row["id"] in ("traffic.survivorship_vehicle_steps_frac", "traffic.revoked_vehicle_frac"):
            continue
        assert row["details"]["mobility_source"] == "oracle"
        assert row["details"]["mobility_source_truncated"] is False


def test_the_unenforced_record_carries_more_traffic_than_the_broadcast_stream(oracle_run):
    """The same run, the same code path, two sources -- and the difference is enforcement."""
    unbiased = rb.scorecard(oracle_run, regime="urban", traffic_source="oracle")
    truncated = rb.scorecard(oracle_run, regime="urban", traffic_source="emissions")
    assert truncated["traffic_source"]["truncated"] is True
    assert truncated["survivorship"]["basis"] == "manifest_counts"
    assert truncated["survivorship"]["vehicle_steps_survival_frac"] < 1.0

    def seg(card):
        return {x["id"]: x for x in card["panels"]["traffic"]}["traffic.trace_segments"]["n"]

    assert seg(unbiased) > seg(truncated), "the un-enforced record must contain more motion"


def test_density_metrics_degrade_to_na_below_the_survivorship_floor(oracle_run, monkeypatch):
    """Below the floor the honest output is `na` PLUS the measured fraction, not a number.

    The floor is raised here rather than engineering a pathological dataset: the rule under test is
    'this fraction, against this threshold, withholds exactly these metrics', and a fixture that
    happens to sit at 0.886 tests it just as sharply from either side.
    """
    monkeypatch.setattr(rb, "SURVIVORSHIP_MIN_FRAC", 0.999)
    card = rb.scorecard(oracle_run, regime="urban", traffic_source="emissions")
    m = {x["id"]: x for x in card["panels"]["traffic"]}
    frac = card["survivorship"]["vehicle_steps_survival_frac"]
    for mid in rb.SURVIVORSHIP_GATED_METRICS:
        if mid in rb.SURVIVORSHIP_LOWER_BOUND_METRICS and m[mid]["status"] == "fail":
            continue                      # one-sided: see the next test
        assert m[mid]["status"] == "na", f"{mid} was published from a truncated stream"
        assert m[mid]["value"] is None
        assert "vehicle-steps" in m[mid]["reason"] and f"{frac:.4f}" in m[mid]["reason"]
    # ... and the per-sample distributional metrics are NOT withheld: they carry the number instead
    for mid in ("traffic.speed_p50_mps", "traffic.accel_within_hard_bound_frac",
                "traffic.moving_vehicle_frac"):
        assert m[mid]["status"] != "na", f"{mid} was withheld but is not density-dependent"
        assert m[mid]["details"]["vehicle_steps_survival_frac"] == frac
    # the survivorship metric itself is the loud one
    assert "traffic.survivorship_vehicle_steps_frac" in card["summary"]["soft_failures"]


def test_a_one_sided_count_that_already_fails_keeps_its_failure(oracle_run, monkeypatch):
    """Truncation removes vehicles, so it can only REMOVE co-presence events.

    That makes the truncated overlap count a valid LOWER BOUND: a value that already breaches a
    `<= 0` reference is a real breach, and the missing traffic can only make it worse (measured on
    the peak hour: 28 truncated against 169 unbiased). Withholding it would disarm a HARD CI gate on
    exactly the datasets that most need it, so the FAIL is kept and the number is labelled. A PASS
    from the same count is worthless and is still withheld -- which is asserted below by construction:
    the fail path and the na path are the same branch.
    """
    monkeypatch.setattr(rb, "SURVIVORSHIP_MIN_FRAC", 0.999)
    trunc = {x["id"]: x for x in
             rb.scorecard(oracle_run, regime="urban",
                          traffic_source="emissions")["panels"]["traffic"]}
    full = {x["id"]: x for x in
            rb.scorecard(oracle_run, regime="urban")["panels"]["traffic"]}
    ov_t, ov_f = trunc["traffic.overlap_events"], full["traffic.overlap_events"]
    assert ov_f["status"] == "fail" and ov_f["value"] > 0, "fixture has no overlaps to bound"
    assert ov_t["status"] == "fail", "a real overlap failure was withheld"
    assert "traffic.overlap_events" in rb.SURVIVORSHIP_LOWER_BOUND_METRICS
    assert "LOWER BOUND" in ov_t["details"]["survivorship_note"]
    assert ov_t["value"] <= ov_f["value"], "the truncated count is not a lower bound"


def test_a_full_survivorship_dataset_is_not_degraded(oracle_run):
    card = rb.scorecard(oracle_run, regime="urban")          # oracle source -> survivorship 1.0
    m = {x["id"]: x for x in card["panels"]["traffic"]}
    assert m["traffic.headway_ks_shifted_exponential"]["status"] != "na"
    assert m["traffic.overlap_events"]["status"] != "na"


def test_forcing_an_unavailable_source_is_an_error_not_a_silent_fallback(plain_run):
    """'Which stream did this number come from' is the question the mechanism exists to answer."""
    plain_dir, _ = plain_run
    with pytest.raises(FileNotFoundError, match="oracle"):
        rb.scorecard(plain_dir, traffic_source="oracle")


def test_a_legacy_dataset_with_heavy_revocation_is_still_withheld(oracle_run, tmp_path,
                                                                  monkeypatch):
    """The gap this closes: an OLD dataset cannot report survivorship, and must not go on publishing.

    `datasets/py_intas_hour` carries neither the un-enforced record nor the step-loop tallies, so the
    exact fraction is gone — but it revoked 61.38% of its vehicles, and its `fd_capacity` reads 763.6
    against the traffic's 1130.0. Without this rule such a dataset would keep publishing the wrong
    number under the new harness, which is the exact failure the whole change exists to stop. The
    `1 - revoked` proxy is not a bound in either direction, so it decides only whether to WITHHOLD
    and is never published as a value.
    """
    import shutil

    legacy = str(tmp_path / "legacy_run")
    shutil.copytree(oracle_run, legacy)
    os.remove(os.path.join(legacy, ORACLE_REL))                    # no un-enforced record ...
    with open(os.path.join(legacy, "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    man["counts"].pop("mobility_survivorship", None)               # ... and no tallies either
    with open(os.path.join(legacy, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(man, fh, indent=2)

    card = rb.scorecard(legacy, regime="urban")
    assert card["traffic_source"]["source"] == "emissions"
    assert card["survivorship"]["basis"] == "unmeasurable"
    m = {x["id"]: x for x in card["panels"]["traffic"]}
    rev = card["survivorship"]["revoked_vehicle_frac"]
    assert 1.0 - rev < rb.SURVIVORSHIP_MIN_FRAC, "fixture revokes too little to exercise the rule"
    hw = m["traffic.headway_ks_shifted_exponential"]
    assert hw["status"] == "na" and hw["value"] is None
    assert "NOT RECOVERABLE" in hw["reason"] and f"{rev:.4f}" in hw["reason"]
    # the proxy decides only whether to withhold -- it is never reported as a survivorship value
    assert m["traffic.survivorship_vehicle_steps_frac"]["value"] is None

    # ... and a legacy dataset that revoked almost nobody is NOT withheld
    monkeypatch.setattr(rb, "SURVIVORSHIP_MIN_FRAC", 0.5)
    relaxed = rb.scorecard(legacy, regime="urban")
    rm = {x["id"]: x for x in relaxed["panels"]["traffic"]}
    assert rm["traffic.headway_ks_shifted_exponential"]["status"] != "na"


def test_survivorship_is_unmeasurable_rather_than_estimated_on_a_legacy_dataset(tmp_path):
    """An older dataset carries neither the record nor the tallies. It must say so, not guess."""
    root = tmp_path / "legacy"
    os.makedirs(root / "ground_truth")
    os.makedirs(root / "ma")
    for name in ("gt_emissions_sample.jsonl", "gt_report_labels.jsonl"):
        (root / "ground_truth" / name).write_text("", encoding="utf-8")
    (root / "ma" / "ma_reports.jsonl").write_text("", encoding="utf-8")
    (root / "ground_truth" / "gt_vehicle.jsonl").write_text(
        "\n".join(json.dumps({"true_vehicle_id": f"veh_{i:03d}"}) for i in range(10)) + "\n",
        encoding="utf-8")
    (root / "ground_truth" / "gt_linkage_revocation.jsonl").write_text(
        "\n".join(json.dumps({"true_vehicle_id": f"veh_{i:03d}"}) for i in range(4)) + "\n",
        encoding="utf-8")
    (root / "manifest.json").write_text(json.dumps(
        {"dataset_version": "0.3.0", "seed": 1, "counts": {},
         "generator": "scms_sim_ref.mock_pipeline (pre-MOSAIC reference, realistic v2)",
         "config": {"emit_sample_prob": 1.0, "road_network": "grid", "dt": 1.0}}), encoding="utf-8")
    card = rb.scorecard(str(root))
    s = card["survivorship"]
    assert s["basis"] == "unmeasurable" and s["vehicle_steps_survival_frac"] is None
    assert s["revoked_vehicle_frac"] == pytest.approx(0.4)   # still exact: two file lengths
    m = {x["id"]: x for x in card["panels"]["traffic"]}
    assert m["traffic.survivorship_vehicle_steps_frac"]["status"] == "na"
    assert "not recoverable" in m["traffic.survivorship_vehicle_steps_frac"]["reason"]


# ==================================================================================================
# 4. the frozen-trace source
# ==================================================================================================
def _write_trace(path, *, dt=0.5, n_steps=6):
    """A minimal `scms-sumo-trace/1` artifact: two vehicles driving due EAST at 10 m/s.

    Due east is SUMO angle 90 (degrees clockwise from North), which must come back as heading 0
    (degrees counter-clockwise from East) -- the conversion this fixture exists to pin.
    """
    lines = ["#" + rb.TRACE_FORMAT,
             "#meta " + json.dumps({"dt": dt, "step0_sim_time": dt, "steps": n_steps},
                                   sort_keys=True, separators=(",", ":")),
             "#vehicles 2",
             f"V 0 carA 0 {n_steps - 1} 0.000 0.000 0 0.000",
             f"V 1 carB 0 {n_steps - 1} 0.000 0.000 0 0.000",
             f"#rows {2 * n_steps}"]
    for k in range(n_steps):
        for idx in (0, 1):
            x = 10.0 * k * dt + idx * 40.0
            lines.append(f"{k} {idx} {x:.3f} {idx * 3.2:.3f} 10.000 90.000")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return str(path)


def test_read_sumo_trace_converts_time_and_heading(tmp_path):
    rows = rb.read_sumo_trace(_write_trace(tmp_path / "t.trace", dt=0.5, n_steps=4))
    assert len(rows) == 8
    assert {r["true_vehicle_id"] for r in rows} == {"carA", "carB"}
    # the engine labels trace step k as t = k*dt, so the panel and the dataset share a clock
    assert sorted({r["t"] for r in rows}) == [0.0, 0.5, 1.0, 1.5]
    # SUMO 90 deg clockwise-from-North == due east == 0 deg counter-clockwise-from-East
    assert all(r["true_heading"] == pytest.approx(0.0) for r in rows)
    assert all(r["true_speed"] == pytest.approx(10.0) for r in rows)


def test_read_sumo_trace_rejects_a_file_that_is_not_one(tmp_path):
    p = tmp_path / "nope.trace"
    p.write_text("hello\n", encoding="utf-8")
    with pytest.raises(ValueError, match=rb.TRACE_FORMAT):
        rb.read_sumo_trace(str(p))


def test_the_pinned_trace_is_preferred_over_the_truncated_stream(oracle_run, tmp_path):
    """A replay dataset can be re-measured from the mobility it replayed, with no re-run."""
    trace = _write_trace(tmp_path / "pinned.trace", dt=1.0, n_steps=40)
    card = rb.scorecard(oracle_run, regime="urban", traffic_source="trace", sumo_trace=trace)
    assert card["traffic_source"]["source"] == "trace"
    assert card["traffic_source"]["path"] == trace
    assert card["survivorship"]["basis"] == "full_record"
    assert card["survivorship"]["vehicle_steps_survival_frac"] == 1.0


def test_resolution_order_is_oracle_then_trace_then_emissions(oracle_run, plain_run, tmp_path):
    trace = _write_trace(tmp_path / "ord.trace")
    probe = rb.probe_dataset(oracle_run)
    assert rb.resolve_mobility_source(oracle_run, probe, sumo_trace=trace)["source"] == "oracle"
    plain_dir, _ = plain_run
    pprobe = rb.probe_dataset(plain_dir)
    assert rb.resolve_mobility_source(plain_dir, pprobe, sumo_trace=trace)["source"] == "trace"
    assert rb.resolve_mobility_source(plain_dir, pprobe)["source"] == "emissions"


def test_auto_never_picks_up_the_manifests_trace_implicitly(plain_run, tmp_path):
    """A scorecard must not depend on whether a host-local absolute path happens to exist.

    `manifest["config"]["sumo_trace"]` is a path on whatever machine froze the artifact. If `auto`
    honoured it, the same dataset would score one way on the machine that produced it and another
    way anywhere else -- silently, and in a module whose contract is determinism. So the pin is
    followed only when the caller asks for the trace by name or by `--traffic-source trace`.
    """
    plain_dir, _ = plain_run
    trace = _write_trace(tmp_path / "pinned.trace")
    with open(os.path.join(plain_dir, "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    man["config"]["sumo_trace"] = trace                   # as a sumo_replay run would record it
    with open(os.path.join(plain_dir, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(man, fh, indent=2)
    probe = rb.probe_dataset(plain_dir)
    assert probe["config"]["sumo_trace"] == trace and os.path.isfile(trace)
    assert rb.resolve_mobility_source(plain_dir, probe)["source"] == "emissions"
    assert rb.resolve_mobility_source(plain_dir, probe, prefer="trace")["source"] == "trace"


# ==================================================================================================
# 5. the ORACLE / MA-visible firewall
# ==================================================================================================
def test_the_unenforced_record_is_oracle_and_can_never_become_a_feature(oracle_run):
    """It is STRICTLY more revealing than the truncated stream, so the firewall has to hold harder.

    It carries a real vehicle id and true kinematics for vehicles the MA has already revoked -- i.e.
    exactly the motion an MA-side consumer is not entitled to. Every row must therefore be tagged
    ORACLE and every row must be rejected by the MA-visible linter.
    """
    rows = _jsonl(os.path.join(oracle_run, ORACLE_REL))
    assert rows
    for r in rows[:200]:
        assert r["_visibility"] == "ORACLE"
        assert find_forbidden_keys(r), "an un-enforced mobility row must trip the leakage linter"
        with pytest.raises(LeakageViolation):
            assert_ma_visible(r, context="gt_mobility_oracle")


def test_the_record_lives_only_under_ground_truth(oracle_run):
    assert os.path.isfile(os.path.join(oracle_run, ORACLE_REL))
    for sub in ("ma", "ml"):
        d = os.path.join(oracle_run, sub)
        if os.path.isdir(d):
            assert "gt_mobility_oracle.jsonl" not in os.listdir(d)


def test_featurize_does_not_touch_the_record_even_when_it_is_present(oracle_run):
    """The `ml/` half of the firewall, asserted end to end rather than by directory listing.

    `featurize.build` loads NAMED ground-truth files; it never globs `ground_truth/*.jsonl`. That is
    what structurally keeps a new ORACLE stream out of the feature tables, and this pins it: build a
    feature set over a dataset that HAS the record, and lint every emitted frame.
    """
    from scms_sim_ref.datagen import featurize
    from scms_sim_ref.datagen.leakage_linter import lint_feature_frame

    import csv

    featurize.build(oracle_run, split_seed=5)
    ml = os.path.join(oracle_run, "ml")
    assert os.path.isdir(ml), "featurize wrote no feature tables"
    assert "gt_mobility_oracle.jsonl" not in os.listdir(ml)
    # FEATURE tables only. The *_labels tables carry ground truth by design (that is what a label
    # is); the invariant under test is that no un-enforced kinematic value reached a FEATURE.
    checked = 0
    for name in ("report_features.csv", "subject_features.csv", "vehicle_features.csv",
                 "vehicle_features_ma.csv"):
        p = os.path.join(ml, name)
        if not os.path.isfile(p):
            continue
        with open(p, encoding="utf-8", newline="") as fh:
            rows = list(csv.DictReader(fh))
        lint_feature_frame(rows, context=name)         # raises on any forbidden column
        checked += 1
    assert checked, "no feature table was linted"


def test_a_scorecard_read_from_the_oracle_leaks_nothing(oracle_run):
    """The panel reads ORACLE data -- like calibration.py -- and publishes only aggregates."""
    card = rb.scorecard(oracle_run, regime="urban")
    blob = json.loads(json.dumps(card, default=str))
    for panel in card["panels"].values():
        for row in panel:
            assert find_forbidden_keys(row.get("details") or {}) == [], row["id"]
    assert "veh_0" not in json.dumps(blob["panels"]), "a per-entity id reached the scorecard"
