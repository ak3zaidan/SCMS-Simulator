# Roadmap to perfect realism — the remaining gap list

Standing goal: traffic and network behaviour indistinguishable from a real city. This is the PM's
running list of what is still not real, ranked by measured impact rather than by how interesting it
is to build. Everything here is backed by a measurement already in this repository.

## Where we actually are

Landed and measured: SUMO-backed mobility with an exact adapter (position and speed match fraction
1.000); real Ingolstadt signal programs (98 junctions, verified against the raw XML three ways);
directed carriageways (opposing-direction overlaps 117/49/32 → 0); a geometric channel with 3GPP
TR 37.885 path loss over real building polygons; real ASN.1 UPER on the wire, independently decoded
by two ASN.1 runtimes; ETSI generation rules, reactive DCC, per-packet latency and real ECDSA; and
plugin seams for channel, detector, fusion, codec, mobility, report format and whole protocol
profiles. 1531 tests, both pinned digests stable.

Validated against reality: traffic, twice, against measured Ingolstadt loop counts. **The radio has
never been validated against measured radio data** — only against published curves.

## P1 — The scene is 2 km² and the city is 66 km²

**The single largest realism term measured anywhere in this project.** The cross-engine benchmark
attributes 84.8% of the 10.3× radio divergence to the SCENE, against 12.4% for the link-state
classifier and 2.8% for propagation physics. The Python engine runs an RDP-simplified OSM core
extract; the MOSAIC path runs the real 66 km² SUMO city whose traffic is on arterials.

Everything downstream inherits it: link-state composition, awareness, density, and the fundamental
diagram. Fixing the radio physics — which was worth doing — moved 2.8% of the problem.

Work: make the Python engine run the full city. The caps are already 4000 nodes / 12000 edges and
InTAS is 3332/7942, so the graph fits; what does not fit is the OSM import path's simplification and
node budget. The netconvert importer is the right route and already exists.

## P2 — Calibrated demand cannot be delivered

Calibration halved the deficit (−55.1% → −27.1% held-out) but **all four FHWA gates fail in all 36
window-gradings**, and the cause is not the demand generator. routeSampler solves its problem
(99.98% of target, GEH < 5 at 100%, overflow exactly zero in every interval). SUMO then delivers
only 24,071 of 31,046 vehicles.

The decisive control is already measured: the warm-up hour uses the SAME route set and hits its
22,710 target exactly, while the graded hour's 31,046 plateaus at 0.74–0.80 and never climbs. That
is **a throughput ceiling between those two figures**, not queueing — undershooting edges are no
more congested than met ones. The calibrated runs never reach steady state (they end at their peak,
70% halted, 1,016 teleports).

Work: find the ceiling. Candidates worth testing rather than assuming: insertion capacity at the
network boundary, `max-depart-delay` discarding vehicles, a lane-choice or junction-capacity limit,
or the 55-of-96 usable counting edges constraining only 69–72% of measured flow.

## P3 — Two traffic metrics still fail, and their measurement was contaminated

`fd_capacity` reads 182.5 (internal) and 811.7 (SUMO replay) veh/h/lane against an 1800–2400 anchor;
`headway_ks` reads 0.1936 against a 0.15 gate. **Both were measured off an enforcement-truncated
sample** — at peak, 61.4% of vehicles are revoked and only 43.8% of vehicle-steps survive, so every
flow and headway figure is low by a factor that grows with run length. The survivorship fix has
landed; these numbers have not been re-established on the corrected source.

Also open: the earlier claim that SUMO mobility made headway *worse* may be pure survivorship.

## P4 — The engine's channel-busy measure is not the standard's

The Python CBR numerator counts **decoded** frames, where ETSI TS 102 687 defines the medium sensed
busy — the Java side senses before any decode drop. Biased low by roughly the delivery ratio, which
matters now that DCC consumes it.

## P5 — One protocol profile is not a proof of the seam

ITS-G5 ships through the `ProtocolProfile` seam, but a seam with one implementation has never been
tested against divergence. A C-V2X / NR-V2X mode-2 profile — different access layer, SPS resource
selection, different congestion control — is both a realism gain and the honest test of whether the
abstraction holds.

## P6 — The internal engine has no collision constraint

Residual overlaps under the internal model are 10 same-direction (a follower penetrating its leader,
because IDM computes a desired acceleration and nothing enforces a minimum gap) and 3 crossing at
junctions. SUMO-backed mobility removes the class entirely, but the internal engine remains the
default and is what runs with no SUMO scenario available.

## P7 — The radio has never met real radio data

Traffic is validated against measured counts. The channel is validated only against published curves
evaluated at our own budget. Candidate open measurement sets exist (the FLOURISH Bristol ITS-G5
dataset carries PDR-versus-RSSI); until one is used, "realistic radio" rests on formulas rather than
on a comparison.

## Standing method

Every item ships with a quantitative gate, and nothing counts as done until an independent agent has
re-run the measurement or the attack. Three times in this project a headline result did not survive
that step: a GEH gate that graded a simulation against itself, a seed-stability gate too loose to
catch a 2× regression, and a plugin firewall defeated by one line of frame introspection.
