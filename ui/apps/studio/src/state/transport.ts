/**
 * What the transport can actually do right now, and why not when it cannot.
 *
 * `lib/status.ts` says what state the run is in, in words. This says which of the five controls
 * that state will accept, and it is a separate question with a separate source: every answer here
 * is a state check in `crates/v2xw-server`, read off the engine rather than guessed.
 *
 *  * `run.resume` — refused unless the run is **paused** (`RunNotRunning: state is …, not paused`).
 *  * `run.pause` — refused unless it is **running**.
 *  * `run.step` — refused while the run is **running** (`pause before stepping`), and on a finished
 *    run it is accepted but steps zero times, because the engine has nothing left to produce. A
 *    button that reports success and moves nothing is worse than a disabled one.
 *  * `run.speed` — accepted in any state; the producer reads the multiple on its next step. `0`
 *    means unthrottled, which the selector never offered even though the server takes it.
 *  * `run.seek` — refused outside the range the run has **produced so far**
 *    (`seek_range() = (base_index·Δt, (produced−1)·Δt)`), and refused entirely over HTTP, where the
 *    server answers "run.seek streams its result over the connection; call it on the socket".
 *  * `run.start` — rewinds, and is refused while the run is **running** (`RunAlreadyRunning`).
 *    Works over HTTP, which is what makes a finished run recoverable from a closed page.
 *
 * The seekable bound is the reason this file exists at all. The bar drew its range over the
 * scenario's whole duration and said nothing about how much of it existed, so on a run three
 * seconds into a sixty-second scenario, most of the bar was time nothing had simulated, and a drag
 * into it came back `-32003 seek out of range` — which `TimeControls` swallowed, leaving the thumb
 * to snap back with no explanation at all.
 *
 * {@link Transport.seekMaxNs} is how far the run has *certainly* got: `run.status` does not publish
 * `seek_range`, but it publishes `t_ns`, and `t_ns = cursor·Δt` with `cursor ≤ produced`, so `t_ns`
 * is always inside the range. It is a bound to **draw**, not to enforce: how much further a given
 * engine will seek is the engine's business — this repository's own two disagree, the live engine
 * allowing only what it has produced and the fixture generating any instant in the run on demand —
 * so the range that gets enforced is the one a refusal reports, and `TimeControls` reads it from
 * the error rather than guessing it here.
 */

import type { RunState, VwpConnectionState } from "@vwp/protocol";

/** One control: whether it will work, and the sentence for its tooltip either way. */
export interface Control {
  readonly enabled: boolean;
  /** What it does, or why it cannot. Always a full phrase, never a method name. */
  readonly why: string;
}

export interface Transport {
  readonly play: Control;
  readonly pause: Control;
  readonly step: Control;
  readonly speed: Control;
  readonly seek: Control;
  readonly restart: Control;
  /** Earliest simulated time the bar can reach. Non-zero only for a recording. */
  readonly minNs: number;
  /** Furthest simulated time a seek can reach — what the engine has produced. */
  readonly seekMaxNs: number;
  /** The span the bar draws, which is the run's intended length or further if it overran. */
  readonly spanNs: number;
  /** True when part of the span has not been simulated yet, so the bar shows two regions. */
  readonly partial: boolean;
}

export interface TransportInput {
  readonly connection: VwpConnectionState;
  readonly runState: RunState;
  /** `run.status.t_ns`. */
  readonly tNs: number;
  /** `run.status.t_end_ns`, or `Hello.sim_duration_ns` before the first status. */
  readonly tEndNs: number;
  /** Where the stream itself has reached, which leads `run.status` between polls. */
  readonly streamNs: number;
  /** Set while a local recording owns the viewport; it replaces the run entirely. */
  readonly recording: { readonly startNs: number; readonly endNs: number } | null;
  /** True while a call is in flight, which disables everything without changing the reasons. */
  readonly busy: boolean;
}

const CLOSED = "The stream is closed. Press Connect to open it again.";
const ENDED = "The run has ended. Press Restart to run it again from the beginning.";

function off(why: string): Control {
  return { enabled: false, why };
}

/** Resolve the input into the five controls and the bar's three bounds. */
export function transport(input: TransportInput): Transport {
  const { connection, runState, recording, busy } = input;

  if (recording !== null) {
    // A recording has a position and nothing else: no clock to start, no speed to set, no engine to
    // refuse anything. Offering a greyed-out play button for it was inviting a press that could
    // never do anything.
    const why = "A recording is already finished — drag the bar to move through it.";
    return {
      play: off(why),
      pause: off(why),
      step: off(why),
      speed: off(why),
      seek: { enabled: !busy, why: "Move to a point in the recording" },
      restart: off("Close the recording to get back to the live run."),
      minNs: recording.startNs,
      seekMaxNs: recording.endNs,
      spanNs: recording.endNs,
      partial: false,
    };
  }

  const streaming = connection === "streaming";
  const produced = Math.max(input.tNs, input.streamNs);
  const spanNs = Math.max(input.tEndNs, produced);
  const ended = runState === "finished";

  const stopped = !streaming ? CLOSED : ended ? ENDED : null;

  return {
    play:
      busy || !streaming || runState !== "paused"
        ? off(stopped ?? (runState === "running" ? "It is already running." : "The run is not paused."))
        : { enabled: true, why: "Let the simulated clock advance again" },
    pause:
      busy || !streaming || runState !== "running"
        ? off(stopped ?? "Only a running run can be paused.")
        : { enabled: true, why: "Hold the simulated clock where it is" },
    step:
      busy || !streaming || (runState !== "paused" && runState !== "idle")
        ? off(
            stopped ??
              (runState === "running"
                ? "Pause first — the engine refuses a step while the run is moving."
                : "Nothing to step through."),
          )
        : { enabled: true, why: "Advance by one interval and stop again" },
    speed:
      busy || !streaming
        ? off(stopped ?? "Connect to change the speed.")
        : { enabled: true, why: "How fast simulated time runs against the clock on the wall" },
    seek:
      busy || !streaming || produced <= 0
        ? off(
            !streaming
              ? // Worth spelling out: over HTTP every other control still works, and this one
                // genuinely cannot, so "the stream is closed" is the whole reason.
                "Seeking needs an open stream — the engine sends the frames for the new position on it."
              : "Nothing has been simulated yet.",
          )
        : { enabled: true, why: "Move to a point the run has already simulated" },
    restart:
      busy || connection === "connecting" || connection === "handshaking"
        ? off("Waiting for the engine.")
        : { enabled: true, why: "Rewind to the beginning and run it again" },
    minNs: 0,
    seekMaxNs: produced,
    spanNs,
    partial: spanNs > 0 && produced < spanNs - 1,
  };
}
