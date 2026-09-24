/**
 * The breakdowns: what a metric measures beyond its headline, pooled over the run so far.
 *
 * The plots strip draws each metric's headline over time. A delivery ratio is quoted by distance, a
 * latency is understood by its stages, and a network is judged by its worst nodes — and none of those
 * is a time series. These three cards ask the engine for them with a grouped `metrics.query`
 * (§6.12 `group_by` and `where`), which pools every window since the run began: a proportion pools
 * its successes and trials and carries the Wilson interval of the pooled count, a ratio of sums pools
 * its sums, and anything else is the sample-weighted mean. So a thin distance bin shows a wide
 * interval rather than a confident line, and a bin nobody reached shows nothing.
 *
 * - **Delivery against distance.** `pdr` (3GPP TR 36.885 packet reception ratio, receivers truly in
 *   range that decoded, 20 m bins) with its 95 % band, and `delivery_ratio` (reached the
 *   application — after verification — 50 m bins).
 * - **Where the delay goes.** `latency_stage` per stage, stacked, for each message type the run
 *   delivered, beside the mean end-to-end delay.
 * - **Nodes.** Per-node figures, ranked: air time, channel load, and delivery as a receiver. A row
 *   follows the node.
 * - **Fragmentation**, only in a run that splits messages: the realised SDU loss beside the
 *   fragment loss it amplifies and the two predictions of 04-models.md §7.4.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";

/** One group of a grouped query: the dimension's value and the pooled figure. */
interface Group {
  readonly key: string;
  readonly value: number | null;
  readonly lo: number | null;
  readonly hi: number | null;
  readonly n: number;
}

/** The latency decomposition's stages, in the order a message lives them (V2V_STAGES). */
const STAGES = [
  "sign_queue",
  "sign",
  "mac_aifs",
  "mac_backoff",
  "mac_defer",
  "airtime",
  "propagation",
  "reception",
  "verify_queue",
  "verify",
] as const;

/** Okabe–Ito, extended by two: distinguishable in both themes and to most colour-deficient eyes. */
const STAGE_COLOURS = [
  "#999999",
  "#E69F00",
  "#56B4E9",
  "#009E73",
  "#44AA99",
  "#F0E442",
  "#0072B2",
  "#D55E00",
  "#CC79A7",
  "#882255",
];

/**
 * The metrics this run measures (the catalogue's names and their bases), asked once per run.
 *
 * A run measures only the metrics its scenario asks for, and the engine answers a query for any
 * other with −32007. Asking anyway put an "unknown metric" error in the page's log every two
 * seconds for each card whose metric the run does not have (the phase 1 grid has no
 * `delivery_ratio`, `e2e_latency` or `channel_load`, and only a fragmenting run has `frag_*`).
 */
let measured: { key: string; names: Promise<ReadonlySet<string>> } | null = null;

function measuredNames(): Promise<ReadonlySet<string>> {
  const run = useStudio.getState().run;
  const key = `${run.runId}#${run.generation}`;
  if (measured === null || measured.key !== key) {
    const names = engine
      .request("metrics.query", {}, { quiet: true })
      .then((res) => {
        const set = new Set<string>();
        for (const c of res.catalogue ?? []) {
          set.add(c.name);
          if (c.base) set.add(c.base);
        }
        return set as ReadonlySet<string>;
      })
      .catch(() => new Set<string>() as ReadonlySet<string>);
    measured = { key, names };
  }
  return measured.names;
}

/** Asks for one metric grouped by one dimension; an engine without breakdowns answers nothing. */
async function grouped(metric: string, by: string, where: Record<string, string> = {}): Promise<Group[]> {
  const names = await measuredNames();
  if (!names.has(metric)) {
    // An empty catalogue is an answer too early in a run; ask again next time rather than keep it.
    if (names.size === 0) measured = null;
    return [];
  }
  try {
    const res = await engine.request(
      "metrics.query",
      {
        metrics: [metric],
        group_by: [by as "t"],
        ...(Object.keys(where).length > 0 ? { where } : {}),
        t_from_ns: 0,
      },
      { quiet: true },
    );
    return (res.rows ?? []).map((row) => ({
      key: String(row[0]),
      value: typeof row[1] === "number" ? row[1] : null,
      lo: typeof row[2] === "number" ? row[2] : null,
      hi: typeof row[3] === "number" ? row[3] : null,
      n: typeof row[4] === "number" ? row[4] : 0,
    }));
  } catch {
    return [];
  }
}

