//! Small-scale fading: `fading/nakagami-m` and `fading/none` (04-models.md §3.4).
//!
//! The power gain of a Nakagami-m channel, sampled once per link and per frame from the
//! `Fading` RNG domain through the core's own Nakagami sampler
//! (`RngStream::nakagami`, a documented algorithm with a fixed draw count).
//!
//! # The key, and why it is single-use
//!
//! A fading draw is scoped to one frame on one directed link, so its key is
//! `EntityRef::LinkFrame { link, frame }`. That scope is `is_single_use` in the contract
//! crate: the key already embeds the frame counter, so `checkout` derives the stream,
//! uses it and drops it instead of interning a generator per frame per link that nothing
//! would ever free (03-interfaces.md §1.1). The consequence a caller can see is that the
//! *same* `(link, frame)` always yields the *same* sample, whatever else the run did in
//! between — which is what `the_same_link_and_frame_always_draws_the_same_sample`
//! asserts, and what makes a receiver's outcome independent of the order receivers are
//! evaluated in (invariant I-R2).
//!
//! `frame` is the frame's start instant on the link, in nanoseconds. Two frames cannot
//! start at the same nanosecond on one directed link, so the instant identifies the frame
//! without a counter that a reordering could disturb.
//!
//! # Presets, and the one that is not here
//!
//! `fixed-severe`, `fixed-medium` and `fixed-low` (m = 1, 3, 5) are the labels the
//! D-FPAV and EMDV studies use [Torrent-Moreno 2009]. `yin-dsrc-freeway` is the
//! distance-dependent preset: the cited source gives *ranges* per 10 m bin (1.0-1.8 below
//! 100 m, 0.7-1.0 above), not per-bin values, so the two shipped numbers are the range
//! midpoints and are recorded as `todo-calibrate` with the range on the parameter.
//!
//! **`taliwal-ns2` is not shipped.** 04-models.md §3.4 marks its thresholds UNVERIFIED —
//! "Torrent-Moreno 2009 credits Taliwal's ns-2.28 port but does not reproduce the
//! thresholds; VANET '04 text not retrieved" — and says "not shipped until confirmed".
//! Shipping it would mean inventing the 50 m and 150 m breakpoints. The Cheng 2007
//! distance-binned m is likewise unrecoverable, and `fading/rician` does not exist
//! because no numeric Rician K for 5.9 GHz V2V was found in any cached source.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::LinkKey;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::SimTime;
use v2xw_world::model::EnvClass;

use crate::traits::Fading;

/// A Nakagami-m shape preset (04-models.md §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NakagamiPreset {
    /// m = 1 everywhere: Rayleigh, the severest of the three fixed labels.
    FixedSevere,
    /// m = 3 everywhere. The default outside highways.
    FixedMedium,
    /// m = 5 everywhere: the most line-of-sight-like.
    FixedLow,
    /// Distance-dependent, Yin et al. as quoted in the α-μ DSRC paper: more
    /// line-of-sight-like below 100 m, Rayleigh-or-worse beyond. The default on highways.
    YinDsrcFreeway,
}

impl NakagamiPreset {
    /// Every shipped preset, in a fixed order.
    pub const ALL: [NakagamiPreset; 4] = [
        NakagamiPreset::FixedSevere,
        NakagamiPreset::FixedMedium,
        NakagamiPreset::FixedLow,
        NakagamiPreset::YinDsrcFreeway,
    ];

    /// The preset's id as a scenario spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            NakagamiPreset::FixedSevere => "fixed-severe",
            NakagamiPreset::FixedMedium => "fixed-medium",
            NakagamiPreset::FixedLow => "fixed-low",
            NakagamiPreset::YinDsrcFreeway => "yin-dsrc-freeway",
        }
    }

    /// The shape parameter `m` at a distance.
    #[must_use]
    pub fn m_at(self, d_m: f64) -> f64 {
        match self {
            NakagamiPreset::FixedSevere => 1.0,
            NakagamiPreset::FixedMedium => 3.0,
            NakagamiPreset::FixedLow => 5.0,
            NakagamiPreset::YinDsrcFreeway => {
                if d_m < 100.0 {
                    // Cited range 1.0-1.8; midpoint, todo-calibrate.
                    1.4
                } else {
                    // Cited range 0.7-1.0; midpoint, todo-calibrate.
                    0.85
                }
            }
        }
    }

    /// True when every constant of the preset is a printed value rather than the midpoint
    /// of a printed range.
    #[must_use]
    pub const fn is_fully_cited(self) -> bool {
        !matches!(self, NakagamiPreset::YinDsrcFreeway)
    }

    /// The preset an environment selects by default (04-models.md §3.4: the Yin freeway
    /// preset on highways, m = 3 elsewhere).
    #[must_use]
    pub const fn for_environment(env: EnvClass) -> NakagamiPreset {
        match env {
            EnvClass::Highway => NakagamiPreset::YinDsrcFreeway,
            EnvClass::Urban | EnvClass::Suburban | EnvClass::Rural => NakagamiPreset::FixedMedium,
        }
    }
}

