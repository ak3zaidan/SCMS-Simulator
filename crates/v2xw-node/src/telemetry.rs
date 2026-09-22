//! `NodeTelemetry`: the 208-byte record of vwp-v1 §3.5.2, populated from the runtime.
//!
//! The record's *shape* is not defined here. `v2xw-record` owns
//! [`v2xw_record::wire::telemetry::NodeTelemetry`], all fifty-six of its fields and the
//! encoder that lays them out at the offsets §3.5.2 pins, and this module fills one in.
//! Declaring a second shape would be the classic way to get two structures that agree
//! today and diverge at the next field addition.
//!
//! What this module owns is the *mapping*: which node quantity goes in which field, in
//! which unit, and what a node that does not model a quantity puts there instead. That
//! mapping is the thing that can be silently wrong — `nbr_verified` and `nbr_unverified`
//! have the same type and adjacent offsets — so
//! `tests/telemetry_layout.rs` walks all fifty-six fields against the specification table
//! one at a time rather than asserting on the struct.
//!
//! # Unknown is a value
//!
//! Conformance rule C1 is that every field is populated or explicitly set to its unknown
//! sentinel. [`TelemetryWindow::record`] starts from
//! [`v2xw_record::wire::telemetry::NodeTelemetry::unknown`] and overwrites what the node
//! actually models, so a quantity this runtime does not track reaches the wire as
//! `0xFFFF` or `NaN` — "not modelled" — rather than as a zero a HUD would draw as a real
//! measurement of nothing.

use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};
use v2xw_record::wire::telemetry::NodeTelemetry;

use crate::queue::DropCause;

/// What state a node is in (§3.5.2 offset 193).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeState {
    /// Powered off.
    Off,
    /// Powering up: trust-store checks, CRL catch-up, top-up if due
    /// (06-node-models.md §2.5).
    Booting,
    /// Running.
    Active,
    /// Parked with the radio off after the configured idle time.
    Parked,
    /// Running with reduced capability — an RSU that has lost its backhaul.
    Degraded,
    /// Not transmitting at all.
    Down,
    /// Under an attacker's control. **Ground truth as a value**: §5.2 blanks it to
    /// `Active` on a node-visibility stream.
    Compromised,
}

impl NodeState {
    /// The code §3.5.2 gives this state.
    pub const fn code(self) -> u8 {
        match self {
            NodeState::Off => 0,
            NodeState::Booting => 1,
            NodeState::Active => 2,
            NodeState::Parked => 3,
            NodeState::Degraded => 4,
            NodeState::Down => 5,
            NodeState::Compromised => 6,
        }
    }

    /// Whether the node transmits in this state.
    pub const fn transmits(self) -> bool {
        matches!(
            self,
            NodeState::Active | NodeState::Degraded | NodeState::Compromised
        )
    }
}

/// The counters a node accumulates between telemetry frames.
///
/// One window is one `Telemetry` frame: §3.5.1 carries `window_ns` beside the record, and
/// every rate and count in §3.5.2 is "measured over `window_ns`".
#[derive(Debug, Clone, Default)]
pub struct TelemetryWindow {
    start: SimTime,
    msgs_in: u32,
    msgs_out: u32,
    verifications: u32,
    delivered_unverified: u32,
    delivered_total: u32,
    full_cert_msgs: u32,
    airtime_ns: u64,
    verify_waits_ns: Vec<u64>,
}

impl TelemetryWindow {
    /// A window opening at `start`.
    pub fn new(start: SimTime) -> Self {
        TelemetryWindow {
            start,
            ..Default::default()
        }
    }

    /// When the window opened.
    pub fn start(&self) -> SimTime {
        self.start
    }

    /// The window's length up to `now`.
    pub fn length(&self, now: SimTime) -> Duration {
        Duration::between(self.start, now)
    }

    /// One message delivered to the stack.
    pub fn message_in(&mut self) {
        self.msgs_in = self.msgs_in.saturating_add(1);
    }

