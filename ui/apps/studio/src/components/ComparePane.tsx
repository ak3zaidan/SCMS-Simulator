/**
 * Side B's viewport — the right half of the side-by-side of 09-ui §6.
 *
 * Its own canvas and its own `Viewer`, because the two runs have two sets of poses and one scene
 * cannot hold both without inventing a convention for which actor is whose. The camera is not its
 * own, though: while `compareSync.camera` is on, side A's `CameraState` is copied here every frame
 * of the publish tick, so the two panes always look at the same place from the same angle — which
 * is the only way a visual difference between them means anything.
 *
 * There is no picking and no `view.follow` here: side B is a baseline being read, not a run being
 * driven, and the inspector belongs to side A. Clicking side B would otherwise silently repoint the
 * HUD at a node from the other run.
 */

import { useEffect, useRef } from "react";

import { compare } from "../state/compare.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { simClock } from "../lib/format.js";
import { clientSubject } from "../lib/provenance.js";

export function ComparePane(): React.JSX.Element | null {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const side = useStudio((s) => s.compare);
  const sync = useStudio((s) => s.compareSync);
  const theme = useStudio((s) => s.theme);
  const setWhy = useStudio((s) => s.setWhy);

  useEffect(() => {
    const canvas = canvasRef.current;
    const host = hostRef.current;
    if (!canvas || !host) return;
    const viewer = compare.mountViewer(canvas, theme);
    const observer = new ResizeObserver(() => viewer.resize(host.clientWidth, host.clientHeight));
    observer.observe(host);
    viewer.resize(host.clientWidth, host.clientHeight);
    compare.start();
    return () => {
      observer.disconnect();
      // The canvas goes away with this component, so the frame loop must stop: a second
      // `requestAnimationFrame` loop drawing into a detached canvas is the main viewport's budget
      // spent on nothing (09-ui §4). Stopped, not unmounted — `Viewer.unmount` disposes the
      // renderer, and re-creating one on a canvas whose WebGL context is already in hand is the
      // fragile path, which StrictMode's mount/cleanup/mount would walk on every page load.
      // `mountViewer` starts the loop again. The controller itself keeps publishing: side B is
      // still open and the scrub bar is still driving its clock.
      compare.viewer?.stop();
    };
    // `theme` has its own effect; re-running this one would re-create the renderer.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    compare.viewer?.setTheme(theme);
  }, [theme]);

  // Mirror the camera on the publish tick rather than on a rAF of its own: at 5 Hz the exponential
  // smoothing in both `CameraController`s does the interpolation, and a second animation loop
  // would fight the first for the frame budget (09-ui §4).
  const tick = useStudio((s) => s.seriesTick);
  useEffect(() => {
    if (sync.camera) compare.mirrorCameraFrom(engine.viewer);
  }, [tick, sync.camera]);

  if (side === null) return null;

  return (
    <div className="viewport compare-pane" ref={hostRef} data-testid="compare-pane">
      <canvas ref={canvasRef} data-testid="compare-canvas" aria-label="Comparison viewport (side B)" />
      <div className="viewport-toolbar">
        <span className="chip" data-testid="compare-label">
          B · {side.label}
        </span>
        <span className="spacer grow" />
        <button
          type="button"
          className="chip linklike"
          data-testid="compare-position-why"
          onClick={() =>
            setWhy(
              clientSubject(
                side.source === "replay" ? "replay_position" : "compare_time",
                `side B at ${simClock(side.tNs)}`,
                simClock(side.tNs),
                "hh:mm:ss.mmm of simulated time",
              ),
            )
          }
        >
          t {simClock(side.tNs)}
        </button>
        <span className="chip">{side.actors} actors</span>
        {compare.worldBorrowed ? (
          <span
            className="chip warn-chip"
            title="A recording does not contain the streets, so the ones from the run on the left are drawn here."
          >
            streets from A
          </span>
        ) : null}
      </div>
    </div>
  );
}