/// `fading/nakagami-m` — per-link, per-frame Nakagami fading (04-models.md §3.4).
#[derive(Debug, Clone)]
pub struct NakagamiFading {
    card: ModelCard,
    preset: NakagamiPreset,
    /// `Ω`, the mean power gain. One, so the fading is a 0 dB-mean multiplicative gain
    /// and the link budget's path loss is not silently rescaled by it.
    omega: f64,
}

impl NakagamiFading {
    /// The model's id.
    pub const ID: &'static str = "fading/nakagami-m";

    /// The model with one preset.
    #[must_use]
    pub fn new(preset: NakagamiPreset) -> Self {
        Self {
            card: nakagami_card(preset),
            preset,
            omega: 1.0,
        }
    }

    /// The model with the preset this environment selects.
    #[must_use]
    pub fn for_environment(env: EnvClass) -> Self {
        Self::new(NakagamiPreset::for_environment(env))
    }

    /// The preset in use.
    #[must_use]
    pub const fn preset(&self) -> NakagamiPreset {
        self.preset
    }

    /// The shape parameter this model uses at a distance.
    #[must_use]
    pub fn m_at(&self, d_m: f64) -> f64 {
        self.preset.m_at(d_m)
    }
}

impl Default for NakagamiFading {
    fn default() -> Self {
        Self::new(NakagamiPreset::FixedMedium)
    }
}

impl Model for NakagamiFading {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Fading<C> for NakagamiFading {
    fn sample_db(&mut self, ctx: &mut C, link: LinkKey, d_m: f64, t: SimTime) -> f64 {
        let m = self.m_at(d_m);
        // `nakagami` returns an amplitude; the power gain is its square, and the gain in
        // dB is 10·log10 of that, i.e. 20·log10 of the amplitude.
        let amplitude = ctx
            .rng(RngDomain::Fading, EntityRef::LinkFrame { link, frame: t })
            .nakagami(m, self.omega);
        if amplitude <= 0.0 {
            // A zero amplitude is a total fade. Reporting −inf would poison every sum it
            // reaches; −200 dB is below any receiver's sensitivity by 100 dB and keeps
            // the arithmetic finite.
            return -200.0;
        }
        20.0 * math::log10(amplitude)
    }
}

fn nakagami_card(preset: NakagamiPreset) -> ModelCard {
    let torrent = Source::new(
        SourceKind::Paper,
        "M. Torrent-Moreno et al., IEEE TVT 2009, Eq. 2 (R3 §B), via 04-models.md §3.4",
    );
    let mut card = ModelCard::new(
        NakagamiFading::ID,
        Family::Fading,
        "1.0.0",
        "Nakagami-m small-scale fading, sampled per link and per frame from the core's \
         Nakagami sampler.",
    );
    card.tier = vec![Tier::High];
    card.equations = vec![Equation {
        name: "Nakagami-m density".to_string(),
        latex_or_text: "f(x; m, Ω) = 2m^m/(Γ(m)Ω^m)·x^(2m−1)·exp(−m·x²/Ω), m >= 1/2".to_string(),
        notes: Some(
            "m = 1 is Rayleigh, larger m is more line-of-sight-like. The sample is an \
             amplitude; the gain reported is 20·log10(amplitude), i.e. 10·log10 of the \
             power gain, with Ω = 1 so the mean power gain is 0 dB."
                .to_string(),
        ),
    }];
    let m_source = if preset.is_fully_cited() {
        torrent.clone()
    } else {
        Source {
            kind: SourceKind::TodoCalibrate,
            reference: "Yin et al. as quoted in the α-μ DSRC paper (R3 §B): m in 1.0-1.8 \
                        below 100 m and 0.7-1.0 above, in 10 m bins; the per-bin values \
                        are not printed, so the midpoints are shipped"
                .to_string(),
            accessed: None,
            note: Some("Secondary source; not Cheng 2007.".to_string()),
        }
    };
    card.parameters = vec![
        Parameter {
            name: "preset".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(preset.label()),
            range: Some(
                NakagamiPreset::ALL
                    .iter()
                    .map(|p| serde_json::json!(p.label()))
                    .collect(),
            ),
            source: m_source.clone(),
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some(
                    "Obtain the Yin et al. per-10 m-bin table (or a 5.9 GHz V2V \
                     measurement of equal standing) and replace the two midpoints with \
                     the binned values."
                        .to_string(),
                )
            },
        },
        Parameter {
            name: "m_near".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(preset.m_at(0.0)),
            range: Some(vec![serde_json::json!(0.5), serde_json::json!(10.0)]),
            source: m_source.clone(),
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some("As for `preset`; the cited range is 1.0-1.8.".to_string())
            },
        },
        Parameter {
            name: "m_far".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(preset.m_at(1_000.0)),
            range: Some(vec![serde_json::json!(0.5), serde_json::json!(10.0)]),
            source: m_source,
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some("As for `preset`; the cited range is 0.7-1.0.".to_string())
            },
        },
        Parameter {
            name: "distance_threshold_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(100.0),
            range: Some(vec![serde_json::json!(10.0), serde_json::json!(1_000.0)]),
            source: Source::new(
                SourceKind::Paper,
                "Yin et al. via R3 §B: the 100 m boundary between the two m bands",
            ),
            calibration: None,
        },
        Parameter {
            name: "omega".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(1.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(10.0)]),
            source: torrent.clone(),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "Ω = 1, so fading is a multiplicative gain with unit mean power and does not \
         rescale the path loss."
            .to_string(),
        "One independent draw per frame per directed link, keyed by the frame's start \
         instant."
            .to_string(),
    ];
    card.limitations = vec![
        "No temporal correlation between successive frames on the same link: each frame \
         draws independently (04-models.md §3.4 records this as a TODO: calibrate pending \
         a cited 5.9 GHz Doppler measurement)."
            .to_string(),
        "No frequency selectivity within the 10 MHz channel.".to_string(),
        "taliwal-ns2 is not shipped: its distance thresholds are UNVERIFIED.".to_string(),
    ];
    card.ignores = vec![
        "Doppler spectrum and coherence time; the medium tier ignores fast fading \
         altogether (fading/none)."
            .to_string(),
    ];
    card.sources = vec![torrent];
    card.validation = Validation {
        status: if preset.is_fully_cited() {
            ValidationStatus::LiteratureChecked
        } else {
            // The shipped m values are range midpoints, not printed values.
            ValidationStatus::Unvalidated
        },
        references: Vec::new(),
        tests: vec![
            "the_same_link_and_frame_always_draws_the_same_sample".to_string(),
            "the_mean_power_gain_is_unity".to_string(),
            "a_smaller_m_fades_deeper".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["fading".to_string()],
    };
    card
}

