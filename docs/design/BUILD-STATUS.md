# Build status

Living record of what is built and what has actually been *measured*, as against
the plan in `10-roadmap.md` and the decisions in `12-build-decisions.md`. Claims
here carry their evidence; anything unmeasured says so.

Last updated 2026-09-24 (QA section below). **The crate table below is stale**: `v2xw-record`,
`v2xw-metrics`, `v2xw-node` and `v2xw-engine` are no longer stubs, and the line counts
predate several waves. It is left as written rather than rewritten from memory, because a
status file whose numbers were re-estimated rather than re-measured is worse than one that
says it is out of date.

For the current release position — what must be true for a 1.0 tag, what is not true, and
what each gap would take — see [`docs/RELEASE-CHECKLIST.md`](../RELEASE-CHECKLIST.md)
(2026-09-22). The Phase 1 acceptance table below is still accurate and the checklist cites
it.

## 2026-09-24 — QA of the website on the release build: final state

The QA lead drove the page as the owner would — `v2xw-server` built with
`cargo build --release -p v2xw-server -p v2xw-cli` on ports 8787 (behind a TCP proxy that can
cut every connection) and the Studio's dev server on 5173 — and fixed what could be fixed
directly. Every number below comes from a command or a Playwright script run on this
machine; the scenarios were copies of `manhattan-5min.yaml` (`qa-manhattan`: 20 s at
6,000 veh/h) and of `revocation-latency.yaml` (`qa-secure`: 40 s, one roadside unit,
attackers from 10 s, 30 s pseudonyms, a 20 s CRL cadence).

### Fixed during QA, one commit each

| Commit | What was wrong, as found on the page |
|---|---|
| `69d0c14` | Loading a ready-made scenario kept unapplied edits (a refused one included) and rebased them onto it, so the next Run was refused for a field the form no longer showed as edited. |
| `0a2466a` | `nodes.default_obu: obu/cohda-mk5`, offered by the page, ran 20 s of Manhattan with 22 vehicles and sent no frame: the profile publishes no P-256 signing cost. The loader now refuses such a profile by name and lists the ones that sign. |
| `d672594` | The chase HUD said `profile: n/a` for every vehicle, beside a state tab naming its profile: vehicles that spawn after the Hello are not in its node table. It now reads `inspect.node`. |
| `b236001` | **Honest traffic rejected at every i-period boundary.** `CrlGate` derived `Default` with `skew: 0`, and every node's gate comes from `Stores::default()`. A certificate one period away was refused, the signature detector reported it, and the authority revoked honest vehicles. Found with the lifecycle compressed to 60 s periods: 3,645 of 33,361 verifications invalid and 2 honest devices revoked, no attacker. Default is now `CrlGate::new(0)`. |
| `b0cf71e` | The `rsu` camera on a run with no roadside unit logged "needs a vehicle to follow … click a vehicle first". |
| `97f0f11` | `v2xw run --duration-s` was applied after validation: a valid override was refused and a shortening one was not checked. |
| `ac3047d`, `ce6bbb6` | Three settings change nothing on a BSM run by design (`radio.tiers.phy: abstract` under a medium MAC, `security.envelope: etsi103097`, `messages.codec_tier: size-model`), and the page gave no reason. Their engine notes now say so, and a fully applied field shows its note once edited. |

Each Rust fix has a test shown red without it. After the fixes: `v2xw-node` 192 pass,
`security_lifecycle` 9/9, engine `scenario` 13/13 and lib 46/46, `v2xw-cli` 16/16,
`v2xw-conformance` 69/69 (the `grid-traffic` golden reproduces unchanged), Studio vitest
165/165, typecheck clean. The release binaries were rebuilt at `ac3047d` (`ce6bbb6` changes only
the page) and the page re-checked.

### What was checked, and what it showed

- **Every control.** A scripted pass over the header, settings panel, transport bar,
  viewport toolbar, inspector, message panel, measurements strip and the Runs, Compare
  and Commands tabs, on a live Manhattan run. All behaved as labelled. Details:
  - The scrub bar moved 8.8 s to 30.8 s on a click at 10 %.
  - 14 of 19 overlays toggle. The other 5 are disabled and labelled "not available in
    this build".
  - The metric picker lists 223 metrics.
  - The engine-backed suite (`e2e-engine`, 9 tests, including its own 18-row control
    table) passed 9/9, twice, on the final release binary.
- **Every settings field.** 111 edits covering 106 of the page's 114 fields. For each, a
  base preset was loaded, the field was edited in the page and Run was pressed. The rest
  were covered separately: the focus-region fields through the page, `events` by
  `events.spec.ts`.
  - 54 moved the run's digest in the expected direction. Examples: OBU 10 dBm, mean RSSI
    −107.5 → −114.5 dBm; buildings off, 1,902 → 27,504 receptions; `time_dilation`, 424
    suppressed frames.
  - 25 were refused, each with the engine's reason. Four refusals are phrased
    "internal error": a missing map or DEM file, an RSU `site` on an imported city, and a
    wrong `net.backend_net` id.
  - 31 left the digest unchanged, each for a stated reason: descriptive fields; the
    default value; nothing in a 40 s run reached the backend; or the three notes above.
  - `keep_holes` and `metres_per_level` changed nothing in a 20 s run and were not
    investigated further.
  - The TR 36.885 drop put 1,250 vehicles on Manhattan and did not finish 20 s within
    240 s.
- **Soak, 20 runs through the page.**
  - The runs included seed, radio and rate edits, two scenario switches, and Run pressed
    twice while a run was playing at 1×.
  - 20/20 finished and streamed to their last instant. The connection stayed streaming,
    with no page or Studio errors.
  - Engine RSS was 67 / 282 / 273 MB at runs 1 / 10 / 20. RSS is compressed on this
    machine; the physical footprint was 506 / 447 / 541 MB.
  - Tab heap was 499 / 454 / 453 MB.
- **Network drop.** All 6 connections were cut at 12.1 s into a 1× run. The page was
  streaming again 1.6 s later, with `HELLO_RESUMED` at seq 135. It applied 441 frames,
  seq 0–440, with no gap, no duplicate and no error.
- **Rendering suites against the mock.**
  - Camera fuzz: 36 steps, 0 failures. Scene validation: 15/15. Studio and regressions:
    8/8.
  - Capture tour: 0 vehicles drawn inside a building (1,600 samples), 0 wrong signal
    heads (14,400 head-checks), chase jitter RMS 7.5 px. All seven pictures were read.
- **Traffic auditor, release, Manhattan with VRUs at 6,000 veh/h for 300 s.**
  - Two runs gave identical counts. The run had 340 vehicles, 538,926 vehicle-steps and
    599,834 pedestrian-steps, 46,517 of them on crosswalks.
  - Zero counts: overlap, teleport, red and amber entry, conflict zone, illegal
    transition, lane change near a junction, queue jump, speed jump, heading flip, accel
    bound, standstill, mid-road despawn, occupied-crosswalk entry, pedestrian
    don't-walk entry, and all three world checks.
  - Non-zero: heading jump 147, gap below s0 217 (one follower creeping at 0.2 m/s to
    1.85 m behind a stopped leader, s0 2.0 m), jerk 37, step-speed 18, in-building 12
    (the newsstand), and **pedestrian overlap 15**.
  - Steps on passages through buildings: 5,834.
  - Without VRUs at the same rate, gap below s0 is 23 and heading jump 143. The
    junction track reported 0 and 177 before wave B.
