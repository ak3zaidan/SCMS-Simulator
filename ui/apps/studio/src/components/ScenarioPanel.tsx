/**
 * The scenario editor: the settings a run is started from, with help text, units, and — for every
 * field — the engine's own statement of whether it acts on it.
 *
 * # Applying an edit
 *
 * An edit here reaches the next run. **Apply** sends the edited document to `scenario.set`, which
 * runs it through the loader (the same code a scenario file goes through) and holds it for the next
 * `run.start`; **Run** applies any unapplied edit first, then starts a run, which builds a fresh
 * kernel on the held scenario. The run's `Hello` then carries the new scenario digest, the new run id
 * and the new world, so what the page shows is provably the edited run and not the old one.
 *
 * It did not use to be. `scenario.set` validated a document, answered with the hash of the scenario
 * the run already had, and threw the document away; `run.start` rewound the same scenario. This panel
 * said so, removed its Apply button and told the user to edit a file and restart the engine. That
 * was honest and it was the wrong product: the page is the simulator. The engine now keeps what it is
 * given, and this panel says which settings will change and which the engine does not act on.
 *
 * # Fields the engine does not act on
 *
 * The form is generated from the engine's published settings surface, and every row carries the
 * `x-status` the engine's `KEY_STATUS` table assigns it. A field the engine reads nothing from is
 * marked **not applied** with the engine's own sentence about it; a field it acts on only partly is
 * marked **partly applied**. Apply lists any edited field of either kind, so an edit that changes
 * nothing is never silently accepted.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ValidationError } from "@vwp/protocol";

import { EventsEditor } from "./EventsEditor.js";
import { Identifier } from "./Identifier.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import {
  PHASE1_FIELDS,
  changedPointers,
  fieldsFromJsonSchema,
  fieldsFromPublished,
  getPointer,
  groupsOf,
  setPointer,
  type FieldStatus,
  type FormField,
  type PublishedFormField,
} from "../lib/schema.js";

/** A field of either source, with a status when the engine published one. */
type Field = FormField & Partial<Pick<PublishedFormField, "status" | "statusNote" | "collection">>;

/** The words for a status, in the user's language. `wired` is the normal case and gets none. */
const STATUS_BADGE: Record<FieldStatus, { label: string; cls: string } | null> = {
  wired: null,
  partial: { label: "partly applied", cls: "warn" },
  "not-implemented": { label: "not applied", cls: "off" },
  refused: { label: "limited choices", cls: "info" },
  descriptive: { label: "description", cls: "" },
  unknown: { label: "unclassified", cls: "off" },
};

function fieldId(field: FormField): string {
  return `f${field.pointer.replace(/\W/g, "_")}`;
}

