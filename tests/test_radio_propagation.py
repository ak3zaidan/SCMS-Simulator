"""Opt-in log-distance path-loss + log-normal shadowing radio model (radio_model="logdistance").

The default radio_model="disc" keeps today's HARD range disc (a receiver hears a transmitter iff
dist <= range), so every existing digest is byte-identical. "logdistance" replaces that cutoff with a
SOFT probabilistic range: mean received power = 10*n*log10(range/d) dB relative to sensitivity (0 dB
exactly at d==range -> median range == radio_range_m), plus a per-link log-normal shadowing draw; a
link is heard iff received power >= sensitivity(+margin). The existing packet_loss_base / nlos_loss /
congestion / weather losses then compose on top as per-packet drops on the links that do close.

These tests pin: (a) the default disc golden digest, (b) that logdistance yields a soft range (some
transmitters are heard well beyond radio_range_m), (c) that sigma=0 collapses to the disc cutoff at
the reference range (calibration sanity), (d) determinism, and (e) pathloss-exponent monotonicity.
"""

import json

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

# Golden digest of the determinism-contract default config (must never move when the feature is OFF;
# re-pinned once by ADR 0002, which added true_speed/true_heading to the ground-truth record).
# ADR 0002 re-pin (true_speed/true_heading added to gt_emissions_sample):
# superseded 04ae9736f519... (default-config run; behaviour unchanged).
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"


def _default_run(tmp, name):
    """The exact determinism-contract config used to pin the default (disc) golden digest."""
    return run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp / name)))


# A denser routed config that produces plenty of benign (honest) false-positive reports, so we can
# reconstruct how far receivers heard transmitters. Fixed seed => every assertion below is deterministic.
_RR = 250.0
_ART = 150.0


def _run(tmp, name, **kw):
    base = dict(seed=11, traffic_flow=True, road_network="grid", duration_s=80, arrival_rate=2.0,
                grid_w=5, grid_h=5, attacker_pct=0.2, faulty_pct=0.05, radio_range_m=_RR,
                art_max_m=_ART, out_dir=str(tmp / name))
    base.update(kw)
    return run_pipeline(PipelineConfig(**base))


def _jsonl(path):
    return [json.loads(ln) for ln in open(path, encoding="utf-8") if ln.strip()]


def _labels(out):
    return {r["report_id"]: r for r in _jsonl(f"{out}/ground_truth/gt_report_labels.jsonl")}


def _report_pairs(out):
    """The (reporter, subject) true-id graph -- the reception/report topology."""
    return {(r["reporter_true_id"], r["subject_true_id"])
            for r in _jsonl(f"{out}/ground_truth/gt_report_labels.jsonl")}


def _honest_heard_dists(out):
    """Reconstructed receiver->subject distance for HONEST (false_positive) subjects.

    For an honest subject the CLAIMED position equals its measured true position, so the recorded
    detnorm_acceptanceRangeThreshold = max(0, dist - rr)/art_max directly reveals how far beyond the
    receiver's range rr the transmitter actually was: dist = detnorm*art_max + rr. Under the hard disc
    this can only be ~rr (+GNSS noise); under logdistance a favourable shadow can push it far past rr.
    """
    lab = _labels(out)
    dists = []
    for r in _jsonl(f"{out}/ma/ma_reports.jsonl"):
        if lab[r["report_id"]]["report_correctness"] != "false_positive":
            continue
        dists.append(r.get("detnorm_acceptanceRangeThreshold", 0.0) * _ART + _RR)
    return dists


def test_default_disc_golden_unchanged(tmp_path):
    """The default config's digest is byte-identical to before the feature, and an EXPLICIT
    radio_model='disc' run reproduces it exactly (disc is literally today's behaviour)."""
    default = _default_run(tmp_path, "default")
    assert default.data_digest == DEFAULT_GOLDEN, (default.data_digest, DEFAULT_GOLDEN)
    explicit = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, radio_model="disc", out_dir=str(tmp_path / "disc")))
    assert explicit.data_digest == DEFAULT_GOLDEN


def test_logdistance_produces_a_soft_range(tmp_path):
    """logdistance is a SOFT range: it hears transmitters well beyond radio_range_m (impossible under
    the disc) and rewires the report graph, while the disc stays capped at rr (within GNSS noise)."""
    disc = _run(tmp_path, "disc", radio_model="disc")
    logd = _run(tmp_path, "logd", radio_model="logdistance")
    assert logd.data_digest != disc.data_digest

    disc_hd, logd_hd = _honest_heard_dists(disc.out_dir), _honest_heard_dists(logd.out_dir)
    assert disc_hd and logd_hd, "config must yield honest false-positive reports to measure"
    # disc never hears an honest transmitter meaningfully past its range (only GNSS-noise slack).
    assert max(disc_hd) <= _RR * 1.2, max(disc_hd)
    # logdistance DOES: some honest transmitters are heard far beyond the nominal range.
    assert max(logd_hd) > _RR * 1.3, max(logd_hd)

    # the soft range also changes WHICH (reporter, subject) links form -> a different report topology.
    assert _report_pairs(disc.out_dir) != _report_pairs(logd.out_dir)


def test_sigma_zero_is_the_disc_cutoff_at_the_reference_range(tmp_path):
    """Reference calibration: with shadowing_sigma_db=0 and no margin, the received power is a
    deterministic function of distance that crosses sensitivity EXACTLY at radio_range_m -> logdistance
    collapses to the disc's hard d<=rr cutoff. It is byte-identical to disc, and the mean honest heard
    distance sits right at the range (no reception beyond it)."""
    disc = _run(tmp_path, "z_disc", radio_model="disc")
    zero = _run(tmp_path, "z_logd", radio_model="logdistance",
                shadowing_sigma_db=0.0, rx_sensitivity_margin_db=0.0)
    assert zero.data_digest == disc.data_digest, "sigma=0/margin=0 must reproduce the disc exactly"

    hd = _honest_heard_dists(zero.out_dir)
    assert hd, "expected honest false-positive reports"
    assert max(hd) <= _RR * 1.2, max(hd)              # near-hard cutoff at the reference range
    mean_hd = sum(hd) / len(hd)
    assert abs(mean_hd - _RR) <= _RR * 0.15, mean_hd  # mean heard distance ~ radio_range_m


def test_logdistance_is_deterministic(tmp_path):
    """Same seed + config -> byte-identical, twice (the per-link shadowing rng is string-keyed)."""
    a = _run(tmp_path, "det_a", radio_model="logdistance")
    b = _run(tmp_path, "det_b", radio_model="logdistance")
    assert a.data_digest == b.data_digest


def test_pathloss_exponent_monotonicity(tmp_path):
    """A steeper path-loss exponent attenuates faster -> the effective range shrinks: the mean honest
    heard distance and the count of beyond-range links both fall monotonically as n grows."""
    means, beyond = [], []
    for n in (2.7, 4.5, 6.0):
        r = _run(tmp_path, f"exp_{n}", radio_model="logdistance", pathloss_exponent=n)
        hd = _honest_heard_dists(r.out_dir)
        assert hd, f"expected honest reports at exponent {n}"
        means.append(sum(hd) / len(hd))
        beyond.append(sum(1 for d in hd if d > _RR + 30.0))   # links clearly past the range
    assert means[0] > means[1] > means[2], means               # steeper -> shorter mean heard distance
    assert beyond[0] >= beyond[1] >= beyond[2], beyond         # steeper -> fewer long links
    assert beyond[0] > beyond[2], beyond                       # and a real, non-trivial reduction
