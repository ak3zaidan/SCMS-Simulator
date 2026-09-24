//! The channel catalogue — 03-interfaces.md §14 and `vwp-v1.md` §3.6.2.
//!
//! One table, three consumers: the recorder (which MCAP topic, which schema, which
//! metadata), the `NODE-only` profile (which channels are ground truth and must be
//! stripped whole), and the exporters (which tables to write). Keeping it in one place is
//! what stops the three drifting apart, which is the failure mode ADR 0008's "one schema
//! set" consequence was written against.
//!
//! # Two topic namespaces, deliberately
//!
//! Build decision D11 item 5 fixes two encodings and says not to unify them, and this
//! table is where that shows up:
//!
//! * `vwp/…` topics carry **VWP v1 frames verbatim**, header included, message encoding
//!   `vwp1` (§7.1). `snapshot.keyframe` and `snapshot.delta` live here and nowhere else:
//!   storing the bytes that went over the wire is what makes live and replay provably
//!   identical (§7.2), and a re-encode would destroy the guarantee.
//! * `record/…` topics carry **serde `Record` JSON**, message encoding `json`, one topic
//!   per event, telemetry and metric family. These are the rows Parquet, Arrow IPC and
//!   JSONL are written from.
//!
//! The wire specification's §7.1 mapping table describes only the first namespace,
//! because it is describing a UI stream; D11 item 5 adds the second, because a dataset
//! wants a self-describing columnar row and not a packed C struct. A recording may carry
//! either or both, and the recorder refuses to mix them on one topic.

use v2xw_core::Visibility;

/// One channel of 03-interfaces.md §14.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSpec {
    /// The stable channel id, e.g. `"node.tx"` — [`v2xw_core::Record::CHANNEL`].
    pub name: &'static str,
    /// The numeric id used in an `Event` frame's index (§3.6.2); `None` for a family
    /// that is never carried in an `Event` frame.
    pub wire_id: Option<u16>,
    /// The visibility tag ([`v2xw_core::Record::VISIBILITY`]).
    pub visibility: Visibility,
    /// The fixed payload size in an `Event` frame, in bytes (§3.6.4–§3.6.17).
    pub payload_bytes: Option<usize>,
}

impl ChannelSpec {
    /// True if the channel must be stripped whole by the `NODE-only` profile (§5.2).
    ///
    /// Only [`Visibility::Gt`] channels go; a mixed channel survives with its ground-truth
    /// *columns* blanked, because a node really does observe the rest of the record.
    pub const fn is_ground_truth_channel(&self) -> bool {
        matches!(self.visibility, Visibility::Gt)
    }

    /// True if any part of the record is ground truth
    /// ([`Visibility::is_gt_tainted`]) — the tag the recorder writes into the MCAP
    /// channel metadata so a consumer can filter without knowing this table.
    pub const fn is_gt_tainted(&self) -> bool {
        self.visibility.is_gt_tainted()
    }

    /// The MCAP topic for the serde `Record` encoding of this channel.
    pub fn record_topic(&self) -> String {
        format!("record/{}", self.name)
    }

    /// The MCAP topic for the VWP `Event`-frame encoding of this channel (§7.1).
    pub fn event_topic(&self) -> String {
        format!("vwp/event/{}", self.name)
    }
}

