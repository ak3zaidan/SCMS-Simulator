/**
 * What the Studio is doing, in one sentence, with the button that resolves it.
 *
 * Every state the app can be in is enumerated here — the seven connection states
 * `@vwp/protocol`'s client publishes (`idle`, `connecting`, `handshaking`, `streaming`,
 * `reconnecting`, `closed`, `failed`) crossed with the seven run states the engine reports on
 * `run.status` (`idle`, `loading`, `running`, `paused`, `seeking`, `finished`, `error`), plus the
 * two states that have no connection at all: a recording open from a file, and no engine found.
 *
 * It exists because the interface used to print the connection token and stop there. A run that had
 * simply reached its end showed the word "closed" beside a Connect button that appeared dead — the
 * engine knew perfectly well (`run.status` answers `state: "finished"`), the page had the
 * information, and said none of it. The rule this module enforces is: name the state in words a
 * researcher can act on, and put the action next to it.
 *
 * Pure, and deliberately so: it takes a snapshot and returns text, so the whole state table is
 * checkable in a unit test without a socket, a server or a browser.
 */

import type { RunState, VwpConnectionState } from "@vwp/protocol";

/** How loudly the state should read. `busy` is a transition nobody needs to act on. */
export type StatusTone = "ok" | "busy" | "warn" | "err" | "idle";

/** What the one button beside the sentence does. */
export type StatusActionKind = "run" | "resume" | "pause" | "connect" | "none";

export interface StatusAction {
  /** Imperative and short: "Run", "Play", "Reconnect". */
  readonly label: string;
  readonly kind: StatusActionKind;
  /** Why this is the next step, for the button's tooltip. */
  readonly hint: string;
}

export interface StatusView {
  readonly tone: StatusTone;
  /** Two or three words, for the chip in the header. Sentence case, never a wire token. */
  readonly chip: string;
  /** One plain sentence: what is happening right now. */
  readonly headline: string;
  /** What to do next, or why nothing needs doing. Empty when the headline says it all. */
  readonly detail: string;
  /** The action that resolves this state, or `null` when there is nothing to press. */
  readonly action: StatusAction | null;
  /**
   * True when the state deserves an explanation on top of the viewport rather than only a chip:
   * anything that is not a run in normal motion.
   */
  readonly banner: boolean;
}

/** The snapshot {@link describeStatus} reads. Everything a caller already has in the store. */
export interface StatusInput {
  readonly connection: VwpConnectionState;
  readonly runState: RunState;
  /** True while a local recording drives the viewport; there is no run to control then. */
  readonly replayOpen: boolean;
  /** The recording's file name, for the sentence. */
  readonly replayLabel?: string;
  /** True once a `Hello` has been decoded, so the page has a world and a scenario. */
  readonly hasHello: boolean;
  /** The scenario's name, when one is known. */
  readonly scenarioName?: string;
  /** Where the engine was looked for: an origin, for the "nothing answered" sentence. */
  readonly targetLabel?: string;
  /** Whether the last probe of that target reached anything. */
  readonly targetReachable?: boolean;
  /** How long the run's span is, already formatted (e.g. "5m 00s"). */
  readonly spanText?: string;
  /** Where the clock is, already formatted. */
  readonly clockText?: string;
}

const RUN: StatusAction = {
  label: "Run",
  kind: "run",
  hint: "Start the scenario from the beginning",
};

const RUN_AGAIN: StatusAction = {
  label: "Run again",
  kind: "run",
  hint: "Rewind to the start and run the scenario again",
};

const PLAY: StatusAction = { label: "Play", kind: "resume", hint: "Let the clock advance again" };

const CONNECT: StatusAction = {
  label: "Connect",
  kind: "connect",
  hint: "Look for an engine again and open a stream to it",
};

/**
 * The sentence, and the button, for the state the app is in.
 *
 * Ordered by what the user is looking at, not by what the socket is doing: a recording open in the
 * page owns the viewport whatever the connection is doing behind it, and a run that has finished is
 * a finished run whether the engine kept the socket open or closed it afterwards.
 */
