//! The C-V2X error models: `phy/lte-v2x/bler-lut-r1-160284`, `phy/cv2x/bler-lut-wilab`
//! and the spectral-efficiency fit that covers the MCS neither of them prints
//! (04-models.md §5.1).
//!
//! # Three models, in decreasing order of how much of them is printed
//!
//! 1. [`BlerCurve::r1_160284_qpsk_r070`] and [`BlerCurve::r1_160284_qpsk_r050`] are the
//!    two verbatim BLER-versus-SNR lookups of 04-models.md §5.1: 190 B at 280 km/h
//!    relative speed, from a Huawei RAN1 contribution through the Gonzalez-Martin
//!    reproduction. Every printed point is in the table, and
//!    `the_r1_160284_lut_reproduces_every_printed_point` checks each one.
//! 2. [`WILAB_LTE_SINR_AT_10PC`] is the WiLabV2Xsim table of *SINR at 10 % PER* per
//!    `(scenario, MCS, transport-block size)`. It is a set of operating points, not
//!    curves, so a model that uses it needs a shape to hang them on — see below.
//! 3. [`SeGapFit`] covers the rest. 04-models.md §5.1 says in as many words that "curves
//!    for MCS 10 and 20 and for 300 B at LTE numerology were not found (UNVERIFIED)", and
//!    §5.2 prints no NR curve at all, while §5.5 requires an NR MCS 21 validation run.
//!    The fit is the honest way across that gap: the four *cited* 10 % operating points
//!    are regressed against their own spectral efficiency, and the one fitted number —
//!    the implementation gap above the Shannon limit — is reported with its residuals.
//!    Any threshold it produces is `todo-calibrate`, and
//!    `the_spectral_efficiency_fit_reports_its_own_residuals` prints them.
//!
//! # Why a curve can be shifted but not reshaped
//!
//! An operating point fixes *where* a curve sits, not how steep it is. Every model here
//! that has a point but no curve borrows the shape of the QPSK r0.7 lookup — the one
//! curve in 04-models.md §5.1 with seven printed points spanning BLER 0.9 to 0.007 — and
//! shifts it in SNR until its 10 % crossing lands on the operating point
//! ([`BlerCurve::with_ten_percent_at`]). The shape is therefore `todo-calibrate` wherever
//! it is borrowed, and a model that borrows it registers `unvalidated`. The calibration
//! plan is the one 04-models.md §5.1 already writes down: regenerate the curves from a
//! link-level simulator under the §3.3 channel models.

use serde::Serialize;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::math;
use v2xw_core::model::Model;

use crate::sidelink::SlMcsSpec;

/// A block-error-rate curve: BLER against SNR or SINR in dB.
///
/// Held as the points the source prints, in ascending SNR. Between them the *logarithm*
/// of the BLER is interpolated linearly, which is how every BLER curve is read off a log
/// plot and what keeps the 10 % crossing where the source's own interpolation puts it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlerCurve {
    /// The curve's id, for reports.
    pub label: String,
    /// `(snr_db, bler)` in ascending SNR. BLER is clamped to `[1e-12, 1.0]`.
    points: Vec<(f64, f64)>,
}

impl BlerCurve {
    /// A curve from its printed points. They are sorted and the BLER clamped, so a
    /// caller cannot build a curve this module's arithmetic would divide by zero on.
    #[must_use]
    pub fn new(label: impl Into<String>, mut points: Vec<(f64, f64)>) -> Self {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        for p in &mut points {
            p.1 = p.1.clamp(1e-12, 1.0);
        }
        Self {
            label: label.into(),
            points,
        }
    }

    /// The printed points.
    #[must_use]
    pub fn points(&self) -> &[(f64, f64)] {
        &self.points
    }

    /// `phy/lte-v2x/bler-lut-r1-160284`, QPSK r0.7, 190 B, 280 km/h relative speed.
    ///
    /// Verbatim from 04-models.md §5.1: `0 → 1; 2 → 0.9; 4 → 0.7; 6 → 0.4; 8 → 0.13;
    /// 10 → 0.045; 12 → 0.017; 14 → 0.007; 16, 18, 20 → 1e−3`
    /// [Huawei R1-160284 via Gonzalez-Martin 2019 `get_BLER.m`].
    #[must_use]
    pub fn r1_160284_qpsk_r070() -> Self {
        Self::new(
            "r1-160284-qpsk-r0.7",
            vec![
                (0.0, 1.0),
                (2.0, 0.9),
                (4.0, 0.7),
                (6.0, 0.4),
                (8.0, 0.13),
                (10.0, 0.045),
                (12.0, 0.017),
                (14.0, 0.007),
                (16.0, 1e-3),
                (18.0, 1e-3),
                (20.0, 1e-3),
            ],
        )
    }

