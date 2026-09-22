Terms as this project uses them. Where a word is used differently elsewhere in the
literature, the entry says so, because a glossary that hides a disagreement is worse
than no glossary.

## The simulator's own vocabulary

| Term | Meaning |
|---|---|
| **Model card** | The mandatory declaration every model ships: id, family, version, tiers, purpose, equations, parameters with unit/default/source, assumptions, limitations, what the tier ignores, validation status and whether it draws random numbers. A model with no card cannot be registered. This site's [model reference](models.html) is generated from cards |
| **Family** | One seam of the architecture — propagation, MAC, detector, metric, and so on. A family is a trait plus a card enum value plus a conformance suite |
| **Fidelity tier** | `abstract`, `medium` or `high`: which physical effects a model represents. A statement about coverage, not about quality. See [methodology](methodology.html) |
| **Registry** | The table of registered models. It validates each card, hashes its canonical bytes, records a licence and a hosting mode, and is what the manifest pins |
| **Manifest** | The record a run writes: engine version, master seed, world content hash, every model's `id@version+hash`, the crypto mode, time-dilation windows and file digests. Replaying from a manifest is what reproducibility means here |
| **Provenance** | The mapping from a displayed value back to the model and parameter set that produced it — what the user interface's "why" panel reads |
| **`SimTime`** | Simulated time, `u64` nanoseconds since the scenario's `t0`. The only clock engine-facing code may read |
| **DES kernel** | The discrete-event core: an event heap ordered by `(time, priority, sequence)`, advancing to the next event rather than in fixed ticks |
| **RNG domain / stream** | A random stream keyed by `(domain, entity)` and derived from the master seed by hashing, so one entity's draws are independent of every other entity's activity, of event order and of thread count |
| **Quantum** | The grid an exported float is rounded to at the writer, so a digest survives a change of compiler, math library or target |
| **`sum_ordered`** | The reduction every parallel phase uses: contributors sorted by id, then summed, so floating-point addition's non-associativity cannot make two runs disagree |
| **`NodeView`** | The belief-only handle a node-hosted plug-in is given. It has no path to the world, which is how the ground-truth firewall (invariant I-C2) is enforced by the type system rather than by discipline |
| **Ground truth** | What is actually true in the simulation, as opposed to what a node believes. A detector is measured against ground truth and must never read it |
| **Belief** | A node's own estimate of its position, time and neighbourhood, produced by the GNSS and clock models. Deliberately imperfect: a node whose belief equals the truth makes every plausibility detector trivially correct |
| **`todo-calibrate`** | A source kind meaning "no citation yet". It requires a calibration plan, and every parameter carrying it appears on the [calibration debt](calibration.html) page |
| **Conformance sentinel** | A check that fails the build when an invariant is violated — for example a plug-in reaching ground truth. Held to the standard that it must have been observed to fail |
| **VWP** | The engine-to-user-interface protocol: a flat fixed binary layout carried over a WebSocket, with `Keyframe` and `Delta` messages, stored byte-for-byte by the recorder so live and replay are provably identical |
| **Keyframe / delta** | A full state snapshot, and the changes since the previous one. A seek loads the nearest keyframe at or before the target and applies deltas forward |
| **Leakage linter** | The check on exported datasets that refuses fields a node could not have known, so a published dataset cannot accidentally teach a model the answer |

## Messages and security

| Term | Meaning |
|---|---|
| **BSM** | Basic Safety Message: the SAE J2735 periodic broadcast of a vehicle's state (position, speed, heading, brake status), the North American equivalent of the CAM |
| **CAM** | Cooperative Awareness Message: the ETSI EN 302 637-2 periodic broadcast of a station's state |
| **DENM** | Decentralized Environmental Notification Message: the ETSI event-triggered message (hazard, hard braking, road works) |
| **SPaT / MAP** | Signal Phase and Timing, and the intersection geometry it refers to |
| **OBU / RSU** | On-Board Unit (the radio and compute in a vehicle) and Road-Side Unit (the fixed infrastructure station) |
| **ASN.1, UPER, COER** | The message description language, and the two encoding rules used here: Unaligned Packed Encoding Rules (ETSI messages) and Canonical Octet Encoding Rules (IEEE 1609.2 structures) |
| **SPDU** | Secured Protocol Data Unit: an IEEE 1609.2 signed (or encrypted) envelope around a message payload |
| **Signer identifier** | What a signed message carries to identify the key: a full certificate, or an eight-byte digest of one. Sending the certificate periodically and the digest otherwise is what makes a frame's length alternate between two values |
| **Pseudonym certificate** | A short-lived certificate carrying no long-term identity, so that tracking a vehicle across certificate changes is hard |
| **Butterfly key expansion** | The CAMP SCMS construction in which a device sends one "caterpillar" key pair and an expansion function, and the authority derives many unlinkable certificate keys from it, so the device need not send one request per certificate |
| **Linkage value** | A per-certificate value derived from linkage seeds held by two independent Linkage Authorities, combined so that no single authority can link a vehicle's certificates, but the two together can revoke all of them at once |
| **CRL** | Certificate Revocation List. In SCMS it can revoke by linkage value, which is what makes revoking a whole vehicle possible without linking its certificates beforehand |
| **SCMS** | Security Credential Management System: the North American V2X public-key infrastructure, with a Registration Authority, Enrollment and Pseudonym Certificate Authorities, Linkage Authorities and a Misbehavior Authority |
| **ETSI ITS PKI** | The European equivalent: Enrolment Authority, Authorization Authority, Enrolment Credentials and Authorization Tickets (ETSI TS 102 941) |
| **Misbehaviour detection / authority** | Local detectors on a node that flag implausible messages, and the back-end pipeline that collects the reports, decides, and revokes |

