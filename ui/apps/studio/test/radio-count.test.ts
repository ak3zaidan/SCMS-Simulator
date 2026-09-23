/**
 * `radios 0` — the count that contradicted the panel beside it.
 *
 * The owner's report: the inspector said `radios 0` while the run had one node and `bytes_air` read
 * 1,468 B/s. Reproduced exactly, on the command line the engine's own banner suggests:
 *
 * ```
 * v2xw-server --scenario scenarios/phase1-manhattan.yaml --port 8787 --paused --speed 0
 * ```
 *
 * Connect to that and `run.status` answers `actors: 0, nodes: 0` — the scenario's vehicle has not
 * spawned at t = 0. `Hello` carries the node table as it stands at that instant, which is empty,
 * and `Hello` is sent **once per connection** (vwp-v1 §3.1). Press play: measured in the browser,
 * `helloNodes 0`, `poses 1`, `live 1`, `bytes_air 1468.000`, `radios 0`. The line was counting the
 * right kind of thing at the wrong time — a connect-time snapshot presented as a live count — and
 * nothing ever revised it.
 *
 * So the properties: the live count wins whenever there is one, the snapshot covers the moment
 * before the first status poll, and a genuine zero on a run that has not started says so in words
 * rather than printing a measurement of a system that is not running.
 */

import { describe, expect, it } from "vitest";

import { radioCount } from "../src/lib/format.js";

describe("radioCount", () => {
  it("reports the live count once the run has radios, whatever Hello said", () => {
    // The reported defect, exactly: Hello's table is empty and a radio is transmitting.
    expect(radioCount(1, 0, "running")).toBe("1");
    expect(radioCount(198, 0, "running")).toBe("198");
    // And it does not go backwards to the snapshot when the two disagree the other way.
    expect(radioCount(4, 198, "running")).toBe("4");
  });

  it("falls back to Hello's table before the first status poll lands", () => {
    // `run.status` is polled every 2 s; `run.nodes` is 0 until the first answer arrives, and a
    // fixture engine that hands over 198 nodes in its Hello should not read 0 for those two
    // seconds.
    expect(radioCount(0, 198, "running")).toBe("198");
    expect(radioCount(0, 1, "finished")).toBe("1");
  });

  it("says 'none yet' rather than 0 on a run that has not produced any", () => {
    // Paused at t = 0 there genuinely are no radios. Printing `0` there is not wrong, but it is
    // the same glyph the defect produced, and it reads as a measurement rather than as "not yet".
    expect(radioCount(0, 0, "paused")).toBe("none yet");
    expect(radioCount(0, 0, "idle")).toBe("none yet");
  });

  it("prints a real zero for a running scenario that truly has no radios", () => {
    // A traffic-only scenario — `equipped_fraction: 0` — has vehicles and no radios, and that is a
    // finding about the scenario, not a pending state.
    expect(radioCount(0, 0, "running")).toBe("0");
    expect(radioCount(0, 0, "finished")).toBe("0");
  });
});
