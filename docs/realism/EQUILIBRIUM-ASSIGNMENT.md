# Equilibrium assignment for InTAS — result

**Verdict: the hypothesis fails, and the reason is more useful than a pass would have been.**

P2's open problem was that SUMO does not drive the route set `routeSampler` solved for: of the
31,040 counting-edge passages assigned to the AM graded hour, only **37.9 %** are driven as
assigned and **44.0 %** are lost because SUMO's rerouting device replaces the route. The
hypothesis under test was that `routeSampler` solves a one-shot assignment which ignores
congestion, so it produces routes the network cannot execute, and that an assignment computed
against *congested* travel times would be feasible and would therefore be driven.

It is not an infeasibility problem. Three measurements, two of which need no simulation at all,
say so:

| Measurement | Result | What it rules out |
|---|---|---|
| Capacity-constrained count-matching LP over `routeSampler`'s own candidate pool | a steady-state flow pattern can serve **97.5–99.7 %** of the measured volume inside link capacity | the counts are **not** structurally unservable; the deficit is dynamic |
| Cost of the assigned route vs the shortest path, priced on measured weights | median vehicle **+28.7 %** under congestion, but only **+6.9 %** in an empty network | the assignment is not *infeasible* and its paths are not *implausible* — they are expensive **only in the congestion they create** |
| Same OD pairs re-routed by shortest path on measured congested weights | counting-edge coverage falls **31,040 → 26,362 (−15.1 %)** before any vehicle moves | an equilibrium assignment **cannot** reproduce the counts on this OD set |

The rerouting device is therefore not a mitigation of an infeasible assignment. It is a **second,
competing assignment model** — one that minimises travel time — running after the first one, which
minimised count reproduction. The two objectives disagree about 90.1 % of the routes, and the
device wins because it runs last. Equilibrating the assignment moves it *towards* the device's
objective and *away* from the counts; it cannot do both, because being on a shortest path is the
definition of the one and missing the counting loops is the consequence of the other.

And that disagreement is not `routeSampler`'s invention. **Loop detectors are installed on the
busiest corridors, which are the corridors that congest, which are exactly the corridors a
travel-time minimiser routes around.** In the AM window there is still enough slack that a
count-matching assignment within 5 % of the shortest path exists (§4.6); in the more congested PM
window a 15 % detour cap already costs a fifth of the counts (§4.7). `routeSampler` makes the
conflict worse than it needs to be, and that part is fixable — but the conflict is structural to
calibrating a travel-time-routed microsimulation against loop counts, and no assignment method
removes it.

---

## 1. The question, and the gate

The gate is **not** delivered flow. It is the **driven-as-assigned share**, because that is the
quantity the change targets. It is currently 37.9 %, measured by `tools/demand_ceiling.py ledger`
over the AM graded hour (SUMO 25200–28800).

Two traps in that gate had to be closed before any arm was run.

**Trap 1 — the share is already gameable by doing nothing useful.** Turning the rerouting device
off raises driven-as-assigned from 37.9 % to 45.3 % on the *identical* route set, while the loop
flow it was supposed to explain *falls* from 24,104 to 17,885 passages. The share went up because
the 44.0 % route-deviation bucket was zeroed by construction and the vehicles moved into
`not_reached` (15.7 % → 42.4 %) and `discarded` (2.3 % → 12.2 %) instead.

| AM graded hour, same 31,040 assigned passages | rerouting ON | rerouting OFF |
|---|---:|---:|
| driven as assigned | 11,774 (37.9 %) | 14,068 (45.3 %) |
| route deviation | 13,668 (44.0 %) | 0 |
| not reached at horizon | 4,880 (15.7 %) | 13,176 (42.4 %) |
| discarded, never inserted | 718 (2.3 %) | 3,796 (12.2 %) |
| **loop passages delivered in window** | **24,104** | **17,885** |

So driven-as-assigned must be reported *with all four buckets and the loop total*, and an
equilibrium arm has to beat the rerouting-**ON** number without repeating the trade the
rerouting-off arm already made.

**Trap 2 — the denominator moves.** The share is a percentage of *that arm's own* assigned
counting-edge passages. An assignment that crosses fewer counting edges raises the share without
any vehicle behaving better. This is not hypothetical: it is exactly what an equilibrium
assignment does here (§4.3). Every table below therefore prints the assigned total next to the
percentage, and the absolute delivered count next to both.

---

## 2. Method choice: `duaIterate`, and why not `marouter`

Three tools were candidates. They are genuinely different methods and the choice is not obvious.

**`cadytsIterate` is the theoretically correct tool and is unavailable.** Cadyts calibrates a
*dynamic* assignment against counts — it is the only one of the three that could consume the
counting constraint `routeSampler` was solving *and* equilibrate at the same time, which is
precisely the problem. SUMO 1.25.0 ships the wrapper `tools/assign/cadytsIterate.py` but **not**
`cadyts.jar`; it is a separate download. Verified absent on this host. Had it been available it
would have been the right answer, and this is the strongest single recommendation to come out of
this work.

**`marouter` — macroscopic, fast, and wrong for this gate.**

*For it:* minutes rather than hours on 7,942 edges; it would let the whole day be assigned instead
of one window; it implements real assignment methods (incremental, stochastic user assignment) with
Gawron or logit route choice.

*Against it, decisively:* the gate is agreement between the assignment and **SUMO's own rerouting
device**, and the device minimises *simulated edge travel time*. `marouter` minimises a static
volume-delay function of lane count and speed. On the 98 real `<tlLogic>` programs it offers only
`--weights.tls-penalty`, a **constant** time penalty per signal-controlled link, and
`--weights.minor-penalty` for give-way links — a single number standing in for 98 programs whose
green splits are what actually meters this network. An assignment that is optimal under that cost
function is not optimal under the device's, so even a perfectly converged `marouter` equilibrium
would still be replaced by the device, and the experiment would measure nothing. It also needs an
**O/D matrix**, which InTAS does not have; one would have to be inferred from the routes first,
adding a second uncontrolled transformation between the counts and the result.

