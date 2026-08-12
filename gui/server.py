"""SCMS-Simulator control GUI — a dependency-free local web app.

Configure every simulation variable, start/stop runs, watch an embedded live map, and
read the resulting dataset stats. It drives the tested `run.ps1` pipeline
(build -> generate -> simulate -> featurize -> validate), passing configuration through
the environment variables the Java back-end/app read and through run.ps1 parameters.

    python gui\\server.py            # then open http://127.0.0.1:8710
"""

from __future__ import annotations

import csv
import json
import os
import re
import subprocess
import sys
import threading
import time
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RUN_PS1 = REPO / "run.ps1"
LOG_PATH = REPO / "gui" / "last_run.log"
sys.path.insert(0, str(REPO / "src"))
sys.path.insert(0, str(REPO / "scms-sim" / "scenarios"))
from scms_sim_ref.datagen import validate as validate_mod  # noqa: E402
from scms_sim_ref.datagen import benchmark as benchmark_mod  # noqa: E402
import mapgen  # noqa: E402

PORT = 8710

# The curated maps (match gen_scenario.REG) plus the generated-map catalogue (procedural
# netgenerate families + real-world OSM cities). Each entry notes its kind so the GUI shows
# the relevant traffic controls (flow count vs route-density scale). Any well-formed
# procedural/osm key works even if it is not in this list.
CURATED = [
    {"key": "smoke",              "label": "Smoke — synthetic highway (fast)",     "kind": "flow", "family": "bundled"},
    {"key": "highway",            "label": "Highway — 3-lane motorway",            "kind": "flow", "family": "bundled"},
    {"key": "barnim",             "label": "Barnim — rural road network",          "kind": "flow", "family": "bundled"},
    {"key": "tiergarten",         "label": "Tiergarten — Berlin inner city",       "kind": "flow", "family": "bundled"},
    {"key": "intas_urban_low",    "label": "InTAS Ingolstadt — urban, off-peak",   "kind": "route", "family": "intas"},
    {"key": "intas_urban_rush",   "label": "InTAS Ingolstadt — urban, 7–9am rush", "kind": "route", "family": "intas"},
    {"key": "intas_highway_low",  "label": "InTAS Ingolstadt — highway, off-peak", "kind": "route", "family": "intas"},
    {"key": "intas_highway_rush", "label": "InTAS Ingolstadt — highway, rush",     "kind": "route", "family": "intas"},
]
SCENARIOS = CURATED + mapgen.catalog()   # ~200+ generated maps appended

# Every attack BASE behaviour our library can assign (must match AttackLib.BASES). Each base
# expands to a matrix of variants (temporal profile x magnitude tier) — ~291 concrete variants
# in total — so selecting a base here enables all of its variants via SCMS_ATTACKS.
ATTACK_TYPES = [
    # position
    "ConstPos", "ConstPosOffset", "ConstPosOffsetX", "ConstPosOffsetY",
    "RandomPos", "RandomPosOffset", "Teleport", "SineWavePos",
    # speed
    "ConstSpeedZero", "ConstSpeedOffset", "ConstSpeedHigh",
    "RandomSpeed", "RandomSpeedOffset", "StopAndGo",
    # heading
    "ReversedHeading", "RandomHeading", "PerpendicularHeading", "HeadingOffset",
    # combined
    "Disruptive", "PosSpeedInconsistent", "PosHeadingInconsistent", "EventualStop",
    # timing
    "DataReplay", "DelayedMessages", "OutOfOrder", "DoS", "DoSRandom",
    # identity
    "Sybil",
    # stealth (hard to detect: subtle, plausible falsifications)
    "AlongRoadOffset", "SlowDrift", "LaggingPosition",
]

