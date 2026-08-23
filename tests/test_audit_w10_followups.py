"""Regression tests for the wave-8-10 consolidation-audit follow-ups (VRU/DENM/GUI hardening)."""
import sys
from pathlib import Path

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig
from scms_sim_ref.mock_pipeline.run import validate_config, KNOWN_ATTACK_TYPES
from scms_sim_ref.schemas.records import is_forbidden_feature_key


def test_vru_pct_ge_1_rejected():
    """#1: vru_pct is a fraction of actors (ratio vru/(1-vru)); >=1 must be rejected, not ZeroDiv."""
    with pytest.raises(ValueError, match="vru_pct must be < 1"):
        validate_config(PipelineConfig(vru_pct=1.0))
    validate_config(PipelineConfig(vru_pct=0.9))          # just under the cap validates


def test_corpus_report_covers_every_opt_in_family_and_type():
    """#2: corpus_report's attack space must track KNOWN_ATTACK_TYPES so stratify/coverage/LOFO see
    the newer opt-in families (event/FakeHazard, identity/VruImpersonation), not just the catalog."""
    from scms_sim_ref.datagen import corpus_report as cr
    assert set(cr.ALL_TYPES) == set(KNOWN_ATTACK_TYPES)
    assert "FakeHazard" in cr.ALL_TYPES and "VruImpersonation" in cr.ALL_TYPES
    assert "event" in cr.ALL_FAMILIES and "combined" in cr.ALL_FAMILIES
    assert "event" in cr.OPT_IN_FAMILIES                   # reachable only via the opt-in DENM tuple


def test_leakage_registry_guards_vru_and_denm_oracle_labels():
    """#4: the oracle labels is_vru/is_fake are in the leakage firewall, but the MA-visible
    is_vru_declared feature is NOT falsely caught (exact-match, not prefix)."""
    assert is_forbidden_feature_key("is_vru") and is_forbidden_feature_key("is_fake")
    assert not is_forbidden_feature_key("is_vru_declared")   # the legitimate MA-visible feature
    assert not is_forbidden_feature_key("n_denms_sent")


def test_gui_exposes_denm_and_fakehazard():
    """#3: the GUI can reach the DENM layer (pf_denm_rate wired to --denm-rate) and FakeHazard."""
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "gui"))
    import server
    denm = [c for c in server.CONFIG_SPEC if c["name"] == "pf_denm_rate"]
    assert denm and denm[0]["arg"] == "--denm-rate" and denm[0]["default"] == 0.0
    assert "FakeHazard" in server.ATTACK_TYPES
