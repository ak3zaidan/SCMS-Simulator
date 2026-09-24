/**
 * The followed vehicle's messages and queues: the owner's chase-view ask.
 *
 * "When you go down into a specific chase view for a vehicle, the user should be able to see the
 * broadcast messages being sent out and their content, along with the queue of that specific node."
 *
 * Three tabs. **Sent** lists every frame the vehicle put on the air; **Received** every reception it
 * resolved, with the sender, the fate or the loss cause, the signal and the delay; **Queues** its five
 * queues at this instant with the messages waiting in them. The rows stream in from the engine's
 * `node.feed` push (`lib/feed.ts`) while the vehicle is followed. Clicking a row opens it underneath:
 * every field decoded from the frame's own octets, the 1609.2 envelope, the delay stage by stage, and
 * the SPDU in hex with its header, payload, headerInfo, signer and signature marked.
 *
 * It lives in the right-hand panel, beside the viewport rather than over it, so it never covers the
 * vehicle the chase camera is following.
 */

import { Fragment, useMemo } from "react";

import type { FeedDecoded, FeedQueue, FeedReceived, FeedSent } from "@vwp/protocol";

import { useStudio } from "../state/store.js";
import { NA, shortDigest, simClock } from "../lib/format.js";
import { fieldOf, fieldText, hexRows, keyOf, typesSeen, visibleRows, type FeedDir } from "../lib/feed.js";

/** Rows drawn per table; the ring keeps more, and the count says so. */
const DRAWN = 60;

const fmt = (v: number | null | undefined, digits: number, unit = ""): string =>
  typeof v === "number" && Number.isFinite(v) ? `${v.toFixed(digits)}${unit}` : NA;

function num(e: FeedSent | FeedReceived, k: string, digits: number): string {
  const f = fieldOf(e, k);
  if (!f) return NA;
  if (f.na) return "n/a";
  return typeof f.v === "number" ? f.v.toFixed(digits) : String(f.v ?? NA);
}