/// `fading/none` — the medium tier's fading model, which is no fading at all
/// (04-models.md §3.4, and the tier table of §3: "medium ignores fast fading").
#[derive(Debug, Clone)]
pub struct NoFading {
    card: ModelCard,
}

impl NoFading {
    /// The model's id.
    pub const ID: &'static str = "fading/none";

    /// The model.
    #[must_use]
    pub fn new() -> Self {
        let mut card = ModelCard::new(
            Self::ID,
            Family::Fading,
            "1.0.0",
            "No small-scale fading: the medium tier's choice, and the way to turn fading \
             off in a study that varies one thing at a time.",
        );
        card.tier = vec![Tier::Abstract, Tier::Medium];
        card.equations = vec![Equation::new("fading gain", "0 dB")];
        card.assumptions =
            vec!["The channel gain is its large-scale mean at every instant.".to_string()];
        card.ignores =
            vec!["Everything fading/nakagami-m models (04-models.md §3 tier table).".to_string()];
        card.sources = vec![Source::new(
            SourceKind::Standard,
            "02-architecture.md §7.1 tier table, via 04-models.md §3.4",
        )];
        card.validation = Validation::new(ValidationStatus::UnitTested);
        Self { card }
    }
}

impl Default for NoFading {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for NoFading {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Fading<C> for NoFading {
    fn sample_db(&mut self, _ctx: &mut C, _link: LinkKey, _d_m: f64, _t: SimTime) -> f64 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric;
    use crate::testctx::TestCtx;
    use v2xw_core::ids::NodeId;

    fn link(a: u32, b: u32) -> LinkKey {
        LinkKey(NodeId::new(a), NodeId::new(b))
    }

    #[test]
    fn the_same_link_and_frame_always_draws_the_same_sample() {
        let mut ctx = TestCtx::new(1234);
        let mut model = NakagamiFading::new(NakagamiPreset::FixedMedium);
        let l = link(3, 9);
        let first = model.sample_db(&mut ctx, l, 150.0, 4_200_000);
        // Draw a few unrelated samples in between, on other links and other frames.
        for i in 0..20 {
            model.sample_db(&mut ctx, link(1, 2), 40.0, 1_000 * i);
            model.sample_db(&mut ctx, l, 150.0, 9_000_000 + i);
        }
        let again = model.sample_db(&mut ctx, l, 150.0, 4_200_000);
        assert_eq!(first.to_bits(), again.to_bits(), "{first} vs {again}");
        // A different frame on the same link is a different sample; a different link at
        // the same instant is a different sample.
        assert_ne!(first, model.sample_db(&mut ctx, l, 150.0, 4_200_001));
        assert_ne!(
            first,
            model.sample_db(&mut ctx, link(9, 3), 150.0, 4_200_000)
        );
    }

