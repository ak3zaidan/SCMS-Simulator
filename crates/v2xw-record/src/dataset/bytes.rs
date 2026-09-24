//! Byte provenance: which of a dataset's byte counts came from real bytes and which from
//! a size model.
//!
//! 08-measurement-and-data.md §6 asks a datasheet to say what produced its numbers. For
//! every number *except* the byte counts, the model card answers that. The byte counts are
//! different, and the difference is the one that decides whether a result can be cited:
//!
//! * a **UPER** or **COER** encoding produces the octets that would have gone over the
//!   air. `bytes_on_wire` is then a measurement of an encoding, and an envelope-overhead
//!   ratio computed from it is a result about IEEE 1609.2, reproducible by anyone with the
//!   same ASN.1 module;
//! * the **validated size model** of 04-models.md §8.4 produces a *length* and fills the
//!   payload with `0xA5`. `bytes_on_wire` is then a prediction of a model whose own
//!   tolerance is stated on its card, and an overhead ratio computed from it is a result
//!   about that model.
//!
//! Both are legitimate and the engine ships both: the SAE J2735 ASN.1 module cannot be
//! redistributed (risk R1, 11-open-questions), so PSM, SRM and SSM are modelled and the
//! ETSI messages are real. What is *not* legitimate is a table of overhead figures that
//! does not say which rows are which — and the failure mode is quiet, because a modelled
//! size and a real one are the same kind of integer and land in the same column.
//!
//! # Why the engine declares this and the exporter does not infer it
//!
//! `v2xw-record` does not depend on `v2xw-msg` and must not: the exporter's job is to read
//! recorded channels, and a dependency on the encoder would let it recompute what the run
//! did rather than report it. So [`ByteProvenance`] is *declared* per message type in
//! [`super::RunProvenance::message_encodings`], by the layer that chose the codec, and
//! this module joins that declaration against what the recording actually carries.
//!
//! The join is the check. A message type that appears in `node.tx` and is missing from the
//! declaration is reported as [`ByteProvenance::Undeclared`] and its bytes are counted as
//! neither real nor modelled, so a dataset cannot acquire citable byte counts by the
//! engine forgetting to declare a codec. A declaration that matches no traffic is reported
//! too ([`ByteProvenanceReport::declared_types_unused`]), because a stale declaration is
//! how this section would come to describe the wrong run.
//!
//! # No floats
//!
//! Every quantity here is an integer count of bytes or messages, and the one derived
//! figure — the real share — is carried in **parts per thousand** as a `u64`. That is not
//! fussiness: build decision D9 requires every exported float to sit on a declared grid,
//! and the honest way to satisfy it for a ratio nobody needs to three decimal places is
//! not to write a float at all.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use v2xw_metrics::channels::NodeTxView;

/// The key a transmission with no declared message type is tallied under.
///
/// Its own bucket rather than folded into a total: a run whose producer stopped filling
/// `msg_type` would otherwise quietly move every byte into "undeclared" with no way to
/// tell that from a missing codec declaration.
pub const UNSTATED_MSG_TYPE: &str = "(msg_type not stated)";

/// Where one message type's bytes came from, as the engine declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "detail")]
pub enum ByteProvenance {
    /// Real wire bytes a decoder can read back. The detail names the encoding rules —
    /// `uper` for the ASN.1 Unaligned Packed Encoding Rules, `coer` for the Canonical
    /// Octet Encoding Rules that IEEE 1609.2 and ETSI TS 103 097 mandate for the security
    /// envelope.
    Real(String),
    /// A validated size model. The detail is its version, because a recorded size is only
    /// interpretable against the version of the model that produced it.
    SizeModel(String),
    /// The engine declared nothing for this message type. Its bytes are counted as neither
    /// real nor modelled.
    Undeclared,
}

impl ByteProvenance {
    /// True when the bytes are real wire bytes.
    #[must_use]
    pub fn is_real(&self) -> bool {
        matches!(self, ByteProvenance::Real(_))
    }

    /// True when the bytes are placeholders of a modelled length.
    #[must_use]
    pub fn is_modelled(&self) -> bool {
        matches!(self, ByteProvenance::SizeModel(_))
    }

