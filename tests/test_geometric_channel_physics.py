"""The OPT-IN channel physics added to `GeometricChannel`: antenna pattern, per-link blockage,
blocker-type-dependent blockage, the two-ray breakpoint and per-type blocker footprints.

Companion to `test_geometric_channel.py`, which pins what the model does BY DEFAULT -- including
the gaps these terms close. The division of labour is deliberate and both halves are needed:

  * that file asserts the DEFAULT still has no antenna term, still redraws the blockage per packet,
    still has no breakpoint and still uses one blocker width, so that none of this can become the
    default silently; and
  * this file asserts that each term, once switched ON, actually does the thing it claims -- and
    grades it against the standard's own closed form or against the measurement it was fitted to,
    never against a second transcription of the implementation.

Every test here fails if its term is removed or neutered; that was verified by mutation, not
assumed. Nothing in this file runs with a default configuration, so nothing in it can move a pinned
digest.
"""
import inspect
import json
import math
import random
import re
import statistics

import pytest

from scms_sim_ref.api.channel import StationSnapshot, StepFrame
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline, validate_config
from scms_sim_ref.mock_pipeline import run as RM

AH = RM.V2X_ANTENNA_HEIGHT_M
CAR_H = RM.TR37885_BLOCKER_HEIGHT_M["car"]
TRUCK_H = RM.TR37885_BLOCKER_HEIGHT_M["truck"]


def _chan(**kw):
    """A geometric channel with the canyon fallback off, so classification is purely geometric."""
    kw.setdefault("radio_nlosb_density_per_km", 0.0)
    cfg = PipelineConfig(seed=3, radio_model="geometric", **kw)
    validate_config(cfg)
    return RM.GeometricChannel(cfg, buildings=None, dt=1.0)


def _station(vid, x, y, *, blocker_h=CAR_H, is_rsu=False, is_vru=False):
    return StationSnapshot(vid, x, y, RSU_H if is_rsu else AH, blocker_h, is_rsu, is_vru, None, 0.0)


RSU_H = RM.RSU_ANTENNA_HEIGHT_M


def _frame(step, stations):
    return StepFrame(step, float(step), 1.0, {s.vid: s for s in stations}, (), (), 0.0, {})


def _settle(ch, stations_by_step):
    """Drive `begin_step` over consecutive frames so headings become established by motion."""
    for i, sts in enumerate(stations_by_step):
        ch.begin_step(_frame(i, sts))


# ================================================================================================
# GAP 1 -- the antenna pattern. CONFORMANT: 3GPP TR 37.885 V15.3.0 Tables 6.1.4-8 / 6.1.4-9,
# "For 6 GHz" column, Option 1.
# ================================================================================================
def _spec_gain(rel_az_deg, zenith_deg, directional, max_gain=3.0):
    """The standard's closed form, transcribed INDEPENDENTLY of the implementation.

    TR 37.885 Table 6.1.4-8, 6 GHz column:
        A_E,V(theta) = -min[12*((theta-90)/theta_3dB)^2, SLA_V],  theta_3dB=90,  SLA_V=20
        Type 2:            A_E,H(phi) = 0
        Type 1 and Type 3: A_E,H(phi) = -min[12*(phi/phi_3dB)^2, A_m], phi_3dB=120, A_m=20
        A''(theta, phi) = -min[-(A_E,V + A_E,H), A_m]
    with the two panel bearings of Table 6.1.4-9 (front 0 deg, rear 180 deg) for Types 1 and 3.
    """
    av = -min(12.0 * ((zenith_deg - 90.0) / 90.0) ** 2, 20.0)
    if not directional:
        return max_gain + -min(-av, 20.0)
    best = -1e9
    for bearing in (0.0, 180.0):
        phi = (rel_az_deg - bearing + 180.0) % 360.0 - 180.0
        ah = -min(12.0 * (phi / 120.0) ** 2, 20.0)
        best = max(best, -min(-(av + ah), 20.0))
    return max_gain + best


@pytest.mark.parametrize("directional", [False, True])
def test_element_pattern_reproduces_the_standards_closed_form(directional):
    """Graded against an independent transcription of Table 6.1.4-8, not against the module."""
    worst = 0.0
    for az in range(-180, 181, 5):
        for zen in (60.0, 75.0, 90.0, 105.0, 120.0):
            got = RM.tr37885_antenna_gain_dbi(az, zen, directional)
            want = _spec_gain(az, zen, directional)
            worst = max(worst, abs(got - want))
    assert worst <= 1e-9, worst


def test_type2_is_azimuth_omnidirectional_and_type3_is_not():
    """Table 6.1.4-8 gives Type 2 `A_E,H(phi) = 0` and Types 1/3 a 120 deg horizontal beamwidth.

    The Type 3 minimum is BROADSIDE, not astern: Table 6.1.4-9 puts panels at 0 and 180 deg, so
    the rear panel covers directly behind and the two nulls meet at +/-90 deg."""
    horizon = [RM.tr37885_antenna_gain_dbi(az, 90.0, False) for az in range(-180, 181, 5)]
    assert len(set(round(g, 12) for g in horizon)) == 1, "Type 2 must not vary with azimuth"
    assert horizon[0] == pytest.approx(RM.TR37885_ANT_MAX_GAIN_DBI)

    ahead = RM.tr37885_antenna_gain_dbi(0.0, 90.0, True)
    astern = RM.tr37885_antenna_gain_dbi(180.0, 90.0, True)
    broadside = RM.tr37885_antenna_gain_dbi(90.0, 90.0, True)
    assert ahead == pytest.approx(RM.TR37885_ANT_MAX_GAIN_DBI)
    assert astern == pytest.approx(RM.TR37885_ANT_MAX_GAIN_DBI), "the rear panel covers astern"
    # 12*(90/120)^2 = 6.75 dB down, and that is the deepest point on the horizon
    assert broadside == pytest.approx(RM.TR37885_ANT_MAX_GAIN_DBI - 6.75)
    assert broadside == pytest.approx(min(RM.tr37885_antenna_gain_dbi(az, 90.0, True)
                                          for az in range(-180, 181)))


def test_received_power_now_depends_on_bearing_for_a_directional_vehicle():
    """THE gap: `mean_rx = tx_dbm - pl + shadow_db` had no antenna term at all.

    Its mirror image, `test_the_default_link_budget_has_no_antenna_gain_or_pattern_term`, asserts
    that rotating the geometry changes nothing by default. Here both endpoints are TR 37.885
    Type 3 (truck/bus), so both patterns are directional and the broadside notch is paid twice."""
    def rssi_at(bearing_deg):
        ch = _chan(radio_antenna_pattern="tr37885_opt1")
        d = 100.0
        px, py = d * math.cos(math.radians(bearing_deg)), d * math.sin(math.radians(bearing_deg))
        # both vehicles drive along +x, so `bearing_deg` IS the peer's relative azimuth
        _settle(ch, [[_station(1, 0.0, 0.0, blocker_h=TRUCK_H),
                      _station(2, px, py, blocker_h=TRUCK_H)],
                     [_station(1, 10.0, 0.0, blocker_h=TRUCK_H),
                      _station(2, px + 10.0, py, blocker_h=TRUCK_H)]])
        heard, rssi, state, _ = ch.evaluate_raw(1, 2, 10.0, 0.0, px + 10.0, py, d, AH, AH)
        assert state == "LOS"
        return rssi

    ahead, broadside = rssi_at(0.0), rssi_at(90.0)
    # identical link, identical streams, identical geometry length: the ONLY difference is bearing
    assert broadside < ahead - 12.0, (ahead, broadside)
    assert ahead - broadside == pytest.approx(2 * 6.75, abs=1e-6)


