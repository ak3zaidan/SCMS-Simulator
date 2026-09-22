//! The CAMP SCMS deployment's nodes and its tunable numbers.
//!
//! Every field of [`ScmsParams`] appears on the plug-in's model card. The ones with a
//! standard behind them cite the clause; the ones without carry a calibration plan, and
//! `tests/model_card.rs` checks that no parameter is in the second group without one.

use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};
use v2xw_sec::primitive::profiles;

use crate::scms::msg::LaIndex;
use crate::service::{BatchPolicy, ServiceModelSpec};
use crate::sizes::SizeParams;

/// Which node hosts which role.
///
/// Fixed ids rather than a map, because the topology of the reference deployment is fixed
/// and a scenario that wants another one builds the links itself. Root CA, the Policy
/// Generator and the electors are offline in the CAMP PoC (05-protocols §3.1) and have no
/// node at all — modelling them as nodes would put messages on the wire that the PoC never
/// sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScmsNodes {
    /// Registration Authority.
    pub ra: NodeId,
    /// Pseudonym Certificate Authority (the ACA of 1609.2.1).
    pub pca: NodeId,
    /// Linkage Authority 1.
    pub la1: NodeId,
    /// Linkage Authority 2.
    pub la2: NodeId,
    /// Misbehaviour Authority.
    pub ma: NodeId,
    /// CRL Generator.
    pub crlg: NodeId,
    /// Location Obscurer Proxy.
    pub lop: NodeId,
    /// CRL Store.
    pub crl_store: NodeId,
    /// The CRL broadcast path (roadside or satellite).
    pub crl_broadcast: NodeId,
    /// Enrolment Certificate Authority.
    pub eca: NodeId,
    /// Device Configuration Manager.
    pub dcm: NodeId,
}

impl Default for ScmsNodes {
    fn default() -> ScmsNodes {
        ScmsNodes {
            ra: NodeId::new(1),
            pca: NodeId::new(2),
            la1: NodeId::new(3),
            la2: NodeId::new(4),
            ma: NodeId::new(5),
            crlg: NodeId::new(6),
            lop: NodeId::new(7),
            crl_store: NodeId::new(8),
            crl_broadcast: NodeId::new(9),
            eca: NodeId::new(10),
            dcm: NodeId::new(11),
        }
    }
}

impl ScmsNodes {
    /// The node hosting one of the two Linkage Authorities.
    pub const fn la(&self, which: LaIndex) -> NodeId {
        match which {
            LaIndex::One => self.la1,
            LaIndex::Two => self.la2,
        }
    }

    /// Every backend node, in a fixed order.
    pub const fn all(&self) -> [NodeId; 11] {
        [
            self.ra,
            self.pca,
            self.la1,
            self.la2,
            self.ma,
            self.crlg,
            self.lop,
            self.crl_store,
            self.crl_broadcast,
            self.eca,
            self.dcm,
        ]
    }

    /// The links the reference deployment needs, as unordered pairs.
    pub fn backend_links(&self) -> Vec<(NodeId, NodeId)> {
        vec![
            (self.lop, self.ra),
            (self.ra, self.la1),
            (self.ra, self.la2),
            (self.ra, self.pca),
            (self.ra, self.ma),
            (self.ma, self.pca),
            (self.ma, self.la1),
            (self.ma, self.la2),
            (self.ma, self.crlg),
            (self.crlg, self.crl_store),
            (self.crlg, self.crl_broadcast),
            (self.dcm, self.eca),
            (self.eca, self.ra),
        ]
    }

    /// The service model each backend node runs.
    pub fn backend_service_models(&self, p: &ScmsParams) -> Vec<(NodeId, ServiceModelSpec)> {
        let plain = ServiceModelSpec::new(p.backend_servers, p.backend_overhead);
        let shuffling = plain.batched(BatchPolicy::CAMP_SHUFFLE);
        vec![
            (self.ra, shuffling),
            (self.pca, plain),
            (self.la1, plain),
            (self.la2, plain),
            (self.ma, plain),
            (self.crlg, plain),
            (self.lop, plain),
            (self.crl_store, plain),
            (self.crl_broadcast, plain),
            (self.eca, plain),
            (self.dcm, plain),
        ]
    }
}