    /// One message transmitted, with the air time it occupied and whether it carried a
    /// full certificate.
    pub fn message_out(&mut self, airtime: Duration, full_certificate: bool) {
        self.msgs_out = self.msgs_out.saturating_add(1);
        self.airtime_ns = self.airtime_ns.saturating_add(airtime.as_nanos());
        if full_certificate {
            self.full_cert_msgs = self.full_cert_msgs.saturating_add(1);
        }
    }

    /// One completed signature verification, and how long it waited for a server.
    pub fn verification(&mut self, wait: Duration) {
        self.verifications = self.verifications.saturating_add(1);
        // Bounded for the same reason the queue's depth samples are: a node under
        // sustained overload must not grow a vector for the length of the run.
        const MAX: usize = 4096;
        if self.verify_waits_ns.len() < MAX {
            self.verify_waits_ns.push(wait.as_nanos());
        }
    }

    /// One message handed to the applications, verified or not.
    pub fn delivered(&mut self, verified: bool) {
        self.delivered_total = self.delivered_total.saturating_add(1);
        if !verified {
            self.delivered_unverified = self.delivered_unverified.saturating_add(1);
        }
    }

    /// Messages delivered to the stack in this window.
    pub fn msgs_in(&self) -> u32 {
        self.msgs_in
    }

    /// Messages transmitted in this window.
    pub fn msgs_out(&self) -> u32 {
        self.msgs_out
    }

    /// Verifications completed in this window.
    pub fn verifications(&self) -> u32 {
        self.verifications
    }

    /// The per-mille share of deliveries that carried no signature check.
    ///
    /// `0xFFFF` when nothing was delivered: a ratio of nothing is unknown, not zero, and
    /// §3.5.2's saturation rule reserves `0xFFFF` for exactly that.
    pub fn unverified_ratio_pm(&self) -> u16 {
        if self.delivered_total == 0 {
            return u16::MAX;
        }
        let r = f64::from(self.delivered_unverified) / f64::from(self.delivered_total);
        (v2xw_core::math::quantize_to(r * 1000.0, 1.0) as u16).min(1000)
    }

    /// The p50 and p95 of the enqueue-to-start wait, in milliseconds.
    pub fn verify_wait_ms(&self) -> (f32, f32) {
        if self.verify_waits_ns.is_empty() {
            return (f32::NAN, f32::NAN);
        }
        let mut ms: Vec<f64> = self
            .verify_waits_ns
            .iter()
            .map(|&ns| ns as f64 / 1e6)
            .collect();
        v2xw_core::math::sort_total_order(&mut ms);
        (
            v2xw_core::math::quantile_sorted(&ms, 0.50) as f32,
            v2xw_core::math::quantile_sorted(&ms, 0.95) as f32,
        )
    }

    /// Opens a new window at `now`.
    pub fn reset(&mut self, now: SimTime) {
        *self = TelemetryWindow::new(now);
    }

    /// A per-second rate from a window count.
    ///
    /// `NaN` for a zero-length window, which is what §3.5.2's "unknown/not-modelled" means
    /// for an `f32`: a rate over no time is not zero.
    pub fn rate(&self, count: u32, now: SimTime) -> f32 {
        let secs = self.length(now).as_secs_f64();
        if secs <= 0.0 {
            return f32::NAN;
        }
        (f64::from(count) / secs) as f32
    }

    /// Transmitted air time per second of window, milliseconds.
    pub fn airtime_ms_per_s(&self, now: SimTime) -> f32 {
        let secs = self.length(now).as_secs_f64();
        if secs <= 0.0 {
            return f32::NAN;
        }
        ((self.airtime_ns as f64 / 1e6) / secs) as f32
    }

    /// How many transmitted messages carried a full certificate.
    pub fn full_cert_msgs(&self) -> u32 {
        self.full_cert_msgs
    }
}

