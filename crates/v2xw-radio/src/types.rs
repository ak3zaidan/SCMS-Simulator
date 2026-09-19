//! The value types of the radio seam: 03-interfaces.md §4's structs and enums, plus the
//! cited constant tables of 04-models.md §4.1-§4.6 they are built on.
//!
//! Nothing here computes anything a model could disagree about. The MCS table, the EDCA
//! table, the timing constants and the frame overheads are the standards' own numbers,
//! each carrying its clause in the doc comment, so a reader can check the code against
//! ETSI EN 302 663 without opening a model card.

use serde::{Deserialize, Serialize};
use v2xw_core::geom::{Dims, Vec3};
use v2xw_core::ids::{ActorId, BuildingId, FrameSeq, NodeId, SduId};
use v2xw_core::math;
use v2xw_core::time::{Duration, SimTime};

use crate::numeric;

// =========================================================================================
// Radio access technology and channels
// =========================================================================================

/// The radio access technology a PHY and MAC pair implements.
///
/// 03-interfaces.md §4: `Dsrc80211p | LteV2xPc5 | NrV2xPc5`. This crate implements the
/// first; the two sidelink RATs of 04-models.md §5 are named here so the seam and the
/// records do not have to change when they land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rat {
    /// IEEE 802.11p / ETSI ITS-G5, 10 MHz OFDM (04-models.md §4).
    Dsrc80211p,
    /// LTE-V2X Mode 4 sidelink (04-models.md §5.1).
    LteV2xPc5,
    /// NR-V2X Mode 2 sidelink (04-models.md §5.2).
    NrV2xPc5,
}

/// A 10 MHz channel, named by its IEEE channel number.
///
/// The number is the identity: channel 180 is 5,900 MHz centre in the European plan
/// (C2C-CC RS 2037 RS_BSP_545) and 5.895-5.905 GHz in the US plan after the 2020
/// reallocation (FCC 2020 First R&O ¶149). [`ChannelId::centre_hz`] carries the European
/// mapping, which is the one the ITS-G5 models default to (04-models.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChannelId(pub u16);

impl ChannelId {
    /// The ITS-G5A control channel, 5,900 MHz — the default single-channel operation
    /// 04-models.md §4.1 selects (`channel_switching = none`).
    pub const CCH: ChannelId = ChannelId(180);
    /// ITS-G5A service channel SCH1, 5,880 MHz.
    pub const SCH1: ChannelId = ChannelId(176);
    /// ITS-G5A service channel SCH2, 5,890 MHz.
    pub const SCH2: ChannelId = ChannelId(178);

    /// The channel's centre frequency in Hz, per the IEEE channel numbering used by the
    /// European plan (C2C-CC RS 2037 RS_BSP_545, 04-models.md §4.1): channel 172 is
    /// 5,860 MHz and every step of two is 10 MHz.
    #[must_use]
    pub const fn centre_hz(self) -> f64 {
        // 5,000 MHz + 5 MHz per channel number is the IEEE 5 GHz numbering rule, which
        // reproduces every row of the RS 2037 table: 172 -> 5,860, 180 -> 5,900,
        // 184 -> 5,920 MHz.
        (5_000 + 5 * self.0 as u64) as f64 * 1e6
    }

    /// The channel width in Hz. Every ITS-G5 channel in 04-models.md §4.1 is 10 MHz.
    #[must_use]
    pub const fn width_hz(self) -> f64 {
        10e6
    }
}

impl core::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ch{}", self.0)
    }
}

// =========================================================================================
// OFDM timing and frame overheads (04-models.md §4.2, §4.4, §4.6)
// =========================================================================================

/// The 10 MHz OFDM PHY and MAC constants of 04-models.md §4.2, §4.4 and §4.6.
///
/// Every value carries its clause. The two marked UNVERIFIED are so marked in the design
/// document as well, and the model cards repeat the tag: they are the constants that come
/// from the cached NIST reproduction and from the universally repeated 802.11 frame
/// format rather than from primary IEEE text.
pub mod timing {
    use v2xw_core::time::Duration;

    /// `aSlotTime`, 13 µs [EN 302 663 V1.3.1 Annex C.4.4].
    pub const SLOT_TIME: Duration = Duration::from_micros(13);
    /// `aSIFSTime`, 32 µs [EN 302 663 V1.3.1 Annex C.4.4].
    pub const SIFS: Duration = Duration::from_micros(32);
    /// PLCP preamble (short + long training sequence), 32 µs [EN 302 663 Table C.2].
    pub const PREAMBLE: Duration = Duration::from_micros(32);
    /// SIGNAL field, 8 µs — 24 bits, always BPSK 1/2 [EN 302 663 Annex C.3].
    pub const SIGNAL: Duration = Duration::from_micros(8);
    /// One OFDM symbol, 8 µs (half-clocked) [EN 302 663 Annex C.3].
    pub const SYMBOL: Duration = Duration::from_micros(8);
    /// Bits in the SIGNAL field [EN 302 663 Annex C.3].
    pub const SIGNAL_BITS: u32 = 24;
    /// `N_SERVICE`: the 16 SERVICE bits prepended to the PSDU.
    ///
    /// UNVERIFIED at the clause level: the value is the one the cached NIST PER
    /// reproduction, Veins and ns-3 use; IEEE 802.11-2016 clause 17.3.2 is not in the
    /// cache (04-models.md §4.2).
    pub const N_SERVICE: u32 = 16;
    /// `N_TAIL`: the 6 tail bits appended to the PSDU. UNVERIFIED at the clause level,
    /// exactly as [`N_SERVICE`] is.
    pub const N_TAIL: u32 = 6;
    /// `aCWmin` for the 10 MHz OFDM PHY [EN 302 663 Annex C.4.4, Table C.6].
    pub const A_CW_MIN: u32 = 15;
    /// `aCWmax` for the 10 MHz OFDM PHY [EN 302 663 Annex C.4.4, Table C.6].
    pub const A_CW_MAX: u32 = 1023;
    /// The CBR measurement window `T_CBR`, 100 ms [EN 302 571 §4.2.10.1; TS 102 687
    /// Table 3].
    pub const T_CBR: Duration = Duration::from_millis(100);
    /// The DCC evaluation period, 200 ms [TS 102 687 §5.2, §5.4].
    pub const T_DCC: Duration = Duration::from_millis(200);
    /// Maximum MSDU, 2,304 bytes. The figure is a well-established constant; its clause
    /// is UNVERIFIED (04-models.md §4.6).
    pub const MAX_MSDU_BYTES: u32 = 2_304;
    /// MAC header for a Data frame without QoS, 24 bytes (clause UNVERIFIED).
    pub const MAC_HEADER_BYTES: u32 = 24;
    /// MAC header for a QoS Data frame — what a BSM or CAM travels in, 26 bytes
    /// (clause UNVERIFIED).
    pub const MAC_HEADER_QOS_BYTES: u32 = 26;
    /// Frame check sequence, 4 bytes.
    pub const FCS_BYTES: u32 = 4;
    /// LLC/SNAP header over ITS-G5, 8 bytes (DERIVED from 802.2/SNAP; EN 302 663
    /// §4.3.1 Figure 3 shows "IEEE/ISO/IEC 8802-2 with SNAP").
    pub const LLC_SNAP_BYTES: u32 = 8;
}

