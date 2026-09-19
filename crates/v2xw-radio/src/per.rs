//! `phy/80211p/nist-per` — the NIST OFDM packet-error-rate model of 04-models.md §4.7.
//!
//! Pei and Henderson's re-derivation of the ns-3 OFDM error model from Miller's NIST BER
//! equations, validated within about 1 dB against the CMU wireless-emulation testbed
//! (the older Yans model was 8-10 dB too optimistic) [Pei and Henderson 2010 §II-III,
//! R1 §C.1]. Three stages:
//!
//! 1. **Uncoded bit-error probability** from the modulation and the SNR
//!    ([`uncoded_ber`]). 04-models.md §4.7 writes these in terms of `Eb/N0`:
//!    `BPSK p = Q(sqrt(2·F·Eb/N0))`, QPSK the same form, `16-QAM p = (3/4)·Q(sqrt(F·(4/5)·Eb/N0))`,
//!    `64-QAM p = (7/12)·Q(sqrt(F·(6/21)·Eb/N0))` with `F = (4/5)·(48/52)`. This module
//!    is written in the equivalent received-SNR form — `BPSK 0.5·erfc(sqrt(SNR))`,
//!    `QPSK 0.5·erfc(sqrt(SNR/2))`, `16-QAM (3/4)·0.5·erfc(sqrt(SNR/10))`,
//!    `64-QAM (7/12)·0.5·erfc(sqrt(SNR/42))` — which is the same set of curves with
//!    `SNR = k·F·Eb/N0` for `k` bits per subcarrier, and is the form the cited
//!    reproduction evaluates, so the SNR that comes out of the link budget can be passed
//!    in without a bandwidth-to-bitrate conversion that would differ per MCS.
//! 2. **Coded error probability** ([`coded_error_probability`]): the Chernoff-bound union
//!    sum over the convolutional code's distance spectrum, one polynomial in
//!    `D = sqrt(4p(1-p))` per code rate.
//! 3. **Frame error** ([`PerModel::per`]):
//!    `PER = 1 − (1 − Pe_SIGNAL)^24 · (1 − Pe_DATA)^(N_SERVICE + 8·bytes + N_TAIL)`,
//!    with the SIGNAL field always BPSK 1/2.
//!
//! # What is cited and what is not
//!
//! The BER equations and the PER composition are cited to Pei and Henderson 2010
//! Table I and Eq. 2 through 04-models.md §4.7. The **distance-spectrum coefficients**
//! of stage 2 are not printed in the design document: they live in Miller's NIST report
//! Tables 3.1.1-3.1.3 and are transcribed here from the reference implementation the
//! document names (`nist_per.py`, the same polynomials ns-3's `NistErrorRateModel`
//! evaluates). They are therefore recorded with `SourceKind::Code` and are UNVERIFIED at
//! the primary level. What makes them trustworthy anyway is that they are
//! over-determined: the model has to reproduce all 24 entries of §4.7's SNR-at-target-PER
//! table, and `reproduces_the_snr_at_10_percent_per_table` checks every one of them to a
//! tenth of a dB. A wrong coefficient would show up there immediately.
//!
//! # `rx_impl_loss_db`
//!
//! Default 0 dB: the model is standards-ideal, and the ideal-receiver assumption is the
//! whole reason it sits about 5 dB below measured hardware (Sjöberg et al. put 10 % PER
//! at 6 Mbit/s and 300 B near 11.5 dB against this model's 6.6 dB at 400 B). The
//! `sjoberg-atheros` preset adds that 5 dB, which is the number to use for a study that
//! wants to match a real chipset rather than 802.11a/g theory.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::math;
use v2xw_core::model::Model;

use crate::numeric;
use crate::types::{CodeRate, Mcs, Modulation, timing};

/// The bits the DATA field of a frame carries, including the SERVICE and tail bits:
/// `N_SERVICE + 8·bytes + N_TAIL` (04-models.md §4.2, §4.7).
#[must_use]
pub const fn data_field_bits(bytes: u32) -> u64 {
    timing::N_SERVICE as u64 + 8 * bytes as u64 + timing::N_TAIL as u64
}