    #[test]
    fn the_draw_order_across_links_does_not_change_any_sample() {
        // Invariant I-R2: two receivers of the same transmission are independent, so the
        // engine may evaluate them in any order.
        let mut ctx = TestCtx::new(77);
        let mut model = NakagamiFading::new(NakagamiPreset::FixedSevere);
        let links: Vec<LinkKey> = (0..8).map(|i| link(0, i + 1)).collect();
        let forward: Vec<f64> = links
            .iter()
            .map(|l| model.sample_db(&mut ctx, *l, 100.0, 7_000_000))
            .collect();
        let backward: Vec<f64> = links
            .iter()
            .rev()
            .map(|l| model.sample_db(&mut ctx, *l, 100.0, 7_000_000))
            .collect();
        let backward: Vec<f64> = backward.into_iter().rev().collect();
        assert_eq!(forward, backward);
    }

    #[test]
    fn the_mean_power_gain_is_unity() {
        // Ω = 1: the fading is a unit-mean multiplicative gain, so it does not silently
        // rescale the path loss.
        let mut ctx = TestCtx::new(2024);
        for preset in NakagamiPreset::ALL {
            let mut model = NakagamiFading::new(preset);
            let n = 20_000;
            let mean_power = math::sum_ordered((0..n).map(|i| {
                let db = model.sample_db(&mut ctx, link(1, 2), 50.0, i as SimTime);
                numeric::db_to_linear(db)
            })) / f64::from(n);
            assert!(
                (mean_power - 1.0).abs() < 0.05,
                "{}: mean power gain {mean_power}",
                preset.label()
            );
        }
    }

    #[test]
    fn a_smaller_m_fades_deeper() {
        // m = 1 (Rayleigh) must spend more time deep in a fade than m = 5.
        let mut ctx = TestCtx::new(31337);
        let n = 20_000;
        let deep_fraction = |model: &mut NakagamiFading, ctx: &mut TestCtx| {
            let count = (0..n)
                .filter(|i| model.sample_db(ctx, link(4, 5), 200.0, *i as SimTime) < -6.0)
                .count();
            count as f64 / f64::from(n)
        };
        let severe = deep_fraction(
            &mut NakagamiFading::new(NakagamiPreset::FixedSevere),
            &mut ctx,
        );
        let low = deep_fraction(&mut NakagamiFading::new(NakagamiPreset::FixedLow), &mut ctx);
        assert!(severe > low, "m=1 {severe} should exceed m=5 {low}");
        // Rayleigh is below −6 dB about 1 − exp(−10^−0.6) = 22 % of the time.
        assert!((severe - 0.221).abs() < 0.02, "{severe}");
    }

    #[test]
    fn the_yin_preset_switches_bands_at_a_hundred_metres() {
        let m = NakagamiFading::new(NakagamiPreset::YinDsrcFreeway);
        assert_eq!(m.m_at(99.9), 1.4);
        assert_eq!(m.m_at(100.0), 0.85);
        // And the preset is registered unvalidated, because both numbers are midpoints of
        // a cited range rather than printed values.
        assert_eq!(m.card().validation.status, ValidationStatus::Unvalidated);
        assert_eq!(
            NakagamiFading::new(NakagamiPreset::FixedMedium)
                .card()
                .validation
                .status,
            ValidationStatus::LiteratureChecked
        );
    }

    #[test]
    fn taliwal_is_not_shippable_and_no_preset_claims_to_be_it() {
        for p in NakagamiPreset::ALL {
            assert_ne!(p.label(), "taliwal-ns2");
        }
    }

    #[test]
    fn no_fading_returns_zero_and_the_environment_default_follows_the_document() {
        let mut ctx = TestCtx::new(5);
        let mut none = NoFading::new();
        assert_eq!(none.sample_db(&mut ctx, link(1, 2), 10.0, 0), 0.0);
        assert_eq!(
            NakagamiPreset::for_environment(EnvClass::Highway),
            NakagamiPreset::YinDsrcFreeway
        );
        assert_eq!(
            NakagamiPreset::for_environment(EnvClass::Urban),
            NakagamiPreset::FixedMedium
        );
    }

    #[test]
    fn the_cards_validate_and_register() {
        let mut registry = v2xw_core::registry::Registry::new();
        registry
            .register(NakagamiFading::default().card().clone())
            .expect("registers");
        registry
            .register(NoFading::new().card().clone())
            .expect("registers");
        assert!(registry.contains(NakagamiFading::ID));
        assert!(registry.contains(NoFading::ID));
        for preset in NakagamiPreset::ALL {
            NakagamiFading::new(preset)
                .card()
                .validate()
                .expect("card validates");
        }
    }
}