// =========================================================================================
// MCS
// =========================================================================================

/// The modulation of one MCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Modulation {
    /// Binary phase-shift keying, 1 bit per subcarrier.
    Bpsk,
    /// Quadrature phase-shift keying, 2 bits per subcarrier.
    Qpsk,
    /// 16-point quadrature amplitude modulation, 4 bits per subcarrier.
    Qam16,
    /// 64-point quadrature amplitude modulation, 6 bits per subcarrier.
    Qam64,
}

/// The convolutional code rate of one MCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeRate {
    /// Rate 1/2, the mother code.
    R1_2,
    /// Rate 2/3, punctured.
    R2_3,
    /// Rate 3/4, punctured.
    R3_4,
}

impl CodeRate {
    /// The `bValue` the NIST coded-error union bound is indexed by (04-models.md §4.7,
    /// Pei and Henderson Table I): 1 for rate 1/2, 2 for 2/3, 3 for 3/4.
    #[must_use]
    pub const fn b_value(self) -> u32 {
        match self {
            CodeRate::R1_2 => 1,
            CodeRate::R2_3 => 2,
            CodeRate::R3_4 => 3,
        }
    }

    /// The rate as a fraction, for reporting.
    #[must_use]
    pub const fn as_fraction(self) -> (u32, u32) {
        match self {
            CodeRate::R1_2 => (1, 2),
            CodeRate::R2_3 => (2, 3),
            CodeRate::R3_4 => (3, 4),
        }
    }
}

/// One of the eight 10 MHz 802.11p modulation and coding schemes.
///
/// The variants are named by data rate, which is how every standard, simulator and paper
/// in 04-models.md §4 names them. The whole table — rate, modulation, coding, data and
/// coded bits per symbol, static and dynamic sensitivity — is
/// [EN 302 663 V1.3.1 Table C.1, Table 1 and Table 2].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mcs {
    /// 3 Mbit/s, BPSK 1/2. Mandatory; the Torrent-Moreno capture reference.
    R3Bpsk12,
    /// 4.5 Mbit/s, BPSK 3/4.
    R4p5Bpsk34,
    /// 6 Mbit/s, QPSK 1/2. Mandatory; the default safety-channel rate.
    R6Qpsk12,
    /// 9 Mbit/s, QPSK 3/4.
    R9Qpsk34,
    /// 12 Mbit/s, 16-QAM 1/2. Mandatory.
    R12Qam16_12,
    /// 18 Mbit/s, 16-QAM 3/4.
    R18Qam16_34,
    /// 24 Mbit/s, 64-QAM 2/3.
    R24Qam64_23,
    /// 27 Mbit/s, 64-QAM 3/4.
    R27Qam64_34,
}

impl Mcs {
    /// Every MCS, in rate order. The order is part of the contract: reports, tables and
    /// the calibration sweep iterate it.
    pub const ALL: [Mcs; 8] = [
        Mcs::R3Bpsk12,
        Mcs::R4p5Bpsk34,
        Mcs::R6Qpsk12,
        Mcs::R9Qpsk34,
        Mcs::R12Qam16_12,
        Mcs::R18Qam16_34,
        Mcs::R24Qam64_23,
        Mcs::R27Qam64_34,
    ];

    /// The three mandatory rates [EN 302 663 V1.3.1 §4.2].
    pub const MANDATORY: [Mcs; 3] = [Mcs::R3Bpsk12, Mcs::R6Qpsk12, Mcs::R12Qam16_12];

    /// The data rate in Mbit/s [EN 302 663 Table C.1].
    #[must_use]
    pub const fn rate_mbps(self) -> f64 {
        match self {
            Mcs::R3Bpsk12 => 3.0,
            Mcs::R4p5Bpsk34 => 4.5,
            Mcs::R6Qpsk12 => 6.0,
            Mcs::R9Qpsk34 => 9.0,
            Mcs::R12Qam16_12 => 12.0,
            Mcs::R18Qam16_34 => 18.0,
            Mcs::R24Qam64_23 => 24.0,
            Mcs::R27Qam64_34 => 27.0,
        }
    }

    /// The modulation [EN 302 663 Table C.1].
    #[must_use]
    pub const fn modulation(self) -> Modulation {
        match self {
            Mcs::R3Bpsk12 | Mcs::R4p5Bpsk34 => Modulation::Bpsk,
            Mcs::R6Qpsk12 | Mcs::R9Qpsk34 => Modulation::Qpsk,
            Mcs::R12Qam16_12 | Mcs::R18Qam16_34 => Modulation::Qam16,
            Mcs::R24Qam64_23 | Mcs::R27Qam64_34 => Modulation::Qam64,
        }
    }

    /// The coding rate [EN 302 663 Table C.1].
    #[must_use]
    pub const fn code_rate(self) -> CodeRate {
        match self {
            Mcs::R3Bpsk12 | Mcs::R6Qpsk12 | Mcs::R12Qam16_12 => CodeRate::R1_2,
            Mcs::R24Qam64_23 => CodeRate::R2_3,
            Mcs::R4p5Bpsk34 | Mcs::R9Qpsk34 | Mcs::R18Qam16_34 | Mcs::R27Qam64_34 => CodeRate::R3_4,
        }
    }

