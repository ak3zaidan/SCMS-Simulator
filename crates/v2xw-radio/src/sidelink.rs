//! The resource structure LTE-V2X Mode 4 and NR-V2X Mode 2 share: numerologies,
//! sub-channels, modulation and coding, SCI sizes, reservation periods, in-band
//! emissions and the channel-occupancy measurements (04-models.md §5.1, §5.2).
//!
//! Mode 4 and Mode 2 are one sensing-and-scheduling engine over two resource grids
//! (04-models.md §5: "Mode 4 and Mode 2 share one parameterized sensing and SPS
//! engine"). This module is that grid. It holds no state a run mutates and no RNG: it is
//! the arithmetic of the pool, so that [`crate::sps`] can be about *selection* and
//! [`crate::cv2x`] about *decoding*.
//!
//! # What a resource is
//!
//! A [`SlResource`] is `(slot, first sub-channel, length in sub-channels)`. The slot is
//! the LTE subframe index or the NR slot index — the same integer under two names,
//! counted from scenario start, which is why one type serves both. The sub-channel is the
//! frequency unit `sizeSubchannel-r14` and `sl-SubchannelSize-r16` define, and the length
//! is what the transport block needs at the configured MCS ([`PoolConfig::subchannels_for`]).
//!
//! # The three places a number here is not a printed standard value
//!
//! 1. **The in-band-emission mask.** 04-models.md §5.1 marks the TS 36.101 §6.5.2A.3
//!    numeric table UNVERIFIED ("garbled extraction") while verifying the
//!    `{W, X, Y, Z} = {3, 6, 3, 3}` parameterization it is evaluated with. The mask here
//!    therefore ships as [`IbeMask::todo_calibrate_default`] with a calibration plan, and
//!    every model that uses it registers `unvalidated` on that account. [`IbeMask::OFF`]
//!    exists because Todisco 2021 Fig. 7's "best" arm is measured *without* IBE, so a
//!    validation run has to be able to turn it off.
//! 2. **The LTE code rates.** Rel-14 sidelink MCS maps through the TS 36.213 transport
//!    block size tables, which 04-models.md §5.1 does not print. The LTE presets here are
//!    therefore built from the *allocations* the literature prints — 190 B in 10 PRB at
//!    QPSK r0.7, 300 B in 20-22 PRB at QPSK r0.5, Bazzi's 300 B at MCS 4 in 50 PRB and
//!    MCS 7 in 25 PRB — and the code rate is derived from [`PoolConfig::payload_bits`].
//!    That derivation is checked against all four printed allocations by
//!    `lte_presets_reproduce_the_printed_allocations`.
//! 3. **Nothing else.** The NR MCS table is TS 38.214 Table 5.1.3.1-1 transcribed in
//!    full; 04-models.md §5.2 prints its two endpoint rows (MCS 0 → Qm 2, R 120/1024,
//!    SE 0.2344; MCS 28 → Qm 6, R 948/1024, SE 5.5547) and
//!    `nr_mcs_table_endpoints_match_the_design_document` checks both, plus monotonicity of
//!    spectral efficiency across all 29 rows, which is what makes a mistyped middle row
//!    visible.

use serde::{Deserialize, Serialize};
use v2xw_core::time::Duration;

/// Which sidelink the pool is configured for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlRat {
    /// LTE-V2X Mode 4, Rel-14 (04-models.md §5.1).
    LteMode4,
    /// NR-V2X Mode 2, Rel-16 (04-models.md §5.2).
    NrMode2,
}

impl SlRat {
    /// The `Rat` of [`crate::types`] this sidelink reports.
    #[must_use]
    pub const fn rat(self) -> crate::types::Rat {
        match self {
            SlRat::LteMode4 => crate::types::Rat::LteV2xPc5,
            SlRat::NrMode2 => crate::types::Rat::NrV2xPc5,
        }
    }

    /// The label a scenario spells.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SlRat::LteMode4 => "lte-v2x-mode4",
            SlRat::NrMode2 => "nr-v2x-mode2",
        }
    }
}

// =========================================================================================
// Numerology (04-models.md §5.2)
// =========================================================================================

/// The NR numerology `µ`, and the one LTE has (`µ = 0`, a 1 ms subframe).
///
/// SCS 15 / 30 / 60 kHz are FR1 and 120 kHz is FR2; one numerology per pool
/// [TS 38.211 via Garcia 2021 Table IV].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Numerology {
    /// µ = 0: 15 kHz sub-carriers, 1 ms slot. The LTE subframe, and the NR baseline.
    Mu0,
    /// µ = 1: 30 kHz, 0.5 ms.
    Mu1,
    /// µ = 2: 60 kHz, 0.25 ms.
    Mu2,
    /// µ = 3: 120 kHz, 0.125 ms. FR2 only.
    Mu3,
}

impl Numerology {
    /// Every numerology, in `µ` order.
    pub const ALL: [Numerology; 4] = [
        Numerology::Mu0,
        Numerology::Mu1,
        Numerology::Mu2,
        Numerology::Mu3,
    ];

    /// `µ` itself.
    #[must_use]
    pub const fn mu(self) -> u32 {
        match self {
            Numerology::Mu0 => 0,
            Numerology::Mu1 => 1,
            Numerology::Mu2 => 2,
            Numerology::Mu3 => 3,
        }
    }

    /// `2^µ`: slots per millisecond.
    #[must_use]
    pub const fn slots_per_ms(self) -> u32 {
        1 << self.mu()
    }

    /// Sub-carrier spacing in kHz: `15 · 2^µ` [TS 38.211 via Garcia 2021 Table IV].
    #[must_use]
    pub const fn scs_khz(self) -> u32 {
        15 * self.slots_per_ms()
    }

    /// The slot duration.
    #[must_use]
    pub const fn slot(self) -> Duration {
        Duration::from_nanos(1_000_000 / self.slots_per_ms() as u64)
    }

    /// `T_proc,0`, the sensing-window processing gap in slots: 1, 1, 2, 4 for µ = 0..3
    /// [TS 38.214 Table 8.1.4-1, via 04-models.md §5.2].
    #[must_use]
    pub const fn t_proc0_slots(self) -> u32 {
        match self {
            Numerology::Mu0 | Numerology::Mu1 => 1,
            Numerology::Mu2 => 2,
            Numerology::Mu3 => 4,
        }
    }

    /// `T_proc,1`, the selection-window processing gap in slots: 3, 5, 9, 17 for µ = 0..3
    /// (3, 2.5, 2.25, 2.125 ms) [TS 38.214 Table 8.1.4-2, via 04-models.md §5.2].
    #[must_use]
    pub const fn t_proc1_slots(self) -> u32 {
        match self {
            Numerology::Mu0 => 3,
            Numerology::Mu1 => 5,
            Numerology::Mu2 => 9,
            Numerology::Mu3 => 17,
        }
    }