## Radio and network

| Term | Meaning |
|---|---|
| **DSRC / ITS-G5 / 802.11p** | The Wi-Fi-derived V2X radio: ad-hoc, no association, operating outside the context of a basic service set |
| **C-V2X PC5** | The cellular sidelink: LTE-V2X Mode 4 and NR-V2X Mode 2 schedule their own transmissions without a base station |
| **SPS** | Semi-Persistent Scheduling: a sidelink node reserves a recurring resource, senses the medium to pick it, and reselects periodically |
| **DCC** | Decentralized Congestion Control: the rules that reduce transmission rate or power as the channel fills |
| **CBR** | Channel Busy Ratio: the fraction of a measurement window in which the channel was busy. The input DCC reacts to |
| **CSMA/CA, EDCA** | Listen-before-talk with collision avoidance, and its prioritised form with four access categories, different inter-frame spacings and contention windows |
| **PER / PDR** | Packet Error Rate (a link-level probability given signal quality) and Packet Delivery Ratio (the fraction of intended receptions that succeeded) |
| **SINR** | Signal-to-Interference-plus-Noise Ratio: received power over the sum of concurrent transmissions and thermal noise |
| **Path loss / shadowing / fading** | Distance-dependent attenuation; slow variation from obstructions, modelled as log-normal and spatially correlated; fast variation from multipath, modelled here as Nakagami-m |
| **LOS / NLOSb / NLOSv** | Line of sight; blocked by a building; blocked by a vehicle. The three link states the propagation models distinguish |
| **MCS** | Modulation and Coding Scheme: the rate the transmitter picks, which fixes both air time and the required SINR |
| **WSMP / GeoNetworking / BTP** | The North American short-message protocol, and the European network and transport layers |
| **Hidden terminal** | Two transmitters that cannot hear each other but share a receiver, so listen-before-talk does not prevent their collision |

## Mobility and world

| Term | Meaning |
|---|---|
| **IDM** | Intelligent Driver Model: the car-following rule giving longitudinal acceleration from speed, gap and closing rate. Every stopping rule in this engine — red lights, yielding, curve caps — is expressed as a virtual leader so this one equation produces every deceleration |
| **MOBIL** | The lane-change decision rule: change if it improves your acceleration enough without imposing more than a set cost on the vehicle behind you in the target lane |
| **Gap acceptance** | The rule for entering an unsignalised junction: accept the crossing gap if it exceeds a critical value |
| **OD demand** | Origin-destination demand: how many trips per hour flow between each pair of zones, from which vehicles are generated |
| **VRU** | Vulnerable Road User: pedestrians and cyclists |
| **Lane-level network** | A world in which each lane has its own centreline, width, connections and turn restrictions, rather than a graph of road centrelines |
| **World content hash** | The digest of the imported world. Part of the manifest, so a run cannot be replayed against a different map without the mismatch being visible |
| **Equipped fraction** | The share of vehicles carrying a working V2X station. A quantity results are extremely sensitive to, and one every published figure should state |

## Tooling

| Term | Meaning |
|---|---|
| **MCAP** | The recording container: a chunked, indexed, self-describing log format, which is what makes a sub-100 ms seek on a long run possible |
| **Parquet** | The columnar format the dataset exporters write for analysis |
| **`just`** | The task runner this repository uses. `just --list` shows every recipe |
| **VeReMi** | A published misbehaviour-detection dataset and its attack taxonomy, used here as a comparison point for detector evaluation |
