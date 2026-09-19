//! What both GNSS models share: the quantile factors, the fit, the environment scale and
//! the confidence ellipse.

use v2xw_core::math;

use crate::views::SkyView;

/// The 95 % factor of a Rayleigh distribution, `sqrt(−2·ln(0.05))`.
///
/// The legacy engine writes it as the literal `2.448` [`run.py` L1895]; the exact value is
/// 2.447 746 830 … and is used here, with the legacy literal kept in
/// [`crate::gnss::ou_bias_legacy`] for bit-parity with the frozen corpus.
pub const RAYLEIGH_95: f64 = 2.4477468306808265;

/// The factor `k_p` such that the `p`-quantile of the magnitude of a two-dimensional
/// zero-mean Gaussian with per-axis σ is `k_p·σ`: `k_p = sqrt(−2·ln(1 − p))`.
///
/// This is the Rayleigh quantile, and it is what a *horizontal* error percentile means.
pub fn rayleigh_factor(p: f64) -> f64 {
    math::sqrt(-2.0 * math::ln(1.0 - p.clamp(0.0, 0.999_999)))
}

/// The factor `z_p` such that the `p`-quantile of `|x|` for `x ~ N(0, σ²)` is `z_p·σ`:
/// `z_p = Φ⁻¹((1 + p)/2)`.
///
/// Only the three percentiles the Reid 2019 table reports are needed, and the standard
/// normal quantiles at them are mathematical constants rather than model parameters:
/// `z_0.68 = 0.994 457 883 …`, `z_0.95 = 1.959 963 985 …`, `z_0.99 = 2.575 829 304 …`.
/// Anything else returns `None` rather than an interpolation nobody asked for.
pub fn folded_normal_factor(p: f64) -> Option<f64> {
    const TABLE: [(f64, f64); 3] = [
        (0.68, 0.9944578832097534),
        (0.95, 1.959963984540054),
        (0.99, 2.5758293035489004),
    ];
    TABLE
        .iter()
        .find(|(q, _)| (q - p).abs() < 1e-9)
        .map(|(_, z)| *z)
}

/// Least-squares fit of one scale `σ` to a set of `(factor, value)` quantile rows.
///
/// `σ = Σ k·v / Σ k²`, the closed-form least-squares solution of `v ≈ σ·k`, and the
/// residuals `σ·k − v` come back with it. A single Gaussian scale cannot reproduce three
/// measured percentiles exactly — a real GNSS error distribution has heavier tails — so
/// §3.8 requires the fit *and its residual* to be recorded on the card rather than the
/// discrepancy being hidden.
pub fn fit_scale(rows: &[(f64, f64)]) -> (f64, Vec<f64>) {
    let numerator: f64 = math::sum_ordered(rows.iter().map(|(k, v)| k * v).collect::<Vec<_>>());
    let denominator: f64 = math::sum_ordered(rows.iter().map(|(k, _)| k * k).collect::<Vec<_>>());
    let sigma = if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    };
    let residuals = rows.iter().map(|(k, v)| sigma * k - v).collect();
    (sigma, residuals)
}

/// The σ multiplier for an environment class (§3.8).
///
/// The ratio of that class's measured mean horizontal error to the open-sky mean:
/// `31.02 / 3.07` for a deep urban canyon and `9.57 / 3.07` with NLOS exclusion working
/// [Wen and Hsu, Reid 2019, R3 §G.2-G.3]. An obstructed receiver has no fix at all, so it
/// has no scale.
pub fn env_scale(sky: SkyView) -> Option<f64> {
    match sky {
        SkyView::OpenSky => Some(1.0),
        SkyView::CanyonMitigated => Some(9.57 / 3.07),
        SkyView::DeepCanyon => Some(31.02 / 3.07),
        SkyView::Obstructed => None,
    }
}

/// The 95 % confidence radius a receiver *may honestly report*, metres.
///
/// `k·sqrt(σ_white² + σ_bias²)` with `k` the Rayleigh 95 % factor. Both inputs are
/// **parameters of the receiver**, not samples of its error: a receiver knows its noise
/// figure and its bias variance, and does not know the bias it is currently carrying. That
/// is the §2.10 correction, and it is why this function takes two standard deviations and
/// nothing else — there is no argument here through which a realised error could be passed.
pub fn confidence_95_m(sigma_white_m: f64, sigma_bias_m: f64, factor: f64) -> f64 {
    factor * math::sqrt(sigma_white_m * sigma_white_m + sigma_bias_m * sigma_bias_m)
}