    /// The label a scenario spells.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Numerology::Mu0 => "mu0-15khz",
            Numerology::Mu1 => "mu1-30khz",
            Numerology::Mu2 => "mu2-60khz",
            Numerology::Mu3 => "mu3-120khz",
        }
    }
}

// =========================================================================================
// Modulation and coding (04-models.md §5.1, §5.2)
// =========================================================================================

/// One modulation-and-coding point: the modulation order and the code rate in 1/1024ths.
///
/// The two fields are what a spectral efficiency needs, and the label is what a report
/// prints. `r_1024` is the *channel* code rate `R`, so the spectral efficiency is
/// `qm · R` and a 64-QAM MCS 21 row reads `Qm 6, R 616/1024, SE 3.6094` exactly as
/// TS 38.214 Table 5.1.3.1-1 prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct SlMcsSpec {
    /// What a report and a scenario call it.
    pub label: &'static str,
    /// Modulation order: 2 QPSK, 4 16-QAM, 6 64-QAM, 8 256-QAM.
    pub qm: u8,
    /// The code rate in 1/1024ths.
    pub r_1024: u16,
}

impl SlMcsSpec {
    /// A spec from its three fields.
    #[must_use]
    pub const fn new(label: &'static str, qm: u8, r_1024: u16) -> Self {
        Self { label, qm, r_1024 }
    }

    /// The code rate as a fraction.
    #[must_use]
    pub fn code_rate(self) -> f64 {
        f64::from(self.r_1024) / 1024.0
    }

    /// Spectral efficiency in bits per resource element: `qm · R`.
    #[must_use]
    pub fn spectral_efficiency(self) -> f64 {
        f64::from(self.qm) * self.code_rate()
    }
}

/// The NR MCS table a pool is configured with
/// (`SL-MinMaxMCS-Config-r16 { sl-MCS-Table-r16 {qam64, qam256, qam64LowSE} }`,
/// TS 38.331 verified, 04-models.md §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NrMcsTable {
    /// TS 38.214 Table 5.1.3.1-1, the 64-QAM table. The only one transcribed here.
    Qam64,
}

/// TS 38.214 Table 5.1.3.1-1: `(Qm, R·1024)` for MCS 0 to 28, the 64-QAM table.
///
/// 04-models.md §5.2 prints the two endpoints, which
/// `nr_mcs_table_endpoints_match_the_design_document` checks; the middle rows are
/// transcribed from the table itself and are held honest by the monotonicity assertion in
/// the same test.
const NR_MCS_TABLE1: [(u8, u16); 29] = [
    (2, 120),
    (2, 157),
    (2, 193),
    (2, 251),
    (2, 308),
    (2, 379),
    (2, 449),
    (2, 526),
    (2, 602),
    (2, 679),
    (4, 340),
    (4, 378),
    (4, 434),
    (4, 490),
    (4, 553),
    (4, 616),
    (4, 658),
    (6, 438),
    (6, 466),
    (6, 517),
    (6, 567),
    (6, 616),
    (6, 666),
    (6, 719),
    (6, 772),
    (6, 822),
    (6, 873),
    (6, 910),
    (6, 948),
];

/// The NR MCS labels, index-aligned with [`NR_MCS_TABLE1`], so a spec can carry a
/// `&'static str` without allocating.
const NR_MCS_LABELS: [&str; 29] = [
    "nr-mcs0", "nr-mcs1", "nr-mcs2", "nr-mcs3", "nr-mcs4", "nr-mcs5", "nr-mcs6", "nr-mcs7",
    "nr-mcs8", "nr-mcs9", "nr-mcs10", "nr-mcs11", "nr-mcs12", "nr-mcs13", "nr-mcs14", "nr-mcs15",
    "nr-mcs16", "nr-mcs17", "nr-mcs18", "nr-mcs19", "nr-mcs20", "nr-mcs21", "nr-mcs22", "nr-mcs23",
    "nr-mcs24", "nr-mcs25", "nr-mcs26", "nr-mcs27", "nr-mcs28",
];

/// One row of TS 38.214 Table 5.1.3.1-1, or `None` for an index above 28.
///
/// MCS 29 to 31 are the reserved retransmission rows, which carry no `Qm`/`R` of their
/// own and so have no spec here.
#[must_use]
pub fn nr_mcs(index: u8) -> Option<SlMcsSpec> {
    let i = usize::from(index);
    let (qm, r) = *NR_MCS_TABLE1.get(i)?;
    Some(SlMcsSpec::new(NR_MCS_LABELS[i], qm, r))
}

/// `190 B at QPSK r0.7 in 10 PRB` — the Molina-Masegosa reference mapping
/// [Molina-Masegosa 2017 §III, via 04-models.md §5.1].
///
/// The rate shipped is 732/1024 = 0.7148, not 717/1024 = 0.7002. The source prints the
/// rate rounded to one decimal and the *allocation* exactly, and the allocation is the
/// binding statement: 190 B plus the 24-bit transport-block CRC is 1,544 bits, and ten
/// PRB of a nine-data-symbol subframe carry 2,160 QPSK coded bits, so the rate the
/// printed mapping implies is 1,544/2,160 = 0.7148. Shipping the rounded 0.7 instead
/// would leave the reference packet 32 bits short of its own reference allocation. The
/// same reasoning fixes every LTE preset below, and
/// `lte_presets_reproduce_the_printed_allocations` is the check.
pub const LTE_QPSK_R070: SlMcsSpec = SlMcsSpec::new("lte-qpsk-r0.7", 2, 732);
/// `300 B at QPSK r0.5 in 20 PRB (22 allocated)` — the same source's second mapping.
pub const LTE_QPSK_R050: SlMcsSpec = SlMcsSpec::new("lte-qpsk-r0.5", 2, 523);
/// Bazzi's MCS 4: 300 B beacons, one beacon resource per TTI, 10-PRB sub-channels, so
/// 50 PRB per transport block [Bazzi 2018 Table 2, via 04-models.md §5.1].
///
/// The code rate is *derived*: 300 B plus the 24-bit CRC in 50 PRB of a 9-data-symbol
/// subframe is `2424 / (108 · 50) = 0.449` bits per resource element at QPSK, hence
/// `R = 0.2245`. `lte_presets_reproduce_the_printed_allocations` checks that the derived
/// rate puts 300 B in exactly the printed 50 PRB.
pub const LTE_MCS4_BAZZI: SlMcsSpec = SlMcsSpec::new("lte-mcs4-bazzi", 2, 240);
/// Bazzi's MCS 7: 300 B beacons, two beacon resources per TTI, so 25 PRB per transport
/// block [Bazzi 2018 Table 2]. Code rate derived the same way.
pub const LTE_MCS7_BAZZI: SlMcsSpec = SlMcsSpec::new("lte-mcs7-bazzi", 2, 460);
/// Bazzi's MCS 14: 16-QAM, 300 B, and the smallest allocation of the three
/// [Bazzi 2018 Fig. 3]. Bazzi Table 2 prints the allocation for MCS 4 and MCS 7 only, so
/// the 16-QAM code rate here is the LTE Rel-14 16-QAM entry point (`I_TBS 13`) rather
/// than a printed value, and every model card that offers it records that.
pub const LTE_MCS14_BAZZI: SlMcsSpec = SlMcsSpec::new("lte-mcs14-bazzi", 4, 490);

