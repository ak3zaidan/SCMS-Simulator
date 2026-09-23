//! Line of sight over terrain: the elevation profile along a link, and what obstructs it.
//!
//! `v2xw-radio`'s `obstacle/terrain/knife-edge-p526` (04-models.md §3.5) turns a terrain
//! profile into a diffraction loss: `ν = h·sqrt(2(d1 + d2)/(λ·d1·d2))` and the ITU-R
//! P.526 `J(ν)`. What it needs from this side is the profile itself — where the ground
//! is along the segment, and where it rises above the straight line between the two
//! antennas. That query is here, in the crate that owns the geometry, for three reasons:
//! the sampling rule belongs with the grid it samples, the parameter that sets it is
//! world data rather than radio data, and a mobility or a rendering consumer wants the
//! same profile without depending on the radio stack.
//!
//! # What a profile is
//!
//! Given two endpoints `a` and `b` — antenna positions in world coordinates, `z`
//! included — [`terrain_profile`] walks the segment at a fixed spacing and records, at
//! each step, the ground height under the point and the height of the straight line
//! `a → b` there. The difference is the **clearance**: negative means the line passes
//! above the ground, positive means a hill is in the way by that much.
//!
//! ```text
//!                       ,--.        b
//!        a             /    \      /
//!         \___________/  h   \____/
//!          line ....../..\...........
//!                    d1    d2
//! ```
//!
//! [`TerrainProfile::edges`] reduces the profile to the knife edges that matter: each
//! local maximum of the clearance that is above the line, with the two distances the
//! P.526 geometry needs. A world with no DEM has no terrain edges at all, which is the
//! honest answer rather than a zero: the ground is the datum plane and cannot block a
//! link that is above it.
//!
//! # Not quantised
//!
//! A profile is an intermediate value, not an artefact, so — exactly as
//! [`crate::model::Terrain::height_at`] documents for a single sample — nothing here is
//! put on the quantisation grid. The grid it samples is quantised, the arithmetic is
//! multiplication and addition, and a writer quantises whatever it exports. The only
//! transcendental in the module is the square root of the diffraction parameter, which is
//! `v2xw-radio`'s to compute, not this module's: everything here is polynomial.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::Vec3;

use crate::dem::DEM_POST_SPACING_M;
use crate::error::{Result, WorldError};
use crate::model::World;

/// The model id under which the profile query's parameters are published.
pub const MODEL_ID: &str = "world/terrain/los-profile";

/// The query's version, as its model card reports it.
pub const MODEL_VERSION: &str = "1.0.0";

/// The smallest number of samples a profile can have.
///
/// Three: the two endpoints and one interior point, which is the fewest that can hold a
/// local maximum. It is a property of the local-maximum test rather than a calibrated
/// value, which is why the card cites the equation and not a measurement.
pub const MIN_PROFILE_SAMPLES: usize = 3;

/// How the segment between two antennas is sampled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProfileParams {
    /// Distance between successive samples along the link, metres.
    ///
    /// Defaults to [`DEM_POST_SPACING_M`] — 30 m, the post spacing of both DEMs
    /// 04-models.md §1.4 names. That is the resolution the elevation data actually
    /// carries: sampling finer interpolates detail the source does not have, and
    /// sampling coarser can step over a ridge the source does have. The value is on the
    /// model card with that citation.
    pub sample_spacing_m: f64,
    /// The most samples one profile may take, whatever the spacing says.
    ///
    /// A guard, so that a 100 km link at 30 m spacing cannot allocate 3 334 samples
    /// inside a per-frame path. Reaching it coarsens the profile, and
    /// [`TerrainProfile::spacing_m`] then reports the spacing actually used rather than
    /// the one asked for.
    pub max_samples: usize,
    /// How far above the straight line the ground must rise before the profile calls the
    /// link obstructed, metres.
    ///
    /// Zero is the geometric definition — grazing incidence, `ν = 0`, about 6 dB of
    /// diffraction loss — and is the default. A caller that wants first-Fresnel-zone
    /// clearance instead passes a negative value.
    pub obstruction_threshold_m: f64,
}