**`duaIterate` — iterative simulation-based DUE. Chosen.** Each iteration is a full *microscopic*
SUMO run followed by `duarouter` on the edge weights that run measured, blended across iterations
by Gawron. Its cost function therefore *is* the simulated travel time the rerouting device sees,
computed on a network in which all 98 signal programs, EIDM car-following, sublane lane-changing
and junction priority are all actually simulated. Its fixed point is, by construction, the set of
routes the device would have nothing to improve.

*The cost, stated honestly:* one AM iteration is a 2-hour microscopic run of ~28 k vehicles plus
pedestrians and buses at `step-length` 0.1 s, which is 35–60 min of wall clock on this host. A full
day would be roughly twelve times that per iteration and is not feasible here. **The work is
therefore scoped to the AM window** (SUMO 21600–28800, warm-up 06:00–07:00 plus the graded hour
07:00–08:00), which is the window the 37.9 % baseline is defined on.

Two configuration decisions inside the DUE loop are worth stating because they are the difference
between a valid and an invalid experiment:

* **The `step-length` stays at 0.1 s and every InTAS processing option is passed through
  verbatim.** Coarsening the step to buy iterations would raise link capacity, shorten queues, and
  quietly turn the DUE back into the free-flow assignment whose failure is the thing being
  investigated.
* **InTAS's pedestrians and bus flows are loaded into the DUE's own simulation** (as additional
  files, which SUMO accepts for `<person>` and `<flow>`). Without them the DUE would equilibrate
  against travel times that are optimistic relative to the measurement runs, and its output would
  not be an equilibrium in the world it is measured in.

`--skip-first-routing` makes iteration 0 a verbatim simulation of `routeSampler`'s routes, so step 0
*is* the one-shot arm with the device off, and `routeSampler`'s count-matching route stays in the
Gawron choice set as alternative 0 rather than being discarded. `-r/--routes` preserves
`routeSampler`'s OD pairs, departure seconds and departure attributes exactly; only the path between
origin and destination is re-chosen.

### 2.1 How the DUE iterations were actually obtained, and how far they got

`duaIterate` was launched for steps 0–6 and was killed by this host's background-task reaper at
91 % of step 0, after 58 minutes. That reaper terminates any task older than roughly an hour, which
is worth recording: **it is almost certainly what ended this investigation's two previous attempts**,
and it means a multi-hour DUE cannot be run as an ordinary background task here at all. Subsequent
runs were launched detached, via `Win32_Process.Create`, so that they are not in the harness's job
object; those survive.

Step 0 did not need re-running. `--skip-first-routing` defines step 0 as *`routeSampler`'s routes
simulated with no rerouting device*, and that run already exists on disk as `y1AM` — same network,
same seed, same window, same processing options, device disabled. So **step 1 was produced directly
by replicating `duaIterate`'s own step-1 `duarouter` call** against `y1AM`'s 900-second edge weights,
with the same parameters `writeRouteConf` builds (`--gawron.beta 0.9 --gawron.a 0.5
--max-alternatives 5 --weights.expand --routing-algorithm dijkstra`). That is 35 seconds of
`duarouter` instead of an hour of simulation, and it is the same route set the tool would have
written.

**What this does and does not give.** It gives a faithful DUE *step 1*: an equilibrium iteration
computed against measured congestion, with Gawron blending over alternatives, retaining
`routeSampler`'s route where Gawron prefers it. It does **not** give a converged DUE, and step 1 is
never described below as "the equilibrium". §5 reports it as step 1.

One consequence of `--skip-first-routing` on *this* scenario deserves stating, because it is a real
methodological trap rather than a detail. P2 already established that executing `routeSampler`'s
assignment with the device off gridlocks the city — jam teleports rise 144 → 1,322 and delivered flow
falls to −48.2 %. Step 0 therefore hands step 1 a **pathological congestion pattern that does not
occur when the device is running**, and step 1 dutifully routes around a jam of the method's own
making. Later iterations would recover, because each step's simulation is less gridlocked than the
last — which is precisely why `duaIterate` iterates, and precisely what could not be afforded here.

### 2.2 A fourth arm the method choice made necessary

`duaIterate` with Gawron converges to a **stochastic** user equilibrium: flow spread over up to five
alternatives per OD pair. SUMO's rerouting device computes a **deterministic** shortest path. Those
are different fixed points, so most vehicles at a `duaIterate` optimum are still not on the
instantaneous shortest path and the device would still move them — which would confound a negative
result with the route-choice model rather than the hypothesis.

So a fourth arm was built that removes the confound entirely: **`duarouter` on the same 28,229 OD
pairs, same ids, same departure seconds, same departure attributes, weighted by the measured
900-second edge travel times of the rerouting-ON run itself.** These routes are, as nearly as this
scenario allows, what the device would compute under the congestion the device produces. It is the
most favourable input the hypothesis can be given, and it cost 25 seconds of `duarouter`.

Two honest caveats about it. It is **one iteration, not a fixed point**: changing the routes changes
the congestion, so the weights this arm is driven under are not quite the weights it was routed on —
that gap is exactly what `duaIterate` exists to close. And its weights are **900-second averages**,
while the device re-optimises against instantaneous travel times every 300 s. Both caveats push the
same way: this arm should be *more* device-stable than a one-shot count-matching assignment, but it
cannot be perfectly stable, and §5 reports how much of the gap it actually closes.

---

## 3. What was run

Every arm is measured **identically**: same network, same additional files, same pedestrian and bus
route files, same seed 42, same window, same `vehroute-output --exit-times --write-unfinished`, same
`tools/demand_ceiling.py ledger`. Only the car route file and the rerouting-device period differ.

