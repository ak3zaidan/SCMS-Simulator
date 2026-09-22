//! The JSON bodies of §6.8–§6.12, answered from the stub engine's state.
//!
//! These are the result shapes the specification names, filled with the fixture's
//! numbers. They live apart from [`crate::stub`] because the *shapes* are normative and
//! the *numbers* are not: when `v2xw-engine` arrives, the shapes stay and the source of
//! the values changes.

use serde_json::{Value, json};
use v2xw_core::math;

use crate::engine::{Engine, Query};
use crate::error::{Result, ServerError};
use crate::stub::StubEngine;

/// The metric catalogue the fixture answers `metrics.query` with.
///
/// `visibility` follows §5.2: `ttc_min` is ground truth and therefore absent from a
/// `node`-profile answer, which is what conformance V3 checks.
pub const METRICS: [(&str, &str, &str, &str); 5] = [
    ("pdr", "-", "ratio", "DERIVED"),
    ("cbr", "-", "mean", "NODE"),
    ("pir_p95_s", "s", "p95", "NODE"),
    ("verify_wait_p95_ms", "ms", "p95", "NODE"),
    ("ttc_min", "s", "min", "GT"),
];

/// Answers one [`Query`] against the stub engine.
///
/// # Errors
/// `-32006` for an unknown id and `-32007` for an unknown metric, as the method schemas
/// of §6.8 and §6.12 list them.
pub fn answer(engine: &mut StubEngine, query: &Query) -> Result<Value> {
    let now = engine.sim_time();
    match query {
        Query::Node {
            node,
            t_ns,
            include,
            limit,
        } => {
            let hello = &engine.descriptor().hello;
            let row = hello
                .nodes
                .iter()
                .find(|r| r.node_id == node.index())
                .ok_or_else(|| ServerError::UnknownId {
                    kind: "node",
                    id: node.index().to_string(),
                })?;
            let strings = &hello.strings;
            let mut out = json!({
                "node": node.index(),
                "t_ns": t_ns.unwrap_or(now),
                "kind": node_kind(row.kind),
                "label": strings.get(row.str_label).unwrap_or(""),
                "profile_id": strings.get(row.str_profile_id).unwrap_or(""),
            });
            if row.actor_id != v2xw_record::wire::U32_NONE {
                out["actor"] = json!(row.actor_id);
            }
            for section in include {
                match section.as_str() {
                    "telemetry" => out["telemetry"] = telemetry_json(engine, node.index()),
                    "queues" => out["queues"] = queues_json(),
                    "neighbors" => out["neighbors"] = neighbours_json(node.index(), *limit),
                    "stores" => out["stores"] = stores_json(),
                    "certs" => out["certs"] = certs_json(node.index(), *limit),
                    "crl" => out["crl"] = json!({"entries": 12, "bytes": 4096, "i_period": 0}),
                    "gnss" => {
                        out["gnss"] = json!({"fix": "3D", "hdop": 1.1, "sigma_m": 1.6,
                                             "satellites": 11});
                    }
                    "clock" => {
                        out["clock"] = json!({"offset_ns": 0, "drift_ppm": 1.5,
                                              "source": "gnss-disciplined"});
                    }
                    "apps" => {
                        out["apps"] = json!([{"id": "fcw", "state": "armed", "warnings": 0},
                                             {"id": "eebl", "state": "armed", "warnings": 0}]);
                    }
                    "detectors" => {
                        out["detectors"] = json!([{"id": "detector/plausibility/position-jump",
                                                   "observations": 3, "prov_id": 5}]);
                    }
                    "provenance" => out["provenance"] = provenance_json(),
                    _ => {}
                }
            }
            Ok(out)
        }
        Query::Link {
            tx,
            rx,
            t_ns,
            window_ns,
        } => {
            let hello = &engine.descriptor().hello;
            for id in [tx, rx] {
                if !hello.nodes.iter().any(|r| r.node_id == id.index()) {
                    return Err(ServerError::UnknownId {
                        kind: "node",
                        id: id.index().to_string(),
                    });
                }
            }
            let a = hello.nodes.iter().find(|r| r.node_id == tx.index());
            let b = hello.nodes.iter().find(|r| r.node_id == rx.index());
            let distance = match (a, b) {
                (Some(a), Some(b)) => math::hypot(
                    f64::from(a.pos_m[0] - b.pos_m[0]),
                    f64::from(a.pos_m[1] - b.pos_m[1]),
                ),
                _ => f64::NAN,
            };
            // Free-space-shaped path loss at 5.9 GHz, so the number moves with distance
            // instead of being a constant. It is a fixture value, not a propagation model.
            let path_loss = 32.45
                + 20.0 * math::log10(5_900.0)
                + 20.0 * math::log10((distance / 1_000.0).max(1e-6));
            Ok(json!({
                "kind": "radio",
                "t_ns": t_ns.unwrap_or(now),
                "distance_m": math::quantize(distance, 3),
                "los": {"class": "LOS", "walls_crossed": 0, "obstructed_len_m": 0.0},
                "path_loss_db": math::quantize(path_loss, 3),
                "shadowing_db": 0.0,
                "fading_db": 0.0,
                "rx_power_dbm": math::quantize(20.0 - path_loss, 3),
                "sinr_db": 12.0,
                "pdr": 0.86,
                "pir_p95_s": 0.31,
                "frames": 10,
                "bytes": 3_500,
                "latency_ms": {"p50": 0.5, "p95": 1.4},
                "provenance": provenance_json(),
                "window_ns": window_ns,
            }))
        }
        Query::NamedLink { link, t_ns, .. } => Err(ServerError::UnknownId {
            kind: "link",
            id: format!(
                "{link} (no backend links in the fixture at t={})",
                t_ns.unwrap_or(now)
            ),
        }),
        Query::Entity { entity, t_ns, .. } => {
            const ROLES: [&str; 5] = ["ra", "pca", "ma", "crlg", "ea"];
            let role = entity.split(':').next().unwrap_or(entity);
            if !ROLES.contains(&role) {
                return Err(ServerError::UnknownId {
                    kind: "entity",
                    id: entity.clone(),
                });
            }
            Ok(json!({
                "entity": entity,
                "t_ns": t_ns.unwrap_or(now),
                "role": role,
                "state": {"requests_served": 0, "backlog": 0},
                "queue": {"depth": 0, "servers": 1, "utilisation": 0.0},
                "storage_bytes": 0,
                "open_cases": 0,
                "decisions": 0,
                "flows": [],
                "provenance": provenance_json(),
            }))
        }
        Query::Explain {
            subject,
            depth,
            markdown,
        } => {
            let kind = subject.get("kind").and_then(Value::as_str).unwrap_or("");
            let id = subject.get("id").and_then(Value::as_str).unwrap_or("");
            if kind == "metric" && !METRICS.iter().any(|(n, ..)| *n == id) {
                return Err(ServerError::UnknownMetric {
                    metric: id.to_string(),
                    did_you_mean: near_misses(id),
                });
            }
            let chain: Vec<Value> = provenance_json()
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(usize::from(*depth).max(1))
                .collect();
            let mut out = json!({
                "subject": subject,
                "chain": chain,
                "definition_md": format!("`{id}` — see the model card linked in the chain."),
                "caveats": ["produced by the server's synthetic fixture, not by an engine"],
            });
            if *markdown {
                out["markdown"] = json!(format!("## {id}\n\nProduced by the fixture.\n"));
            }
            Ok(out)
        }
        Query::MetricCatalogue => Ok(json!({
            "columns": [],
            "rows": [],
            "catalogue": METRICS.iter().map(|(name, unit, agg, vis)| json!({
                "name": name, "unit": unit, "dims": ["t"], "agg": agg, "visibility": vis,
                "definition_md": format!("`{name}` as defined in 08-measurement.")
            })).collect::<Vec<_>>(),
        })),
        Query::Metrics {
            metrics,
            t_from_ns,
            t_to_ns,
            bin_ns,
            group_by,
            limit,
        } => {
            for m in metrics {
                if !METRICS.iter().any(|(n, ..)| n == m) {
                    return Err(ServerError::UnknownMetric {
                        metric: m.clone(),
                        did_you_mean: near_misses(m),
                    });
                }
            }
            let from = t_from_ns.unwrap_or(0);
            let to = t_to_ns.unwrap_or(now).max(from);
            let bin = (*bin_ns).max(1);
            let mut columns = vec![json!({"name": "t_ns", "type": "time_ns", "unit": "ns",
                                          "visibility": "META"})];
            for m in metrics {
                let (_, unit, _, vis) = METRICS.iter().find(|(n, ..)| n == m).copied().unwrap();
                columns.push(json!({"name": m, "type": "float", "unit": unit,
                                    "visibility": vis}));
            }
            let mut rows = Vec::new();
            let mut t = from;
            while t <= to && rows.len() < *limit {
                let mut row = vec![json!(t)];
                let phase = (t as f64) * 1e-9 * 0.1;
                for m in metrics {
                    row.push(json!(math::quantize(metric_value(m, phase), 6)));
                }
                rows.push(Value::Array(row));
                t = t.saturating_add(bin);
            }
            let truncated = rows.len() >= *limit;
            Ok(json!({
                "columns": columns,
                "rows": rows,
                "truncated": truncated,
                "provenance": provenance_json(),
                "group_by": group_by,
            }))
        }
        Query::Plot { metrics, x, kind } => {
            let data: Vec<Value> = metrics
                .iter()
                .map(|m| {
                    let xs: Vec<f64> = (0..20).map(f64::from).collect();
                    let ys: Vec<f64> = xs
                        .iter()
                        .map(|v| math::quantize(metric_value(m, v * 0.1), 6))
                        .collect();
                    json!({"type": kind, "name": m, "x": xs, "y": ys})
                })
                .collect();
            Ok(json!({
                "figure": {"data": data,
                           "layout": {"xaxis": {"title": x}, "yaxis": {"title": "value"}}},
                "provenance": provenance_json(),
            }))
        }
        Query::ExportDataset {
            exporter,
            out_dir,
            visibility,
        } => Err(ServerError::ExportFailed {
            stage: "open".to_string(),
            detail: format!(
                "the fixture engine records nothing, so exporter `{exporter}` \
                 (visibility `{visibility}`) has no run to read; out_dir was {:?}. \
                 A real run writes a recording and this succeeds.",
                out_dir.as_deref().unwrap_or("(default)")
            ),
        }),
        Query::ExportRecording { path, profile } => Err(ServerError::ExportFailed {
            stage: "open".to_string(),
            detail: format!(
                "no recording to copy from the fixture engine (requested profile \
                 `{profile}`, path {:?})",
                path.as_deref().unwrap_or("(default)")
            ),
        }),
    }
}