/** A distance label (`20-40`, `1000+`) as its lower and upper edge in metres. */
function edges(label: string): [number, number] | null {
  const m = /^(\d+(?:\.\d+)?)(?:-(\d+(?:\.\d+)?)|\+)$/.exec(label);
  if (!m) return null;
  const lo = Number(m[1]);
  const hi = m[2] !== undefined ? Number(m[2]) : lo;
  return [lo, hi];
}

/** Refresh `load` whenever the measurements move, at most once every two seconds. */
function usePolled<T>(load: () => Promise<T>, initial: T): T {
  const tick = useStudio((s) => s.seriesTick);
  const [value, setValue] = useState<T>(initial);
  const [last, setLast] = useState(0);
  useEffect(() => {
    const now = Date.now();
    if (now - last < 2000) return;
    setLast(now);
    let live = true;
    void load().then((v) => {
      if (live) setValue(v);
    });
    return () => {
      live = false;
    };
    // `last` is deliberately not a dependency: it throttles, it does not trigger.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tick, load]);
  return value;
}

const W = 300;
const H = 120;
const PAD = { l: 30, r: 8, t: 8, b: 20 };

function DeliveryByDistance(): React.JSX.Element {
  const load = useCallback(
    async () => ({ prr: await grouped("pdr", "dist_bin"), app: await grouped("delivery_ratio", "dist_bin") }),
    [],
  );
  const data = usePolled(load, { prr: [] as Group[], app: [] as Group[] });
  const points = (groups: Group[]) =>
    groups
      .map((g) => ({ g, e: edges(g.key) }))
      .filter((p): p is { g: Group; e: [number, number] } => p.e !== null && p.g.value !== null && p.e[1] > p.e[0])
      .map(({ g, e }) => ({ x: (e[0] + e[1]) / 2, y: g.value as number, lo: g.lo, hi: g.hi, n: g.n, key: g.key }));
  const prr = points(data.prr);
  const app = points(data.app);
  const xmax = Math.max(100, ...prr.map((p) => p.x + 10), ...app.map((p) => p.x + 25));
  const sx = (x: number) => PAD.l + (x / xmax) * (W - PAD.l - PAD.r);
  const sy = (y: number) => PAD.t + (1 - y) * (H - PAD.t - PAD.b);
  const path = (ps: { x: number; y: number }[]) => ps.map((p, i) => `${i === 0 ? "M" : "L"}${sx(p.x)},${sy(p.y)}`).join("");
  const band =
    prr.length > 1 && prr.every((p) => p.lo !== null && p.hi !== null)
      ? `${prr.map((p, i) => `${i === 0 ? "M" : "L"}${sx(p.x)},${sy(p.hi as number)}`).join("")}${[...prr]
          .reverse()
          .map((p) => `L${sx(p.x)},${sy(p.lo as number)}`)
          .join("")}Z`
      : null;
  const ticks = [0, Math.round(xmax / 4), Math.round(xmax / 2), Math.round((3 * xmax) / 4), Math.round(xmax)];
  return (
    <div className="plot-card wide breakdown" data-testid="breakdown-distance">
      <div className="title">
        <span>Delivery against distance</span>
        <span className="faint">pooled over the run</span>
      </div>
      {prr.length === 0 && app.length === 0 ? (
        <p className="dim breakdown-empty">No reception by distance yet.</p>
      ) : (
        <svg width={W} height={H} role="img" aria-label="Delivery ratio against distance">
          {[0, 0.5, 1].map((y) => (
            <g key={y}>
              <line x1={PAD.l} x2={W - PAD.r} y1={sy(y)} y2={sy(y)} className="grid" />
              <text x={PAD.l - 4} y={sy(y) + 3} textAnchor="end" className="axis">
                {y}
              </text>
            </g>
          ))}
          {ticks.map((x) => (
            <text key={x} x={sx(x)} y={H - 6} textAnchor="middle" className="axis">
              {x}
            </text>
          ))}
          {band ? <path d={band} className="band" /> : null}
          <path d={path(prr)} className="line prr" />
          <path d={path(app)} className="line app" />
          {prr.map((p) => (
            <circle key={p.key} cx={sx(p.x)} cy={sy(p.y)} r={2} className="dot prr">
              <title>{`${p.key} m: ${p.y.toFixed(3)} (${p.lo?.toFixed(3) ?? "?"}–${p.hi?.toFixed(3) ?? "?"}, 95 %) over ${p.n} receivers in range`}</title>
            </circle>
          ))}
          {app.map((p) => (
            <circle key={p.key} cx={sx(p.x)} cy={sy(p.y)} r={2} className="dot app">
              <title>{`${p.key} m: ${p.y.toFixed(3)} reached the application, over ${p.n} attempts`}</title>
            </circle>
          ))}
        </svg>
      )}
      <div className="axis-note">
        <span className="legend prr" title="3GPP TR 36.885 packet reception ratio: receivers truly within each 20 m of the sender that decoded the frame, with its 95 % Wilson band">
          pdr (PHY, 20 m)
        </span>
        <span className="legend app" title="Reached the receiver's application: after the PHY, the queues and the signature check, 50 m bins">
          delivery_ratio (app, 50 m)
        </span>
        <span className="faint">m</span>
      </div>
    </div>
  );
}

function LatencyStages(): React.JSX.Element {
  const load = useCallback(async () => {
    const types = (await grouped("e2e_latency", "msg_type")).filter((t) => t.value !== null && t.n > 0);
    const rows: { type: string; e2e: number; n: number; stages: Group[] }[] = [];
    for (const t of types.slice(0, 6)) {
      rows.push({ type: t.key, e2e: t.value as number, n: t.n, stages: await grouped("latency_stage", "stage", { msg_type: t.key }) });
    }
    return rows;
  }, []);
  const rows = usePolled(load, [] as { type: string; e2e: number; n: number; stages: Group[] }[]);
  const widest = Math.max(1e-9, ...rows.map((r) => r.stages.reduce((s, g) => s + (g.value ?? 0), 0)));
  return (
    <div className="plot-card wide breakdown" data-testid="breakdown-stages">
      <div className="title">
        <span>Where the delay goes</span>
        <span className="faint">mean ms per stage</span>
      </div>
      {rows.length === 0 ? (
        <p className="dim breakdown-empty">No delivered message to decompose yet.</p>
      ) : (
        <div className="stage-rows">
          {rows.map((r) => {
            const total = r.stages.reduce((s, g) => s + (g.value ?? 0), 0);
            return (
              <div key={r.type} className="stage-row" data-testid={`stages-${r.type}`}>
                <span className="mono stage-type">{r.type}</span>
                <span className="stage-bar" style={{ width: `${Math.max(2, (total / widest) * 170)}px` }}>
                  {STAGES.map((stage, i) => {
                    const g = r.stages.find((s) => s.key === stage);
                    const v = g?.value ?? 0;
                    if (v <= 0 || total <= 0) return null;
                    return (
                      <span
                        key={stage}
                        className="stage-seg"
                        style={{ width: `${(v / total) * 100}%`, background: STAGE_COLOURS[i] }}
                        title={`${r.type} ${stage}: ${v.toFixed(3)} ms mean (${((v / total) * 100).toFixed(1)} %)`}
                      />
                    );
                  })}
                </span>
                <span className="mono faint" title={`mean end-to-end delay over ${r.n} deliveries`}>
                  {r.e2e.toFixed(2)} ms
                </span>
              </div>
            );
          })}
        </div>
      )}
      <div className="stage-legend">
        {STAGES.map((s, i) => (
          <span key={s} className="faint">
            <i style={{ background: STAGE_COLOURS[i] }} />
            {s}
          </span>
        ))}
      </div>
    </div>
  );
}

type NodeColumn = "airtime" | "load" | "delivery";

function NodeRankings(): React.JSX.Element {
  const [sort, setSort] = useState<NodeColumn>("airtime");
  const load = useCallback(async () => {
    const [airtime, load, delivery] = await Promise.all([
      grouped("airtime_per_node", "node"),
      grouped("channel_load", "node"),
      grouped("delivery_ratio", "node"),
    ]);
    const by = (gs: Group[]) => new Map(gs.map((g) => [g.key, g]));
    return { airtime: by(airtime), load: by(load), delivery: by(delivery) };
  }, []);
  const data = usePolled(load, { airtime: new Map<string, Group>(), load: new Map<string, Group>(), delivery: new Map<string, Group>() });
  const rows = useMemo(() => {
    const keys = new Set([...data.airtime.keys(), ...data.load.keys(), ...data.delivery.keys()]);
    const list = [...keys].map((k) => ({
      node: Number(k),
      airtime: data.airtime.get(k)?.value ?? null,
      load: data.load.get(k)?.value ?? null,
      delivery: data.delivery.get(k)?.value ?? null,
      n: data.delivery.get(k)?.n ?? 0,
    }));
    // The worst first: most air time, most load, least delivery.
    const key = (r: (typeof list)[number]) =>
      sort === "delivery" ? -(r.delivery ?? Number.POSITIVE_INFINITY) : (r[sort] ?? Number.NEGATIVE_INFINITY);
    return list.sort((a, b) => key(b) - key(a)).slice(0, 12);
  }, [data, sort]);
  const follow = (node: number) => {
    for (const [actor, n] of engine.nodeByActor) {
      if (n === node) {
        void engine.selectActor(actor, "chase");
        return;
      }
    }
    void engine.selectNode(node);
  };
  const header = (col: NodeColumn, label: string, title: string) => (
    <th>
      <button type="button" className={`linklike${sort === col ? " active-sort" : ""}`} title={title} onClick={() => setSort(col)}>
        {label}
      </button>
    </th>
  );
  return (
    <div className="plot-card wide breakdown" data-testid="breakdown-nodes">
      <div className="title">
        <span>Nodes, worst first</span>
        <span className="faint">pooled over the run</span>
      </div>
      {rows.length === 0 ? (
        <p className="dim breakdown-empty">No per-node figure yet.</p>
      ) : (
        <table className="node-rank">
          <thead>
            <tr>
              <th>node</th>
              {header("airtime", "air ms/s", "Transmitted air time per second (airtime_per_node)")}
              {header("load", "load", "Channel load around the node: arrivals above −85 dBm plus its own offered air time, per unit time (channel_load)")}
              {header("delivery", "rx deliv.", "Of the attempts at this node as a receiver, the share that reached its applications (delivery_ratio)")}
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.node} onClick={() => follow(r.node)} title="Follow this node" data-testid={`rank-node-${r.node}`}>
                <td className="mono">{r.node}</td>
                <td className="mono">{r.airtime === null ? "—" : r.airtime.toFixed(2)}</td>
                <td className="mono">{r.load === null ? "—" : r.load.toFixed(3)}</td>
                <td className="mono" title={`${r.n} attempts`}>
                  {r.delivery === null ? "—" : r.delivery.toFixed(3)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

/** The fragmentation metrics a run reports, in the card's column order. */
const FRAG_COLUMNS = [
  ["frag_sdu_loss", "SDU loss", "Fragmented SDUs, per receiver in range, of which some fragment did not decode (frag_sdu_loss)"],
  ["frag_fragment_loss", "fragment", "Fragments that did not decode (frag_fragment_loss): what the SDU loss amplifies"],
  [
    "frag_sdu_loss_predicted",
    "PHY-predicted",
    "1 − Π(1 − p_i) with each p_i the PHY's loss probability for that fragment under the interference it met (frag_sdu_loss_predicted)",
  ],
  [
    "frag_sdu_loss_independent",
    "independent",
    "1 − (1 − p)^n with p the measured fragment loss: what independent fragment losses would give (frag_sdu_loss_independent)",
  ],
  ["frag_content_loss", "content", "Payload octets not received (frag_content_loss): what independent segments lose instead"],
] as const;

/**
 * Loss amplification (04-models.md §7.4), when the run fragments anything: the realised SDU loss
 * beside the per-fragment loss it amplifies and the two predictions. Nothing at all in a run that
 * never splits a message.
 */
function Fragmentation(): React.JSX.Element | null {
  const load = useCallback(async () => {
    const all = await Promise.all(FRAG_COLUMNS.map(([m]) => grouped(m, "msg_type")));
    return all.map((gs) => new Map(gs.map((g) => [g.key, g])));
  }, []);
  const data = usePolled(load, [] as Map<string, Group>[]);
  const types = useMemo(() => [...new Set(data.flatMap((m) => [...m.keys()]))].sort(), [data]);
  if (types.length === 0) return null;
  return (
    <div className="plot-card wide breakdown" data-testid="breakdown-fragmentation">
      <div className="title">
        <span>Fragmentation</span>
        <span className="faint">pooled over the run</span>
      </div>
      <table className="node-rank">
        <thead>
          <tr>
            <th>type</th>
            {FRAG_COLUMNS.map(([m, label, title]) => (
              <th key={m} title={title}>
                {label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {types.map((t) => (
            <tr key={t}>
              <td className="mono">{t}</td>
              {FRAG_COLUMNS.map(([m], i) => {
                const g = data[i]?.get(t);
                const band = g?.lo !== null && g?.lo !== undefined && g.hi !== null ? ` (${g.lo.toFixed(3)}–${g.hi.toFixed(3)})` : "";
                return (
                  <td key={m} className="mono" title={g ? `${g.n} samples${band}` : ""}>
                    {g?.value === null || g?.value === undefined ? "—" : g.value.toFixed(4)}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The breakdown cards, laid beside the plots. */
export function Breakdowns(): React.JSX.Element {
  return (
    <>
      <DeliveryByDistance />
      <LatencyStages />
      <NodeRankings />
      <Fragmentation />
    </>
  );
}
