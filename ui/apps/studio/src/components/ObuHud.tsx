/**
 * The OBU HUD — the centrepiece of 09-ui §5.
 *
 * Layout follows the sketch in that section line for line: identity, then message rates and the
 * verification queue, then compute and storage, then the certificate/peer/CRL stores, then
 * neighbours and the channel, then GNSS and the clock, then evidence and reports, then the
 * sparkline row. Every value comes from the live `Telemetry` frame (§3.5) and is formatted by
 * `lib/telemetry.ts`, which knows each field's wire unit and its "not modelled" sentinel.
 *
 * Every value is a button: clicking it — or tabbing to it and pressing Enter or Space — opens the
 * inspector's "why" tab for that field (09-ui §10, keyboard control).
 */

import { useMemo } from "react";

import { Sparkline } from "./Sparkline.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { NA, durationNs, int, shortDigest, simClock } from "../lib/format.js";
import { MISSING_FROM_WIRE, SPARKLINE_SERIES, hudGroups, totalDrops, type HudField } from "../lib/telemetry.js";
import { getPointer } from "../lib/schema.js";

function fieldIndex(fields: HudField[]): Map<string, HudField> {
  const m = new Map<string, HudField>();
  for (const f of fields) m.set(f.key, f);
  return m;
}

/**
 * One clickable value. `label` overrides the field's own when the sketch words it differently.
 *
 * A real `<button>`, not a `<span onClick>`. There are around 45 of these and each one is the
 * entry point to the inspector's "why" tab, so as bare spans they were unreachable by keyboard
 * (WCAG 2.1 SC 2.1.1) and announced as static text (SC 4.1.2) — while the Inspector's equivalent
 * control, one screen to the right, was already a button. The help text moved out of `title`,
 * which is invisible to keyboard and touch, into a visually-hidden `aria-describedby` target.
 */
function Value({ field, label, node }: { field: HudField | undefined; label?: string; node: number | null }): React.JSX.Element {
  const setWhy = useStudio((s) => s.setWhy);
  if (!field) return <span className="hud-na">{NA}</span>;
  const isNa = field.value.includes(NA);
  const name = label ?? field.label;
  const helpId = `hud-help-${field.key}`;
  return (
    <button
      type="button"
      className="hud-field"
      data-testid={`hud-${field.key}`}
      aria-label={`${name}: ${field.value}${field.visibility === "GT" ? " (ground truth)" : ""}`}
      aria-describedby={helpId}
      onClick={() =>
        setWhy({
          kind: "node_field",
          id: field.key,
          label: name,
          ...(node !== null ? { node } : {}),
          value: field.value,
          unit: field.unit,
        })
      }
    >
      <span className="k">{name}</span>
      <span className={isNa ? "hud-na" : "v"}>{field.value}</span>
      {field.visibility === "GT" ? <span className="gt-tag">GT</span> : null}
      <span className="sr-only" id={helpId}>
        {field.key} — {field.unit}
        {field.help ? ` · ${field.help}` : ""} · activate to open the provenance tab
      </span>
    </button>
  );
}

