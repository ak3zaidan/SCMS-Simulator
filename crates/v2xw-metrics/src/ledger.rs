//! The [`EventLedger`]: a run's records, decoded and kept, so an invariant can be checked
//! against them.
//!
//! A metric provider reduces as it goes and keeps no history. An invariant check is the
//! opposite: "every byte is attributed to exactly one bucket" is a statement about the whole
//! run's byte attributions together, and there is no way to check it one record at a time.
//! The ledger is that history — the records this crate knows how to read, decoded once into
//! their [`crate::channels`] views and grouped by channel, plus the bookkeeping the checks
//! need (what failed to decode, which channels were unknown, and what visibility tag each
//! record on each channel carried).
//!
//! # It keeps everything, on purpose
//!
//! A ledger over a long run is large: a million `phy.rx` records at a hundred bytes each is
//! a hundred megabytes. That is the right trade for what it is for — a conformance run, a
//! test fixture, a post-run audit of a recording — and the wrong one for a production run
//! of a city for a week. So the invariant checks take a `&EventLedger` and nothing in the
//! metric path does: a run that wants the checks builds a ledger over the window or the
//! recording it wants to audit, and a run that does not pays nothing. The documentation on
//! each check says what it needs, so a caller can build a ledger over only those channels
//! with [`EventLedger::only`].

use std::collections::BTreeMap;

use v2xw_core::ctx::{OwnedRecord, Visibility};

use crate::channels::{
    ChannelView, DetObservationView, GtAttackActionView, GtKinematicsView, MaDecisionView,
    MaReportView, MacCbrView, NetBytesView, NetFragView, NodeRxView, NodeTelemetryView, NodeTxView,
    NodeVerifyView, PhyRxView, ProtoMsgView, ProtoRevocationView, SecCertView, decode,
};
use crate::latency::LatencyTrace;

/// A run's decoded records, in arrival order per channel.
///
/// Arrival order is preserved because two invariants are *about* it: I-M1 (mobility output
/// is ordered by `ActorId`) and I-C1 (two runs emit byte-identical records). A ledger that
/// sorted its contents would make both unfalsifiable.
#[derive(Debug, Default)]
pub struct EventLedger {
    /// If set, only these channels are ingested.
    filter: Option<Vec<&'static str>>,

    /// `node.tx`, in arrival order.
    pub tx: Vec<NodeTxView>,
    /// `phy.rx`, in arrival order.
    pub rx: Vec<PhyRxView>,
    /// `node.rx`, in arrival order.
    pub node_rx: Vec<NodeRxView>,
    /// `msg.latency`, in arrival order.
    pub latency: Vec<LatencyTrace>,
    /// `mac.cbr`, in arrival order.
    pub cbr: Vec<MacCbrView>,
    /// `net.frag`, in arrival order.
    pub frag: Vec<NetFragView>,
    /// `net.bytes`, in arrival order.
    pub bytes: Vec<NetBytesView>,
    /// `proto.msg`, in arrival order.
    pub proto_msg: Vec<ProtoMsgView>,
    /// `node.verify`, in arrival order.
    pub verify: Vec<NodeVerifyView>,
    /// `node.telemetry`, in arrival order.
    pub telemetry: Vec<NodeTelemetryView>,
    /// `sec.cert`, in arrival order.
    pub cert: Vec<SecCertView>,
    /// `proto.revocation`, in arrival order.
    pub revocation: Vec<ProtoRevocationView>,
    /// `det.observation`, in arrival order.
    pub observations: Vec<DetObservationView>,
    /// `ma.report`, in arrival order.
    pub reports: Vec<MaReportView>,
    /// `ma.decision`, in arrival order.
    pub decisions: Vec<MaDecisionView>,
    /// `gt.kinematics`, in arrival order.
    pub kinematics: Vec<GtKinematicsView>,
    /// `gt.attack.action`, in arrival order.
    pub attacks: Vec<GtAttackActionView>,

    /// Every `(channel, visibility)` pair seen, with how many records carried it — the
    /// input to the leakage invariant. Ordered, so the report is stable.
    pub visibility_seen: BTreeMap<(String, Visibility), u64>,
    /// Records that did not decode, per channel.
    pub decode_failures: BTreeMap<String, u64>,
    /// Records on channels this crate has no view for, per channel.
    pub unknown_channels: BTreeMap<String, u64>,
    /// Every record ingested, in arrival order.
    pub ingested: u64,
}

