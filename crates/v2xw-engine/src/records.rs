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
    ByteBucket, GtKinematicsView, MacCbrView, NetBytesView, NodeRxView, NodeTelemetryView,
    NodeTxView, PhyRxView, RxFate, RxOutcome, SignerId,
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
    /// `phy.prr` — one frame's reception census: receivers truly within each 20 m range
    /// and how many of them decoded it (3GPP TR 36.885 §A.2.1.4). Ground truth whole.
    PhyPrr,
    v2xw_metrics::channels::PhyPrrView,
    "phy.prr",
    Visibility::Gt
);
channel_record!(
    /// `node.rx` — one reception attempt followed from the PHY to its fate, with every
    /// stamp of the message's journey. Node-and-ground-truth, like `phy.rx`.
    NodeRx,
    NodeRxView,
    "node.rx",
    Visibility::NodeAndGt
);
channel_record!(
    /// `net.reassembly` — one fragmented SDU followed to its fate at one receiver, with the
    /// loss its fragments' PHY success probabilities predicted. Ground truth whole.
    NetReassembly,
    v2xw_metrics::channels::NetReassemblyView,
    "net.reassembly",
    Visibility::Gt
);
channel_record!(
    /// `net.bytes` — one transfer on an accounting bucket other than the air.
    NetBytes,
    NetBytesView,
    "net.bytes",
    Visibility::Node
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

/// `scenario.event` — one scenario timeline item taking effect or ending, and what it did.
///
/// PUBLIC: every field restates the scenario document or says what the engine did with
/// it; nothing here is a vehicle's ground truth. The view is defined here rather than in
/// `v2xw-metrics` because no metric reads it — the page and the run log do.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScenarioEventView {
    /// The instant it took effect.
    pub t: SimTime,
    /// Its position in the scenario's `events` list.
    pub index: u32,
    /// Its `type`, e.g. `closure`.
    pub kind: String,
    /// `start`, or `end` when its `until` arrived.
    pub phase: String,
    /// What it did, as a sentence.
    pub effect: String,
    /// The lanes a closure closed or reopened, ascending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<u32>,
    /// The demand multiplier in force after it, for a demand change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplier: Option<f64>,
    /// The parameter a `param.change` set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The value it set, as JSON text (so a float is recorded exactly as written).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The attacker populations an `attack.wave` names, by index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub populations: Vec<u32>,
}

impl ScenarioEventView {
    /// A record for item `index` of kind `kind` at `t`, its effect still to be written.
    pub fn new(t: SimTime, index: usize, kind: crate::scenario::TimelineKind, end: bool) -> Self {
        ScenarioEventView {
            t,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            kind: serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            phase: if end { "end" } else { "start" }.to_string(),
            effect: String::new(),
            lanes: Vec::new(),
            multiplier: None,
            path: None,
            value: None,
            populations: Vec::new(),
        }
    }
}

channel_record!(
    /// `scenario.event` — a scenario timeline item taking effect.
    ScenarioEvent,
    ScenarioEventView,
    "scenario.event",
    Visibility::Public
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
            // Signed and longitudinal — along the heading — as vwp-v1 §3.3.2's `accel_cq`
            // column and every consumer read it. The magnitude of the vector was recorded
            // here, so a braking car streamed a *positive* acceleration.
            acc_mps2: Some(q3(longitudinal(q.acc.x, q.acc.y, q.heading_rad))),
            heading_rad: Some(q.heading_rad),
            lane: q.lane.map(|l| l.lane.0),
            lane_pos_m: q.lane.map(|l| q3(l.s_m)),
            class: Some(class.to_string()),
            node: None,
        })
    }

    /// The same record naming the node mounted on the actor.
    #[must_use]
    pub fn with_node(mut self, node: Option<NodeId>) -> Self {
        self.0.node = node;
        self
    }
}

