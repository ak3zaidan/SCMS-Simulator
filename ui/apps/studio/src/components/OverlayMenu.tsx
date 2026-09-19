/**
 * The `[overlays ▾]` menu of the 09-ui §6 wireframe.
 *
 * The list is the viewer's own catalogue (`OverlayManager.catalogue()`, which is the
 * `overlay.set {list:true}` shape of §6.7) merged with the engine's, so an overlay the engine knows
 * about but this build cannot draw is shown as unavailable rather than quietly missing. Ground-truth
 * overlays are labelled `GT` and can be locked off for blind evaluation (09-ui §6).
 */

import { useEffect, useRef, useState } from "react";
import { overlayLabel } from "@vwp/viewer";
import type { OverlayName } from "@vwp/protocol";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";

export function OverlayMenu(): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const overlays = useStudio((s) => s.overlays);
  const serverOverlays = useStudio((s) => s.serverOverlays);
  const gtLocked = useStudio((s) => s.groundTruthLocked);
  const ref = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (ev: MouseEvent): void => {
      if (ref.current && !ref.current.contains(ev.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const catalogue = engine.overlayCatalogue();
  const drawable = catalogue.filter((c) => c.available);
  const notDrawable = catalogue.filter((c) => !c.available);
  const onCount = Object.values(overlays).filter(Boolean).length;

  const serverOnly = serverOverlays.filter((s) => !catalogue.some((c) => c.name === s.name));

  return (
    <div className="menu" ref={ref}>
      <button type="button" className="chip" onClick={() => setOpen((v) => !v)} data-testid="overlays-button">
        overlays ▾ <span className="dim">{onCount}</span>
      </button>
      {open ? (
        <div className="menu-pop" data-testid="overlays-menu">
          <label>
            <input type="checkbox" checked={gtLocked} onChange={(e) => engine.lockGroundTruth(e.target.checked)} />
            <span>Lock ground truth off (blind evaluation)</span>
          </label>
          <div className="sec">Available</div>
          {drawable.map((entry) => (
            <label key={entry.name} data-testid={`overlay-${entry.name}`}>
              <input
                type="checkbox"
                checked={overlays[entry.name as OverlayName] === true}
                disabled={gtLocked && entry.groundTruth}
                onChange={(e) => void engine.setOverlay(entry.name as OverlayName, e.target.checked)}
              />
              <span>{overlayLabel(entry.name as OverlayName)}</span>
              {entry.groundTruth ? <span className="gt-tag">GT</span> : null}
            </label>
          ))}
          {notDrawable.length > 0 ? (
            <>
              <div className="sec">Not implemented in this build</div>
              {notDrawable.map((entry) => (
                <label key={entry.name} className="disabled">
                  <input type="checkbox" checked={false} disabled readOnly />
                  <span>{overlayLabel(entry.name as OverlayName)}</span>
                  {entry.groundTruth ? <span className="gt-tag">GT</span> : null}
                </label>
              ))}
            </>
          ) : null}
          {serverOnly.length > 0 ? (
            <>
              <div className="sec">Engine offers, viewer cannot draw</div>
              {serverOnly.map((entry) => (
                <label key={entry.name} className="disabled" title={entry.description}>
                  <input type="checkbox" checked={false} disabled readOnly />
                  <span>{entry.name}</span>
                </label>
              ))}
            </>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
