/**
 * The transport's capability rules, against the engine's actual refusals.
 *
 * Each case here is a state in which `crates/v2xw-server` refuses one of the six run-control
 * methods, and the assertion is that the interface does not offer it there. They were written from
 * the engine's own state checks, and the two that matter most were confirmed against a running
 * server: a run that has finished reports `state: "finished"` with the stream closed, and `run.step`
 * on it is accepted and advances nothing.
 */

import { describe, expect, it } from "vitest";

import { transport, type TransportInput } from "../src/state/transport.js";

const BASE: TransportInput = {
  connection: "streaming",
  runState: "running",
  tNs: 12_000_000_000,
  tEndNs: 60_000_000_000,
  streamNs: 12_000_000_000,
  recording: null,
  busy: false,
};

describe("transport", () => {
  it("offers pause while running and nothing else that moves the clock", () => {
    const t = transport(BASE);
    expect(t.pause.enabled).toBe(true);
    expect(t.play.enabled).toBe(false);
    // The engine answers `RunNotRunning: pause before stepping`, so a Step button here is a button
    // that produces an error dialog.
    expect(t.step.enabled).toBe(false);
    expect(t.step.why).toMatch(/pause first/i);
  });

  it("offers play and step while paused", () => {
    const t = transport({ ...BASE, runState: "paused" });
    expect(t.play.enabled).toBe(true);
    expect(t.step.enabled).toBe(true);
    expect(t.pause.enabled).toBe(false);
  });

  it("offers only restart once the run has finished", () => {
    const t = transport({ ...BASE, runState: "finished", tNs: 60_100_000_000, streamNs: 60_100_000_000 });
    expect(t.play.enabled).toBe(false);
    expect(t.step.enabled).toBe(false);
    expect(t.restart.enabled).toBe(true);
    // The sentence has to name the action, not the state: this is the text that replaced "closed".
    expect(t.play.why).toMatch(/Restart/);
  });

  it("keeps restart available when the engine has closed the stream, and drops everything else", () => {
    const t = transport({
      ...BASE,
      connection: "closed",
      runState: "finished",
      tNs: 60_100_000_000,
      streamNs: 0,
    });
    // `run.start` is answered over HTTP, so this is the one thing that still works — which is the
    // whole point: the page the owner saw had a Connect button and no way to run anything.
    expect(t.restart.enabled).toBe(true);
    expect(t.play.enabled).toBe(false);
    expect(t.pause.enabled).toBe(false);
    expect(t.speed.enabled).toBe(false);
    // `run.seek` is refused over HTTP by the server itself, and the reason says so.
    expect(t.seek.enabled).toBe(false);
    expect(t.seek.why).toMatch(/open stream/i);
  });

  it("draws the span past the nominal end when a run overran it", () => {
    // Observed on the live engine: `t_ns` 60.1 s against `t_end_ns` 60.0 s, because the last step
    // lands on the far side of the boundary. A span of 60.0 s would put the thumb off the end.
    const t = transport({ ...BASE, runState: "finished", tNs: 60_100_000_000, streamNs: 60_100_000_000 });
    expect(t.spanNs).toBe(60_100_000_000);
    expect(t.partial).toBe(false);
  });

  it("marks the span partial while most of the run has not been simulated", () => {
    const t = transport({ ...BASE, tNs: 3_000_000_000, streamNs: 3_000_000_000 });
    expect(t.partial).toBe(true);
    expect(t.seekMaxNs).toBe(3_000_000_000);
    expect(t.spanNs).toBe(60_000_000_000);
  });

  it("takes the stream's clock when it leads the 2 s status poll", () => {
    const t = transport({ ...BASE, tNs: 12_000_000_000, streamNs: 13_400_000_000 });
    expect(t.seekMaxNs).toBe(13_400_000_000);
  });

  it("gives a recording a position and no transport at all", () => {
    const t = transport({
      ...BASE,
      connection: "closed",
      recording: { startNs: 5_000_000_000, endNs: 90_000_000_000 },
    });
    expect(t.seek.enabled).toBe(true);
    expect(t.play.enabled).toBe(false);
    expect(t.speed.enabled).toBe(false);
    expect(t.restart.enabled).toBe(false);
    expect(t.minNs).toBe(5_000_000_000);
    expect(t.spanNs).toBe(90_000_000_000);
  });

  it("disables everything, without changing the reasons, while a call is in flight", () => {
    const t = transport({ ...BASE, runState: "paused", busy: true });
    expect(t.play.enabled).toBe(false);
    expect(t.step.enabled).toBe(false);
    expect(t.seek.enabled).toBe(false);
    expect(t.restart.enabled).toBe(false);
  });

  it("does not offer a seek before anything has been simulated", () => {
    // `run.start` rewinds to `produced = 0`, where the engine's seek range is (0, 0).
    const t = transport({ ...BASE, runState: "paused", tNs: 0, streamNs: 0 });
    expect(t.seek.enabled).toBe(false);
    expect(t.seek.why).toMatch(/Nothing has been simulated/);
  });
});
