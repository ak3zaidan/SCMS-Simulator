//! The `receiver-logs`, `telemetry` and `net-trace` export profiles —
//! 08-measurement-and-data.md §5.
//!
//! | Profile | Files | Visibility |
//! |---|---|---|
//! | `receiver-logs` | one table per receiving node, plus a GT file keyed by message id | NODE + separate GT |
//! | `telemetry` | `node.telemetry` time series per node | NODE |
//! | `net-trace` | every frame with its outcome and cause | NODE (+ GT tx id in a separate file) |
//!
//! All three are built on [`crate::export`], so they inherit its two properties: every
//! float is quantised to the grid its column declares, and the grid is written next to the
//! data in `schema.json` so [`crate::export::scan`] can check the file rather than trust
//! the writer.
//!
//! # The separation is the design, not a setting
//!
//! §5 gives `receiver-logs` and `net-trace` the visibility "NODE + separate GT", and the
//! word *separate* is doing the work. Both profiles carry a ground-truth column — the
//! transmitter's real identity, and the true transmitter-to-receiver distance — that a
//! detector must not train on. Rather than offering a flag that includes it, these
//! profiles write it to **a different file**, keyed by message id, exactly as VeReMi's
//! ground-truth log is keyed. A consumer that wants it performs the join and knows it did;
//! a consumer that does not cannot get it by accident, and no amount of careless column
//! selection can leak it.
//!
//! [`ProfileSet::lint`] then checks the written node-visible files with the leakage
//! linter, so the claim is verified over the bytes rather than asserted by the code that
//! wrote them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::leakage::{self, LeakageReport};
use crate::error::{RecordError, Result};
use crate::export::schema::{TIME_COLUMN, TableSchema, ground_truth_fields};
use crate::export::{ExportFormat, ExportProfile, ExportedFile, Exporter};
use crate::reader::RecordedRecord;

/// Which of §5's profiles to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogProfile {
    /// Per-receiver received-message tables, with the ground truth in a separate file.
    ReceiverLogs,
    /// Per-node resource telemetry — the data the HUD shows.
    Telemetry,
    /// Every frame with its outcome and cause, plus the MAC and fragmentation channels.
    NetTrace,
}

impl LogProfile {
    /// The profile's directory name inside the export root.
    #[must_use]
    pub const fn dir_name(self) -> &'static str {
        match self {
            LogProfile::ReceiverLogs => "receiver_logs",
            LogProfile::Telemetry => "telemetry",
            LogProfile::NetTrace => "net_trace",
        }
    }

    /// The `schema` id prefix §5 gives this profile (`v2xw/receiver-logs/1`).
    #[must_use]
    pub const fn schema_id(self) -> &'static str {
        match self {
            LogProfile::ReceiverLogs => "v2xw/receiver-logs/1",
            LogProfile::Telemetry => "v2xw/telemetry/1",
            LogProfile::NetTrace => "v2xw/net-trace/1",
        }
    }

    /// The channels this profile reads, in name order.
    #[must_use]
    pub const fn channels(self) -> &'static [&'static str] {
        match self {
            LogProfile::ReceiverLogs => &["phy.rx"],
            LogProfile::Telemetry => &["node.telemetry"],
            LogProfile::NetTrace => &["mac.cbr", "net.frag", "node.tx", "phy.rx"],
        }
    }
}

/// What one profile wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSet {
    /// The profile.
    pub profile: LogProfile,
    /// The node-visible files.
    pub node_files: Vec<ExportedFile>,
    /// The ground-truth sidecar files, keyed by message id.
    pub gt_files: Vec<ExportedFile>,
}

impl ProfileSet {
    /// Every file, node-visible first.
    #[must_use]
    pub fn all(&self) -> Vec<&ExportedFile> {
        self.node_files.iter().chain(self.gt_files.iter()).collect()
    }

