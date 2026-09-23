/**
 * The inspector's "why" tab (09-ui §5, protocol §3.8 and §6.9).
 *
 * Resolution order, and it matters:
 *  1. If the displayed value carries a `prov_id` on the wire — metric samples (§3.7) and several
 *     event payloads do — it is resolved locally out of the `Provenance` frames (§3.8) into
 *     (model id, model version, parameter-set id) with a link to the model card. No round trip.
 *  2. Otherwise the tab says so, in those words, and offers `explain` (§6.9), which resolves the
 *     same thing server-side for values the binary stream does not tag.
 *
 * `NodeTelemetry` fields are case 2: §3.5.2 has no `prov_id` column, so every HUD value lands here
 * with "no provenance id is carried for this value" rather than a blank panel.
 */

import { useCallback, useEffect, useState } from "react";
import type { ExplainResult, ProvenanceInfo } from "@vwp/protocol";

import { engine } from "../state/engine.js";
import { useStudio, type ProvEntry } from "../state/store.js";
import { PROV_SUBJECT_KINDS } from "../lib/format.js";
import type { ClientProvenance } from "../lib/provenance.js";

/** The model-card families of 03-interfaces §12, in the order the schema lists them. */
const FAMILIES = [
  "radio", "mobility", "network", "security", "protocol", "detection", "node", "backend", "world",
  "weather", "metric", "other",
] as const;

function ProvCard({ entry }: { entry: ProvEntry }): React.JSX.Element {
  return (
    <div className="section" data-testid="prov-card">
      <dl className="kv">
        <dt>prov_id</dt>
        <dd>{entry.provId}</dd>
        <dt>model</dt>
        <dd data-testid="prov-model-id">{entry.modelId || "—"}</dd>
        <dt>version</dt>
        <dd data-testid="prov-model-version">{entry.modelVersion || "—"}</dd>
        <dt>parameter set</dt>
        <dd data-testid="prov-param-set">{entry.paramSetId || "—"}</dd>
        <dt>family</dt>
        <dd>{FAMILIES[entry.family] ?? `family ${entry.family}`}</dd>
        <dt>subject</dt>
        <dd>{PROV_SUBJECT_KINDS[entry.subjectKind] ?? `kind ${entry.subjectKind}`}</dd>
        <dt>model card</dt>
        <dd>
          {entry.cardUrl ? (
            <a href={entry.cardUrl} target="_blank" rel="noreferrer">
              {entry.cardUrl}
            </a>
          ) : (
            <span className="faint">not published by this engine</span>
          )}
        </dd>
      </dl>
    </div>
  );
}

/**
 * Provenance for a value the browser computed.
 *
 * Deliberately not shaped like {@link ProvCard}: there is no model id, no version and no parameter
 * set, because there is no model — and printing empty fields where a model card's would be is how a
 * client-side number starts looking like a simulation result. What it does carry is the thing that
 * makes the value checkable: what computed it, from which inputs, under which rule.
 */
function ClientCard({ info }: { info: ClientProvenance }): React.JSX.Element {
  return (
    <div className="section" data-testid="client-prov-card">
      <dl className="kv">
        <dt>produced by</dt>
        <dd data-testid="client-prov-producer">{info.producer}</dd>
        <dt>computation</dt>
        <dd>{info.computation}</dd>
        <dt>inputs</dt>
        <dd>{info.inputs}</dd>
        {info.reference ? (
          <>
            <dt>follows</dt>
            <dd>{info.reference}</dd>
          </>
        ) : null}
        {info.quantisation ? (
          <>
            <dt>quantisation</dt>
            <dd>{info.quantisation}</dd>
          </>
        ) : null}
      </dl>
      {info.caveat ? (
        <div className="note">
          <strong>Caveat.</strong> {info.caveat}
        </div>
      ) : null}
    </div>
  );
}