def test_a_rooftop_vehicle_gets_gain_but_no_bearing_dependence():
    """A Type 2 pair still gains 3 dBi at each end -- the pattern is flat, the gain is not."""
    def rssi_at(bearing_deg, **kw):
        ch = _chan(**kw)
        d = 100.0
        px, py = d * math.cos(math.radians(bearing_deg)), d * math.sin(math.radians(bearing_deg))
        _settle(ch, [[_station(1, 0.0, 0.0), _station(2, px, py)],
                     [_station(1, 10.0, 0.0), _station(2, px + 10.0, py)]])
        return ch.evaluate_raw(1, 2, 10.0, 0.0, px + 10.0, py, d, AH, AH)[1]

    on = [rssi_at(b, radio_antenna_pattern="tr37885_opt1") for b in (0.0, 45.0, 90.0, 180.0)]
    assert len(set(round(v, 9) for v in on)) == 1, on
    off = rssi_at(0.0)
    assert on[0] - off == pytest.approx(2 * RM.TR37885_ANT_MAX_GAIN_DBI, abs=1e-6)


def test_zero_gain_keeps_the_pattern_shape_without_the_absolute_offset():
    """`radio_antenna_gain_dbi=0` is the knob that separates the two halves of the antenna term."""
    ahead = RM.tr37885_antenna_gain_dbi(0.0, 90.0, True, 0.0)
    broadside = RM.tr37885_antenna_gain_dbi(90.0, 90.0, True, 0.0)
    assert ahead == pytest.approx(0.0)
    assert broadside == pytest.approx(-6.75)


def test_an_rsu_endpoint_is_refused_rather_than_silently_given_no_antenna():
    """THE ASYMMETRY THIS CLOSES, and it was recommended having been measured on ZERO RSU links.

    `_station_antenna` has no model for an RSU -- TR 37.885 gives RSUs antenna ARRAYS (Tables
    6.1.4-1..-5) with panel bearings, tilt and TXRU mapping that this channel cannot evaluate. The
    first version returned None and counted the omission, which means a V2V link gained
    2 x 3 = 6 dB while every V2I link gained 3 dB: a PERMANENT 3 dB relative penalty on
    infrastructure links, produced by a missing model rather than by physics, and invisible in any
    aggregate. `n_rsus` defaults to 0, so it was latent -- but `--rsus` is a shipped knob and the MA
    report path uses RSUs. Refused at both ends: `validate_config` rejects the combination, and the
    channel raises if it is ever reached with validation bypassed.

    A VRU is NOT refused. Table 6.1.4-6 gives a pedestrian UE an omnidirectional 0 dBi element, so a
    VRU link's asymmetry is the STANDARD'S OWN answer rather than a hole in ours."""
    with pytest.raises(ValueError, match="n_rsus"):
        validate_config(PipelineConfig(radio_model="geometric", n_rsus=2,
                                       radio_antenna_pattern="tr37885_opt1"))
    validate_config(PipelineConfig(radio_model="geometric", n_rsus=2))          # pattern off: fine
    validate_config(PipelineConfig(radio_model="geometric",                     # no RSUs: fine
                                   radio_antenna_pattern="tr37885_opt1"))

    ch = _chan(radio_antenna_pattern="tr37885_opt1")
    sts = [_station(1, 0.0, 0.0), _station(7, 100.0, 0.0, blocker_h=0.0, is_rsu=True),
           _station(8, 0.0, 100.0, blocker_h=0.0, is_vru=True)]
    _settle(ch, [sts, sts])
    with pytest.raises(ValueError, match="antenna ARRAY"):
        ch._station_antenna(7)
    assert ch._station_antenna(8) == (False, RM.TR37885_PEDESTRIAN_ANT_GAIN_DBI)
    assert ch._antenna_gain_db(8, 0.0, 100.0, 100.0, 0.0, AH, AH, 100.0) == pytest.approx(0.0)
    assert ch._station_antenna(1)[0] is False          # 1.6 m body -> Type 2, rooftop, omni


def test_the_pattern_refuses_the_legacy_begin_step_instead_of_silently_doing_nothing():
    """Heading and station type only reach the channel through the StepFrame form. A model that
    quietly returned 0 dB there would report a pattern it never applied."""
    ch = _chan(radio_antenna_pattern="tr37885_opt1")
    with pytest.raises(ValueError, match="StepFrame"):
        ch.begin_step(0, [])
    _chan().begin_step(0, [])                          # the default path still accepts it


def test_a_stopped_vehicle_holds_its_heading_rather_than_spinning():
    """The reference arm is `--traffic-lights`: vehicles STOP. Heading is derived from motion, so
    the zero-displacement case must hold the last bearing, not recompute one from rounding."""
    ch = _chan(radio_antenna_pattern="tr37885_opt1")
    # drive north, then stop dead for three steps
    _settle(ch, [[_station(1, 0.0, 0.0)], [_station(1, 0.0, 20.0)],
                 [_station(1, 0.0, 20.0)], [_station(1, 0.0, 20.0)]])
    heading = ch._kin[1][2]
    assert heading == pytest.approx(math.pi / 2, abs=1e-9), "still pointing north while stopped"
    assert ch._kin[1][3] == pytest.approx(0.0)


# ================================================================================================
# GAP 2 -- hold the NLOSv blockage draw for the BLOCKAGE EPISODE rather than per packet.
# PHYSICS-MOTIVATED, NOT SPECIFIED BY THE STANDARD -- and the label matters, so it is pinned.
#
# These tests were written under the heading "CONFORMANT: TR 37.885 V15.3.0 clause 6.2.1, 'The
# additional blockage loss is max {0 dB, a log-normal random variable}'", i.e. under the claim that
# the standard mandates ONE DRAW PER BLOCKED LINK. IT DOES NOT. Clause 6.2.1 read in full states no
# temporal scope for the draw; the claim came from the brief that commissioned the term. Worse for
# the label, where the standard IS explicit it disagrees with this implementation twice over: its
# blocker is drawn STATISTICALLY from the fleet's type mix ("The blocker height is the vehicle
# height which is randomly selected out of the three vehicle types according to the portion of the
# vehicle types in the simulated scenario") rather than identified geometrically, and its own
# baseline holds the state for the whole link lifetime ("Baseline is the state is not updated
# between LOS and NLOSv") -- a STRONGER hold than the signature refresh below.
#
# The term stays because its PHYSICS is defensible and its effect is measured (see
# `test_holding_the_blockage_lengthens_consecutive_loss_runs`). Only the conformance claim is
# withdrawn.
# ================================================================================================
def _blocked(ch, step, blocker_vid=99, blocker_x=50.0, blocker_h=TRUCK_H, d=100.0):
    ch.begin_step(step, [(blocker_vid, blocker_x, 0.0, blocker_h)])
    return ch.evaluate_raw(1, 2, 0.0, 0.0, d, 0.0, d, AH, AH)


def test_the_blockage_deviate_is_held_while_the_same_vehicle_blocks():
    """Within a step AND across steps, the same obstruction must yield the same blockage term."""
    ch = _chan(radio_nlosv_hold=True)
    rssis = []
    for step in range(6):
        heard, rssi, state, _ = _blocked(ch, step)
        assert state == "NLOSv"
        rssis.append(rssi)
    # the geometry is static, so shadowing is frozen too; the ONLY per-packet term left is Nakagami
    key = (1, 2)
    assert ch.stats["nlosv_draw"] == 1, "one draw for one uninterrupted obstruction"
    # (blocker vid, TR 37.885 case). The second element used to be an integer count of antennas
    # below the blocker, and was CALLED a "TR 37.885 case index" while taking values in {1, 2} --
    # where the standard's own Case 2 is "both below" and its Case 3 is "otherwise". It is now the
    # case itself, so the signature cannot be misread and cannot silently lose the Case 1 boundary.
    assert ch._nlosv[key][0] == (99, "both_below")
    # a second, identical channel with the hold OFF draws a fresh blockage on every packet
    off = _chan()
    for step in range(6):
        _blocked(off, step)
    assert off.stats["nlosv_draw"] == 0