impl Default for ProfileParams {
    fn default() -> Self {
        Self {
            sample_spacing_m: DEM_POST_SPACING_M,
            max_samples: 1_024,
            obstruction_threshold_m: 0.0,
        }
    }
}

impl ProfileParams {
    /// The parameters with a different sampling interval.
    #[must_use]
    pub fn sample_spacing_m(mut self, spacing_m: f64) -> Self {
        self.sample_spacing_m = spacing_m;
        self
    }

    /// Checks that the parameters describe a profile that can be taken.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`], naming the parameter.
    pub fn validate(&self) -> Result<()> {
        if !(self.sample_spacing_m.is_finite() && self.sample_spacing_m > 0.0) {
            return Err(WorldError::InvalidParameter {
                parameter: "sample_spacing_m".to_string(),
                problem: format!("{} m is not a positive spacing", self.sample_spacing_m),
            });
        }
        if self.max_samples < MIN_PROFILE_SAMPLES {
            return Err(WorldError::InvalidParameter {
                parameter: "max_samples".to_string(),
                problem: format!(
                    "{} samples cannot hold an interior point; the minimum is {MIN_PROFILE_SAMPLES}",
                    self.max_samples
                ),
            });
        }
        if !self.obstruction_threshold_m.is_finite() {
            return Err(WorldError::InvalidParameter {
                parameter: "obstruction_threshold_m".to_string(),
                problem: format!("{} m is not finite", self.obstruction_threshold_m),
            });
        }
        Ok(())
    }
}

/// One sample of a terrain profile.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProfileSample {
    /// Distance from the first endpoint, measured in the horizontal plane, metres.
    pub s_m: f64,
    /// Where the sample is, horizontally.
    pub x_m: f64,
    /// And in the other axis.
    pub y_m: f64,
    /// The ground height there, metres — the terrain grid's answer, or `0.0` where the
    /// world has no grid or the point is off it.
    pub ground_m: f64,
    /// The height of the straight line `a → b` there, metres.
    pub line_m: f64,
}

impl ProfileSample {
    /// How far the ground rises above the line: positive is an obstruction.
    pub fn clearance_m(&self) -> f64 {
        self.ground_m - self.line_m
    }

    /// The sample as a point on the ground.
    pub fn ground_point(&self) -> Vec3 {
        Vec3::new(self.x_m, self.y_m, self.ground_m)
    }
}

/// One knife edge the terrain puts in a link's way.
///
/// The three distances are exactly the arguments of the P.526 parameter
/// `ν = h·sqrt(2(d1 + d2)/(λ·d1·d2))` (04-models.md §3.5), so a propagation model maps
/// this onto its own edge type field for field and computes `ν` without recomputing any
/// geometry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TerrainEdge {
    /// Distance from the first endpoint to the edge, horizontal metres.
    pub d1_m: f64,
    /// Distance from the edge to the second endpoint, horizontal metres.
    pub d2_m: f64,
    /// How far the edge stands above the straight line between the endpoints, metres.
    pub h_m: f64,
    /// Where the edge is.
    pub point: Vec3,
}

/// The elevation profile along one link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainProfile {
    /// The first endpoint, as given.
    pub a: Vec3,
    /// The second endpoint, as given.
    pub b: Vec3,
    /// The link's horizontal length, metres.
    pub total_m: f64,
    /// The spacing actually used, which is the requested one unless
    /// [`ProfileParams::max_samples`] coarsened it.
    pub spacing_m: f64,
    /// False when the world carries no terrain grid, in which case every `ground_m` is
    /// `0.0` and the profile can obstruct nothing.
    pub has_terrain: bool,
    /// How far above the line the ground had to rise to count as an obstruction.
    pub obstruction_threshold_m: f64,
    /// The samples, from `a` to `b` inclusive.
    pub samples: Vec<ProfileSample>,
}