function ExplainCard({ info }: { info: ProvenanceInfo }): React.JSX.Element {
  return (
    <div className="section">
      <dl className="kv">
        <dt>model</dt>
        <dd>{info.model_id}</dd>
        <dt>version</dt>
        <dd>{info.model_version}</dd>
        <dt>parameter set</dt>
        <dd>{info.param_set_id}</dd>
        {info.family ? (
          <>
            <dt>family</dt>
            <dd>{info.family}</dd>
          </>
        ) : null}
        {info.card_url ? (
          <>
            <dt>model card</dt>
            <dd>
              <a href={info.card_url} target="_blank" rel="noreferrer">
                {info.card_url}
              </a>
            </dd>
          </>
        ) : null}
      </dl>
      {info.assumptions && info.assumptions.length > 0 ? (
        <ul className="faint" style={{ margin: "4px 0 0", paddingLeft: 16 }}>
          {info.assumptions.map((a) => (
            <li key={a}>{a}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function WhyTab(): React.JSX.Element {
  const why = useStudio((s) => s.why);
  const metricProvenance = useStudio((s) => s.metricProvenance);
  const metricDims = useStudio((s) => s.metricDims);
  const provenanceCount = useStudio((s) => s.provenanceCount);
  const [explain, setExplain] = useState<ExplainResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setExplain(null);
    setError(null);
  }, [why?.id, why?.node, why?.kind]);

  const ask = useCallback(async () => {
    if (!why) return;
    setBusy(true);
    setError(null);
    try {
      const res = await engine.request("explain", {
        subject: {
          kind: why.kind,
          id: why.id,
          ...(why.node !== undefined ? { node: why.node } : {}),
          ...(why.actor !== undefined ? { actor: why.actor } : {}),
          ...(why.provId !== undefined ? { prov_id: why.provId } : {}),
        },
        depth: 2,
        format: "json",
      });
      setExplain(res);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [why]);

  if (!why) {
    return (
      <div className="panel-body">
        <p className="dim">
          Every number in the Studio resolves back to what produced it. Activate a value — in the HUD, the
          inspector, the plots strip, the overlay menu, the frame-rate strip, the world summary, the event
          list or the comparison tables — and it opens here with the model, version and parameter set behind
          it, or, when the browser computed it, with the computation and its inputs.
        </p>
        <p className="faint">
          {provenanceCount} provenance entries received on this connection (§3.8).
        </p>
      </div>
    );
  }

  const wireProvId = why.provId ?? (why.kind === "metric" ? metricProvenance[why.id] : undefined);
  const local = wireProvId !== undefined ? engine.resolveProvenance(wireProvId) : null;

  return (
    <div className="panel-body" data-testid="why-tab">
      <div className="section">
        <h3>Subject</h3>
        <dl className="kv">
          <dt>value</dt>
          <dd data-testid="why-value">
            {why.label}
            {why.value !== undefined ? ` = ${why.value}` : ""}
          </dd>
          <dt>ref</dt>
          <dd>
            {why.kind} · {why.id}
            {why.node !== undefined ? ` · node ${why.node}` : ""}
          </dd>
          {why.unit ? (
            <>
              <dt>unit</dt>
              <dd>{why.unit}</dd>
            </>
          ) : null}
          {why.kind === "metric" && metricDims[why.id] ? (
            <>
              <dt>dimensions</dt>
              <dd title="§3.8 dimension dictionary, resolved from MetricSample.dim_key">{metricDims[why.id]}</dd>
            </>
          ) : null}
        </dl>
      </div>

      {why.client ? (
        <>
          <div className="note info" data-testid="why-client">
            Computed in the browser, not by the engine. There is no model card for this number because there
            is no model behind it; what produced it is below.
          </div>
          <ClientCard info={why.client} />
        </>
      ) : null}

      {local ? (
        <>
          <div className="note info">
            Resolved locally from a <code>Provenance</code> frame (§3.8) — <code>prov_id {wireProvId}</code>.
          </div>
          <ProvCard entry={local} />
        </>
      ) : wireProvId !== undefined ? (
        <div className="note" data-testid="why-unresolved">
          This value carries <code>prov_id {wireProvId}</code>, but no <code>Provenance</code> frame has
          defined it yet. §3.8 requires the server to send one covering every id it references before the
          next keyframe; until then the model behind this value is unknown.
        </div>
      ) : why.client ? null : (
        <div className="note" data-testid="why-absent">
          <strong>No provenance id is carried for this value.</strong> The §3.5.2 <code>NodeTelemetry</code>{" "}
          record has no <code>prov_id</code> column, so the binary stream cannot say which model produced it.
          Ask the engine with <code>explain</code> (§6.9) instead.
        </div>
      )}

      <div className="row" style={{ marginTop: 8 }}>
        {/*
          `explain` is offered for anything the engine could know about, and withheld for a value it
          has never seen. Asking the engine to explain this browser's frame rate would either invent
          an answer or return `-32006 UNKNOWN_ID`; neither is worth a button. A subject that carries
          a `prov_id` as well as a client record — a difference the browser computed from two engine
          metrics — keeps the button, because the metric behind it is the engine's.
        */}
        {why.client === undefined || wireProvId !== undefined ? (
          <button type="button" onClick={() => void ask()} disabled={busy} data-testid="why-explain">
            {busy ? "asking…" : "Ask the engine (explain)"}
          </button>
        ) : (
          <span className="faint" data-testid="why-no-explain">
            <code>explain</code> (§6.9) is not offered: the engine never saw this value.
          </span>
        )}
      </div>

      {error ? <div className="note err">explain failed: {error}</div> : null}

      {explain ? (
        <div style={{ marginTop: 8 }} data-testid="why-explain-result">
          {explain.value !== undefined ? (
            <dl className="kv">
              <dt>engine value</dt>
              <dd>
                {String(explain.value)} {explain.unit ?? ""}
              </dd>
            </dl>
          ) : null}
          <h3>Model chain ({explain.chain.length})</h3>
          {explain.chain.map((info, i) => (
            <ExplainCard key={`${info.model_id}-${i}`} info={info} />
          ))}
          {explain.definition_md ? (
            <div className="section">
              <h3>Definition</h3>
              <p className="dim">{explain.definition_md}</p>
            </div>
          ) : null}
          {explain.caveats && explain.caveats.length > 0 ? (
            <div className="note">
              <strong>Caveats</strong>
              <ul style={{ margin: "2px 0 0", paddingLeft: 16 }}>
                {explain.caveats.map((c) => (
                  <li key={c}>{c}</li>
                ))}
              </ul>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