def test_a_different_blocking_vehicle_redraws_the_deviate():
    """"The same blocked link" means the same body in the way. Swap the truck and the variable
    that described the old truck no longer describes anything."""
    ch = _chan(radio_nlosv_hold=True)
    _blocked(ch, 0, blocker_vid=99)
    z_first = ch._nlosv[(1, 2)][1]
    _blocked(ch, 1, blocker_vid=99)
    assert ch._nlosv[(1, 2)][1] == z_first and ch.stats["nlosv_draw"] == 1
    _blocked(ch, 2, blocker_vid=101)                   # a different vehicle, same place, same type
    assert ch.stats["nlosv_draw"] == 2
    assert ch._nlosv[(1, 2)][0] == (101, "both_below")
    assert ch._nlosv[(1, 2)][1] != z_first


def test_leaving_and_re_entering_blockage_is_a_fresh_episode():
    ch = _chan(radio_nlosv_hold=True)
    _blocked(ch, 0)
    z_first = ch._nlosv[(1, 2)][1]
    ch.begin_step(1, [])                               # clear line of sight
    assert ch.evaluate_raw(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0, AH, AH)[2] == "LOS"
    assert (1, 2) not in ch._nlosv
    _blocked(ch, 2)
    assert ch.stats["nlosv_draw"] == 2
    assert ch._nlosv[(1, 2)][1] != z_first


def test_the_held_draw_still_tracks_the_standards_distance_term():
    """Holding the STANDARDISED deviate, not the realised dB value, keeps the standard's
    `max(0, 15*log10(d) - 41)` alive: below 541 m the loss is constant, above it, it is not."""
    ch = _chan(radio_nlosv_hold=True)
    losses = {}
    for step, d in enumerate((200.0, 400.0, 1000.0, 2000.0)):
        ch.begin_step(step, [(99, d / 2.0, 0.0, TRUCK_H)])
        _s, _sh, mu_base, sig = ch._link_state(1, 2, 0.0, 0.0, d, 0.0, d, AH, AH)
        losses[d] = ch._nlosv_loss_db(1, 2, d, mu_base, sig, None)
    assert ch.stats["nlosv_draw"] == 1, "one obstruction, one draw, four distances"
    assert losses[200.0] == pytest.approx(losses[400.0]), "flat below 541.17 m"
    assert losses[1000.0] > losses[400.0] + 3.0, losses
    assert losses[2000.0] > losses[1000.0], losses


def test_holding_the_blockage_lengthens_consecutive_loss_runs():
    """THE point of GAP 2, measured. A blockage redrawn per packet is fast fading wearing a
    large-scale term's name: it makes losses independent from CAM to CAM. Held per link, a
    badly-blocked link STAYS badly blocked, which is what burst-loss statistics -- and the
    misbehaviour detectors that read them -- actually see on the road.

    Compared at MATCHED delivery ratio, so the effect is the run STRUCTURE and not merely a
    different mean. P(loss | previous loss) is reported alongside because it is the same claim
    without the run-length bookkeeping."""
    def episode(hold, links=120, steps=80, d=250.0):
        ch = _chan(radio_nlosv_hold=hold)
        seqs = {}
        for step in range(steps):
            ch.begin_step(step, [(900_000 + i, d / 2.0, 5.0 * i, TRUCK_H) for i in range(links)])
            for i in range(links):
                heard, _r, state, _p = ch.evaluate_raw(2 * i + 1, 2 * i + 2, 0.0, 5.0 * i,
                                                       d, 5.0 * i, d, AH, AH)
                assert state == "NLOSv"
                seqs.setdefault(i, []).append(0 if heard else 1)
        runs, lost, total, pair, prior = [], 0, 0, 0, 0
        for seq in seqs.values():
            total += len(seq)
            lost += sum(seq)
            r = 0
            for v in seq:
                if v:
                    r += 1
                elif r:
                    runs.append(r)
                    r = 0
            if r:
                runs.append(r)
            pair += sum(1 for a, b in zip(seq, seq[1:]) if a == b == 1)
            prior += sum(seq[:-1])
        return 1.0 - lost / total, statistics.fmean(runs), max(runs), pair / prior

    pdr_off, run_off, max_off, cond_off = episode(False)
    pdr_on, run_on, max_on, cond_on = episode(True)
    assert abs(pdr_on - pdr_off) < 0.05, (pdr_off, pdr_on)      # matched, so runs are comparable
    assert run_on > run_off * 1.15, (run_off, run_on)
    assert cond_on > cond_off + 0.05, (cond_off, cond_on)
    assert max_on > max_off, (max_off, max_on)


def test_the_held_blockage_is_reciprocal_and_direction_order_independent():
    """A blockage is a property of the LINK, not of a direction, and not of which direction the
    engine happened to evaluate first.

    Two claims, and the second is the one with teeth. Reciprocity: both directions read one held
    deviate. Order-independence: the deviate's STREAM is keyed on the unordered pair, so a link
    first evaluated 2->1 seeds identically to one first evaluated 1->2. Keying that stream on the
    ordered pair would leave the model reciprocal but make the whole dataset depend on the order
    the reception loop happens to visit directions in."""
    def held(first, second):
        ch = _chan(radio_nlosv_hold=True)
        ch.begin_step(0, [(99, 50.0, 0.0, TRUCK_H)])
        for tx, rx in (first, second):
            x0, x1 = (0.0, 100.0) if tx == 1 else (100.0, 0.0)
            assert ch.evaluate_raw(tx, rx, x0, 0.0, x1, 0.0, 100.0, AH, AH)[2] == "NLOSv"
        assert ch.stats["nlosv_draw"] == 1, "one link, one draw, both directions"
        return ch._nlosv[(1, 2)][1]

    forward_first = held((1, 2), (2, 1))
    reverse_first = held((2, 1), (1, 2))
    assert forward_first == reverse_first, (forward_first, reverse_first)


def test_the_hold_does_not_perturb_the_shadowing_sequence():
    """The blockage gets its OWN named stream so that an A/B isolates exactly this change."""
    a, b = _chan(), _chan(radio_nlosv_hold=True)
    for ch in (a, b):
        ch.begin_step(0, [(99, 50.0, 0.0, TRUCK_H)])
    assert (a._link_state(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0, AH, AH)[1]
            == b._link_state(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0, AH, AH)[1])


# ================================================================================================
# GAP 4 -- blocker-type-dependent blockage. RETRACTED 2026-09-07, and pinned as retracted.
#
# `radio_nlosv_model="measured_boban"` shipped and was withdrawn the same day. Its 100 m LEVEL
# anchor was GEMV^2 simulator output digitised out of an IEEE-copyright figure -- which this
# project's standing rule forbids committing as a source constant -- while the arm called itself
# measurement-based; and graded against the one independent 5.9 GHz measurement in its own citation
# set (Segata et al., IEEE VNC 2013, a truck blocker on the A12) its error was +11 to +15 dB where
# the SPECIFICATION it replaced was -0.96 / +4.04 dB. See the section-2 block comment in run.py and
# docs/realism/CHANNEL-PHYSICS.md section 6 R2 for the whole negative result.
#
# The tests below are what remains: they assert the retraction is COMPLETE, so that neither the
# constants nor the knob can drift back in without someone deleting an explicit pin.
# ================================================================================================
def test_the_retracted_measured_arm_leaves_no_knob_behind():
    """The whole selector is gone, not merely defaulted away from: a single-option enum knob is an
    invitation to re-add the option."""
    assert not hasattr(PipelineConfig(), "radio_nlosv_model")
    assert "radio_nlosv_model" not in RM._ENUM_OPTIONS
    assert "radio_nlosv_model" not in RM._FIELD_META
    with pytest.raises(TypeError):
        PipelineConfig(radio_nlosv_model="measured_boban")