impl EventLedger {
    /// An empty ledger that ingests every channel this crate knows.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty ledger that ingests only `channels`.
    ///
    /// Records on other known channels are counted into [`EventLedger::ingested`] and
    /// otherwise dropped, so a caller that only needs the byte-accounting invariant can
    /// audit a long recording without keeping its `phy.rx` records.
    #[must_use]
    pub fn only(channels: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            filter: Some(channels.into_iter().collect()),
            ..Self::default()
        }
    }

    fn wanted(&self, channel: &str) -> bool {
        self.filter.as_ref().is_none_or(|f| f.contains(&channel))
    }

    /// Ingests one record.
    ///
    /// Never fails: a record that does not decode is counted into
    /// [`EventLedger::decode_failures`] and a record on an unknown channel into
    /// [`EventLedger::unknown_channels`], because an audit that stopped at the first
    /// malformed record would report one problem instead of all of them.
    pub fn ingest(&mut self, rec: &OwnedRecord) {
        self.ingested += 1;
        *self
            .visibility_seen
            .entry((rec.channel.to_string(), rec.visibility))
            .or_insert(0) += 1;
        if !self.wanted(rec.channel) {
            return;
        }
        // One arm per channel. A macro would hide which channels are covered, and the
        // coverage is the point: a channel missing from this list is a channel no invariant
        // in this crate can check.
        macro_rules! take {
            ($view:ty, $field:ident) => {{
                match decode::<$view>(rec) {
                    Ok(v) => self.$field.push(v),
                    Err(_) => {
                        *self
                            .decode_failures
                            .entry(rec.channel.to_string())
                            .or_insert(0) += 1;
                    }
                }
                return;
            }};
        }
        match rec.channel {
            NodeTxView::CHANNEL => take!(NodeTxView, tx),
            PhyRxView::CHANNEL => take!(PhyRxView, rx),
            NodeRxView::CHANNEL => take!(NodeRxView, node_rx),
            crate::latency::MSG_LATENCY => take!(LatencyTrace, latency),
            MacCbrView::CHANNEL => take!(MacCbrView, cbr),
            NetFragView::CHANNEL => take!(NetFragView, frag),
            NetBytesView::CHANNEL => take!(NetBytesView, bytes),
            ProtoMsgView::CHANNEL => take!(ProtoMsgView, proto_msg),
            NodeVerifyView::CHANNEL => take!(NodeVerifyView, verify),
            NodeTelemetryView::CHANNEL => take!(NodeTelemetryView, telemetry),
            SecCertView::CHANNEL => take!(SecCertView, cert),
            ProtoRevocationView::CHANNEL => take!(ProtoRevocationView, revocation),
            DetObservationView::CHANNEL => take!(DetObservationView, observations),
            MaReportView::CHANNEL => take!(MaReportView, reports),
            MaDecisionView::CHANNEL => take!(MaDecisionView, decisions),
            GtKinematicsView::CHANNEL => take!(GtKinematicsView, kinematics),
            GtAttackActionView::CHANNEL => take!(GtAttackActionView, attacks),
            other => {
                *self.unknown_channels.entry(other.to_string()).or_insert(0) += 1;
            }
        }
    }

    /// Ingests a whole stream.
    pub fn ingest_all<'a>(&mut self, records: impl IntoIterator<Item = &'a OwnedRecord>) {
        for r in records {
            self.ingest(r);
        }
    }

    /// The total number of records that failed to decode.
    #[must_use]
    pub fn decode_failure_total(&self) -> u64 {
        self.decode_failures.values().sum()
    }

    /// True if nothing was ingested.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.ingested == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(channel: &'static str, visibility: Visibility, json: serde_json::Value) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    #[test]
    fn it_decodes_every_channel_it_knows_and_keeps_the_order() {
        let mut l = EventLedger::new();
        l.ingest(&rec(
            "node.tx",
            Visibility::Node,
            json!({"t":1,"node":1,"bytes_on_wire":100}),
        ));
        l.ingest(&rec(
            "node.tx",
            Visibility::Node,
            json!({"t":2,"node":2,"bytes_on_wire":200}),
        ));
        l.ingest(&rec(
            "phy.rx",
            Visibility::NodeAndGt,
            json!({"t_start":0,"t_end":1,"rx":3,"outcome":"ok"}),
        ));
        assert_eq!(l.ingested, 3);
        assert_eq!(l.tx.len(), 2);
        assert_eq!(l.tx[0].t, 1, "arrival order is kept");
        assert_eq!(l.tx[1].t, 2);
        assert_eq!(l.rx.len(), 1);
        assert_eq!(
            l.visibility_seen
                .get(&("node.tx".to_string(), Visibility::Node)),
            Some(&2)
        );
    }

    #[test]
    fn a_malformed_record_is_counted_and_the_audit_continues() {
        let mut l = EventLedger::new();
        l.ingest(&rec("mac.cbr", Visibility::Node, json!({"t":1})));
        l.ingest(&rec(
            "mac.cbr",
            Visibility::Node,
            json!({"t":2,"node":1,"cbr":0.3}),
        ));
        assert_eq!(l.decode_failure_total(), 1);
        assert_eq!(l.cbr.len(), 1, "the good record still landed");
    }

    #[test]
    fn an_unknown_channel_is_counted_not_dropped_silently() {
        let mut l = EventLedger::new();
        l.ingest(&rec("a.plugin.invented.this", Visibility::Node, json!({})));
        assert_eq!(l.unknown_channels.get("a.plugin.invented.this"), Some(&1));
        assert_eq!(l.ingested, 1);
    }

    #[test]
    fn a_filtered_ledger_keeps_only_what_was_asked_for() {
        let mut l = EventLedger::only(["node.tx"]);
        l.ingest(&rec(
            "node.tx",
            Visibility::Node,
            json!({"t":1,"node":1,"bytes_on_wire":100}),
        ));
        l.ingest(&rec(
            "phy.rx",
            Visibility::NodeAndGt,
            json!({"t_start":0,"t_end":1,"rx":3,"outcome":"ok"}),
        ));
        assert_eq!(l.tx.len(), 1);
        assert!(l.rx.is_empty());
        assert_eq!(l.ingested, 2, "the visibility tally still sees everything");
        assert_eq!(
            l.visibility_seen
                .get(&("phy.rx".to_string(), Visibility::NodeAndGt)),
            Some(&1)
        );
    }

    #[test]
    fn an_empty_ledger_says_so() {
        assert!(EventLedger::new().is_empty());
    }
}
