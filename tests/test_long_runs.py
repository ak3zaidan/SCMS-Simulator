"""What has to hold for a LONG run to MEAN something.

`docs/realism/LONG-RUNS.md` measured a duration ladder from 300 s to 28,800 s on this repository's
own reference arm and found that four of the things a long run exists to exercise either could not
be measured or did not exist. This file pins the fixes, one section per defect, and every assertion
here is the machine-checkable form of a number in that document.

  1. **A count whose denominator is a sampling cap is not a measurement.** `MAX_TIME_BUCKETS` and
     `HEADWAY_MAX_INSTANTS` are 240, so `traffic.overlap_events` -- a HARD gate -- read 434 at 300 s
     and 319 at 28,800 s while the trajectory sample behind it grew 109x. The count is kept (it is a
     valid one-sided existence test at any coverage) but it now publishes its coverage, and a RATE
     row per co-present pair EXAMINED is published beside it. The rate is what may be compared
     across durations.
  2. **An awareness metric is not trustworthy merely because it exists.** `MIN_SAMPLES = 30` gates
     whether the awareness rows appear; between the duration where they appear (~930 s) and 8 h
     `awareness_ratio_100m` moves 3.8x and `comm.pdr_gray_zone_width_m` -- the only GRADED row --
     flips FAIL at 14,400 s / pass at every other rung, so the run length alone decided a verdict.
     Below `CURVE_MIN_LINKS_PER_BIN` links per populated distance bin the value and the verdict are
     withheld with the shortfall, and the duration this dataset's own link rate would need, stated.
  3. **Certificate expiry did not exist.** `ma_cert_status` was written `[0.0, total_time]` for
     100.0% of rows at every one of the seven rungs. `cert_validity_s` gives a certificate a real
     validity window that does not depend on `--duration`.
  4. **No persistent adversary existed.** `attack_to = spawn_time + life`, so the attack-span
     distribution was stationary from 1800 s at p50 256 s / max 598 s and did not move at 8 h.
     `persistent_attacker_pct` lets an identity re-enter across trips for a campaign of hours.
  5. **`--demand rush` / `--demand night` had no clock.** The profile was evaluated at
     `tt / total_time`, so the "AM peak" sat at 75 s in a 300 s run and 225 s in a 900 s one and two
     durations were two different scenarios. `demand_period_s` puts it on absolute simulated time.

Every fix is OPT-IN wherever it would move a pinned digest, and the byte-identity of the default
arm is asserted here as well as in the golden tests.
"""
from __future__ import annotations

import json
import math
import os
import random

import pytest

from scms_sim_ref.datagen import realism_bench as rb
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import validate_config

# The reference arm of docs/realism/LONG-RUNS.md, shrunk to a size a unit test can afford. The
# SHAPE of every finding below is a property of the engine, not of the grid size.
FLOW = dict(seed=42, traffic_flow=True, road_network="grid", grid_w=4, grid_h=4,
            arrival_rate=2.0, attacker_pct=0.2, traffic_lights=True)


def _jsonl(d, rel):
    with open(os.path.join(d, rel), encoding="utf-8") as fh:
        return [json.loads(ln) for ln in fh]


def _row(card, mid):
    for m in card["panels"]["traffic"] + card["panels"]["comm"]:
        if m["id"] == mid:
            return m
    raise KeyError(mid)


# ==================================================================================================
# fixtures for the harness half (no pipeline run: synthetic emissions with a known answer)
# ==================================================================================================
def _write_jsonl(path, rows):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        for r in rows:
            fh.write(json.dumps(r, sort_keys=True) + "\n")


def _dataset(root, emissions, reports=None, labels=None, config=None):
    cfg = {"emit_sample_prob": 1.0, "road_network": "linear", "n_lanes": 1, "dt": 1.0,
           "radio_range_m": 250.0, "art_max_m": 150.0}
    cfg.update(config or {})
    _write_jsonl(os.path.join(root, "ground_truth", "gt_emissions_sample.jsonl"), emissions)
    _write_jsonl(os.path.join(root, "ground_truth", "gt_report_labels.jsonl"), labels or [])
    _write_jsonl(os.path.join(root, "ma", "ma_reports.jsonl"), reports or [])
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump({"dataset_version": "0.3.0",
                   "generator": "scms_sim_ref.mock_pipeline (pre-MOSAIC reference, realistic v2)",
                   "seed": 1, "config": cfg}, fh, indent=2)
    return root


def _overlapping_platoon(n_steps, n_veh=4, overlap_every=7, overlap_phase=0):
    """A platoon in which one pair overlaps at a FIXED RATE (every `overlap_every` instants).

    The whole point of the fixture: the underlying overlap RATE per co-present pair is a constant of
    the scenario, so any honest estimator of it must read the same number at 100 instants and at
    1,000. A raw count cannot, once the scan is capped.

    `overlap_every=7` is COPRIME with the sub-sampling strides the tests below use, so no result in
    the DEFECT 1 section is an aliasing artefact of the sampler.

    THAT DEFAULT USED TO BE THE WHOLE PROBLEM WITH THIS FILE. Choosing a period coprime with the
    stride is choosing not to test the sampler, and the failure mode the real signalised arm
    actually exhibits -- a period that DIVIDES the stride -- was therefore never exercised here. The
    DEFECT 1b section below passes `overlap_every` and `overlap_phase` explicitly to build exactly
    that case, and it fails against the old fixed-stride rule.
    """
    rows = []
    for step in range(n_steps):
        t = float(step)
        for j in range(n_veh):
            x = -j * 60.0 + 12.0 * t
            if j == 1 and step % overlap_every == overlap_phase:
                x = -0.0 * 60.0 + 12.0 * t        # sits exactly on vehicle 0
            rows.append({"t": t, "true_vehicle_id": f"veh_{j:03d}", "true_x": round(x, 3),
                         "true_y": 0.0, "claimed_x": round(x, 3), "claimed_y": 0.0,
                         "claimed_speed": 12.0, "pos_conf": 5.0, "is_attacker": False,
                         "is_faulty": False, "falsified": False})
    return rows