def test_no_ieee_figure_constant_survives_in_the_module():
    """LICENCE. The level anchor was a digitised IEEE figure. Nothing derived from it may remain as
    a source constant -- quotation with citation in prose is the only permitted form, which is why
    the retraction block comment may (and does) discuss the numbers it declines to commit."""
    for name in ("BOBAN_NLOSV_MU_AT_100M_DB", "BOBAN_NLOSV_SLOPE_DB_PER_DECADE",
                 "BOBAN_NLOSV_VALID_RANGE_M", "ABBAS_OLOS_TOTAL_SIGMA_DB",
                 "boban_nlosv_mu_db", "boban_nlosv_sigma_db"):
        assert not hasattr(RM, name), f"{name} is retracted and must not come back"


def test_the_only_nlosv_mean_left_is_the_standards_and_it_stays_falsified():
    """What the retraction leaves behind is not a fixed model: it is the specified term, whose own
    falsification stays pinned in test_geometric_channel.py section 8. That is the honest state --
    wrong in a way that is measured, rather than wrong in a way that was measured worse."""
    ch = _chan(radio_nlosv_hold=True)
    ch.begin_step(0, [(99, 50.0, 0.0, TRUCK_H)])
    _s, _sh, mu, sig = ch._link_state(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0, AH, AH)
    ch._shadow[(1, 2)]["z"] = 0.0                       # z = 0: read the MEAN, not a draw
    assert ch._nlosv_loss_db(1, 2, 100.0, mu, sig, None) == pytest.approx(
        RM.TR37885_NLOSV["both_below"][0])
    # flat across the whole band our scenarios occupy -- the property the retracted arm existed to
    # fix, still unfixed, still recorded
    flat = {round(RM.tr37885_nlosv_mu_db(9.0, d), 9) for d in (10.0, 30.0, 100.0, 300.0, 500.0)}
    assert flat == {9.0}, flat

# ================================================================================================
# GAP 3 -- the two-ray ground-reflection breakpoint. OPT-IN and UNVALIDATED in magnitude.
# ================================================================================================
def test_the_breakpoint_sits_at_the_physical_distance():
    """d_b = 4*h_TX*h_RX/lambda: 201.5 m at our TR 37.885 Type 2 antennas and 5.9 GHz, which is
    INSIDE the operating range the docs quote (338 m awareness-equivalent).

    RE-PINNED 2026-09-07 from 177.1 m, and NOT because the breakpoint model changed: d_b is
    quadratic in the antenna height, so correcting V2X_ANTENNA_HEIGHT_M from the standard's
    PEDESTRIAN 1.5 m to its Type 2 1.6 m moves the breakpoint out by (1.6/1.5)^2 = 13.8%. That is a
    consequence of the height fix worth stating: this term's onset is now 24 m further out."""
    assert RM.two_ray_breakpoint_m(AH, AH) == pytest.approx(201.5, abs=0.1)
    assert RM.two_ray_breakpoint_m(1.5, 1.5) == pytest.approx(177.1, abs=0.1)   # the old geometry
    assert RM.two_ray_breakpoint_m(AH, RM.RSU_ANTENNA_HEIGHT_M) > RM.two_ray_breakpoint_m(AH, AH)
    # scaling is linear in each height and inverse in wavelength
    assert RM.two_ray_breakpoint_m(3.0, 1.5) == pytest.approx(2 * RM.two_ray_breakpoint_m(1.5, 1.5))


def test_the_breakpoint_is_continuous_and_only_steepens_beyond_itself():
    b = RM.TR37885_PATHLOSS["urban_los"][1]
    d_b = RM.two_ray_breakpoint_m(AH, AH)
    assert RM.two_ray_excess_db(d_b, d_b, b) == 0.0
    assert RM.two_ray_excess_db(d_b * 0.999, d_b, b) == 0.0
    assert RM.two_ray_excess_db(d_b * 1.001, d_b, b) == pytest.approx(0.0, abs=0.02)
    # exactly (slope - b) dB per decade beyond it
    assert RM.two_ray_excess_db(d_b * 10.0, d_b, b) == pytest.approx(40.0 - b)
    # a slope at or below the model's own is a no-op, never a gain
    assert RM.two_ray_excess_db(d_b * 10.0, d_b, b, slope_db_dec=b) == 0.0
    assert RM.two_ray_excess_db(d_b * 10.0, d_b, b, slope_db_dec=1.0) == 0.0


def test_the_breakpoint_moves_the_link_budget_only_past_the_breakpoint():
    def rssi(d, **kw):
        ch = _chan(**kw)
        ch.begin_step(0, [])
        heard, r, state, _ = ch.evaluate_raw(1, 2, 0.0, 0.0, d, 0.0, d, AH, AH)
        assert state == "LOS"
        return r

    for d in (50.0, 150.0):
        assert rssi(d, radio_breakpoint="two_ray") == pytest.approx(rssi(d), abs=1e-9)
    for d in (300.0, 500.0):
        assert rssi(d, radio_breakpoint="two_ray") < rssi(d) - 1.0


def test_the_breakpoint_is_not_applied_to_a_building_blocked_path():
    """A ground reflection is a LINE-OF-SIGHT interference effect. Stacking it on a path that is
    already diffracting round a building asserts a mechanism that geometry does not have."""
    def rssi(model):
        ch = _chan(radio_nlosb_density_per_km=1e6, radio_breakpoint=model)   # everything NLOSb
        ch.begin_step(0, [])
        heard, r, state, _ = ch.evaluate_raw(1, 2, 0.0, 0.0, 400.0, 0.0, 400.0, AH, AH)
        assert state == "NLOSb"
        return r

    assert rssi("two_ray") == pytest.approx(rssi("none"), abs=1e-9)


# ================================================================================================
# GAP 5 -- the blocker footprint. CONFORMANT: TR 37.885 V15.3.0 clause 6.1.2 body widths.
# ================================================================================================
def test_the_spec_widths_are_the_clause_6_1_2_bodies_halved():
    """"Type 1/2 ... width 2.0 meters"; "Type 3 (truck/bus) ... width 2.6 meters"."""
    assert RM.tr37885_blocker_half_width_m(CAR_H) == pytest.approx(1.0)
    assert RM.tr37885_blocker_half_width_m(TRUCK_H) == pytest.approx(1.3)
    # the shipped uniform width already IS the passenger value, so this only ever widens trucks
    assert RM.tr37885_blocker_half_width_m(CAR_H) == RM.GEO_BLOCKER_HALF_WIDTH_M
    assert RM.tr37885_blocker_half_width_m(9.9) == RM.GEO_BLOCKER_HALF_WIDTH_M   # unknown -> uniform


def test_a_truck_occludes_a_wider_corridor_than_a_car():
    """Offset the blocker 1.15 m off the line: inside the truck's 1.3 m half-width, outside the
    car's 1.0 m. Under the uniform footprint neither blocks, which is the defect."""
    def state(blocker_h, mode):
        ch = _chan(radio_blocker_width=mode)
        st = [_station(1, 0.0, 0.0), _station(2, 100.0, 0.0),
              _station(9, 50.0, 1.15, blocker_h=blocker_h)]
        ch.begin_step(_frame(0, st))
        return ch.evaluate_raw(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0, AH, AH)[2]

    assert state(TRUCK_H, "uniform") == "LOS"          # the defect: one width for every vehicle
    assert state(CAR_H, "uniform") == "LOS"
    assert state(TRUCK_H, "tr37885") == "NLOSv"        # 2.6 m body reaches the line
    assert state(CAR_H, "tr37885") == "LOS"            # 2.0 m body does not


