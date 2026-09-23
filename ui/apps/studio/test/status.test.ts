/**
 * The state table of `lib/status.ts`.
 *
 * Every state the interface can be in has to say something a user can act on, so this walks the
 * whole cross product — seven connection states by seven run states, plus the recording case — and
 * asserts three properties of each: the sentence is a sentence, it does not leak a wire token, and
 * a state the user could resolve offers the button that resolves it.
 *
 * The case that drove the module gets its own test: a run that reached its end with the socket
 * closed. That was reported as the single word "closed", beside a Connect button that appeared
 * dead, while the engine was answering `state: "finished"` the whole time.
 */

import { describe, expect, it } from "vitest";
import type { RunState, VwpConnectionState } from "@vwp/protocol";

import { describeStatus, toneClass, type StatusInput } from "../src/lib/status.js";

const CONNECTIONS: VwpConnectionState[] = [
  "idle", "connecting", "handshaking", "streaming", "reconnecting", "closed", "failed",
];
const RUN_STATES: RunState[] = ["idle", "loading", "running", "paused", "seeking", "finished", "error"];

function input(patch: Partial<StatusInput> = {}): StatusInput {
  return {
    connection: "streaming",
    runState: "running",
    replayOpen: false,
    hasHello: true,
    scenarioName: "phase1-manhattan",
    targetLabel: "http://127.0.0.1:8787",
    targetReachable: true,
    spanText: "5m 00s",
    clockText: "00:01:12.400",
    ...patch,
  };
}

/** Anything that would read as a protocol token rather than as English. */
const WIRE_TOKENS = [
  "§", "run.status", "run.start", "prov_id", "NodeTelemetry", "MetricSample", "Hello",
  "with_schema", "vwp-v1", "09-ui", "t_ns",
];

describe("describeStatus", () => {
  it("gives every state a sentence, and no state a wire token", () => {
    for (const connection of CONNECTIONS) {
      for (const runState of RUN_STATES) {
        const view = describeStatus(input({ connection, runState }));
        const where = `${connection}/${runState}`;
        expect(view.chip, where).not.toBe("");
        // A chip is a label, not a paragraph, and never the raw state token.
        expect(view.chip.length, where).toBeLessThanOrEqual(16);
        expect(view.chip, where).not.toBe(connection);
        expect(view.chip, where).not.toBe(runState);
        // A headline is one sentence, ending like one.
        expect(view.headline.trimEnd(), where).toMatch(/[.…]$/);
        expect(view.headline[0], where).toBe(view.headline[0].toUpperCase());
        for (const token of WIRE_TOKENS) {
          expect(`${view.chip} ${view.headline} ${view.detail}`, `${where} leaks ${token}`).not.toContain(token);
        }
      }
    }
  });

  it("offers an action in every state the user can resolve, and none in a state they cannot", () => {
    // Nothing to press while a transition is in flight: pressing something would fight it.
    for (const connection of ["connecting", "handshaking", "reconnecting"] as const) {
      expect(describeStatus(input({ connection, runState: "idle" })).action, connection).toBeNull();
    }
    // Everything the user is waiting on, on the other hand, has its button.
    expect(describeStatus(input({ connection: "idle", runState: "idle" })).action?.kind).toBe("connect");
    expect(describeStatus(input({ connection: "failed", runState: "idle" })).action?.kind).toBe("connect");
    expect(describeStatus(input({ connection: "closed", runState: "paused" })).action?.kind).toBe("connect");
    expect(describeStatus(input({ runState: "paused" })).action?.kind).toBe("resume");
    expect(describeStatus(input({ runState: "idle" })).action?.kind).toBe("run");
    expect(describeStatus(input({ runState: "error" })).action?.kind).toBe("run");
  });

  it("calls a finished run finished, whether the socket is open or closed", () => {
    for (const connection of CONNECTIONS) {
      const view = describeStatus(input({ connection, runState: "finished" }));
      expect(view.chip, connection).toBe("Run finished");
      expect(view.headline, connection).toContain("reached the end");
      // The span, so "finished" answers "finished what?".
      expect(view.headline, connection).toContain("5m 00s");
      // And the action is to run it again — never to reconnect to a run that has ended.
      expect(view.action?.kind, connection).toBe("run");
      expect(view.action?.label, connection).toBe("Run again");
      expect(view.banner, connection).toBe(true);
    }
  });

  it("explains a closed socket after a finished run as the engine having nothing left to send", () => {
    const view = describeStatus(input({ connection: "closed", runState: "finished" }));
    expect(view.detail).toContain("nothing left to send");
    expect(view.detail).toContain("Run again");
    // Never the bare token the page used to show here.
    expect(view.chip).not.toBe("closed");
  });

  it("says nothing about a run while a recording is driving the viewport", () => {
    const view = describeStatus(input({ connection: "closed", runState: "finished", replayOpen: true, replayLabel: "campaign-7.mcap" }));
    expect(view.chip).toBe("Recording");
    expect(view.headline).toContain("campaign-7.mcap");
    // There is no run to restart, so offering a run button would be a lie about what is on screen.
    expect(view.action).toBeNull();
  });

  it("does not call it a reconnection when nothing ever answered", () => {
    // The client retries for ever, so a page opened with no engine running sat on "Reconnecting…"
    // with no action — describing a connection that had never existed.
    const view = describeStatus(
      input({ connection: "reconnecting", runState: "idle", hasHello: false, targetReachable: false, targetLabel: "http://127.0.0.1:9" }),
    );
    expect(view.chip).toBe("No engine");
    expect(view.headline).toContain("http://127.0.0.1:9");
    expect(view.action?.kind).toBe("connect");
    // A genuine drop, after a run had arrived, still reads as a reconnection.
    const dropped = describeStatus(input({ connection: "reconnecting", runState: "running", hasHello: true, targetReachable: true }));
    expect(dropped.chip).toBe("Reconnecting");
  });

  it("names where it looked when nothing answered", () => {
    const view = describeStatus(input({ connection: "failed", runState: "idle", targetLabel: "http://127.0.0.1:9999" }));
    expect(view.headline).toContain("http://127.0.0.1:9999");
    expect(view.tone).toBe("err");
  });

  it("keeps the banner out of the way of a run in normal motion", () => {
    expect(describeStatus(input({ runState: "running" })).banner).toBe(false);
    expect(describeStatus(input({ runState: "paused" })).banner).toBe(false);
    expect(describeStatus(input({ runState: "idle" })).banner).toBe(true);
  });

  it("maps every tone onto a pill class", () => {
    expect(toneClass("ok")).toBe("ok");
    expect(toneClass("busy")).toBe("warn");
    expect(toneClass("err")).toBe("err");
    expect(toneClass("idle")).toBe("");
  });
});