impl TerrainProfile {
    /// True if no sample rises above the line by more than the threshold.
    pub fn is_clear(&self) -> bool {
        !self
            .samples
            .iter()
            .any(|s| s.clearance_m() > self.obstruction_threshold_m)
    }

    /// The greatest height by which the ground rises above the line, metres, or `0.0`
    /// when the link is clear.
    pub fn max_obstruction_m(&self) -> f64 {
        self.samples
            .iter()
            .map(ProfileSample::clearance_m)
            .fold(0.0, f64::max)
    }

    /// The first sample that obstructs the link, in order from `a`.
    pub fn first_obstruction(&self) -> Option<&ProfileSample> {
        self.samples
            .iter()
            .find(|s| s.clearance_m() > self.obstruction_threshold_m)
    }

    /// How much of the link's length runs through ground above the line, metres.
    ///
    /// Each obstructing interior sample stands for one spacing of the path, and each
    /// obstructing endpoint for half a spacing, which is the trapezoidal reading of the
    /// same set. It is an estimate at the profile's resolution, not an exact intersection
    /// length, and the resolution is [`TerrainProfile::spacing_m`].
    pub fn obstructed_length_m(&self) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }
        let last = self.samples.len() - 1;
        let mut weight = 0.0;
        for (i, s) in self.samples.iter().enumerate() {
            if s.clearance_m() <= self.obstruction_threshold_m {
                continue;
            }
            weight += if i == 0 || i == last { 0.5 } else { 1.0 };
        }
        (weight * self.spacing_m).min(self.total_m)
    }

    /// The knife edges along the profile: every local maximum of the clearance that
    /// stands above the line, in order from `a`.
    ///
    /// "Local maximum" is the same test `v2xw-radio`'s terrain model applies — strictly
    /// greater than the previous sample and at least the next, so a flat-topped ridge
    /// yields its first sample and not every sample along it — and only interior samples
    /// qualify: an endpoint is an antenna, not a hill.
    pub fn edges(&self) -> Vec<TerrainEdge> {
        let n = self.samples.len();
        if n < 3 || self.total_m <= 0.0 {
            return Vec::new();
        }
        let mut edges = Vec::new();
        for i in 1..n - 1 {
            let here = self.samples[i].clearance_m();
            if here <= self.obstruction_threshold_m {
                continue;
            }
            if here > self.samples[i - 1].clearance_m() && here >= self.samples[i + 1].clearance_m()
            {
                // Both distances are held off zero: `ν` divides by `d1·d2`, and a hill
                // exactly at an antenna is a hill at one spacing from it as far as this
                // profile's resolution can tell.
                let d1 = self.samples[i].s_m.max(1e-6);
                let d2 = (self.total_m - self.samples[i].s_m).max(1e-6);
                edges.push(TerrainEdge {
                    d1_m: d1,
                    d2_m: d2,
                    h_m: here,
                    point: self.samples[i].ground_point(),
                });
            }
        }
        edges
    }
}

/// The verdict on one link, for a caller that does not want the whole profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainLos {
    /// True if the terrain does not obstruct the link.
    pub clear: bool,
    /// The greatest height by which the ground rises above the line, metres.
    pub max_obstruction_m: f64,
    /// How much of the link runs through obstructing ground, metres.
    pub obstructed_len_m: f64,
    /// The knife edges, for a diffraction model.
    pub edges: Vec<TerrainEdge>,
}

impl TerrainLos {
    /// The verdict for a link nothing obstructs.
    pub fn clear() -> Self {
        Self {
            clear: true,
            max_obstruction_m: 0.0,
            obstructed_len_m: 0.0,
            edges: Vec::new(),
        }
    }
}

