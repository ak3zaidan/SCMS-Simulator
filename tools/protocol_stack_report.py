"""Turn a directory of `protocol_stack_measure` arm records into the tables the write-up carries.

Reads `<tag>_<arm>.json` records and emits Markdown. Every column is a measured field of a record;
nothing is recomputed here except differences between two arms, which are labelled as such.

    python tools/protocol_stack_report.py --dir C:/Temp/pstack --tag intas --base base
"""
from __future__ import annotations

import argparse
import glob
import json
import os


def load(dirname: str, tag: str) -> dict:
    out = {}
    for p in sorted(glob.glob(os.path.join(dirname, f"{tag}_*.json"))):
        try:
            d = json.load(open(p, encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(d, dict) and "arm" in d:
            out[d["arm"]] = d
    return out


def _g(d, *path, default=None):
    cur = d
    for k in path:
        if not isinstance(cur, dict) or k not in cur:
            return default
        cur = cur[k]
    return cur


def _f(v, nd=3, dash="-"):
    return dash if v is None else (f"{v:.{nd}f}" if isinstance(v, float) else str(v))


def cost_table(arms: dict, base: str) -> list[str]:
    """CPU time first: it is what survives a co-tenant. Wall is printed beside it so the two can be
    compared -- on a serial pass they agree to well under a second, and where they do not, the box
    was not idle and the row should be re-measured."""
    b = arms.get(base)
    rows = ["| arm | CPU s | wall s | x base | peak RSS MiB | frames on air | us/frame | digest |",
            "|---|---|---|---|---|---|---|---|"]
    for name, d in sorted(arms.items(), key=lambda kv: kv[1]["cpu_s"]):
        cams = _g(d, "gap", "cams")
        per = (d["cpu_s"] / cams * 1e6) if cams else None
        mult = (d["cpu_s"] / b["cpu_s"]) if (b and b.get("cpu_s")) else None
        rows.append(f"| `{name}` | {d['cpu_s']:.1f} | {d['wall_s']:.1f} | {_f(mult, 2)} | "
                    f"{d['peak_rss_mb']:.0f} | {cams} | {_f(per, 1)} | "
                    f"`{d['data_digest'][:12]}` |")
    return rows


def cam_table(arms: dict) -> list[str]:
    rows = ["| arm | CAMs | mean gap s | rate Hz | dynamics | first | position | heading | speed | "
            "heartbeat |", "|---|---|---|---|---|---|---|---|---|---|"]
    for name, d in sorted(arms.items()):
        c = _g(d, "protocol", "cam_generation")
        if not c:
            continue
        t = c.get("triggers", {})
        rows.append(f"| `{name}` | {c['cams']} | {c['mean_gap_s']:.4f} | {c['mean_rate_hz']:.4f} | "
                    f"{c['dynamics_share']*100:.2f}% | {t.get('first',0)} | {t.get('position',0)} | "
                    f"{t.get('heading',0)} | {t.get('speed',0)} | {t.get('heartbeat',0)} |")
    return rows


def cbr_table(arms: dict) -> list[str]:
    rows = ["| arm | CBR mean | CBR max | samples | DCC states | wire B |",
            "|---|---|---|---|---|---|"]
    for name, d in sorted(arms.items()):
        c = _g(d, "protocol", "cbr")
        if not c:
            continue
        dcc = _g(d, "protocol", "dcc", "states")
        rows.append(f"| `{name}` | {c['mean']:.6f} | {c['max']:.6f} | {c['samples']} | "
                    f"{json.dumps(dcc) if dcc else '-'} | "
                    f"{_f(_g(d,'protocol','wire','mean_wire_bytes'), 1)} |")
    return rows


def latency_table(arms: dict) -> list[str]:
    rows = ["| arm | mean ms | min | p50 | p90 | p99 | max | samples | band ms |",
            "|---|---|---|---|---|---|---|---|---|"]
    for name, d in sorted(arms.items()):
        lat = _g(d, "protocol", "latency_ms")
        if not lat:
            continue
        rows.append(f"| `{name}` | {lat['mean']:.4f} | {lat['min']:.4f} | {lat['p50']:.4f} | "
                    f"{lat['p90']:.4f} | {lat['p99']:.4f} | {lat['max']:.4f} | {lat['samples']} | "
                    f"{lat['reference_band_ms']} |")
    return rows


def security_table(arms: dict) -> list[str]:
    rows = ["| arm | signatures | logical verifications | computed | cache hit | mode | devices |",
            "|---|---|---|---|---|---|---|"]
    for name, d in sorted(arms.items()):
        s = _g(d, "protocol", "security")
        if not s:
            continue
        rows.append(f"| `{name}` | {s.get('signatures_computed')} | "
                    f"{s.get('logical_verifications')} | {s.get('computed_verifications')} | "
                    f"{_f(s.get('hit_rate'), 4)} | {s.get('signing_mode')} | "
                    f"{s.get('devices_provisioned')} |")
    return rows


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--dir", default="C:/Temp/pstack")
    p.add_argument("--tag", default="intas")
    p.add_argument("--base", default="base")
    a = p.parse_args(argv)
    arms = load(a.dir, a.tag)
    if not arms:
        print(f"no arm records in {a.dir} for tag {a.tag!r}")
        return 1
    print(f"## {a.tag} -- {len(arms)} arms\n")
    for title, fn in (("Cost", cost_table), ("CAM generation", cam_table),
                      ("Channel load", cbr_table), ("Latency", latency_table),
                      ("Security", security_table)):
        lines = fn(arms, a.base) if fn is cost_table else fn(arms)
        if len(lines) > 2:
            print(f"### {title}\n")
            print("\n".join(lines))
            print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
