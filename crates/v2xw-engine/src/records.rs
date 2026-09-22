//! The writer-side record types, one per channel the engine emits.
//!
//! 03-interfaces.md §14 says "a plug-in reaches this table through one Rust type per
//! channel implementing [`Record`]". `v2xw-metrics` already publishes a *reader-side*
//! view per channel ([`v2xw_metrics::channels`]) — the shape a metric provider decodes.
//! Rather than declare a second, independently mutable struct per channel, each type here
//! is a `#[serde(transparent)]` newtype **around the reader's own view**.
//!
//! That is the point, and it is not a shortcut: writer and reader cannot disagree about a
//! field name, a unit or an optionality, because there is one struct and the wrapper adds
//! only the two constants [`Record`] needs — the channel id and the visibility tag. The
//! defect this closes is the commonest one in a recording pipeline: a producer renames a
//! field, every consumer silently reads `None`, and the metric goes quietly to zero.
//! `every_record_round_trips_through_its_reader_view` is the test that keeps it closed.
//!
//! # Quantisation
//!
//! Build decision D9: every float reaching a recorded artefact is on its field's declared
//! grid. Each constructor here quantises on the way in — metres and seconds at 1e-3, dB at
//! 1e-2 — so a caller cannot emit an unquantised value by forgetting to.

