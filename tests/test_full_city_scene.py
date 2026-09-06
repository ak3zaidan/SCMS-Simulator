"""The whole city as a scene: a full SUMO net PLUS its building footprints, in ONE frame.

WHY THIS FILE EXISTS. `docs/realism/CROSS-ENGINE-RADIO.md` decomposes the two engines' 10.3x radio
divergence and attributes **84.8% of it to the SCENE** -- the Python engine ran a 2.01 km^2
RDP-simplified OSM extract where the MOSAIC path ran the real 65.96 km^2 SUMO city. Correcting the
propagation physics moved 2.8%. So the scene is the term worth fixing, and fixing it means two
things, not one:

1. **The whole city.** `netimport` already reads a full `.net.xml` cleanly (InTAS: 3332 junctions /
   7941 edges against `CustomNetwork`'s 4000 / 12000 caps), so this half was a matter of making it a
   documented, first-class path rather than new capability.
2. **Its buildings, in the SAME projection frame.** This is the half that fails SILENTLY. `osm.py`
   derives the local frame's origin from ROAD ways only; anything projected with a different origin
   lands hundreds of metres away while every individual polygon still looks like a building. The
   geometric channel then classifies NLOSb against a city translated off itself and the dataset is
   plausible and wrong. Every test below that says "gate" is about that one failure.

The tests are hermetic -- a hand-written 3-junction geo-referenced `.net.xml` and a 3-footprint
`.poly.xml` -- and skip only when sumolib is absent. The real-city numbers they quote in their
docstrings are measured in `docs/realism/FULL-CITY-SCENE.md`.
"""
from __future__ import annotations

import json
import math
import os

import pytest

from scms_sim_ref.datagen import awareness
from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline
from scms_sim_ref.mock_pipeline import netimport
from scms_sim_ref.mock_pipeline.osm import _frame
from scms_sim_ref.mock_pipeline.run import (GEO_BUILDING_CELL_M, GEO_BUILDING_MAX_CELLS,
                                            _BuildingRaster, validate_config)

sumolib = pytest.importorskip("sumolib", reason="sumolib (SUMO tools) not installed")

GOLDEN_DEFAULT = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

# --------------------------------------------------------------------------- #
# fixtures: a geo-referenced 3-junction net (A -> B <-> C -> A) and its footprints
# --------------------------------------------------------------------------- #
NET_XML = """<?xml version="1.0" encoding="UTF-8"?>
<net version="1.20" junctionCornerDetail="5" limitTurnSpeed="5.50">
    <location netOffset="-677584.01,-5403359.85" convBoundary="0.00,0.00,200.00,150.00"
 origBoundary="11.41,48.75,11.44,48.77"
 projParameter="+proj=utm +zone=32 +ellps=WGS84 +datum=WGS84 +units=m +no_defs"/>
    <type id="highway.residential" priority="3" numLanes="1" speed="13.89" oneway="0"/>
    <type id="highway.primary" priority="9" numLanes="2" speed="22.22" oneway="1"/>

    <edge id="AB" from="A" to="B" priority="9" type="highway.primary">
        <lane id="AB_0" index="0" speed="22.22" length="200.00" width="3.20"
              shape="0.00,0.00 200.00,0.00"/>
        <lane id="AB_1" index="1" speed="22.22" length="200.00" width="3.20"
              shape="0.00,3.20 200.00,3.20"/>
    </edge>
    <edge id="BC" from="B" to="C" priority="3" type="highway.residential">
        <lane id="BC_0" index="0" speed="13.89" length="150.00" width="3.20"
              shape="200.00,0.00 200.00,150.00"/>
    </edge>
    <edge id="CB" from="C" to="B" priority="3" type="highway.residential">
        <lane id="CB_0" index="0" speed="13.89" length="150.00" width="3.20"
              shape="200.00,150.00 200.00,0.00"/>
    </edge>
    <edge id="CA" from="C" to="A" priority="3" type="highway.residential">
        <lane id="CA_0" index="0" speed="13.89" length="250.00" width="3.20"
              shape="200.00,150.00 0.00,0.00"/>
    </edge>
    <edge id="AC" from="A" to="C" priority="3" type="highway.residential">
        <lane id="AC_0" index="0" speed="13.89" length="250.00" width="3.20"
              shape="0.00,0.00 200.00,150.00"/>
    </edge>

    <junction id="A" type="priority" x="0.00" y="0.00" incLanes="CA_0" intLanes="" shape="0.00,0.00"/>
    <junction id="B" type="traffic_light" x="200.00" y="0.00" incLanes="AB_0 AB_1 CB_0"
              intLanes="" shape="200.00,0.00"/>
    <junction id="C" type="priority" x="200.00" y="150.00" incLanes="BC_0 AC_0" intLanes=""
              shape="200.00,150.00"/>

    <connection from="AB" to="BC" fromLane="0" toLane="0" dir="l" state="M"/>
    <connection from="BC" to="CA" fromLane="0" toLane="0" dir="r" state="M"/>
    <connection from="CA" to="AB" fromLane="0" toLane="0" dir="s" state="M"/>
</net>
"""
_OFFSET = (-677584.01, -5403359.85)