- **Metrics on a dense run** (227 vehicles, 90 s).
  - Delivery: `pdr` 0.334, `pdr[100m]` 0.916; `per` = 1 − `pdr`. By distance: 0.999 at
    0–50 m, 0.864 at 50–100 m, 0.535 at 100–150 m, falling to about 0.04–0.08 beyond
    400 m. Line-of-sight avenues keep 0.072 beyond 1 km.
  - End-to-end latency: p50 14.69, p95 19.19, p99 19.60 ms. The ten stages sum to
    exactly the 14.708 ms mean, and their shares sum to 1.0000.
  - Loss causes plus delivery sum to 1.0003.
  - Bytes: the buckets sum to `bytes_total` (212,822 B/s), and the per-vehicle-hour
    buckets sum to their total. Offered load 1.703 Mbit/s against carried 1.7026.
  - Channel: CBR 0.015.
  - Overheads: security 0.564, link 0.200, full-certificate share 0.102 (one in ten, as
    J2945/1 says).
  - Awareness: time-weighted AoI 289 ms against a per-delivery peak AoI of 140 ms, which
    is consistent because the two are weighted differently. NAR 0.98 at 100 m and 0.45
    at 300 m.
- **Radio**, the same 60 s Manhattan run:

  | | mean RSSI | PDR 100 m | PDR 300 m | e2e p50 / p95 | air time |
  |---|---|---|---|---|---|
  | DSRC | −107.9 dBm | 0.922 | 0.313 | 14.7 / 19.1 ms | 0.30 ms |
  | LTE-V2X | −99.0 dBm | 0.967 | 0.532 | 63.8 / 102.8 ms | 1 ms |
  | NR-V2X | −98.9 dBm | 0.980 | 0.518 | 25.0 / 50.4 ms | 0.5 ms |

  - Transmit power 10 dBm: DSRC −114.9 dBm, LTE −111.9 dBm. LTE drops exactly the
    13 dB it lost; DSRC was at a mean of 17 dBm under J2945/1 power control.
  - Medium propagation: DSRC −109.4 dBm. Buildings off: −79.2 dBm.
- **Message sets.**
  - In 30 s: SPaT 300 (10 Hz), MAP 30 (1 Hz), PSM 1,501 from 50 VRU devices. On
    GN/BTP: CAM 2,813 and VAM 5,263.
  - DENM: 100 from hard braking in a dense 60 s run.
  - Fragmentation: 1,600 B padding with `generic-sdu` doubled the frames and gave SDU
    loss 0.81 against fragment loss 0.80.
  - A hybrid Falcon signature with no fragmenter: the certificate-carrying frames above
    the MTU were refused (2,241 → 2,008 frames).
  - Engine tests: `message_sets` 5/5 (including SPaT against the lamps), `fragmentation`
    7/7, `timeline` 9/9, `attack_wave` 1/1, `phase2` 8/8.
- **Scenario events** on Manhattan, 90 s at 15,000 veh/h.
  - Closing FDR Drive at 15 s wrote the record "43 lanes closed". Entries onto its
    busiest edge fell from 5 to 0.
  - `param.change` of the arrival rate to 0 at 45 s: vehicles first seen after 45.2 s
    fell from 113 to 1.
- **Security.**
  - Pseudonyms: 600 changes at exactly 30.000 s intervals. Certificate, temporary ID and
    link-layer address changed together 600/600, and the chain was consistent 416/416.
  - Top-up with a compressed lifecycle: 93 started and 91 completed, 455 certificates,
    312 kB up and 123 kB down over cellular Uu.
  - Revocation, seven ConstPos attackers over 180 s:
    - 6 of 7 were revoked, in 11.19 s from detection to enforcement. Stage times: report
      9 ms, shuffle 1.74 s, decision 1.76 s, issue 1.84 s, publish 11.10 s.
    - 567 CRL downloads and 24 RSU broadcasts; 24,952 receptions from revoked senders
      were rejected.
    - **1 honest vehicle was also revoked.**
- **Chase inspector.** A clicked or chase-adopted vehicle lists its sent BSMs, each
  decoded field by field with the 1609.2 header and the octets by layer. It also lists
  received messages with fate, RSSI/SINR, distance and delay by stage, and queues whose
  depths change between reads.

### Still open, most important first

1. **Honest vehicles are revoked with no attacker in dense traffic.**
   - The shipped `revocation-latency.yaml` with its attackers removed revoked 21 honest
     vehicles in 300 s. Its detectors fired 1,040 positionSpeedInconsistency, 611
     headingInconsistency and 204 positionJump verdicts, which became 1,582 reports.
   - `qa-secure` without attackers revoked 1 in 180 s on the fixed binary.
   - Mechanism: the GNSS model's outliers (1 %/s, 12 m) and 3 s bursts deliberately
     under-report their accuracy. The receiver's detector uses a constant 5 m confidence
     rather than the BSM's. At 6,000 veh/h, one burst is seen by enough neighbours to
     pass the authority's gate: 3 reporters over 4 s within 15 s.
   - `phase2.rs::with_no_attacker_nothing_is_revoked` passes only at 600 veh/h. This
     needs a calibrated decision, not a QA patch.
2. **Vehicles hit pedestrians.** There were 15 vehicle–pedestrian overlaps, down to 0.69 m
   between centres.
   - The pedestrians were on sidewalk lanes that lie on the roadway: 811 of 7,741 sidewalk
     lanes overlap a drive lane by more than 0.3 m, about 10.7 km in all.
   - Example: sidewalk 2714 runs 0.73 m from 6th Avenue's lane 1339.
   - This is importer geometry.
3. **The "sign" latency stage includes the J2945/1 hand-off jitter.**
   - `t_signed` is stamped after the jitter (`Engine::hand_down`), so `sign` reads 13.9 ms
     for a 9 ms HSM signature.
   - With `compute_tier: abstract` (1 µs signing) it still reads 4.94 ms.
   - The total is right; the split is not. The fix needs a stage and a record field of
     its own, which moves the golden.
4. **Traffic:**
   - Heading jumps: 147.
   - Gap below s0: 217. One follower creeps to 1.85 m. This was 0 in the junction
     track's report and is 23 without VRUs.
   - Jerk: 37. Step-speed: 18.
5. **Sidelink runs have no `cbr` metric.** It is empty for LTE-V2X and NR-V2X, though the
   chase view shows CBR on a sidelink frame.
6. **Page:**
   - The stats chip says "0 RSUs" on a run with a roadside unit placed by `position_m`.
   - The inspector says "radios 82" beside "106 vehicles or roadside units".
   - The HUD shows "(indices pending — node.tx)" even under the SCMS lifecycle.
   - A page reloaded after a run finished shows no measurements.
   - The tab heap is about 450 MB on Manhattan.
   - One page load in about 10 fell back to the built-in settings list under heavy CPU.
     It was not reproduced in 3 further loads.
