//! The VeReMi-compatible exporter — 08-measurement-and-data.md §5, `receiver-logs` with
//! `--veremi-json`.
//!
//! VeReMi (Van der Heijden, Lukaseder, Kargl, 2018) is the format the misbehaviour-detection
//! literature reads, and comparability with it is the point: a detector written against
//! VeReMi should run on this engine's output with no reader changes. So the field names
//! are VeReMi's — `rcvTime`, `sendTime`, `senderPseudo`, `messageID`, `pos`, `spd`,
//! `RSSI`, `attackerType` — and the file layout is VeReMi's: one JSON-lines file per
//! receiver, plus one ground-truth log for the whole run.
//!
//! # One deliberate deviation, and why
//!
//! VeReMi's per-receiver trace carries a `sender` field holding the **transmitter's real
//! module id**, beside `senderPseudo` holding its pseudonym. That is a ground-truth column
//! inside a receiver-visible file. F2MD shipped the same defect under the name
//! `senderRealId`, and it is the single leak this project's leakage linter was written
//! against — a detector trained on a VeReMi trace can read the real id, link every
//! pseudonym for free, and score far better than any real receiver could.
//!
//! This exporter therefore writes `sender` as a **stable integer derived from the
//! pseudonym**, not from the device: a receiver really can see that two messages carried
//! the same certificate, and really cannot see that two different certificates are the
//! same car. The true identity goes into `GroundTruthJSONlog.json` alone, keyed by
//! `messageID`, which is the join a consumer performs offline — VeReMi's own ground-truth
//! file works exactly that way.
//!
//! The deviation is recorded in [`VEREMI_COMPATIBILITY_NOTE`], written into the export's
//! `README` and stated in the datasheet, because a silent incompatibility is worse than a
//! declared one: a consumer that *wants* the linked ids can join the ground-truth file and
//! get them, and one that does not cannot get them by accident.
//!
//! # Message types
//!
//! VeReMi tags each line with a `type`: `2` is the receiver's own GPS position and `3` is
//! a received beacon. Both are written where the data exists — the own-position lines come
//! from `gt.kinematics` for the receiving node, which is ground truth about the receiver
//! *itself*, and a node reading its own position is not a firewall breach.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use v2xw_core::math::quantize_to;

use super::pyjson;
use crate::error::{RecordError, Result};
use crate::export::{ExportFormat, ExportedFile};
use crate::grid::{Q_DB, Q_METRES};

/// The note this exporter writes beside its output, and the datasheet repeats.
pub const VEREMI_COMPATIBILITY_NOTE: &str = "\
VeReMi compatibility note
=========================

Field names, file layout and message types follow VeReMi (Van der Heijden, Lukaseder,
Kargl, 2018) so that a detector written against VeReMi reads these traces unchanged.

ONE DELIBERATE DEVIATION. In VeReMi, a per-receiver trace's `sender` field holds the
transmitter's real module id, beside `senderPseudo` holding its pseudonym. That is ground
truth inside a receiver-visible file: a model trained on it can link every pseudonym for
free and will score far above anything a real receiver could achieve. (F2MD shipped the
same defect as `senderRealId`.)

Here, `sender` is a stable integer derived from the PSEUDONYM, not from the device. Two
messages carrying one certificate share a `sender`; two certificates of one car do not.
The true identity is in `GroundTruthJSONlog.json` only, keyed by `messageID` — join it
offline, exactly as VeReMi's own ground-truth file is joined.

Consequence for benchmarking: a VeReMi-trained detector that relied on `sender` being a
stable per-vehicle id will score lower here. That is the honest number; the higher one was
measuring the leak.
";

/// One received beacon as a receiver saw it — VeReMi's `type: 3` line, node-visible.
#[derive(Debug, Clone, PartialEq)]
pub struct VeremiReception {
    /// The receiving node.
    pub receiver: u64,
    /// When the frame was received, in seconds.
    pub rcv_time: f64,
    /// When it was generated, in seconds.
    pub send_time: f64,
    /// The sender, as a stable integer derived from its **pseudonym** (see the note).
    pub sender: i64,
    /// The pseudonym itself.
    pub sender_pseudo: String,
    /// The message id, which is the join key to the ground-truth log.
    pub message_id: u64,
    /// The claimed position, `[x, y, z]` in metres.
    pub pos: [f64; 3],
    /// The claimed velocity, `[x, y, z]` in m/s.
    pub spd: [f64; 3],
    /// The received signal strength.
    pub rssi: f64,
}

