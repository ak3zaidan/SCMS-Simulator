"""Unit test for the leave-one-attack-family-out generalization metric."""

import numpy as np
import pandas as pd

from scms_sim_ref.datagen import benchmark


def _synthetic_vehicles(n: int = 240):
    """Benign (separable-low) + two attack families (separable-high), split train/test."""
    rng = np.random.default_rng(0)
    feats, labels = [], []
    for i in range(n):
        eid = f"ent_{i:04d}"
        r = i % 5
        if r < 3:               # benign
            lab, fam, sig = 0, "none", abs(rng.normal(0.2, 0.1))
        elif r == 3:            # position family
            lab, fam, sig = 1, "position", abs(rng.normal(3.0, 0.5))
        else:                   # speed family
            lab, fam, sig = 1, "speed", abs(rng.normal(2.6, 0.5))
        split = "test" if i % 4 == 0 else "train"
        feats.append({"entity_id": eid, "score_norm_max": sig,
                      "n_reports": 0 if lab == 0 else 6, "split": split})
        labels.append({"entity_id": eid, "label_is_attacker": lab,
                       "attack_family": fam, "split": split})
    return pd.DataFrame(feats), pd.DataFrame(labels)


def test_novel_attack_holds_out_each_family():
    vf, vl = _synthetic_vehicles()
    res = benchmark._novel_attack(vf, vl)
    assert res is not None
    # both attacker families are evaluated as held-out targets
    assert set(res["per_family_auc"]) == {"position", "speed"}
    # separable signal -> a model trained on the OTHER family still generalizes
    assert res["mean_novel_attack_auc"] > 0.7


def test_graph_baseline_returns_both_aucs():
    vf, vl = _synthetic_vehicles()
    # a report graph where attackers are densely inter-reported (a signal for message passing)
    ids = vl["entity_id"].tolist()
    atk = vl[vl.label_is_attacker == 1]["entity_id"].tolist()
    ben = vl[vl.label_is_attacker == 0]["entity_id"].tolist()
    edges = []
    for i, a in enumerate(atk):
        edges.append({"src_entity": ben[i % len(ben)], "dst_entity": a, "split": "train"})
        edges.append({"src_entity": atk[(i + 1) % len(atk)], "dst_entity": a, "split": "train"})
    import pandas as pd
    res = benchmark._graph_baseline(vf, vl, pd.DataFrame(edges))
    assert res is not None
    assert res["node_only_auc"] is not None and res["node_plus_graph_auc"] is not None


def test_domain_generalization_leave_one_out():
    import numpy as np
    import pandas as pd
    rng = np.random.default_rng(1)
    feats, labels = [], []
    for dom in range(4):
        for i in range(60):
            eid = f"d{dom}_ent_{i:03d}"
            atk = (i % 4 == 0)
            sig = abs(rng.normal(3.0 if atk else 0.2, 0.4))
            feats.append({"entity_id": eid, "domain_id": dom, "score_norm_max": sig,
                          "n_reports": 5 if atk else 0})
            labels.append({"entity_id": eid, "label_is_attacker": int(atk)})
    res = benchmark._domain_generalization(pd.DataFrame(feats), pd.DataFrame(labels))
    assert res is not None
    assert res["n_domains_evaluated"] >= 3
    assert res["mean_auc"] > 0.7


def test_domain_generalization_none_without_domain_id():
    import pandas as pd
    vf, vl = _synthetic_vehicles(40)
    assert benchmark._domain_generalization(vf, vl) is None   # no domain_id column


def test_novel_attack_none_when_no_families():
    # only benign -> nothing to hold out
    vf = pd.DataFrame([{"entity_id": f"e{i}", "score_norm_max": 0.1, "n_reports": 0,
                        "split": "train" if i % 2 else "test"} for i in range(40)])
    vl = pd.DataFrame([{"entity_id": f"e{i}", "label_is_attacker": 0, "attack_family": "none",
                        "split": "train" if i % 2 else "test"} for i in range(40)])
    assert benchmark._novel_attack(vf, vl) is None