/// The channel table, in ascending name order so that iteration is deterministic.
///
/// `snapshot.keyframe` and `snapshot.delta` appear here for their visibility tag and
/// their wire id; they are delivered as `Keyframe` and `Delta` frames, never as `Event`
/// payloads, so their `payload_bytes` is `None`.
pub const CHANNELS: &[ChannelSpec] = &[
    ChannelSpec {
        name: "app.warning",
        wire_id: Some(40),
        visibility: Visibility::Node,
        payload_bytes: Some(32),
    },
    ChannelSpec {
        name: "det.observation",
        wire_id: Some(30),
        visibility: Visibility::Node,
        payload_bytes: Some(32),
    },
    ChannelSpec {
        name: "gt.attack.action",
        wire_id: Some(2),
        visibility: Visibility::Gt,
        payload_bytes: Some(32),
    },
    ChannelSpec {
        name: "gt.despawn",
        wire_id: Some(4),
        visibility: Visibility::Gt,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "gt.kinematics",
        wire_id: Some(1),
        visibility: Visibility::Gt,
        payload_bytes: Some(56),
    },
    ChannelSpec {
        name: "gt.spawn",
        wire_id: Some(3),
        visibility: Visibility::Gt,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "ma.case",
        wire_id: Some(32),
        visibility: Visibility::Node,
        payload_bytes: Some(40),
    },
    ChannelSpec {
        name: "ma.decision",
        wire_id: Some(33),
        visibility: Visibility::Node,
        payload_bytes: Some(40),
    },
    ChannelSpec {
        name: "ma.report",
        wire_id: Some(31),
        visibility: Visibility::Node,
        payload_bytes: Some(40),
    },
    ChannelSpec {
        name: "mac.cbr",
        wire_id: Some(12),
        visibility: Visibility::Node,
        payload_bytes: Some(16),
    },
    ChannelSpec {
        name: "manifest",
        wire_id: Some(70),
        visibility: Visibility::Meta,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "metric.sample",
        wire_id: Some(50),
        visibility: Visibility::Derived,
        payload_bytes: None,
    },
    // One message's latency decomposed into stages, for a flow other than V2V (a
    // backend exchange, a relay). Its endpoints may be true node ids, so it is tagged as
    // `phy.rx` is.
    ChannelSpec {
        name: "msg.latency",
        wire_id: None,
        visibility: Visibility::NodeAndGt,
        payload_bytes: None,
    },
    // The byte-accounting projection of the `net.*` family (invariant I-N1): one record
    // per transfer on a bucket other than the air, which `node.tx` already carries.
    // Never an `Event` payload.
    ChannelSpec {
        name: "net.bytes",
        wire_id: None,
        visibility: Visibility::Node,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "net.frag",
        wire_id: Some(13),
        visibility: Visibility::Node,
        payload_bytes: Some(24),
    },
    // One fragmented SDU followed to its fate at one receiver, beside the loss its
    // fragments' PHY success probabilities predicted (04-models.md §7.4). Ground truth
    // whole — the sender's identity and those probabilities are no node's — so the
    // NODE-only profile strips it. Never an `Event` payload.
    ChannelSpec {
        name: "net.reassembly",
        wire_id: None,
        visibility: Visibility::Gt,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "node.neighbor",
        wire_id: Some(16),
        visibility: Visibility::Node,
        payload_bytes: Some(32),
    },
    // One reception attempt followed to its fate, with the stamps of its journey. The
    // sender and the distance are ground truth, as on `phy.rx`. Not an `Event` payload:
    // at one record per attempt it is a recording and metric channel, not a UI one.
    ChannelSpec {
        name: "node.rx",
        wire_id: None,
        visibility: Visibility::NodeAndGt,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "node.telemetry",
        wire_id: Some(15),
        visibility: Visibility::Node,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "node.tx",
        wire_id: Some(10),
        visibility: Visibility::Node,
        payload_bytes: Some(40),
    },
    ChannelSpec {
        name: "node.verify",
        wire_id: Some(14),
        visibility: Visibility::Node,
        payload_bytes: Some(48),
    },
    // The per-frame reception census behind the packet reception ratio (3GPP TR 36.885
    // §A.2.1.4): how many equipped receivers were truly within each 20 m range of a frame
    // and how many of them decoded it. Ground truth whole — no node knows who failed to
    // hear it — so the NODE-only profile strips it. Never an `Event` payload.
    ChannelSpec {
        name: "phy.prr",
        wire_id: None,
        visibility: Visibility::Gt,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "phy.rx",
        wire_id: Some(11),
        visibility: Visibility::NodeAndGt,
        payload_bytes: Some(48),
    },
    ChannelSpec {
        name: "proto.msg",
        wire_id: Some(21),
        visibility: Visibility::Node,
        payload_bytes: Some(32),
    },
    ChannelSpec {
        name: "proto.revocation",
        wire_id: Some(22),
        visibility: Visibility::Public,
        payload_bytes: Some(32),
    },
    // A scenario timeline item taking effect (03-interfaces §13): what it was and what the
    // engine did with it. A recording and run-log channel, a handful of records per run; the
    // live server turns it into the timeline `run.status` reports.
    ChannelSpec {
        name: "scenario.event",
        wire_id: None,
        visibility: Visibility::Public,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "sec.cert",
        wire_id: Some(20),
        visibility: Visibility::Node,
        payload_bytes: Some(40),
    },
    ChannelSpec {
        name: "snapshot.delta",
        wire_id: Some(61),
        visibility: Visibility::Mixed,
        payload_bytes: None,
    },
    ChannelSpec {
        name: "snapshot.keyframe",
        wire_id: Some(60),
        visibility: Visibility::Mixed,
        payload_bytes: None,
    },
];