# ==================================================================================================
# DEFECT 1 -- a count over a capped scan is not comparable; the rate is
# ==================================================================================================
def test_overlap_count_saturates_at_the_cap_while_the_rate_does_not(tmp_path):
    """The measured shape of docs/realism/LONG-RUNS.md section 3.3, reproduced in miniature.

    Two runs of the SAME scenario, one 5x longer. The traffic behind the metric grows 5x; the count
    does not grow with it (the scan is capped at MAX_TIME_BUCKETS instants) while the rate per
    examined pair is the constant it should be.
    """
    short = rb.scorecard(_dataset(str(tmp_path / "s"), _overlapping_platoon(rb.MAX_TIME_BUCKETS)))
    long_ = rb.scorecard(_dataset(str(tmp_path / "l"),
                                  _overlapping_platoon(rb.MAX_TIME_BUCKETS * 5)))
    c_s, c_l = _row(short, "traffic.overlap_events"), _row(long_, "traffic.overlap_events")
    r_s, r_l = (_row(short, "traffic.overlap_rate_per_1k_pair_instants"),
                _row(long_, "traffic.overlap_rate_per_1k_pair_instants"))

    # the scan really is capped, and the coverage really does fall as 1/duration
    assert c_s["details"]["instants_examined"] == c_l["details"]["instants_examined"] == 240
    assert c_s["details"]["instants_available"] == 240
    assert c_l["details"]["instants_available"] == 1200
    assert c_s["details"]["coverage_frac"] == 1.0
    assert c_l["details"]["coverage_frac"] == pytest.approx(0.2)

    # THE DEFECT: 5x the traffic, and the count does not move at all
    assert c_l["value"] == c_s["value"]
    # THE FIX: the rate is the same number both times -- which is the truth about this scenario
    assert r_l["value"] == pytest.approx(r_s["value"], rel=0.02)
    # ... and the extrapolated whole-run magnitude DOES grow with the run, as a reader expects
    assert (r_l["details"]["estimated_run_total"]
            == pytest.approx(5 * r_s["details"]["estimated_run_total"], rel=0.05))


def test_a_capped_count_says_out_loud_that_it_cannot_be_compared(tmp_path):
    """No consumer can now read one of these counts without its denominator beside it."""
    card = rb.scorecard(_dataset(str(tmp_path / "c"),
                                 _overlapping_platoon(rb.MAX_TIME_BUCKETS * 3)))
    d = _row(card, "traffic.overlap_events")["details"]
    assert d["instants_examined"] == 240 and d["instants_available"] == 720
    assert d["coverage_frac"] == pytest.approx(1 / 3, abs=1e-4)
    assert d["cap"] == rb.MAX_TIME_BUCKETS
    assert "NOT COMPARABLE ACROSS DURATIONS" in d["comparability"]
    assert d["pair_instants_examined"] < d["pair_instants_available"]
    # a run short enough to be examined in full carries no such warning
    full = rb.scorecard(_dataset(str(tmp_path / "f"), _overlapping_platoon(60)))
    assert "comparability" not in _row(full, "traffic.overlap_events")["details"]


def test_the_ci_line_for_a_capped_count_carries_its_coverage(tmp_path):
    """The CI line is where one run's count is most likely to be compared with another's."""
    card = rb.scorecard(_dataset(str(tmp_path / "ci"),
                                 _overlapping_platoon(rb.MAX_TIME_BUCKETS * 3)))
    line = [x for x in rb.hard_failures(card) if "overlap_events" in x]
    assert line, rb.hard_failures(card)
    assert "over 240 of 720 instants" in line[0]
    assert "NOT comparable with another duration as a count" in line[0]
    # a fully examined run says nothing extra
    full = rb.scorecard(_dataset(str(tmp_path / "ci2"), _overlapping_platoon(60)))
    assert all("instants," not in x for x in rb.hard_failures(full))


def test_max_instants_zero_removes_the_cap_and_the_count_becomes_the_run_s(tmp_path):
    """`--max-instants 0` is the escape hatch for a run long enough to afford the full scan."""
    root = _dataset(str(tmp_path / "u"), _overlapping_platoon(rb.MAX_TIME_BUCKETS * 4))
    capped = _row(rb.scorecard(root), "traffic.overlap_events")
    full = _row(rb.scorecard(root, max_instants=0), "traffic.overlap_events")
    assert capped["details"]["instants_examined"] == 240
    assert full["details"]["instants_examined"] == full["details"]["instants_available"] == 960
    assert full["value"] > capped["value"]
    # and the capped RATE was an unbiased estimate of the uncapped truth all along
    est = _row(rb.scorecard(root), "traffic.overlap_rate_per_1k_pair_instants")
    assert est["details"]["estimated_run_total"] == pytest.approx(full["value"], rel=0.10)


def test_the_default_cap_is_unchanged_so_no_historical_count_moves(tmp_path):
    """`max_instants=None` means the module default, NOT 'unlimited'.

    The overlap row is a HARD gate with pinned expectations across the corpus; silently widening its
    scan would have re-scored every dataset in the repository.
    """
    root = _dataset(str(tmp_path / "d"), _overlapping_platoon(1000))
    assert (_row(rb.scorecard(root), "traffic.overlap_events")["value"]
            == _row(rb.scorecard(root, max_instants=rb.MAX_TIME_BUCKETS),
                    "traffic.overlap_events")["value"])


def test_teleports_get_a_rate_too_and_it_pairs_with_the_count(tmp_path):
    """The teleport scan is uncapped, but its count still grows with the data; the rate does not."""
    emis = [{"t": 50.0 * k, "true_vehicle_id": "veh_000", "true_x": 500_000.0 * k, "true_y": 0.0,
             "claimed_x": 0.0, "claimed_y": 0.0, "claimed_speed": 1.0, "pos_conf": 5.0,
             "is_attacker": False, "is_faulty": False, "falsified": False} for k in range(60)]
    card = rb.scorecard(_dataset(str(tmp_path / "t"), emis, config={"emit_sample_prob": 0.02}))
    cnt, rate = _row(card, "traffic.teleport_events"), _row(card, "traffic.teleport_rate_per_1k_pairs")
    assert cnt["value"] == 59 and cnt["status"] == "fail" and cnt["severity"] == rb.HARD
    # every pair teleports -> 1000 per 1000 pairs
    assert rate["value"] == pytest.approx(1000.0) and rate["status"] == "fail"
    assert rate["severity"] == rb.SOFT          # the HARD gate stays on the count, reported once
    assert card["summary"]["hard_failures"] == ["traffic.teleport_events"]