/// The CAMP SCMS plug-in's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScmsParams {
    /// The master seed of the deterministic streams.
    pub master_seed: u64,
    /// The [`SimTime`] at which i-period 0 begins.
    ///
    /// A certificate's validity window is `[epoch + i·i_period, + cert_lifetime)`, so this
    /// is what aligns the protocol's week numbering with the scenario's `time.t0`. It is a
    /// supplied instant, never a clock read.
    pub epoch: SimTime,
    /// The i-period: 10,080 minutes [CAMP-EE §2.1.5.3.2].
    pub i_period: Duration,
    /// Certificate lifetime: 10,140 minutes, one hour of overlap [CAMP-EE §2.1.5.3.2].
    pub cert_lifetime: Duration,
    /// Certificates per i-period: 20 [CAMP-EE Table 2.1.2.6.2].
    pub certs_per_period: u32,
    /// The initial batch: 3,120 certificates, three years [PRIMER p.7].
    pub initial_batch: u32,
    /// The RA's certificate-request shuffle window [CAMP-EE §2.2.7].
    pub shuffle_window: Duration,
    /// The RA's report shuffle window [CAMP-EE SCMS-765].
    pub report_shuffle_window: Duration,
    /// CRL cadence: daily, the USDOT 2013 working assumption [BRECHT §VI-G].
    pub crl_cadence: Duration,
    /// How long after the acknowledgement the device first polls for a batch.
    pub first_batch_delay: Duration,
    /// How long between polls of a repository that is not ready.
    pub download_poll_interval: Duration,
    /// How many polls before the device gives up.
    pub max_download_polls: u32,
    /// Backend servers per entity, the `c` of the M/M/c.
    pub backend_servers: u32,
    /// Per-request overhead at a backend entity, beyond the cryptography.
    pub backend_overhead: Duration,
    /// Per-request overhead at a device.
    pub device_overhead: Duration,
    /// One-way latency on a backend link.
    pub backend_link_latency: Duration,
    /// Backend link bandwidth.
    pub backend_link_bandwidth_bps: u64,
    /// One-way latency on the cellular uplink.
    pub uu_link_latency: Duration,
    /// Cellular link bandwidth.
    pub uu_link_bandwidth_bps: u64,
    /// One-way latency on the 5.9 GHz air interface between a roadside unit and a vehicle.
    ///
    /// Not propagation — that is under a microsecond at V2X ranges — but the channel access
    /// a broadcast frame waits for before its first bit goes out.
    pub v2x_air_latency: Duration,
    /// The 5.9 GHz air interface's data rate, bits per second.
    ///
    /// 6 Mbit/s is the 10 MHz OFDM PHY's QPSK rate-1/2 mode, which is what the Phase 1
    /// scenario's medium PHY transmits at [EN 302 663 V1.3.1 Annex C.3 Table C.1].
    pub v2x_air_bandwidth_bps: u64,
    /// The hardware profile backend costs are read from.
    pub backend_profile: &'static str,
    /// The hardware profile device costs are read from.
    pub device_profile: &'static str,
    /// The five wire sizes no standard publishes.
    pub sizes: SizeParams,
}

impl Default for ScmsParams {
    fn default() -> ScmsParams {
        ScmsParams {
            master_seed: 0,
            epoch: 0,
            i_period: Duration::from_secs(10_080 * 60),
            cert_lifetime: Duration::from_secs(10_140 * 60),
            certs_per_period: 20,
            initial_batch: 3_120,
            shuffle_window: BatchPolicy::CAMP_SHUFFLE.max_delay,
            report_shuffle_window: BatchPolicy::CAMP_SHUFFLE.max_delay,
            crl_cadence: Duration::from_secs(86_400),
            first_batch_delay: Duration::from_secs(60),
            download_poll_interval: Duration::from_secs(60),
            max_download_polls: 64,
            backend_servers: 4,
            backend_overhead: Duration::from_millis(1),
            device_overhead: Duration::from_millis(1),
            backend_link_latency: Duration::from_millis(10),
            backend_link_bandwidth_bps: 1_000_000_000,
            uu_link_latency: Duration::from_millis(50),
            uu_link_bandwidth_bps: 10_000_000,
            v2x_air_latency: Duration::from_millis(1),
            v2x_air_bandwidth_bps: 6_000_000,
            backend_profile: profiles::I9_11950H_WOLFSSL,
            device_profile: profiles::COHDA_MK6_BOTAN,
            sizes: SizeParams::default(),
        }
    }
}

impl ScmsParams {
    /// When i-period `i` begins.
    #[must_use]
    pub const fn period_start(&self, i: u32) -> SimTime {
        self.i_period.saturating_mul(i as u64).after(self.epoch)
    }

    /// The validity window of a certificate issued for i-period `i`.
    ///
    /// `[start(i), start(i) + cert_lifetime)`. The lifetime is an hour longer than the
    /// period, so consecutive windows overlap by an hour — which is the whole reason
    /// CAMP-EE §2.1.5.3.2 states the two numbers separately, and the reason a device that
    /// rotates at a period boundary always has something valid to rotate to.
    #[must_use]
    pub const fn validity(&self, i: u32) -> (SimTime, SimTime) {
        let from = self.period_start(i);
        (from, self.cert_lifetime.after(from))
    }

    /// A deployment whose batching windows are short enough for a test to run through.
    ///
    /// The CAMP shuffle is "10,000 requests or one day"; a test that waits a day of
    /// simulated time measures nothing extra, so this variant sets the window to a minute
    /// and leaves every cited number alone. It exists so that no test is tempted to change
    /// a *cited* default to make itself convenient.
    #[must_use]
    pub fn quick(mut self) -> ScmsParams {
        self.shuffle_window = Duration::from_secs(60);
        self.report_shuffle_window = Duration::from_secs(60);
        self
    }
}
