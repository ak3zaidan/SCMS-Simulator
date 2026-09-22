//! The results table: one row per (cell, metric, bin), in the formats the exporters
//! already produce.
//!
//! 08-measurement-and-data.md §4: "results land in one Parquet table with cell keys as
//! columns". That is what this is — one long-form table, a column per swept parameter, so
//! the common query ("`pdr` against vehicle count, per protocol") is a `GROUP BY` and not
//! a join across files.
//!
//! # Written through `v2xw-record`, not beside it
//!
//! Parquet, Arrow IPC and JSONL all go through [`v2xw_record::export`], which is the one
//! place in this repository where a float becomes a stored value and the one place that
//! quantises it to its column's declared grid (build decision D9). This module declares the
//! grid for each of its float columns and then gets out of the way; it does not format a
//! number itself.
//!
//! The estimates sit on [`v2xw_record::grid::Q_METRIC_VALUE`], 1e-6, which is the finest
//! grid D9 lists. That is the safe direction for a table whose rows come from different
//! metrics with different grids: a value already on a coarser grid is bit-for-bit unchanged
//! by a finer one, while declaring a coarse grid here would destroy precision a metric's own
//! contract promised.
//!
//! # A swept parameter is a text column
//!
//! A sweep value can be a number, a string, a boolean or an object — `security.protocol.id`
//! is a string and `actors.vehicles.demand.target_count` is an integer, and they are
//! columns of one table. Each is therefore stored as its **compact JSON**, which round-trips
//! all four and sorts stably. A reader that wants the number parses one column; a reader
//! that wants to group does not care.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use v2xw_record::export::schema::{ColumnKind, ColumnSpec, TableSchema};
use v2xw_record::export::{ExportFormat, ExportedFile, arrow_ipc, jsonl, parquet, table};
use v2xw_record::grid;

use crate::aggregate::CellAggregate;
use crate::error::{ExperimentError, Result};
use crate::plan::ExperimentPlan;

/// The schema id the results table carries.
pub const RESULTS_SCHEMA: &str = "v2xw/experiment-results/1";

/// The stem every results file is named with.
pub const RESULTS_STEM: &str = "results";

/// The prefix a swept parameter's column name carries, so it cannot collide with one of
/// the table's own columns.
pub const PARAM_PREFIX: &str = "param_";

/// The aggregated results of a sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultsTable {
    /// The schema id, [`RESULTS_SCHEMA`].
    pub schema: String,
    /// The experiment's name.
    pub experiment: String,
    /// The plan digest, which identifies the sweep the rows came from.
    pub plan_digest: String,
    /// The swept axes, in path order — the parameter columns, before prefixing.
    pub axes: Vec<String>,
    /// The rows, in cell order then metric-key order.
    pub rows: Vec<CellAggregate>,
}

impl ResultsTable {
    /// Assembles a table from a plan and its aggregated rows.
    #[must_use]
    pub fn new(plan: &ExperimentPlan, rows: Vec<CellAggregate>) -> ResultsTable {
        ResultsTable {
            schema: RESULTS_SCHEMA.to_string(),
            experiment: plan.name.clone(),
            plan_digest: plan.digest.clone(),
            axes: plan.axes.clone(),
            rows,
        }
    }

    /// The table's columns, with every float column's grid declared.
    ///
    /// The parameter columns come first, then the identity of the row, then the estimate.
    #[must_use]
    pub fn table_schema(&self) -> TableSchema {
        let mut columns = vec![
            int("cell_index"),
            text("cell_index_label"),
            text("cell_label"),
        ];
        for axis in &self.axes {
            columns.push(text(&param_column(axis)));
        }
        columns.extend([
            text("metric"),
            text("unit"),
            text("dims"),
            text("agg"),
            int("replications"),
            int("replications_with_value"),
            int("samples"),
            float("mean"),
            float("ci_lo"),
            float("ci_hi"),
            text("ci_level"),
            text("ci_method"),
            float("stddev"),
            float("min"),
            float("max"),
        ]);
        TableSchema {
            schema: RESULTS_SCHEMA.to_string(),
            channel: "experiment.results".to_string(),
            // Derived: every value here is a reduction of `metric.sample`, which is itself
            // `derived`. No ground-truth column exists, so no `NODE-only` projection is
            // needed and none is offered.
            visibility: "derived".to_string(),
            columns,
        }
    }