use v2xw_core::ctx::{Record, Visibility};
use v2xw_core::ids::{ActorId, NodeId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::math::{q3, quantize_to};
use v2xw_core::time::SimTime;
use v2xw_metrics::channels::{
    GtKinematicsView, MacCbrView, NodeTelemetryView, NodeTxView, PhyRxView, RxOutcome, SignerId,
};

/// The dB grid every received-power and ratio field is written on (build decision D9).
pub const Q_DB: f64 = 1e-2;

macro_rules! channel_record {
    ($(#[$meta:meta])* $name:ident, $view:ty, $channel:literal, $vis:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize)]
        #[serde(transparent)]
        pub struct $name(
            /// The reader-side view this record serialises as.
            pub $view,
        );

        impl Record for $name {
            const CHANNEL: &'static str = $channel;
            const VISIBILITY: Visibility = $vis;
        }
    };
}

channel_record!(
    /// `gt.kinematics` — an actor's true state.
    GtKinematics,
    GtKinematicsView,
    "gt.kinematics",
    Visibility::Gt
);
channel_record!(
    /// `node.tx` — what a node put on the air.
    NodeTx,
    NodeTxView,
    "node.tx",
    Visibility::Node
);
channel_record!(
    /// `phy.rx` — one reception attempt. Node-and-ground-truth: the receiver's
    /// measurements are its own, the transmitter's identity and the distance are not.
    PhyRx,
    PhyRxView,
    "phy.rx",
    Visibility::NodeAndGt
);
channel_record!(
    /// `mac.cbr` — the channel busy ratio a node measured.
    MacCbr,
    MacCbrView,
    "mac.cbr",
    Visibility::Node
);
channel_record!(
    /// `node.telemetry` — a node's own resource report.
    NodeTelemetry,
    NodeTelemetryView,
    "node.telemetry",
    Visibility::Node
);

impl GtKinematics {
    /// The record for one actor's published state, quantised (D9).
    pub fn new(actor: ActorId, k: &Kinematics, class: &str) -> Self {
        let q = k.quantized();
        GtKinematics(GtKinematicsView {
            t: q.t,
            actor,
            x_m: q.pos.x,
            y_m: q.pos.y,
            z_m: Some(q.pos.z),
            speed_mps: q3(k.ground_speed_mps()),
            acc_mps2: Some(q3(q.acc.x.hypot_like(q.acc.y))),
            heading_rad: Some(q.heading_rad),
            lane: q.lane.map(|l| l.lane.0),
            lane_pos_m: q.lane.map(|l| q3(l.s_m)),
            class: Some(class.to_string()),
        })
    }
}

/// `f64::hypot` is a transcendental in the standard library and therefore banned
/// (ADR 0003, ADR 0004 §4). This is the `v2xw_core::math` spelling of the same quantity.
trait HypotLike {
    fn hypot_like(self, other: f64) -> f64;
}

impl HypotLike for f64 {
    fn hypot_like(self, other: f64) -> f64 {
        v2xw_core::math::hypot(self, other)
    }
}

impl NodeTx {
    /// The record for one frame going on the air, quantised (D9).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        t: SimTime,
        node: NodeId,
        msg: u64,
        msg_type: &str,
        bytes_on_wire: u64,
        airtime_us: u64,
        power_dbm: f64,
        channel: u16,
        signer: SignerId,
        t_generated: SimTime,
    ) -> Self {
        NodeTx(NodeTxView {
            t,
            node,
            msg: Some(msg),
            msg_type: Some(msg_type.to_string()),
            bytes_on_wire,
            payload_bytes: None,
            envelope_bytes: None,
            airtime_us: Some(airtime_us),
            mcs: None,
            power_dbm: Some(quantize_to(power_dbm, Q_DB)),
            channel: Some(channel),
            ac: None,
            dcc_state: None,
            signer: Some(signer),
            t_generated: Some(t_generated),
        })
    }

    /// Fills in the payload/envelope split, when the node's own generator encoded one.
    ///
    /// These two fields were null on every `node.tx` record the Phase 1 build wrote,
    /// because nothing above them knew the split: the node carried a byte *count* and not
    /// bytes. They are what "security overhead as a fraction of airtime" is computed
    /// from, so an aggregation over a recording that finds them null is looking at a run
    /// in which nobody encoded anything — and it can now tell the difference between that
    /// and a zero-byte envelope.
    ///
    /// Both stay `None` for a frame the engine sized from a protocol wire table rather
    /// than encoding (the misbehaviour report and the CRL broadcast); there is no split
    /// to report for a frame whose octets were never built.
    #[must_use]
    pub fn with_sizes(mut self, payload_bytes: Option<u32>, envelope_bytes: Option<u32>) -> Self {
        self.0.payload_bytes = payload_bytes.map(u64::from);
        self.0.envelope_bytes = envelope_bytes.map(u64::from);
        self
    }
}

impl PhyRx {
    /// The record for one reception attempt, quantised (D9).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        t_start: SimTime,
        t_end: SimTime,
        tx: NodeId,
        rx: NodeId,
        msg: u64,
        rssi_dbm: f64,
        sinr_db: f64,
        outcome: RxOutcome,
        cause: Option<&str>,
        dist_m: f64,
    ) -> Self {
        PhyRx(PhyRxView {
            t_start,
            t_end,
            tx: Some(tx),
            rx,
            msg: Some(msg),
            rssi_dbm: Some(quantize_to(rssi_dbm, Q_DB)),
            sinr_db: Some(quantize_to(sinr_db, Q_DB)),
            outcome,
            cause: cause.map(str::to_string),
            causes: Vec::new(),
            dist_m: Some(q3(dist_m)),
            candidate: true,
            payload_bytes: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ctx::ErasedRecord;
    use v2xw_core::geom::Vec3;
    use v2xw_metrics::channels::decode;

    /// Every record this crate writes decodes back into the reader-side view of its own
    /// channel, with the values intact. This is the guard against a writer and a reader
    /// drifting apart: if they were two structs, a renamed field would pass here only
    /// because the reader defaults it to `None`, so the assertions check *values*.
    #[test]
    fn every_record_round_trips_through_its_reader_view() {
        let k = Kinematics {
            t: 1_000_000,
            pos: Vec3::new(12.3456789, -4.2, 1.5),
            vel: Vec3::new(3.0, 4.0, 0.0),
            acc: Vec3::new(1.0, 0.0, 0.0),
            heading_rad: 0.5,
            yaw_rate_rad_s: 0.0,
            lane: None,
            dims: Default::default(),
        };
        let gt = GtKinematics::new(ActorId::new(7), &k, "car");
        let owned = gt.to_owned_record().expect("serialises");
        assert_eq!(owned.channel, "gt.kinematics");
        assert_eq!(owned.visibility, Visibility::Gt);
        let view: GtKinematicsView = decode(&owned).expect("decodes");
        assert_eq!(view.actor, ActorId::new(7));
        assert_eq!(view.speed_mps, 5.0);
        // D9: the writer quantised, so the reader sees a value on the 1 mm grid.
        assert_eq!(view.x_m, 12.346);

        let tx = NodeTx::new(
            2_000_000,
            NodeId::new(3),
            42,
            "bsm",
            400,
            600,
            20.0,
            172,
            SignerId::Digest,
            1_900_000,
        );
        let owned = tx.to_owned_record().expect("serialises");
        let view: NodeTxView = decode(&owned).expect("decodes");
        assert_eq!(view.node, NodeId::new(3));
        assert_eq!(view.bytes_on_wire, 400);
        assert_eq!(view.signer, Some(SignerId::Digest));
        assert_eq!(view.t_generated, Some(1_900_000));

        let rx = PhyRx::new(
            1,
            2,
            NodeId::new(1),
            NodeId::new(2),
            42,
            -82.123_456,
            11.987_65,
            RxOutcome::Ok,
            None,
            123.456_789,
        );
        let owned = rx.to_owned_record().expect("serialises");
        assert_eq!(owned.visibility, Visibility::NodeAndGt);
        let view: PhyRxView = decode(&owned).expect("decodes");
        assert_eq!(view.rssi_dbm, Some(-82.12));
        assert_eq!(view.sinr_db, Some(11.99));
        assert_eq!(view.dist_m, Some(123.457));
        assert_eq!(view.outcome, RxOutcome::Ok);
    }

    /// Every float in a `gt.kinematics` record sits on its declared grid, which is the
    /// property the D9 output scan asserts over a whole run.
    #[test]
    fn every_recorded_float_is_on_its_declared_grid() {
        let k = Kinematics {
            t: 0,
            pos: Vec3::new(1.0 / 3.0, 2.0 / 7.0, 1.0 / 11.0),
            vel: Vec3::new(1.0 / 13.0, 1.0 / 17.0, 0.0),
            acc: Vec3::new(1.0 / 19.0, 1.0 / 23.0, 0.0),
            heading_rad: 1.0 / 29.0,
            yaw_rate_rad_s: 0.0,
            lane: None,
            dims: Default::default(),
        };
        let owned = GtKinematics::new(ActorId::new(0), &k, "car")
            .to_owned_record()
            .expect("serialises");
        let value: serde_json::Value = serde_json::from_slice(&owned.json).expect("json");
        let obj = value.as_object().expect("object");
        for (name, v) in obj {
            let Some(x) = v.as_f64() else { continue };
            let quantum = if name == "heading_rad" {
                Kinematics::Q_RAD
            } else {
                Kinematics::Q_M
            };
            assert!(
                v2xw_core::math::is_on_grid(x, quantum),
                "{name} = {x} is off its {quantum} grid"
            );
        }
    }
}