/// The mean of an exponential whose 95th percentile is `p95`: `mean = p95 / (−ln 0.05)`.
///
/// §3.8 gives outage durations by their 95th percentile (under 7 s for SPS, over 60 s for
/// RTK), and an exponential is the memoryless choice for a duration with no other
/// structure; this is the arithmetic that turns the one into the other, shown rather than
/// pre-computed.
pub fn exponential_mean_for_p95(p95: f64) -> f64 {
    p95 / (-math::ln(0.05))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rayleigh_factors_are_the_published_ones() {
        assert!((rayleigh_factor(0.95) - RAYLEIGH_95).abs() < 1e-12);
        // The legacy literal 2.448 is the same number to three decimals.
        assert!((RAYLEIGH_95 - 2.448).abs() < 5e-4);
        assert!((rayleigh_factor(0.68) - 1.5095921854).abs() < 1e-9);
        assert!((rayleigh_factor(0.99) - 3.0348542587).abs() < 1e-9);
    }

    #[test]
    fn the_folded_normal_factors_are_the_standard_quantiles() {
        assert!((folded_normal_factor(0.68).unwrap() - 0.9944578832).abs() < 1e-9);
        assert!((folded_normal_factor(0.95).unwrap() - 1.9599639845).abs() < 1e-9);
        assert!((folded_normal_factor(0.99).unwrap() - 2.5758293035).abs() < 1e-9);
        assert!(folded_normal_factor(0.5).is_none());
    }

    #[test]
    fn the_fit_is_the_least_squares_solution() {
        // An exact fit has zero residual.
        let rows = [(1.0, 2.0), (2.0, 4.0), (3.0, 6.0)];
        let (sigma, residuals) = fit_scale(&rows);
        assert!((sigma - 2.0).abs() < 1e-12);
        assert!(residuals.iter().all(|r| r.abs() < 1e-12));
        // An inconsistent one has the least-squares scale and non-zero residuals that sum
        // to zero against the factors (the normal equation).
        let rows = [(1.0, 1.0), (2.0, 3.0), (3.0, 4.0)];
        let (sigma, residuals) = fit_scale(&rows);
        let want = (1.0 * 1.0 + 2.0 * 3.0 + 3.0 * 4.0) / (1.0 + 4.0 + 9.0);
        assert!((sigma - want).abs() < 1e-12);
        let orthogonality: f64 = rows.iter().zip(&residuals).map(|((k, _), r)| k * r).sum();
        assert!(orthogonality.abs() < 1e-12, "{orthogonality}");
    }

    #[test]
    fn the_environment_scales_are_the_document_ratios() {
        assert_eq!(env_scale(SkyView::OpenSky), Some(1.0));
        let canyon = env_scale(SkyView::DeepCanyon).unwrap();
        assert!((canyon - 31.02 / 3.07).abs() < 1e-12);
        assert!((canyon - 10.1042).abs() < 1e-3, "{canyon}");
        let mitigated = env_scale(SkyView::CanyonMitigated).unwrap();
        assert!((mitigated - 9.57 / 3.07).abs() < 1e-12);
        assert_eq!(env_scale(SkyView::Obstructed), None);
    }

    #[test]
    fn the_confidence_uses_only_parameters() {
        // 1.6 m of white noise and 1.36 m of bias give 2.448·sqrt(1.6² + 1.36²) ≈ 5.14 m.
        let c = confidence_95_m(1.6, 1.36, RAYLEIGH_95);
        assert!((c - RAYLEIGH_95 * math::sqrt(1.6 * 1.6 + 1.36 * 1.36)).abs() < 1e-12);
        assert!((c - 5.1408).abs() < 1e-3, "{c}");
        // It is monotone in both and zero only when both are.
        assert!(confidence_95_m(2.0, 1.36, RAYLEIGH_95) > c);
        assert!(confidence_95_m(1.6, 2.0, RAYLEIGH_95) > c);
        assert_eq!(confidence_95_m(0.0, 0.0, RAYLEIGH_95), 0.0);
    }

    #[test]
    fn the_outage_mean_reproduces_its_ninety_fifth_percentile() {
        let mean = exponential_mean_for_p95(7.0);
        // P95 of Exp(mean) is −mean·ln(0.05).
        assert!((-mean * math::ln(0.05) - 7.0).abs() < 1e-12);
        assert!((mean - 2.3367).abs() < 1e-3, "{mean}");
        let rtk = exponential_mean_for_p95(60.0);
        assert!((-rtk * math::ln(0.05) - 60.0).abs() < 1e-12);
    }
}