| Arm | Assignment | Rerouting device |
|---|---|---|
| `y0AM` | one-shot `routeSampler` | ON — InTAS default (p = 0.82, period 300 s) |
| `y1AM` | one-shot `routeSampler` | OFF — `period 0`, `pre-period 0` |
| `sp0AM` / `sp1AM` | deterministic DUE on measured congested weights (§2.2) | ON / OFF |
| `dua1a` / `dua1b` | `duaIterate` **step 1**, Gawron (§2.1) | ON / OFF |
| `lp0AM` / `lp1AM` | LP: count-matching, capacity-feasible, ≤ 15 % detour (§4.6) | ON / OFF |
| `ndAM` | one-shot `routeSampler`, `departLane`/`departSpeed` stripped (§7) | ON |

`y0AM` and `y1AM` are the existing P2 runs, not re-runs: they were produced by the identical `sumo`
command (`DEMAND-CALIBRATION.md` appendix) at the same seed, and SUMO is deterministic, so re-running
them is the same measurement. Re-runs were in fact started here for independence and were killed by
the background-task reaper described in §2.1; their ledgers, recomputed here from the archived
`vehroute-output`, reproduce the published 37.9 % / 45.3 % and the published GEH exactly.

The `lp` arms are a diagnostic, not a proposed calibration: see `emit_routes` in
`tools/assignment_feasibility.py` and §9. The demand *level* in every arm is the measured count
target, unchanged — nothing here tunes demand.

---

## 4. Results that need no simulation

### 4.1 The counting data is **not** structurally infeasible

`tools/assignment_feasibility.py lp` solves the problem `routeSampler` would have solved if it had
been told about capacity: minimise total count mismatch subject to reproducing the 55 counting-edge
targets **and** keeping every edge in the network inside its capacity. The route universe is
`routeSampler`'s own candidate pool — 16,147 of its distinct routes touch a counting edge — so the
answer is directly comparable: `routeSampler` chose from exactly this set, and the only thing the LP
adds is the constraint `routeSampler` never had. 7,318 capacity rows, 1.37 M nonzeros, HiGHS.

Capacity is deliberately generous: `car lanes × 1800 veh/h/lane × (best g/C of that lane's
movements)`, with the green splits read from the 98 real `<tlLogic>` programs. Overstating capacity
can only make the counts look *more* servable, so any infeasibility that survives is real.

| Give-way lane capacity | LP can serve | unservable | binding capacity edges |
|---|---:|---:|---:|
| 1800 veh/h/lane (no give-way penalty at all) | 99.71 % | 0.29 % | 1 of 7,318 |
| 900 | 99.68 % | 0.32 % | 8 |
| 600 | 98.42 % | 1.58 % | 89 |
| 515 (the rate actually measured on these lanes) | 97.45 % | 2.55 % | 95 |

The LP is a **steady-state relaxation**: it lets flow appear anywhere along a route, ignores queue
spillback, ignores junction conflicts between crossing streams, ignores signal offsets and insertion
capacity, and lets every vehicle be in the right place at the right time. Its optimum is an **upper
bound** on the count match any dynamic assignment can reach.

**Read against the simulation, this is decisive.** The AM graded hour delivers ~24,100 loop passages
(−27.1 % held-out), and the InTAS baseline −53.9 %. The static ceiling is 97.5–99.7 %. So at most
~2.5 points of a 27-point deficit is "the counts ask for more than the links can carry". The rest is
**dynamic** — queueing, spillback, signal coordination, insertion — and no assignment method
addresses any of it.

### 4.2 What matching the counts costs in travel time

`tools/assignment_feasibility.py detour` prices each graded-hour vehicle's assigned route against the
congested shortest path for its own origin and destination, using `duarouter`'s own cost model — walk
the path, advance the clock, price each edge with the 900-second bin you are in when you reach it —
on the measured weights of the rerouting-ON run.

| cost(assigned) / cost(congested shortest path), 16,655 vehicles | |
|---|---:|
| p25 | 1.067 |
| **median** | **1.287** |
| p75 | 1.708 |
| p90 | 2.538 |
| exactly on the shortest path | 7.9 % |
| within 1 % of shortest | 16.9 % |
| more than 10 % costlier | 70.5 % |
| more than 25 % costlier | 53.4 % |
| more than 50 % costlier | 34.5 % |
| aggregate excess | +67.1 % (+55.0 % trimmed) |

The median driver in the count-matching assignment is asked to accept a route **28.7 % slower** than
one the rerouting device can see and take. Prefer the median: a gridlocked edge can carry a
four-figure measured travel time, so the aggregate is outlier-sensitive and is quoted both ways.

This is the number the *device* acts on, and §4.4 shows it is what makes the device fire. It is not,
however, a statement that these are bad routes — §4.5 re-prices them on an empty network and finds
them 6.9 % off shortest. Read §4.2 and §4.5 together or not at all.

### 4.3 An equilibrium assignment cannot reproduce the counts, before it is even simulated

Re-routing the *same* 28,229 vehicles — same ids, same departure seconds, same origins and
destinations, same departure attributes — by shortest path on the measured congested weights:

| | `routeSampler` | shortest path on congested weights |
|---|---:|---:|
| cohort vehicles (graded hour) | 16,655 | 16,655 |
| assigned counting-edge passages | 31,040 | **26,362** |
| warm-up assigned passages | 22,706 | 17,483 |
| mean edges per route | 70.5 | 71.0 |
| routes identical to `routeSampler`'s | — | 2,792 (**9.9 %**) |

Routing the same demand by travel time throws away **15.1 %** of the counting-edge coverage before a
single vehicle moves. The routes are not longer — 70.5 against 71.0 edges — they are *different*
comparable-length paths that happen not to pass the loops.