# ================================================================================================
# GAP 6 -- temporal correlation of the small-scale fade. Clarke/Jakes, opt-in.
# ================================================================================================
def test_the_incomplete_gamma_inverse_round_trips():
    """The fade transform is only as good as this inverse; graded on its own defining identity."""
    worst = 0.0
    for a in (1.0, 1.5, 3.0):
        for p in (1e-9, 1e-6, 1e-3, 0.01, 0.1, 0.5, 0.9, 0.99, 1 - 1e-9):
            x = RM._gamma_p_inv(a, p)
            worst = max(worst, abs(RM._gamma_p(a, x) - p) / p)
    assert worst < 1e-6, worst


def test_bessel_j0_matches_its_defining_values():
    """A&S 9.4.1/9.4.3, graded on known values rather than on a second copy of the polynomial."""
    assert RM.bessel_j0(0.0) == pytest.approx(1.0, abs=1e-7)
    assert RM.bessel_j0(1.0) == pytest.approx(0.7651976866, abs=1e-7)
    assert RM.bessel_j0(2.404825558) == pytest.approx(0.0, abs=1e-7)      # the first zero
    assert RM.bessel_j0(5.520078110) == pytest.approx(0.0, abs=1e-7)      # the second zero
    assert RM.bessel_j0(10.0) == pytest.approx(-0.2459357645, abs=1e-7)
    assert RM.bessel_j0(-3.0) == pytest.approx(RM.bessel_j0(3.0))         # even


@pytest.mark.parametrize("m", [1.0, 1.5, 3.0])
def test_the_correlated_fade_keeps_the_nakagami_marginal_exactly(m):
    """The whole point of the probability-integral transform: only the TIME structure changes.

    Graded against the closed-form moments of Gamma(shape=m, scale=1/m) -- unit mean, variance
    1/m -- which is what `gammavariate(m, 1/m)` draws in the default arm."""
    rng = __import__("random").Random(4)
    xs = [RM.nakagami_power_from_normal(m, rng.gauss(0.0, 1.0)) for _ in range(60_000)]
    assert statistics.fmean(xs) == pytest.approx(1.0, abs=0.02)
    assert statistics.pvariance(xs) == pytest.approx(1.0 / m, rel=0.05)
    # the transform is monotone, so a deeper normal is a deeper fade -- never a reordering
    assert (RM.nakagami_power_from_normal(m, -2.0) < RM.nakagami_power_from_normal(m, 0.0)
            < RM.nakagami_power_from_normal(m, 2.0))


@pytest.mark.parametrize("m", [1.0, 1.5, 3.0])
def test_the_fade_transform_is_quantile_exact_deep_into_the_fading_tail(m):
    """"Exact marginal" graded where it MATTERS, on the defining identity rather than on two
    sample moments: P(m, m*X(z)) must equal Phi(z) at every z, including far into the lower tail.

    The tail is not a detail here -- it IS the packet-loss behaviour, because a deep fade is what
    drops a CAM. A cheap Wilson-Hilferty cube-root approximation reproduces the mean and variance
    to well inside a percent and still misses the 1-in-10000 fade by tens of dB, so a moments-only
    test cannot tell the two apart. This one can."""
    worst = 0.0
    for z in (-4.0, -3.0, -2.0, -1.0, -0.25, 0.0, 0.25, 1.0, 2.0, 3.0, 4.0):
        x = RM.nakagami_power_from_normal(m, z)
        want = 0.5 * (1.0 + math.erf(z / math.sqrt(2.0)))
        worst = max(worst, abs(RM._gamma_p(m, m * x) - want) / want)
    assert worst < 1e-6, worst
    # the deep-fade end must stay a real, positive power rather than collapsing to the floor
    assert RM.nakagami_power_from_normal(m, -4.0) > 1e-9


@pytest.mark.parametrize("m", [1.0, 1.5, 3.0])
def test_an_extreme_draw_saturates_instead_of_killing_the_run(m):
    """Phi(z) rounds to exactly 1.0 above about z = 8.3, and the un-saturated inverse then
    evaluates log(1 - p) = log(0) and raises. One freak draw in a long run would take the whole run
    down, so both tails must saturate to a finite power rather than raise."""
    for z in (8.3, 9.0, 40.0, 1e6, float("inf")):
        hi = RM.nakagami_power_from_normal(m, z)
        assert math.isfinite(hi) and hi > 1.0, (z, hi)
    for z in (-8.3, -40.0, -1e6, float("-inf")):
        lo = RM.nakagami_power_from_normal(m, z)
        assert math.isfinite(lo) and lo >= 0.0, (z, lo)
    # monotone right through the saturated region
    assert (RM.nakagami_power_from_normal(m, -40.0) <= RM.nakagami_power_from_normal(m, 0.0)
            <= RM.nakagami_power_from_normal(m, 40.0))


def test_the_jakes_correlation_collapses_at_speed_and_persists_in_a_queue():
    """The defect is scoped, not universal: i.i.d. is right at 30 m/s and wrong at a stop line."""
    dt = 0.1                                     # TR 37.885's own 100 ms link-state cadence
    assert RM.jakes_rho(0.0, dt) == 1.0                              # stationary pair: frozen
    # |J0| <= 1 exactly, so the AR(1) can never be handed an unstable coefficient
    assert all(abs(RM.jakes_rho(v, t)) <= 1.0
               for v in (0.0, 1e-9, 0.05, 1.0, 7.3, 30.0) for t in (0.01, 0.1, 1.0))
    assert RM.jakes_rho(0.1, dt) > 0.6                               # crawling: strongly correlated
    assert abs(RM.jakes_rho(30.0, dt)) < 0.1                         # at speed: effectively i.i.d.
    # and the engine's DEFAULT 1 s step is already past coherence for anything but a stopped pair
    assert abs(RM.jakes_rho(1.0, 1.0)) < 0.1


def _fade_series(correlation, v_rel, steps=4000, dt=0.1, m=1.0):
    """Per-step fade POWER on one link whose endpoints close at exactly `v_rel`.

    The fade term is driven directly, with the two endpoints' velocities set by hand, so that path
    loss, shadowing and blockage are all out of the picture and the only thing under measurement is
    the fade's own time structure. Driving it through `evaluate_raw` instead would need the two
    stations to MOVE relative to each other, which moves the link length and the shadowing with it
    and confounds exactly the autocorrelation being measured."""
    import random as _random
    cfg = PipelineConfig(seed=9, radio_model="geometric", radio_nlosb_density_per_km=0.0,
                         radio_fading_correlation=correlation)
    ch = RM.GeometricChannel(cfg, buildings=None, dt=dt)
    prng = _random.Random("fade-series")
    out = []
    for step in range(steps):
        ch.step = step
        ch._kin[1] = (0.0, 0.0, 0.0, 0.0, 0.0)          # (x, y, heading, vx, vy)
        ch._kin[2] = (100.0, 0.0, 0.0, v_rel, 0.0)
        out.append(ch._fade_power(1, 2, m, prng))
    return out


def _lag1(xs):
    mu = statistics.fmean(xs)
    den = sum((v - mu) ** 2 for v in xs)
    return sum((a - mu) * (b - mu) for a, b in zip(xs, xs[1:])) / den