export function ObuHud({ docked = false }: { docked?: boolean }): React.JSX.Element | null {
  const telemetry = useStudio((s) => s.telemetry);
  const telemetryNode = useStudio((s) => s.telemetryNode);
  const pseudonym = useStudio((s) => s.pseudonym);
  const inspect = useStudio((s) => s.inspect);
  const simTimeNs = useStudio((s) => s.simTimeNs);
  const selectedActor = useStudio((s) => s.selectedActor);
  const seriesTick = useStudio((s) => s.seriesTick);
  const hello = useStudio((s) => s.hello);

  const groups = useMemo(() => (telemetry ? hudGroups(telemetry) : []), [telemetry]);
  const byKey = useMemo(() => fieldIndex(groups.flatMap((g) => [...g.fields])), [groups]);

  if (telemetryNode === null) {
    return (
      <div className={`hud${docked ? " docked" : ""}`} data-testid="obu-hud">
        <div className="hud-head">
          <span className="id">No radio selected</span>
          <span className="dim">
            Select a vehicle or a roadside unit on the map and its radio appears here: what it is sending
            and receiving, what it has verified, who it can hear.
          </span>
        </div>
      </div>
    );
  }

  const info = engine.nodes.get(telemetryNode);
  if (!telemetry) {
    return (
      <div className={`hud${docked ? " docked" : ""}`} data-testid="obu-hud">
        <div className="hud-head">
          <span className="id">{info?.label || `node ${telemetryNode}`}</span>
          <span className="dim">Selected. Waiting for its first report from the engine…</span>
        </div>
      </div>
    );
  }

  const drops = totalDrops(telemetry);
  const evidence = MISSING_FROM_WIRE[0];
  const evidenceValue = inspect ? getPointer(inspect, `/${evidence.inspectPath.join("/")}`) : undefined;

  return (
    <div className={`hud${docked ? " docked" : ""}`} data-testid="obu-hud">
      <div className="hud-head">
        <span className="id" data-testid="hud-identity">
          {info?.kind === 2 ? "RSU" : "OBU"} {info?.label || `node ${telemetryNode}`}
        </span>
        <span className="dim">node {telemetryNode}</span>
        {selectedActor !== null ? <span className="dim">actor {selectedActor}</span> : null}
        <span data-testid="hud-pseudonym">
          pseudonym{" "}
          {pseudonym ? (
            <>
              <b>{shortDigest(pseudonym.digest)}</b>{" "}
              <span className="dim">
                {pseudonym.i !== null && pseudonym.j !== null ? `(j=${pseudonym.j}, i=${pseudonym.i})` : `(indices pending — ${pseudonym.source})`}
              </span>
            </>
          ) : (
            <span className="hud-na">not yet seen transmitting</span>
          )}
        </span>
        <span className="dim">profile: {info?.profileId || NA}</span>
        <span className="spacer grow" />
        <span className="dim" data-testid="hud-simtime">
          t {simClock(simTimeNs)}
        </span>
      </div>

      <div className="hud-body">
        <div className="hud-row">
          <Value field={byKey.get("msgs_in_per_s")} label="rx" node={telemetryNode} />
          <Value field={byKey.get("msgs_out_per_s")} label="tx" node={telemetryNode} />
          <Value field={byKey.get("verifications_per_s")} label="verify" node={telemetryNode} />
          <Value field={byKey.get("q_verify_p95")} label="verify q p95" node={telemetryNode} />
          <Value field={byKey.get("verify_wait_p95_ms")} label="p95 wait" node={telemetryNode} />
          <Value field={byKey.get("verify_policy")} label="policy" node={telemetryNode} />
          <span className="hud-field">
            <span className="k">dropped</span>
            <span className="v" data-testid="hud-drops">
              {int(drops)}
            </span>
          </span>
        </div>

        <div className="hud-row">
          <Value field={byKey.get("cpu_util_pm")} label="CPU" node={telemetryNode} />
          <Value field={byKey.get("ram_used_kib")} label="RAM" node={telemetryNode} />
          <Value field={byKey.get("storage_used_b")} label="flash" node={telemetryNode} />
          <Value field={byKey.get("hsm_util_pm")} label="HSM" node={telemetryNode} />
          <Value field={byKey.get("airtime_ms_per_s")} label="air time" node={telemetryNode} />
          <Value field={byKey.get("node_state")} label="state" node={telemetryNode} />
        </div>

        <div className="hud-row">
          <Value field={byKey.get("cert_active")} label="certs active" node={telemetryNode} />
          <Value field={byKey.get("cert_stored")} label="stored" node={telemetryNode} />
          <Value field={byKey.get("next_topup_ns")} label="next top-up" node={telemetryNode} />
          <Value field={byKey.get("peer_cache_entries")} label="peer cache" node={telemetryNode} />
          <Value field={byKey.get("crl_entries")} label="CRL entries" node={telemetryNode} />
          <Value field={byKey.get("crl_bytes")} label="CRL bytes" node={telemetryNode} />
          <Value field={byKey.get("crl_expansion_pm")} label="expansion" node={telemetryNode} />
          <Value field={byKey.get("p2pcd_requests")} label="P2PCD" node={telemetryNode} />
        </div>

        <div className="hud-row">
          <Value field={byKey.get("nbr_total")} label="neighbours" node={telemetryNode} />
          <Value field={byKey.get("nbr_verified")} label="verified" node={telemetryNode} />
          <Value field={byKey.get("nbr_unverified")} label="unverified" node={telemetryNode} />
          <Value field={byKey.get("nbr_revoked")} label="revoked" node={telemetryNode} />
          <Value field={byKey.get("cbr_pm")} label="CBR" node={telemetryNode} />
          <Value field={byKey.get("dcc_state")} label="DCC" node={telemetryNode} />
          <Value field={byKey.get("tx_power_cdbm")} label="tx" node={telemetryNode} />
          <Value field={byKey.get("unverified_ratio_pm")} label="unverified delivered" node={telemetryNode} />
        </div>

        <div className="hud-row">
          <Value field={byKey.get("gnss_fix")} label="GNSS fix" node={telemetryNode} />
          <Value field={byKey.get("gnss_sigma_m")} label="σ" node={telemetryNode} />
          <Value field={byKey.get("gnss_hdop")} label="HDOP" node={telemetryNode} />
          <Value field={byKey.get("clock_drift_ppm")} label="clock drift" node={telemetryNode} />
          <Value field={byKey.get("clock_offset_ns")} label="clock offset" node={telemetryNode} />
          <Value field={byKey.get("pos_error_m")} label="belief vs GT" node={telemetryNode} />
        </div>

        <div className="hud-row">
          <span className="hud-field" title={evidence.reason} data-testid="hud-evidence">
            <span className="k">evidence buffer</span>
            {evidenceValue === undefined ? (
              <span className="hud-missing">not on the wire — {evidence.reason}</span>
            ) : (
              <span className="v">{typeof evidenceValue === "object" ? JSON.stringify(evidenceValue) : String(evidenceValue)}</span>
            )}
          </span>
          <Value field={byKey.get("outbox_msgs")} label="report outbox" node={telemetryNode} />
          <Value field={byKey.get("outbox_bytes")} label="outbox bytes" node={telemetryNode} />
          <Value field={byKey.get("full_cert_msgs")} label="full-cert msgs" node={telemetryNode} />
          <span className="hud-field" title="The period each of these figures was measured over">
            <span className="k">window</span>
            <span className="v">{durationNs(hello?.telemetryPeriodNs ?? 0)}</span>
          </span>
        </div>
      </div>

      <div className="sparkrow" data-testid="hud-sparklines">
        {SPARKLINE_SERIES.map((s, i) => (
          <Sparkline
            key={s.key}
            seriesIndex={i}
            label={s.label}
            unit={s.unit}
            tick={seriesTick}
            fieldKey={s.key}
            node={telemetryNode}
          />
        ))}
      </div>
    </div>
  );
}
