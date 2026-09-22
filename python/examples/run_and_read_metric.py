#!/usr/bin/env python3
"""Load a scenario, run it, read a metric. Plot nothing.

    python examples/run_and_read_metric.py [scenario.yaml]

With no argument it runs the built-in minimal scenario, shortened to five seconds, so the
example works in a fresh checkout with no scenario file to hand.

What it shows, in order:

1. a scenario is loaded and validated, and its content hash is the run's identity;
2. the run writes an MCAP recording and produces metric samples in one pass;
3. a metric comes out as a time series, and the whole table comes out as Arrow;
4. the recording is reopened, verified, and one channel is read back as a table;
5. the manifest in the recording is the manifest of the run that wrote it.

It prints numbers and draws nothing: a figure is a separate concern
(08-measurement-and-data.md §3), and an example that plots hides what it computed.
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

import v2xw


def load(argv: list[str]) -> v2xw.Scenario:
    """The scenario named on the command line, or a built-in one with traffic on it.

    The built-in minimal scenario has no demand model, so nothing drives and there is
    nothing to measure. Five seconds of a Poisson arrival process on the procedural grid
    puts a couple of equipped vehicles on the road, which is enough for the metric and the
    recording to be real and small enough to finish while you watch.
    """
    if len(argv) > 1:
        return v2xw.Scenario.load(argv[1])
    doc = v2xw.Scenario.minimal().as_dict()
    doc["meta"]["name"] = "minimal-with-traffic"
    doc["time"]["duration_s"] = 5.0
    doc["actors"]["vehicles"]["demand"] = {
        "kind": "mobility/demand/poisson-thinned",
        "rate_veh_per_h": 600.0,
        "params": None,
    }
    return v2xw.Scenario.from_dict(doc)


def main(argv: list[str]) -> int:
    scenario = load(argv)

    # Validation is separate from loading: `problems()` reports every conflict without
    # raising, which is what you want when you are editing a document.
    problems = scenario.problems()
    if problems:
        print("this scenario will not run:")
        for p in problems:
            print(f"  - {p}")
        return 1

    print(f"scenario   {scenario.name}  seed={scenario.seed:#x}")
    print(f"           {scenario.duration_s} s, hash {scenario.content_hash[:16]}")

    with tempfile.TemporaryDirectory() as tmp:
        recording = str(Path(tmp) / "run.mcap")

        # One pass: the recorder tees each record to the file and to the metric providers,
        # so the numbers and the file cannot disagree. `build_utc` defaults to now, read in
        # this process — the engine never reads a clock (02-architecture.md §6.1).
        run = v2xw.run(scenario, recording=recording, metric_window_s=1.0)

        print(f"run        {run.records} records, "
              f"{run.report['frames_transmitted']} frames, "
              f"ends at {run.end_ns / 1e9:.1f} s")
        if run.records_refused:
            print(f"           WARNING {run.records_refused} records were refused by the "
                  f"recording, so the file is not the whole run")

        metrics = run.metrics
        print(f"metrics    {len(metrics)} samples over {len(metrics.names())} metrics")
        print(f"           digest {metrics.digest[:16]}")

        # One metric as a time series. A sample the statistics refused — too few
        # observations for an honest estimate — has no point value and is left out rather
        # than shown as a zero, so a series can be shorter than the number of windows, and
        # a metric can have no series at all. That is reported rather than hidden: on a
        # five-second run with two vehicles, `pdr` genuinely has too few receptions to
        # estimate, and a number there would be a fabrication.
        wanted = ("pdr", "airtime_per_node", "bytes_air", "full_cert_share")
        for name in wanted:
            series = metrics.series(name)
            if series:
                head = ", ".join(f"{t / 1e9:.0f}s={v:.4g}" for t, v in series[:5])
                print(f"  {name:<20} {len(series)} points: {head}")
            else:
                print(f"  {name:<20} no point estimate: too few samples to be honest about")

        # The whole table as Arrow. `ipc()` is one buffer and needs nothing installed;
        # `arrow()` hands pyarrow the same allocation and needs pyarrow.
        buffer = metrics.ipc()
        print(f"arrow      {len(buffer)} bytes of Arrow IPC")
        try:
            table = v2xw.read_ipc(buffer)
            print(f"           {table.num_rows} rows x {table.num_columns} columns"
                  f" ({', '.join(table.column_names[:5])}, ...)")
        except ImportError as exc:
            print(f"           (pyarrow not installed: {exc})")

        # Back out of the recording.
        rec = v2xw.Recording.open(recording)
        report = rec.verify()
        print(f"recording  {report['records']} records, {report['frames']} frames, "
              f"integrity_verified={report['integrity_verified']}")
        channels = [c["topic"] for c in rec.channels]
        print(f"           channels: {', '.join(channels[:6])}"
              f"{' ...' if len(channels) > 6 else ''}")

        record_channels = sorted(
            c["topic"].removeprefix("record/") for c in rec.channels
            if c["topic"].startswith("record/")
        )
        if record_channels:
            channel = record_channels[0]
            rows = rec.records(channel, limit=2)
            print(f"           {channel}: first {len(rows)} of its records")
            for row in rows:
                print(f"             t={row['sim_time_ns'] / 1e9:.3f}s {row['record']}")

        assert rec.manifest["scenario_hash"] == scenario.content_hash, (
            "the recording must carry the manifest of the run that wrote it"
        )
        print("manifest   the recording's manifest pins the scenario that produced it")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
