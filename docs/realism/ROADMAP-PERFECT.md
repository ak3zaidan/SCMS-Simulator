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

Validated against reality: traffic, twice, against measured Ingolstadt loop counts. The radio has
been confronted with measured radio data **once as a scalar and once as a per-effect audit**, has
never been validated as a curve, and **is falsified against distance** — see P7, re-scoped
2026-09-07 after the earlier claim ("never met real radio data") was found too strong, and P7c,
which records what the confrontation actually cost us.

## P1 — The scene is 2 km² and the city is 66 km² — **LANDED**, see `FULL-CITY-SCENE.md`

**Was the single largest realism term measured anywhere in this project.** The cross-engine
benchmark attributed 84.8% of the 10.3× radio divergence to the SCENE, against 12.4% for the
link-state classifier and 2.8% for propagation physics. The Python engine ran an RDP-simplified OSM
core extract; the MOSAIC path runs the real 66 km² SUMO city whose traffic is on arterials.

**Closed.** `road_network="sumo"` now carries the whole InTAS net *and* its 21,717 building
footprints (`sumo_buildings`, projected through the road import's own transform and gated on landing
on its junctions — the gate fires on an 11 m displacement). On the full city with InTAS's own
traffic the engine delivers **0.6026** of packets at 200 m against **0.0754** on the 2 km² extract
and MOSAIC's 0.7798: **the whole scene term (100.7% of it, corrected for §6 below), 89.0% of the
whole divergence, residual factor 10.3× → 1.29×**. The 0.90-awareness-equivalent range goes
103.5 m → **338.5 m** against MOSAIC's 504.6 m. Independent cross-check: the unmodified
`awareness.py` pointed at MOSAIC's OWN scene and emissions reads 0.5944 / 335.1 m, so the native
whole-city run agrees with it to **1.4% / 1.0%**. Link-state composition at 200–250 m is
LOS+NLOSv 0.703 / NLOSb 0.298 against the Java side's 0.741 / 0.259 — 3.9 pp apart where the extract
was 67 pp. Road length inside a building falls 10.35% → 2.00% and vehicles standing in walls 9.62% →
0.09%, which retires the registration defect of `CROSS-ENGINE-RADIO.md` §5 as well. And it is
**cheaper**: 104 ms/step and 261 MB against the extract's 426 ms/step and 704 MB, because cost
follows vehicle density, not map area.

Two things fell out of it. The map is 71.3% of the scene term and *where the traffic is* is the
other 33.6% (measured by swapping the mobility alone). And `_BuildingRaster` was silently coarsening
any scene above 6 M cells: the 66 km² city needs 7.41 M at 3 m, so every full-city classification —
including `CROSS-ENGINE-RADIO.md` §6's own "Python instrument on MOSAIC's scene" column — had been
done at 6 m, half the extract's resolution. The ceiling is now 32 M (32 MB).

Still open under this heading: the classifier (12.4%) and physics (2.8%) terms, neither of them
re-measured at 3 m.

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

## P7 — The radio has met measured data, but never as a curve — **RE-SCOPED 2026-09-07**

**The previous wording of this item was wrong, and in our own disfavour.** It read "the radio has
never met real radio data". It has, twice, and both are in the repository:

1. `refdata/v2x_awareness_conditions.json` is a full conditions audit of Boban & d'Orey's *measured*
   four-country campaign (IEEE TVT 65(6):3904–3916, 2016; preprint arXiv:1503.06590), transcribed
   from the full text. That file is also where the project already established that four of the
   reference's conditions differ from our metric, and `PHASE2-GATE.md` correctly refused to tune the
   model to the 0.90-at-200 m number until they were.
2. `docs/realism/RADIO-VS-REALITY.md` (2026-09-07, **revision 2**) is a per-effect confrontation of
   the channel against the literature, with the provenance and licence status of every number
   recorded. Revision 2 exists because revision 1 mislabelled a simulator's output as measurement in
   three places; the withdrawals are tabulated in its §0. Counting only sources that are actually
   *measurements*, the channel has now met four: Abbas et al. (IJAP 2015), Nilsson et al.
   (Sensors 2018), Boban's thesis (arXiv:1405.1008) and Segata et al. (VNC 2013). Against two of
   them it is **falsified** — see P7c.