# Every configurable variable, grouped for the form. `env` = the environment variable the
# Java layer reads; `env=None` means it is passed as a run.ps1 parameter (`param`) instead.
# `kind` on a spec restricts it to flow/route scenarios (None = always shown).
CONFIG_SPEC = [
    # --- Scenario & run ---
    {"group": "Scenario", "name": "scenario", "label": "Map / scenario", "type": "scenario",
     "default": "smoke", "env": None, "param": "Scenario",
     "help": "Eight maps: 4 MOSAIC-bundled + 4 real Ingolstadt (InTAS) variants"},
    {"group": "Scenario", "name": "duration", "label": "Duration", "type": "text", "default": "",
     "env": None, "param": "Duration", "help": "e.g. 300s / 600s (blank = the map's default)"},
    {"group": "Scenario", "name": "seed", "label": "Master seed", "type": "int", "default": 20260809,
     "env": None, "param": "Seed",
     "help": "Deterministic seed — same seed + config reproduces a byte-identical dataset"},
    {"group": "Scenario", "name": "visualize", "label": "Also open MOSAIC 2D map", "type": "bool",
     "default": False, "env": None, "param": "Visualize",
     "help": "Open MOSAIC's own 2D web visualizer alongside the embedded map"},

    # --- Traffic ---
    {"group": "Traffic", "name": "max_vehicles", "label": "Max vehicles", "type": "int", "default": "",
     "env": None, "param": "MaxVehicles", "kind": "flow",
     "help": "Flow maps: cap on concurrent vehicles (number of cars)"},
    {"group": "Traffic", "name": "target_flow", "label": "Target flow (veh/h)", "type": "int",
     "default": "", "env": None, "param": "TargetFlow", "kind": "flow",
     "help": "Flow maps: spawn rate in vehicles per hour"},
    {"group": "Traffic", "name": "lanes", "label": "Lanes", "type": "int", "default": "",
     "env": None, "param": "Lanes", "kind": "flow", "help": "Flow maps: number of lanes used"},
    {"group": "Traffic", "name": "scale", "label": "Density scale", "type": "float", "default": "",
     "step": 0.1, "min": 0, "max": 10, "env": None, "param": "Scale", "kind": "route",
     "help": "Route maps (InTAS): SUMO traffic-density multiplier, e.g. 0.4 or 1.5"},
    {"group": "Traffic", "name": "sim_step", "label": "Sim step (s)", "type": "float", "default": "",
     "min": 0.05, "max": 1.0, "step": 0.05, "env": "SCMS_SIM_STEP",
     "help": "Blank = map default (generated maps 0.1 s = 10 Hz CAMs; curated stay native). Set to override both."},
    {"group": "Traffic", "name": "fleet", "label": "Fleet mix", "type": "choice", "default": "mixed",
     "options": ["mixed", "car"], "env": "SCMS_FLEET",
     "help": "Generated maps: mixed = car/truck/bus/motorcycle; car = homogeneous"},
    {"group": "Traffic", "name": "demand", "label": "Demand profile", "type": "choice", "default": "uniform",
     "options": ["uniform", "rush", "night"], "env": "SCMS_DEMAND",
     "help": "Generated maps: rush front-loads departures (peak then quiet); night is sparse (~35%)"},

    # --- SCMS policy ---
    {"group": "SCMS policy", "name": "attacker_pct", "label": "Attacker %", "type": "int",
     "default": 20, "min": 0, "max": 100, "env": "SCMS_ATTACKER_PCT",
     "help": "Percentage of vehicles that misbehave"},
    {"group": "SCMS policy", "name": "faulty_pct", "label": "Faulty %", "type": "int",
     "default": 5, "min": 0, "max": 100, "env": "SCMS_FAULTY_PCT",
     "help": "Percentage of (non-attacker) vehicles with a malfunctioning sensor (anomalous, not malicious)"},
    {"group": "SCMS policy", "name": "collude_pct", "label": "Colluders % (of attackers)", "type": "int",
     "default": 0, "min": 0, "max": 100, "env": "SCMS_COLLUDE_PCT",
     "help": "Attackers that coordinate to file FALSE accusations against benign victims (0 = off)"},
    {"group": "SCMS policy", "name": "victim_pct", "label": "Collusion victims %", "type": "int",
     "default": 3, "min": 0, "max": 100, "env": "SCMS_VICTIM_PCT",
     "help": "Benign vehicles targeted by the collusion campaign"},
    {"group": "SCMS policy", "name": "report_k", "label": "Reporters to revoke (K)", "type": "int",
     "default": 3, "min": 1, "max": 50, "env": "SCMS_REPORT_K",
     "help": "Distinct reporters before the MA opens an investigation / revokes"},
    {"group": "SCMS policy", "name": "report_prob", "label": "Report probability", "type": "float",
     "default": 0.9, "min": 0, "max": 1, "step": 0.05, "env": "SCMS_REPORT_PROB",
     "help": "Chance a detection is actually reported (models loss/suppression)"},
    {"group": "SCMS policy", "name": "crl_delay", "label": "CRL propagation (s)", "type": "float",
     "default": 2.0, "min": 0, "max": 60, "step": 0.5, "env": "SCMS_CRL_DELAY",
     "help": "Delay before a published CRL is enforced by receivers"},
    {"group": "SCMS policy", "name": "jmax", "label": "Max reports/reporter (jmax)", "type": "int",
     "default": 20, "min": 1, "max": 1000, "env": "SCMS_JMAX",
     "help": "Rate cap on how many reports one reporter contributes"},
    {"group": "SCMS policy", "name": "live_interval", "label": "Live-map interval (s)", "type": "float",
     "default": 1.0, "min": 0.2, "max": 10, "step": 0.2, "env": "SCMS_LIVE_INTERVAL",
     "help": "How often the back-end writes the live vehicle snapshot"},
    {"group": "SCMS policy", "name": "min_seconds", "label": "Revoke: distinct seconds", "type": "int",
     "default": 8, "min": 1, "max": 60, "env": "SCMS_MIN_SECONDS",
     "help": "Reports must span at least this many distinct 1 s intervals before revoking (persistence)"},
    {"group": "SCMS policy", "name": "persist_s", "label": "Revoke: min span (s)", "type": "float",
     "default": 5.0, "min": 0, "max": 120, "step": 1, "env": "SCMS_PERSIST_S",
     "help": "Reports must span at least this long before revoking (rejects transient spikes)"},
    {"group": "SCMS policy", "name": "rotate_period", "label": "Pseudonym lifetime (s)", "type": "float",
     "default": 90.0, "min": 0, "max": 3600, "step": 10, "env": "SCMS_ROTATE_PERIOD",
     "help": "Vehicles change pseudonym certificate this often (LA-linked); 0 disables rotation"},
    {"group": "SCMS policy", "name": "ingest_delay", "label": "Report ingest delay (s)", "type": "float",
     "default": 0.15, "min": 0, "max": 10, "step": 0.05, "env": "SCMS_INGEST_DELAY",
     "help": "Mean MA report-channel latency (detection → ingest)"},

    # --- Attacks ---
    {"group": "Attacks", "name": "attacks", "label": "Enabled attack behaviours", "type": "multi",
     "default": "all", "options": ATTACK_TYPES, "env": "SCMS_ATTACKS",
     "help": "Pick base behaviours; each expands to its profile×tier variants (~291 total, all = every variant)"},
    {"group": "Attacks", "name": "offset_m", "label": "Position offset (m)", "type": "float",
     "default": 1500.0, "min": 0, "max": 5000, "step": 100, "env": "SCMS_OFFSET_M",
     "help": "ConstPosOffset / RandomPosOffset claimed-position shift"},
    {"group": "Attacks", "name": "speed_offset", "label": "Speed offset (m/s)", "type": "float",
     "default": 15.0, "min": 0, "max": 100, "step": 1, "env": "SCMS_SPEED_OFFSET",
     "help": "ConstSpeedOffset / RandomSpeedOffset claimed-speed shift"},
    {"group": "Attacks", "name": "random_radius", "label": "Random-pos radius (m)", "type": "float",
     "default": 2000.0, "min": 0, "max": 10000, "step": 100, "env": "SCMS_RANDOM_RADIUS",
     "help": "RandomPos radius around the true position"},
    {"group": "Attacks", "name": "random_speed_max", "label": "Random-speed max (m/s)", "type": "float",
     "default": 40.0, "min": 0, "max": 200, "step": 5, "env": "SCMS_RANDOM_SPEED_MAX",
     "help": "RandomSpeed upper bound"},
    {"group": "Attacks", "name": "freeze_updates", "label": "Freeze after N updates", "type": "int",
     "default": 5, "min": 1, "max": 100, "env": "SCMS_FREEZE_UPDATES",
     "help": "ConstPos / EventualStop: updates before the claim freezes"},
    {"group": "Attacks", "name": "stale_delay", "label": "Replay/stale delay (s)", "type": "float",
     "default": 8.0, "min": 0, "max": 60, "step": 1, "env": "SCMS_STALE_DELAY",
     "help": "DataReplay / DelayedMessages staleness"},
    {"group": "Attacks", "name": "sybil_ghosts", "label": "Sybil ghosts", "type": "int",
     "default": 4, "min": 1, "max": 20, "env": "SCMS_SYBIL_GHOSTS",
     "help": "Base extra identities a Sybil attacker emits (tier scales this)"},
    {"group": "Attacks", "name": "heading_offset", "label": "Heading offset (deg)", "type": "float",
     "default": 45.0, "min": 5, "max": 180, "step": 5, "env": "SCMS_HEADING_OFFSET",
     "help": "HeadingOffset base deviation (tier scales this)"},
    {"group": "Attacks", "name": "ramp_updates", "label": "Gradual ramp (msgs)", "type": "int",
     "default": 20, "min": 1, "max": 500, "env": "SCMS_RAMP_UPDATES",
     "help": "'gradual' profile: CAMs over which the deviation ramps to full magnitude"},
    {"group": "Attacks", "name": "duty_on", "label": "Intermittent on (s)", "type": "float",
     "default": 6.0, "min": 0.5, "max": 120, "step": 0.5, "env": "SCMS_DUTY_ON",
     "help": "'intermittent' profile: attack-active window"},
    {"group": "Attacks", "name": "duty_off", "label": "Intermittent off (s)", "type": "float",
     "default": 6.0, "min": 0.5, "max": 120, "step": 0.5, "env": "SCMS_DUTY_OFF",
     "help": "'intermittent' profile: benign window between bursts"},
    {"group": "Attacks", "name": "start_delay", "label": "Delayed start (s)", "type": "float",
     "default": 15.0, "min": 0, "max": 300, "step": 1, "env": "SCMS_START_DELAY",
     "help": "'delayed' profile: benign lead-in before the attack begins"},
    {"group": "Attacks", "name": "alongroad_m", "label": "Stealth on-road offset (m)", "type": "float",
     "default": 30.0, "min": 2, "max": 300, "step": 2, "env": "SCMS_ALONGROAD_M",
     "help": "AlongRoadOffset: subtle offset along the direction of travel (evasive)"},
    {"group": "Attacks", "name": "drift_rate", "label": "Stealth drift (m/CAM)", "type": "float",
     "default": 0.3, "min": 0.01, "max": 5, "step": 0.05, "env": "SCMS_DRIFT_RATE",
     "help": "SlowDrift: metres of position error added per CAM (evasive)"},
    {"group": "Attacks", "name": "lag_s", "label": "Stealth lag (s)", "type": "float",
     "default": 2.0, "min": 0.2, "max": 20, "step": 0.2, "env": "SCMS_LAG_S",
     "help": "LaggingPosition: seconds the claimed position lags behind reality (fresh timestamp)"},

    # --- Detectors ---
    {"group": "Detectors", "name": "cam_interval", "label": "CAM interval (s)", "type": "float",
     "default": 1.0, "min": 0.1, "max": 10, "step": 0.1, "env": "SCMS_CAM_INTERVAL",
     "help": "Beaconing period (~1 Hz)"},
    {"group": "Detectors", "name": "art_max_m", "label": "Acceptance range (m)", "type": "float",
     "default": 1000.0, "min": 100, "max": 5000, "step": 100, "env": "SCMS_ART_MAX_M",
     "help": "Acceptance-Range-Threshold detector cutoff"},
    {"group": "Detectors", "name": "stale_max", "label": "Stale/replay max (s)", "type": "float",
     "default": 5.0, "min": 0.5, "max": 60, "step": 0.5, "env": "SCMS_STALE_MAX",
     "help": "Max message age before the stale/replay detector fires"},
    {"group": "Detectors", "name": "freq_max", "label": "Beacon-freq max (/win)", "type": "int",
     "default": 12, "min": 1, "max": 200, "env": "SCMS_FREQ_MAX",
     "help": "Beacon-frequency (DoS) detector threshold per 1 s window (allows up to 10 Hz CAMs)"},
    {"group": "Detectors", "name": "speed_tol", "label": "Pos/speed tolerance (m)", "type": "float",
     "default": 10.0, "min": 1, "max": 200, "step": 1, "env": "SCMS_SPEED_TOL",
     "help": "Slack on the one-sided position/speed physical-plausibility check"},
    {"group": "Detectors", "name": "heading_diff", "label": "Heading diff (deg)", "type": "float",
     "default": 120.0, "min": 10, "max": 180, "step": 5, "env": "SCMS_HEADING_DIFF",
     "help": "Heading-inconsistency detector threshold"},
    {"group": "Detectors", "name": "sybil_min", "label": "Sybil co-location min", "type": "int",
     "default": 5, "min": 2, "max": 50, "env": "SCMS_SYBIL_MIN",
     "help": "Vehicles in one cell before the Sybil detector fires"},
    {"group": "Detectors", "name": "min_consec", "label": "Consecutive violations", "type": "int",
     "default": 2, "min": 1, "max": 10, "env": "SCMS_MIN_CONSEC",
     "help": "Motion-inconsistency must repeat this many times before flagging (filters GPS outliers)"},
    {"group": "Detectors", "name": "frozen_count", "label": "Frozen CAMs to flag", "type": "int",
     "default": 3, "min": 1, "max": 50, "env": "SCMS_FROZEN_COUNT",
     "help": "Consecutive frozen CAMs before the frozen-position detector fires"},

    # --- Vehicle dynamics & radio ---
    {"group": "Vehicle & radio", "name": "veh_maxspeed", "label": "Max speed (m/s)", "type": "float",
     "default": 42.0, "min": 5, "max": 120, "step": 1, "env": "SCMS_VEH_MAXSPEED",
     "help": "Vehicle type top speed (~151 km/h default)"},
    {"group": "Vehicle & radio", "name": "veh_speedfactor", "label": "Speed factor", "type": "float",
     "default": 1.0, "min": 0.2, "max": 2, "step": 0.05, "env": "SCMS_VEH_SPEEDFACTOR",
     "help": "Multiplier on edge speed limits (driver aggressiveness)"},
    {"group": "Vehicle & radio", "name": "veh_accel", "label": "Acceleration (m/s²)", "type": "float",
     "default": 2.6, "min": 0.5, "max": 10, "step": 0.1, "env": "SCMS_VEH_ACCEL",
     "help": "Max acceleration"},
    {"group": "Vehicle & radio", "name": "veh_decel", "label": "Deceleration (m/s²)", "type": "float",
     "default": 4.5, "min": 0.5, "max": 12, "step": 0.1, "env": "SCMS_VEH_DECEL",
     "help": "Max (comfortable) deceleration"},
    {"group": "Vehicle & radio", "name": "veh_sigma", "label": "Driver imperfection σ", "type": "float",
     "default": 0.5, "min": 0, "max": 1, "step": 0.05, "env": "SCMS_VEH_SIGMA",
     "help": "Krauss driver imperfection (0 = perfect, 1 = erratic)"},
    {"group": "Vehicle & radio", "name": "veh_mingap", "label": "Min gap (m)", "type": "float",
     "default": 2.5, "min": 0.5, "max": 10, "step": 0.5, "env": "SCMS_VEH_MINGAP",
     "help": "Minimum bumper-to-bumper gap"},
    {"group": "Vehicle & radio", "name": "veh_tau", "label": "Headway τ (s)", "type": "float",
     "default": 1.0, "min": 0.5, "max": 5, "step": 0.1, "env": "SCMS_VEH_TAU",
     "help": "Desired time headway / reaction time"},
    {"group": "Vehicle & radio", "name": "veh_length", "label": "Vehicle length (m)", "type": "float",
     "default": 5.0, "min": 2, "max": 20, "step": 0.5, "env": "SCMS_VEH_LENGTH",
     "help": "Vehicle length"},
    {"group": "Vehicle & radio", "name": "radio_range", "label": "Radio range (m)", "type": "float",
     "default": 709.4, "min": 50, "max": 3000, "step": 10, "env": "SCMS_RADIO_RANGE",
     "help": "ITS-G5 single-hop communication radius (SNS)"},
    {"group": "Vehicle & radio", "name": "radio_loss", "label": "Packet loss", "type": "float",
     "default": 0.0, "min": 0, "max": 1, "step": 0.02, "env": "SCMS_RADIO_LOSS",
     "help": "Single-hop transmission loss probability (SNS, distance-independent)"},
    {"group": "Vehicle & radio", "name": "chan_capacity", "label": "Channel capacity (CAMs/100ms)", "type": "int",
     "default": 25, "min": 0, "max": 500, "env": "SCMS_CHAN_CAPACITY",
     "help": "CSMA/CA contention: above this local load, frames are lost (CBR collapse). 0 disables"},

    # --- Sensors & CAM timing (data realism) ---
    {"group": "Sensors & CAM timing", "name": "gps_sigma", "label": "GPS noise σ (m)", "type": "float",
     "default": 1.5, "min": 0, "max": 20, "step": 0.5, "env": "SCMS_GPS_SIGMA",
     "help": "White position-measurement noise on every (honest) CAM"},
    {"group": "Sensors & CAM timing", "name": "gps_bias", "label": "GPS bias σ (m)", "type": "float",
     "default": 3.0, "min": 0, "max": 30, "step": 0.5, "env": "SCMS_GPS_BIAS",
     "help": "Temporally-correlated (drifting) position bias amplitude"},
    {"group": "Sensors & CAM timing", "name": "gps_theta", "label": "Bias reversion (1/s)", "type": "float",
     "default": 0.1, "min": 0.01, "max": 2, "step": 0.01, "env": "SCMS_GPS_THETA",
     "help": "How fast the GPS bias mean-reverts (larger = less correlated)"},
    {"group": "Sensors & CAM timing", "name": "speed_sigma", "label": "Speed noise σ (m/s)", "type": "float",
     "default": 0.5, "min": 0, "max": 10, "step": 0.1, "env": "SCMS_SPEED_SIGMA",
     "help": "Odometry/Doppler speed-measurement noise"},
    {"group": "Sensors & CAM timing", "name": "heading_sigma", "label": "Heading noise σ (deg)", "type": "float",
     "default": 2.0, "min": 0, "max": 30, "step": 0.5, "env": "SCMS_HEADING_SIGMA",
     "help": "Heading-measurement noise"},
    {"group": "Sensors & CAM timing", "name": "gps_outlier_rate", "label": "GPS outlier rate (/s)", "type": "float",
     "default": 0.008, "min": 0, "max": 0.5, "step": 0.002, "env": "SCMS_GPS_OUTLIER_RATE",
     "help": "Rate of multipath position spikes on honest CAMs (source of realistic false detections)"},
    {"group": "Sensors & CAM timing", "name": "gps_outlier_m", "label": "GPS outlier size (m)", "type": "float",
     "default": 45.0, "min": 5, "max": 200, "step": 5, "env": "SCMS_GPS_OUTLIER_M",
     "help": "Magnitude of a multipath spike"},
    {"group": "Sensors & CAM timing", "name": "gps_degrade_rate", "label": "GPS degradation rate (/s)", "type": "float",
     "default": 0.004, "min": 0, "max": 0.2, "step": 0.002, "env": "SCMS_GPS_DEGRADE_RATE",
     "help": "Rate at which benign vehicles enter a bad-GPS burst (canyon/tunnel) — a false-positive source"},
    {"group": "Sensors & CAM timing", "name": "gps_degrade_factor", "label": "GPS degradation factor", "type": "float",
     "default": 5.0, "min": 1, "max": 20, "step": 0.5, "env": "SCMS_GPS_DEGRADE_FACTOR",
     "help": "How much worse GPS error gets during a degradation burst"},
    {"group": "Sensors & CAM timing", "name": "cam_min", "label": "CAM min interval (s)", "type": "float",
     "default": 0.1, "min": 0.05, "max": 1, "step": 0.05, "env": "SCMS_CAM_MIN",
     "help": "ETSI T_GenCamMin — fastest CAM rate (paired with CAM interval = T_GenCamMax)"},
    {"group": "Sensors & CAM timing", "name": "flood_burst", "label": "DoS burst (msgs/tick)", "type": "int",
     "default": 10, "min": 1, "max": 100, "env": "SCMS_FLOOD_BURST",
     "help": "How many CAMs a DoS attacker emits per simulation step"},
    {"group": "Sensors & CAM timing", "name": "emit_sample", "label": "Per-msg GT sample rate", "type": "float",
     "default": 0.02, "min": 0, "max": 1, "step": 0.01, "env": "SCMS_EMIT_SAMPLE",
     "help": "Fraction of CAMs logged to gt_emissions_sample (true-vs-claimed, message-level labels)"},
]

