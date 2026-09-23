//! The JSON bodies of §6.8–§6.12: one copy of every normative shape, filled by whichever
//! engine is serving.
//!
//! The *shapes* here are the specification's and the *numbers* are the engine's, so the
//! two are separated by a trait rather than by a second module. [`answer`] builds every
//! result; [`Introspect`] is the eight questions it has to ask the engine to do it. That
//! split is the reason a live run and the fixture cannot drift into answering `inspect.node`
//! with differently-shaped JSON — there is one `inspect.node` in this crate.
//!
//! # What "not known" looks like
//!
//! An engine that cannot answer one of [`Introspect`]'s questions returns `None`, and the
//! section is then **absent from the result** rather than present and invented. §6.8's
//! `include` list is a request, not a promise, and a client that asked for `stores` and got
//! no `stores` key has been told something true. The alternative — a plausible number with
//! no source — is the failure mode this whole codebase is arranged against.

use serde_json::{Value, json};
use v2xw_core::math;
use v2xw_record::wire::telemetry::NodeTelemetry;

use crate::engine::{Engine, NodeFacts, Query};
use crate::error::{Result, ServerError};

/// One metric an engine can answer for: its catalogue row and its wire ids.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricInfo {
    /// The metric's name, e.g. `pdr`.
    pub name: String,
    /// Its unit, e.g. `-`, `s`, `ms`.
    pub unit: String,
    /// The aggregation tag, e.g. `ratio`, `p95`.
    pub agg: String,
    /// The `#/$defs/Visibility` token.
    pub visibility: String,
    /// The definition, in Markdown, including the formula.
    pub definition_md: String,
    /// The dimensions it is broken down by.
    pub dims: Vec<String>,
    /// What the metric does not account for.
    pub not_accounted: Vec<String>,
    /// The symbol-table id of `name`, for `MetricSample.str_metric` (§3.7).
    pub str_id: u32,
    /// §3.7's `MetricAgg` code for `agg`.
    pub agg_code: u16,
    /// The citation for the definition: a standard, a paper or a design section.
    pub source: String,
    /// The metric this series is a view of: `name` itself for the headline series, the
    /// metric's name for a percentile (`e2e_latency.p95`) or a breakdown
    /// (`latency_stage[airtime]`).
    pub base: String,
}

/// What [`answer`] needs from the engine that is serving the run.
///
/// Every method that can be unanswerable returns `Option`; see the module header.
pub trait Introspect: Engine {
    /// Every node the run has, now.
    fn node_list(&self) -> Vec<NodeFacts>;

    /// One node's most recent telemetry row, if the run has one for it.
    fn telemetry_of(&self, node: u32) -> Option<NodeTelemetry>;

    /// One metric binned over `[from, to]`, as `(bin start, value)`; `None` for a bin with
    /// no observation, which is reported as `null` and is not the same as zero.
    fn metric_series(
        &self,
        name: &str,
        from: u64,
        to: u64,
        bin: u64,
        limit: usize,
    ) -> Vec<(u64, Option<f64>)>;

    /// The provenance chain every answer cites (§3.8, §6.9).
    fn provenance_chain(&self) -> Vec<Value>;

    /// What a reader of these numbers must be told about them.
    fn caveats(&self) -> Vec<String>;

    /// One `include` section of `inspect.node`, or `None` if the run does not know it.
    fn node_section(&self, node: u32, section: &str, limit: usize) -> Option<Value>;

    /// Measured facts about one radio link over `[t_ns − window_ns, t_ns]`, or `None` if
    /// the run has observed no reception on it.
    fn link_facts(&self, tx: u32, rx: u32, t_ns: u64, window_ns: u64) -> Option<Value>;

    /// Answers [`Query::ExportDataset`] and [`Query::ExportRecording`].
    ///
    /// # Errors
    /// `-32008` when the export cannot be performed, with the stage it failed at.
    fn export(&mut self, query: &Query) -> Result<Value>;

    /// The backend entity `inspect.entity` names, or `None` if the run has no backend.
    ///
    /// This build's kernel schedules no `Event::NetDeliver`, so no run has one yet; the
    /// default says so rather than each engine repeating it.
    fn entity_facts(&self, _entity: &str, _t_ns: u64, _limit: usize) -> Option<Value> {
        None
    }
}