def test_the_scorecard_names_its_sampling_caps_at_the_top(tmp_path):
    card = rb.scorecard(_dataset(str(tmp_path / "s"), _overlapping_platoon(60)))
    s = card["settings"]
    assert s["max_time_buckets"] == rb.MAX_TIME_BUCKETS
    assert s["headway_max_instants"] == rb.HEADWAY_MAX_INSTANTS
    assert s["curve_min_links_per_bin"] == rb.CURVE_MIN_LINKS_PER_BIN
    assert s["max_instants"] is None                        # i.e. the module defaults are in force


def test_headway_rows_carry_the_coverage_of_their_own_capped_scan(tmp_path):
    card = rb.scorecard(_dataset(str(tmp_path / "h"), _overlapping_platoon(rb.MAX_TIME_BUCKETS * 2)))
    d = _row(card, "traffic.headway_ks_shifted_exponential")["details"]
    assert d["instants_examined"] == rb.HEADWAY_MAX_INSTANTS
    assert d["instants_available"] > d["instants_examined"]
    # The row states the condition under which a distribution survives the cap. The condition is
    # UNIFORM INCLUSION PROBABILITY, not even spacing: the old wording claimed the latter and was
    # measured false (see DEFECT 1b), so the note must no longer offer even spacing as the reason.
    note = d["distributional_note"]
    assert "same probability of being sampled" in note
    assert "EVEN SPACING ALONE DOES NOT GIVE THAT" in note


# ==================================================================================================
# DEFECT 1b -- the RATE was still wrong, because the SAMPLER was phase-locked
#
# The rate row above divides by the sub-sample it was measured over, so it is duration-invariant BY
# CONSTRUCTION. It was still duration-DEPENDENT IN FACT, because the sub-sample was not a
# uniform-probability sample of the run: `_even_subsample` took a FIXED STRIDE, a fixed stride is a
# Dirac comb, and a comb aliases against any periodic structure whose period divides it. On the
# reference grid arm `light_cycle_s = 24 s` makes the true overlap rate periodic with period 12 s
# while the stride is 7.5/15/30/60 s at 1800/3600/7200/14400 s, so the number of distinct signal
# phases sampled was 12/gcd(stride, 12) = 8/4/2/1: at 14,400 s ALL 240 sampled instants sat at ONE
# PHASE and the published rate read -38.6% against the uncapped truth.
#
# Every test in this section uses a period that DIVIDES the stride -- i.e. exactly the case the
# `overlap_every=7` default above was chosen to avoid -- and each one is red against the old rule.
# ==================================================================================================
def _legacy_sampler(items, max_items, *, stream=None, seed=None):
    """The withdrawn fixed-stride rule, in the current signature, for monkeypatching.

    Delegates to `rb._fixed_stride_subsample`, which is the byte-for-byte old implementation kept in
    the module for this purpose, so these tests measure the REAL previous behaviour and not a
    re-typed approximation of it.
    """
    return rb._fixed_stride_subsample(items, max_items)


def test_the_old_sampler_saw_one_signal_phase_and_the_new_one_sees_them_all():
    """The mechanism, on the index arithmetic alone, before any dataset is involved.

    1,440 instants capped to 240 is a stride of 6. A structure of period 12 -- what a 24 s signal
    cycle does to an overlap rate -- is then visible at exactly 12/gcd(6, 12) = 2 of its 12 phases,
    whatever the cap is raised to while the stride stays a multiple of the period.
    """
    items = list(range(1440))
    old = rb._fixed_stride_subsample(items, 240)
    assert sorted({j % 12 for j in old}) == [0, 6]          # 2 of 12 phases, and always the same 2

    new = rb._subsample(items, 240, stream="overlap")
    assert len(new) == 240 and new == sorted(set(new))      # still 240 distinct instants, in order
    assert len({j % 12 for j in new}) == 12                 # ... spread over every phase
    # and it is still an EVEN sweep of the run, not a clump: exactly one index per stride window
    assert all(int(i * 6) <= new[i] < int((i + 1) * 6) + 1 for i in range(240))


def test_the_sampler_inclusion_probability_is_uniform_over_every_instant():
    """The property that makes the pooled distribution unbiased, measured rather than asserted.

    Jittered systematic sampling selects item m iff some window's uniform draw lands in [m, m+1);
    the windows tile [0, n) and each draw is uniform over its own window of width s = n/k, so
    P(m) = 1/s = k/n for EVERY m, whatever the population does. Over 400 seeds the observed
    frequency of every one of the 1,440 instants sits inside a 4-sigma binomial band around 240/1440.
    """
    n, k, trials = 1440, 240, 400
    seen = [0] * n
    for sd in range(trials):
        for j in rb._subsample(list(range(n)), k, stream="p", seed=sd):
            seen[j] += 1
    p = k / n
    sd_band = 4.0 * math.sqrt(p * (1 - p) / trials)
    lo, hi = p - sd_band, p + sd_band
    assert all(lo <= c / trials <= hi for c in seen), (min(seen) / trials, max(seen) / trials)
    # the same draws, read as PHASES of a period-12 structure: flat, where the old rule had 2 of 12
    phase = [0] * 12
    for sd in range(trials):
        for j in rb._subsample(list(range(n)), k, stream="p", seed=sd):
            phase[j % 12] += 1
    share = [c / (trials * k) for c in phase]
    assert all(abs(s - 1 / 12) < 0.004 for s in share), share