/// Every LTE preset, in a fixed order.
pub const LTE_PRESETS: [SlMcsSpec; 5] = [
    LTE_QPSK_R070,
    LTE_QPSK_R050,
    LTE_MCS4_BAZZI,
    LTE_MCS7_BAZZI,
    LTE_MCS14_BAZZI,
];

// =========================================================================================
// Reservation periods (04-models.md §5.1 step 6, §5.2)
// =========================================================================================

/// The resource reservation interval in milliseconds.
///
/// LTE: `{0, 20, 50, 100, 200, …, 1000}` ms, twelve non-zero values, up to sixteen
/// configured [Garcia 2021 §II.B]. NR widens the list to
/// `{0, 1..99, 100, 200, …, 1000}` ms [`sl-ResourceReservePeriodList-r16`]. Zero means
/// "no reservation": one-shot selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Rri(pub u32);

impl Rri {
    /// The twelve non-zero LTE values [Garcia 2021 §II.B].
    pub const LTE_VALUES: [u32; 12] = [20, 50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];

    /// No reservation: select a resource for this transport block and nothing further.
    pub const NONE: Rri = Rri(0);
    /// 100 ms, the 10 Hz beaconing period every validation row in 04-models.md §5.5 uses.
    pub const MS100: Rri = Rri(100);
    /// 20 ms, the 50 pps row.
    pub const MS20: Rri = Rri(20);

    /// True when the value is one LTE admits.
    #[must_use]
    pub fn is_lte_legal(self) -> bool {
        self.0 == 0 || Self::LTE_VALUES.contains(&self.0)
    }

    /// True when the value is one NR admits: zero, 1 to 99, or a multiple of 100 up to
    /// 1,000 [`sl-ResourceReservePeriodList-r16`].
    #[must_use]
    pub fn is_nr_legal(self) -> bool {
        self.0 < 100 || (self.0 <= 1000 && self.0 % 100 == 0)
    }

    /// The reselection-counter range this RRI draws `C_resel` from.
    ///
    /// LTE [Garcia 2021 §II.B]: `[5, 15]` for RRI ≥ 100 ms, `[10, 30]` for 50 ms,
    /// `[25, 75]` for 20 ms. NR [TS 38.321 §5.22.1]: `[5C, 15C]` with
    /// `C = 100 / max(20, RRI)`, which reproduces the LTE numbers at 20 and 100 ms and
    /// differs at 50 ms (`C = 2`, so `[10, 30]` — the same) and below 20 ms (`C = 5`).
    #[must_use]
    pub fn c_resel_range(self, rat: SlRat) -> (u32, u32) {
        if self.0 == 0 {
            return (1, 1);
        }
        match rat {
            SlRat::LteMode4 => match self.0 {
                0..=20 => (25, 75),
                21..=50 => (10, 30),
                _ => (5, 15),
            },
            SlRat::NrMode2 => {
                if self.0 >= 100 {
                    (5, 15)
                } else {
                    let c = 100 / self.0.max(20);
                    (5 * c, 15 * c)
                }
            }
        }
    }

    /// The RRI in slots at a numerology.
    #[must_use]
    pub const fn slots(self, mu: Numerology) -> u64 {
        self.0 as u64 * mu.slots_per_ms() as u64
    }
}

/// `probResourceKeep` / `sl-ProbResourceKeep-r16`: the probability a UE keeps its
/// resource when the reselection counter expires.
///
/// The IE admits `{0, 0.2, 0.4, 0.6, 0.8}` [TS 36.331 V14.4.0 `probResourceKeep-r14`,
/// verified]. The simulation choices in the literature are 0.4 (Bazzi) and 0 (Molina-
/// Masegosa); 04-models.md §5.1 makes 0 the default and records it as a study choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProbResourceKeep(u8);

impl ProbResourceKeep {
    /// The default, and Molina-Masegosa's choice.
    pub const ZERO: ProbResourceKeep = ProbResourceKeep(0);
    /// Bazzi's choice.
    pub const P040: ProbResourceKeep = ProbResourceKeep(2);

    /// The five legal values, in order.
    pub const ALL: [ProbResourceKeep; 5] = [
        ProbResourceKeep(0),
        ProbResourceKeep(1),
        ProbResourceKeep(2),
        ProbResourceKeep(3),
        ProbResourceKeep(4),
    ];

    /// One of the five legal values, or `None`.
    #[must_use]
    pub fn from_probability(p: f64) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| (c.probability() - p).abs() < 1e-9)
    }

    /// The probability itself.
    #[must_use]
    pub fn probability(self) -> f64 {
        f64::from(self.0) * 0.2
    }
}

// =========================================================================================
// Sub-channel sizes and the pool (04-models.md §5.1, §5.2)
// =========================================================================================

/// `sizeSubchannel-r14`: the nineteen legal LTE sub-channel sizes in PRB
/// [TS 36.331 V14.4.0 ASN.1, grep verified].
pub const LTE_SUBCHANNEL_SIZES: [u32; 19] = [
    4, 5, 6, 8, 9, 10, 12, 15, 16, 18, 20, 25, 30, 48, 50, 72, 75, 96, 100,
];

/// `sl-SubchannelSize-r16`: the eight legal NR sub-channel sizes in PRB
/// [TS 38.331 ASN.1, grep verified].
pub const NR_SUBCHANNEL_SIZES: [u32; 8] = [10, 12, 15, 20, 25, 50, 75, 100];

/// `PSCCH` PRB counts NR admits, all below the sub-channel size
/// [RAN1 #99 via Garcia 2021; the 38.331 clause is UNVERIFIED in 04-models.md §5.2].
pub const NR_PSCCH_PRBS: [u32; 5] = [10, 12, 15, 20, 25];