fn metric_value(name: &str, phase: f64) -> f64 {
    let w = 0.5 + 0.5 * math::sin(phase);
    match name {
        "pdr" => 0.82 + 0.12 * w,
        "cbr" => 0.18 + 0.30 * w,
        "pir_p95_s" => 0.28 + 0.20 * w,
        "verify_wait_p95_ms" => 1.2 + 3.0 * w,
        "ttc_min" => 2.4 + 1.5 * w,
        _ => f64::NAN,
    }
}

/// Catalogue names sharing a prefix with `wanted`, for `-32007`'s `did_you_mean`.
fn near_misses(wanted: &str) -> Vec<String> {
    let head: String = wanted.chars().take(3).collect();
    METRICS
        .iter()
        .map(|(n, ..)| *n)
        .filter(|n| !head.is_empty() && n.starts_with(&head))
        .map(str::to_string)
        .collect()
}

fn node_kind(code: u8) -> &'static str {
    match code {
        0 => "obu",
        1 => "vru-device",
        2 => "rsu",
        3 => "base-station",
        4 => "router",
        5 => "backend-entity",
        _ => "other",
    }
}

fn telemetry_json(engine: &StubEngine, node_id: u32) -> Value {
    let _ = engine;
    json!({
        "msgs_in_per_s": {"value": 120.0, "unit": "1/s"},
        "msgs_out_per_s": {"value": 10.0, "unit": "1/s"},
        "verifications_per_s": {"value": 100.0, "unit": "1/s"},
        "cbr_pm": {"value": 340, "unit": "per-mille"},
        "cpu_util_pm": {"value": 420, "unit": "per-mille"},
        "node_id": node_id,
    })
}