    /// The rows as `(sim_time_ns, json)` pairs, which is what the exporters take.
    ///
    /// The instant is zero for every row and the schema declares no `sim_time_ns` column,
    /// so it never reaches a file: an aggregate over a whole run has no instant, and a
    /// column of zeros claiming to be one would be worse than none.
    #[must_use]
    pub fn export_rows(&self) -> Vec<(u64, Vec<u8>)> {
        let mut out = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let json = serde_json::to_vec(&self.row_object(row)).unwrap_or_else(|_| b"{}".to_vec());
            out.push((0u64, json));
        }
        out
    }

    /// One row as the flat JSON object the exporters read.
    fn row_object(&self, row: &CellAggregate) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("cell_index".to_string(), Value::from(row.cell.index as u64));
        object.insert("cell_label".to_string(), Value::from(row.cell.label()));
        // `cell_index_label` is the zero-padded form, so a spreadsheet that reads the
        // integer column as a number still has something that sorts as text.
        object.insert(
            "cell_index_label".to_string(),
            Value::from(format!("{:04}", row.cell.index)),
        );
        for axis in &self.axes {
            let value = row
                .cell
                .values
                .get(axis)
                .map(|v| v.to_string())
                .unwrap_or_default();
            object.insert(param_column(axis), Value::from(value));
        }
        object.insert("metric".to_string(), Value::from(row.metric.clone()));
        object.insert("unit".to_string(), Value::from(row.unit.clone()));
        object.insert("dims".to_string(), Value::from(row.dims.clone()));
        object.insert("agg".to_string(), Value::from(row.agg.clone()));
        object.insert("replications".to_string(), Value::from(row.replications));
        object.insert(
            "replications_with_value".to_string(),
            Value::from(row.replications_with_value),
        );
        object.insert("samples".to_string(), Value::from(row.samples));
        insert_float(&mut object, "mean", row.mean);
        insert_float(&mut object, "ci_lo", row.ci_lo);
        insert_float(&mut object, "ci_hi", row.ci_hi);
        object.insert(
            "ci_level".to_string(),
            Value::from(row.ci_level.to_string()),
        );
        object.insert(
            "ci_method".to_string(),
            Value::from(row.ci_method.to_string()),
        );
        insert_float(&mut object, "stddev", row.stddev);
        insert_float(&mut object, "min", row.min);
        insert_float(&mut object, "max", row.max);
        Value::Object(object)
    }

    /// Writes `results.json`: the whole table with its plan digest, unflattened.
    ///
    /// Returns the file names written, relative to `dir`.
    ///
    /// # Errors
    /// [`ExperimentError::Io`] if the file cannot be written and
    /// [`ExperimentError::Json`] if the table will not serialise.
    pub fn write_json(&self, dir: &Path) -> Result<Vec<String>> {
        let name = format!("{RESULTS_STEM}.json");
        let path = dir.join(&name);
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| ExperimentError::json("the results table", e))?;
        std::fs::write(&path, &bytes)
            .map_err(|e| ExperimentError::io("cannot write the results table", &path, e))?;
        Ok(vec![name])
    }

    /// Writes the table in one of the exporters' tabular formats, plus its `schema.json`
    /// sidecar, exactly as [`v2xw_record::export::Exporter`] does for a channel.
    ///
    /// # Errors
    /// [`ExperimentError::Record`] if the writer refuses the batch,
    /// [`ExperimentError::Io`] if a file cannot be written, and
    /// [`ExperimentError::Json`] if the sidecar will not serialise.
    pub fn write_table(&self, dir: &Path, format: ExportFormat) -> Result<Vec<ExportedFile>> {
        if format == ExportFormat::Json {
            let names = self.write_json(dir)?;
            let path = dir.join(&names[0]);
            let bytes = std::fs::metadata(&path)
                .map_err(|e| ExperimentError::io("cannot stat the results table", &path, e))?
                .len();
            return Ok(vec![ExportedFile {
                path,
                format: ExportFormat::Json,
                channel: Some("experiment.results".to_string()),
                rows: self.rows.len(),
                bytes,
                schema: None,
            }]);
        }

        let schema = self.table_schema();
        let rows = self.export_rows();
        let data_path = dir.join(format!("{RESULTS_STEM}.{}", format.extension()));
        let bytes = match format {
            ExportFormat::Parquet => {
                let batch = table::build_batch(&schema, &rows)?;
                parquet::write(&data_path, &batch)?
            }
            ExportFormat::ArrowIpc => {
                let batch = table::build_batch(&schema, &rows)?;
                arrow_ipc::write(&data_path, &[batch])?
            }
            ExportFormat::Jsonl => jsonl::write(&data_path, &schema, &rows)?,
            // Handled above; kept as an arm rather than an `unreachable!` because this
            // crate forbids nothing it can simply answer.
            ExportFormat::Json => 0,
        };
        let schema_path = dir.join(format!("{RESULTS_STEM}.schema.json"));
        let schema_bytes = serde_json::to_vec_pretty(&schema)
            .map_err(|e| ExperimentError::json("the results schema", e))?;
        std::fs::write(&schema_path, &schema_bytes)
            .map_err(|e| ExperimentError::io("cannot write the results schema", &schema_path, e))?;
        Ok(vec![
            ExportedFile {
                path: data_path,
                format,
                channel: Some("experiment.results".to_string()),
                rows: rows.len(),
                bytes,
                schema: Some(schema),
            },
            ExportedFile {
                path: schema_path,
                format: ExportFormat::Json,
                channel: Some("experiment.results".to_string()),
                rows: 0,
                bytes: schema_bytes.len() as u64,
                schema: None,
            },
        ])
    }

    /// The rows grouped by metric name, for a human-readable report.
    #[must_use]
    pub fn by_metric(&self) -> BTreeMap<String, Vec<&CellAggregate>> {
        let mut out: BTreeMap<String, Vec<&CellAggregate>> = BTreeMap::new();
        for row in &self.rows {
            out.entry(row.metric.clone()).or_default().push(row);
        }
        out
    }
}