/// The in-band-emission mask: how far below its own transmit power a sidelink transmitter
/// leaks into a sub-channel `k` away.
///
/// 04-models.md §5.1 verifies the *parameterization* TR 36.885 Annex A.1.1 reuses —
/// TS 36.101 §6.5.2A.3 with `{W, X, Y, Z} = {3, 6, 3, 3}` for single-cluster SC-FDMA —
/// and marks the numeric mask table itself UNVERIFIED (garbled extraction). The values
/// here are therefore `todo-calibrate`, and every model that installs this mask says so
/// in its card.
///
/// Why it has to be here at all: Todisco's sub-carrier-spacing ablation shows the benefit
/// of a higher numerology comes mainly from having fewer co-slot IBE contributors
/// [04-models.md §5.2 modeling note 7], so an NR PHY without an IBE term over-predicts
/// every high numerology. [`IbeMask::OFF`] is the arm that measures exactly that.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IbeMask {
    /// Attenuation of the leakage into the immediately adjacent sub-channel, dB.
    pub adjacent_db: f64,
    /// Extra attenuation per further sub-channel of separation, dB.
    pub per_extra_subchannel_db: f64,
    /// The floor: leakage never modelled weaker than this below the carrier, dB.
    pub floor_db: f64,
}

impl IbeMask {
    /// No in-band emissions at all: the Todisco "without IBE" arm.
    pub const OFF: IbeMask = IbeMask {
        adjacent_db: f64::INFINITY,
        per_extra_subchannel_db: 0.0,
        floor_db: f64::INFINITY,
    };

    /// The shipped default: 30 dB into the adjacent sub-channel, 3 dB more per further
    /// sub-channel, floored at 50 dB.
    ///
    /// `TODO: calibrate` — plan: read TS 36.101 Table 6.5.2A.3-1, evaluate it at
    /// `{W, X, Y, Z} = {3, 6, 3, 3}` for each `(allocation, separation)` pair a 10 MHz
    /// pool can produce, and replace these three numbers with the resulting table. The
    /// 30 dB entry point is the general in-band-emission limit's order of magnitude and
    /// not a printed value.
    #[must_use]
    pub const fn todo_calibrate_default() -> Self {
        Self {
            adjacent_db: 30.0,
            per_extra_subchannel_db: 3.0,
            floor_db: 50.0,
        }
    }

    /// The leakage into a sub-channel `separation` away, as an attenuation in dB below
    /// the transmitter's own power. `separation == 0` is the transmitter's own
    /// sub-channel, which is not in-band emission and returns 0 dB.
    #[must_use]
    pub fn attenuation_db(&self, separation: u32) -> f64 {
        if separation == 0 {
            return 0.0;
        }
        if !self.adjacent_db.is_finite() {
            return f64::INFINITY;
        }
        let extra = f64::from(separation - 1) * self.per_extra_subchannel_db;
        (self.adjacent_db + extra).min(self.floor_db)
    }

    /// True when this mask models no emissions.
    #[must_use]
    pub fn is_off(&self) -> bool {
        !self.adjacent_db.is_finite()
    }
}

impl Default for IbeMask {
    fn default() -> Self {
        Self::todo_calibrate_default()
    }
}

/// One resource: a slot, a first sub-channel and a length in sub-channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlResource {
    /// The LTE subframe index or NR slot index, counted from scenario start.
    pub slot: u64,
    /// The first sub-channel.
    pub subch: u32,
    /// How many contiguous sub-channels the transport block occupies.
    pub len: u32,
}

impl SlResource {
    /// A resource from its three fields.
    #[must_use]
    pub const fn new(slot: u64, subch: u32, len: u32) -> Self {
        Self { slot, subch, len }
    }

    /// The sub-channels this resource occupies, as a half-open range.
    #[must_use]
    pub const fn range(&self) -> core::ops::Range<u32> {
        self.subch..self.subch + self.len
    }

    /// True when the two resources are in the same slot and share a sub-channel.
    #[must_use]
    pub const fn overlaps(&self, other: &SlResource) -> bool {
        self.slot == other.slot
            && self.subch < other.subch + other.len
            && other.subch < self.subch + self.len
    }

    /// The smallest sub-channel separation between two resources in the same slot:
    /// 0 when they overlap, 1 when they are adjacent, and so on. `None` when they are in
    /// different slots.
    #[must_use]
    pub const fn separation(&self, other: &SlResource) -> Option<u32> {
        if self.slot != other.slot {
            return None;
        }
        if self.overlaps(other) {
            return Some(0);
        }
        if self.subch >= other.subch + other.len {
            Some(self.subch - (other.subch + other.len) + 1)
        } else {
            Some(other.subch - (self.subch + self.len) + 1)
        }
    }

    /// The same resource shifted by a whole reservation period.
    #[must_use]
    pub const fn shifted(&self, slots: u64) -> Self {
        Self {
            slot: self.slot + slots,
            subch: self.subch,
            len: self.len,
        }
    }
}

/// The SCI the receiver has to decode before the transport block means anything
/// (04-models.md §5.1 SCI format 1, §5.2 SCI 1-A and 2-A/2-B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SciSize {
    /// Payload bits before the CRC.
    pub payload_bits: u32,
    /// CRC bits.
    pub crc_bits: u32,
}

impl SciSize {
    /// LTE SCI format 1: 32 bits [TS 36.212 V14.4.0 §5.4.3.1.2, quoted verbatim in
    /// 04-models.md §5.1]. The 16-bit CRC is the PSCCH CRC.
    pub const LTE_FORMAT1: SciSize = SciSize {
        payload_bits: 32,
        crc_bits: 16,
    };

    /// NR SCI 2-A: 35 bits plus a 24-bit CRC [TS 38.212 §8.4.1.1].
    pub const NR_2A: SciSize = SciSize {
        payload_bits: 35,
        crc_bits: 24,
    };

    /// NR SCI 2-B: 48 bits plus a 24-bit CRC [TS 38.212 §8.4.1.2].
    pub const NR_2B: SciSize = SciSize {
        payload_bits: 48,
        crc_bits: 24,
    };

    /// Total coded-input bits.
    #[must_use]
    pub const fn total_bits(self) -> u32 {
        self.payload_bits + self.crc_bits
    }
}

/// The configured sidelink resource pool: the geometry every other sidelink module reads.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PoolConfig {
    /// Which sidelink.
    pub rat: SlRat,
    /// The numerology. Always [`Numerology::Mu0`] for LTE.
    pub mu: Numerology,
    /// The channel bandwidth in PRB (10 MHz = 50 PRB at 15 kHz).
    pub bandwidth_prb: u32,
    /// The sub-channel size in PRB, one of [`LTE_SUBCHANNEL_SIZES`] or
    /// [`NR_SUBCHANNEL_SIZES`].
    pub subchannel_prb: u32,
    /// PRB the control channel occupies inside the sub-channel: 2 for LTE adjacent PSCCH
    /// [TS 36.213 V14.4.0 §14.2.4], one of [`NR_PSCCH_PRBS`] for NR.
    ///
    /// For NR the field is the PSCCH *width*, which the SINR of the control channel is
    /// computed over ([`crate::cv2x`]); it does not subtract from the shared channel's
    /// PRB, because the NR PSCCH costs *symbols* inside the slot rather than a separate
    /// frequency pool, and that cost is already in `data_symbols` (04-models.md §5.2 slot
    /// layout). `pscch_adjacent` is therefore false for every NR pool.
    pub pscch_prb: u32,
    /// Whether the PSCCH sits in the first PRB of the sub-channel (adjacent) or in a
    /// separate pool (non-adjacent). A pool configuration [TS 36.213 §14.2.4].
    pub pscch_adjacent: bool,
    /// Data symbols per slot available to the shared channel.
    ///
    /// LTE: 9 of the 14 subframe symbols, the other 5 being 4 DMRS and 1 guard
    /// [Garcia 2021 §II.A]. NR: the PSSCH symbols of the slot layout, minus its DMRS
    /// symbols; Todisco's configuration is 12 PSSCH symbols with 2 DMRS
    /// [04-models.md §5.2].
    pub data_symbols: u32,
    /// The modulation and coding the pool transmits data at.
    pub mcs: SlMcsSpec,
    /// The in-band-emission mask.
    pub ibe: IbeMask,
    /// The centre frequency, Hz.
    pub centre_hz: f64,
}