    /// `phy/lte-v2x/bler-lut-r1-160284`, QPSK r0.5, 190 B.
    ///
    /// Verbatim: `−2 → 1; 0 → 0.9; 2 → 0.7; 4 → 0.3; 6 → 0.09; 8 → 0.02; 10 → 0.002;
    /// 12, 14 → 1e−3`.
    #[must_use]
    pub fn r1_160284_qpsk_r050() -> Self {
        Self::new(
            "r1-160284-qpsk-r0.5",
            vec![
                (-2.0, 1.0),
                (0.0, 0.9),
                (2.0, 0.7),
                (4.0, 0.3),
                (6.0, 0.09),
                (8.0, 0.02),
                (10.0, 0.002),
                (12.0, 1e-3),
                (14.0, 1e-3),
            ],
        )
    }

    /// The BLER at an SNR, with the BLER interpolated in log space.
    ///
    /// Below the first printed point the curve is 1.0 (nothing decodes); above the last
    /// it holds the last printed value, which for both shipped lookups is the 1e−3 floor
    /// the source's own table stops at. Extrapolating the slope past the floor would be
    /// inventing a curve, so it does not.
    #[must_use]
    pub fn bler(&self, snr_db: f64) -> f64 {
        if self.points.is_empty() || snr_db.is_nan() {
            return 1.0;
        }
        let first = self.points[0];
        if snr_db <= first.0 {
            return first.1;
        }
        let last = self.points[self.points.len() - 1];
        if snr_db >= last.0 {
            return last.1;
        }
        for w in self.points.windows(2) {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            if snr_db >= x0 && snr_db <= x1 {
                if (x1 - x0).abs() < 1e-12 {
                    return y1;
                }
                let t = (snr_db - x0) / (x1 - x0);
                let l0 = math::log10(y0);
                let l1 = math::log10(y1);
                return math::pow(10.0, l0 + t * (l1 - l0)).clamp(1e-12, 1.0);
            }
        }
        last.1
    }

    /// The SNR at which the curve crosses `target`, by the same log-space interpolation.
    ///
    /// `None` when the curve never reaches the target — a curve floored at 1e−3 has no
    /// 1e−4 crossing, and inventing one is exactly what this returns `None` instead of
    /// doing.
    #[must_use]
    pub fn snr_at_bler(&self, target: f64) -> Option<f64> {
        if self.points.len() < 2 {
            return None;
        }
        for w in self.points.windows(2) {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            // The curve falls, so the crossing is where y0 >= target >= y1.
            if y0 >= target && target >= y1 {
                let l0 = math::log10(y0);
                let l1 = math::log10(y1);
                let lt = math::log10(target);
                if (l1 - l0).abs() < 1e-12 {
                    return Some(x0);
                }
                return Some(x0 + (lt - l0) / (l1 - l0) * (x1 - x0));
            }
        }
        None
    }

    /// The same curve translated by `delta_db` along the SNR axis.
    #[must_use]
    pub fn shifted(&self, delta_db: f64) -> Self {
        Self {
            label: format!("{}+{delta_db:+.2}dB", self.label),
            points: self
                .points
                .iter()
                .map(|(x, y)| (x + delta_db, *y))
                .collect(),
        }
    }

    /// This curve's shape, translated so that its 10 % crossing lands on `snr_db`.
    ///
    /// The shape is borrowed and therefore `todo-calibrate`; see the module docs.
    #[must_use]
    pub fn with_ten_percent_at(&self, snr_db: f64) -> Option<Self> {
        let own = self.snr_at_bler(0.1)?;
        let mut shifted = self.shifted(snr_db - own);
        shifted.label = format!("{}@10pc={snr_db:.2}dB", self.label);
        Some(shifted)
    }
}

/// One row of the WiLabV2Xsim PER table: a scenario, an MCS index, a transport-block size
/// and the SINR at which 10 % of packets fail.
///
/// 04-models.md §5.1 lists the 26 valid files of 45 and warns that the 19 "404: Not Found"
/// placeholders must not be used; only the LTE rows it prints are here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct WilabRow {
    /// The scenario the link-level curve was generated in.
    pub scenario: &'static str,
    /// The LTE MCS index.
    pub mcs: u8,
    /// The transport-block size in bytes.
    pub bytes: u32,
    /// The SINR at 10 % PER, dB.
    pub sinr_at_10pc_db: f64,
}