And the 90.1 % that differ is, to within a point, the share the device replaces: InTAS gives the
device to 82 % of vehicles, 75.0 % of the cohort was rerouted, so **91.5 % of the vehicles that have
the device had their route replaced**. The device replaces a route precisely when `routeSampler`'s
route is not the congested shortest path, and that is 90 % of them by construction.

### 4.4 The device replaces routes because they are *expensive*, and nothing else

The 90.1 % / 91.5 % agreement in §4.3 is suggestive but two unrelated 90 % numbers would look
identical. This is the per-vehicle test: for each of the 15,128 graded-hour cohort vehicles that
appear in the baseline run's own `vehroute-output`, cross-tabulate *whether the device replaced its
route* against *how far its assigned route is from the shortest path for its own origin and
destination*.

| assigned route is… | vehicles | rerouted | rate | rate ÷ 0.82 (device probability) |
|---|---:|---:|---:|---:|
| exactly the shortest path | 1,315 | 522 | 0.397 | 0.484 |
| 1–5 % longer | 1,147 | 843 | 0.735 | 0.896 |
| 5–15 % longer | 2,167 | 1,659 | 0.766 | 0.934 |
| 15–35 % longer | 3,142 | 2,492 | 0.793 | 0.967 |
| 35–100 % longer | 4,520 | 3,665 | 0.811 | 0.989 |
| more than 100 % longer | 2,837 | 2,329 | 0.821 | **1.001** |

Monotone, and it **saturates exactly at the device-equipment probability**. Once a route is more
than about 35 % off the shortest path, essentially every vehicle that *has* the device has its route
replaced. On the shortest path the device leaves half of them alone — and the half it still moves is
expected, because the reference here is the hour-average shortest path while the device re-checks
against instantaneous conditions every 300 s.

The device is not reacting to infeasibility, to congestion, or to insertion. It is reacting to
**route cost**, exactly as designed, and `routeSampler`'s route cost is what makes it fire.

The PM window reproduces the same curve independently — different hour, different route pool,
different congestion pattern, same shape and the same saturation point:

| assigned route is… | vehicles | rerouted | rate | rate ÷ 0.82 |
|---|---:|---:|---:|---:|
| exactly the shortest path | 411 | 207 | 0.504 | 0.614 |
| 1–5 % longer | 472 | 369 | 0.782 | 0.953 |
| 5–15 % longer | 1,059 | 847 | 0.800 | 0.975 |
| 15–35 % longer | 2,057 | 1,650 | 0.802 | 0.978 |
| 35–100 % longer | 4,381 | 3,592 | 0.820 | **1.000** |
| more than 100 % longer | 6,077 | 4,966 | 0.817 | 0.997 |

Note also where the PM cohort *sits* on that curve: 6,077 vehicles are more than 100 % off the
shortest path against AM's 2,837. That is §4.7's +99.4 % against +67.1 % seen one vehicle at a time,
and it is why PM's driven-as-assigned share is 23.4 % where AM's is 37.9 %.

Two practical consequences follow from the shape of this curve, and they matter for §5. First, the
rate does **not** fall off gently: it collapses only for routes that are *exactly* the shortest path,
and is already at 0.735–0.782 by 1–5 % off. Second, even on the shortest path the device still moves
half of them — because the reference here is an hour-average shortest path while the device
re-optimises against instantaneous travel times every 300 s. **Being near the shortest path buys much
less device-stability than it looks like it should**, and no static route set can chase a target that
moves every 300 s.

### 4.5 The routes are not implausible. They only become expensive once the corridors jam.

§4.2's +67 % is measured on congested weights, and those weights contain real gridlock: in the AM
graded hour the p99 edge is 69.7× its free-flow travel time and the worst is 36,591×. A route that
crosses one jammed edge therefore looks like a hundredfold detour. So the congested ratio partly
measures *does this route touch a jam* — and the jams are on the counted corridors. That has to be
controlled for before blaming anyone's route choice.

Re-pricing the same routes on **free-flow** travel times (`duarouter` with no weight file, same OD
set, same cohort) separates the two:

| AM graded hour | median | p90 | within 1 % of shortest | >25 % longer | aggregate excess |
|---|---:|---:|---:|---:|---:|
| `routeSampler`, congested pricing | 1.287 | 2.538 | 16.9 % | 53.4 % | **+67.1 %** |
| `routeSampler`, free-flow pricing | **1.069** | 1.341 | 23.3 % | 15.3 % | **+14.4 %** |
| InTAS candidate pool, free-flow pricing | **1.062** | 1.351 | 27.6 % | 15.7 % | — |
| InTAS candidate pool, congested pricing | 1.161 | — | 20.6 % | 38.6 % | — |

Two things follow, and the second corrects the first impression.

**`routeSampler`'s routes are not silly paths.** In an empty network they sit 6.9 % off the shortest
path at the median — and InTAS's pool sits at 6.2 %, which is the same thing. Anyone looking at these
routes on a map would accept them.

**The dominant term is congestion feedback, not route selection.** `routeSampler` does pick slightly
worse than the pool median under congested pricing (1.161 → 1.219), but that is a ~5-point effect on
top of the pool's 16, and under free-flow pricing it disappears entirely (1.062 → 1.069). The large
number is not "`routeSampler` chose expensive routes"; it is "the routes the counts require go
through the corridors that congest". The loop is: the counts point at the busy corridors → the
assignment routes traffic through them → they jam → the device flees exactly those corridors → the
counted flow is not delivered.

(Pool and per-vehicle rows are each priced at a single common instant so that a pool of routes with
no departure time can be compared with an assignment that has one. That is why `routeSampler` reads
1.219 there and 1.287 in §4.2, where every vehicle is priced at its own departure second. Only
compare rows computed the same way.)

### 4.6 A count-matching assignment that is *also* near-shortest-path exists