function Widget({
  field,
  value,
  onChange,
}: {
  field: Field;
  value: unknown;
  onChange: (v: unknown) => void;
}): React.JSX.Element {
  const id = fieldId(field);
  if (field.kind === "boolean") {
    return (
      <input id={id} type="checkbox" checked={value === true} onChange={(e) => onChange(e.target.checked)} style={{ width: "auto" }} />
    );
  }
  if (field.kind === "enum") {
    const options = field.options ?? [];
    const current = value === undefined || value === null ? "" : String(value);
    return (
      <select id={id} value={current} onChange={(e) => onChange(e.target.value)}>
        {options.includes(current) ? null : <option value={current}>{current || "—"}</option>}
        {options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    );
  }
  // The master seed is an integer the document writes as a hexadecimal string, and a number input
  // cannot hold "0x…": a numeric kind with a string value is edited as text.
  if ((field.kind === "number" || field.kind === "integer") && typeof value !== "string") {
    return (
      <input
        id={id}
        type="number"
        value={typeof value === "number" ? value : ""}
        min={field.min}
        max={field.max}
        step={field.step ?? (field.kind === "integer" ? 1 : "any")}
        onChange={(e) => onChange(e.target.value === "" ? undefined : Number(e.target.value))}
      />
    );
  }
  if (field.kind === "json") {
    return <JsonWidget id={id} value={value} onChange={onChange} />;
  }
  return <input id={id} type="text" value={value === undefined || value === null ? "" : String(value)} onChange={(e) => onChange(e.target.value)} />;
}

/**
 * A JSON value, edited as text. The text is kept while it does not parse, so a half-typed list is
 * not thrown away; the document gets the value only once it is valid JSON.
 */
function JsonWidget({ id, value, onChange }: { id: string; value: unknown; onChange: (v: unknown) => void }): React.JSX.Element {
  const serialised = value === undefined ? "" : JSON.stringify(value);
  const [text, setText] = useState(serialised);
  const [bad, setBad] = useState(false);
  useEffect(() => {
    setText(serialised);
    setBad(false);
  }, [serialised]);
  return (
    <>
      <textarea
        id={id}
        rows={Math.min(6, Math.max(1, Math.ceil(serialised.length / 48)))}
        value={text}
        className={bad ? "bad" : undefined}
        onChange={(e) => {
          setText(e.target.value);
          if (e.target.value.trim() === "") {
            setBad(false);
            onChange(undefined);
            return;
          }
          try {
            onChange(JSON.parse(e.target.value));
            setBad(false);
          } catch {
            setBad(true);
          }
        }}
      />
      {bad ? <div className="help err-text">Not valid JSON yet — the setting keeps its last valid value.</div> : null}
    </>
  );
}

function Errors({ items, kind }: { items: readonly ValidationError[]; kind: "err" | "warn" }): React.JSX.Element | null {
  if (items.length === 0) return null;
  return (
    <div className={`note ${kind === "err" ? "err" : ""}`} data-testid={`validation-${kind}`}>
      {items.map((e, i) => (
        <div key={`${e.path}-${i}`}>
          <code>{e.path}</code> — {e.message}
          {e.hint ? <span className="faint"> ({e.hint})</span> : null}
        </div>
      ))}
    </div>
  );
}

function StatusBadge({ field }: { field: Field }): React.JSX.Element | null {
  if (!field.status) return null;
  const badge = STATUS_BADGE[field.status];
  if (!badge) return null;
  return (
    <span className={`field-status ${badge.cls}`} title={field.statusNote} data-testid="field-status" data-status={field.status}>
      {badge.label}
    </span>
  );
}

/** Groups open when the panel first shows: the ones a run is usually changed in. */
const OPEN_GROUPS = new Set(["Run", "Traffic", "Radio", "Messages", "Timeline"]);

export function ScenarioPanel(): React.JSX.Element {
  const scenario = useStudio((s) => s.scenario);
  const scenarioHash = useStudio((s) => s.scenarioHash);
  const schema = useStudio((s) => s.scenarioSchema);
  const extras = useStudio((s) => s.scenarioExtras);
  const list = useStudio((s) => s.scenarioList);
  const validation = useStudio((s) => s.validation);
  const hello = useStudio((s) => s.hello);
  const devDetails = useStudio((s) => s.devDetails);
  const run = useStudio((s) => s.run);
  const [draft, setDraft] = useState<Record<string, unknown> | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{ text: string; tone: "info" | "err" | "warn" } | null>(null);
  const [filter, setFilter] = useState("");

  // A fresh copy of the engine's scenario replaces the form only when the form holds no edits.
  // It is re-fetched on every connect, every new run and every reconnect, and it used to replace
  // the draft unconditionally — so an edit made while a fetch was in flight vanished, and Apply
  // went grey under the user's cursor. Edits are rebased onto the new document instead.
  const previous = useRef<unknown>(scenario);
  useEffect(() => {
    const before = previous.current;
    previous.current = scenario;
    setDraft((current) => {
      const next = (scenario ?? null) as Record<string, unknown> | null;
      // Edits typed before the engine's document first arrived (into the built-in field list) are
      // edits against nothing, so every leaf of them is the user's.
      if (current === null) return next;
      const mine = changedPointers(before ?? {}, current);
      if (mine.length === 0 || next === null) return next;
      let rebased: Record<string, unknown> = next;
      for (const pointer of mine) rebased = setPointer(rebased, pointer, getPointer(current, pointer));
      return rebased;
    });
  }, [scenario]);

  const published = useMemo(
    () => (extras.fields.length > 0 ? fieldsFromPublished(extras.fields, schema, scenario) : []),
    [extras.fields, schema, scenario],
  );
  const generated = useMemo(() => (published.length === 0 && schema ? fieldsFromJsonSchema(schema) : []), [published, schema]);
  const fields: readonly Field[] = published.length > 0 ? published : generated.length > 0 ? generated : PHASE1_FIELDS;
  const needle = filter.trim().toLowerCase();
  const shown = needle === ""
    ? fields
    : fields.filter((f) => `${f.label} ${f.pointer} ${f.help ?? ""}`.toLowerCase().includes(needle));
  const groups = useMemo(() => groupsOf(shown), [shown]);
  const edits = useMemo(() => (draft === null ? [] : changedPointers(scenario, draft)), [scenario, draft]);
  const dirty = edits.length > 0;
  const staged = extras.staged;

  /** Edited fields the engine will not act on, by the status it published for them. */
  const inert = useMemo(() => {
    if (!dirty) return [];
    return fields.filter(
      (f) =>
        (f.status === "not-implemented" || f.status === "unknown") &&
        edits.some((p) => p === f.pointer || p.startsWith(`${f.pointer}/`)),
    );
  }, [dirty, edits, fields]);

  const edit = useCallback((pointer: string, value: unknown) => {
    setDraft((prev) => setPointer<Record<string, unknown>>(prev ?? {}, pointer, value));
  }, []);

  const runAction = useCallback(async (name: string, fn: () => Promise<string>) => {
    setBusy(name);
    setMessage(null);
    try {
      setMessage({ text: await fn(), tone: "info" });
    } catch (err) {
      // The engine's own words are kept, because they are what says *why* — prefixed with which
      // button did not work, in the user's words rather than a method name.
      setMessage({ text: `${name} did not work: ${err instanceof Error ? err.message : String(err)}`, tone: "err" });
    } finally {
      setBusy(null);
    }
  }, []);

  /** Send the draft to the engine for the next run. Returns a sentence, or throws with the reasons. */
  const apply = useCallback(async (): Promise<string> => {
    if (draft === null) return "There is nothing to apply.";
    const res = await engine.request("scenario.set", { scenario: draft, validate: false });
    useStudio.getState().setValidation({ valid: res.valid, errors: res.errors ?? [], warnings: [] });
    if (!res.valid) {
      const n = (res.errors ?? []).length;
      throw new Error(`the engine refused ${n} setting${n === 1 ? "" : "s"} — see Check, below. Nothing was changed.`);
    }
    await engine.refreshScenario();
    const changed = res.requires_restart ?? [];
    const inertNote =
      inert.length > 0
        ? ` ${inert.length} of your edits (${inert.map((f) => f.label).join(", ")}) change nothing in this build — see the marks on those fields.`
        : "";
    return changed.length === 0
      ? `These are the settings of the run already on screen; there is nothing to change.${inertNote}`
      : `Applied ${changed.length} change${changed.length === 1 ? "" : "s"}. The next run uses them — press Run.${inertNote}`;
  }, [draft, inert]);

  return (
    <>
      <div className="panel-body" data-testid="scenario-panel">
        <div className="section">
          <h3>Settings</h3>
          <div className={published.length > 0 || generated.length > 0 ? "note info" : "note"} data-testid="schema-source">
            {published.length > 0 ? (
              <>
                These {published.length} settings are the ones this engine accepts, with what it does with each.
                Settings marked <span className="field-status off">not applied</span> are accepted and recorded
                but change nothing in this build; <span className="field-status warn">partly applied</span> ones
                say which part the engine uses.
                {devDetails ? (
                  <span className="faint"> From its published settings surface, via <code>scenario.get {"{with_schema:true}"}</code>.</span>
                ) : null}
              </>
            ) : generated.length > 0 ? (
              <>These {generated.length} fields are the ones this engine published.</>
            ) : (
              <>
                These are the standard scenario fields. This engine did not publish its own list, so it may accept
                settings that are not shown here, or ignore some that are. Press <b>Check</b> below and it will say which.
              </>
            )}
          </div>
          <dl className="kv">
            <dt>scenario</dt>
            <dd>{hello?.scenarioName ?? "—"}</dd>
            <dt>identity</dt>
            <dd>
              <Identifier value={scenarioHash} label="scenario digest" testId="scenario-hash" />
            </dd>
            {run.outputDigest ? (
              <>
                <dt>result</dt>
                <dd title="A digest of everything the finished run produced. Two runs with the same settings and seed give the same one.">
                  <Identifier value={run.outputDigest} label="output digest" testId="output-digest" />
                </dd>
              </>
            ) : null}
          </dl>
          {staged ? (
            <div className="note warn-note" data-testid="staged-note">
              {staged.changed.length} setting{staged.changed.length === 1 ? "" : "s"} applied and waiting for the next run:{" "}
              <span className="faint">{staged.changed.slice(0, 6).join(", ")}{staged.changed.length > 6 ? ", …" : ""}</span>. Press <b>Run</b> to start it.
              <div className="row" style={{ marginTop: 4 }}>
                <button
                  type="button"
                  disabled={busy !== null}
                  data-testid="unstage"
                  title="Forget the applied settings; the next run uses the scenario on screen now"
                  onClick={() =>
                    void runAction("Revert", async () => {
                      await engine.request("scenario.load", { path: extras.runningHash, validate: true });
                      await engine.refreshScenario();
                      return "The applied settings were withdrawn. The next run is the scenario on screen.";
                    })
                  }
                >
                  Revert to the running scenario
                </button>
              </div>
            </div>
          ) : null}
        </div>

        {list.filter((i) => i.kind === "preset").length > 0 ? (
          <div className="section" data-testid="presets">
            <h3>Ready-made scenarios</h3>
            {list
              .filter((i) => i.kind === "preset")
              .map((item) => (
                <div key={item.id} className="row" style={{ justifyContent: "space-between" }}>
                  <span title={item.description}>
                    {item.name ?? item.id}
                    {list.filter((o) => o.kind === "preset" && o.name === item.name).length > 1 ? (
                      <span className="faint"> ({item.id.split("/").pop()})</span>
                    ) : null}
                    {(item as { running?: boolean }).running ? <span className="faint"> (running)</span> : null}
                  </span>
                  <button
                    type="button"
                    disabled={busy !== null}
                    data-testid="preset-load"
                    data-preset={item.name ?? item.id}
                    title="Load these settings into the form. The next run uses them."
                    onClick={() =>
                      void runAction("Load", async () => {
                        const res = await engine.request("scenario.load", { path: item.id, validate: true });
                        await engine.refreshScenario();
                        if (!res.valid) {
                          useStudio.getState().setValidation({ valid: false, errors: res.errors ?? [], warnings: [] });
                          return `${item.name ?? item.id} does not load: see Check, below.`;
                        }
                        return res.hash === extras.runningHash
                          ? "That is the scenario the engine is running now."
                          : `Loaded ${item.name ?? item.id}. Press Run to start it.`;
                      })
                    }
                  >
                    load
                  </button>
                </div>
              ))}
          </div>
        ) : null}

        <div className="section">
          <input
            type="search"
            placeholder="Find a setting…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            data-testid="settings-filter"
            aria-label="Find a setting"
          />
        </div>

        {groups.map((group) => (
          <details className="section settings-group" key={group} open={needle !== "" || OPEN_GROUPS.has(group)}>
            <summary>
              <h3 style={{ display: "inline" }}>{group}</h3>
            </summary>
            {shown
              .filter((f) => f.group === group)
              .map((f) => {
                const inactive = f.status === "not-implemented" || f.status === "unknown";
                const edited = edits.some((p) => p === f.pointer || p.startsWith(`${f.pointer}/`));
                return (
                  <div
                    className={`field${inactive ? " inactive" : ""}${edited ? " edited" : ""}`}
                    key={f.pointer}
                    data-testid="setting"
                    data-pointer={f.pointer}
                  >
                    <label htmlFor={fieldId(f)}>
                      {f.label} {f.unit ? <span className="unit">[{f.unit}]</span> : null} <StatusBadge field={f} />
                    </label>
                    {f.pointer === "/events" ? (
                      <EventsEditor
                        value={getPointer(draft, f.pointer)}
                        onChange={(v) => edit(f.pointer, v)}
                        durationS={(() => {
                          const d = getPointer(draft, "/time/duration_s");
                          return typeof d === "number" ? d : undefined;
                        })()}
                      />
                    ) : (
                      <Widget field={f} value={getPointer(draft, f.pointer)} onChange={(v) => edit(f.pointer, v)} />
                    )}
                    {f.help ? <div className="help">{f.help}</div> : null}
                    {f.status && f.status !== "wired" && f.statusNote ? (
                      <div className="help status-note">{f.statusNote}</div>
                    ) : null}
                    {devDetails ? <div className="help faint"><code>{f.pointer}</code></div> : null}
                  </div>
                );
              })}
          </details>
        ))}

        {dirty ? (
          <div className="section" data-testid="edit-route">
            <h3>Your edits</h3>
            <div className="note">
              {edits.length} setting{edits.length === 1 ? "" : "s"} edited. <b>Apply</b> sends them to the engine for the
              next run; <b>Run</b> applies them and starts it.
              {inert.length > 0 ? (
                <div className="err-text" data-testid="inert-edits">
                  {inert.length === 1 ? "One edit changes" : `${inert.length} edits change`} nothing in this build:{" "}
                  {inert.map((f) => f.label).join(", ")}.
                </div>
              ) : null}
            </div>
          </div>
        ) : null}

        {validation ? (
          <div className="section">
            <h3>Check</h3>
            <div className={`pill ${validation.valid ? "ok" : "err"}`} data-testid="validation-state">
              {validation.valid ? "ready to run" : "needs fixing"}
            </div>
            {validation.valid && validation.warnings.length === 0 ? (
              <p className="help">The engine accepts these settings.</p>
            ) : null}
            <Errors items={validation.errors} kind="err" />
            <Errors items={validation.warnings} kind="warn" />
          </div>
        ) : null}

        {message ? (
          <div className={`note ${message.tone === "err" ? "err" : "info"}`} data-testid="scenario-message">
            {message.text}
          </div>
        ) : null}
      </div>

      <div className="panel-foot">
        <button
          type="button"
          disabled={busy !== null}
          data-testid="validate"
          title="Ask the engine whether it accepts these settings, without changing anything"
          onClick={() =>
            void runAction("Check", async () => {
              const res = await engine.request("scenario.validate", {
                ...(draft ? { scenario: draft } : {}),
                strict: false,
              });
              useStudio.getState().setValidation({ valid: res.valid, errors: res.errors, warnings: res.warnings });
              return res.valid
                ? "The engine accepts these settings."
                : `${res.errors.length} setting${res.errors.length === 1 ? "" : "s"} need fixing — see Check, above.`;
            })
          }
        >
          {busy === "Check" ? "checking…" : "Check"}
        </button>
        <button
          type="button"
          disabled={busy !== null || !dirty}
          data-testid="apply"
          title="Send the edited settings to the engine. The next run uses them."
          onClick={() => void runAction("Apply", apply)}
        >
          {busy === "Apply" ? "applying…" : "Apply"}
        </button>
        <button
          type="button"
          disabled={busy !== null || !dirty}
          title="Put the form back to the settings the engine holds"
          onClick={() => {
            setDraft((scenario ?? null) as Record<string, unknown> | null);
            setMessage({ text: "Edits discarded. The form shows the engine's settings again.", tone: "info" });
          }}
          data-testid="discard-edits"
        >
          Discard
        </button>
        <span className="grow" />
        <button
          type="button"
          className="primary"
          disabled={busy !== null}
          data-testid="run-start"
          title={dirty ? "Apply your edits and start a run with them" : "Start the scenario from the beginning"}
          onClick={() =>
            void runAction("Run", async () => {
              let applied = "";
              if (dirty) applied = `${await apply()} `;
              const state = await engine.startRun();
              const now = useStudio.getState().run;
              return `${applied.replace(" — press Run.", ".")}${state === "running" ? "Running" : `Started; the engine reports it is ${state}`} — ${Math.round(now.tEndNs / 1e9)} s of simulated time.`;
            })
          }
        >
          {busy === "Run" ? "starting…" : "Run"}
        </button>
      </div>
    </>
  );
}