/// Everything outside the window counters that the record needs.
///
/// A plain struct rather than a builder because the mapping is the thing under test: a
/// reader comparing this against §3.5.2 should see fifty-six names once each.
#[derive(Debug, Clone)]
pub struct TelemetryInputs {
    /// Which node.
    pub node: NodeId,
    /// Bytes across the stores.
    pub storage_used_b: u64,
    /// The profile's flash capacity, or `u64::MAX` when it publishes none.
    pub storage_total_b: u64,
    /// When the next certificate top-up is due, or `u64::MAX` for none scheduled.
    pub next_topup_ns: u64,
    /// Bytes held in the CRL store.
    pub crl_bytes: u64,
    /// Bytes pending in the report outbox.
    pub outbox_bytes: u64,
    /// Believed minus true time. **Ground truth.**
    pub clock_offset_ns: i64,
    /// Stores plus queues plus the profile's baseline, KiB.
    pub ram_used_kib: u32,
    /// The profile's RAM, KiB, or `u32::MAX` when it publishes none.
    pub ram_total_kib: u32,
    /// Drop counts, in [`DropCause::ALL`] order.
    pub drops: [u32; 6],
    /// Own certificates held.
    pub cert_stored: u32,
    /// CRL entries held.
    pub crl_entries: u32,
    /// Reports pending.
    pub outbox_msgs: u32,
    /// Peer certificates cached.
    pub peer_cache_entries: u32,
    /// P2PCD requests issued this window.
    pub p2pcd_requests: u32,
    /// Horizontal 1-sigma from the GNSS model's own noise parameters.
    pub gnss_sigma_m: f32,
    /// Horizontal dilution of precision, or `NaN` when not modelled.
    pub gnss_hdop: f32,
    /// The oscillator's drift rate, ppm.
    pub clock_drift_ppm: f32,
    /// Distance from belief to truth, metres. **Ground truth.**
    pub pos_error_m: f32,
    /// CPU busy fraction, per mille.
    pub cpu_util_pm: u16,
    /// HSM busy fraction, per mille.
    pub hsm_util_pm: u16,
    /// `(p50, p95)` depth for each queue, in [`crate::queue::QueueKind::ALL`] order.
    pub queue_depths: [(u16, u16); 5],
    /// The DCC state code, or `0xFFFF` when DCC is not modelled.
    pub dcc_state: u16,
    /// Channel busy ratio, per mille, or `0xFFFF`.
    pub cbr_pm: u16,
    /// Transmit power, centi-dBm.
    pub tx_power_cdbm: i16,
    /// `(total, verified, unverified, revoked)` neighbour counts.
    pub neighbors: (usize, usize, usize, usize),
    /// Own certificates inside their validity window.
    pub cert_active: u16,
    /// Per-mille of the current i-period expansion done.
    pub crl_expansion_pm: u16,
    /// The GNSS fix quality code.
    pub gnss_fix: u8,
    /// What state the node is in.
    pub state: NodeState,
    /// Which verification policy is in force.
    pub verify_policy: u8,
}