/// The component of a horizontal acceleration along `heading_rad`, m/s².
pub(crate) fn longitudinal(ax: f64, ay: f64, heading_rad: f64) -> f64 {
    let (s, c) = v2xw_core::math::sin_cos(heading_rad);
    ax * c + ay * s
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
            radio: None,
        })
    }

    /// Fills in how the access layer sent the frame.
    #[must_use]
    ///
    /// `mcs_index` is the index in the technology's own table — the 802.11 OFDM rate index
    /// (0 = 3 Mbit/s … 7 = 27 Mbit/s at 10 MHz), the LTE MCS or the NR MCS — which is what
    /// the `mcs` byte of the wire record carries; `radio.mcs` names the table.
    pub fn with_radio(
        mut self,
        mcs_index: Option<u8>,
        radio: Option<v2xw_metrics::channels::TxRadioView>,
    ) -> Self {
        self.0.mcs = mcs_index;
        self.0.radio = radio;
        self
    }

    /// Fills in which pseudonym signed the frame and what the message said.
    #[must_use]
    pub fn with_content(
        mut self,
        pseudonym: Option<String>,
        content: Option<v2xw_metrics::channels::MsgContentView>,
    ) -> Self {
        self.0.pseudonym = pseudonym;
        self.0.content = content;
        self
    }

    /// Fills in the sender's side of the latency decomposition: when signing started, when
    /// the frame reached the MAC, and how much of the channel-access delay was AIFS and how
    /// much the backoff countdown the MAC reported.
    #[must_use]
    pub fn with_journey(
        mut self,
        t_sign_start: SimTime,
        t_signed: SimTime,
        mac_aifs_ns: u64,
        mac_backoff_ns: u64,
    ) -> Self {
        self.0.t_sign_start = Some(t_sign_start);
        self.0.t_signed = Some(t_signed);
        self.0.mac_aifs_ns = Some(mac_aifs_ns);
        self.0.mac_backoff_ns = Some(mac_backoff_ns);
        self
    }

    /// Fills in the frame's layers (`v2xw_net::frame`): the SPDU, the network and transport
    /// header, the link layer and any fragmentation header, which with the payload and
    /// envelope partition `bytes_on_wire`; and the attached certificate's octets.
    #[must_use]
    pub fn with_layers(mut self, l: &v2xw_net::FrameLayers, cert_bytes: Option<u32>) -> Self {
        self.0.spdu_bytes = Some(u64::from(l.spdu_bytes()));
        self.0.net_header_bytes = Some(u64::from(l.network));
        self.0.link_bytes = Some(u64::from(l.link_bytes()));
        self.0.frag_header_bytes = Some(u64::from(l.fragmentation));
        self.0.cert_bytes = cert_bytes.map(u64::from);
        self
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

impl NodeRx {
    /// A reception attempt at `rx`, not yet resolved: the PHY's measurements and the
    /// sender's side of the journey, quantised (D9). The engine fills in the fate.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn attempt(
        tx: NodeId,
        rx: NodeId,
        msg: u64,
        msg_type: &str,
        rssi_dbm: f64,
        sinr_db: f64,
        dist_m: f64,
        bytes_on_wire: u64,
        airtime_us: u64,
        payload_bytes: Option<u64>,
    ) -> Self {
        NodeRx(NodeRxView {
            t: 0,
            rx,
            tx: Some(tx),
            msg: Some(msg),
            msg_type: Some(msg_type.to_string()),
            outcome: RxFate::InFlight,
            cause: None,
            verification: None,
            rssi_dbm: Some(quantize_to(rssi_dbm, Q_DB)),
            sinr_db: Some(quantize_to(sinr_db, Q_DB)),
            dist_m: Some(q3(dist_m)),
            bytes_on_wire: Some(bytes_on_wire),
            airtime_us: Some(airtime_us),
            payload_bytes,
            t_generated: None,
            t_sign_start: None,
            t_signed: None,
            mac_aifs_ns: None,
            mac_backoff_ns: None,
            t_tx_start: None,
            t_tx_end: None,
            t_arrival: None,
            t_rx_done: None,
            t_verify_start: None,
            t_verify_done: None,
            t_delivered: None,
        })
    }

    /// The sender's side of the message's journey and the instant it reached this
    /// receiver, all on the simulation's timeline.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn journey(
        mut self,
        t_generated: SimTime,
        t_sign_start: SimTime,
        t_signed: SimTime,
        mac_aifs_ns: u64,
        mac_backoff_ns: u64,
        t_tx_start: SimTime,
        t_tx_end: SimTime,
        t_arrival: SimTime,
    ) -> Self {
        self.0.t_generated = Some(t_generated);
        self.0.t_sign_start = Some(t_sign_start);
        self.0.t_signed = Some(t_signed);
        self.0.mac_aifs_ns = Some(mac_aifs_ns);
        self.0.mac_backoff_ns = Some(mac_backoff_ns);
        self.0.t_tx_start = Some(t_tx_start);
        self.0.t_tx_end = Some(t_tx_end);
        self.0.t_arrival = Some(t_arrival);
        self
    }

    /// Resolves the attempt as lost to `cause` at `t`.
    #[must_use]
    pub fn lost(mut self, t: SimTime, cause: &str) -> Self {
        self.0.t = t;
        self.0.outcome = RxFate::Lost;
        self.0.cause = Some(cause.to_string());
        self
    }

    /// Resolves the attempt as delivered to the applications at `delivered`.
    #[must_use]
    pub fn delivered(mut self, t: SimTime, verification: &str, delivered: SimTime) -> Self {
        self.0.t = t;
        self.0.outcome = RxFate::Delivered;
        self.0.cause = None;
        self.0.verification = Some(verification.to_string());
        self.0.t_delivered = Some(delivered);
        self
    }

    /// Marks the attempt as still between the PHY and the application when the run ended.
    #[must_use]
    pub fn in_flight(mut self, t: SimTime) -> Self {
        self.0.t = t;
        self.0.outcome = RxFate::InFlight;
        self
    }
}