/// Answers one [`Query`].
///
/// # Errors
/// `-32006` for an unknown id, `-32007` for an unknown metric, `-32008` for a failed
/// export and `-32009` where the query does not apply to this kind of run, as the method
/// schemas of §6.8–§6.12 list them.
pub fn answer<E: Introspect + ?Sized>(engine: &mut E, query: &Query) -> Result<Value> {
    let now = engine.sim_time();
    match query {
        Query::Node {
            node,
            t_ns,
            include,
            limit,
        } => {
            let id = node.index();
            let row = engine
                .node_list()
                .into_iter()
                .find(|r| r.node_id == id)
                .ok_or_else(|| ServerError::UnknownId {
                    kind: "node",
                    id: id.to_string(),
                })?;
            let mut out = json!({
                "node": id,
                "t_ns": t_ns.unwrap_or(now),
                "kind": node_kind(row.kind),
                "label": row.label,
                "profile_id": row.profile_id,
            });
            if row.actor_id != v2xw_record::wire::U32_NONE {
                out["actor"] = json!(row.actor_id);
            }
            for section in include {
                let value = match section.as_str() {
                    "telemetry" => engine.telemetry_of(id).map(telemetry_json),
                    "provenance" => Some(Value::Array(engine.provenance_chain())),
                    other => engine.node_section(id, other, *limit),
                };
                if let Some(value) = value {
                    out[section.as_str()] = value;
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
            let nodes = engine.node_list();
            for id in [tx, rx] {
                if !nodes.iter().any(|r| r.node_id == id.index()) {
                    return Err(ServerError::UnknownId {
                        kind: "node",
                        id: id.index().to_string(),
                    });
                }
            }
            let at = t_ns.unwrap_or(now);
            let mut out = json!({
                "kind": "radio",
                "t_ns": at,
                "window_ns": window_ns,
                "provenance": engine.provenance_chain(),
            });
            match engine.link_facts(tx.index(), rx.index(), at, *window_ns) {
                Some(Value::Object(facts)) => {
                    for (k, v) in facts {
                        out[k] = v;
                    }
                }
                _ => {
                    // Not an error: a pair of nodes that has exchanged nothing in the
                    // window is a real answer, and `frames: 0` is what it looks like. The
                    // geometry is still reported, because it is known.
                    let a = nodes.iter().find(|r| r.node_id == tx.index());
                    let b = nodes.iter().find(|r| r.node_id == rx.index());
                    let distance = match (a, b) {
                        (Some(a), Some(b)) => math::hypot(
                            f64::from(a.pos_m[0] - b.pos_m[0]),
                            f64::from(a.pos_m[1] - b.pos_m[1]),
                        ),
                        _ => f64::NAN,
                    };
                    out["frames"] = json!(0);
                    out["distance_m"] = json!(math::quantize(distance, 3));
                    out["note"] = json!(
                        "no reception attempt on this pair inside the window: the \
                         transmitter was out of the modelled candidate range, or did not \
                         transmit"
                    );
                }
            }
            Ok(out)
        }
        Query::NamedLink { link, t_ns, .. } => Err(ServerError::UnknownId {
            kind: "link",
            id: format!(
                "{link}: this build schedules no backend or backhaul delivery, so there \
                 are no named links at t={}",
                t_ns.unwrap_or(now)
            ),
        }),
        Query::Entity {
            entity,
            t_ns,
            limit,
        } => {
            const ROLES: [&str; 5] = ["ra", "pca", "ma", "crlg", "ea"];
            let role = entity.split(':').next().unwrap_or(entity);
            if !ROLES.contains(&role) {
                return Err(ServerError::UnknownId {
                    kind: "entity",
                    id: entity.clone(),
                });
            }
            let at = t_ns.unwrap_or(now);
            match engine.entity_facts(entity, at, *limit) {
                Some(facts) => Ok(facts),
                None => Err(ServerError::NotSupportedHere(format!(
                    "`{entity}` is a backend role and this run has no backend: the kernel \
                     schedules no `NetDeliver` or `FlowTimer` event, so there is nothing \
                     at t={at} to report. The seam exists; the model does not."
                ))),
            }
        }
        Query::Explain {
            subject,
            depth,
            markdown,
        } => {
            let kind = subject.get("kind").and_then(Value::as_str).unwrap_or("");
            let id = subject.get("id").and_then(Value::as_str).unwrap_or("");
            let catalogue = engine.metric_catalogue();
            if kind == "metric" && !catalogue.iter().any(|m| m.name == id) {
                return Err(ServerError::UnknownMetric {
                    metric: id.to_string(),
                    did_you_mean: near_misses(&catalogue, id),
                });
            }
            let chain: Vec<Value> = relevant_chain(engine.provenance_chain(), kind)
                .into_iter()
                .take(usize::from(*depth).max(1))
                .collect();
            let definition = catalogue
                .iter()
                .find(|m| m.name == id)
                .map(|m| m.definition_md.clone())
                .unwrap_or_else(|| format!("`{id}` — see the model card linked in the chain."));
            let mut caveats = engine.caveats();
            if let Some(metric) = catalogue.iter().find(|m| m.name == id) {
                caveats.extend(metric.not_accounted.iter().cloned());
            }
            let mut out = json!({
                "subject": subject,
                "chain": chain,
                "definition_md": definition,
                "caveats": caveats,
            });
            if *markdown {
                let chain_md = out["chain"]
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .map(|r| {
                                format!(
                                    "- `{}` {} — {}\n",
                                    r.get("model_id").and_then(Value::as_str).unwrap_or("?"),
                                    r.get("model_version")
                                        .and_then(Value::as_str)
                                        .unwrap_or("?"),
                                    r.get("card_url").and_then(Value::as_str).unwrap_or("")
                                )
                            })
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                out["markdown"] = json!(format!(
                    "## {id}\n\n{}\n\n### Models\n\n{chain_md}",
                    out["definition_md"].as_str().unwrap_or("")
                ));
            }
            Ok(out)
        }
        Query::MetricCatalogue => Ok(json!({
            "columns": [],
            "rows": [],
            "catalogue": engine.metric_catalogue().iter().map(|m| json!({
                "name": m.name,
                "unit": m.unit,
                "dims": m.dims,
                "agg": m.agg,
                "visibility": m.visibility,
                "definition_md": m.definition_md,
                "not_accounted": m.not_accounted,
                // The card's `Source` shape (`{"ref": …}`), which is what
                // `MetricsQueryResult.catalogue[].source` declares.
                "source": if m.source.is_empty() {
                    serde_json::Value::Null
                } else {
                    json!({ "ref": m.source })
                },
                "base": m.base,
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
            let catalogue = engine.metric_catalogue();
            for m in metrics {
                if !catalogue.iter().any(|row| &row.name == m) {
                    return Err(ServerError::UnknownMetric {
                        metric: m.clone(),
                        did_you_mean: near_misses(&catalogue, m),
                    });
                }
            }
            let from = t_from_ns.unwrap_or(0);
            let to = t_to_ns.unwrap_or(now).max(from);
            let bin = (*bin_ns).max(1);
            let mut columns = vec![json!({"name": "t_ns", "type": "time_ns", "unit": "ns",
                                          "visibility": "META"})];
            let mut series = Vec::with_capacity(metrics.len());
            for m in metrics {
                let row = catalogue
                    .iter()
                    .find(|row| &row.name == m)
                    .expect("checked above");
                columns.push(json!({"name": m, "type": "float", "unit": row.unit,
                                    "visibility": row.visibility}));
                series.push(engine.metric_series(m, from, to, bin, *limit));
            }
            let depth = series.iter().map(Vec::len).min().unwrap_or(0);
            let mut rows = Vec::with_capacity(depth);
            for i in 0..depth {
                let mut row = vec![json!(series[0][i].0)];
                for column in &series {
                    row.push(match column[i].1 {
                        Some(v) => json!(math::quantize(v, 6)),
                        None => Value::Null,
                    });
                }
                rows.push(Value::Array(row));
            }
            let truncated = rows.len() >= *limit;
            Ok(json!({
                "columns": columns,
                "rows": rows,
                "truncated": truncated,
                "provenance": relevant_chain(engine.provenance_chain(), "metric"),
                "group_by": group_by,
            }))
        }
        Query::Plot { metrics, x, kind } => {
            let catalogue = engine.metric_catalogue();
            for m in metrics {
                if !catalogue.iter().any(|row| &row.name == m) {
                    return Err(ServerError::UnknownMetric {
                        metric: m.clone(),
                        did_you_mean: near_misses(&catalogue, m),
                    });
                }
            }
            let to = now.max(1);
            let data: Vec<Value> = metrics
                .iter()
                .map(|m| {
                    let series = engine.metric_series(m, 0, to, 1_000_000_000, 10_000);
                    let xs: Vec<f64> = series.iter().map(|(t, _)| (*t as f64) * 1e-9).collect();
                    let ys: Vec<Value> = series
                        .iter()
                        .map(|(_, v)| match v {
                            Some(v) => json!(math::quantize(*v, 6)),
                            None => Value::Null,
                        })
                        .collect();
                    json!({"type": kind, "name": m, "x": xs, "y": ys})
                })
                .collect();
            Ok(json!({
                "figure": {"data": data,
                           "layout": {"xaxis": {"title": x}, "yaxis": {"title": "value"}}},
                "provenance": relevant_chain(engine.provenance_chain(), "metric"),
            }))
        }
        export @ (Query::ExportDataset { .. } | Query::ExportRecording { .. }) => {
            engine.export(export)
        }
    }
}

/// The provenance chain with the models that could have produced `kind` first.
///
/// A run registers every model it *could* use, and a live run's chain is therefore all of
/// them — thirty-four for the Phase 1 scenario. §6.9's `depth` truncates the chain, so the
/// order decides what a caller sees, and the first entry has to be one that plausibly
/// produced the subject rather than whichever model happened to register first.
fn relevant_chain(chain: Vec<Value>, kind: &str) -> Vec<Value> {
    let wanted: &[&str] = match kind {
        "metric" => &["metric"],
        "node" => &["hardware-profile", "verification-policy", "clock", "gnss"],
        "link" => &["propagation", "fading", "obstacle", "phy", "mac", "dcc"],
        "actor" => &["mobility", "vru"],
        _ => return chain,
    };
    let (mut first, rest): (Vec<Value>, Vec<Value>) = chain.into_iter().partition(|entry| {
        entry
            .get("family")
            .and_then(Value::as_str)
            .is_some_and(|f| wanted.contains(&f))
    });
    first.extend(rest);
    first
}

/// Catalogue names sharing a prefix with `wanted`, for `-32007`'s `did_you_mean`.
fn near_misses(catalogue: &[MetricInfo], wanted: &str) -> Vec<String> {
    let head: String = wanted.chars().take(3).collect();
    catalogue
        .iter()
        .filter(|m| !head.is_empty() && m.name.starts_with(&head))
        .map(|m| m.name.clone())
        .collect()
}

/// §3.1.3's `kind` as the `#/$defs/NodeKind` token.
pub fn node_kind(code: u8) -> &'static str {
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

/// One telemetry row as §6.8's `telemetry` section.
///
/// A field at its §3.5.2 unknown sentinel is omitted, so a client reading this JSON sees
/// only what the run measured. `f32::NAN` and `u16::MAX`-style sentinels are exactly the
/// values [`NodeTelemetry::unknown`] writes, so the test for "is it known" is the same one
/// the wire format uses.
pub fn telemetry_json(row: NodeTelemetry) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("node_id".to_string(), json!(row.node_id));
    let mut rate = |name: &str, v: f32, unit: &str| {
        if v.is_finite() {
            out.insert(
                name.to_string(),
                json!({"value": math::quantize(f64::from(v), 3), "unit": unit}),
            );
        }
    };
    rate("msgs_in_per_s", row.msgs_in_per_s, "1/s");
    rate("msgs_out_per_s", row.msgs_out_per_s, "1/s");
    rate("verifications_per_s", row.verifications_per_s, "1/s");
    rate("verify_wait_p50_ms", row.verify_wait_p50_ms, "ms");
    rate("verify_wait_p95_ms", row.verify_wait_p95_ms, "ms");
    rate("airtime_ms_per_s", row.airtime_ms_per_s, "ms/s");
    rate("pos_error_m", row.pos_error_m, "m");
    rate("gnss_sigma_m", row.gnss_sigma_m, "m");
    let mut permille = |name: &str, v: u16| {
        if v != u16::MAX {
            out.insert(name.to_string(), json!({"value": v, "unit": "per-mille"}));
        }
    };
    permille("cbr_pm", row.cbr_pm);
    permille("cpu_util_pm", row.cpu_util_pm);
    permille("hsm_util_pm", row.hsm_util_pm);
    if row.full_cert_msgs != u32::MAX {
        out.insert(
            "full_cert_msgs".to_string(),
            json!({"value": row.full_cert_msgs, "unit": "-"}),
        );
    }
    if row.tx_power_cdbm != i16::MIN {
        out.insert(
            "tx_power_dbm".to_string(),
            json!({"value": math::quantize(f64::from(row.tx_power_cdbm) / 100.0, 2),
                   "unit": "dBm"}),
        );
    }
    Value::Object(out)
}