    /// Runs the leakage linter over the **node-visible** files only.
    ///
    /// The ground-truth sidecars are meant to hold ground truth, so linting them would be
    /// nonsense; the whole point of the separation is that only the node-visible half has
    /// to be clean. A JSONL file is read back and linted row by row; a Parquet file is
    /// linted by its column names, which is the legacy audit's `L1` check.
    ///
    /// # Errors
    /// [`RecordError::Io`] or [`RecordError::Json`] if a written file cannot be read back.
    /// A file the linter cannot read is a failure, not a pass: an unreadable artefact
    /// proves nothing.
    pub fn lint(&self) -> Result<LeakageReport> {
        let mut report = LeakageReport::default();
        for f in &self.node_files {
            let name = f
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| f.path.display().to_string());
            let one = match f.format {
                ExportFormat::Jsonl => {
                    let rows = crate::export::jsonl::read(&f.path)?;
                    match &f.channel {
                        // Both gates: the name registry, and the channel's own
                        // ground-truth column list. `phy.rx` carries the transmitter id as
                        // `tx`, which no name rule would ever flag.
                        Some(channel) => leakage::lint_rows_of_channel(&name, channel, &rows),
                        None => leakage::lint_rows(&name, &rows),
                    }
                }
                ExportFormat::Parquet | ExportFormat::ArrowIpc => {
                    let cols: Vec<String> = f
                        .schema
                        .as_ref()
                        .map(|s| s.columns.iter().map(|c| c.name.clone()).collect())
                        .unwrap_or_default();
                    match &f.channel {
                        Some(channel) => leakage::lint_channel_columns(&name, channel, &cols),
                        None => leakage::lint_columns(&name, &cols),
                    }
                }
                ExportFormat::Json => LeakageReport::default(),
            };
            report.files += one.files;
            report.rows += one.rows;
            report.keys += one.keys;
            report.violations.extend(one.violations);
        }
        report.violations.sort();
        Ok(report)
    }
}

/// Writes one of §5's profiles into `root/<profile>/`.
///
/// `format` is the node-visible tables' format; the ground-truth sidecar is always JSONL,
/// because it is a small join table that a human reads as often as a program does.
///
/// # Errors
/// Whatever the underlying exporter returns, plus [`RecordError::Io`].
pub fn write(
    root: impl AsRef<Path>,
    profile: LogProfile,
    records: &[RecordedRecord],
    format: ExportFormat,
) -> Result<ProfileSet> {
    let dir = root.as_ref().join(profile.dir_name());
    std::fs::create_dir_all(&dir).map_err(|e| RecordError::io(&dir, e))?;

    let mut node_files = Vec::new();
    let mut gt_files = Vec::new();

    match profile {
        LogProfile::ReceiverLogs => {
            // One file per receiving node. The split is by the receiver, because that is
            // the unit a detector runs on: a per-receiver trace is what VeReMi ships and
            // what a local misbehaviour detector actually sees.
            let mut by_receiver: BTreeMap<i64, Vec<(u64, Vec<u8>)>> = BTreeMap::new();
            for r in records.iter().filter(|r| r.channel == "phy.rx") {
                let v: serde_json::Value = serde_json::from_slice(&r.json)?;
                let rx = v
                    .get("rx")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or_else(|| {
                        RecordError::malformed("receiver-logs", "a phy.rx record has no `rx` field")
                    })?;
                by_receiver
                    .entry(rx)
                    .or_default()
                    .push((r.sim_time, r.json.clone()));
            }
            for (rx, rows) in &by_receiver {
                let full = TableSchema::infer("phy.rx", rows)?;
                let schema = blind(&full, profile);
                let stem = format!("receiver_{rx}");
                node_files.extend(write_table(&dir, &stem, &schema, rows, format)?);
            }
            // The ground truth, once, keyed by message id — never per receiver, because a
            // per-receiver copy invites the join to happen inside one directory listing.
            let rows: Vec<(u64, Vec<u8>)> = records
                .iter()
                .filter(|r| r.channel == "phy.rx")
                .map(|r| (r.sim_time, r.json.clone()))
                .collect();
            gt_files.extend(write_gt_sidecar(
                &dir,
                "phy.rx",
                &rows,
                &["msg", "tx", "dist_m"],
            )?);
        }
        LogProfile::Telemetry => {
            let mut by_node: BTreeMap<i64, Vec<(u64, Vec<u8>)>> = BTreeMap::new();
            for r in records.iter().filter(|r| r.channel == "node.telemetry") {
                let v: serde_json::Value = serde_json::from_slice(&r.json)?;
                let node = v
                    .get("node")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or_else(|| {
                        RecordError::malformed(
                            "telemetry",
                            "a node.telemetry record has no `node` field",
                        )
                    })?;
                by_node
                    .entry(node)
                    .or_default()
                    .push((r.sim_time, r.json.clone()));
            }
            for (node, rows) in &by_node {
                let full = TableSchema::infer("node.telemetry", rows)?;
                let schema = blind(&full, profile);
                let stem = format!("node_{node}");
                node_files.extend(write_table(&dir, &stem, &schema, rows, format)?);
            }
        }
        LogProfile::NetTrace => {
            for channel in profile.channels() {
                let rows: Vec<(u64, Vec<u8>)> = records
                    .iter()
                    .filter(|r| r.channel == *channel)
                    .map(|r| (r.sim_time, r.json.clone()))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                let full = TableSchema::infer(channel, &rows)?;
                let schema = blind(&full, profile);
                let stem = channel.replace('.', "_");
                node_files.extend(write_table(&dir, &stem, &schema, &rows, format)?);
                if channel == &"phy.rx" {
                    gt_files.extend(write_gt_sidecar(
                        &dir,
                        "phy.rx",
                        &rows,
                        &["msg", "tx", "dist_m"],
                    )?);
                }
            }
        }
    }

    Ok(ProfileSet {
        profile,
        node_files,
        gt_files,
    })
}