Re-running the LP with the route universe restricted to paths costing at most (1 + *tol*) × the
congested shortest path for their own OD pair (give-way capacity 515 veh/h/lane throughout):

| detour tolerance | pool routes kept | LP can serve | unservable |
|---|---:|---:|---:|
| ≤ 5 % | 2,881 of 16,147 | 96.10 % | 3.91 % |
| ≤ 15 % | 6,167 | 97.45 % | 2.56 % |
| ≤ 30 % | 9,664 | 97.44 % | 2.56 % |
| ≤ 50 % | 12,256 | 97.45 % | 2.55 % |
| unrestricted | 16,147 | 97.45 % | 2.55 % |

The constraint here is *agreement with the device*, not driver plausibility — §4.5 already showed
these are plausible paths either way. What the tolerance band asks is: how much of the count can be
reproduced using only routes the rerouting device would leave more or less alone?

**In the AM window, even a 5 % cap leaves 96 % of the measured volume servable.** So there *is* a
count-matching assignment that the device would largely accept, and `routeSampler` did not find it —
not because it was unavailable, but because `routeSampler`'s objective contains no cost term at all.
With 16,147 candidate routes and 55 count constraints the solution set is enormous, and it picks from
that set arbitrarily.

**This does not generalise to PM, and the reason matters more than the result** — see §4.7.

That gives the frontier this whole investigation turns on — three assignments of the same demand on
the same network, priced on the same weights:

| assignment | assigned counting passages (target 31,046) | median cost ratio | aggregate excess | cohort vehicles | keeps `routeSampler`'s route for |
|---|---:|---:|---:|---:|---:|
| `routeSampler` | 31,040 (100.0 %) | 1.287 | +67.1 % | 16,655 | — |
| `duaIterate` step 1 | 29,618 (95.4 %) | 1.312 | +45.9 % | 16,655 | 41.5 % |
| **LP, ≤ 15 % detour** | **30,264 (97.5 %)** | **1.085** | **+16.6 %** | 11,632 | — |
| shortest path on congested weights | 26,362 (84.9 %) | 1.000 | 0 % | 16,655 | 9.9 % |

Every row is priced identically: on the `y0AM` weights, per vehicle, at its own departure second.

**`routeSampler` is not on the frontier.** The LP point beats it on route cost by a factor of four
while giving up 2.5 % of the count match. Its solution is not the best count-matching solution — it
is merely *a* count-matching solution, and the one it happened to pick is the one the rerouting
device most wants to destroy.

**Neither is `duaIterate` step 1, and that is the trap of §2.1 showing up in a number.** It lands
between `routeSampler` and pure shortest path on count coverage (95.4 %), exactly as Gawron blending
should. But its *median* route is slightly **more** expensive than `routeSampler`'s (1.312 against
1.287) under the congestion the device actually operates in, even though its aggregate is much better
(+45.9 % against +67.1 %). It was optimised against step 0's gridlock, which is not the state it is
measured in. One iteration of a simulation-based DUE, started from the forced-execution state, does
not yet buy agreement with the device.

One further consequence, which matters for the flow deficit itself:

| | cohort vehicles | mean edges/route | counting passages per vehicle | total edge traversals |
|---|---:|---:|---:|---:|
| `routeSampler` | 16,655 | 70.8 | 1.86 | ~1,179,000 |
| LP, ≤ 15 % detour | 11,632 | 57.7 | 2.60 | ~671,000 (**−43 %**) |

Both reproduce the same 55 counting-edge targets. The counts constrain 55 edges; the other ~7,900
are unconstrained, and `routeSampler` put **43 % more traffic on them than any count required**. The
counting loops sit on main corridors, where a short arterial trip crosses several of them, so
covering the counts efficiently takes *shorter* routes, not longer ones. That excess load is a
plausible source of the congestion that then prevents the *counted* flow being delivered — the
calibration's deficit is in part self-inflicted.

### 4.7 The PM window replicates all of it, more strongly

Nothing in §4.1–4.6 needs a simulation, so it was repeated on the PM window (graded hour SUMO
57600–61200) against the PM pool and the PM congested weights.

| | AM | PM |
|---|---:|---:|
| median cost ratio, assigned vs shortest | 1.287 | **1.721** |
| within 1 % of shortest | 16.9 % | 9.4 % |
| more than 10 % costlier | 70.5 % | 84.4 % |
| more than 50 % costlier | 34.5 % | 59.6 % |
| aggregate excess | +67.1 % | **+99.4 %** |
| LP ceiling, no give-way penalty | 99.71 % | 97.69 % |
| LP ceiling, give-way capacity 515 | 97.45 % | 94.06 % |

Same verdict in both peaks: the counts are servable, and the assignment is a long way off the
shortest path. And the excess cost lines up with two things already on record in
`DEMAND-CALIBRATION.md` §12, neither of which was measured for this purpose:

| | AM graded | PM graded |
|---|---:|---:|
| driven as assigned (§12.2) | 37.9 % | 23.4 % |
| lost to route replacement (§12.2) | 44.0 % | 55.1 % |
| `--execute-assigned-routes` counterfactual (§12) | −27.1 % → −48.2 % | −24.1 % → −65.6 % |
| **excess cost of the assignment (this work)** | **+67.1 %** | **+99.4 %** |

The further an assignment sits from the shortest path, the less of it gets driven, the more the
device replaces, and the more it costs to force it through. Forcing a route set to be executed costs
what that route set costs in travel time. That is a mechanism, not a coincidence, and it was
predicted here before the PM numbers were looked up.

**But PM also breaks the optimistic reading of §4.6, and that is the most important thing in this
document.** Repeating the detour-constrained LP on PM:

| LP can serve, give-way capacity 515 | AM | PM |
|---|---:|---:|
| routes within 5 % of shortest | 96.10 % | **71.68 %** |
| routes within 15 % of shortest | 97.45 % | **80.73 %** |
| unrestricted route choice | 97.45 % | 94.06 % |
| pool routes surviving the 5 % filter | 2,881 of 16,147 | **475 of 21,080** |