def test_a_period_that_divides_the_stride_makes_the_old_sampler_publish_zero(tmp_path, monkeypatch):
    """THE REGRESSION. 1,200 instants capped to 240 is a stride of 5; the overlaps recur every 5
    instants at phase 1, so the fixed stride lands on phase 0 every single time and NEVER SEES ONE.

    The dataset really does overlap 240 times. The old rule publishes 0 -- and `overlap_events` is a
    HARD gate, so this is a one-sided existence test reporting "no overlap ever happened" about a
    dataset in which one happens every five seconds.
    """
    root = _dataset(str(tmp_path / "phase"),
                    _overlapping_platoon(1200, overlap_every=5, overlap_phase=1))
    truth = _row(rb.scorecard(root, max_instants=0), "traffic.overlap_rate_per_1k_pair_instants")
    assert truth["details"]["instants_examined"] == 1200
    assert truth["value"] == pytest.approx(1000.0 * 240 / (1200 * 6), rel=1e-9)   # 33.333

    with monkeypatch.context() as mp:                       # <- the module as it used to behave
        mp.setattr(rb, "_subsample", _legacy_sampler)
        old = rb.scorecard(root)
    assert _row(old, "traffic.overlap_events")["value"] == 0
    assert _row(old, "traffic.overlap_rate_per_1k_pair_instants")["value"] == 0.0
    assert _row(old, "traffic.overlap_rate_per_1k_pair_instants")["details"][
        "estimated_run_total"] == 0

    new = rb.scorecard(root)
    r = _row(new, "traffic.overlap_rate_per_1k_pair_instants")
    assert r["details"]["instants_examined"] == 240         # the same cap, the same cost
    assert r["value"] == pytest.approx(truth["value"], rel=0.35)
    assert _row(new, "traffic.overlap_events")["value"] > 0


def test_the_same_alias_can_also_inflate_the_published_rate_fivefold(tmp_path, monkeypatch):
    """The other direction, because a sampler that only ever under-reports is a different bug.

    Same stride of 5, same period of 5, phase 0 this time: now the fixed stride lands on an
    overlapping instant EVERY time and publishes five times the truth.
    """
    root = _dataset(str(tmp_path / "hot"),
                    _overlapping_platoon(1200, overlap_every=5, overlap_phase=0))
    truth = _row(rb.scorecard(root, max_instants=0),
                 "traffic.overlap_rate_per_1k_pair_instants")["value"]
    with monkeypatch.context() as mp:
        mp.setattr(rb, "_subsample", _legacy_sampler)
        old = _row(rb.scorecard(root), "traffic.overlap_rate_per_1k_pair_instants")["value"]
    assert old == pytest.approx(5.0 * truth, rel=1e-9)      # every sampled instant overlaps
    new = _row(rb.scorecard(root), "traffic.overlap_rate_per_1k_pair_instants")["value"]
    assert new == pytest.approx(truth, rel=0.35)


def test_shifting_the_run_by_one_instant_no_longer_moves_the_published_rate(tmp_path, monkeypatch):
    """The sharp control, in miniature: sweep the phase offset across a FULL PERIOD.

    A fix that merely moved the lock to a different phase would pass both tests above and fail this
    one. Dropping the first `off` instants of the run re-phases the sampling grid; under the old
    rule the published rate then swings over the whole period (here 0x to 5x the truth), and under
    the new rule it does not move outside its sampling noise.
    """
    rows = _overlapping_platoon(1205, overlap_every=5, overlap_phase=0)
    old_vals, new_vals = [], []
    for off in range(5):                                    # a full period of the structure
        root = _dataset(str(tmp_path / f"off{off}"), [r for r in rows if r["t"] >= off])
        with monkeypatch.context() as mp:
            mp.setattr(rb, "_subsample", _legacy_sampler)
            old_vals.append(_row(rb.scorecard(root),
                                 "traffic.overlap_rate_per_1k_pair_instants")["value"])
        new_vals.append(_row(rb.scorecard(root),
                             "traffic.overlap_rate_per_1k_pair_instants")["value"])
    truth = _row(rb.scorecard(_dataset(str(tmp_path / "t"), rows), max_instants=0),
                 "traffic.overlap_rate_per_1k_pair_instants")["value"]
    assert min(old_vals) == 0.0                             # the lock lands off the structure ...
    assert max(old_vals) >= 4.5 * truth                     # ... and dead on it
    assert max(new_vals) / max(min(new_vals), 1e-9) < 2.0   # the spread collapses
    assert all(v == pytest.approx(truth, rel=0.4) for v in new_vals), new_vals
    assert (sum(new_vals) / len(new_vals)) == pytest.approx(truth, rel=0.15)


def test_the_sampler_is_deterministic_seeded_and_stream_isolated(tmp_path):
    """Byte-identical output for the same seed+config is this repository's core contract.

    And the jitter must come from a NAMED stream keyed on (seed, label, n, k) alone, so that adding
    a capped scan can never move an existing one's numbers -- the property the engine's single
    global `random.Random(cfg.seed)` does not have.
    """
    root = _dataset(str(tmp_path / "d"), _overlapping_platoon(1200, overlap_every=5))
    a = json.dumps(rb.scorecard(root), sort_keys=True, default=str)
    b = json.dumps(rb.scorecard(root), sort_keys=True, default=str)
    assert a == b
    assert json.dumps(rb.scorecard(root, sampler_seed=rb.SAMPLER_SEED + 1),
                      sort_keys=True, default=str) != a     # the seed is live, not decorative

    items = list(range(1000))
    assert (rb._subsample(items, 240, stream="overlap")
            != rb._subsample(items, 240, stream="headway"))
    # ... and no global RNG is touched, whatever the sampler draws
    random.seed(12345)
    before = random.random()
    random.seed(12345)
    rb._subsample(items, 240, stream="overlap")
    rb.scorecard(root)
    assert random.random() == before