What has **never** happened, and is what this item now means: a **curve-shaped, per-link-state**
comparison — PDR or RSSI versus distance, split by LOS / NLOSv / NLOSb, against a measured V2V set
at comparable antenna heights. Every check so far is either a single scalar at a single distance or
a per-effect audit of constants.

**The obvious candidate is not usable, and the entry that recommended it has been withdrawn.** The
FLOURISH Bristol ITS-G5 dataset was named here and in `refdata/v2x_awareness.json` as the way to
close this. Its own authors state, in the Data in Brief abstract, *"The dataset is not intended to
be used for signal propagation modelling."* It is also V2I with masts at ~5/8/12/25 m against our
1.5 m V2V, and the dataset (unlike the paper) is under the Non-Commercial Government Licence v2.
All three were verified first-hand on 2026-09-07 and are now recorded next to the recipe.

### P7a — The standard's own reference list contains no measurement for the term we treat as truth

Read first-hand from TR 37.885 V15.3.0 on 2026-09-07. Its References clause has nineteen entries:
eleven 3GPP TRs and work-item proposals, six ITU-R / ECC / ETSI / IEEE-802.18 spectrum and
regulatory documents about the 63–64 GHz band, the Republic of Korea frequency-allocation table,
and one antenna-radiation-pattern paper (F. Gil et al., VTC-Fall 1999). **Not one is a V2V
propagation measurement campaign.** The NLOSv blockage term's only traceable provenance is change
request RP-182530 CR 0004, *"Correction on vehicle blockage loss in NLOSv"*, agreed at RAN#82 in
December 2018 for version 15.2.0.

**Correction, 2026-09-07 (revision 2).** This paragraph previously ended: *"the empirical checks in
`RADIO-VS-REALITY.md` find one of them in near-exact agreement with measurement."* **That clause is
withdrawn.** The "near-exact agreement" it pointed at was TR 37.885's 5.20 dB against GEMV²'s 5 dB,
and GEMV² Fig. 12's own caption reads *"Received power distribution **as generated by GEMV²**"* —
model output, not measurement. That was a model-vs-model comparison, and the 0.20 dB was a rounding
artefact besides. See `RADIO-VS-REALITY.md` §0 W1.

What the paragraph correctly established stands: "anchored" throughout
`refdata/pathloss_3gpp_tr37885.json` has always meant *anchored to a standard*, never *anchored to
data*, and the files say so (`nlosv_has_no_measurement_provenance_in_the_standard`). The one
NLOSv constant that **does** agree with a measurement is the blocker-type-agnostic mean: our
9.04 dB against Abbas et al.'s measured *"about 10 dB"*, agreeing to 0.96 dB. That is a single
scalar, which is exactly why P7 stays open.

### P7b — Six effects the model has no term for at all — **NEW 2026-09-07**

Every check before revision 2 audited *constants*, so it could only find gaps where a constant
exists. `RADIO-VS-REALITY.md` §7 records six effects for which `GeometricChannel` has **no term at
all**, ranked by evidence × impact ÷ cost. **All six are still the DEFAULT behaviour**, which is
what both pinned digests measure and what every gate grades.