_LOCK = threading.Lock()
RUN = {"proc": None, "logf": None, "out_dir": None, "scenario": None, "config": None,
       "started": None, "finished": None, "returncode": None}


def _defaults() -> dict:
    return {c["name"]: c["default"] for c in CONFIG_SPEC}


def _attacks_env(value) -> str | None:
    """Normalize the attack multi-select into an SCMS_ATTACKS value ('all' or comma list)."""
    if value in (None, "", "all"):
        return "all"
    if isinstance(value, list):
        picked = [a for a in ATTACK_TYPES if a in value]
        if not picked or len(picked) == len(ATTACK_TYPES):
            return "all"
        return ",".join(picked)
    return str(value)


def start_run(config: dict) -> dict:
    with _LOCK:
        if RUN["proc"] is not None and RUN["proc"].poll() is None:
            return {"ok": False, "error": "A simulation is already running."}
        scenario = str(config.get("scenario", "smoke"))
        valid = {s["key"] for s in SCENARIOS}
        if scenario not in valid and not mapgen.is_mapgen_key(scenario):
            return {"ok": False, "error": f"unknown scenario '{scenario}'"}
        out_dir = REPO / "datasets" / scenario

        # Environment variables the Java layer reads.
        env = os.environ.copy()
        for c in CONFIG_SPEC:
            if not c.get("env"):
                continue
            val = config.get(c["name"], "")
            if c["type"] == "multi":
                env[c["env"]] = _attacks_env(val)
            elif val != "":
                env[c["env"]] = str(val)

        # run.ps1 parameters (scenario, traffic, seed, duration, visualize).
        cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(RUN_PS1),
               "-Scenario", scenario]
        for c in CONFIG_SPEC:
            if c.get("env") is not None or not c.get("param") or c["name"] == "scenario":
                continue
            val = config.get(c["name"], "")
            if c["type"] == "bool":
                if val:
                    cmd.append(f"-{c['param']}")
            elif val != "":
                cmd += [f"-{c['param']}", str(val)]

        # Best-effort: clear any stale live-map snapshot from a previous run of this map.
        try:
            (out_dir / "live_state.json").unlink()
        except OSError:
            pass

        LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
        logf = open(LOG_PATH, "w", encoding="utf-8", errors="replace")
        proc = subprocess.Popen(cmd, cwd=str(REPO), env=env, stdout=logf,
                                stderr=subprocess.STDOUT, text=True)
        RUN.update(proc=proc, logf=logf, out_dir=out_dir, scenario=scenario, config=config,
                   started=time.time(), finished=None, returncode=None)
        return {"ok": True, "scenario": scenario, "cmd": " ".join(cmd)}