    /// `N_DBPS`, data bits per OFDM symbol [EN 302 663 Table C.1].
    #[must_use]
    pub const fn data_bits_per_symbol(self) -> u32 {
        match self {
            Mcs::R3Bpsk12 => 24,
            Mcs::R4p5Bpsk34 => 36,
            Mcs::R6Qpsk12 => 48,
            Mcs::R9Qpsk34 => 72,
            Mcs::R12Qam16_12 => 96,
            Mcs::R18Qam16_34 => 144,
            Mcs::R24Qam64_23 => 192,
            Mcs::R27Qam64_34 => 216,
        }
    }

    /// `N_CBPS`, coded bits per OFDM symbol [EN 302 663 Table C.1].
    #[must_use]
    pub const fn coded_bits_per_symbol(self) -> u32 {
        match self {
            Mcs::R3Bpsk12 | Mcs::R4p5Bpsk34 => 48,
            Mcs::R6Qpsk12 | Mcs::R9Qpsk34 => 96,
            Mcs::R12Qam16_12 | Mcs::R18Qam16_34 => 192,
            Mcs::R24Qam64_23 | Mcs::R27Qam64_34 => 288,
        }
    }

    /// Minimum static receiver sensitivity, dBm [EN 302 663 V1.3.1 Table 1].
    #[must_use]
    pub const fn sensitivity_static_dbm(self) -> f64 {
        match self {
            Mcs::R3Bpsk12 => -91.0,
            Mcs::R4p5Bpsk34 => -90.0,
            Mcs::R6Qpsk12 => -88.0,
            Mcs::R9Qpsk34 => -86.0,
            Mcs::R12Qam16_12 => -83.0,
            Mcs::R18Qam16_34 => -79.0,
            Mcs::R24Qam64_23 => -75.0,
            Mcs::R27Qam64_34 => -74.0,
        }
    }

    /// Minimum dynamic (interference-present) sensitivity, dBm.
    ///
    /// EN 302 663 Table 2 states one row, 6 Mbit/s at −85 dBm; 04-models.md §3.7 records
    /// that the dynamic figures are 3 dB above the static ones, which reproduces that
    /// row exactly, so the same offset is applied to every MCS.
    #[must_use]
    pub const fn sensitivity_dynamic_dbm(self) -> f64 {
        self.sensitivity_static_dbm() + 3.0
    }

    /// The commercial cross-check row: Cohda MK5, 10 MHz, no multipath, one antenna
    /// [Cohda MK5 module datasheet Table 2, 04-models.md §3.7].
    #[must_use]
    pub const fn sensitivity_cohda_mk5_dbm(self) -> f64 {
        match self {
            Mcs::R3Bpsk12 => -98.0,
            Mcs::R4p5Bpsk34 => -96.0,
            Mcs::R6Qpsk12 => -95.0,
            Mcs::R9Qpsk34 => -93.0,
            Mcs::R12Qam16_12 => -90.0,
            Mcs::R18Qam16_34 => -86.0,
            Mcs::R24Qam64_23 => -82.0,
            Mcs::R27Qam64_34 => -80.0,
        }
    }

    /// The id this MCS is spelled with in a scenario, a record and a report.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Mcs::R3Bpsk12 => "3-bpsk-1/2",
            Mcs::R4p5Bpsk34 => "4.5-bpsk-3/4",
            Mcs::R6Qpsk12 => "6-qpsk-1/2",
            Mcs::R9Qpsk34 => "9-qpsk-3/4",
            Mcs::R12Qam16_12 => "12-16qam-1/2",
            Mcs::R18Qam16_34 => "18-16qam-3/4",
            Mcs::R24Qam64_23 => "24-64qam-2/3",
            Mcs::R27Qam64_34 => "27-64qam-3/4",
        }
    }
}

impl core::fmt::Display for Mcs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

// =========================================================================================
// EDCA
// =========================================================================================

/// An EDCA access category.
///
/// The parameter table is [EN 302 663 V1.3.1 Annex C.4.4, Tables C.4-C.6, citing
/// IEEE 802.11-2016 Table 9-138], and the UP mapping is [EN 302 663 Table C.3].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessCategory {
    /// Background.
    Bk,
    /// Best effort.
    Be,
    /// Video.
    Vi,
    /// Voice — the category a CAM or BSM uses.
    Vo,
}

impl AccessCategory {
    /// Every category, from the one that defers longest to the one that defers least.
    /// Internal collisions resolve to the *higher* category, which is the later entry.
    pub const ALL: [AccessCategory; 4] = [
        AccessCategory::Bk,
        AccessCategory::Be,
        AccessCategory::Vi,
        AccessCategory::Vo,
    ];

    /// `CWmin` [EN 302 663 Table C.5].
    #[must_use]
    pub const fn cw_min(self) -> u32 {
        match self {
            AccessCategory::Vo => 3,
            AccessCategory::Vi => 7,
            AccessCategory::Be | AccessCategory::Bk => timing::A_CW_MIN,
        }
    }

    /// `CWmax` [EN 302 663 Table C.5].
    #[must_use]
    pub const fn cw_max(self) -> u32 {
        match self {
            AccessCategory::Vo => 7,
            AccessCategory::Vi => 15,
            AccessCategory::Be | AccessCategory::Bk => timing::A_CW_MAX,
        }
    }

    /// `AIFSN`, in slots [EN 302 663 Table C.5].
    #[must_use]
    pub const fn aifsn(self) -> u32 {
        match self {
            AccessCategory::Vo => 2,
            AccessCategory::Vi => 3,
            AccessCategory::Be => 6,
            AccessCategory::Bk => 9,
        }
    }

    /// `AIFS[AC] = AIFSN[AC] × aSlotTime + aSIFSTime` [EN 302 663 Annex C.4.4].
    ///
    /// Reproduces the table's own µs column exactly: 58, 71, 110 and 149 µs.
    #[must_use]
    pub const fn aifs(self) -> Duration {
        Duration::from_nanos(
            timing::SLOT_TIME.as_nanos() * self.aifsn() as u64 + timing::SIFS.as_nanos(),
        )
    }