fn queues_json() -> Value {
    json!({
        "rx": {"depth": 4, "p50": 2.0, "p95": 9.0, "policy": "drop-tail",
               "drops": {"overflow": 0}},
        "verify": {"depth": 3, "p50": 1.0, "p95": 7.0, "policy": "prioritized",
                   "drops": {"overflow": 0, "policy_skip": 2}},
        "tx": {"depth": 1, "p50": 1.0, "p95": 2.0, "policy": "edca",
               "drops": {"overflow": 0}}
    })
}

fn neighbours_json(node_id: u32, limit: usize) -> Value {
    let rows: Vec<Value> = (0..limit.min(8))
        .map(|i| {
            let d = v2xw_core::hash::sha256(&(node_id + i as u32).to_le_bytes());
            json!({
                "digest": v2xw_core::hash::hex_encode(&d[..8]),
                "verify_state": if i % 4 == 3 { "unverified" } else { "verified" },
                "last_seen_ns": 0,
                "distance_m": 35.0 + f64::from(i as u32) * 11.0,
                "relevance": 0.7,
                "messages": 10 + i,
            })
        })
        .collect();
    Value::Array(rows)
}

fn stores_json() -> Value {
    json!({
        "cert_store": {"entries": 20, "bytes": 8_192, "capacity": 100},
        "peer_cache": {"entries": 40, "bytes": 32_768, "capacity": 256},
        "crl_store": {"entries": 12, "bytes": 4_096},
        "trust_store": {"anchors": 2},
        "neighbor_table": {"entries": 24, "capacity": 128},
        "evidence_buffer": {"entries": 0, "bytes": 0},
        "report_outbox": {"entries": 0, "bytes": 0}
    })
}