/// `phy/cv2x/bler-lut-wilab`, the LTE rows of 04-models.md §5.1.
///
/// Every value here is printed in the design document. The cells it does *not* print are
/// absent rather than interpolated: [`wilab_sinr_at_10pc`] returns `None` for them, and a
/// caller then has to say what it wants done about it.
pub const WILAB_LTE_SINR_AT_10PC: [WilabRow; 22] = [
    WilabRow {
        scenario: "highway-los",
        mcs: 3,
        bytes: 190,
        sinr_at_10pc_db: -0.53,
    },
    WilabRow {
        scenario: "highway-los",
        mcs: 4,
        bytes: 350,
        sinr_at_10pc_db: 0.14,
    },
    WilabRow {
        scenario: "highway-los",
        mcs: 5,
        bytes: 350,
        sinr_at_10pc_db: 1.20,
    },
    WilabRow {
        scenario: "highway-los",
        mcs: 7,
        bytes: 190,
        sinr_at_10pc_db: 8.71,
    },
    WilabRow {
        scenario: "highway-los",
        mcs: 7,
        bytes: 350,
        sinr_at_10pc_db: 4.14,
    },
    WilabRow {
        scenario: "highway-los",
        mcs: 9,
        bytes: 350,
        sinr_at_10pc_db: 10.54,
    },
    WilabRow {
        scenario: "highway-los",
        mcs: 11,
        bytes: 550,
        sinr_at_10pc_db: 7.16,
    },
    WilabRow {
        scenario: "highway-nlos",
        mcs: 3,
        bytes: 190,
        sinr_at_10pc_db: 2.35,
    },
    WilabRow {
        scenario: "highway-nlos",
        mcs: 4,
        bytes: 350,
        sinr_at_10pc_db: 3.31,
    },
    WilabRow {
        scenario: "highway-nlos",
        mcs: 5,
        bytes: 350,
        sinr_at_10pc_db: 4.37,
    },
    WilabRow {
        scenario: "highway-nlos",
        mcs: 7,
        bytes: 350,
        sinr_at_10pc_db: 7.29,
    },
    WilabRow {
        scenario: "highway-nlos",
        mcs: 9,
        bytes: 350,
        sinr_at_10pc_db: 14.63,
    },
    WilabRow {
        scenario: "highway-nlos",
        mcs: 11,
        bytes: 550,
        sinr_at_10pc_db: 10.70,
    },
    WilabRow {
        scenario: "urban-los",
        mcs: 4,
        bytes: 350,
        sinr_at_10pc_db: 0.83,
    },
    WilabRow {
        scenario: "urban-los",
        mcs: 5,
        bytes: 350,
        sinr_at_10pc_db: 1.87,
    },
    WilabRow {
        scenario: "urban-los",
        mcs: 7,
        bytes: 350,
        sinr_at_10pc_db: 4.89,
    },
    WilabRow {
        scenario: "urban-los",
        mcs: 9,
        bytes: 350,
        sinr_at_10pc_db: 11.55,
    },
    WilabRow {
        scenario: "crossing-nlos",
        mcs: 4,
        bytes: 350,
        sinr_at_10pc_db: 2.85,
    },
    WilabRow {
        scenario: "crossing-nlos",
        mcs: 5,
        bytes: 350,
        sinr_at_10pc_db: 3.85,
    },
    WilabRow {
        scenario: "crossing-nlos",
        mcs: 7,
        bytes: 350,
        sinr_at_10pc_db: 6.88,
    },
    WilabRow {
        scenario: "crossing-nlos",
        mcs: 9,
        bytes: 350,
        sinr_at_10pc_db: 13.71,
    },
    // Bazzi's own hard thresholds for 300 B, which 04-models.md §5.1 offers as the
    // alternative to the LUT [Bazzi 2018 Table 2].
    WilabRow {
        scenario: "bazzi-300b",
        mcs: 4,
        bytes: 300,
        sinr_at_10pc_db: 2.76,
    },
];

/// The SINR at 10 % PER for one `(scenario, MCS, bytes)` cell, or `None` when the table
/// does not cover it.
#[must_use]
pub fn wilab_sinr_at_10pc(scenario: &str, mcs: u8, bytes: u32) -> Option<f64> {
    WILAB_LTE_SINR_AT_10PC
        .iter()
        .find(|r| r.scenario == scenario && r.mcs == mcs && r.bytes == bytes)
        .map(|r| r.sinr_at_10pc_db)
}

/// One cited 10 %-operating point, as the fit consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SeAnchor {
    /// What it is.
    pub label: &'static str,
    /// Spectral efficiency, bits per resource element.
    pub spectral_efficiency: f64,
    /// SINR at 10 % block error, dB.
    pub sinr_at_10pc_db: f64,
}

/// The four cited operating points whose spectral efficiency 04-models.md §5.1 lets us
/// compute, and which therefore constrain the fit.
///
/// The two LUT crossings are the design document's own interpolations ("about 8.7 dB
/// (r0.7) and 5.9 dB (r0.5)", derived in R2e); the two Bazzi points are printed hard
/// thresholds. The spectral efficiencies are the shipped LTE presets'
/// ([`crate::sidelink::LTE_PRESETS`]), so a change to a preset's code rate moves the fit
/// and the residual test notices.
pub fn cited_anchors() -> Vec<SeAnchor> {
    use crate::sidelink as sl;
    vec![
        SeAnchor {
            label: "r1-160284 QPSK r0.7, 190 B",
            spectral_efficiency: sl::LTE_QPSK_R070.spectral_efficiency(),
            sinr_at_10pc_db: 8.7,
        },
        SeAnchor {
            label: "r1-160284 QPSK r0.5, 190 B",
            spectral_efficiency: sl::LTE_QPSK_R050.spectral_efficiency(),
            sinr_at_10pc_db: 5.9,
        },
        SeAnchor {
            label: "Bazzi MCS 4, 300 B",
            spectral_efficiency: sl::LTE_MCS4_BAZZI.spectral_efficiency(),
            sinr_at_10pc_db: 2.76,
        },
        SeAnchor {
            label: "Bazzi MCS 7, 300 B",
            spectral_efficiency: sl::LTE_MCS7_BAZZI.spectral_efficiency(),
            sinr_at_10pc_db: 7.30,
        },
    ]
}