    /// The category a user priority maps to [EN 302 663 Table C.3]: UP 1, 2 → AC_BK;
    /// UP 0, 3 → AC_BE; UP 4, 5 → AC_VI; UP 6, 7 → AC_VO.
    #[must_use]
    pub const fn from_user_priority(up: u8) -> AccessCategory {
        match up {
            1 | 2 => AccessCategory::Bk,
            4 | 5 => AccessCategory::Vi,
            6 | 7 => AccessCategory::Vo,
            // UP 0 and 3 per the table; anything out of range is best effort, which is
            // what an unmarked frame gets.
            _ => AccessCategory::Be,
        }
    }

    /// A short label for records and reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            AccessCategory::Bk => "AC_BK",
            AccessCategory::Be => "AC_BE",
            AccessCategory::Vi => "AC_VI",
            AccessCategory::Vo => "AC_VO",
        }
    }
}

// =========================================================================================
// Frames
// =========================================================================================

/// Whether a frame is group-addressed, which decides whether it can be acknowledged,
/// retransmitted or fragmented at all (04-models.md §4.3, §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrameKind {
    /// Group-addressed: a CAM, a BSM, a DENM. No ACK, so no retransmission, no CW
    /// doubling and no MAC fragmentation.
    Broadcast,
    /// Individually addressed. Retries apply (Veins reference: short retry limit 7).
    Unicast {
        /// The intended receiver.
        to: NodeId,
    },
}

impl FrameKind {
    /// True for a group-addressed frame.
    #[must_use]
    pub const fn is_group_addressed(self) -> bool {
        matches!(self, FrameKind::Broadcast)
    }
}

/// Which SDU a frame carries, and which frame it is on its link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SduRef {
    /// The service data unit handed down the stack.
    pub sdu: SduId,
    /// The frame counter on the transmitting node.
    pub seq: FrameSeq,
}

impl SduRef {
    /// A reference to `sdu` carried by frame number `seq`.
    #[must_use]
    pub const fn new(sdu: SduId, seq: FrameSeq) -> Self {
        Self { sdu, seq }
    }
}

/// Everything the PHY needs to know about a frame it is asked to send
/// (03-interfaces.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameDescriptor {
    /// PSDU length in bytes: the MAC frame including header and FCS.
    pub bytes: u32,
    /// The modulation and coding scheme.
    pub mcs: Mcs,
    /// Transmit power at the antenna connector, dBm (EIRP once the antenna gain is
    /// added by the propagation model).
    pub tx_power_dbm: f64,
    /// The channel.
    pub channel: ChannelId,
    /// The EDCA access category the MAC queued it in.
    pub ac: AccessCategory,
    /// Group-addressed or individually addressed.
    pub kind: FrameKind,
    /// Which SDU it carries.
    pub sdu_ref: SduRef,
}

impl FrameDescriptor {
    /// A broadcast frame of `bytes` at `mcs`, the shape of a CAM or BSM: AC_VO, 23 dBm,
    /// on the control channel.
    #[must_use]
    pub fn broadcast(bytes: u32, mcs: Mcs, sdu_ref: SduRef) -> Self {
        Self {
            bytes,
            mcs,
            tx_power_dbm: 23.0,
            channel: ChannelId::CCH,
            ac: AccessCategory::Vo,
            kind: FrameKind::Broadcast,
            sdu_ref,
        }
    }
}

/// A transmission in progress: what [`crate::traits::Phy::begin_tx`] returns.
///
/// It carries the deadline rather than scheduling it, for the reason
/// [`crate::traits::Phy`] documents: the event payload enum lives in the engine crate
/// (build decision D8), so a model cannot construct one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TxHandle {
    /// A per-PHY-instance monotonic id. Identifies the transmission in every record and
    /// in every receiver's arrival set.
    pub id: u64,
    /// The transmitting node.
    pub tx: NodeId,
    /// The channel.
    pub channel: ChannelId,
    /// When the first preamble symbol left the antenna.
    pub start: SimTime,
    /// When the last symbol leaves it: `start + air_time`, the instant the engine must
    /// schedule the `PhyEnd` event for.
    pub end: SimTime,
    /// The air time [`crate::traits::Phy::air_time`] computed.
    pub air_time: Duration,
}

/// One arrival at one receiver: what a `finish_rx` call names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RxHandle {
    /// The transmission this arrival belongs to.
    pub tx: u64,
    /// The receiving node.
    pub rx: NodeId,
}

/// What became of one arrival (03-interfaces.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RxOutcome {
    /// Decoded.
    Received {
        /// The effective SINR the error model was evaluated at, dB.
        sinr_db: f64,
        /// The received signal strength, dBm.
        rssi_dbm: f64,
    },
    /// Not decoded, for exactly one reason (invariant I-R3).
    Lost(LossCause),
}

/// Why an arrival was not decoded. Exactly one per lost frame (invariant I-R3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LossCause {
    /// Beyond the model's range cutoff — the abstract tier's disc, or a `high`-tier link
    /// the engine never registered an arrival for.
    OutOfRange,
    /// The received power was below the receiver's sensitivity for the frame's MCS.
    BelowSensitivity,
    /// The error model drew a frame error at the computed SINR with interferers present.
    Collision,
    /// The preamble could not be locked: the receiver was already locked to a stronger
    /// frame and this one did not exceed it by the capture threshold.
    PreambleMissed,
    /// The receiver was transmitting (802.11p is half duplex).
    HalfDuplex,
    /// A collider that the receiver could hear was outside the *transmitter's* CCA range
    /// at the start of the frame, so CSMA could not have prevented the overlap.
    HiddenTerminal,
    /// Deliberate interference (07-threats).
    Jammed,
    /// The error model drew a frame error with no interferer present: thermal noise and
    /// fading alone.
    Fading,
    /// In-band emission from a sidelink transmitter on an adjacent subchannel
    /// (04-models.md §5).
    InBandEmission,
    /// Two sidelink transmitters selected the same resource (04-models.md §5).
    ResourceCollision,
    /// The abstract tier's Bernoulli draw said "not received" and the tier models no
    /// mechanism finer than that.
    Abstract,
}

impl LossCause {
    /// The spelling used in records and reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            LossCause::OutOfRange => "out-of-range",
            LossCause::BelowSensitivity => "below-sensitivity",
            LossCause::Collision => "collision",
            LossCause::PreambleMissed => "preamble-missed",
            LossCause::HalfDuplex => "half-duplex",
            LossCause::HiddenTerminal => "hidden-terminal",
            LossCause::Jammed => "jammed",
            LossCause::Fading => "fading",
            LossCause::InBandEmission => "in-band-emission",
            LossCause::ResourceCollision => "resource-collision",
            LossCause::Abstract => "abstract",
        }
    }
}