7. **Wording:**
   - Four run-time refusals are prefixed "internal error" although the cause is the
     user's input.
   - `TimelineKind::Closure`'s doc says `lane` or `edge`; the loader wants
     `target: "edge:N" | "street:NAME"`.

### Completeness against the owner's request

| Asked for | State | Evidence |
|---|---|---|
| Parameters applied actually take effect | delivered | 106 fields through the page; 54 change the run, 25 refused with a reason, 31 unchanged with a stated reason; two carry-over and note fixes |
| Every button, config and feature works | delivered, with the open items above | control pass; `e2e-engine` 9/9 twice |
| Runs keep working after a while | delivered | 20-run soak, Run pressed mid-run twice, memory flat from run 10 |
| Traffic smooth and correct | partial | 0 overlaps, teleports and red entries; 147 heading jumps, 37 jerk events and 217 gap steps remain |
| Cars through buildings | partial | 12 steps at one newsstand; 0 drawn in buildings by the viewer |
| Camera blacks out or goes underground | delivered | fuzz 36/0; scene validation 15/15 |
| Cars touch each other | partial | vehicle–vehicle overlap 0; 15 vehicle–pedestrian overlaps from sidewalk geometry |
| A car goes round one stopped at the junction | delivered | queue-jump 0; conflict-zone 0 |
| Clean rendering, lights green only sometimes | delivered | 0 wrong heads of 14,400; `signal_heads.rs` |
| Network load, e2e delay, overhead metrics | delivered, one split wrong | stages tile the mean exactly; the sign stage includes the hand-off jitter; no sidelink CBR |
| Certificate lifecycle, pseudonym rotation, CRL distribution | delivered | 30.000 s rotation with every identifier; top-up 91/93; CRL by download and RSU broadcast |
| Backend over cellular, not C-V2X | delivered | reports and top-ups over Uu (2.67 MB up in the attack run); RSU relay path; backhaul |
| Appropriate to protocol and deployment | partial | honest revocations in dense traffic; ETSI butterfly and ECTL not driven |
| Chase view shows the messages, content and queue | delivered | sent and received decoded with octets; queues live |
| The engine improved end to end | partial | the open list above |

## 2026-09-24 — junction geometry, signal timing, passages (junction track)

Every number below comes from `traffic_audit` (`crates/v2xw-engine/examples/traffic_audit.rs`)
or a test run on this machine. Scenarios are run for 300 s. "Dense" means 6,000 veh/h.

| Class | dense Manhattan | manhattan-5min | dense grid |
|---|---|---|---|
| world: conflicting protected greens | 19 → 0 | 19 → 0 | 0 → 0 |
| world: lanes in a building, no passage | 64 → 0 | 64 → 0 | 0 → 0 |
| heading flip | 1 → 0 | 0 → 0 | 0 → 0 |
| heading jump, old 4 m bound | 79 → 39 | 10 → 21 | 0 → 0 |
| heading jump, AASHTO P bound (5.42 m) | — → 177 | — → 46 | — → 0 |
| in a building outside a passage | 12 → 12 | 6 → 6 | 0 → 0 |
| jerk > 30 m/s³ | 52 → 38 | 5 → 4 | 13 → 13 |
| step vs reported speed (new) | — → 7 | — → 2 | — → 0 |
| all other safety classes | 0 → 0 | 0 → 0 | 0 → 0 |

Two dense-Manhattan runs gave identical counts.

What changed:

- **Turns are drivable arcs.**
  - Junction connectors used to be quadratic Béziers. They are now AASHTO simple curves:
    the largest circular arc that fits between the two lane ends.
  - Motor lane corners are rounded the same way (`v2xw_world::curve`).
  - Lane ends are pulled back, up to 7 m, until each turn has room for the P design
    vehicle's 6.4 m centreline radius.
  - The auditor holds each class to its AASHTO minimum path radius. For a car that is
    5.42 m.
- **Forks share their lanes out in order.** Signal plans use split phasing where
  approaches conflict.
- **Change intervals follow ITE 2020, per phase group.**
  - Amber: `y = t + v/(2a + 2Gg)`, clamped to 3-6 s (MUTCD §4D.26).
  - All-red: `r = (W + L)/v`, capped at 6 s.
  - Room is left in the plan for a pedestrian walk phase, which this track does not add.
- **Passages are in the world model** (`World.passages`).
  - Each lane that runs through a building is classified by its OSM tags.
  - Of the 62 such lanes: 39 are tagged (the Helmsley Building portals, the Park Avenue
    Viaduct at Grand Central, basement ramps) and 23 are untagged driveways. The untagged
    ones are counted as the `untagged-building-passage` anomaly.
  - The auditor now accepts a vehicle inside a building only on a passage through that
    building, and it compares heights as well as footprints.
  - The viewer draws an opening in the wall where each passage lane enters a building.
- **Signals.**
  - Every producer now streams one row per head group: the live projector, the fixture
    engine and the recorder. Before, the recorder's signal block was empty, and the fixture
    engine sent only the controller's first movement.
  - Rows are evaluated with the plan's own arithmetic. Previously, at a phase boundary on
    Manhattan, a head showed green while its movements were amber.
  - All producers use one J2735 table.
  - `tests/signal_heads.rs` checks 733,764 Manhattan head-group samples over a full cycle,
    after a seek and after a rewind. All match the engine.
- **Studio.**
  - The aerial view now opens with all of the traffic in frame. The three aerial
    scene-validation tests that predated wave A now pass.
  - At 800 × 520 the viewport is 470 × 278 (it was 360 × 146).

Still open:

- **Heading jumps.** Every remaining jump is on a connector next to a 1 m lane. That lane is
  left where a segment is too short to hold both junctions' areas; it happens on divided
  avenues.
  - Joining those junctions (netconvert `--junctions.join`) cut the P-bound count from 177
    to 52 in a trial. It also produced 8 overlaps between side-by-side connectors, so it
    was not kept.
- **Jerk.** Classified onsets of hard braking in moving vehicles on dense Manhattan:
  - 23 of 30 are junction merge ordering against a vehicle on another path;
  - 4 are on a free road;
  - 3 are same-path leaders;
  - none are from a signal.

  Two merge-ordering changes each lowered the count on one scenario and raised it on
  another, so neither was kept.
- **In a building outside a passage.** All 12 are one newsstand (way 1117866998), mapped
  0.8 m from a lane centreline.
- **Shadow acne.** No pixel test was added. Bias 0, a positive bias and a 128² shadow map
  all left the headless SwiftShader frame free of measurable acne, so a test could not be
  shown to fail.

## 2026-09-23 — five tracks merged: stability, traffic, radio, metrics, rendering

Five engineers worked in isolated worktrees and the integrator merged them into `main` in
that order (merge commits `2cf3e75`, `0f1340d`, `b0ad18a`, `af00eec`, `7636985`), testing
the touched crates after each merge and fixing the seams between tracks in commits of their
own. Every number below comes from a command run on this machine; the release binaries were
built from `main` and driven in a browser.