def stop_run() -> dict:
    with _LOCK:
        proc = RUN["proc"]
        if proc is None or proc.poll() is not None:
            return {"ok": False, "error": "No simulation is running."}
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True, text=True)
        return {"ok": True}


_STAGE_RE = re.compile(r"==\s*\[(\d)/(\d)\]\s*([^=]+?)\s*==")
_PROG_RE = re.compile(r"-\s*([0-9]+(?:\.[0-9]+)?)%")


def _log_tail(n: int = 40) -> tuple[str, str, float]:
    stage, progress = "", 0.0
    lines: list[str] = []
    try:
        with open(LOG_PATH, encoding="utf-8", errors="replace") as fh:
            lines = fh.read().splitlines()
    except FileNotFoundError:
        return "", "", 0.0
    for ln in lines:
        m = _STAGE_RE.search(ln)
        if m:
            stage = f"[{m.group(1)}/{m.group(2)}] {m.group(3).strip()}"
        p = _PROG_RE.search(ln)
        if p and "Simulating" in ln:
            progress = float(p.group(1))
    return "\n".join(lines[-n:]), stage, progress


def _read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(x) for x in fh if x.strip()]


def live_state() -> dict:
    """Return the latest live vehicle snapshot for the currently-selected run's map."""
    out_dir = RUN["out_dir"]
    if out_dir is None:
        return {"vehicles": [], "t": 0, "n": 0}
    p = Path(out_dir) / "live_state.json"
    try:
        with open(p, encoding="utf-8") as fh:
            return json.load(fh)
    except (FileNotFoundError, json.JSONDecodeError):
        return {"vehicles": [], "t": 0, "n": 0}