export function describeStatus(input: StatusInput): StatusView {
  const { connection, runState } = input;

  if (input.replayOpen) {
    return {
      tone: "ok",
      chip: "Recording",
      headline: `Showing ${input.replayLabel ?? "a recording"} from a file.`,
      detail: "Drag the timeline to move through it. No engine is running and nothing is uploaded.",
      action: null,
      banner: false,
    };
  }

  // A finished run is a finished run. The socket may be open (the engine stopped stepping) or
  // closed — the protocol tells the client never to retry after "run complete", so the socket ends
  // up closed and the client's own state token is the least informative thing on the page. Either
  // way the honest sentence is the same one, and the action is to run it again, not to reconnect.
  // `runState` only reaches "finished" from a `run.status` the engine answered, so this cannot
  // fire before an engine has been reached.
  if (runState === "finished") {
    return {
      tone: "ok",
      chip: "Run finished",
      headline: input.spanText
        ? `The run reached the end of its ${input.spanText} of simulated time.`
        : "The run reached the end of its simulated time.",
      detail:
        connection === "streaming"
          ? "Press Run again to rewind to the start, or drag the timeline to look back over what happened."
          : "The engine closed the stream because there was nothing left to send. Press Run again to rewind and start over.",
      action: RUN_AGAIN,
      banner: true,
    };
  }

  /**
   * Nothing has ever answered, so this is not a dropped connection.
   *
   * The client retries for ever, which meant a page opened with no engine running said
   * "Reconnecting…" and offered nothing — implying a connection that had existed and would come
   * back, when the probe had already found nothing at the address. When no run has ever arrived and
   * the last probe reached nothing, the honest report is that there is no engine there, and the
   * action is to start one and press Connect.
   */
  const neverReached = !input.hasHello && input.targetReachable === false;
  if (neverReached && (connection === "connecting" || connection === "reconnecting" || connection === "closed")) {
    return {
      tone: "err",
      chip: "No engine",
      headline: input.targetLabel
        ? `Nothing is answering at ${input.targetLabel}.`
        : "No engine is answering.",
      detail:
        "Start a simulator and press Connect. The Details panel lists every address the page tried.",
      action: CONNECT,
      banner: true,
    };
  }

  switch (connection) {
    case "idle":
      return {
        tone: "idle",
        chip: "Not connected",
        headline: "Not connected to an engine yet.",
        detail: "Press Connect to look for one.",
        action: CONNECT,
        banner: true,
      };

    case "connecting":
      return {
        tone: "busy",
        chip: "Connecting",
        headline: input.targetLabel
          ? `Connecting to the engine at ${input.targetLabel}…`
          : "Connecting to the engine…",
        detail: "",
        action: null,
        banner: true,
      };

    case "handshaking":
      return {
        tone: "busy",
        chip: "Loading",
        headline: "Connected. Loading the world and the run's description…",
        detail: "The streets appear first, then the vehicles.",
        action: null,
        banner: true,
      };

    case "reconnecting":
      return {
        tone: "warn",
        chip: "Reconnecting",
        headline: "The connection dropped. Reconnecting…",
        detail: "The stream resumes where it left off, so nothing already received is lost.",
        action: null,
        banner: true,
      };

    case "failed":
      return {
        tone: "err",
        chip: "No engine",
        headline: input.targetLabel
          ? `No engine answered at ${input.targetLabel}.`
          : "No engine answered.",
        detail: "Start the engine, then press Connect. The Details panel lists everywhere the page looked.",
        action: CONNECT,
        banner: true,
      };

    case "closed":
      return {
        tone: "warn",
        chip: "Disconnected",
        headline: input.hasHello
          ? "The stream closed. What is on screen is the last thing the engine sent."
          : "The stream closed before the run arrived.",
        detail: "Press Connect to open it again.",
        action: CONNECT,
        banner: true,
      };

    case "streaming":
      break;
  }

  switch (runState) {
    case "running":
      return {
        tone: "ok",
        chip: "Running",
        headline: input.scenarioName ? `Running ${input.scenarioName}.` : "The run is playing.",
        detail: "",
        action: null,
        banner: false,
      };

    case "paused":
      return {
        tone: "ok",
        chip: "Paused",
        headline: input.clockText
          ? `Paused at ${input.clockText} of simulated time.`
          : "The run is paused.",
        detail: "Press Play to let it advance, or step forward one interval at a time.",
        action: PLAY,
        banner: false,
      };

    case "idle":
      return {
        tone: "idle",
        chip: "Ready",
        headline: "Connected to the engine. No run has been started yet.",
        detail: "Press Run to start the scenario in the panel on the left.",
        action: RUN,
        banner: true,
      };

    case "loading":
      return {
        tone: "busy",
        chip: "Starting",
        headline: "The engine is loading the scenario…",
        detail: "Vehicles appear once the first snapshot arrives.",
        action: null,
        banner: true,
      };

    case "seeking":
      return {
        tone: "busy",
        chip: "Seeking",
        headline: "Moving to a new point in simulated time…",
        detail: "",
        action: null,
        banner: false,
      };

    case "error":
      return {
        tone: "err",
        chip: "Engine error",
        headline: "The engine stopped with an error and cannot continue this run.",
        detail: "The Log tab of the inspector has what it reported. Press Run again to start over.",
        action: RUN_AGAIN,
        banner: true,
      };
  }
  // "finished" is handled above, before the connection is considered at all: the compiler knows
  // this switch has covered every remaining run state.
}

/** The chip's colour class, matching the `pill` modifiers in `styles/app.css`. */
export function toneClass(tone: StatusTone): string {
  switch (tone) {
    case "ok":
      return "ok";
    case "busy":
    case "warn":
      return "warn";
    case "err":
      return "err";
    case "idle":
      return "";
  }
}