/// The schema with the ground-truth columns projected out and the profile's schema id
/// stamped on it.
fn blind(full: &TableSchema, profile: LogProfile) -> TableSchema {
    let mut s = full.without_ground_truth();
    s.schema = profile.schema_id().to_string();
    s
}

fn write_table(
    dir: &Path,
    stem: &str,
    schema: &TableSchema,
    rows: &[(u64, Vec<u8>)],
    format: ExportFormat,
) -> Result<Vec<ExportedFile>> {
    let data_path = dir.join(format!("{stem}.{}", format.extension()));
    let bytes = match format {
        ExportFormat::Parquet => {
            let batch = crate::export::table::build_batch(schema, rows)?;
            crate::export::parquet::write(&data_path, &batch)?
        }
        ExportFormat::ArrowIpc => {
            let batch = crate::export::table::build_batch(schema, rows)?;
            crate::export::arrow_ipc::write(&data_path, &[batch])?
        }
        ExportFormat::Jsonl => crate::export::jsonl::write(&data_path, schema, rows)?,
        ExportFormat::Json => {
            return Err(RecordError::malformed(
                "export",
                "plain JSON is for sidecars, not for a record table",
            ));
        }
    };
    let schema_path = dir.join(format!("{stem}.schema.json"));
    let schema_bytes = schema.to_json()?;
    std::fs::write(&schema_path, &schema_bytes).map_err(|e| RecordError::io(&schema_path, e))?;
    Ok(vec![
        ExportedFile {
            path: data_path,
            format,
            channel: Some(schema.channel.clone()),
            rows: rows.len(),
            bytes,
            schema: Some(schema.clone()),
        },
        ExportedFile {
            path: schema_path,
            format: ExportFormat::Json,
            channel: Some(schema.channel.clone()),
            rows: 0,
            bytes: schema_bytes.len() as u64,
            schema: None,
        },
    ])
}