impl PoolConfig {
    /// Data resource elements one PRB carries in one slot: `12 · data_symbols`.
    #[must_use]
    pub const fn data_res_per_prb(&self) -> u32 {
        12 * self.data_symbols
    }

    /// How many sub-channels the pool has: `floor(bandwidth / subchannel_prb)`.
    #[must_use]
    pub const fn subchannels(&self) -> u32 {
        self.bandwidth_prb / self.subchannel_prb
    }

    /// PRB one sub-channel gives the shared channel, after the control channel takes its
    /// share.
    ///
    /// With an adjacent PSCCH the control channel eats PRB out of the *first*
    /// sub-channel of the allocation only, which is why this is not simply
    /// `subchannel_prb - pscch_prb` for a multi-sub-channel allocation; see
    /// [`PoolConfig::data_prb_for`].
    #[must_use]
    pub const fn data_prb_per_subchannel(&self) -> u32 {
        if self.pscch_adjacent {
            self.subchannel_prb.saturating_sub(self.pscch_prb)
        } else {
            self.subchannel_prb
        }
    }

    /// PRB available to the transport block in an allocation of `len` sub-channels.
    #[must_use]
    pub const fn data_prb_for(&self, len: u32) -> u32 {
        if self.pscch_adjacent {
            (self.subchannel_prb * len).saturating_sub(self.pscch_prb)
        } else {
            self.subchannel_prb * len
        }
    }

    /// The payload bits an allocation of `len` sub-channels carries at the pool's MCS,
    /// net of the 24-bit transport-block CRC.
    ///
    /// `bits = floor(data_res_per_prb · data_prb · Qm · R) − 24`.
    #[must_use]
    pub fn payload_bits(&self, len: u32) -> u32 {
        let res = f64::from(self.data_res_per_prb() * self.data_prb_for(len));
        let coded = res * self.mcs.spectral_efficiency();
        // Floor, not round: a transport block cannot use a fraction of a coded bit.
        let bits = coded.floor() as i64 - i64::from(TB_CRC_BITS);
        bits.max(0) as u32
    }

    /// The smallest number of sub-channels that carries `bytes`, or `None` when the whole
    /// pool cannot.
    #[must_use]
    pub fn subchannels_for(&self, bytes: u32) -> Option<u32> {
        let want = bytes * 8;
        (1..=self.subchannels()).find(|&len| self.payload_bits(len) >= want)
    }

    /// The SCI the pool's control channel carries.
    #[must_use]
    pub const fn sci(&self) -> SciSize {
        match self.rat {
            SlRat::LteMode4 => SciSize::LTE_FORMAT1,
            // Broadcast V2X uses SCI 2-B, whose zone id and communication-range fields
            // are what groupcast option 1 needs (04-models.md §5.2).
            SlRat::NrMode2 => SciSize::NR_2B,
        }
    }

    /// The slot duration.
    #[must_use]
    pub const fn slot(&self) -> Duration {
        self.mu.slot()
    }

    /// The slot index `t` falls in.
    #[must_use]
    pub const fn slot_of(&self, t: v2xw_core::time::SimTime) -> u64 {
        t / self.slot().as_nanos()
    }

    /// The instant slot `s` begins.
    #[must_use]
    pub const fn slot_start(&self, s: u64) -> v2xw_core::time::SimTime {
        s * self.slot().as_nanos()
    }

    /// True when every structural field is one the standards admit.
    #[must_use]
    pub fn is_legal(&self) -> bool {
        let sizes: &[u32] = match self.rat {
            SlRat::LteMode4 => &LTE_SUBCHANNEL_SIZES,
            SlRat::NrMode2 => &NR_SUBCHANNEL_SIZES,
        };
        let mu_ok = match self.rat {
            SlRat::LteMode4 => self.mu == Numerology::Mu0,
            SlRat::NrMode2 => true,
        };
        let pscch_ok = match self.rat {
            SlRat::LteMode4 => self.pscch_prb == 2,
            SlRat::NrMode2 => self.pscch_prb <= self.subchannel_prb,
        };
        mu_ok
            && pscch_ok
            && sizes.contains(&self.subchannel_prb)
            && self.subchannels() >= 1
            && self.data_symbols >= 1
    }

    /// The Molina-Masegosa validation pool: 10 MHz, four sub-channels of 12 PRB, adjacent
    /// PSCCH of 2 PRB, 9 data symbols, QPSK r0.7
    /// [Molina-Masegosa 2017 §III, via 04-models.md §5.5].
    #[must_use]
    pub fn molina_masegosa_highway() -> Self {
        Self {
            rat: SlRat::LteMode4,
            mu: Numerology::Mu0,
            bandwidth_prb: 50,
            subchannel_prb: 12,
            pscch_prb: 2,
            pscch_adjacent: true,
            data_symbols: 9,
            mcs: LTE_QPSK_R070,
            ibe: IbeMask::todo_calibrate_default(),
            centre_hz: 5_900e6,
        }
    }

    /// Bazzi's pool: 10 MHz, five sub-channels of 10 PRB [Bazzi 2018 Table 2].
    #[must_use]
    pub fn bazzi_10prb(mcs: SlMcsSpec) -> Self {
        Self {
            subchannel_prb: 10,
            mcs,
            ..Self::molina_masegosa_highway()
        }
    }

    /// Todisco's NR pool: 10 MHz, sub-channels of 10 PRB, PSCCH 1 symbol, PSSCH 12
    /// symbols of which 2 are DMRS, at a caller-chosen numerology and MCS
    /// [Todisco 2021 via 04-models.md §5.2, §5.5].
    #[must_use]
    pub fn todisco_nr(mu: Numerology, mcs: SlMcsSpec) -> Self {
        // 10 MHz holds 52 PRB at 15 kHz, 24 at 30 kHz and 11 at 60 kHz
        // [TS 38.104 Table 5.3.2-1]; the sub-channel size divides what is left.
        let bandwidth_prb = match mu {
            Numerology::Mu0 => 52,
            Numerology::Mu1 => 24,
            Numerology::Mu2 => 11,
            Numerology::Mu3 => 11,
        };
        Self {
            rat: SlRat::NrMode2,
            mu,
            bandwidth_prb,
            subchannel_prb: 10,
            pscch_prb: 10,
            pscch_adjacent: false,
            // 12 PSSCH symbols with 2 DMRS symbols among them.
            data_symbols: 10,
            mcs,
            ibe: IbeMask::todo_calibrate_default(),
            centre_hz: 5_900e6,
        }
    }
}