### What each track delivered

- **Stability.** Apply reaches the next run. Every `run.start` sends a fresh `Hello`, and
  there is one kernel per run: `run.stop` joins it. A world imported once is reused exactly
  (`world.cache`, and in memory). The engine-backed Playwright suite (`e2e-engine/`, 5 tests)
  drives every control against the real server.
- **Traffic.** An invariant auditor (`v2xw_mobility::audit`) checks every vehicle at every
  step, with a fault-injection test for each class. Fixed: commitment at junctions, merges,
  deadlock at a red, insertion, smooth headings, tunnels below ground. Signal state is now
  streamed per signal group rather than per controller. Weather, fleet classes, demand
  models and VRUs now affect driving.
- **Radio.** `radio.rat` runs LTE-V2X Mode 4 and NR-V2X Mode 2 sidelinks as well as 802.11p.
  Buildings obstruct links (Sommer), and so does terrain from a DEM (knife-edge). Each
  vehicle generates at its own phase with J2945/1-style jitter. Also wired: jammers,
  `radio.models`, the focus region and `nodes.compute_tier`.
- **Metrics.** End-to-end delay is split into stages that tile each message's journey, and
  every reception attempt has exactly one recorded fate. New metrics: awareness (AoI, NAR),
  load and overhead (security, network, link, certificate share, bytes per vehicle-hour).
  The PSDU is composed layer by layer; the page offers every series through a picker.
- **Rendering.** Vehicles move along smooth curves between mobility samples. Signal lamps
  show the state for the drawn instant, and each head shows its own group's state. The
  camera stays out of walls and above the road, and the lens is shifted so the HUD does
  not cover the followed car. Plan-view markings no longer shimmer.

### Seams fixed at integration, each in its own commit

- `world.cache` key: it now includes the importer revision and the DEM bytes (`159a371`,
  `b0ad18a`).
- `messages.generator`: the radio track's timing parameters and the metrics track's rule
  parameters now share one validator (`af00eec`).
- Sidelink frames carry no 802.11 framing (`af00eec`).
- Signal group keys were ported into the rendering track's `SignalRenderer` (`7636985`).
- The stream now sends each vehicle's body centre. It used to send the rear-bumper
  reference, which drew every car 2.5 m behind itself (`c2bd4e6`).
- Tests that the merged behaviour had made vacuous were changed to measure what they
  meant, with no assertion weakened (`98fc437`, `f915539`, `894f329`, `45cd5d6`).
- `v2xw-threat` in-the-loop broke under the metrics track's continuous-time verification:
  3,604 reports and 0 of 5 attackers caught. Bisected to the metrics branch; the test host
  now joins claims across steps and gets 72,916 reports with all 5 caught (`246d6b0`).
- Conformance kit: the method count is now 33 and a hash-order false positive is gone
  (`23a81ab`). The first golden record is blessed, `grid-traffic` (`1aaa943`, re-blessed
  in `b66b3cd` and `5cac9a0`). Before each blessing the digest was shown identical twice
  at `RAYON_NUM_THREADS` = 1, 4 and 8.

### Built for the owner's requests during integration

- **Message content in the chase view.** `node.tx` names the pseudonym that signed each
  frame and carries the BSM's decoded Part I (`17f85bc`). The inspector lists the followed
  vehicle's broadcasts, marking any pseudonym change; each row expands to every field, the
  octets by layer and the signing delay. It also lists what the vehicle heard, with fate,
  RSSI, SINR, distance and end-to-end delay (`7954007`, `fdd18ca`).
- **Pseudonym rotation.** It now follows `security.pseudonym_change` (time, distance or
  silent) over a pool of 20 pseudonyms used round-robin. Before this, a period other than
  300 s was ignored, each vehicle held a single pseudonym, and the store alternated
  between two (`544a23e`).
- **Found by driving the release build in a browser:**
  - Clicking a car followed no radio when the page did not know the car's node (`fdd7cae`).
  - The kernel simulated the whole run ahead of the stream, which emptied the message log
    and inflated `run.status` (`d4fe381`).
  - The node's telemetry window, with its queues and CPU load, was never published
    (`1950d08`).
  - The fallback settings list offered radio technologies the engine refuses (`cafa7f5`).

### Evidence

- **Rust, one crate at a time, debug:**
  - core 215, proto 101, world 211 (4 ignored), mobility 214, msg 203 (1 ignored), sec 99,
    radio 263 (6 ignored), net 113, node 186, threat 210, record 239, copilot 68, py 16,
    wasm 6: all pass, at `1aaa943`.
  - metrics 235, server 77, cli 15, experiment 80, conformance 69: all pass, at `5cac9a0`.
  - engine at `5cac9a0`: 115 pass, 2 fail (below).
- **UI:**
  - Typecheck is clean for protocol, viewer, mock-server and studio.
  - vitest: protocol 186, viewer 132, mock-server 45, studio 143.
  - Studio e2e against the mock: 20 of 24 pass (below).
  - Studio e2e against the real server: 5 of 5.
- **Release build:** `cargo build --release -p v2xw-server -p v2xw-cli` succeeds.
- **Live check** (the owner's `run.txt` commands on ports 8787 and 5173, Playwright,
  screenshots read):
  - The aerial view shows 26 vehicles whose positions move between frames. A click on a
    car switches to chase view on `node 21`, which is stopped behind a red stop bar.
  - The inspector lists 50 broadcasts (BSM #116, id `ac285d32`, 40.75261°, -73.97935°,
    176 B) and 50 receptions, all delivered and verified. The queues show 0/0, 1/1, 0/0,
    1/1, 0/0 (p50/p95), and the HUD shows the pseudonym `ac28…5b`.
  - Pause stops the clock at 00:02:00.500; one step moves it to 00:02:00.599.
  - After editing the duration to 20 s and the seed to 0x2a, Apply says "Applied 2
    changes". The run finishes at 20 s, and Run again gives the same digest,
    `d0d6139e…`. No page errors.

### Still open

- **Engine `phase2.rs`, 2 of 8 fail** (they failed on `main` before this wave):
  `with_no_attacker_nothing_is_revoked` and
  `the_detector_suite_has_false_positives_on_honest_traffic`. The legacy-12 suite fires on
  3.51 % of honest messages (positionSpeedInconsistency 1.84 %, headingInconsistency
  1.74 %). Of the 3 candidate pairs, the two linkage authorities refuse 2, and 1 innocent
  device is revoked.
- **Studio mock e2e, 4 failures** (they fail on `main` too, per the rendering track):
  - The aerial view opens at a fixed 1,400 m extent: 54 of 200 vehicles are outside the
    frustum, and 0.735 of the map is populated against a 0.9 floor.
  - A vehicle mark covers 16 px against a 40 px floor.
  - The inspector's radio count reads 178 against 191 nodes in the table.
  - `the nothing in the interface covers the vehicle` test is flaky under load: 2 of 3
    passes, drift 0.066 against 0.03. Q3 scrub failed 3 times and passed 3 times across
    runs; the e2e file itself notes that a Vite hot reload produces exactly that failure.