impl TelemetryWindow {
    /// The §3.5.2 record for this window.
    ///
    /// Starts from the all-sentinel record so that a field this runtime does not model
    /// reaches the wire as "unknown" (conformance C1), and quantises every float to its
    /// declared grid, which `TelemetryBody::encode` does again at the writer (D9) —
    /// deliberately twice, because a caller reading the struct rather than the bytes
    /// should see the same numbers the bytes carry.
    pub fn record(&self, now: SimTime, i: &TelemetryInputs) -> NodeTelemetry {
        let mut r = NodeTelemetry::unknown(i.node.index());

        r.storage_used_b = i.storage_used_b;
        r.storage_total_b = i.storage_total_b;
        r.next_topup_ns = i.next_topup_ns;
        r.crl_bytes = i.crl_bytes;
        r.outbox_bytes = i.outbox_bytes;
        r.clock_offset_ns = i.clock_offset_ns;

        r.ram_used_kib = i.ram_used_kib;
        r.ram_total_kib = i.ram_total_kib;
        r.drop_rx_overflow = i.drops[idx(DropCause::RxOverflow)];
        r.drop_verify_policy_skip = i.drops[idx(DropCause::VerifyPolicySkip)];
        r.drop_verify_overflow = i.drops[idx(DropCause::VerifyOverflow)];
        r.drop_tx_overflow = i.drops[idx(DropCause::TxOverflow)];
        r.drop_reassembly_timeout = i.drops[idx(DropCause::ReassemblyTimeout)];
        r.drop_crl_backlog = i.drops[idx(DropCause::CrlBacklog)];
        r.cert_stored = i.cert_stored;
        r.crl_entries = i.crl_entries;
        r.outbox_msgs = i.outbox_msgs;
        r.peer_cache_entries = i.peer_cache_entries;
        r.p2pcd_requests = i.p2pcd_requests;
        r.full_cert_msgs = self.full_cert_msgs;

        r.msgs_in_per_s = self.rate(self.msgs_in, now);
        r.msgs_out_per_s = self.rate(self.msgs_out, now);
        r.verifications_per_s = self.rate(self.verifications, now);
        let (p50, p95) = self.verify_wait_ms();
        r.verify_wait_p50_ms = p50;
        r.verify_wait_p95_ms = p95;
        r.gnss_hdop = i.gnss_hdop;
        r.gnss_sigma_m = i.gnss_sigma_m;
        r.clock_drift_ppm = i.clock_drift_ppm;
        r.pos_error_m = i.pos_error_m;
        r.airtime_ms_per_s = self.airtime_ms_per_s(now);

        r.cpu_util_pm = i.cpu_util_pm;
        r.hsm_util_pm = i.hsm_util_pm;
        r.q_rx_p50 = i.queue_depths[0].0;
        r.q_rx_p95 = i.queue_depths[0].1;
        r.q_verify_p50 = i.queue_depths[1].0;
        r.q_verify_p95 = i.queue_depths[1].1;
        r.q_app_p50 = i.queue_depths[2].0;
        r.q_app_p95 = i.queue_depths[2].1;
        r.q_tx_p50 = i.queue_depths[3].0;
        r.q_tx_p95 = i.queue_depths[3].1;
        r.q_crl_p50 = i.queue_depths[4].0;
        r.q_crl_p95 = i.queue_depths[4].1;
        r.dcc_state = i.dcc_state;
        r.cbr_pm = i.cbr_pm;
        r.tx_power_cdbm = i.tx_power_cdbm;
        let (total, verified, unverified, revoked) = i.neighbors;
        r.nbr_total = sat(total);
        r.nbr_verified = sat(verified);
        r.nbr_unverified = sat(unverified);
        r.nbr_revoked = sat(revoked);
        r.cert_active = i.cert_active;
        r.crl_expansion_pm = i.crl_expansion_pm;
        r.unverified_ratio_pm = self.unverified_ratio_pm();

        r.gnss_fix = i.gnss_fix;
        r.node_state = i.state.code();
        r.verify_policy = i.verify_policy;

        r.quantised()
    }
}

fn idx(c: DropCause) -> usize {
    DropCause::ALL.iter().position(|x| *x == c).unwrap_or(0)
}

