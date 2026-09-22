# `v2xw` — the V2X World Simulator from Python

The Python package is how a researcher drives the simulator: load a scenario, run it, read
a metric as an Arrow table, open the recording, and — when the model you need does not
exist — write it in Python and have the engine call it.

```python
import v2xw

scenario = v2xw.Scenario.load("scenarios/downtown.yaml")
run = v2xw.run(scenario, recording="run.mcap")
print(run.metrics.series("pdr"))
table = v2xw.read_ipc(run.metrics.ipc())          # or run.metrics.arrow(), zero copy
rec = v2xw.Recording.open("run.mcap")
print(rec.verify())
```

## Install

```
pip install maturin
cd python && maturin develop          # editable, into the active virtualenv
cd python && maturin build --release  # a wheel, in ../target/wheels
```

`pyarrow` is optional (`pip install 'v2xw[arrow]'`). Without it every table still crosses
as an Arrow IPC buffer, which `polars.read_ipc_stream` reads and a `.arrows` file holds.
With it, `Metrics.arrow()` and `Recording.arrow()` hand Python the engine's own buffers
through the Arrow C data interface — nothing is serialised and nothing is copied.

## What is in the package

| Name | What it is |
|---|---|
| `v2xw.Scenario` | load, overlay, validate, hash. `problems()` reports every conflict without raising; `load` raises on the first. |
| `v2xw.run` | build the engine and run it. Writes the recording and computes the metrics from one record stream, so they cannot disagree. |
| `v2xw.Run` | the manifest, the counters, the metrics, the recording path. |
| `v2xw.Metrics` | `series`, `arrow`, `ipc`, `summary`, `digest`. `rows()` exists and is the slow path, and says so. |
| `v2xw.Recording` | open, `verify`, `channels`, `records`, `arrow`, `table_schema`, `content_digest`. |
| `v2xw.math` | the engine's transcendentals. A plug-in uses these instead of `math`. |
| `v2xw.plugins` | `CarFollowing` and `Detector` to subclass, `card()` to build a model card that validates. |
| `v2xw.conformance` | the checks of 03-interfaces.md §17 a plug-in must pass. |
| `v2xw.run_twice` | two runs of one scenario, both record-stream digests — the determinism gate. |

## Writing a model

```python
import v2xw
from v2xw.plugins import CarFollowing, card, source

class Mine(CarFollowing):
    card = card(
        id="mobility/car-following/mine", family="mobility", version="0.1.0",
        purpose="...",
        parameters=[{"name": "tau", "unit": "s", "default": 0.8,
                     "source": source("paper", "doi:10.1000/xyz")}],
    )
    def accel(self, ego, leader, lane, weather):
        return ego.max_accel_mps2 * (1.0 - v2xw.math.pow(ego.speed_mps / ego.desired_speed_mps, 4.0))

v2xw.conformance.check(Mine()).raise_for_failures()
```

Three rules matter, and `v2xw.conformance` enforces them rather than advising them:

1. **No randomness of your own.** `random`, `numpy.random`, `secrets`, `os.urandom`. The
   simulator's randomness comes from streams keyed by `(domain, entity)` so a draw does not
   depend on what was drawn before it. The conformance kit replaces those functions with
   ones that raise and then calls your model.
2. **No wall clock.** Simulated time arrives as an argument.
3. **No `math`, and no `**` on floats.** Those are the platform C library, which is not
   specified to the last bit; two machines can disagree in the low bits and the run digests
   will not match although nothing is wrong with your model. Use `v2xw.math`.

Passing the kit is necessary and not sufficient: state carried between calls is not
decidable, and `v2xw.run_twice` over a real scenario is what catches it.

## Examples

* `examples/run_and_read_metric.py` — load, run, read a metric, read the recording back.
  Plots nothing on purpose.
* `examples/python_idm.py` — the Intelligent Driver Model as a plug-in, the conformance kit
  over it, and the same kit failing a deliberately broken version of it.

## Tests

```
cd python && maturin develop && python -m pytest
```

The Rust-side tests (`cargo test -p v2xw-py`) cover the same surface without a wheel; the
Python ones cover what needs `pyarrow` and the Python halves of the conformance kit.