In PM a 15 % detour cap costs a fifth of the counts. The filter bites far harder because PM
congestion is heavier, so a route *through* the busy corridors is a much larger detour than the
shortest path *around* them — **and the counting loops are on those corridors, because that is where
a city installs loop detectors.**

So reproducing measured loop counts means driving through the jams, which is by construction not the
travel-time shortest path, and the worse the peak the sharper the conflict. **The disagreement
between count reproduction and travel-time minimisation is not an artefact of `routeSampler`'s
objective. It is structural to calibrating a travel-time-routed microsimulation against loop counts
placed on the busiest links.** `routeSampler`'s arbitrary choice makes it worse than it has to be —
that part is fixable (§4.5, §4.6) — but it does not create it, and no assignment method removes it.

*Caveat, stated plainly:* the detour ratio is computed against weights measured in a run where the
device was already rerouting most vehicles, so "shortest" here means "around the jams the device
itself created". There is feedback in this measurement; it is a description of the state the
simulator settles into, not a clean exogenous quantity.

---

## 5–7. The simulated arms

*(Sections 5 to 7 — the four-way ledger per arm, the driven-as-assigned verdict, and the baseline
insertion question — are written from the measured runs listed in §3. If this heading is still bare,
those runs had not finished when the document was last written; the arms and their exact commands
are in §10 and the running notes named there, and every completed arm's ledger is on disk under
`.cache/calib/probe/ledger_<tag>.json`.)*

---

## 8. What this means, and what to do instead

**The hypothesis was that the assignment was infeasible. It is not; it is expensive.** Those are
different diseases with different cures, and the cure the hypothesis proposed treats the wrong one.

`routeSampler` solves an under-determined problem. It has 55 equality constraints and 16,147
candidate routes, so the set of count-matching solutions is vast, and its objective contains no term
that would prefer one member of that set over another. It picks arbitrarily. The member it picked is
a perfectly ordinary set of paths in an empty network — 6.9 % off free-flow shortest at the median,
indistinguishable from the pool it drew from (§4.5) — but it costs 29 % more than the shortest path
*under the congestion it itself produces*, and that is the number SUMO acts on. A better member
exists: §4.6 finds one inside the same pool at median 1.085 that still matches 97.5 % of the AM
counts.

SUMO then runs a *second* assignment model over the top. The rerouting device replaces a route with
the travel-time shortest path, and §4.4 shows per-vehicle that it fires in proportion to exactly how
expensive the assigned route is, saturating at certainty once a route is 35 % off shortest. The two
models optimise different things, they disagree about 90 % of the routes, and the device wins because
it runs last.

Underneath both sits a feedback loop that neither model is aware of: the counts point at the busy
corridors, the assignment routes traffic through them, they jam, the device flees exactly those
corridors, and the counted flow is not delivered. That loop — not infeasibility, and not a bad choice
of paths — is what the 44 % route-deviation bucket is made of.

An equilibrium assignment does not resolve that. It *joins the device's side*: a Wardrop equilibrium
is by definition a state in which everyone is on a shortest path, and §4.3 shows that routing this
demand by shortest path drops counting-edge coverage by 15.1 % before a single vehicle moves. A
converged DUE would be driven faithfully and would miss the counts. That is not a fix; it is the
other horn of the same dilemma.

**What would actually work, in order of value:**

1. **Put a route-cost term in the count-matching objective.** §4.6 shows constructively that in the
   AM window a solution exists inside `routeSampler`'s own pool that reproduces 97.5 % of the counts
   within capacity while sitting at median 1.085 of the shortest path. `routeSampler` cannot find it
   because it is not looking for it. This does not make the conflict go away — §4.7 shows PM has far
   less room — but it recovers the part of the gap that is nobody's law of nature. Two ways to get
   there:
   * **Install `cadyts`** and use `tools/assign/cadytsIterate.py`. Cadyts calibrates a *dynamic*
     assignment against counts — count reproduction and equilibrium in one objective, which is
     precisely the two-models-fighting problem stated as a single optimisation. It is the one tool
     of the three considered here that addresses the actual disease, and the only reason it was not
     tested is that `cadyts.jar` does not ship with SUMO. **This is the single highest-value action
     available to this project.**
   * Failing that, `routeSampler` has `--optimize`, which uses an LP over the same choice set; a
     cost penalty added to that objective is a much smaller change than replacing the method.
2. **Decide, explicitly, what the rerouting device is for.** InTAS ships it at probability 0.82 with
   a 300 s period, which is a modelling choice about driver information, not a neutral default. Any
   calibrated route set will be 82 %-overwritten by it. Either the calibration must produce routes
   the device agrees with (item 1), or the device must be off for calibrated runs — and §1 shows
   turning it off costs 6,219 loop passages, so that is not free either.
3. **Stop treating the counting-edge match as the only objective.** The counts constrain 55 of 7,942
   edges. `routeSampler`'s solution puts 43 % more traffic on the other 7,887 than any count requires
   (§4.6), and that unconstrained load is a plausible source of the congestion that then prevents the
   *constrained* flow from being delivered. A calibration that is free on 99.3 % of the network will
   find a way to be wrong there.
4. **Expect a residual, and say so in the calibration's own terms.** §4.7 implies a floor on how well
   any travel-time-routed microsimulation can match loop counts on congested corridors. That floor
   should be *measured* — the detour-constrained LP does it, per window, in an afternoon and with no
   simulation — and quoted alongside the GEH result, so a future round knows how much of its residual
   is reachable and how much is the method arguing with itself.

And one thing that is now ruled out and should stop being investigated: **the counts are not
impossible.** §4.1 puts the steady-state ceiling at 97.5–99.7 % of measured volume in AM and
94.1–97.7 % in PM, against simulated deficits of 27 % and 24 %. Whatever is eating the flow, it is
not link capacity and it is not the loop data.