/// The column name a swept path gets: `param_actors_vehicles_demand_rate_veh_per_h`.
///
/// Dots and brackets become underscores, because a column name with a dot in it is a
/// column name every query engine needs quoting for.
#[must_use]
pub fn param_column(path: &str) -> String {
    let mut name = String::with_capacity(PARAM_PREFIX.len() + path.len());
    name.push_str(PARAM_PREFIX);
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            name.push('_');
        }
    }
    name
}

fn insert_float(object: &mut serde_json::Map<String, Value>, name: &str, value: Option<f64>) {
    // A non-finite estimate is written as null rather than as a number: `NaN` is not JSON,
    // and a value that reached here non-finite is a failed reduction, not a measurement.
    let number = value
        .filter(|v| v.is_finite())
        .and_then(serde_json::Number::from_f64);
    match number {
        Some(n) => object.insert(name.to_string(), Value::Number(n)),
        None => object.insert(name.to_string(), Value::Null),
    };
}

fn text(name: &str) -> ColumnSpec {
    ColumnSpec {
        name: name.to_string(),
        kind: ColumnKind::Text,
        quantum: None,
        ground_truth: false,
    }
}

fn int(name: &str) -> ColumnSpec {
    ColumnSpec {
        name: name.to_string(),
        kind: ColumnKind::Int,
        quantum: None,
        ground_truth: false,
    }
}

fn float(name: &str) -> ColumnSpec {
    ColumnSpec {
        name: name.to_string(),
        kind: ColumnKind::Float,
        // The finest grid D9 lists; see the module header for why a coarser one here would
        // be wrong for a table whose rows come from metrics with different grids.
        quantum: Some(grid::Q_METRIC_VALUE),
        ground_truth: false,
    }
}
