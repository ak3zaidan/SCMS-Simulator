/**
 * The scenario editor: the settings a run is started from, with help text and units per field.
 *
 * The form is generated, not hand-written. The engine is asked for its scenario schema and the
 * widgets are flattened out of it; when it publishes none, the standard field set is used instead.
 * That distinction used to be reported to the user as a sentence naming two specification sections
 * and an RPC method with its arguments — text nobody outside this repository could act on. It now
 * says which fields are on screen and what that means for them, and the protocol detail appears
 * only with developer details switched on.
 *
 * # What an edit here can and cannot do
 *
 * It can be checked, and it can be taken away. It cannot be applied, and the panel now says so
 * instead of claiming otherwise.
 *
 * The engine serves one scenario, fixed when the process started, and neither method that looks
 * like it would change that actually does. `scenario.set` validates the document, returns
 * `applied_live: []` and the hash of the scenario the run already had, and discards what it was
 * given; `run.start`'s `scenario` parameter is read by nobody — `rpc::run_start` reads `speed`,
 * `paused` and `seed` and nothing else. Both were verified against a running engine rather than
 * read off the source: `scenario.set` was sent a document with a different name and a duration of
 * 999 s, answered `valid: true` with the original hash, and a following `scenario.get` returned the
 * original document; `run.start` with the same document rewound the run and reported `t_end_ns`
 * still 60 s.
 *
 * So this panel had an Apply button that saved nothing and reported the unchanged hash as the
 * user's own, and a Run button that sent the edits to `scenario.set` on the way past and then
 * announced a run of settings the engine had thrown away. Apply is gone, Run says which settings it
 * ran, and an edit can be copied out to the scenario file — which is where a change has to go, and
 * the panel gives the command line that starts the engine on it.
 *
 * What remains is genuinely useful and genuinely honest: `scenario.validate` does validate the
 * document you hand it, so the form is a way to check an edit before writing it to the file.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import type { ValidationError } from "@vwp/protocol";

import { Identifier } from "./Identifier.js";
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
  const devDetails = useStudio((s) => s.devDetails);
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
        // The engine's own words are kept, because they are what says *why* — but prefixed with
        // which of the three buttons did not work, in the user's words rather than a method name.
        setMessage(`${name} did not work: ${err instanceof Error ? err.message : String(err)}`);
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
          <h3>Settings</h3>
          {/*
            Which of the two field lists this is, decided by whether the engine's schema actually
            produced any fields. Deciding it on `schema !== null` was wrong against this
            repository's own engine, which publishes a schema carrying `$id`, `type` and `required`
            and no `properties` at all: the form fell back to the built-in list, and the panel
            announced "These 0 fields are the ones this engine accepts" over sixteen fields the
            engine had never mentioned.
          */}
          <div className={generated.length > 0 ? "note info" : "note"} data-testid="schema-source">
            {generated.length > 0 ? (
              <>
                These {generated.length} fields are the ones this engine accepts — it published the list
                itself, so anything you can set here, it understands.
                {devDetails ? (
                  <span className="faint"> From its scenario schema, via <code>scenario.get {"{with_schema:true}"}</code>.</span>
                ) : null}
              </>
            ) : (
              <>
                These are the standard scenario fields. This engine did not publish its own list, so it may
                accept settings that are not shown here, or ignore some that are. Press <b>Check</b> below
                and it will say which.
                {devDetails ? (
                  <span className="faint">
                    {" "}
                    <code>scenario.get {"{with_schema:true}"}</code> returned no schema; the built-in field
                    list is in <code>lib/schema.ts</code>.
                  </span>
                ) : null}
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
          </dl>
        </div>

        {list.length > 0 ? (
          <div className="section">
            <h3>Ready-made scenarios</h3>
            {list.map((item) => (
              <div key={item.id} className="row" style={{ justifyContent: "space-between" }}>
                <span title={item.description}>{item.name ?? item.id}</span>
                <button
                  type="button"
                  onClick={() =>
                    void runAction("Open", async () => {
                      const res = await engine.request("scenario.load", { path: item.id, validate: true });
                      const same = res.hash === scenarioHash;
                      useStudio.getState().setScenario(res.scenario, res.hash, schema);
                      // The engine lists the scenario the run is already on among its presets, so
                      // "opened" would be a claim that something changed when nothing did.
                      return same
                        ? `That is the scenario this engine is already running.`
                        : `Showing ${item.name ?? item.id}. Note that this engine runs the scenario it was started on; see Applying an edit, above.`;
                    })
                  }
                >
                  open
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

        {/*
          Shown only once something has been edited, because until then there is nothing to explain
          and the panel should not open with a paragraph about its own limitations.
        */}
        {dirty ? (
          <div className="section" data-testid="edit-route">
            <h3>Applying an edit</h3>
            <div className="note warn-note">
              This engine cannot take an edited scenario while it is running: it serves the one it was
              started on, and the run id, the world and the node table are fixed at that moment. Press
              <b> Check</b> to have it validate the edit, then copy the settings into your scenario file and
              start the engine on that file.
            </div>
            <div className="row">
              <button
                type="button"
                disabled={busy !== null}
                data-testid="copy-scenario"
                title="Put the edited settings on the clipboard, ready to paste into a scenario file"
                onClick={() =>
                  void runAction("Copy", async () => {
                    const text = JSON.stringify(draft ?? {}, null, 2);
                    await navigator.clipboard.writeText(text);
                    return `Copied ${text.length.toLocaleString("en-US")} characters of JSON. Paste it into a scenario file and start the engine on it.`;
                  })
                }
              >
                Copy settings as JSON
              </button>
            </div>
            <p className="help">
              Then, in a terminal: <code>v2xw-server --scenario your-scenario.yaml --port 8787 --paused</code>.
              Starting it <code>--paused</code> means it waits for you instead of running to the end while you
              open this page.
            </p>
          </div>
        ) : null}

        {validation ? (
          <div className="section">
            <h3>Check</h3>
            <div className={`pill ${validation.valid ? "ok" : "err"}`} data-testid="validation-state">
              {validation.valid ? "ready to run" : "needs fixing"}
            </div>
            {validation.valid && validation.warnings.length === 0 ? (
              <p className="help">The engine accepts these settings. Press Run to start.</p>
            ) : null}
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
          title="Ask the engine whether it accepts these settings, without starting anything"
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
        {/*
          There is no Apply button, because there is nothing behind one. `scenario.set` validates
          the document, answers with the hash of the scenario the run already had, and throws away
          what it was given — see the note this panel shows while a draft is dirty, and the header
          comment for the check that proved it. A button reporting "Saved" over that was the worst
          thing on this panel.
        */}
        <button
          type="button"
          disabled={busy !== null || !dirty}
          title="Put these settings back to what the engine is running"
          onClick={() => {
            setDraft((scenario ?? null) as Record<string, unknown> | null);
            setMessage("Edits discarded. The form shows the engine's own settings again.");
          }}
          data-testid="discard-edits"
        >
          Discard edits
        </button>
        <span className="grow" />
        <button
          type="button"
          className="primary"
          disabled={busy !== null}
          data-testid="run-start"
          title={
            dirty
              ? "Rewind and run again. The engine runs the scenario it was started on, not the edits on this form."
              : "Rewind and run the scenario again from the beginning"
          }
          onClick={() =>
            void runAction("Run", async () => {
              // The draft is deliberately *not* sent. `run.start`'s `scenario` parameter is read by
              // nobody in `rpc::run_start`, and `scenario.set` discards what it is given, so either
              // call would be a gesture whose only effect is to make the message below a lie.
              const state = await engine.startRun();
              const ran = state === "running" ? "Running from the start." : `Started; the engine reports it is ${state}.`;
              return dirty
                ? `${ran} It is running the engine's own settings — the edits on this form are not in it, and cannot be. See Applying an edit, above.`
                : `${ran} Watch the viewport.`;
            })
          }
        >
          {busy === "Run" ? "starting…" : "Run"}
        </button>
      </div>
    </>
  );
}
