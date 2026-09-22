# `v2xw-py`

The native half of the `v2xw` Python package. The Python half is in `python/v2xw/`; the
package documentation is `python/README.md`.

```
cargo test  -p v2xw-py                                    # no wheel needed
cargo clippy -p v2xw-py --all-targets --no-deps -- -D warnings
cd python && maturin develop && python -m pytest          # the Python half
```

## Two version pins, and why they are load-bearing

* **`pyo3 = 0.25`**, not the newest release. `arrow`'s `pyarrow` feature — the zero-copy
  hand-off — depends on `arrow-pyarrow`, which requires `pyo3 ^0.25.1`. Two pyo3 versions in
  one binary do not interoperate: a `Bound<'py, PyAny>` from one is a different type from
  the other's. Raising this means waiting for `arrow-pyarrow` to follow, or giving up the
  zero-copy path for Arrow IPC bytes.
* **`extension-module` is not a default feature.** It tells pyo3 not to link libpython,
  which is right for a wheel and wrong for `cargo test`; maturin turns it on
  (`python/pyproject.toml`).

## Owed changes, each of which belongs to another crate

These are referenced from the module documentation. None can be fixed from here.

1. **Metric providers do not reach the run manifest.** `ProviderSet::register` needs
   `&mut Registry` and `Engine` owns its registry privately, so `run` registers the
   providers in a registry of its own. The manifest therefore pins the models the engine
   registered and not the metric providers that measured it. Closing it needs a seam on
   `Engine` — a `metrics: &mut ProviderSet` argument to `build`, or a `registry_mut`.
   Owner: `v2xw-engine`.

2. **A run holds the interpreter lock.** `Python::allow_threads` needs its closure to be
   `Send`, and `Engine` is not: it owns `Box<dyn Mobility>` and a dozen other family trait
   objects that the family traits do not require to be `Send`. So a run blocks every other
   Python thread in the process for its duration. Use a process per run, which is what the
   experiment system does anyway (08-measurement §4). Making `Engine: Send` is a decision
   with consequences for every family trait. Owner: `v2xw-engine`.

3. **A `python-hot-path` run is not marked as one.** 03-interfaces.md §15 rule P2 requires
   the manifest to mark a run that uses a Python model on a hot path, with an expected
   slowdown. `Engine::build` assembles the manifest before a plug-in can be attached, so
   this crate has nowhere to write the mark. Owner: `v2xw-engine` (the manifest), with the
   `Registry` carrying the flag.

4. **There is no `Detector` trait to implement.** 07-threats-and-detection.md specifies the
   family and `v2xw-threat` is a stub, so `plugins::Detector` is declared here against what
   §15 publishes, together with `plugins::SpeedPlausibilityDetector` as a reference
   implementation. When the real trait lands, both move and `PyDetector` follows unchanged.
   Owner: `v2xw-threat`.

5. **A Python plug-in cannot yet be named in a scenario.** `plugins::PyCarFollowing`
   implements `v2xw_mobility::CarFollowing` and is callable through `dyn CarFollowing`
   today, but nothing in the scenario schema says "use this Python object", and
   `Engine::build` resolves its models from ids. The conformance kit drives the adapter
   directly, which is what makes the shape provable; wiring it into a run needs a
   `ModelChoice` variant that carries a Python callable, and a place on `Engine` to put it.
   Owner: `v2xw-engine` (`scenario::ModelChoice`, `wiring`).

## A leak found while testing, in `v2xw-record`

`export::schema::ground_truth_fields("phy.rx")` returns `["tx_node", "distance_m",
"los_class"]`. The record the engine writes on that channel has fields `tx`, `dist_m`,
`rx`, `msg`, `rssi_dbm`, `sinr_db`, `candidate`, `outcome`, `causes`, `t_start`, `t_end` —
none of the three declared names exists. So `TableSchema::without_ground_truth()` drops
nothing on the one channel where it matters most, and a `ground_truth=False` export of
`phy.rx` carries the true transmitter id and the true distance, which is exactly the oracle
state a NODE export must not have. The channel's own visibility tag says `node-and-gt`, so
the intent is not in doubt.

`python/tests/test_sdk.py` pins it two ways: `test_dropping_the_ground_truth_columns_drops_columns`
is `xfail(strict=True)`, and `test_the_ground_truth_field_list_does_not_match_the_channel`
asserts the mismatch directly. When `v2xw-record` renames the list, both fail at once,
which is the loudest available signal that the leak is closed.