def test_the_scorecard_names_the_sampler_that_chose_its_instants(tmp_path):
    """A cap without its sampler is not a reproducible description of a capped scan -- the sampler
    is the half that carried the bias, and it was previously invisible in the output."""
    card = rb.scorecard(_dataset(str(tmp_path / "s"), _overlapping_platoon(1200, overlap_every=5)))
    assert card["settings"]["sampler"] == rb.SAMPLER_KEY
    assert card["settings"]["sampler_seed"] == rb.SAMPLER_SEED
    for mid in ("traffic.overlap_events", "traffic.overlap_rate_per_1k_pair_instants",
                "traffic.headway_p50_s", "traffic.headway_ks_shifted_exponential"):
        d = _row(card, mid)["details"]
        assert d["sampler"] == rb.SAMPLER_KEY and d["sampler_seed"] == rb.SAMPLER_SEED
        assert "jittered systematic" in d["sampling"]
    # the comm panel's co-presence buckets are sub-sampled by the same rule and say so too
    d = _row(card, "comm.copresence_pairs")["details"] if any(
        m["id"] == "comm.copresence_pairs" for m in card["panels"]["comm"]) else None
    if d is not None:
        assert d["sampler"] == rb.SAMPLER_KEY


def test_an_uncapped_scan_draws_no_random_numbers_at_all(tmp_path):
    """`max_instants=0` must be the same answer under every seed: there is nothing left to choose."""
    root = _dataset(str(tmp_path / "u"), _overlapping_platoon(600, overlap_every=5))
    a = rb.scorecard(root, max_instants=0)
    b = rb.scorecard(root, max_instants=0, sampler_seed=rb.SAMPLER_SEED + 977)
    ida, idb = ("traffic.overlap_rate_per_1k_pair_instants", "traffic.headway_ks_shifted_exponential")
    assert _row(a, ida)["value"] == _row(b, ida)["value"]
    assert _row(a, idb)["value"] == _row(b, idb)["value"]
    assert rb._subsample(list(range(50)), 240, stream="x") == list(range(50))   # n <= k: identity


# ==================================================================================================
# DEFECT 2 -- an awareness metric that exists is not the same as one that has converged
# ==================================================================================================
def _link_dataset(root, n_steps, n_veh=12, reach_m=150.0):
    """Co-present vehicles 100 m apart, each hearing only its immediate neighbours.

    Link count grows linearly with `n_steps`, which is exactly how `comm.honest_links` behaves on the
    real arm (9 / 29 / 56 / 96 / 185 / 358 / 755 over the ladder), so the fixture reproduces the
    "the row appears long before it converges" situation the floor exists for.
    """
    emis, reports, labels = [], [], []
    k = 0
    for step in range(n_steps):
        t = float(step)
        for j in range(n_veh):
            emis.append({"t": t, "true_vehicle_id": f"veh_{j:03d}", "true_x": 100.0 * j,
                         "true_y": 0.0, "claimed_x": 100.0 * j, "claimed_y": 0.0,
                         "claimed_speed": 0.0, "pos_conf": 5.0, "is_attacker": False,
                         "is_faulty": False, "falsified": False})
        for a in range(n_veh):
            for b in range(n_veh):
                if a == b or abs(a - b) * 100.0 > reach_m:
                    continue
                rid = f"rpt_{k:05d}"
                k += 1
                reports.append({"report_id": rid, "detection_time": t,
                                "reason_codes": ["positionJump"],
                                "detnorm_acceptanceRangeThreshold": 0.0})
                labels.append({"report_id": rid, "reporter_true_id": f"veh_{a:03d}",
                               "subject_true_id": f"veh_{b:03d}",
                               "report_correctness": "false_positive"})
    return _dataset(root, emis, reports=reports, labels=labels,
                    config={"duration_s": float(n_steps)})


def test_an_under_resolved_curve_withholds_the_graded_verdict_and_says_why(tmp_path):
    """The fix for the one metric whose PASS/FAIL flipped on duration alone.

    Just past MIN_SAMPLES the curve exists and used to be graded. It is now `na`, the reason names
    the measured links-per-bin, the floor, and -- the actionable part -- how long a run this
    dataset's own observed link rate would need.
    """
    card = rb.scorecard(_link_dataset(str(tmp_path / "thin"), n_steps=6))
    links = _row(card, "comm.honest_links")
    assert links["value"] >= rb.MIN_SAMPLES          # the row EXISTS by the old rule
    d = links["details"]
    assert d["links_per_usable_bin"] < rb.CURVE_MIN_LINKS_PER_BIN
    assert d["curve_resolved"] is False
    assert d["duration_for_curve_metrics_s"] > d["simulated_span_s"]

    gray = _row(card, "comm.pdr_gray_zone_width_m")
    assert gray["status"] == "na" and gray["value"] is None
    assert "UNDER-RESOLVED CURVE" in gray["reason"]
    assert str(rb.CURVE_MIN_LINKS_PER_BIN) in gray["reason"]
    # the ungated rows lose their VALUE too: a wrong number under an `na` status is still wrong
    for mid in ("comm.awareness_ratio_100m", "comm.awareness_ratio_200m",
                "comm.awareness_ratio_300m", "comm.effective_range_m"):
        assert _row(card, mid)["value"] is None, mid
    # nothing became a failure: withholding is not failing
    assert "comm.pdr_gray_zone_width_m" not in card["summary"]["soft_failures"]


def test_a_resolved_curve_is_still_published_and_still_graded(tmp_path):
    """The floor must not silence a dataset that has the links. Same fixture, longer run."""
    card = rb.scorecard(_link_dataset(str(tmp_path / "thick"), n_steps=60))
    d = _row(card, "comm.honest_links")["details"]
    assert d["links_per_usable_bin"] >= rb.CURVE_MIN_LINKS_PER_BIN
    assert d["curve_resolved"] is True
    assert _row(card, "comm.awareness_ratio_100m")["value"] == pytest.approx(1.0)
    assert _row(card, "comm.awareness_ratio_300m")["value"] == pytest.approx(0.0)
    eff = _row(card, "comm.effective_range_m")
    assert eff["value"] is not None and "UNDER-RESOLVED" not in (eff["reason"] or "")