    /// A short phrase for a datasheet cell.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            ByteProvenance::Real(encoding) => format!("real bytes ({encoding})"),
            ByteProvenance::SizeModel(version) => format!("size model {version}"),
            ByteProvenance::Undeclared => "**undeclared**".to_string(),
        }
    }
}

/// What one message type contributed to a dataset's byte counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBytes {
    /// How many transmissions the recording carried.
    pub messages: u64,
    /// Their total `bytes_on_wire`.
    pub bytes_on_wire: u64,
    /// The sum of `payload_bytes` over the transmissions that stated one.
    pub payload_bytes: u64,
    /// How many transmissions stated a `payload_bytes`. Carried because a sum over a
    /// subset is not a sum: a payload total that covered a third of the messages would
    /// otherwise read as a small payload rather than as a partly-absent field.
    pub payload_stated: u64,
    /// The sum of `envelope_bytes` over the transmissions that stated one.
    pub envelope_bytes: u64,
    /// How many transmissions stated an `envelope_bytes`.
    pub envelope_stated: u64,
}

/// Every message type a recording transmitted, in message-type order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxTally {
    /// `msg_type` → its byte counts. A `BTreeMap`, so the order the datasheet prints is
    /// the message-type order and not an iteration accident.
    pub by_msg_type: BTreeMap<String, MessageBytes>,
}

impl TxTally {
    /// Tallies a recording's `node.tx` views.
    #[must_use]
    pub fn of(transmissions: &[NodeTxView]) -> Self {
        let mut by_msg_type: BTreeMap<String, MessageBytes> = BTreeMap::new();
        for tx in transmissions {
            let key = tx
                .msg_type
                .clone()
                .unwrap_or_else(|| UNSTATED_MSG_TYPE.to_string());
            let row = by_msg_type.entry(key).or_default();
            // Saturating rather than wrapping: these are sums over a whole run, and a byte
            // total that wrapped would be a wrong number rather than a missing one. No
            // realistic run comes close, which is exactly why a silent wrap would never be
            // noticed.
            row.messages = row.messages.saturating_add(1);
            row.bytes_on_wire = row.bytes_on_wire.saturating_add(tx.bytes_on_wire);
            if let Some(payload) = tx.payload_bytes {
                row.payload_bytes = row.payload_bytes.saturating_add(payload);
                row.payload_stated = row.payload_stated.saturating_add(1);
            }
            if let Some(envelope) = tx.envelope_bytes {
                row.envelope_bytes = row.envelope_bytes.saturating_add(envelope);
                row.envelope_stated = row.envelope_stated.saturating_add(1);
            }
        }
        TxTally { by_msg_type }
    }

    /// True when no transmission was recorded at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_msg_type.is_empty()
    }

    /// The total number of transmissions.
    #[must_use]
    pub fn messages(&self) -> u64 {
        self.by_msg_type
            .values()
            .fold(0u64, |acc, row| acc.saturating_add(row.messages))
    }
}

/// One row of the byte-provenance section: a message type, where its bytes came from, and
/// how many of them there were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteProvenanceRow {
    /// The message type as `node.tx` spelled it.
    pub msg_type: String,
    /// What the engine declared for it.
    pub provenance: ByteProvenance,
    /// Its counts.
    pub bytes: MessageBytes,
}

/// The byte-provenance verdict for one dataset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteProvenanceReport {
    /// One row per message type the recording carried, in message-type order.
    pub rows: Vec<ByteProvenanceRow>,
    /// Total transmissions.
    pub messages: u64,
    /// Total `bytes_on_wire`.
    pub bytes_on_wire: u64,
    /// Of those, the bytes a real encoder produced.
    pub real_bytes: u64,
    /// Of those, the bytes a size model predicted.
    pub modelled_bytes: u64,
    /// Of those, the bytes whose message type the engine declared nothing for.
    pub undeclared_bytes: u64,
    /// Declared message types that no transmission used, in declaration-key order.
    ///
    /// Not an error: a scenario legitimately runs without sending every message type the
    /// engine can encode. Reported because a declaration that describes a different run is
    /// how this section would come to be wrong while looking complete.
    pub declared_types_unused: Vec<String>,
    /// The real share of `bytes_on_wire`, in parts per thousand, or `None` when there were
    /// no bytes to divide by.
    ///
    /// Parts per thousand and not a float: see the module documentation.
    pub real_share_permille: Option<u64>,
}