/// One transmission as it really was — the ground-truth log, ORACLE.
#[derive(Debug, Clone, PartialEq)]
pub struct VeremiTruth {
    /// When it was sent, in seconds.
    pub send_time: f64,
    /// The **real** transmitter. This field exists only in this file.
    pub sender: i64,
    /// The pseudonym it used.
    pub sender_pseudo: String,
    /// The message id.
    pub message_id: u64,
    /// The true position.
    pub pos: [f64; 3],
    /// The true velocity.
    pub spd: [f64; 3],
    /// VeReMi's attacker type code: `0` for benign, non-zero per attack family.
    pub attacker_type: i64,
}

/// One receiver's own position report: the instant, the position and the velocity.
///
/// VeReMi's `type: 2` line. A named alias rather than a bare tuple because the tuple is
/// three `[f64; 3]`-shaped things in a row and nothing in the type says which is which.
pub type OwnPosition = (f64, [f64; 3], [f64; 3]);

/// A whole VeReMi-shaped export.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VeremiExport {
    /// Receptions grouped by receiving node, each already in time order.
    pub by_receiver: BTreeMap<u64, Vec<VeremiReception>>,
    /// The receivers' own position reports (VeReMi's `type: 2`), by node.
    pub own_positions: BTreeMap<u64, Vec<OwnPosition>>,
    /// The ground-truth log, in message-id order.
    pub truth: Vec<VeremiTruth>,
}

/// The stable integer a pseudonym maps to in the `sender` field.
///
/// Derived from the pseudonym's own bytes, so it is reproducible across runs of the same
/// scenario and carries no device identity. VeReMi's `sender` is an `int`, so the value is
/// folded into a positive 32-bit range rather than handed out as a 64-bit hash: a consumer
/// that stores it in an `int32` column — and VeReMi consumers do — must not see it wrap.
#[must_use]
pub fn pseudonym_int(pseudonym: &str) -> i64 {
    let h = v2xw_core::hash::sha256(pseudonym.as_bytes());
    let v = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    i64::from(v & 0x7fff_ffff)
}

/// VeReMi's `attackerType` code for an attack family.
///
/// VeReMi 1.0 numbered its five attacker types 1–4 (`ConstPos`, `ConstPosOffset`,
/// `RandomPos`, `RandomPosOffset`) with `0` for benign; VeReMi Extension added more. A
/// family this engine has and VeReMi does not gets a code above the original range rather
/// than being squeezed into one of VeReMi's, because a consumer that maps `2` to
/// `ConstPosOffset` would otherwise silently mislabel it.
#[must_use]
pub fn attacker_type_code(family: &str) -> i64 {
    match family {
        "" | "none" | "benign" => 0,
        "const-pos" | "ConstPos" => 1,
        "const-pos-offset" | "ConstPosOffset" => 2,
        "random-pos" | "RandomPos" => 4,
        "random-pos-offset" | "RandomPosOffset" => 8,
        "const-speed" | "ConstSpeed" => 16,
        "eventual-stop" | "EventualStop" => 32,
        "disruptive" | "Disruptive" => 64,
        "data-replay" | "DataReplay" => 128,
        "sybil" | "Sybil" => 256,
        // Above VeReMi's own range: an unmapped family is reported as "some attack this
        // codebook does not name", which a consumer can detect, rather than as one of
        // VeReMi's, which it cannot.
        _ => 1024,
    }
}

impl VeremiExport {
    /// Quantises every float to its declared grid (D9) — positions and speeds on the metre
    /// grid, RSSI on the dB grid.
    ///
    /// Called by [`write`], so an export written through this module is on-grid whatever
    /// the caller handed in; it is public because a caller assembling an export by hand
    /// should be able to normalise it before comparing.
    pub fn quantise(&mut self) {
        for rows in self.by_receiver.values_mut() {
            for r in rows {
                r.rcv_time = quantize_to(r.rcv_time, crate::grid::Q_SECONDS);
                r.send_time = quantize_to(r.send_time, crate::grid::Q_SECONDS);
                for v in r.pos.iter_mut().chain(r.spd.iter_mut()) {
                    *v = quantize_to(*v, Q_METRES);
                }
                r.rssi = quantize_to(r.rssi, Q_DB);
            }
        }
        for rows in self.own_positions.values_mut() {
            for (t, pos, spd) in rows {
                *t = quantize_to(*t, crate::grid::Q_SECONDS);
                for v in pos.iter_mut().chain(spd.iter_mut()) {
                    *v = quantize_to(*v, Q_METRES);
                }
            }
        }
        for t in &mut self.truth {
            t.send_time = quantize_to(t.send_time, crate::grid::Q_SECONDS);
            for v in t.pos.iter_mut().chain(t.spd.iter_mut()) {
                *v = quantize_to(*v, Q_METRES);
            }
        }
    }
}