def test_the_link_adequacy_block_is_stated_in_the_dataset_s_own_units(tmp_path):
    card = rb.scorecard(_link_dataset(str(tmp_path / "adq"), n_steps=6))
    d = _row(card, "comm.honest_links")["details"]
    n = d["links_in_usable_bins"]                # the curve's own numerator, not every link seen
    assert 0 < n <= _row(card, "comm.honest_links")["value"]
    assert d["links_per_bin_floor"] == rb.CURVE_MIN_LINKS_PER_BIN
    assert d["usable_distance_bins"] >= 1
    assert d["links_required_for_curve_metrics"] == (rb.CURVE_MIN_LINKS_PER_BIN
                                                     * d["usable_distance_bins"])
    assert d["links_per_usable_bin"] == pytest.approx(n / d["usable_distance_bins"], abs=0.01)
    assert d["observed_link_rate_per_sim_s"] == pytest.approx(n / d["simulated_span_s"], rel=1e-3)
    assert d["duration_for_curve_metrics_s"] == pytest.approx(       # published as whole seconds
        d["links_required_for_curve_metrics"] / d["observed_link_rate_per_sim_s"], abs=1.0)
    # and the comm rows carry the co-presence sampling coverage their denominator came from
    assert d["buckets_examined"] <= d["buckets_available"]


# ==================================================================================================
# DEFECT 3 -- certificates expire on their own clock, not the simulation's
# ==================================================================================================
def test_certificate_validity_is_independent_of_the_run_length(tmp_path):
    """The measured defect: 100% of `ma_cert_status` rows were `[0.0, total_time]` at EVERY rung.

    With `cert_validity_s` the issued window is the SAME at two different durations, which is the
    whole property that was missing: a certificate's lifetime stopped being the `--duration` flag.
    """
    spans = {}
    for dur in (60, 180):
        res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / f"on{dur}"), duration_s=dur,
                                          cert_validity_s=45.0, **FLOW))
        idm = _jsonl(res.out_dir, "ground_truth/gt_identity_map.jsonl")
        st = _jsonl(res.out_dir, "ma/ma_cert_status.jsonl")
        spans[dur] = sorted({round(r["valid_to"] - r["valid_from"], 3) for r in idm})
        # NOT ONE row is the whole run any more
        whole = [r for r in st if r["valid_from"] == 0.0 and abs(r["valid_to"] - dur) < 1e-6]
        assert whole == [], f"{len(whole)} of {len(st)} rows still carry [0, total_time]"
        assert {round(r["valid_to"] - r["valid_from"], 3) for r in st} == {45.0}
    assert spans[60] == spans[180] == [45.0]


def test_certificate_validity_off_reproduces_the_run_length_window(tmp_path):
    """The legacy behaviour is preserved exactly on the default arm -- and it IS the defect."""
    dur = 60
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "off"), duration_s=dur, **FLOW))
    st = _jsonl(res.out_dir, "ma/ma_cert_status.jsonl")
    assert st and all(r["valid_from"] == 0.0 and r["valid_to"] == float(dur) for r in st)


def test_cert_validity_is_byte_identical_when_off(tmp_path):
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), duration_s=60, **FLOW))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), duration_s=60,
                                    cert_validity_s=0.0, persistent_attacker_pct=0.0,
                                    demand_period_s=0.0, **FLOW))
    assert a.data_digest == b.data_digest


def test_rotation_becomes_meaningful_when_a_certificate_can_expire(tmp_path):
    """`rotate_period_s` is 0 by default, so a pseudonym was a stable identifier for a whole life:
    measured certificates-per-vehicle 1.0506 -> 1.0430 over a 96x duration range. A certificate that
    EXPIRES forces a rotation, so `cert_validity_s` alone now drives the pseudonym change."""
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "rot"), duration_s=120,
                                      cert_validity_s=20.0, **FLOW))
    idm = _jsonl(res.out_dir, "ground_truth/gt_identity_map.jsonl")
    per = {}
    for r in idm:
        per[r["true_vehicle_id"]] = per.get(r["true_vehicle_id"], 0) + 1
    assert max(per.values()) >= 3, per
    assert sum(per.values()) / len(per) > 1.5
    # more than one of a vehicle's certificates is actually USED on the air
    seen = {r["cert_digest"] for r in _jsonl(res.out_dir, "ma/ma_cert_status.jsonl")}
    by_veh = {}
    for r in idm:
        if r["pseudonym_cert_digest"] in seen:
            by_veh[r["true_vehicle_id"]] = by_veh.get(r["true_vehicle_id"], 0) + 1
    assert max(by_veh.values()) >= 2, "no vehicle ever rotated onto a second certificate"


def test_a_validity_shorter_than_the_rotation_period_is_refused():
    with pytest.raises(ValueError, match="uncredentialed between rotations"):
        validate_config(PipelineConfig(duration_s=60, cert_validity_s=10.0,
                                       rotate_period_s=60.0, **FLOW))


def test_a_negative_certificate_validity_is_refused():
    with pytest.raises(ValueError, match="cert_validity_s must be >= 0"):
        validate_config(PipelineConfig(duration_s=60, cert_validity_s=-1.0, **FLOW))


def test_certificate_expiry_does_not_manufacture_false_positives(tmp_path):
    """The forward cap it replaces existed to stop benign vehicles showing an expired cert.

    A pool sized to the trip plus a spare window has to keep that property, or the fix trades an
    unmeasurable certificate lifetime for a precision collapse. What must not grow is the FALSE
    `certValidity` -- an honest subject flagged for a credential the engine mis-budgeted. True
    `certValidity` firings belong to the attack types that falsify the window on purpose and are
    supposed to be there.
    """
    def false_cert_validity(res):
        labels = {r["report_id"]: r for r in
                  _jsonl(res.out_dir, "ground_truth/gt_report_labels.jsonl")}
        return sum(1 for r in _jsonl(res.out_dir, "ma/ma_reports.jsonl")
                   if "certValidity" in r["reason_codes"]
                   and labels.get(r["report_id"], {}).get("report_correctness") == "false_positive")

    off = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "p0"), duration_s=120, **FLOW))
    on = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "p1"), duration_s=120,
                                     cert_validity_s=60.0, **FLOW))
    assert false_cert_validity(off) == 0, "the baseline must not false-flag a benign credential"
    assert false_cert_validity(on) == 0, "finite certificate validity must not either"


# ==================================================================================================
# DEFECT 4 -- an adversary whose campaign outlives its trip
# ==================================================================================================
# Deliberately longer than any trip this grid produces (the baseline's longest attack span is the
# control the test asserts against), because "longer than one trip" is the whole property.
CAMPAIGN_S, CAMPAIGN_JITTER = 900.0, 0.2


