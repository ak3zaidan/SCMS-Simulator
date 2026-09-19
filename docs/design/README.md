# V2X World Simulator — design set

Status: design for review (2026-09-18). Nothing here is implemented; the roadmap starts only after review. Read `02-architecture.md`'s executive summary first (five minutes), then the ADRs, then the rest in order.

| Document | Content |
|---|---|
| `00-design-brief.md` | the brief this design answers |
| `01-inventory.md` | inventory and disposition of the existing repository; evolve-vs-replace decision with evidence |
| `02-architecture.md` | executive summary, components, data flow, time and determinism model, fidelity ladder and mixed tiers, plug-in system, engine-to-UI protocol, repository layout, performance evidence |
| `03-interfaces.md` | every plug-in interface with signatures, invariants, the model-card schema, the scenario schema outline, the event-log schema, the Python SDK, the conformance kit |
| `04-models.md` | model catalog: tiers, equations, parameters with cited defaults or `TODO: calibrate`, validation references |
| `05-protocols.md` | credential-management protocol interface and the SCMS, ETSI, and threshold/umbrella/PQ sketches with message sequences over the modeled network |
| `06-node-models.md` | OBU, RSU, backend, cellular models and the initial hardware profiles with sources |
| `07-threats-and-detection.md` | attacker model, attack catalog, detection and response pipeline, privacy metrics |
| `08-measurement-and-data.md` | metric catalog, experiments, exporters, MA-dataset compatibility and migration, world-data licensing, end-to-end traces of the canonical research questions |
| `09-ui.md` | 2D/3D viewer, HUD, inspector, editors, comparison, performance budget, replay, API parity |
| `10-roadmap.md` | phases with acceptance criteria, risk register, effort |
| `11-open-questions.md` | open questions grouped by impact, each with the assumption used |
| `appendix-a-spike-des-throughput.md` | the throwaway performance spike (evidence for ADR 0003 and 0011) |

ADRs in `../adr/`: 0001 (original, now partly superseded), 0002 base stack supersession, 0003 core language, 0004 time model and determinism, 0005 mobility provider, 0006 network fidelity source, 0007 plug-ins and model cards, 0008 recording and engine-to-UI protocol, 0009 UI stack, 0010 repository layout and build, 0011 performance targets.

Research fact sheets that back the citations (standards clauses, papers, datasheets, licences) are in `research/` (R1–R11, indexed in `research/README.md`) and are cited as `[R<n> §<section>]`; every default in `04-models.md`, `05-protocols.md` and `06-node-models.md` carries its source or a calibration plan.
