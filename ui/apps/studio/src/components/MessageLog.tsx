/**
 * What the followed vehicle is saying and hearing, message by message.
 *
 * The owner's ask: in the chase view, see the broadcast messages the vehicle sends and their
 * content, alongside its queues. This is the list under the inspector's state tab, refreshed once a
 * second while a vehicle is followed (`engine.inspectFollowed`). Each broadcast shows its message
 * count, temporary id, position, speed and heading as decoded from the octets that went on the air,
 * the pseudonym that signed it (a change is flagged on the frame where it happens), the octets by
 * layer and the radio parameters; a row expands to every decoded field. Receptions show who sent
 * them, what became of them and the end-to-end delay.
 */

import { useState } from "react";

import { useStudio } from "../state/store.js";
import { receivedRows, sentRows, shortPseudonym } from "../lib/messages.js";

const SHOWN = 12;

export function MessageLog(): React.JSX.Element | null {
  const inspect = useStudio((s) => s.inspect);
  const [open, setOpen] = useState<string | null>(null);
  const messages = inspect?.messages;
  if (!messages) return null;
  const sent = sentRows(messages.sent);
  const received = receivedRows(messages.received);
  const delivered = received.filter((r) => r.delivered).length;

  return (
    <>
      <div className="section" data-testid="message-log-sent">
        <h3>
          Broadcasts ({sent.length}
          {sent.length > 0 ? `, pseudonym ${shortPseudonym(sent[0].pseudonym)}` : ""})
        </h3>
        {sent.length === 0 ? (
          <p className="dim">Nothing sent yet by this radio in the part of the run played so far.</p>
        ) : (
          <table className="table msg-table">
            <thead>
              <tr>
                <th>time</th>
                <th>message</th>
                <th>content</th>
                <th>octets</th>
              </tr>
            </thead>
            <tbody>
              {sent.slice(0, SHOWN).map((r) => (
                <MessageRow key={r.key} row={r} open={open === r.key} onToggle={() => setOpen(open === r.key ? null : r.key)} />
              ))}
            </tbody>
          </table>
        )}
      </div>
      <div className="section" data-testid="message-log-received">
        <h3>
          Heard ({received.length}, {delivered} delivered)
        </h3>
        {received.length === 0 ? (
          <p className="dim">Nothing received yet.</p>
        ) : (
          <table className="table msg-table">
            <thead>
              <tr>
                <th>time</th>
                <th>from</th>
                <th>fate</th>
                <th>signal</th>
                <th>dist</th>
                <th>delay</th>
              </tr>
            </thead>
            <tbody>
              {received.slice(0, SHOWN).map((r) => (
                <tr key={r.key} className={r.delivered ? undefined : "msg-lost"}>
                  <td>{r.time.slice(3)}</td>
                  <td>
                    {r.from} <span className="faint">{r.type}</span>
                  </td>
                  <td>{r.fate}</td>
                  <td>{r.signal}</td>
                  <td>{r.distance}</td>
                  <td>{r.e2e}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </>
  );
}

function MessageRow({
  row,
  open,
  onToggle,
}: {
  row: ReturnType<typeof sentRows>[number];
  open: boolean;
  onToggle: () => void;
}): React.JSX.Element {
  return (
    <>
      <tr className={row.newPseudonym ? "msg-rotated" : undefined}>
        <td>{row.time.slice(3)}</td>
        <td>
          <button type="button" className="linklike" aria-expanded={open} onClick={onToggle} title="Show every field of this message">
            {row.type}
          </button>
          {row.newPseudonym ? <span className="msg-badge"> new pseudonym</span> : null}
        </td>
        <td style={{ textAlign: "left" }}>{row.summary}</td>
        <td title={row.layers}>{row.totalBytes} B</td>
      </tr>
      {open ? (
        <tr className="msg-detail">
          <td colSpan={4}>
            <dl className="kv">
              {row.fields.map(([k, v]) => (
                <FragmentKV key={k} k={k} v={v} />
              ))}
              <FragmentKV k="radio" v={row.radio} />
            </dl>
          </td>
        </tr>
      ) : null}
    </>
  );
}

function FragmentKV({ k, v }: { k: string; v: string }): React.JSX.Element {
  return (
    <>
      <dt>{k}</dt>
      <dd>{v}</dd>
    </>
  );
}