- **Not built:**
  - The backend path over cellular (Uu). `net.uu`, `net.backhaul` and `net.backend_net`
    are read by nothing. Vehicle-to-SCMS traffic (enrolment, pseudonym top-up, misbehaviour
    reports, CRL download) uses a constant backhaul latency with no capacity limit, and
    CRL distribution is an RSU broadcast only.
  - A pseudonym certificate's expiry and re-provisioning during a run: the bootstrap pool
    is valid for the whole run.
- **From the tracks:**
  - Traffic, Manhattan at 6,000 veh/h:
    - 79 heading jumps, from OSM connectors tighter than a 4 m radius.
    - 12 in-building steps at covered ramps.
    - 64 lanes through building footprints, which are real passages in the source.
    - 19 conflicting protected greens in synthesised plans.
    - No all-red interval.
    - No VRU device (PSM/VAM).
  - Radio:
    - No sidelink congestion control is enforced, and there are no blind retransmissions.
    - The 1 km candidate range truncates reception.
    - The NR BLER is a fit, not a measured curve.
    - `phase2-manhattan.yaml` runs with buildings off, because one mast hears 2 of 30
      reports with them on.
  - Metrics:
    - Only `fragmenter/none` exists.
    - The size-model codec tier is refused.
    - Per-node breakdowns are only in the recording.
  - Stability:
    - `events` beyond outage and weather are inert.
    - A reconnect gets a resync, not a true resume.
- **Behaviour the owner will see:**
  - With buildings obstructing, the Midtown 5-minute run delivers 9,970 of 146,618
    reception attempts (PDR 0.07). The attempts include every pair within 1 km; within
    100 m, delivery is 0.85 to 1.0 (radio track, `radio_access.rs`).
  - The e2e-latency plot kept a previous run's history after Run again with a short run
    (`e2e_latency.p95` axis 50–125 s on a 20 s run). Not investigated.
  - Forward seeking now reaches only as far as the kernel's bounded lead, 12.8 s by default.

### Wave B — eight more tracks, merged 2026-09-24

Eight engineers worked in isolated worktrees; the integrator merged them into `main` in
this order: session `e6548f9`, geometry `26a0dcb`, vru `8d3ccfc`, radioprop `96703ca`,
radioaccess `724e92c`, messages `2d306c7`, security `58aa4df`, inspector `e3983cf`. The
crates each merge touched were built and tested before the next one, and every seam found
was fixed in a commit of its own. Every number below comes from a command run on this
machine. The integrator deleted the shared debug target first, because worktrees sharing one
`CARGO_TARGET_DIR` had silently linked each other's crates; every result here was built from
the merged tree alone.

#### What each track delivered

- **Session.**
  - A session outlives its socket: a reconnect resumes the stream with nothing missed
    (`HELLO_RESUMED`) instead of resyncing.
  - A seek past the kernel's lead runs the kernel there, reporting progress, and lands.
    Before, the scrub bar could reach only 12.8 s ahead.
  - A rewind keeps the run's speed, and every per-run view resets on a new run.
  - Every scenario-timeline kind now acts and writes a `scenario.event` record: closures
    with re-routing, demand multipliers, `param.change` for the live parameters, and attack
    waves. The page edits the events and draws them on the time bar.
  - Tests: server resume 7, rewind 3, seek_ahead 1, pacing 1, timeline 1; engine
    `timeline.rs` 9 and `attack_wave.rs` 1.
- **Geometry.** Junction turns are AASHTO arcs, forks share their lanes out in order,
  change intervals follow ITE, roads through buildings are passages, and every producer
  streams one signal row per head group. Its own section above (2026-09-24, junction track)
  has the before/after audit counts.
- **VRU.**
  - Crosswalk lanes and MUTCD pedestrian signal intervals, from OSM `footway=crossing` and
    on the grid.
  - Vehicles yield at crosswalks, and pedestrians obey walk signals.
  - Pedestrians and cyclists can carry VRU devices hosted in the node phase. They send real
    SAE J2735 PSMs and ETSI VAMs, the VAM generated from TS 103 300-3 V2.2.1.
  - Cyclists are drawn as riders, and VRUs have their own aerial mark.
  - Three new audit checks (`Check::ALL` is 26 after the merge);
    `pedestrian_invariants` 4/4.
- **Radioprop.**
  - The Mangel 2011 street-corner model, a geometric city-street law (the new default
    `high` tier) and ITU-R P.838-3 rain at the carrier.
  - `radio.devices` and `radio.range` reach every link. The candidate range comes from the
    link budget, not a fixed 1 km, and arrivals below the noise floor count as energy only.
  - Engine `propagation.rs` 9/9.
- **Radioaccess.**
  - SAE J3161/1 is the default LTE-V2X profile.
  - Sidelink congestion control (ETSI TS 103 574 / J3161 CR limits), blind HARQ
    retransmissions with chase combining, and SCI decoding drawn against the control BLER.
  - An honest 802.11p AIFS.
  - Jammers can ride a vehicle or drive a path.
  - `node.tx` carries a radio view: the MCS by name and, on a sidelink, the slot,
    sub-channels, HARQ attempt and CBR/CR.
  - Engine `radio_access_layers.rs` 10 pass, 1 ignored measurement.
- **Messages.**
  - Roadside units broadcast SPaT and MAP, and vehicles send DENM, SRM and SSM.
  - Every fragmentation strategy runs, with reassembly, timeouts and the loss it amplifies.
  - The headline `pdr` is the 3GPP TR 36.885 packet reception ratio within a stated range.
  - A message reaches the applications when its verification finishes, not at the next
    periodic step.
  - Breakdowns reach the page: delivery by distance, latency by stage, and per-node
    rankings.
  - Engine `message_sets.rs` 5/5 and `fragmentation.rs` 7/7.
- **Security.**
  - The SCMS runs in lockstep with the engine: a pre-run pool, pseudonym top-up over a
    backend link, proxy hand-off, single-certificate revocation and a CRL on its cadence.
  - Vehicles reach the backend over cellular Uu (`net.uu`), or relay through a roadside unit
    with a backhaul.
  - The ETSI ITS PKI runs as a second credential protocol, with passive revocation.
  - Rotation is overlap-aware, and there are no phantom pseudonym changes.
  - GNSS burst and outlier probabilities are per-second hazards.
  - Roadside units detect and report.
  - The two `phase2.rs` tests that failed on `main` now pass: phase2 8/8 (1 ignored
    diagnostic) and `security_lifecycle.rs` 8/8.
- **Inspector.**
  - `node.feed` streams the followed vehicle's sent and received messages, decoded from
    their own octets: the 1609.2 spans, signer, HashedId8 and J2735 fields. It also streams
    the vehicle's queues (receive, verification, application, transmit, CRL) with waits and
    drops.
  - The Studio has a message panel with Sent, Received and Queues tabs. The `frame_tap`
    test shows the tap changes no record digest.
  - Server `feed.rs` 6/6, and a shared vector (`node-feed-v1.json`) checked from Rust and
    TypeScript.