impl ByteProvenanceReport {
    /// Joins a tally against the engine's declaration.
    #[must_use]
    pub fn new(tally: &TxTally, declared: &BTreeMap<String, ByteProvenance>) -> Self {
        let mut report = ByteProvenanceReport::default();
        let mut used: BTreeSet<&str> = BTreeSet::new();
        for (msg_type, bytes) in &tally.by_msg_type {
            let provenance = match declared.get(msg_type) {
                Some(p) => {
                    used.insert(msg_type.as_str());
                    p.clone()
                }
                None => ByteProvenance::Undeclared,
            };
            report.messages = report.messages.saturating_add(bytes.messages);
            report.bytes_on_wire = report.bytes_on_wire.saturating_add(bytes.bytes_on_wire);
            if provenance.is_real() {
                report.real_bytes = report.real_bytes.saturating_add(bytes.bytes_on_wire);
            } else if provenance.is_modelled() {
                report.modelled_bytes = report.modelled_bytes.saturating_add(bytes.bytes_on_wire);
            } else {
                report.undeclared_bytes =
                    report.undeclared_bytes.saturating_add(bytes.bytes_on_wire);
            }
            report.rows.push(ByteProvenanceRow {
                msg_type: msg_type.clone(),
                provenance,
                bytes: bytes.clone(),
            });
        }
        report.declared_types_unused = declared
            .keys()
            .filter(|k| !used.contains(k.as_str()))
            .cloned()
            .collect();
        report.real_share_permille = if report.bytes_on_wire == 0 {
            None
        } else {
            // Integer arithmetic throughout, rounding down: a share printed as 99.9 % when
            // it is not quite 100 % is the safe direction for a claim about citability.
            Some(report.real_bytes.saturating_mul(1000) / report.bytes_on_wire)
        };
        report
    }

    /// True when every recorded byte came from a real encoder.
    ///
    /// The one question this section exists to answer: whether a byte-count or
    /// overhead result from this dataset is a measurement of an encoding or a prediction
    /// of a model. `false` does not make the dataset less useful — it makes one class of
    /// claim about it unavailable.
    #[must_use]
    pub fn all_real(&self) -> bool {
        self.bytes_on_wire > 0 && self.modelled_bytes == 0 && self.undeclared_bytes == 0
    }

    /// True when nothing was transmitted, so there is no byte provenance to state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// One sentence a datasheet can print verbatim.
    #[must_use]
    pub fn verdict(&self) -> String {
        if self.is_empty() {
            return "No transmission was recorded, so this dataset carries no byte counts \
                    and no byte-count result can be drawn from it."
                .to_string();
        }
        if self.all_real() {
            return format!(
                "All {} recorded bytes across {} transmissions are real wire bytes. A \
                 byte-count or envelope-overhead result from this dataset is a measurement \
                 of an encoding and is citable as one.",
                self.bytes_on_wire, self.messages
            );
        }
        let share = self
            .real_share_permille
            .map(|p| format!("{}.{} %", p / 10, p % 10))
            .unwrap_or_else(|| "n/a".to_string());
        format!(
            "{} of {} recorded bytes ({share}) are real wire bytes; {} come from a size \
             model and {} from a message type the engine declared nothing for. A \
             byte-count or envelope-overhead result from this dataset is therefore **not** \
             a measurement of an encoding, and must be reported as a prediction of the \
             size model named in the table below.",
            self.real_bytes, self.bytes_on_wire, self.modelled_bytes, self.undeclared_bytes
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ids::NodeId;

    fn tx(msg_type: Option<&str>, bytes: u64, payload: Option<u64>) -> NodeTxView {
        NodeTxView {
            t: 0,
            node: NodeId(1),
            msg: Some(1),
            msg_type: msg_type.map(str::to_string),
            bytes_on_wire: bytes,
            payload_bytes: payload,
            envelope_bytes: Some(111),
            airtime_us: None,
            mcs: None,
            power_dbm: None,
            channel: None,
            ac: None,
            dcc_state: None,
            signer: None,
            t_generated: None,
            t_sign_start: None,
            t_signed: None,
            mac_aifs_ns: None,
            mac_backoff_ns: None,
            net_header_bytes: None,
            link_bytes: None,
            frag_header_bytes: None,
            spdu_bytes: None,
            cert_bytes: None,
            pseudonym: None,
            content: None,
        }
    }

    fn declared() -> BTreeMap<String, ByteProvenance> {
        BTreeMap::from([
            ("cam".to_string(), ByteProvenance::Real("uper".to_string())),
            (
                "psm".to_string(),
                ByteProvenance::SizeModel("1.0.0".to_string()),
            ),
        ])
    }

    #[test]
    fn a_fully_real_dataset_says_its_byte_counts_are_citable() {
        let tally = TxTally::of(&[tx(Some("cam"), 400, Some(289)), tx(Some("cam"), 402, None)]);
        let report = ByteProvenanceReport::new(&tally, &declared());
        assert!(report.all_real());
        assert_eq!(report.bytes_on_wire, 802);
        assert_eq!(report.real_bytes, 802);
        assert_eq!(report.real_share_permille, Some(1000));
        assert!(report.verdict().contains("citable"));
        // One `cam` row stated a payload and one did not, and the row says so rather than
        // presenting a sum over half the messages as a sum.
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].bytes.payload_stated, 1);
        assert_eq!(report.rows[0].bytes.messages, 2);
        assert_eq!(report.declared_types_unused, vec!["psm".to_string()]);
    }