/// Samples the ground along the segment `a → b` (04-models.md §1.4, §3.5).
///
/// The number of intervals is `ceil(total / spacing)`, at least
/// [`MIN_PROFILE_SAMPLES`] − 1 and at most [`ProfileParams::max_samples`] − 1, and the
/// samples are evenly spaced from `a` to `b` inclusive. A degenerate link — the two
/// endpoints at the same place — gives the two endpoint samples and no edges.
///
/// The ground height is [`World::ground_height_at`], which answers from the terrain grid
/// where the world has one and returns `0.0` where it does not, so a flat world produces
/// a flat profile rather than an error.
///
/// # Errors
///
/// [`WorldError::InvalidParameter`] for parameters [`ProfileParams::validate`] rejects,
/// or [`WorldError::NonFinite`] for an endpoint that is not a finite point.
pub fn terrain_profile(
    world: &World,
    a: Vec3,
    b: Vec3,
    params: &ProfileParams,
) -> Result<TerrainProfile> {
    params.validate()?;
    for (what, p) in [("a", a), ("b", b)] {
        if !p.is_finite() {
            return Err(WorldError::NonFinite {
                what: format!("terrain profile endpoint {what}"),
            });
        }
    }
    let total_m = a.distance_2d(b);
    let intervals = if total_m <= 0.0 {
        1
    } else {
        let wanted = (total_m / params.sample_spacing_m).ceil();
        let capped = wanted.min((params.max_samples - 1) as f64);
        (capped as usize).max(MIN_PROFILE_SAMPLES - 1)
    };
    // A degenerate link has no length to divide up, so it reports the spacing it was
    // asked for rather than zero.
    let spacing_m = if total_m > 0.0 {
        total_m / intervals as f64
    } else {
        params.sample_spacing_m
    };
    let mut samples = Vec::with_capacity(intervals + 1);
    for i in 0..=intervals {
        let f = i as f64 / intervals as f64;
        let x = a.x + (b.x - a.x) * f;
        let y = a.y + (b.y - a.y) * f;
        samples.push(ProfileSample {
            s_m: total_m * f,
            x_m: x,
            y_m: y,
            ground_m: world.ground_height_at(x, y),
            line_m: a.z + (b.z - a.z) * f,
        });
    }
    Ok(TerrainProfile {
        a,
        b,
        total_m,
        spacing_m,
        has_terrain: world.terrain.is_some(),
        obstruction_threshold_m: params.obstruction_threshold_m,
        samples,
    })
}

/// Whether the terrain obstructs the segment `a → b`, and by how much.
///
/// A world with no terrain grid is answered without sampling at all: its ground is the
/// datum plane, so [`TerrainLos::clear`] is the whole answer and no allocation is made.
///
/// # Errors
///
/// As [`terrain_profile`].
pub fn terrain_los(world: &World, a: Vec3, b: Vec3, params: &ProfileParams) -> Result<TerrainLos> {
    if world.terrain.is_none() {
        return Ok(TerrainLos::clear());
    }
    let profile = terrain_profile(world, a, b, params)?;
    Ok(TerrainLos {
        clear: profile.is_clear(),
        max_obstruction_m: profile.max_obstruction_m(),
        obstructed_len_m: profile.obstructed_length_m(),
        edges: profile.edges(),
    })
}