def test_a_stopped_pairs_fade_persists_across_cams_instead_of_being_resampled():
    """THE gap, measured. At a stop line the channel is frozen: a deep fade that should still be
    there on the next CAM must not be resampled away. At exactly zero relative speed the Doppler
    is zero, J0(0) = 1, and the correlated arm holds the fade EXACTLY."""
    frozen = _fade_series("jakes", 0.0, steps=50)
    assert len(set(frozen)) == 1, "a zero-Doppler channel must not move at all"
    crawling = _lag1(_fade_series("jakes", 0.1))
    assert crawling > 0.5, crawling
    # ...where the historic arm resamples it every single packet, at every speed
    assert abs(_lag1(_fade_series("none", 0.0))) < 0.05
    assert abs(_lag1(_fade_series("none", 0.1))) < 0.05


def test_at_speed_the_correlated_arm_is_indistinguishable_from_iid():
    """The honest half: i.i.d. is RIGHT at 30 m/s (coherence time 0.72 ms against a 100 ms CAM
    period), so this term must not quietly change the regime it was already modelling correctly."""
    assert abs(_lag1(_fade_series("jakes", 30.0))) < 0.15


def test_the_correlated_arm_adds_no_power():
    """Correlation redistributes fades in time; it must not add or remove any power. Measured on
    the decorrelated regime, where the sample mean of a 4000-step series is meaningful."""
    for m in (1.0, 3.0):
        iid = statistics.fmean(_fade_series("none", 30.0, m=m))
        jakes = statistics.fmean(_fade_series("jakes", 30.0, m=m))
        assert iid == pytest.approx(1.0, abs=0.06), (m, iid)
        assert jakes == pytest.approx(1.0, abs=0.06), (m, jakes)


def test_the_fade_state_advances_once_per_step_not_once_per_packet():
    """A Sybil attacker broadcasting several ghosts in one step transmits through ONE channel at
    ONE instant, so every ghost must see the same fade -- which is what carrying the fade as a
    per-step state, rather than a per-call draw, buys."""
    import random as _random
    ch = _chan(radio_fading_correlation="jakes")
    prng = _random.Random("ghosts")
    ch.step = 0
    ch._kin[1] = (0.0, 0.0, 0.0, 0.0, 0.0)
    ch._kin[2] = (100.0, 0.0, 0.0, 5.0, 0.0)
    assert len({ch._fade_power(1, 2, 1.0, prng) for _ in range(5)}) == 1
    ch.step = 1
    assert len({ch._fade_power(1, 2, 1.0, prng) for _ in range(5)}) == 1


def test_the_fade_correlation_also_refuses_the_legacy_begin_step():
    ch = _chan(radio_fading_correlation="jakes")
    with pytest.raises(ValueError, match="StepFrame"):
        ch.begin_step(0, [])


# ================================================================================================
# Determinism and default-safety, across every new term at once.
# ================================================================================================
_ALL_ON = dict(radio_model="geometric", radio_nlosv_hold=True,
               radio_antenna_pattern="tr37885_opt1", radio_blocker_width="tr37885",
               radio_breakpoint="two_ray", radio_fading_correlation="jakes")


def _digest(tmp, name, **kw):
    cfg = PipelineConfig(seed=17, traffic_flow=True, road_network="grid", duration_s=40,
                         arrival_rate=1.5, grid_w=4, grid_h=4, attacker_pct=0.2,
                         out_dir=str(tmp / name), **kw)
    return run_pipeline(cfg).data_digest


def test_every_new_term_together_is_deterministic(tmp_path):
    """Byte-identical output for the same seed and config, with all five terms live at once."""
    assert _digest(tmp_path, "a", **_ALL_ON) == _digest(tmp_path, "b", **_ALL_ON)


def test_each_new_term_is_load_bearing_on_the_geometric_model(tmp_path):
    """Each knob must MOVE the geometric dataset -- a knob that changes nothing is not a feature,
    and a test suite that cannot tell is not testing one."""
    base = _digest(tmp_path, "base", radio_model="geometric")
    moved = {}
    for i, kw in enumerate(({"radio_nlosv_hold": True},
                            {"radio_antenna_pattern": "tr37885_opt1"},
                            {"radio_blocker_width": "tr37885"},
                            {"radio_breakpoint": "two_ray"},
                            {"radio_fading_correlation": "jakes"})):
        key = next(iter(kw))
        moved[key] = _digest(tmp_path, f"k{i}", radio_model="geometric", **kw)
        assert moved[key] != base, f"{key} changed nothing"
    assert len(set(moved.values())) == len(moved), "two knobs collapsed to the same dataset"


def test_the_defaults_leave_the_geometric_model_exactly_where_it_was(tmp_path):
    """Spelling every knob out at its documented default must reproduce the un-spelled run."""
    base = _digest(tmp_path, "d0", radio_model="geometric")
    spelled = _digest(tmp_path, "d1", radio_model="geometric", radio_nlosv_hold=False,
                      radio_antenna_pattern="none",
                      radio_antenna_gain_dbi=RM.TR37885_ANT_MAX_GAIN_DBI,
                      radio_blocker_width="uniform", radio_breakpoint="none",
                      radio_breakpoint_slope_db_per_decade=RM.TWO_RAY_SLOPE_DB_PER_DECADE,
                      radio_fading_correlation="none")
    assert base == spelled


def test_none_of_the_new_per_link_state_leaks_when_vehicles_retire():
    """Each new term carries per-link or per-station state, and an 8 h run retires thousands of
    vehicles. Anything `prune` forgets is an unbounded leak that only shows up at scale."""
    ch = _chan(radio_nlosv_hold=True, radio_antenna_pattern="tr37885_opt1",
               radio_fading_correlation="jakes")
    sts = [_station(1, 0.0, 0.0), _station(2, 100.0, 0.0), _station(9, 50.0, 0.0,
                                                                    blocker_h=TRUCK_H)]
    for step in (0, 1):
        ch.begin_step(_frame(step, sts))
        ch.evaluate_raw(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0, AH, AH)
    assert ch._nlosv and ch._kin and ch._fade and ch._shadow and ch._packet
    ch.prune(set())                                    # every vehicle has left the simulation
    for name in ("_shadow", "_packet", "_nlosv", "_kin", "_fade"):
        assert getattr(ch, name) == {}, f"{name} leaked {getattr(ch, name)}"


def test_validate_config_rejects_every_new_knob_when_it_is_wrong():
    for kw in ({"radio_antenna_pattern": "option2"},
               {"radio_blocker_width": "wide"}, {"radio_breakpoint": "three_ray"},
               {"radio_fading_correlation": "clarke"},
               {"radio_antenna_gain_dbi": 99.0}, {"radio_breakpoint_slope_db_per_decade": -1.0}):
        with pytest.raises(ValueError):
            validate_config(PipelineConfig(radio_model="geometric", **kw))