/// The transport-block CRC, bits [TS 38.212 §7.2.1; TS 36.212 §5.3.2].
pub const TB_CRC_BITS: u32 = 24;

// =========================================================================================
// Channel occupancy: CBR and CR (04-models.md §5.1, §5.2)
// =========================================================================================

/// The sidelink channel-busy-ratio and channel-occupancy-ratio meter
/// [TS 36.214 §5.1.30-5.1.31; TS 38.215 §5.1.25-5.1.27, via 04-models.md §5.1, §5.2].
///
/// CBR is "the fraction of sub-channels whose S-RSSI exceeds the configured threshold
/// over the previous 100 subframes" — a *sub-channel* count, not a busy-time fraction,
/// which is why the sidelink cannot reuse [`crate::mac::CbrMeter`]. CR is
/// "sub-channels used in `[n − a, n − 1]` plus granted in `[n, n + b]` over all
/// configured sub-channels in the window", with `a + b + 1 = 1000` and `a ≥ 500`.
#[derive(Debug, Clone, PartialEq)]
pub struct SidelinkOccupancy {
    subchannels: u32,
    window_slots: u64,
    cr_window_slots: u64,
    cr_past_slots: u64,
    /// `(slot, subchannel)` seen busy, in slot order. Pruned to the CBR window.
    busy: std::collections::BTreeSet<(u64, u32)>,
    /// `(slot, subchannel)` this UE used or was granted, in slot order.
    used: std::collections::BTreeSet<(u64, u32)>,
}

impl SidelinkOccupancy {
    /// A meter for a pool.
    ///
    /// The CBR window is `100 · 2^µ` slots and the CR window `1000 · 2^µ` slots, of which
    /// the past part is `a = 500 · 2^µ` — the smallest `a` the standard admits, and the
    /// one that makes the window symmetric.
    #[must_use]
    pub fn new(pool: &PoolConfig) -> Self {
        let scale = u64::from(pool.mu.slots_per_ms());
        Self {
            subchannels: pool.subchannels(),
            window_slots: 100 * scale,
            cr_window_slots: 1000 * scale,
            cr_past_slots: 500 * scale,
            busy: std::collections::BTreeSet::new(),
            used: std::collections::BTreeSet::new(),
        }
    }

    /// Notes that a sub-channel was above the S-RSSI threshold in a slot.
    pub fn note_busy(&mut self, slot: u64, subch: u32) {
        self.busy.insert((slot, subch));
    }

    /// Notes that this UE transmitted on, or was granted, a sub-channel in a slot.
    pub fn note_used(&mut self, slot: u64, subch: u32) {
        self.used.insert((slot, subch));
    }

    /// Drops everything that has fallen out of the longer of the two windows.
    pub fn prune(&mut self, now_slot: u64) {
        let cutoff = now_slot.saturating_sub(self.cr_window_slots);
        self.busy = self.busy.split_off(&(cutoff, 0));
        self.used = self.used.split_off(&(cutoff, 0));
    }

    /// CBR at slot `n`: busy sub-channels over the last `100 · 2^µ` slots divided by all
    /// sub-channels in that window.
    #[must_use]
    pub fn cbr(&self, now_slot: u64) -> f64 {
        let from = now_slot.saturating_sub(self.window_slots);
        let count = self.busy.range((from, 0)..(now_slot, 0)).count() as f64;
        let total = (self.window_slots * u64::from(self.subchannels)) as f64;
        if total == 0.0 {
            return 0.0;
        }
        (count / total).clamp(0.0, 1.0)
    }

    /// CR at slot `n`: sub-channels used in `[n − a, n − 1]` plus granted in `[n, n + b]`
    /// over all sub-channels in the window.
    #[must_use]
    pub fn cr(&self, now_slot: u64) -> f64 {
        let from = now_slot.saturating_sub(self.cr_past_slots);
        let to = from + self.cr_window_slots;
        let count = self.used.range((from, 0)..(to, 0)).count() as f64;
        let total = (self.cr_window_slots * u64::from(self.subchannels)) as f64;
        if total == 0.0 {
            return 0.0;
        }
        (count / total).clamp(0.0, 1.0)
    }
}

/// `cr-limit-r1-1611594` — the illustrative CBR-to-CR-limit table of 04-models.md §5.1.
///
/// **Illustrative, not normative.** The real table is defined regionally (ETSI TS 103 574
/// in Europe) and 04-models.md §5.1 marks it UNVERIFIED; this one comes from a RAN1
/// contribution quoted in a survey [Qualcomm R1-1611594 via Mansouri 2019 Table III].
/// A model that installs it is tagged `illustrative` in its card.
pub const CR_LIMIT_R1_1611594: [(f64, f64); 10] = [
    (0.650, f64::INFINITY),
    (0.675, 1.6e-3),
    (0.700, 1.5e-3),
    (0.725, 1.4e-3),
    (0.750, 1.3e-3),
    (0.800, 1.2e-3),
    (0.825, 1.1e-3),
    (0.850, 1.0e-3),
    (0.875, 0.9e-3),
    (f64::INFINITY, 0.8e-3),
];

/// The CR limit the illustrative table gives at a CBR.
#[must_use]
pub fn cr_limit(cbr: f64) -> f64 {
    for (upper, limit) in CR_LIMIT_R1_1611594 {
        if cbr <= upper {
            return limit;
        }
    }
    0.8e-3
}

/// The RSRP thresholds LTE and NR admit, as the (index, dBm) pairs their IEs define.
///
/// LTE's `sl-ThresPSSCH-RSRP` list spans `[−128, −2]` dBm in 2 dB steps, 64 entries; the
/// algebraic form `P_th = −128 + 2·index` is *derived* in 04-models.md §5.1, not quoted.
/// NR's Rel-16 range is `(−112 + 2n) dBm, 0 ≤ n ≤ 45`, which is narrower and, as
/// 04-models.md §5.2 says in as many words, must not be conflated with LTE's.
#[must_use]
pub fn rsrp_threshold_dbm(rat: SlRat, index: u32) -> Option<f64> {
    match rat {
        SlRat::LteMode4 if index < 64 => Some(-128.0 + 2.0 * f64::from(index)),
        SlRat::NrMode2 if index <= 45 => Some(-112.0 + 2.0 * f64::from(index)),
        _ => None,
    }
}