#### Seams fixed at integration, each in its own commit

- **`run.seek` over HTTP** (`49c93b6`). The kernel runs ahead on its own clock, so an HTTP
  seek got -32003 or -32009 depending on a race. The transport refusal now comes first. A
  new assertion is red on the old order.
- **Pedestrian heads streamed Off** (`6abb260`). Geometry's `World::group_signals` found a
  head group only through an approach lane, but vru's walk heads face their crosswalk.
  `signal_heads.rs` is red without the fix (plan sg1 group 100: Off against Red). With it,
  1,930,684 Manhattan samples agree with the engine.
- **SPaT reference** (`d97bf80`). The test's reference got the same pedestrian rule (junction
  7 group 100 read Dark against StopAndRemain).
- **Focus-region sidelink test** (`06364dc`). Radioprop's high tier is line of sight on a
  building-free extract, so the region compared two path-loss laws: 5,803 receptions
  against 5,426. Both runs now use one law. It is 5,422 against 5,426, and red (5,426
  against 5,426) with the high sidelink PHY disabled.
- **The messages merge.**
  - The node phase is a step or a wake over `HostedNode`; a VRU device has no deferred
    checks and is never woken for one.
  - MAP/SPaT payloads and the hard-braking acceleration go to OBUs only.
  - The fragmenting hand-down carries radioprop's and radioaccess's new frame fields.
- **The security merge.** The `security.signature` resize is applied before padding,
  fragmentation and the MTU check, so all three see the resized SPDU.
- **The inspector merge.**
  - The feed push in the session track's connection loop returns `Exit::Park`.
  - The projector keeps the metric breakdowns and the security panel's store beside the
    feed.
  - A resumed Hello keeps the session handling and still re-asks `view.follow`, which is
    idempotent.
- **Radio view in the chase panel** (`6e55ed0`). Radioaccess's radio view reached no client
  after the feed replaced the old message log. It is now `radio.access` on each sent frame,
  shown as an "access" line (live: `dsrc-80211p 6mbps-qpsk-1/2`). The feed vector was
  re-blessed.
- **Breakdown cards** (`8dbda63`). The cards polled metrics the run does not measure, which
  left 27 "unknown metric" page errors in `resume.spec.ts`. They now ask only for what the
  run's catalogue lists, and quietly.
- **E2E tests the merges made wrong, with no assertion weakened.**
  - `lifecycle.spec.ts` (`a3efa9d`): `nodes.backend_tier` is now partly applied, and the
    test asserts that the engine publishes no unclassified or not-implemented leaf.
  - `studio.spec.ts` (`e16f15d`): the legend's five state glyphs are counted apart from
    vru's road-user key.
- **Goldens.** `grid-traffic` was re-blessed after radioprop (`27dc00c`), radioaccess
  (`83eca76`), messages (`1d6b7d0`) and security (`50338d1`). Each merged change equals
  what that track's own branch moved. Before each blessing, the digest was identical
  twice at `RAYON_NUM_THREADS` = 1, 4 and 8. The golden is now 15,914 records, digest
  `2d37766a5fb69289…`.
- `cargo fmt --all` (`d1a4d17`).

#### Evidence

- **Rust**, at `d1a4d17`, all 20 crates one at a time, debug: 2,904 pass, 0 fail, 14
  ignored.
  - core 215, world 223 (4 ignored), mobility 228, msg 210 (1 ignored), net 113, sec 99,
    radio 287 (6 ignored), node 191, metrics 251, record 239, proto 104, threat 210.
  - engine 174 (2 ignored), experiment 80, server 106 (1 ignored), cli 15, copilot 68,
    py 16, wasm 6, conformance 69.
- **UI:**
  - Typecheck is clean for protocol, viewer, mock-server and studio.
  - vitest: protocol 191, viewer 139, mock-server 45, studio 165.
- **Studio e2e against the release server:** 9 of 9 (controls, events, 4 × lifecycle,
  messages, resume, seek-and-plots).
- **Studio e2e against the mock:** 23 of 25 on the first run. The legend test was then
  fixed and passed 2 of 2. The aerial frustum test is still flaky (below).
- **Release build:** `cargo build --release -p v2xw-server -p v2xw-cli` succeeds (8 m 28 s).
- **Live check.** The owner's `run.txt` commands ran on ports 8787 and 5173, driven by
  Playwright, and the screenshots were read.
  - Setting 40 pedestrians and 10 cyclists and pressing Apply said "Applied 2 changes".
    Run started the run.
  - About 18 s later: 84 live actors (34 passenger, 40 pedestrian, 10 bicycle), 55 of which
    moved in 3 s; the run reached 2:06 of 5:00.
  - A real mouse click on a car switched to chase view on node 6.
  - The message panel listed 60 sent BSMs with decoded position, speed and heading. The
    first row opened to: pseudonym `3a66801336da2bd9`, 40 + 93 + 5 + 38 = 176 B, 17.0 dBm
    on channel 172, the access line, and 10.9 ms signing.
  - It also showed 60 received rows and a transmit queue with 2 messages passed through.
  - No page errors and no page exceptions.

#### Closed from the 2026-09-23 list

These items were open above and wave B closed them, each by the track named:

- The two `phase2.rs` failures (security).
- The backend over cellular Uu, and relaying through a roadside unit (security).
- 19 conflicting protected greens, 64 unmarked lanes in buildings, and the missing
  all-red interval (geometry).
- No VRU device (vru).
- No sidelink congestion control or blind retransmissions (radioaccess).
- The fixed 1 km candidate range (radioprop).
- Only `fragmenter/none`, the refused size-model tier, and per-node breakdowns only in the
  recording (messages).
- Inert timeline events, a reconnect that resynced instead of resuming, and forward seeking
  limited to the kernel's lead (session).
- The mock e2e radio-count failure (inspector).

#### Still open

- **Flaky tests.**
  - The mock aerial test "draws one mark per live vehicle" sees 1 of 200 vehicles outside
    the opening frustum in 2 of 3 runs at the merged `HEAD`. Centring the
    world when the traffic needs all of it made this worse (3 of 4 runs failed), so that
    change was reverted.
  - The first engine e2e run once opened on the built-in settings list (a cold start); it
    passed on the two runs after that.
- **The chase view with fragmentation on.** The frame tap hands the whole SPDU for every
  fragment, so the feed shows each fragment as a full message. Fragmentation is off by
  default.
- **SPaT.** It is built from `SignalPlan::group_timelines`, which merges phases. The
  geometry track moved the live signal block to per-phase evaluation because merged phases
  round differently at a boundary. The SPaT test samples at 0.35 s offsets and has not been
  checked at a boundary.
- **Chase HUD.** It shows `n/a` for CPU, RAM, stored certificates, CRL entries and report
  outbox on `manhattan-5min`, which has no security path. The HUD header still says "OBU"
  for a node spawned after the Hello.
- **Settings.** No setting is left that the engine reads nothing from.
  `nodes.backend_tier` became Partial.
