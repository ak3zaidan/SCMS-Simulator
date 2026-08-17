"""Tests for radio realism: range-limited reception, packet loss, and timing-family attacks."""

import collections
import json

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def _reasons(out):
    c = collections.Counter()
    for ln in open(f"{out}/ma/ma_reports.jsonl", encoding="utf-8"):
        c.update(json.loads(ln).get("reason_codes", []))
    return c


def test_radio_range_limits_who_can_report(tmp_path):
    """A short radio range on a strung-out fleet yields far fewer reports than all-to-all."""
    kw = dict(seed=4, n_vehicles=60, n_steps=60, attacker_pct=0.2, faulty_pct=0.0)
    near = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "near"), radio_range_m=120.0, **kw))
    allrange = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "all"), radio_range_m=100000.0, **kw))
    assert near.n_reports < allrange.n_reports, "shorter range must reduce the report volume"


def test_packet_loss_reduces_reports(tmp_path):
    kw = dict(seed=4, n_vehicles=40, n_steps=60, attacker_pct=0.2, faulty_pct=0.0, radio_range_m=100000.0)
    lossless = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), packet_loss_base=0.0, **kw))
    lossy = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), packet_loss_base=0.4, **kw))
    assert lossy.n_reports < lossless.n_reports


def test_dos_attack_triggers_beacon_frequency(tmp_path):
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, seed=2, n_vehicles=30, n_steps=60, attacker_ids=(4,),
                                attack_type="DoS", attack_types=("DoS",), faulty_pct=0.0))
    assert _reasons(out)["beaconFrequency"] > 0, "a DoS flood must trigger the beaconFrequency detector"


def test_out_of_order_and_dos_random_are_caught(tmp_path):
    """Completes the timing family: OutOfOrder -> staleOrReplay, DoSRandom -> beaconFrequency."""
    for atk, reason in (("OutOfOrder", "staleOrReplay"), ("DoSRandom", "beaconFrequency")):
        out = str(tmp_path / atk)
        run_pipeline(PipelineConfig(out_dir=out, seed=4, n_vehicles=36, n_steps=70, attacker_ids=(5, 11),
                                    attack_type=atk, attack_types=(atk,), faulty_pct=0.0))
        assert _reasons(out)[reason] > 0, f"{atk} should trigger {reason}"


def test_delayed_messages_trigger_stale_detector(tmp_path):
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, seed=2, n_vehicles=30, n_steps=60, attacker_ids=(4,),
                                attack_type="DelayedMessages", attack_types=("DelayedMessages",),
                                faulty_pct=0.0))
    assert _reasons(out)["staleOrReplay"] > 0, "stale timestamps must trigger staleOrReplay"


def test_degradation_bursts_yield_false_positives_but_not_faulty_revocations(tmp_path):
    """Transient bad-GNSS bursts give realistic benign false positives; short bursts stay under the
    revocation persistence gate so faults/benign are not systematically revoked."""
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, seed=1, n_vehicles=60, n_steps=90, attacker_pct=0.15,
                                faulty_pct=0.1, gps_degrade_rate=0.02))
    labels = [json.loads(l)["report_correctness"]
              for l in open(f"{out}/ground_truth/gt_report_labels.jsonl", encoding="utf-8")]
    assert "false_positive" in labels