/// The model card of the terrain-profile query (03-interfaces.md §12).
///
/// It is published as a card, and not left as three constants in a function, because the
/// sampling interval is a modelling choice that changes which hills a link sees: a
/// reader of a result has to be able to find out what it was and where it came from.
pub fn card() -> ModelCard {
    let post_spacing = Source {
        kind: SourceKind::Dataset,
        reference: "SRTMGL1 (1 arc-second) and Copernicus COP-DEM-GLO-30 post spacing, \
                    via 04-models.md §1.4"
            .to_string(),
        accessed: None,
        note: Some(
            "Sampling at the source's own post spacing is the only interval that neither \
             invents detail nor discards it."
                .to_string(),
        ),
    };
    let geometry = Source::new(
        SourceKind::Code,
        "local-maximum test of the profile, as ITU-R P.526-14 Eq. 26/31/33 requires it \
         (04-models.md §3.5)",
    );
    ModelCard {
        tier: vec![Tier::High],
        equations: vec![
            Equation::new(
                "line height",
                "line(f) = a_z + (b_z − a_z)·f, f = s/total in the horizontal plane",
            ),
            Equation::new("clearance", "h(s) = ground(s) − line(s); h > 0 obstructs"),
            Equation {
                name: "knife edge".to_string(),
                latex_or_text: "an interior sample i with h_i > threshold, h_i > h_{i−1} and \
                                h_i >= h_{i+1}, contributing (d1, d2, h) = (s_i, total − s_i, h_i)"
                    .to_string(),
                notes: Some(
                    "These are the arguments of ν = h·sqrt(2(d1 + d2)/(λ·d1·d2)); the \
                     square root and J(ν) belong to the propagation model."
                        .to_string(),
                ),
            },
        ],
        parameters: vec![
            Parameter::new(
                "sample_spacing_m",
                "m",
                DEM_POST_SPACING_M.into(),
                post_spacing.clone(),
            ),
            Parameter {
                name: "max_samples".to_string(),
                unit: "-".to_string(),
                default: 1_024.into(),
                range: None,
                source: Source::todo_calibrate("los-profile max_samples"),
                calibration: Some(
                    "A guard on the per-link allocation, not a physical value. 1 024 \
                     samples is 30 km at the default spacing, which no 5.9 GHz link \
                     reaches. Measure the link-length distribution of the Phase 3 \
                     scenarios and set the cap so that no link the radio stack evaluates \
                     is coarsened, then assert it in the validation suite."
                        .to_string(),
                ),
            },
            Parameter::new("obstruction_threshold_m", "m", 0.0.into(), geometry.clone()),
            Parameter::new(
                "min_profile_samples",
                "-",
                (MIN_PROFILE_SAMPLES as u64).into(),
                geometry,
            ),
        ],
        assumptions: vec![
            "The line between the two antennas is straight: no atmospheric refraction and \
             no earth curvature. Over the 1 km at which a 5.9 GHz V2X link is already \
             below sensitivity, the earth's bulge is about 2 cm, which is under the \
             height quantum."
                .to_string(),
            "The ground between two samples is the linear interpolation the terrain grid \
             gives, so a feature narrower than the sampling interval is invisible — as it \
             is in the source data, whose posts are the same width apart."
                .to_string(),
            "Endpoint z values are absolute world heights, antenna height included. It is \
             the caller's job to add the antenna height to the ground, which is what \
             `Site::antenna_position` does."
                .to_string(),
        ],
        limitations: vec![
            "Buildings, vehicles and foliage are not in the profile: the other obstacle \
             models of 04-models.md §3.5 own those, and adding them here would double-count \
             them."
                .to_string(),
            "The obstructed length is a trapezoidal estimate at the profile's resolution, \
             not an exact intersection of the line with the surface."
                .to_string(),
        ],
        ignores: vec![
            "Diffraction itself: this query returns geometry, and \
             `obstacle/terrain/knife-edge-p526` turns it into decibels."
                .to_string(),
        ],
        sources: vec![post_spacing],
        validation: Validation::new(ValidationStatus::UnitTested),
        determinism: Determinism {
            uses_rng: false,
            rng_domains: Vec::new(),
        },
        ..ModelCard::new(
            MODEL_ID,
            Family::World,
            MODEL_VERSION,
            "The terrain elevation profile along a link, and the knife edges it puts in \
             the way: the world-side query that the P.526 terrain diffraction model of \
             04-models.md §3.5 consumes.",
        )
    }
}