- **From the tracks' own reports:**
  - Geometry: 39 / 177 heading jumps, jerk 38 and 12 in-building steps on dense Manhattan
    (see its section). Viaduct ramps are about 38°.
  - VRU: the PSM codec is not oracle-validated. No mid-block jaywalking. VRU devices
    transmit at 20 dBm, not 23.
  - Radioprop: Mangel is read from a reprint. The 25 m side cap is a design choice. Rain
    has no two-run test. The medium tier has no vehicle blockage.
  - Radioaccess: the NR BLER is still a fit. The J3161 values are second-hand. The compute
    tier `high` is partial. Hybrid is refused. The Studio has no widget for
    `radio.models.sidelink`.
  - Messages: no vehicle acts on SPaT (no GLOSA or red-light warning), and SRM priority is
    never granted. CPM is refused. The SPaT/MAP encoders are not independently decoded.
    PSID 0x82 is unverified.
  - Security:
    - ETSI butterfly authorization and ECTL are not driven.
    - Post-quantum sign/verify time is not charged.
    - Several backend-access figures are uncited defaults with no model card.
    - The 55-unit Phase 2 scenario and the Manhattan pseudonym-privacy study were not run
      end to end.
    - `compromised_rsus` is refused.
  - Session:
    - A vehicle that cannot avoid a closed lane leaves as RouteBlocked.
    - Radio and model parameters are refused by `param.change`.
    - An attack wave has a single window.
    - Sessions live in memory, so a resume across an engine restart is a fresh Hello.
  - Inspector:
    - TX-overflow and CRL-backlog drops are on no record channel.
    - "Depth now" can read low when the kernel is slower than real time.
    - No feed from a recording.
    - Roadside units are missing from a live Hello's node table.


## Crates

| Crate | Lines | State | Evidence |
|---|---:|---|---|
| `v2xw-core` | 13,487 | complete | 215 tests, zero clippy warnings. Determinism kernel independently reviewed: no defect in the RNG algorithm, math routing, event ordering, manifest digest or float reduction order. 1 critical + 10 major API defects found and fixed, then 7 more in a completion pass. |
| `v2xw-world` | 20,336 | complete | Imports real Midtown Manhattan in 377 ms. Verified below. |
| `v2xw-msg` | 7,956 | ETSI done, J2735 BSM in progress | ETSI stack generates from the forge modules and compiles (9,811 lines). |
| `v2xw-mobility` | 2,409 | in progress | — |
| `v2xw-radio` | 1,866 | in progress | — |
| `v2xw-net` | 1,374 | in progress | — |
| `v2xw-sec` | 555 | in progress | — |
| `v2xw-record` | stub | in progress | — |
| `v2xw-metrics` | stub | in progress | — |
| `v2xw-node` | stub | not started | Blocked on the radio, net, msg and sec trait definitions. |
| `v2xw-engine` | absent | **not started** | Owed by build-decision D8. Gates the headless end-to-end run. |
| `v2xw-cli`, `-server`, `-proto`, `-py`, `-wasm`, `-threat` | stubs | not started | |

UI: `ui/packages/{protocol,mock-server,viewer}` and `ui/apps/studio` exist; the
conformance and quality review is in progress.

## World import — verified

The importer was checked against the real city, not only against itself.

**Correct.** Projection error is 0.110 m worst case over an 856 m baseline
(0.013 %), cross-checked against three surveyed landmarks by geodesic distance.
The modal drivable heading is 60–61° with 90.5 % of lane length within ±4° of
it, which matches Manhattan's grid being rotated about 29° from true north;
Broadway correctly falls out as the 81° diagonal. Lane ordering is right-hand
traffic in 610 of 610 multi-lane edges and 178 of 178 two-way pairs, with none
wrong. Rendering confirms per-lane centrelines, crossings with stop lines,
sidewalks, turn connections and one-way chevrons. Building heights top out at
443 m with a median of 45 m.

**Scale.** 3,421 junctions (303 signalised), 13,760 lanes (5,450 drivable,
3,029 junction connectors, 8,291 sidewalk), 27,497 connections with 78 banned by
34 turn restrictions, 7,390 buildings, 963 crossings. 295 anomalies across 18
named categories, every one counted with example way ids, no panics.

**Defects found.** Six, recorded with evidence in
`findings/world-import-defects.md`. Two are major and affect any traffic result:
the speed-limit class defaults are SUMO's German rural values, giving 16 % of
drivable lanes a 100 km/h limit on Manhattan side streets, and lane width is a
single global constant so every lane is exactly 3.50 m. A third is worse for
routing: only 52.8 % of driving lanes lie in a strongly connected component.

## Traffic — audited, 2026-09-23

`v2xw_mobility::audit` checks every vehicle at every mobility step against the traffic
invariants (footprint overlap, gap below `s0`, lateral offset, body in a building or
outside its junction, red and avoidable-amber entry, two conflicting movements in one
conflict zone, lane change near a junction or round a standing queue, unconnected lane
transition, teleport, heading jump and flip, speed jump, acceleration and jerk bounds,
gridlock, mid-road despawn) plus three world checks. Measure any scenario with
`cargo run -p v2xw-engine --example traffic_audit -- <scenario.yaml> [--rate R]`; the
gate is `crates/v2xw-mobility/tests/traffic_invariants.rs`, which also runs a control
with the junction rules off that must go red.

300 s runs, before → after (counts are vehicle-steps, or pair-steps for overlaps):

| Class | dense grid (6000 veh/h) | Manhattan (manhattan-5min) | Manhattan, 6000 veh/h |
|---|---|---|---|
| overlap | 183 → 0 | 82 → 0 | 1787 → 0 |
| gap below s0 | 256 → 0 | 96 → 0 | 1189 → 0 |
| red entry | 59 → 0 | 42 → 0 | 287 → 0 |
| conflict zone | 33 → 0 | 14 → 0 | 257 → 0 |
| illegal lane transition | 119 → 0 | 56 → 0 | 515 → 0 |
| teleport | 2503 → 0 | 1419 → 0 | 6523 → 0 |
| lane change near a junction | 39 → 0 | 18 → 0 | 269 → 0 |
| heading flip | 0 → 0 | 19 → 0 | 108 → 1 |
| heading jump | 1198 → 0 | 367 → 10 | 1629 → 79 |
| body in a building | 0 → 0 | 1351 → 6 | 4397 → 12 |
| jerk > 30 m/s³ | 231 → 13 | 122 → 5 | 784 → 52 |

What remains, honestly: heading jumps are vehicles on OSM junction connectors whose
curvature is tighter than a 4 m radius; the bodies in buildings clip the corners of two
buildings built over covered tunnel ramps (The Horizon, The Corinthian); the jerk
excursions are emergency braking the car-following model asks for. The world check still
reports 64 lanes whose centreline runs through a building footprint — Park Avenue's
portals in the Helmsley Building and covered ramps, at grade in the source — and 19
conflicting protected greens in the synthesised OSM signal plans (crossing lane
assignments within one approach). Tunnels now run below ground and bridges above it.

Mean speed 4.4–5.8 m/s with 10–15 % of vehicle-steps standing;
no vehicle stands longer than 46 s.