/// The one-parameter fit that covers the MCS no cited curve reaches.
///
/// The model is `SINR_10%[dB] = 10·log10(2^SE − 1) + gap`: the Shannon SNR for the
/// spectral efficiency, plus one implementation gap fitted by least squares over
/// [`cited_anchors`]. It is a *fit to cited data*, not a citation, so every threshold it
/// produces is `todo-calibrate` and carries the residual spread with it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeGapFit {
    /// The fitted gap, dB.
    pub gap_db: f64,
    /// Root-mean-square residual over the anchors, dB.
    pub rms_residual_db: f64,
    /// Largest absolute residual, dB.
    pub max_residual_db: f64,
    /// `(anchor label, residual dB)`, in anchor order.
    pub residuals: Vec<(&'static str, f64)>,
}

impl SeGapFit {
    /// The Shannon SNR for a spectral efficiency, dB: `10·log10(2^SE − 1)`.
    #[must_use]
    pub fn shannon_snr_db(spectral_efficiency: f64) -> f64 {
        let lin = math::pow(2.0, spectral_efficiency) - 1.0;
        if lin <= 0.0 {
            return f64::NEG_INFINITY;
        }
        10.0 * math::log10(lin)
    }

    /// The fit over [`cited_anchors`].
    #[must_use]
    pub fn cited() -> Self {
        Self::over(&cited_anchors())
    }

    /// The fit over a caller-supplied anchor set.
    #[must_use]
    pub fn over(anchors: &[SeAnchor]) -> Self {
        if anchors.is_empty() {
            return Self {
                gap_db: 0.0,
                rms_residual_db: 0.0,
                max_residual_db: 0.0,
                residuals: Vec::new(),
            };
        }
        // Least squares for a pure offset is the mean of the offsets; summed in a fixed
        // order so the fit is bit-identical on every platform.
        let gaps: Vec<f64> = anchors
            .iter()
            .map(|a| a.sinr_at_10pc_db - Self::shannon_snr_db(a.spectral_efficiency))
            .collect();
        let gap_db = math::sum_ordered(gaps.iter().copied()) / anchors.len() as f64;
        let residuals: Vec<(&'static str, f64)> = anchors
            .iter()
            .zip(&gaps)
            .map(|(a, g)| (a.label, g - gap_db))
            .collect();
        let sq = math::sum_ordered(residuals.iter().map(|(_, r)| r * r));
        Self {
            gap_db,
            rms_residual_db: math::sqrt(sq / anchors.len() as f64),
            max_residual_db: residuals
                .iter()
                .map(|(_, r)| r.abs())
                .fold(0.0f64, f64::max),
            residuals,
        }
    }

    /// The 10 %-BLER SINR this fit predicts for a spectral efficiency, dB.
    #[must_use]
    pub fn sinr_at_10pc_db(&self, spectral_efficiency: f64) -> f64 {
        Self::shannon_snr_db(spectral_efficiency) + self.gap_db
    }

    /// The 10 %-BLER SINR for an MCS spec.
    #[must_use]
    pub fn sinr_for(&self, mcs: SlMcsSpec) -> f64 {
        self.sinr_at_10pc_db(mcs.spectral_efficiency())
    }
}

/// How a [`SidelinkErrorModel`] got the curve it is evaluating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CurveProvenance {
    /// Every point printed by the source: the two `r1-160284` lookups.
    Verbatim,
    /// The 10 % point is printed (a WiLab row, a Bazzi threshold); the *shape* is the
    /// QPSK r0.7 lookup's, shifted onto it. `todo-calibrate` on the shape.
    CitedPointBorrowedShape,
    /// Both the point and the shape are derived: the point from [`SeGapFit`], the shape
    /// borrowed. `todo-calibrate` on both.
    FittedPointBorrowedShape,
}

impl CurveProvenance {
    /// The label a report prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            CurveProvenance::Verbatim => "verbatim",
            CurveProvenance::CitedPointBorrowedShape => "cited-point-borrowed-shape",
            CurveProvenance::FittedPointBorrowedShape => "fitted-point-borrowed-shape",
        }
    }

    /// Whether a model resting on this provenance may be registered better than
    /// `unvalidated`.
    #[must_use]
    pub const fn is_fully_cited(self) -> bool {
        matches!(self, CurveProvenance::Verbatim)
    }
}

/// The sidelink error model: one curve, its provenance, and the block-error draw.
///
/// It is the C-V2X counterpart of [`crate::per::PerModel`], and deliberately not the same
/// type: the 802.11p model *computes* a PER from modulation, code rate and length, while
/// every C-V2X source publishes measured curves and no equation, so this one looks values
/// up. 04-models.md §5.1 names the error models separately for that reason.
#[derive(Debug, Clone, PartialEq)]
pub struct SidelinkErrorModel {
    card: ModelCard,
    data: BlerCurve,
    control: BlerCurve,
    provenance: CurveProvenance,
}