/// Builds a VeReMi-shaped export from a recording's records.
///
/// The join is the one a VeReMi trace needs and the one this engine's channels make
/// possible: `phy.rx` gives the reception and the receiver's measurements, `node.tx` gives
/// the generation instant and the message id, `gt.kinematics` gives the truth, and
/// `gt.attack.action` gives the attacker type. A message whose transmission was never
/// recorded still produces a reception line — a receiver saw it, after all — with the send
/// time taken from the reception, because dropping it would silently shrink a trace.
///
/// What is **not** joined is the device behind the pseudonym. `sender` is a function of the
/// pseudonym (see [`VEREMI_COMPATIBILITY_NOTE`]); the device appears in the ground-truth
/// log alone.
///
/// # Errors
/// [`RecordError::Malformed`] if a record does not fit its channel's view.
pub fn from_records(records: &[crate::reader::RecordedRecord]) -> Result<VeremiExport> {
    use std::collections::BTreeMap;
    use v2xw_metrics::channels::{
        GtAttackActionView, GtKinematicsView, NodeTxView, PhyRxView, RxOutcome,
    };

    let decode = |rec: &crate::reader::RecordedRecord| -> Result<serde_json::Value> {
        serde_json::from_slice(&rec.json).map_err(|e| {
            RecordError::malformed(
                "veremi",
                format!("a record on {} does not fit its view: {e}", rec.channel),
            )
        })
    };
    let secs = |t: u64| v2xw_core::math::q3(t as f64 / 1e9);

    // The transmission side, keyed by message id.
    let mut tx_of_msg: BTreeMap<u64, (u32, u64)> = BTreeMap::new();
    for r in records.iter().filter(|r| r.channel == "node.tx") {
        let v: NodeTxView = serde_json::from_value(decode(r)?)
            .map_err(|e| RecordError::malformed("veremi", format!("node.tx: {e}")))?;
        if let Some(msg) = v.msg {
            tx_of_msg.insert(msg, (v.node.0, v.t_generated.unwrap_or(v.t)));
        }
    }
    // The truth, keyed by (actor, instant) so a reception can find the state its sender
    // really was in.
    let mut truth_at: BTreeMap<(u32, u64), GtKinematicsView> = BTreeMap::new();
    for r in records.iter().filter(|r| r.channel == "gt.kinematics") {
        let v: GtKinematicsView = serde_json::from_value(decode(r)?)
            .map_err(|e| RecordError::malformed("veremi", format!("gt.kinematics: {e}")))?;
        truth_at.insert((v.actor.0, v.t), v);
    }
    // Which attacker family each device belongs to, and when it was acting.
    let mut family_of: BTreeMap<u32, String> = BTreeMap::new();
    let mut acting: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    for r in records.iter().filter(|r| r.channel == "gt.attack.action") {
        let v: GtAttackActionView = serde_json::from_value(decode(r)?)
            .map_err(|e| RecordError::malformed("veremi", format!("gt.attack.action: {e}")))?;
        family_of
            .entry(v.actor.0)
            .or_insert_with(|| v.attacker.clone());
        let e = acting.entry(v.actor.0).or_insert((v.t, v.t));
        e.0 = e.0.min(v.t);
        e.1 = e.1.max(v.t);
    }

    let mut out = VeremiExport::default();
    let mut truth_seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();

    for r in records.iter().filter(|r| r.channel == "phy.rx") {
        let v: PhyRxView = serde_json::from_value(decode(r)?)
            .map_err(|e| RecordError::malformed("veremi", format!("phy.rx: {e}")))?;
        // VeReMi logs what a receiver *received*. A frame that was lost never reached the
        // application, so it is not a trace line; the net-trace profile is where losses
        // live.
        if v.outcome != RxOutcome::Ok {
            continue;
        }
        let Some(msg) = v.msg else { continue };
        let (sender_actor, generated) = match tx_of_msg.get(&msg) {
            Some((a, g)) => (*a, *g),
            // No transmission record: fall back to the transmitter the reception names and
            // to the reception instant, rather than dropping the line.
            None => (v.tx.map_or(0, |n| n.0), v.t_start),
        };
        // The pseudonym the sender was using. `sec.cert` binds a node to a digest over
        // time; where the recording has no binding, a stable per-device pseudonym keeps
        // the trace joinable without inventing an identity.
        let pseudonym = pseudonym_for(records, sender_actor, v.t_start);
        let truth = truth_at
            .range((sender_actor, 0)..=(sender_actor, v.t_start))
            .next_back()
            .map(|(_, k)| k);
        let (pos, spd) = claimed(
            truth,
            family_of.get(&sender_actor),
            acting.get(&sender_actor),
            v.t_start,
        );

        out.by_receiver
            .entry(u64::from(v.rx.0))
            .or_default()
            .push(VeremiReception {
                receiver: u64::from(v.rx.0),
                rcv_time: secs(v.t_end),
                send_time: secs(generated),
                sender: pseudonym_int(&pseudonym),
                sender_pseudo: pseudonym.clone(),
                message_id: msg,
                pos,
                spd,
                rssi: v.rssi_dbm.unwrap_or(f64::from(0)),
            });

        if truth_seen.insert(msg) {
            let (tpos, tspd) = match truth {
                Some(k) => (
                    [k.x_m, k.y_m, k.z_m.unwrap_or(0.0)],
                    [k.speed_mps, 0.0, 0.0],
                ),
                None => ([0.0; 3], [0.0; 3]),
            };
            let in_window = acting
                .get(&sender_actor)
                .is_some_and(|(from, to)| v.t_start >= *from && v.t_start <= *to);
            out.truth.push(VeremiTruth {
                send_time: secs(generated),
                sender: i64::from(sender_actor),
                sender_pseudo: pseudonym,
                message_id: msg,
                pos: tpos,
                spd: tspd,
                attacker_type: if in_window {
                    family_of
                        .get(&sender_actor)
                        .map_or(0, |f| attacker_type_code(f))
                } else {
                    0
                },
            });
        }
    }

    // The receivers' own position reports (VeReMi's `type: 2`).
    for ((actor, t), k) in &truth_at {
        if out.by_receiver.contains_key(&u64::from(*actor)) {
            out.own_positions
                .entry(u64::from(*actor))
                .or_default()
                .push((
                    secs(*t),
                    [k.x_m, k.y_m, k.z_m.unwrap_or(0.0)],
                    [k.speed_mps, 0.0, 0.0],
                ));
        }
    }

    for rows in out.by_receiver.values_mut() {
        rows.sort_by(|a, b| {
            a.rcv_time
                .partial_cmp(&b.rcv_time)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.message_id.cmp(&b.message_id))
        });
    }
    out.truth.sort_by_key(|t| t.message_id);
    out.quantise();
    Ok(out)
}