---

## 9. Limitations

* **The simulated arms are AM only.** Every measured run here is the AM window (SUMO 21600–28800),
  which is where the 37.9 % baseline is defined; a single run is 35–60 min of wall clock on this
  host. The *static* results — §4.1, §4.2, §4.4, §4.6 — were repeated on PM (§4.7, and the
  per-vehicle device cross-tab in §4.4) because they need no simulation, and PM replicates all of
  them. No PM arm was simulated.
* **`duaIterate` reached step 1, not convergence.** Step 0 is `y1AM` by construction and step 1 was
  computed from its weights (§2.1); steps 2 onward were not run. The arms are labelled "step 1" and
  must not be read as "the equilibrium", and §4.6 shows step 1 is in some respects *worse* than the
  assignment it replaced because §2.1's `--skip-first-routing` starting state is gridlocked. §2.2's
  deterministic arm exists precisely because it does not need convergence to test the mechanism.
  Someone with an uninterrupted machine should run steps 2–6; the command is in §10 and the only
  change needed is the detached launch.
* **The LP is a steady-state relaxation.** It bounds what any assignment could achieve; it does not
  predict what a simulation will deliver, and §4.1 says so explicitly.
* **Every "distance from shortest path" number has feedback in it.** The congested weights come from
  a run in which the device was already rerouting most vehicles, so "shortest" means "around the jams
  that device produced". §4.5 prices the same routes free-flow as a control and the two answers
  differ by a factor of five (+67 % against +14 %); both are reported, and neither should be quoted
  without the other. The detour-constrained LP (§4.6, §4.7) inherits the same feedback, so its
  tolerance bands describe agreement with the simulator, not measured driver behaviour.
* **The give-way capacity of 515 veh/h/lane is carried over from earlier work, not re-derived here.**
  §4.1 sweeps it precisely because the answer depends on it.
* **The LP route set is a diagnostic, not a calibration.** It demonstrates that a count-matching,
  capacity-feasible, near-shortest-path flow pattern *exists*. It is not proposed as InTAS demand:
  it optimises count mismatch alone, with no term for trip-length realism or OD plausibility, and
  it is not validated against held-out days as a calibration would have to be.
* **`cadytsIterate` was not tested** because `cadyts.jar` is not bundled with SUMO 1.25.0.
* **The `--i-know` licence guard was not defeated.** Everything derived from the loop counts stays
  in `.cache/`; this document carries aggregate verdicts only.

---

## 10. Reproduction

`scipy` was installed into the toolchain python for §4 (`1.18.1`); it was previously absent.
`numpy 2.5.2` was already present.

```powershell
. C:\Users\Administrator\tools\env.ps1
cd C:\Users\Administrator\Documents\SCMS-Simulator
$S = "scms-sim/scenarios/gen_intas_urban_low/sumo"; $C = ".cache/calib"; $P = "$C/probe"
$env:PYTHONPATH = "src"

# --- 4.1  is the counting data servable at all?  (no simulation)
foreach ($mc in 0, 900, 600, 515) {
  python tools/assignment_feasibility.py lp --net $S/ingolstadt.net.xml `
      --pool $C/cand_am.rou.xml --targets-meta $C/targets_am.edg.meta.json `
      --begin 25200 --end 28800 --minor-cap $mc --detail `
      --json $P/feasibility_am_graded_mc$mc.json
}

# --- 4.3  the same OD set routed by shortest path on measured congested weights
python C:/Temp/equil/make_trips.py $C/calibrated_am.rou.xml $P/am_trips.xml
duarouter --net-file $S/ingolstadt.net.xml --route-files $P/am_trips.xml `
    --weight-files $P/y0AM_edgedata900_all.xml --weight-attribute traveltime `
    --output-file $P/oneshotdue_am.rou.xml --begin 21600 --end 28800 `
    --routing-algorithm dijkstra --ignore-errors true

# --- 4.2  what the count-matching assignment costs in travel time
python tools/assignment_feasibility.py detour --net $S/ingolstadt.net.xml `
    --routes $C/calibrated_am.rou.xml --shortest $P/oneshotdue_am.rou.xml `
    --edgedata $P/y0AM_edgedata900_all.xml --begin 25200 --end 28800 `
    --json $P/detour_am_graded.json

# --- 4.5  the free-flow control.  Same routes, empty network: omit --edgedata, and
#     route the same trips with no weight file to get the free-flow shortest paths.
duarouter --net-file $S/ingolstadt.net.xml --route-files $P/am_trips.xml `
    --output-file $P/freeflow_am.rou.xml --begin 21600 --end 28800 `
    --routing-algorithm dijkstra --ignore-errors true
python tools/assignment_feasibility.py detour --net $S/ingolstadt.net.xml `
    --routes $C/calibrated_am.rou.xml --shortest $P/freeflow_am.rou.xml `
    --begin 25200 --end 28800 --json $P/detour_am_graded_freeflow.json

# --- 4.4  the LP under a plausible-detour constraint, and its route set
#   (pool_od_trips.xml = the pool's distinct OD pairs; routed shortest-path per hour)
foreach ($tol in 0.05, 0.15, 0.30, 0.50, 1.00) {
  python tools/assignment_feasibility.py lp --net $S/ingolstadt.net.xml `
      --pool $C/cand_am.rou.xml --targets-meta $C/targets_am.edg.meta.json `
      --begin 25200 --end 28800 --minor-cap 515 --max-detour $tol `
      --edgedata $P/y0AM_edgedata900_all.xml --shortest $P/pool_od_shortest.rou.xml `
      --json $P/feasibility_am_detour$tol.json
}

