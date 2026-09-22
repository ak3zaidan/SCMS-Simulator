# `tests/` — cross-crate suites

* **`conformance/`** — the conformance kit (`v2xw-conformance`, a workspace member): the
  VWP v1 wire checklist mechanised item by item, the plug-in conformance suite a contributor
  runs against their own model, the golden determinism harness CI's three-platform
  comparison shares, and the interface firewall generalised from `v2xw-node`'s sentinel.
  See `conformance/README.md`.
* **`fixtures/`** — committed input data. `midtown-6block.osm.xml` is the map the
  determinism gate imports on all three platforms (`just world-hash`).

Per-crate unit and integration tests live with their crate; the frozen Python conformance
vectors live in `legacy/tests/`.