function SentTable({ rows, openKey }: { rows: readonly FeedSent[]; openKey: string | null }): React.JSX.Element {
  const open = useStudio((s) => s.openFeedMessage);
  return (
    <table className="table feed-table" data-testid="feed-table-sent">
      <thead>
        <tr>
          <th>time</th>
          <th>type</th>
          <th title="msgCnt · temporary id, as decoded from the payload">cnt · id</th>
          <th title="latitude, longitude decoded from the payload">position</th>
          <th title="speed, m/s">m/s</th>
          <th title="heading, degrees from north">hdg</th>
          <th title="octets on the air">B</th>
        </tr>
      </thead>
      <tbody>
        {rows.slice(0, DRAWN).map((e, i) => {
          const key = keyOf("sent", e);
          const rotated = i + 1 < rows.length && rows[i + 1].pseudonym !== e.pseudonym;
          return (
            <tr
              key={key}
              data-testid="feed-row-sent"
              data-msg={e.msg}
              className={`${openKey === key ? "open" : ""}${rotated ? " rotated" : ""}`}
              aria-selected={openKey === key}
              tabIndex={0}
              onClick={() => open("sent", openKey === key ? null : key)}
              onKeyDown={(ev) => {
                if (ev.key === "Enter" || ev.key === " ") {
                  ev.preventDefault();
                  open("sent", openKey === key ? null : key);
                }
              }}
            >
              <td>{simClock(e.t_ns).slice(3)}</td>
              <td>
                {e.type.toUpperCase()}
                {rotated ? <span className="feed-badge" title="the pseudonym changed on this frame"> new id</span> : null}
              </td>
              <td>
                {num(e, "msg_cnt", 0)} · {String(fieldOf(e, "temp_id")?.v ?? NA)}
              </td>
              <td>
                {num(e, "lat", 5)}, {num(e, "lon", 5)}
              </td>
              <td>{num(e, "speed", 1)}</td>
              <td>{num(e, "heading", 0)}</td>
              <td>{e.bytes.on_wire}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function fate(e: FeedReceived): string {
  if (e.outcome === "delivered") return e.verification ? `delivered · ${e.verification}` : "delivered";
  if (e.outcome === "in-flight") return "in flight";
  return `lost · ${e.cause ?? "unknown"}`;
}

function ReceivedTable({ rows, openKey }: { rows: readonly FeedReceived[]; openKey: string | null }): React.JSX.Element {
  const open = useStudio((s) => s.openFeedMessage);
  return (
    <table className="table feed-table" data-testid="feed-table-received">
      <thead>
        <tr>
          <th>time</th>
          <th title="the sending node (ground truth), or its temporary id on a node-profile connection">from</th>
          <th>type</th>
          <th>fate</th>
          <th title="received power, dBm / SINR, dB">RSSI / SINR</th>
          <th title="sender distance, m (ground truth)">m</th>
          <th title="end-to-end delay, generation to delivery, ms">ms</th>
        </tr>
      </thead>
      <tbody>
        {rows.slice(0, DRAWN).map((e) => {
          const key = keyOf("received", e);
          const from = e.from !== undefined && e.from !== null ? `node ${e.from}` : String(fieldOf(e, "temp_id")?.v ?? NA);
          return (
            <tr
              key={key}
              data-testid="feed-row-received"
              data-outcome={e.outcome}
              className={`${openKey === key ? "open" : ""}${e.outcome === "delivered" ? "" : " lost"}`}
              aria-selected={openKey === key}
              tabIndex={0}
              onClick={() => open("received", openKey === key ? null : key)}
              onKeyDown={(ev) => {
                if (ev.key === "Enter" || ev.key === " ") {
                  ev.preventDefault();
                  open("received", openKey === key ? null : key);
                }
              }}
            >
              <td>{simClock(e.t_ns).slice(3)}</td>
              <td data-testid="feed-from">{from}</td>
              <td>{e.type.toUpperCase()}</td>
              <td>{fate(e)}</td>
              <td>
                {fmt(e.rssi_dbm, 0)} / {fmt(e.sinr_db, 0)}
              </td>
              <td>{fmt(e.dist_m, 0)}</td>
              <td>{fmt(e.e2e_ms, 1)}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/** The SPDU in hex, eight octets a line, each octet tinted by the span it belongs to. */
function HexDump({ decoded }: { decoded: FeedDecoded }): React.JSX.Element | null {
  const rows = useMemo(() => hexRows(decoded.hex, decoded.spans, 8), [decoded.hex, decoded.spans]);
  if (rows.length === 0) return null;
  const spans = decoded.spans ?? [];
  const cls = (i: number): string => (i < 0 ? "" : `span-${spans[i].name.replace(/[^a-z0-9]+/gi, "-").toLowerCase()}`);
  return (
    <div className="feed-hex" data-testid="feed-hex">
      <div className="feed-hex-legend">
        {spans.map((s, i) => (
          <span key={s.name} className={`feed-hex-key ${cls(i)}`} data-testid={`feed-${cls(i)}`}>
            {s.name} <span className="faint">
              {s.start}–{s.end - 1} ({s.end - s.start} B)
            </span>
          </span>
        ))}
      </div>
      <pre className="mono" aria-label={`the SPDU, ${decoded.spdu_bytes ?? rows.length * 8} octets in hex`}>
        {rows.map((r) => (
          <Fragment key={r.offset}>
            <span className="faint">{r.offset.toString(16).padStart(4, "0")} </span>
            {r.bytes.map((b) => (
              <span key={b.offset} className={cls(b.span)} title={b.span >= 0 ? spans[b.span].name : undefined}>
                {b.hex}{" "}
              </span>
            ))}
            {"\n"}
          </Fragment>
        ))}
      </pre>
    </div>
  );
}

function KV({ k, v, testid }: { k: string; v: string; testid?: string }): React.JSX.Element {
  return (
    <>
      <dt>{k}</dt>
      <dd data-testid={testid}>{v}</dd>
    </>
  );
}

function Decoded({ d }: { d: FeedDecoded }): React.JSX.Element {
  const m = d.message;
  const sec = d.security;
  return (
    <>
      {d.note ? <p className="dim">{d.note}</p> : null}
      {d.error ? <p className="warn-text">{d.error}</p> : null}
      {m ? (
        <div className="section">
          <h4>{m.format}</h4>
          {m.error ? <p className="warn-text">{m.error}</p> : null}
          {m.note ? <p className="dim">{m.note}</p> : null}
          <dl className="kv" data-testid="feed-fields">
            {m.fields.map((f) => (
              <Fragment key={f.k}>
                <dt>{f.label}</dt>
                <dd data-testid={`feed-field-${f.k}`} data-value={f.v === null ? "" : String(f.v)} className={f.na ? "hud-na" : undefined}>
                  {fieldText(f.v, f.unit, f.na)}
                  {typeof f.raw === "number" && f.unit ? <span className="faint"> ({f.raw})</span> : null}
                </dd>
              </Fragment>
            ))}
          </dl>
        </div>
      ) : null}
      {sec ? (
        <div className="section">
          <h4>{sec.standard}</h4>
          <dl className="kv" data-testid="feed-security">
            <KV k="PSID" v={`${sec.psid} (0x${sec.psid.toString(16)})`} />
            <KV k="generation time" v={sec.generation_time_us === null ? NA : `${sec.generation_time_us} µs since 2004-01-01`} />
            <KV k="signer" v={sec.signer.kind} testid="feed-signer-kind" />
            {sec.signer.hashed_id8 ? <KV k="HashedId8" v={sec.signer.hashed_id8} testid="feed-hashedid8" /> : null}
            {sec.signer.certificate ? (
              <>
                <KV k="certificate" v={`${sec.signer.certificate.type}, ${sec.signer.certificate.bytes ?? "?"} B, issuer ${sec.signer.certificate.issuer}`} />
                <KV k="validity" v={`from Time32 ${sec.signer.certificate.validity_start_time32} for ${sec.signer.certificate.validity_duration}`} />
                <KV k="permissions" v={`PSID ${sec.signer.certificate.app_permissions.join(", ")}`} />
                <KV k="CRL series" v={String(sec.signer.certificate.crl_series)} />
              </>
            ) : null}
            <KV k="signature" v={`${sec.signature.alg}, r ${shortDigest(sec.signature.r)}, s ${shortDigest(sec.signature.s)}`} />
          </dl>
        </div>
      ) : null}
      <HexDump decoded={d} />
    </>
  );
}

function Detail(): React.JSX.Element | null {
  const open = useStudio((s) => s.feed.open);
  const close = useStudio((s) => s.openFeedMessage);
  if (!open) return null;
  const e = open.entry;
  const sent = open.dir === "sent" ? (e as FeedSent) : null;
  const rx = open.dir === "received" ? (e as FeedReceived) : null;
  return (
    <div className="feed-detail" data-testid="feed-detail" data-msg={String(e.msg)}>
      <div className="feed-detail-head">
        <b>
          {e.type.toUpperCase()} · message {String(e.msg ?? NA)} · {open.dir === "sent" ? "sent" : "resolved"} {simClock(e.t_ns)}
        </b>
        <button type="button" className="linklike" onClick={() => close(open.dir, null)} title="Close this message">
          close
        </button>
      </div>
      {sent ? (
        <dl className="kv">
          <KV k="pseudonym" v={sent.pseudonym ?? NA} />
          <KV k="signer id" v={sent.signer ?? NA} />
          <KV
            k="octets"
            v={`payload ${sent.bytes.payload ?? NA} + envelope ${sent.bytes.envelope ?? NA}${sent.bytes.certificate ? ` (cert ${sent.bytes.certificate})` : ""} + network ${sent.bytes.network ?? NA} + link ${sent.bytes.link ?? NA} = ${sent.bytes.on_wire} B`}
          />
          <KV k="radio" v={`${fmt(sent.radio.power_dbm, 1, " dBm")} · channel ${sent.radio.channel ?? NA} · ${sent.radio.airtime_us ?? NA} µs on air`} />
          <KV
            k="timing"
            v={`sign queue ${fmt(sent.timing.sign_queue_ms, 3, " ms")} · signing ${fmt(sent.timing.sign_ms, 3, " ms")} · channel access ${fmt(sent.timing.channel_access_ms, 3, " ms")}`}
          />
        </dl>
      ) : null}
      {rx ? (
        <dl className="kv">
          <KV k="from" v={rx.from !== undefined && rx.from !== null ? `node ${rx.from}` : "not shown on a node-profile connection"} />
          <KV k="fate" v={fate(rx)} testid="feed-fate" />
          <KV k="signal" v={`${fmt(rx.rssi_dbm, 1, " dBm")} · SINR ${fmt(rx.sinr_db, 1, " dB")} · ${fmt(rx.dist_m, 1, " m")}`} />
          <KV k="end to end" v={fmt(rx.e2e_ms, 3, " ms")} />
          {Object.entries(rx.stages_ms).map(([k, v]) => (
            <KV key={k} k={`· ${k.replace(/_/g, " ")}`} v={`${v.toFixed(3)} ms`} />
          ))}
        </dl>
      ) : null}
      {e.decoded ? <Decoded d={e.decoded} /> : <p className="dim">The receiver never had these octets: the frame was lost before it reached the application.</p>}
    </div>
  );
}

function QueueTable({ queues, stepMs }: { queues: readonly FeedQueue[]; stepMs: number }): React.JSX.Element {
  return (
    <>
      <table className="table feed-table" data-testid="feed-queues">
        <thead>
          <tr>
            <th>queue</th>
            <th title={`messages waiting at this instant / the most at once during the last ${stepMs} ms`}>now/peak</th>
            <th title="being served now">srv</th>
            <th title="wait, median over the last second, ms">p50</th>
            <th title="wait, 95th percentile over the last second, ms">p95</th>
            <th title="drops over the last ten seconds">drops</th>
            <th title="the node's own telemetry window: depth p50 / p95">node</th>
          </tr>
        </thead>
        <tbody>
          {queues.map((q) => (
            <tr key={q.id} data-testid={`queue-${q.id}`} title={q.what}>
              <td>{q.label}</td>
              <td data-testid={`queue-${q.id}-depth`} data-depth={q.depth ?? ""} data-peak={q.peak ?? ""}>
                {q.depth === null ? "—" : `${q.depth}/${q.peak ?? q.depth}`}
              </td>
              <td>{q.in_service}</td>
              <td>{fmt(q.wait_p50_ms, 2)}</td>
              <td>{fmt(q.wait_p95_ms, 2)}</td>
              <td title={q.drops_note}>
                {Object.keys(q.drops).length === 0
                  ? q.drops_note
                    ? "n/o"
                    : "0"
                  : Object.entries(q.drops)
                      .map(([k, v]) => `${k} ${v}`)
                      .join(", ")}
              </td>
              <td>{q.reported_depth ? `${q.reported_depth.p50 ?? NA}/${q.reported_depth.p95 ?? NA}` : NA}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {queues
        .filter((q) => q.waiting.length > 0)
        .map((q) => (
          <div className="section" key={q.id} data-testid={`queue-waiting-${q.id}`}>
            <h4>
              {q.label} queue in the last {stepMs} ms: {q.depth ?? 0} waiting now, {q.waiting.length + q.waiting_omitted} passed through
              {q.waiting_omitted > 0 ? ` (${q.waiting_omitted} not listed)` : ""}
            </h4>
            <table className="table feed-table">
              <thead>
                <tr>
                  <th>message</th>
                  <th>type</th>
                  <th>from</th>
                  <th>enqueued</th>
                  <th>waited</th>
                  <th>now</th>
                </tr>
              </thead>
              <tbody>
                {q.waiting.map((w) => (
                  <tr
                    key={`${String(w.msg)}-${w.enqueued_ns}`}
                    data-testid="queue-entry"
                    data-waiting={w.left_ns === null ? "yes" : "no"}
                    className={w.left_ns === null ? undefined : "lost"}
                  >
                    <td>{String(w.msg ?? NA)}</td>
                    <td>{w.type.toUpperCase()}</td>
                    <td>{w.from === null ? "self" : `node ${w.from}`}</td>
                    <td>{simClock(w.enqueued_ns).slice(3)}</td>
                    <td>{w.waited_ms.toFixed(2)} ms</td>
                    <td>{w.left_ns === null ? w.stage : `left at ${simClock(w.left_ns).slice(6)} · ${w.stage}`}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ))}
    </>
  );
}

export function MessagePanel(): React.JSX.Element {
  const feed = useStudio((s) => s.feed);
  const tab = useStudio((s) => s.feedTab);
  const setTab = useStudio((s) => s.setFeedTab);
  const setPaused = useStudio((s) => s.setFeedPaused);
  const setFilter = useStudio((s) => s.setFeedFilter);
  const selectedNode = useStudio((s) => s.selectedNode);

  if (selectedNode === null) {
    return (
      <div className="panel-body" data-testid="message-panel">
        <p className="dim">Follow a vehicle and what it broadcasts, what it hears and what is queued inside it appears here, message by message.</p>
      </div>
    );
  }

  const types = typesSeen(feed);
  const sent = visibleRows(feed, "sent");
  const received = visibleRows(feed, "received");
  const openKey = feed.open?.key ?? null;
  const latest = feed.sent[0];
  const toggleType = (t: string): void => {
    const on = new Set(feed.types);
    if (on.has(t)) on.delete(t);
    else on.add(t);
    setFilter({ types: [...on] });
  };
  const dir: FeedDir | null = tab === "queues" ? null : tab;

  return (
    <div className="panel-body feed" data-testid="message-panel">
      <div className="feed-bar">
        <div className="tabs feed-tabs" role="tablist">
          {(["sent", "received", "queues"] as const).map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={tab === t}
              className={tab === t ? "active" : ""}
              data-testid={`feed-tab-${t}`}
              onClick={() => setTab(t)}
            >
              {t === "sent" ? `Sent ${feed.sent.length}` : t === "received" ? `Received ${feed.received.length}` : "Queues"}
            </button>
          ))}
        </div>
        <button
          type="button"
          data-testid="feed-pause"
          aria-pressed={feed.paused}
          onClick={() => setPaused(!feed.paused)}
          title={feed.paused ? "Show what arrived while paused and keep streaming" : "Freeze the tables; messages keep arriving and are shown on resume"}
        >
          {feed.paused ? `Resume${feed.held.length > 0 ? ` (${feed.held.length})` : ""}` : "Pause"}
        </button>
      </div>

      <div className="feed-status faint" data-testid="feed-status">
        {feed.unavailable ? (
          <span className="warn-text">{feed.unavailable}</span>
        ) : feed.pushes === 0 ? (
          "waiting for the engine's first push…"
        ) : (
          <>
            {latest?.pseudonym ? <>pseudonym {shortDigest(latest.pseudonym)} · </> : null}
            {feed.paused ? "paused" : "live"} at {simClock(feed.tNs)}
            {feed.omitted.sent + feed.omitted.received > 0 ? ` · ${feed.omitted.sent + feed.omitted.received} left out by the push limit` : ""}
            {feed.dropped.sent + feed.dropped.received > 0 ? ` · ${feed.dropped.sent + feed.dropped.received} older rows dropped` : ""}
            {feed.undetected > 0 ? ` · ${feed.undetected} frames never detected (out of range)` : ""}
          </>
        )}
      </div>

      {dir !== null && types.length > 1 ? (
        <div className="feed-filters" data-testid="feed-filters">
          {types.map((t) => (
            <button
              key={t}
              type="button"
              className={feed.types.length === 0 || feed.types.includes(t) ? "chip on" : "chip"}
              aria-pressed={feed.types.includes(t)}
              data-testid={`feed-type-${t}`}
              onClick={() => toggleType(t)}
            >
              {t.toUpperCase()}
            </button>
          ))}
        </div>
      ) : null}
      {dir === "received" ? (
        <div className="feed-filters">
          <label>
            show{" "}
            <select
              value={feed.outcome}
              data-testid="feed-outcome"
              onChange={(ev) => setFilter({ outcome: ev.target.value as "all" | "delivered" | "lost" })}
            >
              <option value="all">every fate</option>
              <option value="delivered">delivered</option>
              <option value="lost">lost</option>
            </select>
          </label>
        </div>
      ) : null}

      {dir === "sent" ? (
        sent.length === 0 ? (
          <p className="dim">Nothing sent yet by this radio in the part of the run played so far.</p>
        ) : (
          <div className="feed-scroll">
            <SentTable rows={sent} openKey={openKey} />
          </div>
        )
      ) : null}
      {dir === "received" ? (
        received.length === 0 ? (
          <p className="dim">Nothing received that passes the filters.</p>
        ) : (
          <div className="feed-scroll">
            <ReceivedTable rows={received} openKey={openKey} />
          </div>
        )
      ) : null}
      {dir === null ? (
        feed.queues ? (
          <>
            <QueueTable queues={feed.queues.list} stepMs={feed.queues.step_ms} />
            <p className="help">
              {feed.queues.source}.
              {typeof feed.queues.kernel_lead_ms === "number" && feed.queues.kernel_lead_ms < 2 * feed.queues.step_ms
                ? ` The engine is only ${feed.queues.kernel_lead_ms.toFixed(0)} ms ahead of this instant, so a message still queued past that is not yet known and "now" may read low.`
                : ""}
            </p>
          </>
        ) : (
          <p className="dim">No queue reading yet.</p>
        )
      ) : null}

      <Detail />
    </div>
  );
}