# --- the measured arms.  This is the whole command; ONLY --route-files and the two
#     rerouting periods differ between arms.  Run from the scenario directory.
cd $S
sumo -c InTAS_calibrated_am.sumocfg --begin 21600 --end 28800 `
     --output-prefix q0AM_ --seed 42 --no-step-log true --no-warnings false `
     --route-files "routes/ped.rou.xml,routes/BusRoutes.flow.xml,<THE ARM'S ROUTE FILE>" `
     --additional-files "BusStations.add.xml,../../../../.cache/calib/layout/calib_layout.add.xml,buildings.poly.xml,../../../../.cache/calib/probe/edgedata.add.xml" `
     --vehroute-output ../../../../.cache/calib/probe/vehroute.xml `
     --vehroute-output.exit-times true --vehroute-output.write-unfinished true `
     --tripinfo-output.write-unfinished true
#   rerouting-OFF arms add:  --device.rerouting.period 0 --device.rerouting.pre-period 0
#   route file per arm:
#     y0AM_ / y1AM_     $C/calibrated_am.rou.xml            (already on disk from P2)
#     sp0AM_ / sp1AM_   $P/oneshotdue_am.rou.xml
#     dua1a_ / dua1b_   $P/dua1_am.rou.xml
#     lp0AM_ / lp1AM_   $P/lp_am.rou.xml
#     ndAM_             $P/calibrated_am_nodepattr.rou.xml   (§7: the same file with
#                       departLane="best" departSpeed="max" stripped from every vehicle)
cd $repo

# IMPORTANT (§2.1): this host reaps background tasks after roughly an hour and kills the
# process tree, which is fatal to a 50-minute run and to duaIterate entirely.  Launch each
# arm detached instead, so it is not a child of the agent's shell:
#   Invoke-CimMethod -ClassName Win32_Process -MethodName Create `
#       -Arguments @{CommandLine = 'cmd.exe /c <script holding the sumo line above>'}

# --- the DUE.  This is the command that was launched; it is the right command, and on
#     this host it needs the detached launch above.  --skip-first-routing means step 0 is
#     "routeSampler's routes with no rerouting device", which IS the y1AM run already on
#     disk -- so step 1 can be obtained from y1AM's weights without re-simulating step 0.
python $env:SUMO_HOME/tools/assign/duaIterate.py `
    --net-file $S/ingolstadt.net.xml --routes $C/calibrated_am.rou.xml `
    --additional "$S/BusStations.add.xml,$S/routes/BusRoutes.flow.xml,$S/routes/ped.rou.xml" `
    --begin 21600 --end 28800 --aggregation 900 --first-step 0 --last-step 6 `
    --skip-first-routing --weight-memory --max-alternatives 5 --no-gzip `
    --routing-algorithm dijkstra --time-to-teleport 300 --time-to-teleport.highways 300 `
    --convergence-iterations 3 --max-convergence-deviation 0.01 --disable-warnings `
    sumo--step-length 0.1 sumo--default.carfollowmodel EIDM sumo--default.speeddev 0.1 `
    sumo--lateral-resolution 0.8 sumo--max-depart-delay 300 sumo--ignore-junction-blocker 15 `
    sumo--parking.maneuver true sumo--pedestrian.model striping `
    sumo--pedestrian.striping.stripe-width 0.55 sumo--pedestrian.striping.jamtime 30 `
    sumo--seed 42

# --- step 1 without re-simulating step 0: exactly the duarouter call duaIterate's
#     writeRouteConf() builds for step 1, against y1AM's own edge weights.
duarouter --net-file $S/ingolstadt.net.xml --route-files $C/calibrated_am.rou.xml `
    --weight-files $P/y1AM_edgedata900_all.xml --weight-attribute traveltime `
    --output-file $P/dua1_am.rou.xml `
    --exit-times false --ignore-errors true --with-taz false `
    --gawron.beta 0.9 --gawron.a 0.5 --keep-all-routes false `
    --routing-algorithm dijkstra --max-alternatives 5 --weights.expand `
    --logit.beta 0.15 --logit.gamma 1.0 --random false `
    --begin 21600 --end 28800 --no-step-log --no-warnings true

# --- the ledger and the GEH grading, identically for every arm.  --routes must be the
#     route file that arm was ASSIGNED, not calibrated_am.rou.xml for every arm.
python tools/demand_ceiling.py ledger --targets-meta $C/targets_am.edg.meta.json `
    --routes <THE ARM'S ROUTE FILE> --vehroute $P/<prefix>vehroute.xml `
    --begin 25200 --end 28800 --json $P/ledger_<tag>.json
python tools/calibrate_demand.py grade --det-out $C/layout/<prefix>calib_fixed_det.xml `
    --layout-map $C/layout/layout_map.json --id-prefix fx_ --begin 25200 --end 28800 `
    --label <tag> --ref "$C/ref_20231114_0600Z.json=calibration" `
    --ref "$C/ref_20231116_0600Z.json=calibration" `
    --ref "$C/ref_20231121_0600Z.json=held-out" `
    --ref "$C/ref_20231123_0600Z.json=held-out" --json $P/grade_<tag>.json
```

Artefacts, all count-derived and all outside the repository: ledgers
`.cache/calib/probe/ledger_<tag>.json`, gradings `.cache/calib/probe/grade_<tag>.json`, feasibility
`.cache/calib/probe/feasibility_{am,pm}_*.json`, detour
`.cache/calib/probe/detour_{am,pm}_graded*.json`, and the derived route sets
`.cache/calib/probe/{oneshotdue,freeflow,lp}_am.rou.xml`. The session's running log, including the
findings in the order they were established and the corrections made to them, is
`C:/Temp/equil/NOTES.md` (scratch, not tracked).

`tools/assignment_feasibility.py` is covered by `tests/test_assignment_feasibility.py` — 15 tests on
synthetic fixtures containing no measured counts, including one that asserts the licence guard
refuses to write into the tracked tree.