/// Writes the ground-truth sidecar: the join key plus the ground-truth columns, and
/// nothing else.
///
/// `keep` names the columns to carry. The first is the join key; the rest must all be
/// ground-truth columns of the channel, which is asserted rather than assumed — a sidecar
/// that accidentally carried a node-visible column would blur the very separation it
/// exists to draw, and a sidecar that *missed* a ground-truth column would mean that
/// column went into the node-visible file instead.
fn write_gt_sidecar(
    dir: &Path,
    channel: &str,
    rows: &[(u64, Vec<u8>)],
    keep: &[&str],
) -> Result<Vec<ExportedFile>> {
    let declared: BTreeSet<&str> = ground_truth_fields(channel).iter().copied().collect();
    for name in keep.iter().skip(1) {
        if !declared.contains(name) {
            return Err(RecordError::malformed(
                "gt sidecar",
                format!(
                    "{channel}.{name} is not declared a ground-truth column, so it does not \
                     belong in the ground-truth sidecar"
                ),
            ));
        }
    }
    let path = dir.join(format!("{}.groundtruth.jsonl", channel.replace('.', "_")));
    let mut text = String::new();
    let mut written = 0usize;
    for (at, json) in rows {
        let v: serde_json::Value = serde_json::from_slice(json)?;
        let Some(obj) = v.as_object() else { continue };
        let mut row = serde_json::Map::new();
        row.insert(TIME_COLUMN.to_string(), serde_json::Value::from(*at));
        for name in keep {
            if let Some(value) = obj.get(*name) {
                row.insert(
                    (*name).to_string(),
                    crate::export::schema::quantise_nested(name, value),
                );
            }
        }
        row.insert(
            "_visibility".to_string(),
            serde_json::Value::from(leakage::ORACLE),
        );
        text.push_str(&super::pyjson::canonical_line(&serde_json::Value::Object(
            row,
        )));
        written += 1;
    }
    std::fs::write(&path, text.as_bytes()).map_err(|e| RecordError::io(&path, e))?;
    Ok(vec![ExportedFile {
        path,
        format: ExportFormat::Jsonl,
        channel: Some(channel.to_string()),
        rows: written,
        bytes: text.len() as u64,
        schema: None,
    }])
}