/// The clear-channel assessment state of one channel at one node.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CcaState {
    /// The medium is idle.
    Idle,
    /// The medium is busy: the measured energy exceeded the threshold.
    Busy {
        /// The energy that made it busy, dBm.
        energy_dbm: f64,
    },
}

impl CcaState {
    /// True when the medium is busy.
    #[must_use]
    pub const fn is_busy(self) -> bool {
        matches!(self, CcaState::Busy { .. })
    }
}

/// Why the MAC refused an SDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DropCause {
    /// The frame exceeds the MSDU cap and the fragmenter of 04-models.md §7.3 should
    /// have acted first.
    #[error("frame of {bytes} B exceeds the {cap} B MSDU cap")]
    TooLarge {
        /// The offending size.
        bytes: u32,
        /// The cap.
        cap: u32,
    },
    /// The access category's queue is full.
    #[error("the {ac} queue at this node is full ({depth} frames)")]
    QueueFull {
        /// Which queue.
        ac: &'static str,
        /// Its depth.
        depth: usize,
    },
    /// DCC refused the transmission outright.
    #[error("congestion control dropped the frame")]
    Dcc,
}

/// One SDU as the MAC receives it from the network layer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MacSdu {
    /// The frame that will carry it, already sized and rated.
    pub frame: FrameDescriptor,
    /// When the network layer handed it down.
    pub enqueued_at: SimTime,
}

/// How a MAC shares the medium (03-interfaces.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceModel {
    /// Carrier sense multiple access, with or without the full EDCA state machine.
    Csma {
        /// True when the four access categories have their own AIFS and backoff
        /// (`mac/80211p/edca-ocb`); false for the slotted abstraction.
        edca: bool,
    },
    /// A sidelink resource pool (04-models.md §5).
    SidelinkPool {
        /// Subchannels in the pool.
        subch: u32,
        /// Selection-window period, ms.
        period_ms: u32,
        /// Whether semi-persistent scheduling is active.
        sps: bool,
    },
}

// =========================================================================================
// Propagation and geometry
// =========================================================================================

/// One end of a radio link (03-interfaces.md §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioEndpoint {
    /// The node.
    pub node: NodeId,
    /// The antenna phase centre, world-local metres. `z` is the antenna height above the
    /// local ground.
    pub pos: Vec3,
    /// Antenna gain, dBi. Scalar unless `pattern` is set.
    pub gain_dbi: f64,
    /// The azimuth and elevation pattern, when the `high` tier uses one.
    pub pattern: Option<PatternRef>,
    /// When `pos` was last updated. The propagation model reports it so a caller can see
    /// how stale the geometry behind a loss is.
    pub pos_time: SimTime,
    /// What kind of station this is, which selects the default antenna height and the
    /// NLOSv antenna-height case.
    pub class: ActorClass,
}

impl RadioEndpoint {
    /// An isotropic endpoint of `class` at `pos`, with the gain 04-models.md §3.7 cites
    /// for that class.
    #[must_use]
    pub fn isotropic(node: NodeId, pos: Vec3, class: ActorClass, pos_time: SimTime) -> Self {
        Self {
            node,
            pos,
            gain_dbi: class.default_gain_dbi(),
            pattern: None,
            pos_time,
            class,
        }
    }

    /// The antenna height above ground, metres: the `z` of the phase centre.
    #[must_use]
    pub const fn height_m(&self) -> f64 {
        self.pos.z
    }
}

/// A named antenna pattern.
///
/// No pattern is cached (04-models.md §3.7), so nothing in this crate ships one: the
/// reference is carried so that a scenario which supplies a pattern file can be
/// represented, and `propagation/antenna/pattern` is `TODO: calibrate`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PatternRef(pub String);

/// The station class of a radio endpoint.
///
/// Heights and gains are 04-models.md §3.7's: vehicle UE 3 dBi and 1.5 m
/// [TR 36.885 Table A.1.1-1], pedestrian UE 0 dBi, RSU 3 dBi at 5 m. The TR 37.885
/// vehicle types (antenna 0.75, 1.6 and 3 m) are the [`ActorClass::Car`],
/// [`ActorClass::Van`] and [`ActorClass::Truck`] rows.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ActorClass {
    /// Passenger car: 1.5 m body, TR 37.885 Type 1 or 2.
    #[default]
    Car,
    /// Van or delivery vehicle.
    Van,
    /// Truck or bus: TR 37.885 Type 3, 3 m body.
    Truck,
    /// Motorcycle or moped.
    Motorcycle,
    /// Bicycle.
    Bicycle,
    /// Pedestrian carrying a VRU device.
    Pedestrian,
    /// Roadside unit.
    Rsu,
    /// Cellular base station.
    BaseStation,
}

impl ActorClass {
    /// The default antenna gain, dBi [TR 36.885 Table A.1.1-1, 04-models.md §3.7].
    #[must_use]
    pub const fn default_gain_dbi(self) -> f64 {
        match self {
            ActorClass::Pedestrian | ActorClass::Bicycle => 0.0,
            ActorClass::Car
            | ActorClass::Van
            | ActorClass::Truck
            | ActorClass::Motorcycle
            | ActorClass::Rsu => 3.0,
            // TR 37.885's macro-comparable 23 dBi is a beam-forming assumption for the
            // above-6 GHz study and is not a default here (04-models.md §3.7), so a base
            // station gets the same scalar as an RSU until a pattern is supplied.
            ActorClass::BaseStation => 3.0,
        }
    }

    /// The default antenna height above ground, metres [04-models.md §3.7]: 1.5 m for a
    /// vehicle (TR 36.885), 3 m for a truck or bus (TR 37.885 Type 3), 5 m for an RSU.
    #[must_use]
    pub const fn default_antenna_height_m(self) -> f64 {
        match self {
            ActorClass::Car | ActorClass::Van | ActorClass::Motorcycle => 1.5,
            ActorClass::Truck => 3.0,
            ActorClass::Bicycle | ActorClass::Pedestrian => 1.6,
            ActorClass::Rsu | ActorClass::BaseStation => 5.0,
        }
    }

