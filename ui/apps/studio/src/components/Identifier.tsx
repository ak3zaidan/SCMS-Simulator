/**
 * A long identifier — a run id, a world digest, an engine build — shown short and copied in full.
 *
 * A 64-character hex digest wrapped across two lines of a panel is not information, it is texture:
 * nobody reads it, and it pushes the things people do read off the screen. But it is not noise
 * either — checking that two runs were computed on the same world is exactly the kind of thing this
 * project exists for, and that check needs every character.
 *
 * So: the first eight characters on screen, the whole value in the clipboard on a click, and the
 * whole value in the tooltip and in the accessible name for anyone who cannot click. Nothing is
 * lost and nothing is shouted.
 */

import { useCallback, useEffect, useState } from "react";

export function Identifier({
  value,
  label,
  chars = 8,
  testId,
}: {
  value: string;
  /** What this identifies, for the accessible name: "world digest". */
  label: string;
  chars?: number;
  testId?: string;
}): React.JSX.Element {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1400);
    return () => clearTimeout(timer);
  }, [copied]);

  const copy = useCallback(() => {
    // `navigator.clipboard` is absent on an insecure origin and can reject when the page is not
    // focused; a failed copy must not look like a successful one.
    void navigator.clipboard
      ?.writeText(value)
      .then(() => setCopied(true))
      .catch(() => setCopied(false));
  }, [value]);

  if (value === "") return <span className="faint">—</span>;

  const short = value.length <= chars ? value : `${value.slice(0, chars)}…`;
  return (
    <button
      type="button"
      className="ident"
      onClick={copy}
      title={`${value}\n\nClick to copy the full ${label}.`}
      aria-label={`${label} ${value} — copy`}
      {...(testId ? { "data-testid": testId } : {})}
    >
      <span className="mono">{short}</span>
      <span className="ident-hint" aria-hidden="true">
        {copied ? "copied" : "copy"}
      </span>
    </button>
  );
}