/// Writes the full recording as `recording`-profile tables — §5's `recording` row, in its
/// tabular form.
///
/// This is [`Exporter::export_all`] under the `NodeOnly` visibility, kept here so a caller
/// asking for "every channel, node-visible" has one call rather than having to know which
/// channels are ground truth.
///
/// # Errors
/// As [`Exporter::export_all`].
pub fn write_all_channels(
    root: impl AsRef<Path>,
    records: &[RecordedRecord],
    format: ExportFormat,
) -> Result<Vec<ExportedFile>> {
    let dir: PathBuf = root.as_ref().join("channels");
    let exporter = Exporter::new(&dir, ExportProfile::NodeOnly)?;
    exporter.export_all(records, format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::scratch_dir;

    fn rx(t: u64, rx: u64, tx: u64, msg: u64) -> RecordedRecord {
        RecordedRecord {
            channel: "phy.rx".to_string(),
            sim_time: t,
            json: format!(
                r#"{{"t_start":{t},"t_end":{t},"rx":{rx},"tx":{tx},"msg":{msg},
                    "rssi_dbm":-70.25,"dist_m":123.456,"outcome":"ok"}}"#
            )
            .into_bytes(),
        }
    }

    #[test]
    fn a_receiver_log_holds_no_ground_truth_column_and_the_sidecar_holds_them_all() {
        let dir = scratch_dir("receiver-logs").expect("scratch");
        let records = vec![rx(1_000_000_000, 1, 2, 7), rx(2_000_000_000, 1, 3, 8)];
        let set = write(
            &dir,
            LogProfile::ReceiverLogs,
            &records,
            ExportFormat::Jsonl,
        )
        .expect("write");

        // The node-visible file: the receiver's own measurements, no transmitter, no
        // distance.
        let data = set
            .node_files
            .iter()
            .find(|f| f.format == ExportFormat::Jsonl)
            .expect("a data file");
        let rows = crate::export::jsonl::read(&data.path).expect("read back");
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert!(
                row.get("rssi_dbm").is_some(),
                "the receiver's own measurement"
            );
            assert!(row.get("tx").is_none(), "the transmitter is ground truth");
            assert!(row.get("dist_m").is_none(), "the distance is ground truth");
        }

        // The sidecar: the join key and the ground truth, tagged ORACLE.
        let gt = &set.gt_files[0];
        let gt_rows = crate::export::jsonl::read(&gt.path).expect("read back");
        assert_eq!(gt_rows.len(), 2);
        assert_eq!(gt_rows[0]["tx"], 2);
        assert_eq!(gt_rows[0]["msg"], 7);
        assert_eq!(gt_rows[0]["_visibility"], "ORACLE");

        // And the linter agrees, over the bytes that were written.
        let report = set.lint().expect("lint");
        assert!(report.is_clean(), "{}", report.summary());
        assert_eq!(report.rows, 2);
    }

    #[test]
    fn the_sidecar_refuses_a_column_that_is_not_declared_ground_truth() {
        // Otherwise the separation blurs: a sidecar carrying a node-visible column makes
        // the join pointless, and the mistake is silent.
        let dir = scratch_dir("gt-sidecar-guard").expect("scratch");
        let rows = vec![(1u64, br#"{"rx":1,"tx":2,"rssi_dbm":-70.0}"#.to_vec())];
        let e = write_gt_sidecar(&dir, "phy.rx", &rows, &["msg", "rssi_dbm"])
            .expect_err("rssi is not ground truth");
        assert!(
            e.to_string().contains("not declared a ground-truth column"),
            "{e}"
        );
    }

    #[test]
    fn a_net_trace_keeps_the_loss_cause_which_is_the_whole_point_of_it() {
        let dir = scratch_dir("net-trace").expect("scratch");
        let records = vec![RecordedRecord {
            channel: "phy.rx".to_string(),
            sim_time: 1,
            json: br#"{"t_start":1,"t_end":2,"rx":1,"tx":2,"msg":9,"outcome":"lost",
                      "cause":"collision","sinr_db":-3.5}"#
                .to_vec(),
        }];
        let set = write(&dir, LogProfile::NetTrace, &records, ExportFormat::Jsonl).expect("write");
        let data = set
            .node_files
            .iter()
            .find(|f| f.format == ExportFormat::Jsonl)
            .expect("a data file");
        let rows = crate::export::jsonl::read(&data.path).expect("read back");
        assert_eq!(rows[0]["outcome"], "lost");
        assert_eq!(rows[0]["cause"], "collision");
        assert_eq!(rows[0]["sinr_db"], -3.5);
        assert!(set.lint().expect("lint").is_clean());
    }

    #[test]
    fn a_telemetry_profile_writes_one_table_per_node() {
        let dir = scratch_dir("telemetry").expect("scratch");
        let records: Vec<RecordedRecord> = [1u64, 1, 2]
            .iter()
            .enumerate()
            .map(|(i, node)| RecordedRecord {
                channel: "node.telemetry".to_string(),
                sim_time: i as u64 * 1_000_000_000,
                json: format!(
                    r#"{{"t":{},"node":{node},"cpu":0.25,"verify_queue_depth":3}}"#,
                    i as u64 * 1_000_000_000
                )
                .into_bytes(),
            })
            .collect();
        let set = write(&dir, LogProfile::Telemetry, &records, ExportFormat::Jsonl).expect("write");
        let data: Vec<&ExportedFile> = set
            .node_files
            .iter()
            .filter(|f| f.format == ExportFormat::Jsonl)
            .collect();
        assert_eq!(data.len(), 2, "one table per node");
        assert_eq!(data[0].rows, 2);
        assert_eq!(data[1].rows, 1);
    }

    #[test]
    fn the_schema_id_is_the_one_section_five_declares() {
        assert_eq!(LogProfile::ReceiverLogs.schema_id(), "v2xw/receiver-logs/1");
        assert_eq!(LogProfile::Telemetry.schema_id(), "v2xw/telemetry/1");
        assert_eq!(LogProfile::NetTrace.schema_id(), "v2xw/net-trace/1");
    }
}