#: Three footprints in the net's OWN metric frame (which is what polyconvert writes), none of them
#: on a junction, plus one landuse polygon that must be ignored.
POLY_XML = """<?xml version="1.0" encoding="UTF-8"?>
<additional>
    <poly id="b1" type="building" color="blue" fill="1" layer="2.00"
          shape="40.00,30.00 60.00,30.00 60.00,50.00 40.00,50.00 40.00,30.00"/>
    <poly id="b2" type="building" color="blue" fill="1" layer="2.00"
          shape="150.00,20.00 170.00,20.00 170.00,40.00 150.00,40.00 150.00,20.00"/>
    <poly id="b3" type="building" color="blue" fill="1" layer="2.00"
          shape="120.00,90.00 130.00,90.00 130.00,100.00 120.00,100.00"/>
    <poly id="green" type="landuse.grass" color="green" fill="1" layer="0.00"
          shape="0.00,300.00 10.00,300.00 10.00,310.00 0.00,310.00 0.00,300.00"/>
</additional>
"""

#: b1 widened until it swallows junctions A (0, 0) and B (200, 0) -- exactly what a displaced
#: footprint layer does to a road graph.
POLY_XML_ON_THE_ROAD = POLY_XML.replace(
    'shape="40.00,30.00 60.00,30.00 60.00,50.00 40.00,50.00 40.00,30.00"',
    'shape="-10.00,-10.00 210.00,-10.00 210.00,10.00 -10.00,10.00 -10.00,-10.00"')


@pytest.fixture()
def net_path(tmp_path):
    p = tmp_path / "fixture.net.xml"
    p.write_text(NET_XML, encoding="utf-8")
    return str(p)


@pytest.fixture()
def poly_path(tmp_path):
    p = tmp_path / "fixture.poly.xml"
    p.write_text(POLY_XML, encoding="utf-8")
    return str(p)


def _fixture_frame():
    """The osm.py-style local frame for the fixture's own three junctions."""
    pts = []
    for x, y in ((0.0, 0.0), (200.0, 0.0), (200.0, 150.0)):
        lon, lat = netimport.inverse_utm(x - _OFFSET[0], y - _OFFSET[1], 32)
        pts.append((lat, lon))
    return _frame(pts)


def _junctions(net_path):
    net = netimport.read_net(net_path)
    return [tuple(n.getCoord()[:2]) for n in net.getNodes() if n.getType() != "internal"]


def _bbox(pts):
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    return [min(xs), min(ys), max(xs), max(ys)]


