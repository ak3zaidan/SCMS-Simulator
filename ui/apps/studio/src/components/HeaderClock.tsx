/**
 * The simulated clock in the header, as its own subscriber.
 *
 * A leaf for the same reason `StatsReadout` is one: the clock moves five times a second, and
 * reading it from `App` would re-render the scenario panel, the inspector and the plots strip at
 * that rate for the sake of one string.
 *
 * It says *simulated* time, not wall-clock time, because the difference is the whole point — at 5×
 * these two numbers diverge, and a clock that does not say which one it is is a trap.
 */

import { useStudio } from "../state/store.js";
import { simClock } from "../lib/format.js";

export function HeaderClock(): React.JSX.Element {
  const simTimeNs = useStudio((s) => s.simTimeNs);
  const tNs = useStudio((s) => s.run.tNs);
  const replay = useStudio((s) => s.replay);
  const now = replay !== null ? replay.tNs : simTimeNs > 0 ? simTimeNs : tNs;
  return (
    <span className="headline-clock" data-testid="header-clock" title="Time inside the simulation, hours:minutes:seconds">
      <span className="mono">{simClock(now)}</span>
      <span className="faint"> simulated</span>
    </span>
  );
}
