/**
 * The viewport: one canvas, one `@vwp/viewer` scene, and the toolbar of 09-ui §6
 * (`[overlays ▾] [camera ▾] [follow: …]`).
 *
 * The click handler is the signature interaction of 09-ui §1.2/§3 — click a vehicle on the top-down
 * map and the camera flies down into a chase view of it. `Viewer.pickAtPixel` resolves the hit and
 * `engine.selectActor` does the rest: `viewer.flyTo`, plus `view.follow` (§6.7), which is also what
 * subscribes the node to `Telemetry` frames and therefore what populates the OBU HUD.
 */

import { useCallback, useEffect, useRef } from "react";
import { CAMERA_MODES, type CameraMode } from "@vwp/viewer";

import { ObuHud } from "./ObuHud.js";
import { OverlayMenu } from "./OverlayMenu.js";
import { StateLegend } from "./StateLegend.js";
import { StatsChip } from "./StatsReadout.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";

/** A handle for Playwright and for notebooks driving the Studio (09-ui §8). */
declare global {
  interface Window {
    __vwpStudio?: {
      engine: typeof engine;
      selectFirstActor: () => number | null;
      fps: () => number;
      actorCount: () => number;
    };
  }
}

export function Viewport(): React.JSX.Element {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const theme = useStudio((s) => s.theme);
  const cameraMode = useStudio((s) => s.cameraMode);
  const selectedActor = useStudio((s) => s.selectedActor);
  const selectedNode = useStudio((s) => s.selectedNode);
  const world = useStudio((s) => s.world);
  const hudDocked = useStudio((s) => s.hudDocked);
  const setHudDocked = useStudio((s) => s.setHudDocked);

  useEffect(() => {
    const canvas = canvasRef.current;
    const host = hostRef.current;
    if (!canvas || !host) return;

    // Idempotent: `Viewer.mount` returns early for a canvas it already owns, which is what makes
    // React 19's StrictMode double-invoke harmless. The viewer is deliberately *not* disposed on
    // cleanup — it outlives this component and belongs to the engine.
    const viewer = engine.mountViewer(canvas, theme);
    engine.attachViewer();
    const detachInput = viewer.cameras.attachInput(canvas);

    const observer = new ResizeObserver(() => {
      viewer.resize(host.clientWidth, host.clientHeight);
    });
    observer.observe(host);
    viewer.resize(host.clientWidth, host.clientHeight);

    window.__vwpStudio = {
      engine,
      selectFirstActor: () => {
        const client = engine.client;
        if (!client) return null;
        const poses = client.poses;
        for (let slot = 0; slot < poses.count; slot++) {
          if (poses.occupied[slot] === 1) {
            const id = poses.actorId[slot];
            void engine.selectActor(id, "chase");
            return id;
          }
        }
        return null;
      },
      fps: () => engine.viewer?.stats.snapshot().fps ?? 0,
      actorCount: () => engine.client?.poses.count ?? 0,
    };

    return () => {
      observer.disconnect();
      detachInput();
    };
    // `theme` is applied through its own effect; re-running this one would re-create the renderer.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    engine.viewer?.setTheme(theme);
  }, [theme]);

  const onClick = useCallback((ev: React.MouseEvent<HTMLCanvasElement>) => {
    const viewer = engine.viewer;
    if (!viewer) return;
    const rect = ev.currentTarget.getBoundingClientRect();
    const hit = viewer.pickAtPixel(ev.clientX - rect.left, ev.clientY - rect.top);
    if (!hit) {
      void engine.selectActor(null);
      return;
    }
    if (hit.kind === "actor") {
      void engine.selectActor(hit.actorId, "chase");
    } else if (hit.kind === "site") {
      void engine.selectNode(hit.nodeId);
    }
  }, []);

  const onKeyDown = useCallback((ev: React.KeyboardEvent<HTMLCanvasElement>) => {
    // 09-ui §10 — keyboard control for the camera.
    const index = CAMERA_MODES.indexOf(useStudio.getState().cameraMode);
    if (ev.key === "[" || ev.key === "]") {
      const next = CAMERA_MODES[(index + (ev.key === "]" ? 1 : CAMERA_MODES.length - 1)) % CAMERA_MODES.length];
      engine.setCameraMode(next);
      ev.preventDefault();
    } else if (ev.key === "Escape") {
      void engine.selectActor(null);
    }
  }, []);

  const followLabel = selectedNode !== null ? engine.nodes.get(selectedNode)?.label ?? `node ${selectedNode}` : null;

  return (
    <div className="viewport" ref={hostRef} data-testid="viewport">
      <canvas
        ref={canvasRef}
        data-testid="viewer-canvas"
        tabIndex={0}
        onClick={onClick}
        onKeyDown={onKeyDown}
        aria-label="2D/3D viewport"
      />

      <div className="viewport-toolbar">
        <OverlayMenu />

        <div className="menu">
          <select
            className="chip"
            data-testid="camera-mode"
            value={cameraMode}
            onChange={(e) => engine.setCameraMode(e.target.value as CameraMode)}
            aria-label="Camera mode"
          >
            {CAMERA_MODES.map((m) => (
              <option key={m} value={m}>
                camera: {m}
              </option>
            ))}
          </select>
        </div>

        <span className="chip" data-testid="follow-chip">
          follow: {followLabel ?? (selectedActor !== null ? `actor ${selectedActor}` : "—")}
        </span>

        <span className="spacer grow" />

        <span className="chip" title="World build report from @vwp/viewer">
          {world ? `${world.lanes} lanes · ${world.buildings} buildings · ${world.sites} RSUs` : "world loading…"}
        </span>
        <StatsChip />
        <button type="button" className="chip" onClick={() => setHudDocked(!hudDocked)} data-testid="hud-dock">
          HUD {hudDocked ? "float" : "dock"}
        </button>
      </div>

      {/*
        There is no status note floating here any more. It said "Stream is closed. The viewport
        shows the last decoded state." — the connection token, with no reason and no action, over
        the picture. The same state is now one sentence with its button in `StatusBanner`, in the
        flow above the viewport where it does not cover anything.
      */}

      <StateLegend />
      {hudDocked ? null : <ObuHud />}
    </div>
  );
}