# --------------------------------------------------------------------------- #
# 1. reading the footprints
# --------------------------------------------------------------------------- #
def test_poly_buildings_are_read_in_the_files_own_frame(poly_path):
    """`type="building"` ONLY -- the same selection `org.scms.radio.BuildingIndex.parse` makes on
    the Java side, so a cross-engine comparison of link-state composition rasterises one footprint
    set rather than two subsets of one file -- and the repeated closing vertex is dropped, because
    every ring in this codebase is an OPEN vertex list."""
    rings, stats = netimport._poly_rings(poly_path)
    assert stats == {"poly_elements": 4, "skipped_wrong_type": 1, "degenerate": 0, "rings": 3}
    assert [len(r) for r in rings] == [4, 4, 4]     # b1/b2 lost their duplicate; b3 was open already
    assert rings[0][0] == (40.0, 30.0) and rings[0][-1] == (40.0, 50.0)


def test_the_layer_is_lossless_by_default(net_path, poly_path):
    """No minimum area, no RDP simplification, no polygon cap. `osm.extract_buildings` does all
    three because it is rebuilding rings from raw OSM node refs; here the polygons arrive already
    resolved, and a comparison against the Java classifier is only a comparison if neither side
    dropped anything."""
    polys, info = netimport.scene_from_net(net_path, poly_path)
    assert info["polygons"] == 3 and info["vertices"] == 12
    assert info["min_area_m2"] == 0.0 and info["simplify_tol_m"] == 0.0
    assert "truncated_from" not in info and "dropped_below_min_area" not in info
    assert [40.0, 30.0] in polys[0]                  # projection=None -> the file's own coordinates


def test_filters_and_caps_exist_for_a_caller_that_wants_them(net_path, poly_path):
    polys, info = netimport.scene_from_net(net_path, poly_path, min_area_m2=200.0)
    assert info["dropped_below_min_area"] == 1 and len(polys) == 2      # b3 is 10 x 10 = 100 m^2
    polys2, info2 = netimport.scene_from_net(net_path, poly_path, max_polygons=2)
    assert len(polys2) == 2 and info2["truncated_from"] == 3


def test_a_file_with_no_buildings_is_refused_loudly(net_path, tmp_path):
    """Silence here would produce a whole-city run with no footprints that still LOOKS configured."""
    p = tmp_path / "none.poly.xml"
    p.write_text('<additional><poly id="g" type="landuse.grass" shape="0,0 1,0 1,1"/></additional>',
                 encoding="utf-8")
    with pytest.raises(ValueError, match="no <poly> of type"):
        netimport.scene_from_net(net_path, str(p))


# --------------------------------------------------------------------------- #
# 2. THE GATE -- the only place a misprojected footprint layer is observable
# --------------------------------------------------------------------------- #
def test_a_correct_overlay_reports_all_three_registration_statistics(net_path, poly_path):
    _polys, info = netimport.scene_from_net(net_path, poly_path)
    assert info["alignment_anchor_is"] == "median junction"
    assert info["centroid_inside_road_bbox_frac"] == 1.0
    assert info["junctions"] == 3
    assert info["junctions_in_footprint"] == 0 and info["junctions_in_footprint_frac"] == 0.0


def test_the_sharp_arm_fires_when_footprints_swallow_junctions(net_path, tmp_path):
    """ARM 3, and it is the one that earns the gate. A city's junctions are on tarmac, so a correct
    overlay puts almost none of them inside a wall. MEASURED on the real 21,717-footprint InTAS
    layer: 0.0027 at zero shift, 0.0441 at 10 m, 0.1327 at 25 m, ~0.16 from 100-400 m -- and the
    gate fires from a 10.6 m displacement in every direction (docs/realism/FULL-CITY-SCENE.md)."""
    p = tmp_path / "onroad.poly.xml"
    p.write_text(POLY_XML_ON_THE_ROAD, encoding="utf-8")
    with pytest.raises(ValueError, match="land INSIDE a building footprint"):
        netimport.scene_from_net(net_path, str(p))