# ================================================================================================
# THE COLLUSION RSSI ORACLE. A SECURITY PIN, AND THE ONE THAT MUST SURVIVE THE NEXT TERM.
#
# A colluder fabricating a misbehaviour report has never received a frame from its victim, so the
# `rssi_dbm` column of that report is SYNTHESISED. If the synthesis and the reception loop compute
# the link budget separately, the difference between them is a free classifier on the dataset:
# "this report's RSSI is 6 dB high, therefore it is a fabrication".
#
# THAT IS MEASURED HISTORY, not a hypothetical. The collusion path used to hand-roll
# `tx_dbm - pathloss(state, d)`; with the shipped defaults it agreed with the real budget to
# +0.036 dB and the design was sound. Switching on `radio_antenna_pattern` -- one knob, no change to
# the collusion code -- opened +5.824 dB (1.15 sigma, AUC ~= 0.79 for a single-threshold
# classifier), because the antenna term existed on one side of the model only.
#
# The tests below are therefore written so that they FAIL FOR A KNOB THAT DID NOT EXIST WHEN THEY
# WERE WRITTEN: the arm list is derived from the channel's own `getattr(cfg, "radio_...")` reads, so
# adding an opt-in term without covering it here is a test failure, not a silent hole.
# ================================================================================================
#: Tolerance on the fabricated-minus-genuine offset. At n = 4000 pairs one standard error of the
#: difference is ~0.11 dB, and the worst offset measured across every arm and geometry is 0.199 dB
#: -- so this is ~4.5 se, comfortably above the residual sampling-importance bias and **12x below
#: the +5.824 dB the antenna term used to open**. Deterministic seeds, so it is not flaky; the
#: headroom is there so that a legitimate refactor of the draw order cannot cause a false alarm.
_SYNTH_TOL_DB = 0.50


def _channel_knob_names():
    """Every `radio_*` config field `GeometricChannel.__init__` reads, from its own source.

    Deliberately introspective rather than a hand-written list: the point of this test is to catch
    the term SOMEONE ELSE adds later, and a hand-written list cannot."""
    src = inspect.getsource(RM.GeometricChannel.__init__)
    return set(re.findall(r'getattr\(cfg,\s*"(radio_[a-z_]+)"', src))


#: Non-default value per knob, used to build the one-term-on arms. Enum knobs take their non-default
#: option from `_ENUM_OPTIONS` automatically; anything else has to be named here, and the coverage
#: test below fails if a knob is named in neither place.
_NUMERIC_ARM_VALUES = {"radio_antenna_gain_dbi": 6.0,
                       "radio_breakpoint_slope_db_per_decade": 28.0,
                       "radio_nlosv_hold": True}


def _synthesis_arms():
    """[(name, config overrides)] -- off, every term alone, and every term at once."""
    arms = [("off", {})]
    every = {}
    for knob in sorted(_channel_knob_names()):
        opts = RM._ENUM_OPTIONS.get(knob)
        if opts:
            default = getattr(PipelineConfig(), knob)
            for opt in opts:
                if opt != default:
                    arms.append((f"{knob}={opt}", {knob: opt}))
                    every[knob] = opt
        elif knob in _NUMERIC_ARM_VALUES:
            v = _NUMERIC_ARM_VALUES[knob]
            arms.append((f"{knob}={v}", {knob: v}))
            every.setdefault(knob, v)
        else:
            raise AssertionError(
                f"GeometricChannel now reads a config knob this pin does not cover: {knob!r}. "
                f"Add a non-default value to _NUMERIC_ARM_VALUES (or an entry to _ENUM_OPTIONS) so "
                f"that the fabricated-vs-genuine RSSI check exercises it. This is the collusion "
                f"oracle guard refusing to be silently incomplete -- see the section comment.")
    # the antenna gain and the pattern have to travel together for the gain to do anything
    every["radio_antenna_pattern"] = "tr37885_opt1"
    arms.append(("all_on", every))
    return arms


def test_every_channel_knob_is_covered_by_the_synthesis_arms():
    """The guard on the guard. A new opt-in term that is neither an enum nor listed above makes
    `_synthesis_arms` raise, which is the loud failure this pin exists to produce."""
    knobs = _channel_knob_names()
    assert "radio_antenna_pattern" in knobs and "radio_breakpoint" in knobs, knobs
    covered = {k for k in knobs if k in RM._ENUM_OPTIONS} | set(_NUMERIC_ARM_VALUES)
    assert knobs <= covered, f"channel knobs with no synthesis arm: {sorted(knobs - covered)}"
    assert len(_synthesis_arms()) >= 1 + len(knobs)


def _synth_vs_genuine(over, n=4000, d=150.0, blocker_h=None):
    """(genuine mean, fabricated mean) rssi in dBm on the same true links, conditioned on decoding.

    `n` INDEPENDENT pairs 5 km apart, so each pair carries its own shadowing stream and no pair is
    in any other pair's way. The fabricated column is drawn from a stream of its own, exactly as a
    colluder's `collude_fab` stream is."""
    ch = _chan(**over)
    def sts(step):
        out = {}
        for k in range(n):
            x0 = k * 5000.0 + step * 10.0
            out[3 * k] = _station(3 * k, x0, 0.0)
            out[3 * k + 1] = _station(3 * k + 1, x0 + d, 0.0)
            if blocker_h:
                out[3 * k + 2] = _station(3 * k + 2, x0 + d / 2.0, 0.0, blocker_h=blocker_h)
        return list(out.values())
    _settle(ch, [sts(0), sts(1)])
    place = {s.vid: s.x for s in sts(1)}
    gen, fab, states = [], [], set()
    for k in range(n):
        a, b = 3 * k, 3 * k + 1
        heard, rssi, state, _ = ch.evaluate_raw(a, b, place[a], 0.0, place[b], 0.0, d, AH, AH)
        states.add(state)
        if heard:
            gen.append(rssi)
        fab.append(ch.synthesize_rx_dbm(a, b, place[a], 0.0, place[b], 0.0, d,
                                        random.Random(f"fab:{k}")))
    assert len(states) == 1, states
    return statistics.fmean(gen), statistics.fmean(fab), states.pop()


@pytest.mark.parametrize("arm,over", _synthesis_arms())
def test_a_fabricated_rssi_is_indistinguishable_from_a_genuine_one(arm, over):
    """THE PIN. For every term, on or off, the fabricated population sits on the genuine one.

    Verified RED by reverting `synthesize_rx_dbm` to the hand-rolled budget it replaced: the
    `radio_antenna_pattern=tr37885_opt1` and `all_on` arms fail by 5.8 dB, and the rest pass --
    which is exactly the shape of the defect, a term-by-term hole rather than a general one."""
    mg, mf, _state = _synth_vs_genuine(over)
    assert abs(mf - mg) < _SYNTH_TOL_DB, f"{arm}: fabricated {mf:.3f} vs genuine {mg:.3f} dBm"


def test_the_synthesis_matches_on_a_blocked_link_and_past_the_breakpoint_too():
    """The 150 m LOS geometry the original finding used cannot see two of the terms: the breakpoint
    does not switch on until 201.5 m and the blockage draw needs a blocker. Both are checked here,
    and the NLOSv case is the one that also grades the CONDITIONING -- 26% of genuine frames on it
    fail the decode floor, and a synthesis that conditioned only its fade (as the first version did)
    lands 1.16 dB low."""
    for over in ({}, {"radio_nlosv_hold": True}, {"radio_antenna_pattern": "tr37885_opt1"}):
        mg, mf, state = _synth_vs_genuine(over, d=150.0, blocker_h=TRUCK_H)
        assert state == "NLOSv"
        assert abs(mf - mg) < _SYNTH_TOL_DB, (over, mf, mg)
    for over in ({}, {"radio_breakpoint": "two_ray"},
                 {"radio_breakpoint": "two_ray", "radio_breakpoint_slope_db_per_decade": 28.0}):
        mg, mf, state = _synth_vs_genuine(over, d=300.0)
        assert state == "LOS"
        assert abs(mf - mg) < _SYNTH_TOL_DB, (over, mf, mg)