    /// The default body height, metres, used when this class blocks a link and no actual
    /// [`Dims`] are available [04-models.md §2.7 SUMO vClass table].
    #[must_use]
    pub const fn default_body_height_m(self) -> f64 {
        match self {
            ActorClass::Car => 1.5,
            ActorClass::Van => 2.86,
            ActorClass::Truck => 3.4,
            ActorClass::Motorcycle => 1.5,
            ActorClass::Bicycle => 1.7,
            ActorClass::Pedestrian => 1.719,
            ActorClass::Rsu | ActorClass::BaseStation => 0.0,
        }
    }
}

/// Every term of a large-scale loss, so the inspector can show the whole breakdown
/// (03-interfaces.md §4).
///
/// Signs: `path_db`, `obstacle_db` and `weather_db` are **losses** (positive numbers
/// attenuate), `shadow_db` is a loss that may be negative (a favourable shadowing
/// realisation), and `antenna_db` is a **gain** (positive numbers help). `total_db` is
/// the loss the link budget subtracts from the transmit power:
/// `P_rx = P_tx − total_db`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct LossBreakdown {
    /// Distance-dependent path loss, dB.
    pub path_db: f64,
    /// Large-scale shadowing realisation, dB.
    pub shadow_db: f64,
    /// Obstacle shadowing (buildings, vehicles, terrain, foliage), dB.
    pub obstacle_db: f64,
    /// Atmospheric attenuation, dB (negligible at 5.9 GHz, §3.6, kept for honesty).
    pub weather_db: f64,
    /// Antenna gain of both ends, dB (a gain, not a loss).
    pub antenna_db: f64,
    /// `path + shadow + obstacle + weather − antenna`.
    pub total_db: f64,
}

impl LossBreakdown {
    /// The quantum every field is written at (build decision D9).
    pub const Q_DB: f64 = numeric::Q_DB;

    /// A breakdown from its terms, with `total_db` computed so that it can never
    /// disagree with them.
    #[must_use]
    pub fn new(
        path_db: f64,
        shadow_db: f64,
        obstacle_db: f64,
        weather_db: f64,
        antenna_db: f64,
    ) -> Self {
        // The sum is ordered so that two builds cannot disagree about its last bit.
        let total_db =
            math::sum_ordered([path_db, shadow_db, obstacle_db, weather_db, -antenna_db]);
        Self {
            path_db,
            shadow_db,
            obstacle_db,
            weather_db,
            antenna_db,
            total_db,
        }
    }

    /// A breakdown with a path loss and nothing else.
    #[must_use]
    pub fn path_only(path_db: f64) -> Self {
        Self::new(path_db, 0.0, 0.0, 0.0, 0.0)
    }

    /// What a recorder or exporter writes: every term on the [`LossBreakdown::Q_DB`]
    /// grid.
    #[must_use]
    pub fn quantized(&self) -> Self {
        Self {
            path_db: numeric::q_db(self.path_db),
            shadow_db: numeric::q_db(self.shadow_db),
            obstacle_db: numeric::q_db(self.obstacle_db),
            weather_db: numeric::q_db(self.weather_db),
            antenna_db: numeric::q_db(self.antenna_db),
            total_db: numeric::q_db(self.total_db),
        }
    }

    /// The received power in dBm for a transmit power in dBm.
    #[must_use]
    pub fn rx_power_dbm(&self, tx_power_dbm: f64) -> f64 {
        tx_power_dbm - self.total_db
    }

    /// True when every term is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.path_db.is_finite()
            && self.shadow_db.is_finite()
            && self.obstacle_db.is_finite()
            && self.weather_db.is_finite()
            && self.antenna_db.is_finite()
            && self.total_db.is_finite()
    }
}

/// How a link is obstructed (03-interfaces.md §2).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum LosClass {
    /// Unobstructed.
    #[default]
    Los,
    /// Blocked by a building.
    NlosB,
    /// Blocked by a vehicle (the "OLOS" of Abbas 2015, the "NLOSv" of TR 37.885).
    NlosV,
    /// Blocked by terrain.
    NlosT,
    /// Blocked by both a building and a vehicle.
    NlosBv,
}

impl LosClass {
    /// True when nothing obstructs the link.
    #[must_use]
    pub const fn is_los(self) -> bool {
        matches!(self, LosClass::Los)
    }

    /// True when a building obstructs the link.
    #[must_use]
    pub const fn has_building(self) -> bool {
        matches!(self, LosClass::NlosB | LosClass::NlosBv)
    }

    /// True when a vehicle obstructs the link.
    #[must_use]
    pub const fn has_vehicle(self) -> bool {
        matches!(self, LosClass::NlosV | LosClass::NlosBv)
    }

    /// The spelling used in records and reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            LosClass::Los => "los",
            LosClass::NlosB => "nlos-b",
            LosClass::NlosV => "nlos-v",
            LosClass::NlosT => "nlos-t",
            LosClass::NlosBv => "nlos-bv",
        }
    }
}

/// One diffracting edge on the path, for the knife-edge models of 04-models.md §3.5.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KnifeEdge {
    /// Distance from the transmitter to the edge, metres.
    pub d1_m: f64,
    /// Distance from the edge to the receiver, metres.
    pub d2_m: f64,
    /// Height of the edge above the straight transmitter-receiver line, metres.
    /// Negative when the edge is below the line.
    pub h_m: f64,
    /// What the edge is, for the breakdown and the inspector.
    pub source: EdgeSource,
}

/// What a [`KnifeEdge`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeSource {
    /// A terrain summit from the DEM profile.
    Terrain,
    /// A vehicle body.
    Vehicle {
        /// Which actor.
        actor: ActorId,
    },
    /// A building roof line.
    Building {
        /// Which building.
        building: BuildingId,
    },
}

/// Line-of-sight classification and obstruction geometry (03-interfaces.md §2).
///
/// `SmallVec` in the published signature is `Vec` here: the crate has no `smallvec`
/// dependency, the allocation is off the hot path (an edge list is built only for the
/// `high` tier's diffraction models) and nothing in the contract depends on the
/// container.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LosResult {
    /// How the link is obstructed.
    pub class: LosClass,
    /// Building outline intersections along the path — the Sommer 2011 `n`.
    pub walls_crossed: u16,
    /// Length of the path inside buildings, metres — the Sommer 2011 `d_m`.
    pub obstructed_len_m: f64,
    /// Diffracting edges, for the vehicle and terrain knife-edge models.
    pub knife_edges: Vec<KnifeEdge>,
}