def test_a_large_translation_is_caught_by_the_centre_arm_that_arm_three_cannot_see(net_path,
                                                                                   poly_path):
    """The two arms are chosen as a PAIR with no hole between them. A translation big enough to
    carry the footprints off the city stops putting junctions in walls, so arm 3 alone would let it
    through; arm 1 (median centroid against the median JUNCTION) catches it. On InTAS the crossover
    is comfortable: arm 3 fires from 10.6 m and stays lit past 3.2 km, arm 1 from ~0.9 km."""
    rings, _ = netimport._poly_rings(poly_path)
    junc = _junctions(net_path)
    bbox = _bbox(junc)
    netimport._assert_buildings_aligned(rings, bbox, junc)                     # as-is: passes
    far = [[(x + 5000.0, y) for x, y in r] for r in rings]
    idx = netimport.FootprintIndex(far)
    assert not any(idx.entered(jx, jy, jx, jy) for jx, jy in junc), (
        "arm 3 must be silent at this displacement, or this test is not exercising arm 1")
    with pytest.raises(ValueError, match="do not sit on the road network"):
        netimport._assert_buildings_aligned(far, bbox, junc)


def test_the_anchor_is_the_median_junction_not_the_bbox_centre(net_path, poly_path):
    """A real netconvert net keeps motorway stubs far outside the built-up area -- InTAS's road bbox
    is 13.6 x 11.1 km around an 8.3 x 8.0 km city, and its centre sits 1.9 km east of the buildings.
    Anchoring on the bbox centre would fail a CORRECT import; the junction cloud's median is the
    city. Reproduced here by hanging one junction 4 km off the map."""
    xml = NET_XML.replace('x="200.00" y="150.00"', 'x="4200.00" y="150.00"').replace(
        'convBoundary="0.00,0.00,200.00,150.00"', 'convBoundary="0.00,0.00,4200.00,150.00"')
    p = os.path.join(os.path.dirname(net_path), "stub.net.xml")
    open(p, "w", encoding="utf-8").write(xml)
    junc = _junctions(p)
    bbox = _bbox(junc)
    assert 0.5 * (bbox[0] + bbox[2]) > 2000.0, "the bbox centre must be off the built-up area"
    rings, _ = netimport._poly_rings(poly_path)
    info = netimport._assert_buildings_aligned(rings, bbox, junc)
    assert info["alignment_anchor"][0] < 1000.0        # the median junction, not 2100


def test_without_junctions_the_gate_is_much_weaker_and_says_so(net_path, poly_path):
    """Stated rather than hidden. With no junction cloud the only rule a bbox supports is
    containment, which on InTAS tolerates a 3.9-8.2 km shift. Any caller that can supply junctions
    must -- `scene_from_net` always does."""
    rings, _ = netimport._poly_rings(poly_path)
    bbox = [0.0, 0.0, 200.0, 150.0]
    info = netimport._assert_buildings_aligned(rings, bbox, None)
    assert info["alignment_anchor_is"].startswith("road bbox containment")
    assert info["junctions_in_footprint_frac"] is None
    # a displacement arm 3 would catch instantly sails through the weak rule
    netimport._assert_buildings_aligned([[(x + 60.0, y) for x, y in r] for r in rings], bbox, None)


def test_both_layers_move_together_under_a_re_projection(net_path, poly_path):
    """The whole reason `scene_from_net` reads the net itself instead of taking a transform: the
    footprints go through the SAME `_transformer` closure the junctions did. Under a re-projection
    into osm.py's local frame the distance from every footprint corner to the nearest junction is
    preserved -- which is what "one frame" means operationally."""
    frame = _fixture_frame()
    raw, _ = netimport.scene_from_net(net_path, poly_path)
    geo, info = netimport.scene_from_net(net_path, poly_path, projection=frame)
    nodes_raw, _e, _i = netimport.import_net(net_path, projection=None)
    nodes_geo, _e2, _i2 = netimport.import_net(net_path, projection=frame)
    assert info["polygons"] == 3
    assert geo != raw, "the geo import must actually land in a different frame"
    for ring_raw, ring_geo in zip(raw, geo):
        for p_raw, p_geo in zip(ring_raw, ring_geo):
            d_raw = min(math.dist(p_raw, n) for n in nodes_raw)
            d_geo = min(math.dist(p_geo, n) for n in nodes_geo)
            assert d_geo == pytest.approx(d_raw, abs=1.0), "the two layers did not move together"