def test_the_link_budget_has_exactly_one_site_in_the_whole_class():
    """Agreement measured at one geometry is evidence; sharing the code is the guarantee.

    Every link budget in this model starts from `self.tx_dbm`, so a SECOND budget anywhere in the
    class has to mention it. Outside the constructor (which uses it for the candidate window, not
    for a received power) exactly one method may: `mean_rx_dbm`. This is the assertion that would
    have failed the day the collusion path grew a budget of its own."""
    methods = {n: f for n, f in vars(RM.GeometricChannel).items() if inspect.isfunction(f)}
    users = sorted(n for n, f in methods.items()
                   if "self.tx_dbm" in inspect.getsource(f) and n != "__init__")
    assert users == ["mean_rx_dbm"], f"a second link budget lives in {users}"
    for caller in ("evaluate_raw", "synthesize_rx_dbm"):
        assert "self.mean_rx_dbm(" in inspect.getsource(methods[caller]), caller


def test_perturbing_the_budget_site_moves_both_populations_together():
    """The behavioural half of the same claim: shift `mean_rx_dbm` and BOTH the genuine RSSI and the
    fabricated one shift with it. A term computed outside that method would move only one."""
    def rssi(patched, d=50.0):
        ch = _chan(radio_antenna_pattern="tr37885_opt1")
        _settle(ch, [[_station(1, 0.0, 0.0), _station(2, d, 0.0)],
                     [_station(1, 10.0, 0.0), _station(2, d + 10.0, 0.0)]])
        if patched:
            ch.mean_rx_dbm = (lambda *a, _c=ch, **k:
                              RM.GeometricChannel.mean_rx_dbm(_c, *a, **k) + 100.0)
        args = (1, 2, 10.0, 0.0, d + 10.0, 0.0, d)
        return ch.evaluate_raw(*args, AH, AH)[1], ch.synthesize_rx_dbm(*args, random.Random("s"))

    (g0, f0), (g1, f1) = rssi(False), rssi(True)
    assert g1 - g0 == pytest.approx(100.0, abs=1e-6), "the genuine path lost the site"
    # the synthesis picks its large-scale candidate by P(decode); at 50 m that probability is 1 to
    # within 1e-6 both before and after, so the same candidate and the same fade quantile are drawn
    assert f1 - f0 == pytest.approx(100.0, abs=1e-3), "the synthesis lost the site"


def test_the_synthesis_never_emits_a_value_below_the_decode_floor_or_a_point_mass_at_it():
    """A report is only filed on a frame that DECODED. Two failure modes are pinned: a value under
    the floor (which no genuine report can carry), and a pile-up EXACTLY at the floor, which is what
    clamping would produce and is just as good an oracle as a NULL."""
    ch = _chan()
    ch.begin_step(_frame(0, [_station(1, 0.0, 0.0), _station(2, 400.0, 0.0)]))
    vals = [ch.synthesize_rx_dbm(1, 2, 0.0, 0.0, 400.0, 0.0, 400.0, random.Random(f"h:{k}"))
            for k in range(2000)]
    assert min(vals) >= ch.decode_floor_dbm, min(vals)
    assert sum(1 for v in vals if v == ch.decode_floor_dbm) == 0, "point mass at the floor"
    # ... including on a link that is hopeless by tens of dB, where the tail branch is what answers
    far = [ch.synthesize_rx_dbm(1, 2, 0.0, 0.0, 3000.0, 0.0, 3000.0, random.Random(f"f:{k}"))
           for k in range(500)]
    assert min(far) >= ch.decode_floor_dbm and len(set(far)) == len(far)


def test_end_to_end_the_fabricated_column_follows_the_link_budget(tmp_path):
    """The unit pins above never execute `run.py`'s CALL into the synthesis -- its argument order,
    its geometry source, its rng. This one does, on real datasets, with the antenna pattern: the arm
    on which the original oracle measured AUC ~= 0.79 from a single threshold.

    WHAT IS COMPARED, AND WHY IT IS NOT "fabricated vs honest IN ONE RUN". Those two populations are
    not drawn from the same link-length distribution -- a colluder frames victims anywhere inside
    `radio_range_m`, while a genuine report comes from a link that actually delivered, which skews
    short -- so on the reference scene they differ by ~9 dB even when the model is exactly right,
    and that gap is a property of VICTIM SELECTION, not of the RSSI column (measured, and recorded
    as an unfixed finding, in docs/realism/CHANNEL-PHYSICS.md section 6 R9). What must hold instead
    is that BOTH populations respond to the link budget the same way: switch on a term worth
    2 x 3 dBi and both move by about 6 dB. Under the hand-rolled second budget the fabricated column
    moved by ZERO.

    The scene is deliberately SHORT-RANGE with the canyon fallback off, so that essentially every
    framed link is comfortably decodable. On a scene where most framed links sit near the decode
    floor, conditioning on decoding absorbs most of a budget change in BOTH columns and the test
    would measure the scene rather than the code."""
    def dataset(name, **kw):
        res = run_pipeline(PipelineConfig(
            seed=42, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=2.0,
            grid_w=5, grid_h=5, grid_block_m=100.0, traffic_lights=True, attacker_pct=0.2,
            attack_type="ConstPos", attack_intensity=1.0, collude_pct=0.5, victim_pct=0.15,
            radio_range_m=200.0, radio_model="geometric", radio_cap_max_mult=2.0,
            radio_nlosb_density_per_km=0.0, emit_sample_prob=1.0,
            out_dir=str(tmp_path / name), **kw))

        def rows(fn):
            with open(f"{res.out_dir}/{fn}", encoding="utf-8") as fh:
                return [json.loads(ln) for ln in fh if ln.strip()]

        labs = {r["report_id"]: r for r in rows("ground_truth/gt_report_labels.jsonl")}
        rep = rows("ma/ma_reports.jsonl")
        fab = [r["rssi_dbm"] for r in rep
               if labs[r["report_id"]]["report_correctness"] == "malicious_false_report"]
        hon = [r["rssi_dbm"] for r in rep
               if labs[r["report_id"]]["report_correctness"] != "malicious_false_report"]
        assert len(fab) > 50 and len(hon) > 50, (name, len(fab), len(hon))
        assert all(v is not None for v in fab + hon), "a NULL rssi is a fabrication oracle"
        assert min(fab) >= -81.0 - 1e-6, (name, min(fab))
        assert sum(1 for v in fab if abs(v + 81.0) < 0.005) <= 0.02 * len(fab), "clamp fingerprint"
        return statistics.fmean(fab), statistics.fmean(hon)

    fab_off, hon_off = dataset("ant_off")
    fab_on, hon_on = dataset("ant_on", radio_antenna_pattern="tr37885_opt1")
    assert 3.5 < hon_on - hon_off < 8.5, (hon_off, hon_on)      # the genuine column moves ~6 dB
    assert 3.5 < fab_on - fab_off < 8.5, (fab_off, fab_on)      # ... and so must the fabricated one
    assert abs((fab_on - fab_off) - (hon_on - hon_off)) < 2.0, (fab_on - fab_off, hon_on - hon_off)


def test_the_synthesis_draws_nothing_from_the_reception_loops_streams():
    """A colluder must not be able to move a genuine link by filing a report. Every draw comes from
    the caller's stream, so the genuine sequence is bit-identical with and without the synthesis."""
    def run(with_synthesis):
        ch = _chan()
        out = []
        for step in range(4):
            ch.begin_step(_frame(step, [_station(1, 10.0 * step, 0.0),
                                        _station(2, 150.0 + 10.0 * step, 0.0)]))
            if with_synthesis:
                for k in range(3):
                    ch.synthesize_rx_dbm(1, 2, 10.0 * step, 0.0, 150.0 + 10.0 * step, 0.0, 150.0,
                                         random.Random(f"c:{step}:{k}"))
            out.append(ch.evaluate_raw(1, 2, 10.0 * step, 0.0, 150.0 + 10.0 * step, 0.0,
                                       150.0, AH, AH)[1])
        return out

    assert run(False) == run(True)