/// The claimed position and velocity a receiver saw.
///
/// An attacker inside its window claims a falsified position; anything else claims what it
/// believed. The offset is the one `assemble` uses for the emissions tables, so the two
/// exports of one run tell the same story rather than two.
fn claimed(
    truth: Option<&v2xw_metrics::channels::GtKinematicsView>,
    family: Option<&String>,
    window: Option<&(u64, u64)>,
    at: u64,
) -> ([f64; 3], [f64; 3]) {
    let Some(k) = truth else {
        return ([0.0; 3], [0.0; 3]);
    };
    let attacking = family.is_some() && window.is_some_and(|(from, to)| at >= *from && at <= *to);
    let offset = if attacking { 25.0 } else { 0.0 };
    (
        [k.x_m + offset, k.y_m, k.z_m.unwrap_or(0.0)],
        [k.speed_mps, 0.0, 0.0],
    )
}

/// The pseudonym a device was using at `at`, from the `sec.cert` bindings in the recording.
///
/// With no binding recorded, a stable per-device pseudonym keeps the trace joinable. That
/// is weaker than the real thing — it means a trace from a run with no credential layer
/// cannot exercise pseudonym unlinkability — and it is the honest fallback, because the
/// alternatives are to drop the line or to put the device id in the `sender` field.
fn pseudonym_for(records: &[crate::reader::RecordedRecord], actor: u32, at: u64) -> String {
    let mut best: Option<(u64, String)> = None;
    for r in records.iter().filter(|r| r.channel == "sec.cert") {
        let Ok(v) = serde_json::from_slice::<v2xw_metrics::channels::SecCertView>(&r.json) else {
            continue;
        };
        if v.node.0 != actor || v.t > at {
            continue;
        }
        if let Some(d) = v.digest {
            if best.as_ref().is_none_or(|(t, _)| v.t >= *t) {
                best = Some((v.t, d));
            }
        }
    }
    best.map(|(_, d)| d).unwrap_or_else(|| {
        let hex = v2xw_core::hash::sha256_hex(format!("v2xw-unbound-pseudonym|{actor}").as_bytes());
        hex[..16].to_string()
    })
}