## Phase 1 acceptance criteria

Three of seven are now met with independently reproduced evidence. "Independent"
means a second agent re-derived the number from the standard or the literature
rather than re-running the builder's test.

| # | Criterion | Status |
|---|---|---|
| 1 | Golden determinism, identical digests on each operating system | **Gate built, never run.** CI imports a committed fixture on all three platforms and fails unless the digests agree. Determinism itself is repeatedly proven *within* a platform: a double import of the real 30 MB extract is byte-identical across all four artefacts in separate processes, and two recordings of one run have identical data sections. No CI run has executed. |
| 2 | Envelope size equals the size model, zero bytes tolerance | **Met.** Exactly 93 bytes for a digest signer and 87 plus the certificate for a certificate signer, confirmed by decomposing a real signed message octet by octet against the derivation. The derivation's own byte-count threshold was wrong and is corrected (D12.1). |
| 3 | Modelled and real crypto produce identical logs | **Not met.** Genuine but incomplete: a verifier broke equivalence three ways, through signature malleability, invalid key material and unsupported post-quantum primitives. Repairs in flight. |
| 4 | Seek at most 100 ms at the 95th percentile | **Met, 57x margin.** 1.763 ms independently measured on a 9,001-frame, 600-second recording, using a type-7 quantile rather than nearest-rank so the figure is not a ranking artefact. |
| 5 | 60 fps with the heads-up display, every value resolving to a model card | **Met and exceeded.** 604 fps on a real GPU at 5,000 actors against ADR 0011's 60 fps target. Every heads-up value is keyboard reachable and opens its provenance; the focus indicator measures 9.10:1 contrast against a 3:1 floor. |
| 6 | Manifest lists engine, plug-in, world and card hashes | Pending. The world hash exists; manifest assembly is owed by `v2xw-engine`. |
| 7 | Manual map-to-chase fly-down | The automated fly-down passes end to end against the mock engine. Needs a human to judge. |

## 2026-09-23 — the measurement layer measures a real run

What changed, with the evidence (details in `08-measurement-and-data.md` §2.7):

- **Every reception attempt has one recorded fate** on `node.rx` (delivered, lost with one
  cause, or in flight at the end), with every stamp of the message's journey. Checked on a
  real 10-vehicle Manhattan run by `crates/v2xw-engine/tests/measurement.rs`: `node.rx`
  count = `phy.rx` count = the run report's attempts, and M-RX1, M-LAT1, M-BYTE1, M-BYTE2,
  M-SHARE and M-RANGE all hold with data to check. Shown to fail with delivery stamps
  shifted by 1 ns (821 M-LAT1 violations).
- **Frames carry their headers.** A BSM on the air is the SPDU plus 43 octets (WSMP 5,
  LLC/SNAP 8, QoS Data MAC header 26, FCS 4); a CAM over GN/BTP plus 82. Air time is
  computed over the whole PSDU. `net.layer: gn-btp` and `messages.generator` now reach the
  engine (a 200 ms BSM interval halves the frames; shown to fail with the parameters
  disconnected: ratio 1.0).
- **The verification queue runs in continuous time**, so a backlog outlives a tick and a
  frame waits from the instant it arrived. `node.verify` records now decode as the channel's
  reader-side view; they did not, for any record, before.
- **Measured on a 27-vehicle procedural grid, live in the page** (debug build): e2e p50
  10.0-10.7 ms and p95 10.3-21.0 ms (10 s bins), of which the OBU's 9.0 ms HSM signature
  is 61-86 % (30 s bins);
  air time 0.30 ms; security envelope 56 % of the octets on the air.
- **Finding for the radio track, not fixed here:** on that run 23-46 % of resolved reception
  attempts were lost to collisions and the mean MAC deferral grew from 0.5 to 5.3 ms while
  each node occupied 0.3 % of the channel. Every node generates at the same engine tick and
  signs for exactly 9 ms, so every frame reaches the MAC in the same instant; the
  decomposition (`latency_stage[mac_defer]`, `collision_rate`) is what makes this visible.
- **Cost, measured:** with `metrics: [all]` the 100-vehicle Manhattan rung ran 23.6 s of
  engine time for 2 simulated seconds against 14.6 s with `metrics: [pdr]` (debug). Most of
  the difference is four providers each decoding every `node.rx` record from JSON.

## Correction, 2026-09-22 — the end-to-end run is a stub at the message layer

An earlier entry here and my report to the owner both described the Phase 1 run
as producing "970 signed messages". That was wrong, and an independent audit
caught it. Nothing is encoded and nothing is signed: the node returns a size from
a model and hands the engine a byte count. The layers below messaging are real
and verifiably deterministic; the messaging layer is a faithful size model with
no payload and no signature behind it.

Recorded rather than quietly fixed, because the reason it passed unnoticed is
instructive. Every number in the run report was plausible and self-consistent,
the recording verified, and the digest reproduced. What gave it away was one
comparison nobody had made: two different message formats came out at exactly the
same size. Full detail in `findings/slice-verification.md`.

## Verification standard used

Every crate was built, then adversarially validated by an agent told to re-derive
rather than review. That produced results worth recording, because in several
cases the independent derivation was the only thing that could have caught the
defect:

- The packet-error model was re-implemented from scratch in Python and agrees
  with the crate to 5e-10 dB across all 24 cells.
- The message encoder was checked against two independent implementations: a
  Python oracle compiled from the real standards modules, and an encoder the
  verifier wrote from the encoding rules. 235 vectors, both directions.
- Signature determinism was confirmed by reimplementing the relevant standard in
  Python and reproducing the exact 64 bytes.
- The cryptographic port was checked by re-running the legacy Python to capture
  fresh vectors rather than trusting committed ones.
- The wire specification's worked hex dumps were re-extracted from the
  specification text at run time and shown byte-identical to the checked-in
  fixture, so the golden test really is the specification's bytes.
- Timing fixes were mutation-verified: reintroducing each bug reproduced the
  original failure signature.

## Corrections made to the design during the build

The design is not treated as infallible. Where implementation disproved it, the
document was amended and the reason recorded:

- **ADR 0008's pose quantisation was impossible as written.** Int16 millimetres
  spans ±32.767 m and cannot address a square-kilometre world. Corrected to i32
  millimetre keyframes about a per-run origin with i16 millimetre deltas about
  the previously *transmitted* quantised value, which is also what stops error
  accumulating, plus an absolute escape for teleports.
- **FlatBuffers dropped for VWP v1** in favour of a flat fixed layout, so the
  recorder can store the exact bytes that went over the wire and live and replay
  are provably identical.
- **ADR 0004 gained an evidence section** from the legacy digest forensics, which
  produced build-decisions D9 and D10.
- **D11** arbitrates five places where the design and the implementation
  disagreed.
- **CI contradicted D1**, pinning Rust 1.86.0 against the file's 1.98.1. Fixed,
  with an assertion so they cannot drift again.
- **The staged Phase 0 cleanup would have deleted three files** that
  `01-inventory.md` §3.7 explicitly preserves. Rescued into `legacy/reference/`.