/// `sl-TxPercentage-r16` / LTE's mandated `R_sel`: the fraction of candidate resources
/// that must survive the RSRP exclusion before the step-up stops.
///
/// LTE mandates 20 % [04-models.md §5.1 step 4]; NR configures `{p20, p35, p50}` per
/// priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TxPercentage {
    /// 20 %, the LTE-mandated value and the NR default.
    P20,
    /// 35 %, NR only.
    P35,
    /// 50 %, NR only.
    P50,
}

impl TxPercentage {
    /// The fraction itself.
    #[must_use]
    pub const fn fraction(self) -> f64 {
        match self {
            TxPercentage::P20 => 0.20,
            TxPercentage::P35 => 0.35,
            TxPercentage::P50 => 0.50,
        }
    }

    /// The label a scenario spells.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            TxPercentage::P20 => "p20",
            TxPercentage::P35 => "p35",
            TxPercentage::P50 => "p50",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nr_mcs_table_endpoints_match_the_design_document() {
        // 04-models.md §5.2 prints these two rows verbatim.
        let m0 = nr_mcs(0).expect("MCS 0 exists");
        assert_eq!((m0.qm, m0.r_1024), (2, 120));
        assert!((m0.spectral_efficiency() - 0.2344).abs() < 5e-5);
        let m28 = nr_mcs(28).expect("MCS 28 exists");
        assert_eq!((m28.qm, m28.r_1024), (6, 948));
        assert!((m28.spectral_efficiency() - 5.5547).abs() < 5e-5);
        assert!(nr_mcs(29).is_none());

        // Spectral efficiency rises monotonically *within* each modulation order, which
        // is what makes a mistyped row visible. It is not monotone across the whole
        // table: TS 38.214 Table 5.1.3.1-1 steps *down* from MCS 16 (16-QAM, SE 2.5703)
        // to MCS 17 (64-QAM, SE 2.5664), because the 64-QAM run restarts at a lower code
        // rate. Asserting global monotonicity would be asserting the table is something
        // it is not.
        let mut prev = (0u8, 0.0f64);
        for i in 0..=28u8 {
            let row = nr_mcs(i).expect("row exists");
            let se = row.spectral_efficiency();
            if row.qm == prev.0 {
                assert!(
                    se > prev.1,
                    "MCS {i} does not rise within its modulation order"
                );
            } else {
                assert!(row.qm > prev.0, "modulation order must not fall");
            }
            prev = (row.qm, se);
        }
        // The one printed cross-order dip, asserted so that removing it would fail here.
        assert!(
            nr_mcs(17).unwrap().spectral_efficiency() < nr_mcs(16).unwrap().spectral_efficiency()
        );
    }

    #[test]
    fn lte_presets_reproduce_the_printed_allocations() {
        // Molina-Masegosa: 190 B at QPSK r0.7 in 10 RB. With the 2-PRB adjacent PSCCH a
        // 12-PRB sub-channel leaves exactly 10 PRB for data.
        let pool = PoolConfig::molina_masegosa_highway();
        assert_eq!(pool.data_prb_for(1), 10);
        assert_eq!(pool.subchannels(), 4);
        let bits = pool.payload_bits(1);
        assert!(
            bits >= 190 * 8,
            "190 B must fit in one sub-channel, got {bits} bits"
        );
        assert_eq!(pool.subchannels_for(190), Some(1));

        // The same source's second mapping: 300 B at QPSK r0.5 in 20 RB, 22 allocated —
        // two 12-PRB sub-channels less the 2-PRB PSCCH.
        let pool_r05 = PoolConfig {
            mcs: LTE_QPSK_R050,
            ..PoolConfig::molina_masegosa_highway()
        };
        assert_eq!(pool_r05.data_prb_for(2), 22);
        assert_eq!(pool_r05.subchannels_for(300), Some(2));

        // Bazzi MCS 4: one 300 B beacon resource per TTI on 10-PRB sub-channels, i.e.
        // the whole 50 PRB.
        let mcs4 = PoolConfig::bazzi_10prb(LTE_MCS4_BAZZI);
        assert_eq!(mcs4.subchannels(), 5);
        assert_eq!(mcs4.subchannels_for(300), Some(5));

        // Bazzi MCS 7: two beacon resources per TTI, so 25 PRB each — which on 10-PRB
        // sub-channels is three sub-channels (28 PRB of data), the smallest allocation
        // that fits.
        let mcs7 = PoolConfig::bazzi_10prb(LTE_MCS7_BAZZI);
        let len = mcs7.subchannels_for(300).expect("300 B fits");
        assert_eq!(len, 3, "MCS 7 should need three 10-PRB sub-channels");
    }

    #[test]
    fn a_350_byte_nr_transport_block_needs_the_allocations_todisco_reports() {
        // Todisco Fig. 7 is MCS 21 (64-QAM, R 616/1024) and Fig. 8 is MCS 4
        // (QPSK, R 308/1024) — the high-rate one must fit in far fewer sub-channels.
        let hi = PoolConfig::todisco_nr(Numerology::Mu0, nr_mcs(21).unwrap());
        let lo = PoolConfig::todisco_nr(Numerology::Mu0, nr_mcs(4).unwrap());
        let hi_len = hi.subchannels_for(350).expect("MCS 21 fits 350 B");
        let lo_len = lo.subchannels_for(350).expect("MCS 4 fits 350 B");
        assert_eq!(hi_len, 1, "MCS 21 should need one sub-channel");
        assert!(
            lo_len > hi_len,
            "MCS 4 must need more sub-channels than MCS 21, got {lo_len} and {hi_len}"
        );
    }

    #[test]
    fn overlap_and_separation_agree_with_each_other() {
        let a = SlResource::new(10, 0, 2);
        assert!(a.overlaps(&SlResource::new(10, 1, 1)));
        assert!(!a.overlaps(&SlResource::new(11, 0, 2)));
        assert_eq!(a.separation(&SlResource::new(10, 1, 1)), Some(0));
        assert_eq!(a.separation(&SlResource::new(10, 2, 1)), Some(1));
        assert_eq!(a.separation(&SlResource::new(10, 3, 1)), Some(2));
        assert_eq!(a.separation(&SlResource::new(11, 3, 1)), None);
        // Separation is symmetric.
        let b = SlResource::new(10, 3, 1);
        assert_eq!(a.separation(&b), b.separation(&a));
    }

    #[test]
    fn the_ibe_mask_can_be_turned_off_and_is_monotone_when_on() {
        let on = IbeMask::todo_calibrate_default();
        assert_eq!(on.attenuation_db(0), 0.0);
        assert!(on.attenuation_db(1) < on.attenuation_db(2));
        assert!(on.attenuation_db(1) >= 30.0);
        assert!(on.attenuation_db(100) <= on.floor_db);
        assert!(IbeMask::OFF.attenuation_db(1).is_infinite());
        assert!(IbeMask::OFF.is_off());
        assert!(!on.is_off());
    }