@pytest.fixture(scope="module")
def attack_runs(tmp_path_factory):
    """One baseline arm and one persistent arm, shared by the two tests below (each is ~25 s)."""
    root = tmp_path_factory.mktemp("campaign")
    dur = 360
    base = run_pipeline(PipelineConfig(out_dir=str(root / "base"), duration_s=dur, **FLOW))
    pers = run_pipeline(PipelineConfig(out_dir=str(root / "pers"), duration_s=dur,
                                       persistent_attacker_pct=0.5,
                                       persistent_campaign_s=CAMPAIGN_S,
                                       persistent_campaign_jitter=CAMPAIGN_JITTER,
                                       persistent_dwell_min_s=20.0, persistent_dwell_max_s=60.0,
                                       cert_validity_s=120.0, **FLOW))
    return dur, base, pers


def _attack_spans(res):
    return [r["end_time"] - r["start_time"]
            for r in _jsonl(res.out_dir, "ground_truth/gt_attacks.jsonl")]


def test_an_attacker_cannot_outlive_its_trip_without_the_opt_in(attack_runs):
    """The measured baseline: the attack span is the TRIP's, and it is stationary in duration.

    `attack_to = spawn_time + life`, so the span is bounded by the certificate-life budget of one
    journey however long the run is -- p50 256 s / p95 484 s / max 598 s at 1800 s, 3600 s, 7200 s,
    14400 s and 28800 s alike (docs/realism/LONG-RUNS.md 4.5). The bound below is that trip scale,
    not the run length: `gt_attacks.end_time` is not clipped to the simulation end, so on a short run
    an attacker spawned near the end legitimately reports a window that runs past it.
    """
    _dur, base, _pers = attack_runs
    spans = _attack_spans(base)
    assert spans and max(spans) < 600.0, "no opt-in: no adversary should outlive one trip"


def test_a_persistent_adversary_spans_many_trips(attack_runs):
    _dur, base, pers = attack_runs
    base_max = max(_attack_spans(base))
    spans = _attack_spans(pers)
    assert max(spans) > base_max, (max(spans), base_max)
    # a campaign is drawn around its configured mean, jitter included
    lo = CAMPAIGN_S * (1.0 - CAMPAIGN_JITTER)
    hi = CAMPAIGN_S * (1.0 + CAMPAIGN_JITTER)
    campaigns = [s for s in spans if s > base_max]
    assert campaigns, "no campaign outlived the longest ordinary trip"
    assert all(lo <= c <= hi for c in campaigns), sorted(campaigns)[:5]
    # a campaign identity really drove more than one journey: its pseudonym pool outgrows one trip
    idm = _jsonl(pers.out_dir, "ground_truth/gt_identity_map.jsonl")
    per = {}
    for r in idm:
        per[r["true_vehicle_id"]] = per.get(r["true_vehicle_id"], 0) + 1
    assert max(per.values()) >= 3, per


def test_a_re_entering_adversary_never_reads_as_a_teleport(tmp_path):
    """Re-entry leaves a GAP in the trajectory, and `teleport_events` tests mean speed across gaps.

    The dwell is floored at the driving time to the next origin exactly so that this stays true; a
    fix that bought a persistent adversary at the cost of a HARD gate would not be a fix.
    """
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "tel"), duration_s=280,
                                      persistent_attacker_pct=0.6, persistent_campaign_s=240.0,
                                      persistent_campaign_jitter=0.0,
                                      persistent_dwell_min_s=1.0, persistent_dwell_max_s=2.0,
                                      emit_mobility_oracle=True, **FLOW))
    card = rb.scorecard(res.out_dir)
    tele = _row(card, "traffic.teleport_events")
    assert tele["value"] == 0 and tele["status"] == "pass", tele
    assert _row(card, "traffic.teleport_rate_per_1k_pairs")["value"] == 0.0


def test_persistent_attackers_off_is_byte_identical(tmp_path):
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), duration_s=60, **FLOW))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), duration_s=60,
                                    persistent_attacker_pct=0.0, persistent_campaign_s=7200.0,
                                    persistent_dwell_min_s=5.0, **FLOW))
    assert a.data_digest == b.data_digest


def test_a_persistent_adversary_needs_flow_and_a_graph():
    with pytest.raises(ValueError, match="needs traffic_flow"):
        validate_config(PipelineConfig(persistent_attacker_pct=0.2, n_vehicles=5, n_steps=10))
    kw = dict(FLOW)
    kw["road_network"] = "linear"
    with pytest.raises(ValueError, match="routed road network"):
        validate_config(PipelineConfig(duration_s=60, persistent_attacker_pct=0.2, **kw))


def test_persistent_campaign_knobs_are_range_checked():
    with pytest.raises(ValueError, match="persistent_attacker_pct must be in"):
        validate_config(PipelineConfig(duration_s=60, persistent_attacker_pct=1.5, **FLOW))
    with pytest.raises(ValueError, match="persistent_campaign_s must be > 0"):
        validate_config(PipelineConfig(duration_s=60, persistent_attacker_pct=0.2,
                                       persistent_campaign_s=0.0, **FLOW))
    with pytest.raises(ValueError, match="persistent_dwell_min_s <= persistent_dwell_max_s"):
        validate_config(PipelineConfig(duration_s=60, persistent_attacker_pct=0.2,
                                       persistent_dwell_min_s=100.0,
                                       persistent_dwell_max_s=10.0, **FLOW))


# ==================================================================================================
# DEFECT 5 -- the demand profile gets a real clock
# ==================================================================================================
def _spawn_times(res):
    return sorted(r["spawn_time"] for r in _jsonl(res.out_dir, "ground_truth/gt_vehicle.jsonl"))


def _peak_time(res, dur, nb=12):
    h = [0] * nb
    for s in _spawn_times(res):
        h[min(nb - 1, int(s / dur * nb))] += 1
    return (h.index(max(h)) + 0.5) * dur / nb, h


