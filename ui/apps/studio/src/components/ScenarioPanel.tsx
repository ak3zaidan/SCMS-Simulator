/**
 * The scenario editor of 09-ui §6: "generated from the JSON Schema (03-interfaces §13) with help
 * text and units per field, presets, … validation with clickable errors".
 *
 * The form is generated, not hand-written: `scenario.get {with_schema:true}` (§6.10) is asked for
 * `scenario-1.json` and the widgets are flattened out of it. When the engine publishes no schema the
 * panel falls back to the Phase 1 field list and says so, so nobody mistakes the fallback for the
 * engine's own contract. Edits are held as a draft; `validate` sends `scenario.validate`, `apply`
 * sends `scenario.set`, `run` sends `run.start` with the draft (§6.6).
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import type { ValidationError } from "@vwp/protocol";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { PHASE1_FIELDS, fieldsFromJsonSchema, getPointer, groupsOf, setPointer, type FormField } from "../lib/schema.js";

function Widget({
  field,
  value,
  onChange,
}: {
  field: FormField;
  value: unknown;
  onChange: (v: unknown) => void;
}): React.JSX.Element {
  const id = `f${field.pointer.replace(/\W/g, "_")}`;
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
  if (field.kind === "number" || field.kind === "integer") {
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
    return (
      <textarea
        id={id}
        rows={2}
        value={value === undefined ? "" : JSON.stringify(value)}
        onChange={(e) => {
          try {
            onChange(JSON.parse(e.target.value));
          } catch {
            onChange(e.target.value);
          }
        }}
      />
    );
  }
  return <input id={id} type="text" value={value === undefined || value === null ? "" : String(value)} onChange={(e) => onChange(e.target.value)} />;
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

export function ScenarioPanel(): React.JSX.Element {
  const scenario = useStudio((s) => s.scenario);
  const scenarioHash = useStudio((s) => s.scenarioHash);
  const schema = useStudio((s) => s.scenarioSchema);
  const list = useStudio((s) => s.scenarioList);
  const validation = useStudio((s) => s.validation);
  const hello = useStudio((s) => s.hello);
  const [draft, setDraft] = useState<Record<string, unknown> | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    setDraft((scenario ?? null) as Record<string, unknown> | null);
  }, [scenario]);

  const generated = useMemo(() => (schema ? fieldsFromJsonSchema(schema) : []), [schema]);
  const fields = generated.length > 0 ? generated : PHASE1_FIELDS;
  const groups = useMemo(() => groupsOf(fields), [fields]);
  const dirty = draft !== null && JSON.stringify(draft) !== JSON.stringify(scenario);

  const edit = useCallback((pointer: string, value: unknown) => {
    setDraft((prev) => setPointer<Record<string, unknown>>(prev ?? {}, pointer, value));
  }, []);

  const runAction = useCallback(
    async (name: string, fn: () => Promise<string>) => {
      setBusy(name);
      setMessage(null);
      try {
        setMessage(await fn());
      } catch (err) {
        setMessage(`${name} failed: ${err instanceof Error ? err.message : String(err)}`);
      } finally {
        setBusy(null);
      }
    },
    [],
  );

  return (
    <>
      <div className="panel-body" data-testid="scenario-panel">
        <div className="section">
          <h3>Source</h3>
          <div className={schema ? "note info" : "note"} data-testid="schema-source">
            {schema ? (
              <>
                Form generated from the engine&rsquo;s <code>scenario-1.json</code> ({generated.length} fields,
                via <code>scenario.get&nbsp;{"{with_schema:true}"}</code>).
              </>
            ) : (
              <>
                The engine published no schema on <code>scenario.get&nbsp;{"{with_schema:true}"}</code> (§6.10),
                so this form is the built-in Phase 1 field list from 03-interfaces §13.
              </>
            )}
          </div>
          <dl className="kv">
            <dt>hash</dt>
            <dd style={{ fontSize: 10 }}>{scenarioHash || "—"}</dd>
            <dt>name</dt>
            <dd>{hello?.scenarioName ?? "—"}</dd>
          </dl>
        </div>

        {list.length > 0 ? (
          <div className="section">
            <h3>Presets</h3>
            {list.map((item) => (
              <div key={item.id} className="row" style={{ justifyContent: "space-between" }}>
                <span title={item.description}>{item.name ?? item.id}</span>
                <button
                  type="button"
                  onClick={() =>
                    void runAction("scenario.load", async () => {
                      const res = await engine.request("scenario.load", { path: item.id, validate: true });
                      useStudio.getState().setScenario(res.scenario, res.hash, schema);
                      return `loaded ${item.id}`;
                    })
                  }
                >
                  load
                </button>
              </div>
            ))}
          </div>
        ) : null}

        {groups.map((group) => (
          <div className="section" key={group}>
            <h3>{group}</h3>
            {fields
              .filter((f) => f.group === group)
              .map((f) => (
                <div className="field" key={f.pointer}>
                  <label htmlFor={`f${f.pointer.replace(/\W/g, "_")}`}>
                    {f.label} {f.unit ? <span className="unit">[{f.unit}]</span> : null}
                  </label>
                  <Widget field={f} value={getPointer(draft, f.pointer)} onChange={(v) => edit(f.pointer, v)} />
                  {f.help ? <div className="help">{f.help}</div> : null}
                </div>
              ))}
          </div>
        ))}

        {validation ? (
          <div className="section">
            <h3>Validation</h3>
            <div className={`pill ${validation.valid ? "ok" : "err"}`} data-testid="validation-state">
              {validation.valid ? "valid" : "invalid"}
            </div>
            <Errors items={validation.errors} kind="err" />
            <Errors items={validation.warnings} kind="warn" />
          </div>
        ) : null}

        {message ? <div className="note info" data-testid="scenario-message">{message}</div> : null}
      </div>

      <div className="panel-foot">
        <button
          type="button"
          disabled={busy !== null}
          data-testid="validate"
          onClick={() =>
            void runAction("scenario.validate", async () => {
              const res = await engine.request("scenario.validate", {
                ...(draft ? { scenario: draft } : {}),
                strict: false,
              });
              useStudio.getState().setValidation({ valid: res.valid, errors: res.errors, warnings: res.warnings });
              return res.valid ? "scenario is valid" : `${res.errors.length} errors`;
            })
          }
        >
          {busy === "scenario.validate" ? "validating…" : "validate"}
        </button>
        <button
          type="button"
          disabled={busy !== null || !dirty}
          title="scenario.set — apply the draft to the engine (§6.10)"
          onClick={() =>
            void runAction("scenario.set", async () => {
              const res = await engine.request("scenario.set", { scenario: draft ?? {}, validate: true });
              return `applied, hash ${res.hash.slice(0, 12)}…`;
            })
          }
        >
          apply
        </button>
        <span className="grow" />
        <button
          type="button"
          className="primary"
          disabled={busy !== null}
          data-testid="run-start"
          onClick={() =>
            void runAction("run.start", async () => {
              const res = await engine.request("run.start", {
                ...(draft ? { scenario: draft } : {}),
              });
              void engine.refreshStatus();
              return `run ${res.run_id.slice(0, 8)}… ${res.state}`;
            })
          }
        >
          run
        </button>
      </div>
    </>
  );
}