def compute_stats(out_dir: Path) -> dict | None:
    manifest = out_dir / "manifest.json"
    if not manifest.exists():
        return None
    with open(manifest, encoding="utf-8") as fh:
        man = json.load(fh)
    summary, _leaks = validate_mod.validate(str(out_dir))
    attacks = Counter(a.get("attack_type") for a in _read_jsonl(out_dir / "ground_truth" / "gt_attacks.jsonl"))
    reasons = Counter(r["reason_codes"][0] for r in _read_jsonl(out_dir / "ma" / "ma_reports.jsonl")
                      if r.get("reason_codes"))
    splits: Counter = Counter()
    subj_csv = out_dir / "ml" / "subject_labels.csv"
    if subj_csv.exists():
        with open(subj_csv, encoding="utf-8") as fh:
            for row in csv.DictReader(fh):
                splits[row.get("split", "?")] += 1
    correctness = Counter(r.get("report_correctness")
                          for r in _read_jsonl(out_dir / "ground_truth" / "gt_report_labels.jsonl"))
    try:
        bench_tasks = benchmark_mod.run(str(out_dir)).get("tasks", {})
    except Exception:
        bench_tasks = {}
    return {
        "counts": man.get("counts", {}),
        "seed": man.get("seed"),
        "entities": man.get("scms_entities", []),
        "data_digest": man.get("data_digest_sha256", "")[:16],
        "validate": summary,
        "attack_types": dict(attacks),
        "detector_reasons": dict(reasons),
        "report_correctness": dict(correctness),
        "benchmark": bench_tasks,
        "splits": dict(splits),
    }