impl LosResult {
    /// An unobstructed link.
    #[must_use]
    pub fn clear() -> Self {
        Self::default()
    }

    /// A link blocked by buildings: `walls` exterior walls and `len_m` metres inside.
    #[must_use]
    pub fn blocked_by_buildings(walls: u16, len_m: f64) -> Self {
        Self {
            class: LosClass::NlosB,
            walls_crossed: walls,
            obstructed_len_m: len_m,
            knife_edges: Vec::new(),
        }
    }
}

/// One actor as an obstacle: the subset of an actor's ground truth the obstacle models
/// read.
///
/// 03-interfaces.md §2 passes `Option<&ActorIndex>` to `los()`. No actor index exists yet
/// — `v2xw-world` indexes static geometry only, and the engine's dynamic index lands with
/// the mobility phase — so this crate defines the shape it needs and [`ActorSet`] holds
/// them in id order. The engine's index converts into one; the conversion is a map, and
/// keeping it here means the obstacle models can be tested against hand-placed blockers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ActorObstacle {
    /// Which actor.
    pub actor: ActorId,
    /// Its reference point, world-local metres (rear-axle centre for a vehicle).
    pub pos: Vec3,
    /// Its bounding dimensions.
    pub dims: Dims,
    /// Its heading, ENU radians.
    pub heading_rad: f64,
    /// Its class.
    pub class: ActorClass,
}

impl ActorObstacle {
    /// The top of the body above the ground the actor stands on, metres.
    #[must_use]
    pub fn top_z_m(&self) -> f64 {
        self.pos.z + self.dims.height_m
    }
}

/// A set of actors that may obstruct links, held in [`ActorId`] order.
///
/// The order is the point: an obstacle model that walked an unordered set would let the
/// collection order reach a loss value, and two runs that collected the same actors
/// differently would produce different dB.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActorSet {
    actors: Vec<ActorObstacle>,
}

impl ActorSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A set from any iterator, sorted by [`ActorId`].
    #[must_use]
    pub fn from_iter_sorted(it: impl IntoIterator<Item = ActorObstacle>) -> Self {
        let mut actors: Vec<ActorObstacle> = it.into_iter().collect();
        actors.sort_by_key(|a| a.actor);
        Self { actors }
    }

    /// Adds an actor, keeping the set sorted.
    pub fn insert(&mut self, actor: ActorObstacle) {
        let at = self.actors.partition_point(|a| a.actor < actor.actor);
        self.actors.insert(at, actor);
    }

    /// The actors, in id order.
    #[must_use]
    pub fn as_slice(&self) -> &[ActorObstacle] {
        &self.actors
    }

    /// How many actors are in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.actors.len()
    }

    /// True when the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aifs_column_of_en302663_table_c5_is_reproduced() {
        assert_eq!(AccessCategory::Vo.aifs(), Duration::from_micros(58));
        assert_eq!(AccessCategory::Vi.aifs(), Duration::from_micros(71));
        assert_eq!(AccessCategory::Be.aifs(), Duration::from_micros(110));
        assert_eq!(AccessCategory::Bk.aifs(), Duration::from_micros(149));
    }

    #[test]
    fn user_priorities_map_to_access_categories_per_table_c3() {
        assert_eq!(AccessCategory::from_user_priority(1), AccessCategory::Bk);
        assert_eq!(AccessCategory::from_user_priority(2), AccessCategory::Bk);
        assert_eq!(AccessCategory::from_user_priority(0), AccessCategory::Be);
        assert_eq!(AccessCategory::from_user_priority(3), AccessCategory::Be);
        assert_eq!(AccessCategory::from_user_priority(4), AccessCategory::Vi);
        assert_eq!(AccessCategory::from_user_priority(5), AccessCategory::Vi);
        assert_eq!(AccessCategory::from_user_priority(6), AccessCategory::Vo);
        assert_eq!(AccessCategory::from_user_priority(7), AccessCategory::Vo);
    }

    #[test]
    fn the_mcs_table_is_self_consistent() {
        for mcs in Mcs::ALL {
            // rate = N_DBPS / symbol duration, and the symbol is 8 µs.
            let implied_mbps = mcs.data_bits_per_symbol() as f64 / 8.0;
            assert!(
                (implied_mbps - mcs.rate_mbps()).abs() < 1e-12,
                "{mcs}: {implied_mbps} != {}",
                mcs.rate_mbps()
            );
            // N_DBPS = N_CBPS × code rate.
            let (num, den) = mcs.code_rate().as_fraction();
            assert_eq!(
                mcs.data_bits_per_symbol() * den,
                mcs.coded_bits_per_symbol() * num,
                "{mcs}"
            );
        }
    }

    #[test]
    fn the_dynamic_sensitivity_row_of_table_2_is_reproduced() {
        // EN 302 663 Table 2 states 6 Mbit/s at −85 dBm.
        assert_eq!(Mcs::R6Qpsk12.sensitivity_dynamic_dbm(), -85.0);
    }

    #[test]
    fn european_channel_numbers_map_to_the_rs2037_centres() {
        assert_eq!(ChannelId(180).centre_hz(), 5_900e6);
        assert_eq!(ChannelId(178).centre_hz(), 5_890e6);
        assert_eq!(ChannelId(176).centre_hz(), 5_880e6);
        assert_eq!(ChannelId(174).centre_hz(), 5_870e6);
        assert_eq!(ChannelId(172).centre_hz(), 5_860e6);
        assert_eq!(ChannelId(182).centre_hz(), 5_910e6);
        assert_eq!(ChannelId(184).centre_hz(), 5_920e6);
    }

    #[test]
    fn an_actor_set_is_ordered_by_id_however_it_was_built() {
        let mk = |id: u32| ActorObstacle {
            actor: ActorId::new(id),
            pos: Vec3::new(0.0, 0.0, 0.0),
            dims: Dims {
                length_m: 5.0,
                width_m: 1.8,
                height_m: 1.5,
            },
            heading_rad: 0.0,
            class: ActorClass::Car,
        };
        let forward = ActorSet::from_iter_sorted([mk(3), mk(1), mk(2)]);
        let mut incremental = ActorSet::new();
        incremental.insert(mk(2));
        incremental.insert(mk(3));
        incremental.insert(mk(1));
        assert_eq!(forward, incremental);
        assert_eq!(
            forward
                .as_slice()
                .iter()
                .map(|a| a.actor.index())
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn a_loss_breakdown_totals_its_own_terms_and_quantises() {
        let b = LossBreakdown::new(90.123_456_7, 4.25, 9.0, 0.11, 6.0);
        assert!((b.total_db - (90.123_456_7 + 4.25 + 9.0 + 0.11 - 6.0)).abs() < 1e-9);
        let q = b.quantized();
        assert!(math::is_on_grid(q.path_db, LossBreakdown::Q_DB));
        assert!(math::is_on_grid(q.total_db, LossBreakdown::Q_DB));
        assert_eq!(q.path_db, 90.123);
    }
}

// =========================================================================================
// MAC grants and congestion control
// =========================================================================================

/// The MAC's decision that one queued frame may go on the air now.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TxGrant {
    /// The SDU that won access.
    pub sdu: MacSdu,
    /// The access category it was queued in.
    pub ac: AccessCategory,
    /// When access was won, which is when the PHY starts the preamble.
    pub at: SimTime,
    /// How many backoff slots were counted down before this frame went out. Zero for a
    /// frame that found the medium idle for a full AIFS.
    pub backoff_slots: u32,
    /// How many times this frame has been attempted. Always 1 for a group-addressed
    /// frame: OCB has no ACK, so nothing is ever retransmitted (04-models.md §4.3).
    pub attempt: u32,
}