    #[test]
    fn the_reselection_counter_ranges_are_the_ones_the_sources_print() {
        // LTE [Garcia 2021 §II.B].
        assert_eq!(Rri(100).c_resel_range(SlRat::LteMode4), (5, 15));
        assert_eq!(Rri(50).c_resel_range(SlRat::LteMode4), (10, 30));
        assert_eq!(Rri(20).c_resel_range(SlRat::LteMode4), (25, 75));
        // NR's C = 100/max(20, RRI) rule reproduces them [TS 38.321 §5.22.1].
        assert_eq!(Rri(100).c_resel_range(SlRat::NrMode2), (5, 15));
        assert_eq!(Rri(50).c_resel_range(SlRat::NrMode2), (10, 30));
        assert_eq!(Rri(20).c_resel_range(SlRat::NrMode2), (25, 75));
        // RRI 0 means no reservation, so one transmission.
        assert_eq!(Rri::NONE.c_resel_range(SlRat::LteMode4), (1, 1));
    }

    #[test]
    fn the_rri_lists_are_the_ones_each_release_admits() {
        assert!(Rri(100).is_lte_legal() && Rri(20).is_lte_legal());
        assert!(!Rri(30).is_lte_legal(), "30 ms is not an LTE RRI");
        assert!(Rri(30).is_nr_legal(), "NR admits 1..99 ms");
        assert!(!Rri(1100).is_nr_legal());
    }

    #[test]
    fn the_rsrp_threshold_ranges_differ_between_the_releases() {
        // 04-models.md §5.2: NR's range is narrower and must not be conflated.
        assert_eq!(rsrp_threshold_dbm(SlRat::LteMode4, 0), Some(-128.0));
        assert_eq!(rsrp_threshold_dbm(SlRat::LteMode4, 63), Some(-2.0));
        assert_eq!(rsrp_threshold_dbm(SlRat::LteMode4, 64), None);
        assert_eq!(rsrp_threshold_dbm(SlRat::NrMode2, 0), Some(-112.0));
        assert_eq!(rsrp_threshold_dbm(SlRat::NrMode2, 45), Some(-22.0));
        assert_eq!(rsrp_threshold_dbm(SlRat::NrMode2, 46), None);
        // −128 dBm, the Bazzi and Ali study value, is outside the Rel-16 list.
        assert!(
            !(0..=45).any(|n| rsrp_threshold_dbm(SlRat::NrMode2, n) == Some(-128.0)),
            "−128 dBm must not appear in the Rel-16 list"
        );
    }

    #[test]
    fn the_numerologies_carry_the_printed_processing_gaps() {
        assert_eq!(
            Numerology::ALL.map(|m| m.scs_khz()),
            [15, 30, 60, 120],
            "SCS is 15·2^µ"
        );
        assert_eq!(Numerology::ALL.map(|m| m.t_proc0_slots()), [1, 1, 2, 4]);
        assert_eq!(Numerology::ALL.map(|m| m.t_proc1_slots()), [3, 5, 9, 17]);
        // T_proc,1 in time: 3, 2.5, 2.25, 2.125 ms.
        let ms: Vec<f64> = Numerology::ALL
            .iter()
            .map(|m| f64::from(m.t_proc1_slots()) * m.slot().as_secs_f64() * 1e3)
            .collect();
        for (got, want) in ms.iter().zip([3.0, 2.5, 2.25, 2.125]) {
            assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
        }
        assert_eq!(Numerology::Mu0.slot(), Duration::from_millis(1));
        assert_eq!(Numerology::Mu2.slot(), Duration::from_micros(250));
    }

    #[test]
    fn the_pool_geometry_check_rejects_what_the_standards_do_not_admit() {
        let mut pool = PoolConfig::molina_masegosa_highway();
        assert!(pool.is_legal());
        pool.subchannel_prb = 13;
        assert!(!pool.is_legal(), "13 PRB is not in sizeSubchannel-r14");
        let mut nr = PoolConfig::todisco_nr(Numerology::Mu1, nr_mcs(21).unwrap());
        assert!(nr.is_legal());
        nr.mu = Numerology::Mu0;
        assert!(nr.is_legal(), "NR admits µ = 0");
        let mut lte = PoolConfig::molina_masegosa_highway();
        lte.mu = Numerology::Mu1;
        assert!(!lte.is_legal(), "LTE has only the 1 ms subframe");
    }

    #[test]
    fn the_occupancy_meter_counts_subchannels_not_busy_time() {
        let pool = PoolConfig::molina_masegosa_highway();
        let mut occ = SidelinkOccupancy::new(&pool);
        // Every sub-channel of every slot in the window busy: CBR 1.0.
        for slot in 0..100u64 {
            for sc in 0..pool.subchannels() {
                occ.note_busy(slot, sc);
            }
        }
        assert!((occ.cbr(100) - 1.0).abs() < 1e-12);
        // One sub-channel of one slot busy in a 100-slot, 4-sub-channel window.
        let mut sparse = SidelinkOccupancy::new(&pool);
        sparse.note_busy(50, 2);
        assert!((sparse.cbr(100) - 1.0 / 400.0).abs() < 1e-12);
        // CR counts this UE's own use over the 1,000-slot window.
        let mut cr = SidelinkOccupancy::new(&pool);
        for slot in (0..1000u64).step_by(100) {
            cr.note_used(slot, 0);
        }
        let v = cr.cr(500);
        assert!(v > 0.0 && v < 0.01, "CR {v} should be small but non-zero");
    }

    #[test]
    fn the_illustrative_cr_limit_table_is_monotone_and_starts_unlimited() {
        assert!(cr_limit(0.5).is_infinite());
        assert!(cr_limit(0.66) < cr_limit(0.6));
        assert!((cr_limit(0.66) - 1.6e-3).abs() < 1e-12);
        assert!((cr_limit(0.9) - 0.8e-3).abs() < 1e-12);
        let mut prev = f64::INFINITY;
        for i in 0..100 {
            let v = cr_limit(f64::from(i) / 100.0);
            assert!(v <= prev, "CR limit must not rise with CBR");
            prev = v;
        }
    }

    #[test]
    fn prob_resource_keep_admits_only_the_five_ie_values() {
        assert_eq!(
            ProbResourceKeep::from_probability(0.0),
            Some(ProbResourceKeep::ZERO)
        );
        assert_eq!(
            ProbResourceKeep::from_probability(0.4).map(ProbResourceKeep::probability),
            Some(0.4)
        );
        assert!(ProbResourceKeep::from_probability(0.3).is_none());
        assert!(ProbResourceKeep::from_probability(1.0).is_none());
        for p in ProbResourceKeep::ALL {
            assert!((0.0..=0.8).contains(&p.probability()));
        }
    }
}