def status() -> dict:
    proc = RUN["proc"]
    running = proc is not None and proc.poll() is None
    if proc is not None and not running and RUN["finished"] is None:
        RUN["finished"] = time.time()
        RUN["returncode"] = proc.returncode
        if RUN["logf"]:
            try:
                RUN["logf"].flush()
                RUN["logf"].close()
            except Exception:
                pass
    tail, stage, progress = _log_tail()
    st = {
        "running": running,
        "scenario": RUN["scenario"],
        "stage": stage,
        "progress": progress,
        "log_tail": tail,
        "returncode": RUN["returncode"],
        "elapsed": round((time.time() - RUN["started"]) if RUN["started"] else 0, 1),
        "stats": None,
    }
    if not running and RUN["out_dir"] is not None:
        try:
            st["stats"] = compute_stats(RUN["out_dir"])
        except Exception as ex:  # stats are best-effort
            st["stats_error"] = str(ex)
    return st


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass  # quiet

    def do_GET(self):
        if self.path in ("/", "/index.html"):
            html = (REPO / "gui" / "index.html").read_bytes()
            return self._send(200, html, "text/html; charset=utf-8")
        if self.path == "/api/defaults":
            return self._send(200, {"spec": CONFIG_SPEC, "defaults": _defaults(),
                                    "scenarios": SCENARIOS, "attack_types": ATTACK_TYPES})
        if self.path == "/api/status":
            return self._send(200, status())
        if self.path == "/api/live":
            return self._send(200, live_state())
        return self._send(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return self._send(400, {"error": "bad json"})
        if self.path == "/api/start":
            return self._send(200, start_run(body))
        if self.path == "/api/stop":
            return self._send(200, stop_run())
        return self._send(404, {"error": "not found"})


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"SCMS-Simulator control GUI -> http://127.0.0.1:{PORT}  (Ctrl+C to quit)")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\nshutting down")
        srv.shutdown()


if __name__ == "__main__":
    main()