/// The channel with this name, or `None`.
pub fn by_name(name: &str) -> Option<&'static ChannelSpec> {
    CHANNELS.iter().find(|c| c.name == name)
}

/// The channel with this `Event` wire id, or `None` (§3.6.2).
///
/// Ids 1000–65534 are plug-in channels announced in `Hello.channels`; a reader that does
/// not know one skips it by `payload_len` (§3.6.1), which is why this returns an option.
pub fn by_wire_id(id: u16) -> Option<&'static ChannelSpec> {
    CHANNELS.iter().find(|c| c.wire_id == Some(id))
}

/// Every channel the `NODE-only` profile strips whole (§5.2): `gt.kinematics`,
/// `gt.attack.action`, `gt.spawn`, `gt.despawn`.
pub fn ground_truth_channels() -> impl Iterator<Item = &'static ChannelSpec> {
    CHANNELS.iter().filter(|c| c.is_ground_truth_channel())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_unique() {
        for pair in CHANNELS.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "{} must sort before {}",
                pair[0].name,
                pair[1].name
            );
        }
        let mut ids: Vec<u16> = CHANNELS.iter().filter_map(|c| c.wire_id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two channels share a wire id");
    }

    /// §5.2 names the exhaustive set of whole *wire* channels the `node` profile withholds:
    /// the ones a `Hello` channel table can carry. A record-only channel (no wire id) is
    /// never on the wire, so §5.2 does not list it; the recording's `NODE-only` profile
    /// still strips it whole, and the second assertion pins that set too, so a new
    /// ground-truth channel cannot arrive unnoticed in either.
    #[test]
    fn the_ground_truth_channel_set_is_the_one_the_specification_lists() {
        let on_the_wire: Vec<&str> = ground_truth_channels()
            .filter(|c| c.wire_id.is_some())
            .map(|c| c.name)
            .collect();
        assert_eq!(
            on_the_wire,
            vec![
                "gt.attack.action",
                "gt.despawn",
                "gt.kinematics",
                "gt.spawn"
            ]
        );
        let record_only: Vec<&str> = ground_truth_channels()
            .filter(|c| c.wire_id.is_none())
            .map(|c| c.name)
            .collect();
        assert_eq!(record_only, vec!["net.reassembly", "phy.prr"]);
    }

    /// The channel-name pattern of §6.5's `ChannelName` schema.
    ///
    /// One exception, and it is the specification's rather than ours: §6.5's pattern
    /// `^[a-z][a-z0-9]*(\.[a-z][a-z0-9_]*)+$` requires at least one dot, while
    /// 03-interfaces §14 lists a channel named plainly `manifest`. The table keeps the
    /// name the design gives it and this test records the discrepancy instead of hiding
    /// it by renaming a published channel.
    #[test]
    fn every_channel_name_matches_the_published_pattern() {
        for c in CHANNELS {
            let parts: Vec<&str> = c.name.split('.').collect();
            assert!(
                parts.len() >= 2 || c.name == "manifest",
                "{} has no family prefix",
                c.name
            );
            for (i, p) in parts.iter().enumerate() {
                assert!(!p.is_empty(), "{} has an empty segment", c.name);
                assert!(
                    p.starts_with(|ch: char| ch.is_ascii_lowercase()),
                    "{} segment {i} must start with a lower-case letter",
                    c.name
                );
                assert!(
                    p.chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
                    "{} segment {i} has an illegal character",
                    c.name
                );
            }
        }
    }
}