impl SidelinkErrorModel {
    /// The model's id when its curve is verbatim.
    pub const ID_R1_160284: &'static str = "phy/lte-v2x/bler-lut-r1-160284";
    /// The model's id when its operating points come from the WiLab table.
    pub const ID_WILAB: &'static str = "phy/cv2x/bler-lut-wilab";
    /// The model's id when the operating point is fitted.
    pub const ID_SE_FIT: &'static str = "phy/cv2x/bler-se-gap-fit";

    /// How much more robust the control channel is than the shared channel, dB.
    ///
    /// The SCI is carried on 2 PRB at QPSK with its own CRC while the transport block
    /// spans the whole allocation, so the control channel enjoys both a lower code rate
    /// and a narrower band. 04-models.md §5.1 requires SCI decoding to be evaluated first
    /// but prints no PSCCH curve, so this offset is `todo-calibrate`: the shipped 6 dB is
    /// the difference between the two printed LUTs' 10 % crossings (8.7 dB at r0.7 against
    /// 5.9 dB at r0.5, so 2.8 dB for one halving of the rate) extended by one further
    /// halving, and the calibration plan is to regenerate a PSCCH curve alongside the
    /// PSSCH ones.
    pub const CONTROL_ADVANTAGE_DB: f64 = 6.0;

    /// The verbatim QPSK r0.7 model.
    #[must_use]
    pub fn r1_160284_qpsk_r070() -> Self {
        Self::from_curve(
            Self::ID_R1_160284,
            BlerCurve::r1_160284_qpsk_r070(),
            CurveProvenance::Verbatim,
        )
    }

    /// The verbatim QPSK r0.5 model.
    #[must_use]
    pub fn r1_160284_qpsk_r050() -> Self {
        Self::from_curve(
            Self::ID_R1_160284,
            BlerCurve::r1_160284_qpsk_r050(),
            CurveProvenance::Verbatim,
        )
    }

    /// A model whose 10 % point is the WiLab row for `(scenario, mcs, bytes)`, with the
    /// borrowed shape.
    ///
    /// `None` when the table does not cover the cell — which is the point of the table
    /// listing which cells are backed by data.
    #[must_use]
    pub fn wilab(scenario: &str, mcs: u8, bytes: u32) -> Option<Self> {
        let point = wilab_sinr_at_10pc(scenario, mcs, bytes)?;
        let curve = BlerCurve::r1_160284_qpsk_r070().with_ten_percent_at(point)?;
        Some(Self::from_curve(
            Self::ID_WILAB,
            curve,
            CurveProvenance::CitedPointBorrowedShape,
        ))
    }

    /// A model whose 10 % point comes from the spectral-efficiency fit.
    #[must_use]
    pub fn se_fit(mcs: SlMcsSpec) -> Self {
        let fit = SeGapFit::cited();
        let point = fit.sinr_for(mcs);
        let curve = BlerCurve::r1_160284_qpsk_r070()
            .with_ten_percent_at(point)
            .expect("the r0.7 lookup crosses 10 %");
        Self::from_curve(
            Self::ID_SE_FIT,
            curve,
            CurveProvenance::FittedPointBorrowedShape,
        )
    }

    /// The model that best covers an MCS: the verbatim lookup when the MCS *is* one of the
    /// two printed ones, the fit otherwise.
    #[must_use]
    pub fn best_for(mcs: SlMcsSpec) -> Self {
        use crate::sidelink as sl;
        if mcs == sl::LTE_QPSK_R070 {
            Self::r1_160284_qpsk_r070()
        } else if mcs == sl::LTE_QPSK_R050 {
            Self::r1_160284_qpsk_r050()
        } else if mcs == sl::LTE_MCS4_BAZZI {
            Self::wilab("bazzi-300b", 4, 300).expect("the Bazzi MCS 4 row is in the table")
        } else if mcs == sl::LTE_MCS5_J3161 {
            // The J3161/1 presets are LTE MCS indices, which is what the WiLabV2Xsim rows
            // are keyed by: urban line-of-sight at 350 B where the table has it, the
            // only MCS 11 row (highway line-of-sight, 550 B) otherwise.
            Self::wilab("urban-los", 5, 350).expect("the urban MCS 5 row is in the table")
        } else if mcs == sl::LTE_MCS7_J3161 {
            Self::wilab("urban-los", 7, 350).expect("the urban MCS 7 row is in the table")
        } else if mcs == sl::LTE_MCS11_J3161 {
            Self::wilab("highway-los", 11, 550).expect("the MCS 11 row is in the table")
        } else {
            Self::se_fit(mcs)
        }
    }

    fn from_curve(id: &'static str, data: BlerCurve, provenance: CurveProvenance) -> Self {
        let control = data.shifted(-Self::CONTROL_ADVANTAGE_DB);
        Self {
            card: card(id, &data, provenance),
            data,
            control,
            provenance,
        }
    }