/// The uncoded bit-error probability for one modulation at a linear SNR.
///
/// [Pei and Henderson 2010 Table I, via 04-models.md §4.7]. `snr` is a linear power
/// ratio, not dB.
#[must_use]
pub fn uncoded_ber(modulation: Modulation, snr: f64) -> f64 {
    if snr.is_nan() || snr <= 0.0 {
        // No signal: a coin flip per bit for BPSK, and worse for the larger
        // constellations, but 0.5 is the ceiling the polynomials below are stable at.
        return 0.5;
    }
    match modulation {
        Modulation::Bpsk => 0.5 * numeric::erfc(math::sqrt(snr)),
        Modulation::Qpsk => 0.5 * numeric::erfc(math::sqrt(snr / 2.0)),
        Modulation::Qam16 => 0.75 * 0.5 * numeric::erfc(math::sqrt(snr / 10.0)),
        Modulation::Qam64 => (7.0 / 12.0) * 0.5 * numeric::erfc(math::sqrt(snr / 42.0)),
    }
}

/// The coded error probability: the Chernoff-bound union sum over the convolutional
/// code's distance spectrum.
///
/// `p` is the uncoded bit-error probability of [`uncoded_ber`]. The polynomials are in
/// `D = sqrt(4p(1-p))`, one per code rate, with the leading factor `1/(2·bValue)` of the
/// bound (`bValue` is 1, 2 and 3 for rates 1/2, 2/3 and 3/4). Coefficients as the module
/// docs record: Miller's NIST report Tables 3.1.1-3.1.3, transcribed from the cited
/// reference implementation.
///
/// The result is clamped to `[0, 1]`: a union bound is an upper bound and exceeds one
/// long before the link is usable.
#[must_use]
pub fn coded_error_probability(p: f64, rate: CodeRate) -> f64 {
    if p.is_nan() || p <= 0.0 {
        return 0.0;
    }
    let d = math::sqrt(4.0 * p * (1.0 - p));
    // Powers are accumulated in increasing exponent, so the sum is ordered by
    // construction and two builds cannot disagree about its last bit.
    let terms: &[(f64, i32)] = match rate {
        CodeRate::R1_2 => &[
            (36.0, 10),
            (211.0, 12),
            (1_404.0, 14),
            (11_633.0, 16),
            (77_433.0, 18),
            (502_690.0, 20),
            (3_322_763.0, 22),
            (21_292_910.0, 24),
            (134_365_911.0, 26),
        ],
        CodeRate::R2_3 => &[
            (3.0, 6),
            (70.0, 7),
            (285.0, 8),
            (1_276.0, 9),
            (6_160.0, 10),
            (27_128.0, 11),
            (117_019.0, 12),
            (498_860.0, 13),
            (2_103_891.0, 14),
            (8_784_123.0, 15),
        ],
        CodeRate::R3_4 => &[
            (42.0, 5),
            (201.0, 6),
            (1_492.0, 7),
            (10_469.0, 8),
            (62_935.0, 9),
            (379_644.0, 10),
            (2_253_373.0, 11),
            (13_073_811.0, 12),
            (75_152_755.0, 13),
            (428_005_675.0, 14),
        ],
    };
    let sum = math::sum_ordered(
        terms
            .iter()
            .map(|(coeff, exp)| coeff * math::pow(d, f64::from(*exp))),
    );
    let pe = sum / (2.0 * f64::from(rate.b_value()));
    pe.clamp(0.0, 1.0)
}

/// The NIST OFDM error model, `phy/80211p/nist-per`.
///
/// Stateless and drawless: it turns an SNR into a probability. The Bernoulli draw that
/// turns that probability into a frame outcome belongs to the PHY
/// ([`crate::phy::OfdmPhy`]), which owns the RNG stream it comes from.
#[derive(Debug, Clone)]
pub struct PerModel {
    card: ModelCard,
    rx_impl_loss_db: f64,
}

/// The `rx_impl_loss_db` presets of 04-models.md §4.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PerPreset {
    /// 0 dB: the standards-ideal model, the default.
    Ideal,
    /// 5 dB: the gap between this model and the Sjöberg et al. Atheros measurement
    /// [R1 §C.2].
    SjobergAtheros,
}