# A period a unit test can afford to drive all the way round. The SHAPE of the finding does not
# depend on the period being 86,400 s -- what matters is that the peak sits at an ABSOLUTE time
# inside the period instead of at a fraction of the run. Both peaks are put at the same instant so
# there is exactly one, and the off-peak floor is dropped so the contrast is unmistakable in a small
# vehicle count.
CLOCK = dict(demand_profile="rush", demand_period_s=1200.0, demand_start_s=0.0,
             demand_am_peak_s=600.0, demand_pm_peak_s=600.0, demand_peak_sigma_s=120.0,
             demand_off_peak_frac=0.05)
DEM = dict(FLOW, arrival_rate=1.0)


def test_the_legacy_demand_profile_is_a_shape_stretched_to_the_run(tmp_path):
    """The defect itself, asserted so the fix has something to be a fix OF.

    Without a clock the "AM peak" lands at the same FRACTION of any run, so its wall-clock position
    moves with `--duration` and two durations are two different scenarios.
    """
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "l300"), duration_s=300,
                                    demand_profile="rush", **DEM))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "l600"), duration_s=600,
                                    demand_profile="rush", **DEM))
    pa, _ = _peak_time(a, 300)
    pb, _ = _peak_time(b, 600)
    assert pb > 1.5 * pa, (pa, pb)          # the peak moved with the run length: ~75 s vs ~150 s
    sa, sb = _spawn_times(a), _spawn_times(b)
    assert sa != sb[:len(sa)], "the legacy profile is expected to break prefix nesting"


def test_a_clocked_demand_profile_puts_the_peak_at_a_wall_clock_time(tmp_path):
    """With `demand_period_s` the peak is at its clock time whatever `--duration` says."""
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "c1"), duration_s=1200,
                                    **CLOCK, **DEM))
    pa, ha = _peak_time(a, 1200, nb=12)
    assert 500.0 <= pa <= 700.0, (pa, ha)   # the 600 s bucket, i.e. 50% of the run, not 25% of it
    # and the multiplier really is the floor away from the peak: the first tenth is nearly empty
    assert ha[0] * 4 < max(ha), ha


def test_a_clockless_demand_profile_is_stamped_on_every_traffic_row(tmp_path):
    """docs/realism/LONG-RUNS.md section 6, recommendation 3: the scorecard must refuse a
    cross-duration comparison on a `rush`/`night` dataset, or at minimum stamp the profile into the
    traffic panel so a reader cannot line two such runs up without seeing it."""
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "s1"), duration_s=120,
                                    demand_profile="rush", **DEM))
    d = _row(rb.scorecard(a.out_dir), "traffic.speed_p50_mps")["details"]
    assert d["demand_profile"] == "rush" and not d["demand_period_s"]
    assert "REFUSED" in d["cross_duration_comparison"]

    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "s2"), duration_s=120,
                                    **CLOCK, **DEM))
    d2 = _row(rb.scorecard(b.out_dir), "traffic.speed_p50_mps")["details"]
    assert d2["demand_profile"] == "rush" and d2["demand_period_s"] == CLOCK["demand_period_s"]
    assert "cross_duration_comparison" not in d2      # a clocked profile IS comparable

    u = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "s3"), duration_s=120, **DEM))
    d3 = _row(rb.scorecard(u.out_dir), "traffic.speed_p50_mps")["details"]
    assert d3["demand_profile"] == "uniform"
    assert "cross_duration_comparison" not in d3      # ... and so is the uniform arm


def test_a_clocked_demand_profile_restores_prefix_nesting(tmp_path):
    """A shorter run becomes an exact PREFIX of a longer one again -- the property section 0 of
    docs/realism/LONG-RUNS.md needs before any quantity may be compared across durations, and the
    one `--demand rush` destroyed (11 of 278 vehicles survived a 300 s -> 900 s change)."""
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "n300"), duration_s=300,
                                    **CLOCK, **DEM))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "n600"), duration_s=600,
                                    **CLOCK, **DEM))
    sa, sb = _spawn_times(a), _spawn_times(b)
    assert sa, "the clocked profile spawned nothing"
    assert sa == sb[:len(sa)], "a shorter clocked run must be a prefix of a longer one"


def test_a_rush_hour_is_an_hour(tmp_path):
    """`demand_peak_sigma_s` is in SECONDS, so the peak has a real width instead of 9% of the run."""
    from scms_sim_ref.mock_pipeline.run import PipelineConfig as _C
    c = _C(demand_period_s=86400.0, demand_peak_sigma_s=3600.0, demand_am_peak_s=28800.0)
    assert c.demand_peak_sigma_s == 3600.0 and c.demand_am_peak_s == 28800.0
    # the shape itself: half-power points one sigma either side of 08:00
    off, sig = c.demand_off_peak_frac, c.demand_peak_sigma_s
    at_peak = off + (1.0 - off) * min(1.0, math.exp(0.0))
    at_sigma = off + (1.0 - off) * min(1.0, math.exp(-1.0))
    assert at_peak == pytest.approx(1.0)
    assert 0.4 < at_sigma < 0.6                    # demand has roughly halved an hour off the peak


def test_the_demand_clock_is_range_checked():
    with pytest.raises(ValueError, match="demand_period_s must be >= 0"):
        validate_config(PipelineConfig(duration_s=60, demand_period_s=-1.0, **FLOW))
    with pytest.raises(ValueError, match="demand_peak_sigma_s must be > 0"):
        validate_config(PipelineConfig(duration_s=60, demand_period_s=86400.0,
                                       demand_peak_sigma_s=0.0, **FLOW))
    with pytest.raises(ValueError, match="demand_off_peak_frac must be in"):
        validate_config(PipelineConfig(duration_s=60, demand_period_s=86400.0,
                                       demand_off_peak_frac=2.0, **FLOW))


def test_the_uniform_arm_is_untouched_by_the_clock(tmp_path):
    """`demand_profile='uniform'` multiplies by 1.0 either way, so the clock cannot move it."""
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "u0"), duration_s=60, **FLOW))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "u1"), duration_s=60,
                                    demand_period_s=86400.0, demand_start_s=25200.0, **FLOW))
    assert a.data_digest == b.data_digest