def test_buildings_reach_the_network_document_and_are_absent_when_not_asked(net_path, poly_path):
    nodes, edges, info = netimport.import_net(net_path, projection=None)
    polys, binfo = netimport.scene_from_net(net_path, poly_path)
    doc = netimport.signal_document(nodes, edges, info, buildings=polys)
    assert len(doc["buildings"]) == 3
    assert binfo["source"].endswith("fixture.poly.xml")
    assert "buildings" not in netimport.signal_document(nodes, edges, info)


# --------------------------------------------------------------------------- #
# 3. the raster the footprints are classified with
# --------------------------------------------------------------------------- #
def test_a_city_sized_scene_is_not_silently_coarsened(tmp_path):
    """THE TRAP THIS CONSTANT HIDES. `_BuildingRaster` doubles its cell size until the grid fits
    `GEO_BUILDING_MAX_CELLS`, silently. The 2.01 km^2 extract needs 0.22 M cells at 3 m and never
    came near the old 6 M ceiling; the whole 66 km^2 city needs 7.41 M and WOULD have been coarsened
    to 6 m -- i.e. the full-city scene would have been classified at half the extract's resolution
    and the two would not have been comparable at all. MEASURED on InTAS: 6 m instead of 3 m moves
    NLOSb at 200 m from 0.2471 to 0.3170 and the awareness range from 338.5 m to 304.7 m."""
    span = 9000.0                                    # a 9 km x 9 km scene, larger than Ingolstadt
    polys = [[(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)],
             [(span, span), (span + 5, span), (span + 5, span + 5), (span, span + 5)]]
    r = _BuildingRaster(polys)
    assert r.cell == GEO_BUILDING_CELL_M == 3.0, "a city-sized scene was coarsened"
    assert r.nx * r.ny <= GEO_BUILDING_MAX_CELLS
    assert GEO_BUILDING_MAX_CELLS >= 7_500_000, "InTAS needs 7.41 M cells at 3 m"


# --------------------------------------------------------------------------- #
# 4. the engine knob
# --------------------------------------------------------------------------- #
def test_sumo_buildings_is_self_describing_and_default_inert():
    cfg = PipelineConfig()
    sch = config_schema()
    assert cfg.sumo_buildings == ""
    assert sch["sumo_buildings"]["default"] == ""
    assert sch["sumo_buildings"]["group"] == "Network"
    assert sch["sumo_buildings"]["help"]


def test_cli_exposes_the_knob():
    import subprocess
    import sys
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    r = subprocess.run([sys.executable, "-m", "scms_sim_ref.mock_pipeline.run", "--help"],
                       capture_output=True, text=True, cwd=repo,
                       env={**os.environ, "PYTHONPATH": os.path.join(repo, "src")})
    assert r.returncode == 0, r.stderr
    assert "--sumo-buildings" in r.stdout


def test_footprints_without_a_sumo_net_are_refused():
    """A `.poly.xml` is meaningless without the net whose frame it is in — the transform and the
    junctions the gate needs both come from that net."""
    cfg = PipelineConfig(traffic_flow=True, duration_s=10, sumo_buildings="x.poly.xml")
    with pytest.raises(ValueError, match="sumo_buildings needs road_network='sumo'"):
        validate_config(cfg)


def test_a_missing_footprint_file_is_refused_before_the_run(net_path):
    cfg = PipelineConfig(traffic_flow=True, duration_s=10, road_network="sumo", sumo_net=net_path,
                         sumo_buildings=os.path.join(os.path.dirname(net_path), "nope.poly.xml"))
    with pytest.raises(ValueError, match="sumo_buildings not found"):
        validate_config(cfg)