impl PerPreset {
    /// The implementation loss this preset adds to the required SNR, dB.
    #[must_use]
    pub const fn rx_impl_loss_db(self) -> f64 {
        match self {
            PerPreset::Ideal => 0.0,
            PerPreset::SjobergAtheros => 5.0,
        }
    }

    /// The preset's id as a scenario spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PerPreset::Ideal => "ideal",
            PerPreset::SjobergAtheros => "sjoberg-atheros",
        }
    }
}

impl Default for PerModel {
    fn default() -> Self {
        Self::new(PerPreset::Ideal)
    }
}

impl PerModel {
    /// The model's id.
    pub const ID: &'static str = "phy/80211p/nist-per";

    /// The model with one of the two cited implementation-loss presets.
    #[must_use]
    pub fn new(preset: PerPreset) -> Self {
        Self {
            card: per_card(preset),
            rx_impl_loss_db: preset.rx_impl_loss_db(),
        }
    }

    /// The model with an arbitrary implementation loss, for a study calibrating against
    /// its own hardware.
    #[must_use]
    pub fn with_impl_loss_db(rx_impl_loss_db: f64) -> Self {
        let mut card = per_card(PerPreset::Ideal);
        card.validation = Validation::new(ValidationStatus::Unvalidated);
        if let Some(p) = card
            .parameters
            .iter_mut()
            .find(|p| p.name == "rx_impl_loss_db")
        {
            p.default = serde_json::json!(rx_impl_loss_db);
            p.source = Source::todo_calibrate("caller-supplied implementation loss");
            p.calibration = Some(
                "Measure 10 % PER against the target chipset in a cabled AWGN setup, as \
                 Sjöberg et al. did, and record the offset from this model."
                    .to_string(),
            );
        }
        Self {
            card,
            rx_impl_loss_db,
        }
    }

    /// The implementation loss, dB, that this instance adds to the required SNR.
    #[must_use]
    pub const fn rx_impl_loss_db(&self) -> f64 {
        self.rx_impl_loss_db
    }

    /// The coded bit-error probability of the DATA field at an SINR, after the
    /// implementation loss.
    #[must_use]
    pub fn pe_data(&self, mcs: Mcs, sinr_db: f64) -> f64 {
        let snr = numeric::db_to_linear(sinr_db - self.rx_impl_loss_db);
        coded_error_probability(uncoded_ber(mcs.modulation(), snr), mcs.code_rate())
    }

    /// The coded bit-error probability of the SIGNAL field at an SINR.
    ///
    /// The SIGNAL field is always BPSK 1/2, whatever the DATA field's MCS is
    /// [EN 302 663 Annex C.3], which is why it has its own evaluation and why a frame at
    /// 27 Mbit/s and one at 3 Mbit/s lose their headers at the same SNR.
    #[must_use]
    pub fn pe_signal(&self, sinr_db: f64) -> f64 {
        let snr = numeric::db_to_linear(sinr_db - self.rx_impl_loss_db);
        coded_error_probability(uncoded_ber(Modulation::Bpsk, snr), CodeRate::R1_2)
    }

    /// The probability that `bits` bits survive at one SINR: `(1 − Pe)^bits`.
    ///
    /// `bits` is an `f64` because the frame-level model splits a frame into SINR windows
    /// and apportions its bits across them by time, which does not land on integers
    /// (04-models.md §4.8: "per symbol-group SINR windows over the arrival set").
    #[must_use]
    pub fn survival(pe: f64, bits: f64) -> f64 {
        if bits <= 0.0 {
            return 1.0;
        }
        math::pow((1.0 - pe).clamp(0.0, 1.0), bits)
    }

    /// The probability that one frame of `bytes` at `mcs` is received in error at an
    /// SINR of `sinr_db`.
    ///
    /// `rx_impl_loss_db` is subtracted from the SINR before the BER is evaluated, which
    /// is the same thing as adding it to the required SNR.
    #[must_use]
    pub fn per(&self, bytes: u32, mcs: Mcs, sinr_db: f64) -> f64 {
        let psr_signal = Self::survival(self.pe_signal(sinr_db), f64::from(timing::SIGNAL_BITS));
        let psr_data = Self::survival(self.pe_data(mcs, sinr_db), data_field_bits(bytes) as f64);
        (1.0 - psr_signal * psr_data).clamp(0.0, 1.0)
    }