    #[test]
    fn a_modelled_message_makes_an_overhead_result_uncitable() {
        let tally = TxTally::of(&[tx(Some("cam"), 400, Some(289)), tx(Some("psm"), 100, Some(39))]);
        let report = ByteProvenanceReport::new(&tally, &declared());
        assert!(!report.all_real());
        assert_eq!(report.real_bytes, 400);
        assert_eq!(report.modelled_bytes, 100);
        assert_eq!(report.undeclared_bytes, 0);
        assert_eq!(report.real_share_permille, Some(800));
        let verdict = report.verdict();
        assert!(verdict.contains("not"), "{verdict}");
        assert!(verdict.contains("size model"), "{verdict}");
        assert!(verdict.contains("80.0 %"), "{verdict}");
    }

    /// The property that matters most: an engine that forgets to declare a codec does not
    /// thereby get citable byte counts.
    #[test]
    fn an_undeclared_message_type_is_neither_real_nor_modelled() {
        let tally = TxTally::of(&[tx(Some("cam"), 400, None), tx(Some("bsm"), 421, None)]);
        let report = ByteProvenanceReport::new(&tally, &declared());
        assert!(!report.all_real());
        assert_eq!(report.real_bytes, 400);
        assert_eq!(report.modelled_bytes, 0);
        assert_eq!(report.undeclared_bytes, 421);
        assert!(
            report
                .rows
                .iter()
                .any(|r| r.msg_type == "bsm" && r.provenance == ByteProvenance::Undeclared)
        );
    }

    #[test]
    fn a_transmission_with_no_message_type_gets_its_own_bucket() {
        let tally = TxTally::of(&[tx(None, 300, None)]);
        assert!(tally.by_msg_type.contains_key(UNSTATED_MSG_TYPE));
        let report = ByteProvenanceReport::new(&tally, &declared());
        assert_eq!(report.undeclared_bytes, 300);
        assert_eq!(report.rows[0].msg_type, UNSTATED_MSG_TYPE);
    }

    #[test]
    fn an_empty_dataset_claims_nothing() {
        let report = ByteProvenanceReport::new(&TxTally::default(), &declared());
        assert!(report.is_empty());
        assert!(!report.all_real(), "zero bytes is not 'all real'");
        assert_eq!(report.real_share_permille, None);
        assert!(report.verdict().contains("no byte counts"));
    }

    #[test]
    fn the_rows_are_in_message_type_order_whatever_order_the_records_arrived_in() {
        let forwards = TxTally::of(&[tx(Some("cam"), 1, None), tx(Some("psm"), 2, None)]);
        let backwards = TxTally::of(&[tx(Some("psm"), 2, None), tx(Some("cam"), 1, None)]);
        let a = ByteProvenanceReport::new(&forwards, &declared());
        let b = ByteProvenanceReport::new(&backwards, &declared());
        assert_eq!(a, b);
        assert_eq!(a.rows[0].msg_type, "cam");
    }
}