fn certs_json(node_id: u32, limit: usize) -> Value {
    let rows: Vec<Value> = (0..limit.min(4))
        .map(|i| {
            let d = v2xw_core::hash::sha256(&(node_id * 31 + i as u32).to_le_bytes());
            json!({
                "cert_id": node_id * 13 + i as u32,
                "digest": v2xw_core::hash::hex_encode(&d[..8]),
                "kind": "pseudonym",
                "valid_from_ns": 0,
                "valid_until_ns": 300_000_000_000u64,
                "i": 0, "j": i,
            })
        })
        .collect();
    Value::Array(rows)
}

fn provenance_json() -> Value {
    json!([
        {"prov_id": 1, "model_id": "net/delivery/pdr-from-phy-rx", "model_version": "0.1.0",
         "param_set_id": "b3:stub-fixture", "family": "network",
         "card_url": "/cards/net-delivery-pdr-from-phy-rx",
         "assumptions": ["a transport fixture, not a model"]},
        {"prov_id": 2, "model_id": "radio/mac/cbr-window", "model_version": "0.1.0",
         "param_set_id": "b3:stub-fixture", "family": "radio",
         "card_url": "/cards/radio-mac-cbr-window"},
        {"prov_id": 3, "model_id": "node/hsm/ecdsa-p256-service-time", "model_version": "0.1.0",
         "param_set_id": "b3:stub-fixture", "family": "node",
         "card_url": "/cards/node-hsm-ecdsa-p256-service-time"}
    ])
}