fn reception_json(r: &VeremiReception) -> serde_json::Value {
    serde_json::json!({
        "type": 3,
        "rcvTime": r.rcv_time,
        "sendTime": r.send_time,
        "sender": r.sender,
        "senderPseudo": r.sender_pseudo,
        "messageID": r.message_id,
        "pos": [r.pos[0], r.pos[1], r.pos[2]],
        "spd": [r.spd[0], r.spd[1], r.spd[2]],
        "RSSI": r.rssi,
    })
}

fn truth_json(t: &VeremiTruth) -> serde_json::Value {
    serde_json::json!({
        "type": 3,
        "sendTime": t.send_time,
        "sender": t.sender,
        "senderPseudo": t.sender_pseudo,
        "messageID": t.message_id,
        "pos": [t.pos[0], t.pos[1], t.pos[2]],
        "spd": [t.spd[0], t.spd[1], t.spd[2]],
        "attackerType": t.attacker_type,
    })
}

/// Writes a VeReMi-shaped export into `dir`.
///
/// Files: `traceJSON-<receiver>.json` per receiver, `GroundTruthJSONlog.json` for the run,
/// and `VEREMI-NOTE.txt` carrying [`VEREMI_COMPATIBILITY_NOTE`]. Lines are the legacy
/// canonical JSON encoding, so the files are reproducible byte for byte.
///
/// # Errors
/// [`RecordError::Io`] if a file cannot be written.
pub fn write(dir: impl AsRef<Path>, export: &VeremiExport) -> Result<Vec<ExportedFile>> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir).map_err(|e| RecordError::io(dir, e))?;
    let mut out = Vec::new();
    let mut export = export.clone();
    export.quantise();

    for (receiver, rows) in &export.by_receiver {
        let path: PathBuf = dir.join(format!("traceJSON-{receiver}.json"));
        let mut text = String::new();
        // VeReMi interleaves the receiver's own position reports with what it received,
        // in time order, which is how a detector consuming the trace expects to see them.
        let mut lines: Vec<(f64, serde_json::Value)> = rows
            .iter()
            .map(|r| (r.rcv_time, reception_json(r)))
            .collect();
        if let Some(own) = export.own_positions.get(receiver) {
            for (t, pos, spd) in own {
                lines.push((
                    *t,
                    serde_json::json!({
                        "type": 2,
                        "rcvTime": t,
                        "pos": [pos[0], pos[1], pos[2]],
                        "spd": [spd[0], spd[1], spd[2]],
                    }),
                ));
            }
        }
        lines.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| pyjson::canonical(&a.1).cmp(&pyjson::canonical(&b.1)))
        });
        for (_, v) in &lines {
            text.push_str(&pyjson::canonical_line(v));
        }
        std::fs::write(&path, text.as_bytes()).map_err(|e| RecordError::io(&path, e))?;
        out.push(ExportedFile {
            path,
            format: ExportFormat::Jsonl,
            channel: Some("phy.rx".to_string()),
            rows: lines.len(),
            bytes: text.len() as u64,
            schema: None,
        });
    }

    let mut truth = export.truth.clone();
    truth.sort_by_key(|t| t.message_id);
    let truth_path = dir.join("GroundTruthJSONlog.json");
    let mut text = String::new();
    for t in &truth {
        text.push_str(&pyjson::canonical_line(&truth_json(t)));
    }
    std::fs::write(&truth_path, text.as_bytes()).map_err(|e| RecordError::io(&truth_path, e))?;
    out.push(ExportedFile {
        path: truth_path,
        format: ExportFormat::Jsonl,
        channel: Some("gt.kinematics".to_string()),
        rows: truth.len(),
        bytes: text.len() as u64,
        schema: None,
    });

    let note_path = dir.join("VEREMI-NOTE.txt");
    std::fs::write(&note_path, VEREMI_COMPATIBILITY_NOTE.as_bytes())
        .map_err(|e| RecordError::io(&note_path, e))?;
    out.push(ExportedFile {
        path: note_path,
        format: ExportFormat::Json,
        channel: None,
        rows: 0,
        bytes: VEREMI_COMPATIBILITY_NOTE.len() as u64,
        schema: None,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reception() -> VeremiReception {
        VeremiReception {
            receiver: 1,
            rcv_time: 10.0,
            send_time: 9.998,
            sender: pseudonym_int("aabbccdd"),
            sender_pseudo: "aabbccdd".to_string(),
            message_id: 7,
            pos: [100.5, 200.25, 0.0],
            spd: [12.0, 0.0, 0.0],
            rssi: -72.5,
        }
    }

    #[test]
    fn the_receiver_visible_line_uses_veremi_field_names() {
        let v = reception_json(&reception());
        for key in [
            "type",
            "rcvTime",
            "sendTime",
            "sender",
            "senderPseudo",
            "messageID",
            "pos",
            "spd",
            "RSSI",
        ] {
            assert!(v.get(key).is_some(), "VeReMi field {key} is missing");
        }
        assert_eq!(v["type"], 3);
    }

    #[test]
    fn the_receiver_visible_line_carries_no_real_identity() {
        // The whole deviation, as a test: `sender` is a function of the pseudonym and of
        // nothing else, so two pseudonyms of one device do not share it.
        let a = pseudonym_int("pseudonym-a");
        let b = pseudonym_int("pseudonym-b");
        assert_ne!(a, b);
        assert_eq!(a, pseudonym_int("pseudonym-a"), "stable across calls");
        assert!(
            a >= 0 && a <= i64::from(i32::MAX),
            "fits a VeReMi int32 column"
        );
        // …and the linter agrees the line is clean.
        let v = reception_json(&reception());
        let report = crate::dataset::leakage::lint_rows("traceJSON-1.json", &[v]);
        assert!(report.is_clean(), "{}", report.summary());
    }

    #[test]
    fn the_ground_truth_log_is_where_the_real_identity_lives() {
        let t = VeremiTruth {
            send_time: 9.998,
            sender: 42,
            sender_pseudo: "aabbccdd".to_string(),
            message_id: 7,
            pos: [100.0, 200.0, 0.0],
            spd: [12.0, 0.0, 0.0],
            attacker_type: 2,
        };
        let v = truth_json(&t);
        assert_eq!(v["attackerType"], 2);
        assert_eq!(v["sender"], 42);
    }

    #[test]
    fn an_unmapped_attack_family_is_not_squeezed_into_a_veremi_code() {
        assert_eq!(attacker_type_code("none"), 0);
        assert_eq!(attacker_type_code("ConstPosOffset"), 2);
        assert_eq!(
            attacker_type_code("umbrella-threshold-pq-replay"),
            1024,
            "an unnamed family is reported as unnamed, not mislabelled as VeReMi's #2"
        );
    }

    #[test]
    fn every_float_is_quantised_before_it_is_written() {
        let mut e = VeremiExport::default();
        let mut r = reception();
        r.pos[0] = 100.123_456_789;
        r.rssi = -72.567_89;
        r.rcv_time = 10.000_4;
        e.by_receiver.insert(1, vec![r]);
        e.quantise();
        let r = &e.by_receiver[&1][0];
        assert_eq!(r.pos[0], 100.123);
        assert_eq!(r.rssi, -72.57);
        assert_eq!(r.rcv_time, 10.0);
        assert!(v2xw_core::math::is_on_grid(r.pos[0], Q_METRES));
        assert!(v2xw_core::math::is_on_grid(r.rssi, Q_DB));
    }

    #[test]
    fn the_note_states_the_deviation_rather_than_leaving_it_to_be_discovered() {
        assert!(VEREMI_COMPATIBILITY_NOTE.contains("senderRealId"));
        assert!(VEREMI_COMPATIBILITY_NOTE.contains("will score lower here"));
    }
}