    /// The SNR, dB, at which a frame of `bytes` at `mcs` reaches exactly `target_per`.
    ///
    /// Bisection on `[-40, 80]` dB with a fixed 96 iterations, so the answer is a
    /// deterministic function of its inputs with no convergence tolerance to tune: 96
    /// halvings of 120 dB is far below the last bit of an `f64` in that range, and the
    /// loop cost is irrelevant because this is a reporting and calibration routine, not
    /// a per-frame one. [`PerModel::per`] is monotone non-increasing in SNR, which
    /// `per_is_monotone_in_snr` asserts, so the bisection is well posed.
    #[must_use]
    pub fn snr_for_per(&self, bytes: u32, mcs: Mcs, target_per: f64) -> f64 {
        let mut lo = -40.0_f64;
        let mut hi = 80.0_f64;
        for _ in 0..96 {
            let mid = 0.5 * (lo + hi);
            if self.per(bytes, mcs, mid) > target_per {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }
}

impl Model for PerModel {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

/// The card for one implementation-loss preset.
fn per_card(preset: PerPreset) -> ModelCard {
    let pei = Source {
        kind: SourceKind::Paper,
        reference: "G. Pei and T. R. Henderson, \"Validation of OFDM error rate model in \
                    ns-3\", 2010, Table I and Eq. 2 (04-models.md §4.7, R1 §C.1)"
            .to_string(),
        accessed: None,
        note: Some(
            "Re-derivation of the ns-3 OFDM error model from L. E. Miller, \"Validation of \
             802.11a/UWB Coexistence Simulation\", NIST, 2003. Validated within about 1 dB \
             against the CMU wireless-emulation testbed."
                .to_string(),
        ),
    };
    let nist_code = Source {
        kind: SourceKind::Code,
        reference: "nist_per.py (the reproduction 04-models.md §4.7 names) / ns-3 \
                    NistErrorRateModel::CalculatePe, distance spectra from Miller NIST \
                    report Tables 3.1.1-3.1.3"
            .to_string(),
        accessed: None,
        note: Some(
            "UNVERIFIED at the primary level: the design document prints the BER equations \
             but not the distance-spectrum coefficients. They are over-determined by \
             §4.7's 24-entry SNR-at-target-PER table, which the crate's tests reproduce to \
             0.1 dB."
                .to_string(),
        ),
    };
    let en302663 = Source::new(
        SourceKind::Standard,
        "ETSI EN 302 663 V1.3.1 Annex C.3 (SIGNAL field: 24 bits, BPSK 1/2; N_SERVICE and \
         N_TAIL per 04-models.md §4.2, clause UNVERIFIED)",
    );

    let mut card = ModelCard::new(
        PerModel::ID,
        Family::Phy,
        "1.0.0",
        "NIST OFDM packet-error-rate model: uncoded BER per modulation, the convolutional \
         code's union bound, and the SIGNAL-plus-DATA frame-error composition of \
         04-models.md §4.7.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "uncoded BER".to_string(),
            latex_or_text: "BPSK: p = Q(sqrt(2·F·Eb/N0)) = 0.5·erfc(sqrt(SNR)); QPSK: \
                            0.5·erfc(sqrt(SNR/2)); 16-QAM: (3/4)·0.5·erfc(sqrt(SNR/10)); \
                            64-QAM: (7/12)·0.5·erfc(sqrt(SNR/42)); F = (4/5)·(48/52)"
                .to_string(),
            notes: Some(
                "The Eb/N0 form of 04-models.md §4.7 and the received-SNR form implemented \
                 here are the same curves under SNR = k·F·Eb/N0 for k bits per subcarrier."
                    .to_string(),
            ),
        },
        Equation {
            name: "coded error probability".to_string(),
            latex_or_text: "Pe = (1/(2·b))·Σ a_d · D^d, D = sqrt(4p(1−p)), b = 1, 2, 3 for \
                            rates 1/2, 2/3, 3/4"
                .to_string(),
            notes: Some("Chernoff-bound union sum over the code's distance spectrum.".to_string()),
        },
        Equation {
            name: "packet error rate".to_string(),
            latex_or_text: "PER = 1 − (1 − Pe_SIGNAL)^24 · (1 − Pe_DATA)^(N_SERVICE + 8·bytes \
                            + N_TAIL)"
                .to_string(),
            notes: None,
        },
    ];
    card.parameters = vec![
        Parameter {
            name: "rx_impl_loss_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(preset.rx_impl_loss_db()),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(20.0)]),
            source: match preset {
                PerPreset::Ideal => Source {
                    kind: SourceKind::Paper,
                    reference: "04-models.md §4.7: default 0 dB, standards-ideal (source: \
                                model)"
                        .to_string(),
                    accessed: None,
                    note: Some(
                        "The ideal-receiver assumption is why this model sits about 5 dB \
                         below measured hardware."
                            .to_string(),
                    ),
                },
                PerPreset::SjobergAtheros => Source {
                    kind: SourceKind::Paper,
                    reference: "K. Sjöberg et al., \"Measuring and Using the RSSI of IEEE \
                                802.11p\" (R1 §C.2): 10 % PER at 6 Mbit/s measured near \
                                11.5 dB for 300 B against this model's 6.6 dB at 400 B"
                        .to_string(),
                    accessed: None,
                    note: Some(
                        "Digitized from Fig. 5 at ±0.3 dB; the 5 dB preset is the gap, \
                         literature-checked."
                            .to_string(),
                    ),
                },
            },
            calibration: None,
        },
        Parameter {
            name: "n_service_bits".to_string(),
            unit: "bit".to_string(),
            default: serde_json::json!(timing::N_SERVICE),
            range: None,
            source: en302663.clone(),
            calibration: None,
        },
        Parameter {
            name: "n_tail_bits".to_string(),
            unit: "bit".to_string(),
            default: serde_json::json!(timing::N_TAIL),
            range: None,
            source: en302663.clone(),
            calibration: None,
        },
        Parameter {
            name: "signal_field_bits".to_string(),
            unit: "bit".to_string(),
            default: serde_json::json!(timing::SIGNAL_BITS),
            range: None,
            source: en302663,
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "Additive white Gaussian noise: the model gives the PER for a given instantaneous \
         SINR, and the fading and shadowing models decide what SINR trace is fed into it."
            .to_string(),
        "Ideal coherent detection with perfect channel estimation and synchronisation, \
         corrected only by rx_impl_loss_db."
            .to_string(),
        "The BER equations depend on modulation and coding only, so the 802.11a/g \
         constants apply unchanged to the 10 MHz rates (derived reasoning, consistent with \
         EN 302 663 Table C.1)."
            .to_string(),
    ];
    card.limitations = vec![
        "Predicts about 5 dB better than a real 802.11p chipset at 6 Mbit/s (Sjöberg et \
         al.); use the sjoberg-atheros preset to match hardware."
            .to_string(),
        "The union bound is an upper bound on the coded error probability, so the PER is \
         pessimistic where the bound is loose (very low SNR); it is clamped to 1."
            .to_string(),
        "No frequency-selective fading and no Doppler: a flat SINR over the frame.".to_string(),
    ];
    card.sources = vec![pei, nist_code];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §4.7 model-output table: SNR at 10 % PER for 200 / 400 / 1000 B \
             and at 1 % PER for 400 B, all eight MCS",
        )],
        tests: vec![
            "reproduces_the_snr_at_10_percent_per_table".to_string(),
            "reproduces_the_snr_at_1_percent_per_column".to_string(),
            "per_is_monotone_in_snr".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.ignores = vec![
        "Frequency selectivity within the 10 MHz channel and the Doppler-dependent \
         channel-estimation loss, which the implementation-loss offset partly absorbs \
         (04-models.md §3 tier table)."
            .to_string(),
    ];
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 04-models.md §4.7's model-output table: SNR in dB for 10 % PER at 200, 400 and
    /// 1,000 B, and for 1 % PER at 400 B.
    const TABLE_10PCT: [(Mcs, [f64; 3]); 8] = [
        (Mcs::R3Bpsk12, [3.4, 3.6, 3.8]),
        (Mcs::R4p5Bpsk34, [6.2, 6.5, 6.7]),
        (Mcs::R6Qpsk12, [6.4, 6.6, 6.9]),
        (Mcs::R9Qpsk34, [9.3, 9.5, 9.7]),
        (Mcs::R12Qam16_12, [12.9, 13.1, 13.4]),
        (Mcs::R18Qam16_34, [16.0, 16.2, 16.5]),
        (Mcs::R24Qam64_23, [20.7, 20.9, 21.2]),
        (Mcs::R27Qam64_34, [21.9, 22.2, 22.5]),
    ];

    const TABLE_1PCT_400B: [(Mcs, f64); 8] = [
        (Mcs::R3Bpsk12, 4.2),
        (Mcs::R4p5Bpsk34, 7.2),
        (Mcs::R6Qpsk12, 7.3),
        (Mcs::R9Qpsk34, 10.2),
        (Mcs::R12Qam16_12, 13.8),
        (Mcs::R18Qam16_34, 16.9),
        (Mcs::R24Qam64_23, 21.7),
        (Mcs::R27Qam64_34, 23.0),
    ];

    #[test]
    fn reproduces_the_snr_at_10_percent_per_table() {
        let m = PerModel::default();
        for (mcs, expected) in TABLE_10PCT {
            for (bytes, want) in [200u32, 400, 1000].into_iter().zip(expected) {
                let got = m.snr_for_per(bytes, mcs, 0.1);
                assert!(
                    (got - want).abs() <= 0.1,
                    "{mcs} at {bytes} B: model {got:.3} dB, document {want} dB"
                );
            }
        }
    }

    #[test]
    fn reproduces_the_snr_at_1_percent_per_column() {
        let m = PerModel::default();
        for (mcs, want) in TABLE_1PCT_400B {
            let got = m.snr_for_per(400, mcs, 0.01);
            assert!(
                (got - want).abs() <= 0.1,
                "{mcs} at 400 B, 1 % PER: model {got:.3} dB, document {want} dB"
            );
        }
    }

    #[test]
    fn the_two_hand_checked_entries_hold_exactly() {
        // The two numbers the task states: 3 Mbit/s about 3.6 dB and 6 Mbit/s about
        // 6.6 dB, both at 400 bytes.
        let m = PerModel::default();
        assert!((m.snr_for_per(400, Mcs::R3Bpsk12, 0.1) - 3.6).abs() <= 0.1);
        assert!((m.snr_for_per(400, Mcs::R6Qpsk12, 0.1) - 6.6).abs() <= 0.1);
        // And the PER at those SNRs really is about 10 %.
        assert!((m.per(400, Mcs::R3Bpsk12, 3.6) - 0.1).abs() < 0.01);
        assert!((m.per(400, Mcs::R6Qpsk12, 6.6) - 0.1).abs() < 0.01);
    }

    #[test]
    fn qpsk_needs_three_db_more_than_bpsk_at_the_same_code_rate() {
        // A property of the equations rather than of the table: QPSK carries two bits per
        // subcarrier, so its DATA-field BER curve is the BPSK curve shifted by
        // 10·log10(2) = 3.0103 dB. The shift of the *frame* curve is a hair smaller,
        // because the 24-bit SIGNAL field is BPSK 1/2 in both cases and therefore enjoys
        // the QPSK frame's higher SNR without paying for it. 2 millidecibels of
        // asymmetry is the size of that effect, and it is worth pinning: a bug that
        // evaluated the SIGNAL field at the DATA field's modulation would move it.
        let m = PerModel::default();
        let bpsk = m.snr_for_per(400, Mcs::R3Bpsk12, 0.1);
        let qpsk = m.snr_for_per(400, Mcs::R6Qpsk12, 0.1);
        let shift = qpsk - bpsk;
        assert!(
            (shift - 3.010_299_956_639_812).abs() < 0.005,
            "{qpsk} - {bpsk} = {shift}"
        );
        assert!(
            shift < 3.010_299_956_639_812,
            "the SIGNAL field helps QPSK: {shift}"
        );
    }

    #[test]
    fn per_is_monotone_in_snr() {
        let m = PerModel::default();
        for mcs in Mcs::ALL {
            let mut previous = 2.0;
            let mut snr = -10.0;
            while snr <= 40.0 {
                let per = m.per(400, mcs, snr);
                assert!(
                    per <= previous + 1e-12,
                    "{mcs}: PER rose from {previous} to {per} at {snr} dB"
                );
                assert!((0.0..=1.0).contains(&per), "{mcs}: PER {per} at {snr} dB");
                previous = per;
                snr += 0.25;
            }
            // Deep in the noise every frame is lost; far above it none is.
            assert!(m.per(400, mcs, -20.0) > 0.999_9, "{mcs}");
            assert!(m.per(400, mcs, 45.0) < 1e-9, "{mcs}");
        }
    }

    #[test]
    fn a_longer_frame_needs_more_snr() {
        let m = PerModel::default();
        for mcs in Mcs::ALL {
            let s200 = m.snr_for_per(200, mcs, 0.1);
            let s400 = m.snr_for_per(400, mcs, 0.1);
            let s1000 = m.snr_for_per(1000, mcs, 0.1);
            assert!(s200 < s400 && s400 < s1000, "{mcs}: {s200} {s400} {s1000}");
        }
    }

    #[test]
    fn the_sjoberg_preset_shifts_the_curve_by_five_db() {
        let ideal = PerModel::new(PerPreset::Ideal);
        let atheros = PerModel::new(PerPreset::SjobergAtheros);
        let a = ideal.snr_for_per(400, Mcs::R6Qpsk12, 0.1);
        let b = atheros.snr_for_per(400, Mcs::R6Qpsk12, 0.1);
        assert!((b - a - 5.0).abs() < 1e-9, "{a} -> {b}");
        // Which lands on the measured figure: about 11.5-11.7 dB at 300 B.
        assert!((11.0..12.0).contains(&b), "{b}");
    }

    /// Prints the model's own SNR-at-10 %-PER table beside 04-models.md §4.7's, which is
    /// the validation deliverable of this crate. Run it with
    /// `cargo test -p v2xw-radio measured_snr -- --nocapture`.
    #[test]
    fn the_measured_snr_table_is_reported() {
        let m = PerModel::default();
        println!(
            "\n{:<16} | {:>21} | {:>21} | {:>21}",
            "MCS", "200 B model/doc", "400 B model/doc", "1000 B model/doc"
        );
        for (mcs, documented) in TABLE_10PCT {
            let mut cells = Vec::new();
            for (bytes, want) in [200u32, 400, 1000].into_iter().zip(documented) {
                let got = m.snr_for_per(bytes, mcs, 0.1);
                cells.push(format!("{got:>8.3} / {want:>5.1} dB"));
            }
            println!(
                "{:<16} | {} | {} | {}",
                mcs.label(),
                cells[0],
                cells[1],
                cells[2]
            );
        }
        // Also the 1 % column, and the deltas, so a reader can see the worst case.
        let mut worst = 0.0_f64;
        for (mcs, documented) in TABLE_10PCT {
            for (bytes, want) in [200u32, 400, 1000].into_iter().zip(documented) {
                worst = worst.max((m.snr_for_per(bytes, mcs, 0.1) - want).abs());
            }
        }
        for (mcs, want) in TABLE_1PCT_400B {
            worst = worst.max((m.snr_for_per(400, mcs, 0.01) - want).abs());
        }
        println!("worst absolute deviation from the document: {worst:.3} dB");
        assert!(worst <= 0.1, "{worst}");
    }

    #[test]
    fn data_field_bits_counts_service_and_tail() {
        assert_eq!(data_field_bits(400), 16 + 3_200 + 6);
        assert_eq!(data_field_bits(0), 22);
    }

    #[test]
    fn the_card_validates_and_declares_every_parameter_the_model_reads() {
        for preset in [PerPreset::Ideal, PerPreset::SjobergAtheros] {
            let m = PerModel::new(preset);
            m.card().validate().expect("card validates");
            m.card().check_api_version().expect("api version matches");
            assert!(
                m.card()
                    .parameters
                    .iter()
                    .any(|p| p.name == "rx_impl_loss_db")
            );
            assert_eq!(m.card().id, PerModel::ID);
        }
        PerModel::with_impl_loss_db(3.0)
            .card()
            .validate()
            .expect("a caller-supplied loss still validates (rule R1: it has a plan)");
    }
}