impl NetBytes {
    /// One transfer of `bytes_on_wire` octets on `bucket`, identified by `id` (invariant
    /// I-N1 needs the identity to detect a double attribution).
    #[must_use]
    pub fn new(
        t: SimTime,
        id: u64,
        bucket: ByteBucket,
        bytes_on_wire: u64,
        node: Option<NodeId>,
    ) -> Self {
        NetBytes(NetBytesView {
            t,
            id: Some(id),
            bucket,
            bytes_on_wire,
            node,
        })
    }
}

impl MacCbr {
    /// One node's MAC report for a window: the measured busy ratio (quantised to D9's ratio
    /// grid), the EDCA queue depth, and what was offered to and refused by the MAC.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn report(
        t: SimTime,
        node: NodeId,
        channel: u16,
        cbr: f64,
        queue_depth: u64,
        mac_drops: u64,
        offered_frames: u64,
        offered_bytes: u64,
        offered_airtime_us: u64,
        span_ns: u64,
    ) -> Self {
        MacCbr(MacCbrView {
            t,
            node,
            channel: Some(channel),
            cbr: quantize_to(cbr, 1e-4),
            busy_us: None,
            window_us: None,
            queue_depth: Some(queue_depth),
            mac_drops: Some(mac_drops),
            offered_frames: Some(offered_frames),
            offered_bytes: Some(offered_bytes),
            offered_airtime_us: Some(offered_airtime_us),
            span_ns: Some(span_ns),
        })
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
            focus: None,
            copies: None,
            sdu: None,
        })
    }

    /// Tags the link with its place against the focus region and, for a sidelink, the
    /// number of copies combined.
    #[must_use]
    pub fn with_link_tags(mut self, focus: Option<&str>, copies: Option<u32>) -> Self {
        self.0.focus = focus.map(str::to_string);
        self.0.copies = copies;
        self
    }

    /// Marks the attempt as one fragment of the SDU followed on `node.rx` as `sdu`.
    #[must_use]
    pub fn of_sdu(mut self, sdu: Option<u64>) -> Self {
        self.0.sdu = sdu;
        self
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
        assert_eq!(view.acc_mps2, Some(q3(v2xw_core::math::cos(0.5))));
        // A braking vehicle records a negative acceleration.
        let braking = Kinematics {
            acc: Vec3::new(
                -3.0 * v2xw_core::math::cos(0.5),
                -3.0 * v2xw_core::math::sin(0.5),
                0.0,
            ),
            ..k
        };
        let view: GtKinematicsView = decode(
            &GtKinematics::new(ActorId::new(7), &braking, "car")
                .to_owned_record()
                .expect("serialises"),
        )
        .expect("decodes");
        assert!(
            view.acc_mps2.is_some_and(|a| (a + 3.0).abs() < 2e-3),
            "a car braking at 3 m/s² recorded {:?}",
            view.acc_mps2
        );
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

        let rx = NodeRx::attempt(
            NodeId::new(1),
            NodeId::new(2),
            42,
            "bsm",
            -82.123_456,
            11.987_65,
            123.456_789,
            300,
            424,
            Some(40),
        )
        .lost(9, "collision");
        let owned = rx.to_owned_record().expect("serialises");
        assert_eq!(owned.channel, "node.rx");
        assert_eq!(owned.visibility, Visibility::NodeAndGt);
        let view: NodeRxView = decode(&owned).expect("decodes");
        assert_eq!(view.outcome, RxFate::Lost);
        assert_eq!(view.cause.as_deref(), Some("collision"));
        assert_eq!(view.rssi_dbm, Some(-82.12));
        assert_eq!(view.dist_m, Some(123.457));
        assert_eq!(view.airtime_us, Some(424));

        let bytes = NetBytes::new(5, 77, ByteBucket::Backhaul, 256, Some(NodeId::new(4)));
        let owned = bytes.to_owned_record().expect("serialises");
        let view: NetBytesView = decode(&owned).expect("decodes");
        assert_eq!(view.bucket, ByteBucket::Backhaul);
        assert_eq!(view.id, Some(77));

        let cbr = MacCbr::report(5, NodeId::new(4), 172, 0.123_456_7, 3, 1, 10, 3000, 4240, 1);
        let owned = cbr.to_owned_record().expect("serialises");
        let view: MacCbrView = decode(&owned).expect("decodes");
        assert_eq!(view.cbr, 0.1235);
        assert_eq!(view.queue_depth, Some(3));
        assert_eq!(view.offered_bytes, Some(3000));
        v2xw_record::grid::scan_record("mac.cbr", &owned.json).expect("on its declared grid");
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