/// Saturates a count into a `u16`, which §3.5.2 reads as ">= 65535".
fn sat(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// The GNSS fix codes of §3.5.2 offset 192.
///
/// The wire enumerates seven qualities and [`v2xw_core::belief::FixQuality`] six: the
/// wire separates an RTK float solution (`4`) from a fixed one (`5`) and the engine does
/// not, so code `4` is never emitted. That is a modelling gap, not an encoding one — the
/// GNSS model of 04-models.md §3.8 has no float/fixed ambiguity state — and it is
/// recorded here rather than papered over by mapping two engine states onto it.
pub fn gnss_fix_code(fix: v2xw_core::belief::FixQuality) -> u8 {
    use v2xw_core::belief::FixQuality;
    match fix {
        FixQuality::NoFix => 0,
        FixQuality::TwoD => 1,
        FixQuality::ThreeD => 2,
        FixQuality::Differential => 3,
        FixQuality::Rtk => 5,
        FixQuality::DeadReckoning => 6,
        // `FixQuality` is `#[non_exhaustive]`: a quality added later is reported as
        // "unknown at this tier" rather than silently taking another quality's code.
        _ => v2xw_record::wire::U8_NONE,
    }
}

/// The DCC state codes of §3.5.2 offset 172.
pub fn dcc_state_code(state: v2xw_radio::types::ReactiveState) -> u16 {
    use v2xw_radio::types::ReactiveState;
    match state {
        ReactiveState::Relaxed => 0,
        ReactiveState::Active1 => 1,
        ReactiveState::Active2 => 2,
        ReactiveState::Active3 => 3,
        // `ReactiveState` is exhaustive at five states, matching the five TS 102 687
        // bands, so there is no unknown arm here; `gnss_fix_code` needs one because
        // `FixQuality` is `#[non_exhaustive]`.
        ReactiveState::Restrictive => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::time::NS_PER_S;

    /// Rates are per second of window, not per step.
    #[test]
    fn rates_are_per_second_of_window() {
        let mut w = TelemetryWindow::new(0);
        for _ in 0..25 {
            w.message_in();
        }
        assert_eq!(w.rate(w.msgs_in(), 500_000_000), 50.0);
        assert_eq!(w.rate(w.msgs_in(), NS_PER_S), 25.0);
        assert!(
            w.rate(w.msgs_in(), 0).is_nan(),
            "a rate over no time is unknown"
        );
    }

    /// A ratio of nothing is unknown, not zero — the distinction a HUD needs to draw a
    /// blank rather than a reassuring bar.
    #[test]
    fn an_unverified_ratio_of_nothing_is_the_unknown_sentinel() {
        let mut w = TelemetryWindow::new(0);
        assert_eq!(w.unverified_ratio_pm(), u16::MAX);
        w.delivered(true);
        assert_eq!(w.unverified_ratio_pm(), 0);
        for _ in 0..3 {
            w.delivered(false);
        }
        assert_eq!(w.unverified_ratio_pm(), 750);
    }

    /// Verification waits are reported as a distribution, so a node whose median is fine
    /// but whose tail is not is visible as exactly that.
    #[test]
    fn verify_waits_report_a_distribution() {
        let mut w = TelemetryWindow::new(0);
        for _ in 0..95 {
            w.verification(Duration::from_millis(1));
        }
        for _ in 0..5 {
            w.verification(Duration::from_millis(50));
        }
        let (p50, p95) = w.verify_wait_ms();
        assert_eq!(p50, 1.0);
        assert!(p95 > 1.0, "the tail is visible: p95 was {p95}");
    }

    /// An empty window's wait percentiles are `NaN`, which is §3.5.2's unknown for an
    /// `f32`, rather than a zero that would read as "no queueing".
    #[test]
    fn an_empty_window_reports_unknown_waits() {
        let w = TelemetryWindow::new(0);
        let (p50, p95) = w.verify_wait_ms();
        assert!(p50.is_nan() && p95.is_nan());
    }

    /// The state codes are §3.5.2's, and `compromised` is the one that is ground truth.
    #[test]
    fn node_state_codes_match_the_specification() {
        assert_eq!(
            [
                NodeState::Off.code(),
                NodeState::Booting.code(),
                NodeState::Active.code(),
                NodeState::Parked.code(),
                NodeState::Degraded.code(),
                NodeState::Down.code(),
                NodeState::Compromised.code(),
            ],
            [0, 1, 2, 3, 4, 5, 6]
        );
        assert!(!NodeState::Parked.transmits());
        assert!(!NodeState::Down.transmits());
        assert!(NodeState::Compromised.transmits());
    }

    /// The fix and DCC enumerations map onto the codes the wire carries.
    #[test]
    fn fix_and_dcc_codes_match_the_specification() {
        use v2xw_core::belief::FixQuality;
        use v2xw_radio::types::ReactiveState;
        // `FixQuality::ALL` is worst-first, so the codes are not in ascending order; the
        // gap at 4 is the RTK-float state the engine does not model.
        assert_eq!(FixQuality::ALL.map(gnss_fix_code), [0, 6, 1, 2, 3, 5]);
        assert!(
            !FixQuality::ALL.map(gnss_fix_code).contains(&4),
            "code 4 (RTK float) has no engine state behind it"
        );
        assert_eq!(
            [
                dcc_state_code(ReactiveState::Relaxed),
                dcc_state_code(ReactiveState::Active1),
                dcc_state_code(ReactiveState::Active2),
                dcc_state_code(ReactiveState::Active3),
                dcc_state_code(ReactiveState::Restrictive),
            ],
            [0, 1, 2, 3, 4]
        );
    }
}