def _run_cfg(net_path, out, **kw):
    base = dict(seed=42, traffic_flow=True, duration_s=30.0, dt=1.0, arrival_rate=1.0,
                road_network="sumo", sumo_net=net_path, custom_network_directed=True,
                radio_model="geometric", radio_env="urban", radio_tx_power_dbm=23.0,
                radio_rx_sensitivity_dbm=-81.0, emit_sample_prob=1.0,
                attacker_pct=0.2, faulty_pct=0.0, out_dir=str(out))
    base.update(kw)
    return PipelineConfig(**base)


def test_the_footprints_reach_the_channel_and_the_manifest(net_path, poly_path, tmp_path):
    """End to end: a `road_network="sumo"` run with `sumo_buildings` rasterises real polygons
    instead of falling back to the synthetic canyon density, and the manifest records every
    registration statistic the gate measured -- so a reader can check the overlay after the fact."""
    cfg = _run_cfg(net_path, tmp_path / "with", sumo_buildings=poly_path)
    validate_config(cfg)
    run_pipeline(cfg)
    man = json.load(open(os.path.join(cfg.out_dir, "manifest.json"), encoding="utf-8"))
    scene = man["counts"]["scene_buildings"]
    assert scene["polygons"] == 3 and scene["gated"] is True
    assert scene["junctions_in_footprint_frac"] == 0.0
    assert scene["source"].endswith("fixture.poly.xml")
    assert man["config"]["sumo_buildings"] == poly_path

    # ... and without the knob the same map runs with no footprints at all
    cfg2 = _run_cfg(net_path, tmp_path / "without")
    run_pipeline(cfg2)
    man2 = json.load(open(os.path.join(cfg2.out_dir, "manifest.json"), encoding="utf-8"))
    assert "scene_buildings" not in man2["counts"]
    assert man2["data_digest_sha256"] != man["data_digest_sha256"], (
        "the footprints must actually change the radio, or the layer is decorative")


def test_awareness_reads_the_scene_back_off_a_sumo_dataset(net_path, poly_path, tmp_path):
    """`awareness.load_scenario` used to look for footprints in ONE place -- the custom-network
    document embedded in the manifest -- so a whole-city run's scene was invisible to the instrument
    that measures the scene. It now re-imports through the SAME `scene_from_net` call the run made,
    which is not a second opinion: same transform, same gate, same polygons."""
    cfg = _run_cfg(net_path, tmp_path / "aw", sumo_buildings=poly_path)
    run_pipeline(cfg)
    sc = awareness.load_scenario(cfg.out_dir)
    assert len(sc["buildings"]) == 3
    assert sc["buildings_source"].startswith("sumo_buildings")
    comp = awareness.link_state_composition(sc, max_dist_m=400.0)
    assert comp["nlosb_method"] == "building_raster" and comp["n_buildings"] == 3


def test_a_moved_footprint_file_is_reported_not_raised(net_path, poly_path, tmp_path):
    """The instrument is read-only over a finished dataset, so a missing input must degrade to a
    stated fact rather than an exception in the middle of a measurement run."""
    cfg = _run_cfg(net_path, tmp_path / "moved", sumo_buildings=poly_path)
    run_pipeline(cfg)
    mp = os.path.join(cfg.out_dir, "manifest.json")
    man = json.load(open(mp, encoding="utf-8"))
    man["config"]["sumo_buildings"] = str(tmp_path / "gone.poly.xml")
    with open(mp, "w", encoding="utf-8") as fh:
        json.dump(man, fh)
    sc = awareness.load_scenario(cfg.out_dir)
    assert sc["buildings"] == []
    assert sc["buildings_source"].startswith("sumo_buildings unreadable")


def test_the_default_golden_is_untouched(tmp_path):
    """Everything above is opt-in. The pinned default digest is re-measured here rather than
    assumed, because `run.py`'s channel-construction block and `GEO_BUILDING_MAX_CELLS` both moved.
    """
    res = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "golden")))
    assert res.data_digest == GOLDEN_DEFAULT