/// What a plug-in asks congestion control for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TxRequest {
    /// Frame length, bytes.
    pub bytes: u32,
    /// The MCS the caller wants.
    pub mcs: Mcs,
    /// The transmit power the caller wants, dBm.
    pub power_dbm: f64,
    /// The access category.
    pub ac: AccessCategory,
    /// The channel.
    pub channel: ChannelId,
    /// `T_on`: the air time of this frame, which the gatekeeper divides by δ.
    pub air_time: Duration,
    /// When the request was made.
    pub at: SimTime,
}

/// What congestion control decided (03-interfaces.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateDecision {
    /// Transmit now, with these parameters — which may be lower than requested.
    Now {
        /// Transmit power, dBm.
        power_dbm: f64,
        /// The MCS.
        mcs: Mcs,
    },
    /// Wait until this instant and ask again.
    DelayUntil(SimTime),
    /// Do not transmit this frame at all.
    Drop,
}

/// The reactive-approach state of TS 102 687 Annex A (04-models.md §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReactiveState {
    /// CBR below 30 %.
    Relaxed,
    /// CBR 30-39 %.
    Active1,
    /// CBR 40-49 %.
    Active2,
    /// CBR 50-60 % (Table A.1) or 50-65 % (Table A.2).
    Active3,
    /// Above the Active-3 band.
    Restrictive,
}

impl ReactiveState {
    /// The spelling used in records and reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ReactiveState::Relaxed => "relaxed",
            ReactiveState::Active1 => "active-1",
            ReactiveState::Active2 => "active-2",
            ReactiveState::Active3 => "active-3",
            ReactiveState::Restrictive => "restrictive",
        }
    }
}

/// Which congestion-control algorithm produced a [`DccState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DccAlgorithm {
    /// No congestion control.
    None,
    /// `dcc/etsi/adaptive-ts102687` (04-models.md §6.1).
    AdaptiveTs102687,
    /// `dcc/etsi/reactive-ts102687` (04-models.md §6.2).
    ReactiveTs102687,
    /// `dcc/sae/j2945-1-rate-power` (04-models.md §6.4).
    SaeJ2945_1,
}

/// The congestion-control state of one node, as the HUD and the metrics see it
/// (03-interfaces.md §4: "`state` is exported to the HUD").
///
/// `v2xw_msg::generator::DccState` is the two-field subset a message generator needs
/// (`t_off` and `cbr`); [`DccState::generator_view`] returns exactly those two, so the
/// message layer can be fed without this crate depending on it (the dependency runs the
/// other way in ADR 0010's table).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DccState {
    /// Which algorithm is running.
    pub algorithm: DccAlgorithm,
    /// The last CBR measurement, when there is one.
    pub cbr: Option<f64>,
    /// The minimum time that must elapse between two transmissions.
    pub t_off: Duration,
    /// The adaptive approach's δ, the fraction of time the station may transmit.
    pub delta: Option<f64>,
    /// The reactive approach's state.
    pub state: Option<ReactiveState>,
    /// The transmit power the algorithm allows, dBm.
    pub power_dbm: Option<f64>,
    /// The MCS the algorithm allows.
    pub mcs: Option<Mcs>,
    /// The J2945/1 inter-transmission time.
    pub itt: Option<Duration>,
}

impl DccState {
    /// The quantum δ and the CBR are written at.
    pub const Q_RATIO: f64 = numeric::Q_RATIO;
    /// The quantum a power is written at.
    pub const Q_DB: f64 = numeric::Q_DB;

    /// Congestion control imposing nothing: the state of an unloaded channel, and the
    /// right value for a scenario that does not model DCC.
    pub const UNRESTRICTED: DccState = DccState {
        algorithm: DccAlgorithm::None,
        cbr: None,
        t_off: Duration::ZERO,
        delta: None,
        state: None,
        power_dbm: None,
        mcs: None,
        itt: None,
    };

    /// The `(t_off, cbr)` pair a message generator reads.
    #[must_use]
    pub const fn generator_view(&self) -> (Duration, Option<f64>) {
        (self.t_off, self.cbr)
    }

    /// What a recorder or exporter writes: every float on its declared grid.
    #[must_use]
    pub fn quantized(&self) -> Self {
        Self {
            cbr: self.cbr.map(numeric::q_ratio),
            delta: self.delta.map(|d| math::quantize_to(d, 1e-9)),
            power_dbm: self.power_dbm.map(numeric::q_db),
            ..*self
        }
    }
}

impl Default for DccState {
    fn default() -> Self {
        Self::UNRESTRICTED
    }
}