| | Gap | Opt-in arm, default off |
|---|---|---|
| **G1** | **The NLOSv blockage loss is redrawn per packet, not per link.** TR 37.885 6.2.1 specifies *"max {0 dB, a log-normal random variable}"* — one draw per blocked link. Ours redraws at σ = 4.5 dB on every CAM, demoting a large-scale term to fast fading, shortening burst-loss runs, and double-counting against the Nakagami fade. A pure conformance defect, cheap to fix — **do this one first** | `radio_nlosv_hold=True` |
| **G2** | **No antenna gain or pattern term.** TR 37.885 6.1.4 Tables 6.1.4-8/-9 and 6.1.4-10A…D specify a per-vehicle-type directional pattern. The largest omission, and the confound that forced the withdrawal of the decorrelation "falsification" | `radio_antenna_pattern="tr37885_opt1"` |
| **G3** | **No two-ray ground reflection and no breakpoint.** At our own 1.5 m antennas and 5.9 GHz the physical breakpoint is 177.1 m, inside the operating range | `radio_breakpoint="two_ray"` |
| **G4** | **Blockage variance does not grow with blocker size**; σ is one value for a motorcycle and an articulated truck alike. Falsified by Segata et al. | **none, and none possible yet** — no licence-clean source found states a fitted σ *per blocker class*. Blocked on evidence, not effort |
| **G5** | **Blocker footprint ignores vehicle type** — one half-width constant | `radio_blocker_width="tr37885"` |
| **G6** | **No temporal correlation in the small-scale fade.** Defensible at 30 m/s; optimistic at a stop line, which is the reference arm | **none** |

Five of those arms landed from a concurrent workstream on 2026-09-07, all defaulting to the historic
behaviour. **One of them carried a provenance defect of exactly the kind this revision exists to
correct**: `radio_nlosv_model="measured_boban"` anchored its 100 m level on the GEMV² 5/13/20 triple
— simulator output — while calling itself a measurement-based arm. Its *slope* anchor is genuinely
measured. See `RADIO-VS-REALITY.md` §7.7.

**RESOLVED LATER THE SAME DAY: the arm is RETRACTED**, knob and constants deleted, and written up as
a negative result in `CHANNEL-PHYSICS.md` §6 R2. Graded against Segata et al. — the one independent
5.9 GHz measurement in its own citation set, quoted in P7c below — it was **+11 to +15 dB wrong
where the specification it replaced is −0.96 / +4.04 dB**. Two of the gaps in the table above (the
antenna height and the Case-1 boundary, `RADIO-VS-REALITY.md` §4a/§4b) were closed in the same pass,
and neither pinned digest moved.

### P7c — The NLOSv term is falsified against distance — **NEW 2026-09-07**

Our NLOSv excess loss is flat at 9.0382 dB from 1 m to 541 m by construction. Two measured campaigns
state the opposite **in body text** (not in figures): Boban's thesis §2.3.1 measures ≈20 dB at 10 m
falling to ≈7 dB at 100 m for a van, at 5900 MHz 802.11p with the blocker 37 cm above the antenna
tips — our `both_below` branch exactly — with a standard deviation under 1 dB; Segata et al. measure
≈10 dB at 80 m falling to ≈5 dB at 120 m for a truck at 5.89 GHz. **Our error changes sign across
the band our scenarios occupy**: −11.0 dB at 10 m, +2.0 dB at 100 m, +4.0 dB at 120 m.

This is TR 37.885's limitation, faithfully transcribed, not a transcription error. It bounds what
"realistic radio" can mean for this project until a distance-dependent NLOSv term is the default.
**The opt-in one that briefly existed (`radio_nlosv_model="measured_boban"`) was retracted the same
day** (P7b): measured against the Segata numbers in the paragraph above, it was one-sidedly +11 to
+15 dB wrong where the flat specified term is −0.96 / +4.04 dB. **So this gap is still open, and it
is open having survived one attempt to close it** — which is the useful part of the result. A
successor must be named for what it is, carry no figure-digitised or licence-unclear constant, and
be graded against Segata et al. before it ships.
Revision 1 of `RADIO-VS-REALITY.md` reported this as "unresolved — could not verify";
both sources are openly reachable and text-extractable, and that entry was a retrieval failure.

## Standing method

Every item ships with a quantitative gate, and nothing counts as done until an independent agent has
re-run the measurement or the attack. Three times in this project a headline result did not survive
that step: a GEH gate that graded a simulation against itself, a seed-stability gate too loose to
catch a 2× regression, and a plugin firewall defeated by one line of frame introspection.