    /// The shared-channel curve.
    #[must_use]
    pub const fn data_curve(&self) -> &BlerCurve {
        &self.data
    }

    /// The control-channel curve.
    #[must_use]
    pub const fn control_curve(&self) -> &BlerCurve {
        &self.control
    }

    /// Where the curve came from.
    #[must_use]
    pub const fn provenance(&self) -> CurveProvenance {
        self.provenance
    }

    /// The transport block's error probability at an effective SINR.
    #[must_use]
    pub fn tb_bler(&self, sinr_db: f64) -> f64 {
        self.data.bler(sinr_db)
    }

    /// The SCI's error probability at an effective SINR.
    #[must_use]
    pub fn sci_bler(&self, sinr_db: f64) -> f64 {
        self.control.bler(sinr_db)
    }

    /// The SINR at which the transport block fails one time in ten.
    #[must_use]
    pub fn sinr_at_10pc_db(&self) -> Option<f64> {
        self.data.snr_at_bler(0.1)
    }
}

impl Model for SidelinkErrorModel {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

fn card(id: &'static str, curve: &BlerCurve, provenance: CurveProvenance) -> ModelCard {
    let huawei = Source::new(
        SourceKind::Paper,
        "Huawei R1-160284 via Gonzalez-Martin et al. 2019 `get_BLER.m`, through \
         04-models.md §5.1: BLER versus SNR for 190 B at 280 km/h relative speed",
    );
    let wilab = Source::new(
        SourceKind::Code,
        "WiLabV2Xsim PER tables, LTE rows, through 04-models.md §5.1 (26 of 45 files \
         valid; the 19 \"404: Not Found\" placeholders are not used)",
    );
    let mut card = ModelCard::new(
        id,
        Family::Phy,
        "1.0.0",
        "Sidelink block-error rate: a measured BLER-versus-SINR lookup for the shared \
         channel and a more robust one for the control channel.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "log-space interpolation".to_string(),
            latex_or_text: "log10 BLER(γ) linear in γ between printed points; BLER = 1 \
                            below the first point and held at the last printed value above \
                            the last"
                .to_string(),
            notes: Some(
                "A BLER curve is read off a log plot, so log-space interpolation is what \
                 puts the 10 % crossing where the source's own reading puts it. The curve \
                 is not extrapolated past its printed floor."
                    .to_string(),
            ),
        },
        Equation {
            name: "spectral-efficiency gap fit".to_string(),
            latex_or_text: "SINR_10%[dB] = 10·log10(2^(Qm·R) − 1) + gap".to_string(),
            notes: Some(
                "One fitted parameter over the four cited 10 % operating points; used \
                 only for an MCS no source covers."
                    .to_string(),
            ),
        },
    ];
    let point_source = match provenance {
        CurveProvenance::Verbatim => huawei.clone(),
        CurveProvenance::CitedPointBorrowedShape => wilab.clone(),
        CurveProvenance::FittedPointBorrowedShape => Source {
            kind: SourceKind::TodoCalibrate,
            reference: "fitted from the four cited 10 % operating points of \
                        04-models.md §5.1 by `SeGapFit`; 04-models.md §5.1 records that \
                        curves for MCS 10 and 20, for 300 B at LTE numerology, and for \
                        every NR MCS were not found"
                .to_string(),
            accessed: None,
            note: Some(format!(
                "Fitted gap {:.2} dB, RMS residual {:.2} dB over the anchors.",
                SeGapFit::cited().gap_db,
                SeGapFit::cited().rms_residual_db
            )),
        },
    };
    let shape_source = if provenance.is_fully_cited() {
        huawei.clone()
    } else {
        Source {
            kind: SourceKind::TodoCalibrate,
            reference: "the QPSK r0.7 lookup's shape, translated onto this MCS's 10 % \
                        operating point"
                .to_string(),
            accessed: None,
            note: Some("Only the crossing is placed; the slope is borrowed.".to_string()),
        }
    };
    let plan = "Regenerate the BLER curves from a link-level simulator under the \
                04-models.md §3.3 channel models, one per (MCS, transport-block size, \
                scenario) the scenarios use, and replace the borrowed shape and the \
                fitted point with measured ones. This is the plan 04-models.md §5.1 \
                already records.";
    card.parameters = vec![
        Parameter {
            name: "curve".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(curve.label.clone()),
            range: None,
            source: point_source,
            calibration: if provenance.is_fully_cited() {
                None
            } else {
                Some(plan.to_string())
            },
        },
        Parameter {
            name: "curve_shape".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(BlerCurve::r1_160284_qpsk_r070().label),
            range: None,
            source: shape_source,
            calibration: if provenance.is_fully_cited() {
                None
            } else {
                Some(plan.to_string())
            },
        },
        Parameter {
            name: "control_advantage_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(SidelinkErrorModel::CONTROL_ADVANTAGE_DB),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(15.0)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "no PSCCH curve is printed in 04-models.md §5.1; the shipped \
                            6 dB extends the 2.8 dB the two printed PSSCH lookups differ \
                            by for one halving of the code rate"
                    .to_string(),
                accessed: None,
                note: Some(
                    "The SCI is 2 PRB of QPSK with its own CRC against a transport block \
                     spanning the whole allocation, so the control channel is more robust; \
                     by how much is not printed."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Regenerate a PSCCH BLER curve alongside the PSSCH curves and replace the \
                 single offset with it."
                    .to_string(),
            ),
        },
    ];
    card.assumptions = vec![
        "One effective SINR represents the whole allocation (the EESM effective SINR of \
         04-models.md §5.4 is approximated by the mean over the allocation)."
            .to_string(),
        "The curves were generated at 280 km/h relative speed and 190 B, and are applied \
         at other speeds and sizes unchanged."
            .to_string(),
    ];
    card.limitations = vec![
        "Below the first printed point the curve reports certain failure and above the \
         last it reports the printed floor, so a study of the 1e−5 reliability region of \
         TR 38.913 §7.9 cannot use it."
            .to_string(),
    ];
    card.ignores = vec![
        "Code-block segmentation, LDPC versus turbo differences, HARQ combining gain \
         across retransmissions (each transmission is drawn independently)."
            .to_string(),
    ];
    card.sources = vec![huawei, wilab];
    card.validation = Validation {
        status: if provenance.is_fully_cited() {
            ValidationStatus::LiteratureChecked
        } else {
            ValidationStatus::Unvalidated
        },
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §13, row \"C-V2X BLER\": 10 % BLER at about 8.7 dB (QPSK r0.7) \
             and 5.9 dB (r0.5) at 190 B; hard thresholds 2.76 dB (MCS 4) and 7.30 dB \
             (MCS 7) at 300 B, ±0.5 dB",
        )],
        tests: vec![
            "the_r1_160284_lut_reproduces_every_printed_point".to_string(),
            "the_ten_percent_crossings_match_the_validation_row".to_string(),
            "the_spectral_efficiency_fit_reports_its_own_residuals".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_r1_160284_lut_reproduces_every_printed_point() {
        let c = BlerCurve::r1_160284_qpsk_r070();
        for (snr, bler) in [
            (0.0, 1.0),
            (2.0, 0.9),
            (4.0, 0.7),
            (6.0, 0.4),
            (8.0, 0.13),
            (10.0, 0.045),
            (12.0, 0.017),
            (14.0, 0.007),
            (16.0, 1e-3),
        ] {
            let got = c.bler(snr);
            assert!(
                (got - bler).abs() < 1e-12,
                "QPSK r0.7 at {snr} dB: got {got}, printed {bler}"
            );
        }
        let d = BlerCurve::r1_160284_qpsk_r050();
        for (snr, bler) in [
            (-2.0, 1.0),
            (0.0, 0.9),
            (2.0, 0.7),
            (4.0, 0.3),
            (6.0, 0.09),
            (8.0, 0.02),
            (10.0, 0.002),
            (12.0, 1e-3),
        ] {
            let got = d.bler(snr);
            assert!(
                (got - bler).abs() < 1e-12,
                "QPSK r0.5 at {snr} dB: got {got}, printed {bler}"
            );
        }
        // Outside the printed range the curve does not invent anything.
        assert_eq!(c.bler(-10.0), 1.0);
        assert!((c.bler(40.0) - 1e-3).abs() < 1e-12);
    }

    #[test]
    fn the_ten_percent_crossings_match_the_validation_row() {
        // 04-models.md §13: "10 % BLER at about 8.7 dB (QPSK r0.7) and 5.9 dB (r0.5),
        // 190 B", tolerance ±0.5 dB.
        let r07 = BlerCurve::r1_160284_qpsk_r070()
            .snr_at_bler(0.1)
            .expect("crosses 10 %");
        let r05 = BlerCurve::r1_160284_qpsk_r050()
            .snr_at_bler(0.1)
            .expect("crosses 10 %");
        assert!(
            (r07 - 8.7).abs() <= 0.5,
            "QPSK r0.7 crosses 10 % at {r07:.2} dB, target 8.7 ±0.5"
        );
        assert!(
            (r05 - 5.9).abs() <= 0.5,
            "QPSK r0.5 crosses 10 % at {r05:.2} dB, target 5.9 ±0.5"
        );
        // A curve floored at 1e−3 has no 1e−4 crossing, and must say so.
        assert!(BlerCurve::r1_160284_qpsk_r070().snr_at_bler(1e-4).is_none());
    }

    #[test]
    fn shifting_a_curve_moves_its_crossing_and_nothing_else() {
        let base = BlerCurve::r1_160284_qpsk_r070();
        let moved = base.with_ten_percent_at(17.0).expect("shiftable");
        let at = moved.snr_at_bler(0.1).expect("still crosses");
        assert!((at - 17.0).abs() < 1e-9, "crossing landed at {at}");
        // The shape is preserved: the same BLER appears at the same offset from the
        // crossing.
        let base_at = base.snr_at_bler(0.1).unwrap();
        for d in [-4.0, -2.0, 0.0, 2.0, 4.0] {
            let a = base.bler(base_at + d);
            let b = moved.bler(17.0 + d);
            assert!((a - b).abs() < 1e-12, "shape changed at offset {d}");
        }
    }

    #[test]
    fn the_spectral_efficiency_fit_reports_its_own_residuals() {
        let fit = SeGapFit::cited();
        // The gap is an implementation loss above the Shannon limit: positive, and of the
        // order every practical link budget puts it at.
        assert!(
            fit.gap_db > 3.0 && fit.gap_db < 12.0,
            "implausible fitted gap {:.2} dB",
            fit.gap_db
        );
        // What matters is that the spread is reported, not that it is small. This is the
        // number a reader needs in order to know how much to trust a fitted threshold.
        println!(
            "SeGapFit: gap {:.3} dB, RMS residual {:.3} dB, max {:.3} dB",
            fit.gap_db, fit.rms_residual_db, fit.max_residual_db
        );
        for (label, r) in &fit.residuals {
            println!("  residual {r:+.3} dB  {label}");
        }
        assert_eq!(fit.residuals.len(), 4);
        assert!(
            fit.max_residual_db < 2.0,
            "the fit is not usable if an anchor is off by {:.2} dB",
            fit.max_residual_db
        );
        // The fit reproduces each anchor to within its own residual, by construction.
        for (a, (_, r)) in cited_anchors().iter().zip(&fit.residuals) {
            let predicted = fit.sinr_at_10pc_db(a.spectral_efficiency);
            assert!((predicted + r - a.sinr_at_10pc_db).abs() < 1e-9);
        }
        // Higher spectral efficiency must need more SINR.
        let mut prev = f64::NEG_INFINITY;
        for se in [0.5, 1.0, 2.0, 3.6094, 5.5547] {
            let v = fit.sinr_at_10pc_db(se);
            assert!(v > prev);
            prev = v;
        }
    }

    #[test]
    fn the_wilab_table_covers_only_the_cells_the_document_prints() {
        assert_eq!(wilab_sinr_at_10pc("highway-los", 7, 190), Some(8.71));
        assert_eq!(wilab_sinr_at_10pc("crossing-nlos", 9, 350), Some(13.71));
        // A cell the document does not print is absent, not interpolated.
        assert_eq!(wilab_sinr_at_10pc("highway-los", 10, 190), None);
        assert_eq!(wilab_sinr_at_10pc("urban-nlos", 4, 350), None);
        // NLOS always costs SINR relative to the same LOS cell, which is the sanity
        // property the table has to have.
        for mcs in [3u8, 4, 5, 7, 9] {
            let (los, nlos) = (
                wilab_sinr_at_10pc("highway-los", mcs, 350),
                wilab_sinr_at_10pc("highway-nlos", mcs, 350),
            );
            if let (Some(l), Some(n)) = (los, nlos) {
                assert!(n > l, "MCS {mcs}: NLOS {n} should exceed LOS {l}");
            }
        }
    }

    #[test]
    fn the_control_channel_is_more_robust_than_the_shared_channel() {
        let m = SidelinkErrorModel::r1_160284_qpsk_r070();
        for sinr in [0.0, 4.0, 8.0, 12.0] {
            assert!(
                m.sci_bler(sinr) <= m.tb_bler(sinr),
                "the SCI must not be more fragile than the TB at {sinr} dB"
            );
        }
        let tb10 = m.sinr_at_10pc_db().expect("crosses");
        let sci10 = m.control_curve().snr_at_bler(0.1).expect("crosses");
        assert!((tb10 - sci10 - SidelinkErrorModel::CONTROL_ADVANTAGE_DB).abs() < 1e-9);
    }

    #[test]
    fn the_model_chooses_a_verbatim_curve_when_one_exists() {
        use crate::sidelink as sl;
        assert_eq!(
            SidelinkErrorModel::best_for(sl::LTE_QPSK_R070).provenance(),
            CurveProvenance::Verbatim
        );
        assert_eq!(
            SidelinkErrorModel::best_for(sl::LTE_QPSK_R050).provenance(),
            CurveProvenance::Verbatim
        );
        assert_eq!(
            SidelinkErrorModel::best_for(sl::LTE_MCS4_BAZZI).provenance(),
            CurveProvenance::CitedPointBorrowedShape
        );
        let nr21 = SidelinkErrorModel::best_for(sl::nr_mcs(21).unwrap());
        assert_eq!(nr21.provenance(), CurveProvenance::FittedPointBorrowedShape);
        println!(
            "NR MCS 21 (SE {:.4}) 10 % BLER at {:.2} dB",
            sl::nr_mcs(21).unwrap().spectral_efficiency(),
            nr21.sinr_at_10pc_db().unwrap()
        );
        // A fitted curve is never registered better than unvalidated.
        assert_eq!(nr21.card().validation.status, ValidationStatus::Unvalidated);
        assert!(nr21.card().validate().is_ok());
        assert!(
            SidelinkErrorModel::r1_160284_qpsk_r070()
                .card()
                .validate()
                .is_ok()
        );
    }
}
