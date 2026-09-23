//! Mixed radio tiers: the focus region of 02-architecture.md §7.3.
//!
//! # What the fidelity ladder is for
//!
//! The ladder ([`Tier`]) exists so that a run can be expensive where it matters and cheap
//! everywhere else. A *focus region* `F` — a circle around a followed node, or a box — runs
//! at the high tier; the rest of the world runs at the cheap one. 02-architecture.md §7.3
//! answers the brief's question ("can full PHY around the followed vehicle and abstract
//! elsewhere be made sound?") with **"sound with a stated bias, under these rules"**, and
//! this module is those rules:
//!
//! 1. A link with both endpoints in `F` is evaluated at the focus tier.
//! 2. A link with both endpoints outside `F` is evaluated at the surrounding tier.
//! 3. A transmission originating outside `F` but within `range_max` of it still creates
//!    arrivals inside `F`. Its received power at an `F`-receiver is computed with the
//!    **medium-tier propagation model, deterministically**, so the interference inside `F`
//!    is complete and the high-tier region is exact.
//! 4. A transmission originating inside `F` is received outside `F` **by the cheap tier's
//!    rule**. This is the one direction that carries a bias, and it is the bias the
//!    roadmap's Phase 3 acceptance criterion bounds.
//!
//! The receiver therefore decides the tier, and the crossing direction decides whether the
//! propagation stack is the full one or the deterministic one. [`FocusPlan::evaluate`]
//! returns exactly that decision, and nothing else in the crate has to know a focus region
//! exists: the engine picks one of two already-built stacks from
//! [`LinkEvaluation::phy_tier`], which is what "a scenario swaps a tier by changing one
//! field" means in practice.
//!
//! # The bias, and why 5 percentage points is the right bound
//!
//! Rule 4 is where the error lives. A receiver just outside `F` gets a frame from inside
//! `F` through the cheap tier's rule — at the abstract tier, a Bernoulli draw against
//! `P_rx(distance, load)` from a table calibrated on *homogeneous* conditions
//! ([`crate::abstract_tier`]). Nothing about that draw knows the transmitter was evaluated
//! at high fidelity, so the delivery a receiver sees changes discontinuously as it crosses
//! the boundary, by however much the abstract table disagrees with the high tier at that
//! distance and load.
//!
//! That quantity has a name and a bound already: 04-models.md §4.9 step 5 accepts an
//! abstract table only when "for each 25 m bin at each calibration density the abstract
//! PDR must be within 5 percentage points of the high-tier PDR". The boundary discontinuity
//! **is** that per-bin disagreement, evaluated at the boundary rather than averaged over
//! the run. So the Phase 3 criterion ("focus-region boundary bias ≤ 5 percentage points
//! PDR at the calibration density") is satisfied *by construction* for any table that
//! passed its own acceptance test, and is violated exactly when the table is used outside
//! the envelope it was calibrated for — which is why [`TableEnvelope`] is part of the table
//! and why [`FocusPlan::warnings`] refuses to stay quiet about it.
//!
//! [`crate::abstract_tier::AcceptanceReport`] measures the table-wide version of the gap;
//! [`BoundaryBiasMeter`] here measures the boundary version, which is the statistic
//! 02-architecture.md §7.3 asks the validation suite for and the number a report quotes.
//!
//! [`TableEnvelope`]: crate::abstract_tier::TableEnvelope
//!
//! # Where it is not sound, and the warning that says so
//!
//! §7.3 also states the limit: "CSMA back-off state of nodes outside `F` is not modelled,
//! so hidden-terminal effects that originate outside `F` are under-represented inside
//! `F`. Experiments whose object of study is MAC-level contention must run homogeneous
//! `high`". [`FocusPlan::warnings`] returns
//! [`FocusWarning::MacContentionUnderRepresented`] whenever a focus region is combined
//! with `mac: high`, which is the warning the scenario validator is asked to emit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::Tier;
use v2xw_core::geom::{Bbox, Vec3};
use v2xw_core::ids::NodeId;
use v2xw_core::math;

use crate::abstract_tier::{DISTANCE_BIN_M, PDR_TOLERANCE};
use crate::numeric;

/// The boundary-bias bound the roadmap's Phase 3 acceptance criterion states, in
/// percentage points of packet delivery (02-architecture.md §7.3, 10-roadmap.md Phase 3,
/// invariant I-R4).
///
/// The same five points as [`crate::abstract_tier::PDR_TOLERANCE`], expressed in the unit
/// the criterion is written in, because it is the same quantity measured at the boundary
/// rather than over the table. Defined in terms of it so the two can never drift apart.
pub const BOUNDARY_BIAS_TOLERANCE_PP: f64 = PDR_TOLERANCE * 100.0;

/// How wide a band either side of the boundary the bias is measured over, metres.
///
/// Two 25 m distance bins ([`crate::abstract_tier::DISTANCE_BIN_M`]). A band has to be
/// wide enough to collect receivers — a zero-width boundary has no samples — and narrow
/// enough that "just inside" and "just outside" are the same place as far as the link
/// geometry is concerned. Two bins is the narrowest band that is a whole number of the
/// bins the tolerance itself is stated per, and it is recorded on the report so a reader
/// can see what "at the boundary" meant.
pub const DEFAULT_BOUNDARY_BAND_M: f64 = 2.0 * DISTANCE_BIN_M;

/// How many observations a side of a bin needs before its gap is judged.
///
/// Thirty: below that the binomial standard error on a delivery ratio near one half
/// exceeds the nine percentage points that the five-point tolerance is meant to detect, so
/// a bin judged on fewer samples would fail or pass on noise. Bins below the floor are
/// reported with `judged = false` rather than dropped, so a report shows how much of the
/// boundary was actually measured.
pub const MIN_BOUNDARY_TRIALS: u64 = 30;

// =========================================================================================
// The region
// =========================================================================================

/// Where the high-fidelity region is, in world-local metres.
///
/// The scenario spells it `follow:<node>` or a geodetic box (03-interfaces.md §1044,
/// `radio.tiers.focus`); both resolve to one of these two shapes before the radio sees it.
/// A `follow` region is a circle the engine re-centres as the followed node moves
/// ([`FocusPlan::recentre`]), because resolving a node's position is the engine's job:
/// this crate is handed endpoints, not an actor index.
///
/// Containment and distance are **horizontal**. A focus region is a piece of the map, and
/// making it a sphere would put a vehicle on a hill outside a region its neighbours are
/// inside.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FocusShape {
    /// A disc of `radius_m` about `centre`, in the horizontal plane.
    Circle {
        /// The centre, world-local metres.
        centre: Vec3,
        /// The radius, metres.
        radius_m: f64,
    },
    /// An axis-aligned box, tested in the horizontal plane.
    Box {
        /// The box.
        bbox: Bbox,
    },
}

impl FocusShape {
    /// A disc.
    #[must_use]
    pub const fn circle(centre: Vec3, radius_m: f64) -> Self {
        FocusShape::Circle { centre, radius_m }
    }

    /// A box.
    #[must_use]
    pub const fn bbox(bbox: Bbox) -> Self {
        FocusShape::Box { bbox }
    }

    /// True when `p` is inside the region or on its boundary.
    #[must_use]
    pub fn contains(&self, p: Vec3) -> bool {
        self.signed_distance_m(p) <= 0.0
    }

    /// The signed horizontal distance from `p` to the boundary, metres: negative inside,
    /// zero on it, positive outside.
    ///
    /// Signed rather than absolute because the bias measurement needs the side, and a
    /// caller that only wants the side should not have to ask twice.
    #[must_use]
    pub fn signed_distance_m(&self, p: Vec3) -> f64 {
        match *self {
            FocusShape::Circle { centre, radius_m } => {
                let dx = p.x - centre.x;
                let dy = p.y - centre.y;
                math::sqrt(dx * dx + dy * dy) - radius_m
            }
            FocusShape::Box { bbox } => {
                // Outside: the distance to the nearest point of the box. Inside: minus the
                // distance to the nearest face.
                let out_x = (bbox.min.x - p.x).max(p.x - bbox.max.x).max(0.0);
                let out_y = (bbox.min.y - p.y).max(p.y - bbox.max.y).max(0.0);
                if out_x > 0.0 || out_y > 0.0 {
                    return math::sqrt(out_x * out_x + out_y * out_y);
                }
                let inward = (p.x - bbox.min.x)
                    .min(bbox.max.x - p.x)
                    .min(p.y - bbox.min.y)
                    .min(bbox.max.y - p.y);
                -inward
            }
        }
    }

    /// True when `p` is inside the region or within `margin_m` of it.
    ///
    /// This is the `range_max` skirt of rule 3: a transmitter here can reach a receiver
    /// inside `F`, so the engine must still evaluate it even though it is outside.
    #[must_use]
    pub fn within_m(&self, p: Vec3, margin_m: f64) -> bool {
        self.signed_distance_m(p) <= margin_m.max(0.0)
    }

    /// The same shape re-centred on `centre`; a box is translated so its centre moves.
    #[must_use]
    pub fn recentred(self, centre: Vec3) -> Self {
        match self {
            FocusShape::Circle { radius_m, .. } => FocusShape::Circle { centre, radius_m },
            FocusShape::Box { bbox } => {
                let half = bbox.size().scale(0.5);
                FocusShape::Box {
                    bbox: Bbox::new(centre - half, centre + half),
                }
            }
        }
    }

    /// A box containing the region grown by `margin_m` — the region a spatial query has to
    /// cover to find every transmitter that can reach inside.
    #[must_use]
    pub fn envelope(&self, margin_m: f64) -> Bbox {
        let m = margin_m.max(0.0);
        match *self {
            FocusShape::Circle { centre, radius_m } => {
                let r = radius_m + m;
                Bbox::new(
                    Vec3::new(centre.x - r, centre.y - r, centre.z),
                    Vec3::new(centre.x + r, centre.y + r, centre.z),
                )
            }
            FocusShape::Box { bbox } => bbox.expand(m),
        }
    }
}

// =========================================================================================
// Tier sets and placements
// =========================================================================================

/// The three radio families a scenario picks a tier for (02-architecture.md §7.1,
/// `radio.tiers` in 03-interfaces.md §1044).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioTierSet {
    /// Path loss, shadowing, obstacles and fading.
    pub propagation: Tier,
    /// The physical layer.
    pub phy: Tier,
    /// Medium access.
    pub mac: Tier,
}

impl RadioTierSet {
    /// All three families at one tier — the homogeneous run.
    #[must_use]
    pub const fn uniform(tier: Tier) -> Self {
        Self {
            propagation: tier,
            phy: tier,
            mac: tier,
        }
    }

    /// The highest tier any of the three runs at, which is what the validator compares a
    /// focus tier against.
    #[must_use]
    pub fn max_tier(&self) -> Tier {
        self.propagation.max(self.phy).max(self.mac)
    }
}

/// Which side of the boundary a link's two endpoints sit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkPlacement {
    /// Both endpoints inside the focus region.
    Inside,
    /// Both outside it.
    Outside,
    /// Transmitter outside, receiver inside — the inbound crossing of rule 3.
    Inbound,
    /// Transmitter inside, receiver outside — the outbound crossing of rule 4, the one
    /// that carries the bias.
    Outbound,
}

impl LinkPlacement {
    /// The spelling used in records and reports, which is also the metric tag
    /// 02-architecture.md §7.3 asks for ("metrics are tagged by tier so cross-region
    /// aggregates are never mixed silently").
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            LinkPlacement::Inside => "inside",
            LinkPlacement::Outside => "outside",
            LinkPlacement::Inbound => "inbound",
            LinkPlacement::Outbound => "outbound",
        }
    }

    /// True when the link crosses the boundary.
    #[must_use]
    pub const fn crosses(self) -> bool {
        matches!(self, LinkPlacement::Inbound | LinkPlacement::Outbound)
    }
}

/// What one link is evaluated with, and what that costs in honesty.
///
/// The engine holds two already-built radio stacks — the focus one and the surrounding one
/// — and picks between them on [`LinkEvaluation::phy_tier`]. It does not need to know why.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinkEvaluation {
    /// Which side of the boundary the link sits on.
    pub placement: LinkPlacement,
    /// The tier of the PHY and MAC that evaluate this arrival — the **receiver's** tier,
    /// because reception is a property of the receiver.
    pub phy_tier: Tier,
    /// The tier of the propagation stack that produces the received power.
    pub propagation_tier: Tier,
    /// Whether the propagation must be evaluated **deterministically**: with the
    /// large-scale terms only and no per-frame fading realisation.
    ///
    /// True on the inbound crossing, which is rule 3's own word: "their receive power at
    /// F-receivers is computed with the medium-tier propagation model (deterministic), so
    /// interference inside F is complete". It is what makes the interference inside the
    /// focus region reproducible without running the high tier over the whole world.
    ///
    /// In this crate's own terms it means one thing: call
    /// [`crate::budget::evaluate`] with [`crate::fading::NoFading`] instead of the
    /// high-tier Nakagami model. Everything else — the path loss, the correlated
    /// shadowing, the obstacle stack — is unchanged, because those are per-link state
    /// rather than per-frame draws and a deterministic power still has to be the right
    /// power.
    pub deterministic_propagation: bool,
    /// Whether this arrival's outcome carries the mixed-tier bias — true exactly on the
    /// outbound crossing (rule 4).
    ///
    /// A metric that aggregates over both sides of the boundary must either exclude these
    /// or report them separately; the tag travels with the evaluation so a consumer cannot
    /// lose it.
    pub biased: bool,
}

impl LinkEvaluation {
    /// The metric tag for this evaluation: the placement and the tier that produced it.
    #[must_use]
    pub fn tag(&self) -> String {
        format!("{}/{}", self.placement.label(), self.phy_tier)
    }
}

/// Why a focus region cannot answer the question a scenario is asking.
///
/// `PartialEq` but not `Eq`: two of the variants carry metres, and a float is not
/// reflexively equal to itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FocusWarning {
    /// `mac: high` with a focus region: the CSMA back-off state of nodes outside `F` is
    /// not modelled, so hidden-terminal effects originating outside `F` are
    /// under-represented inside it (02-architecture.md §7.3).
    MacContentionUnderRepresented {
        /// The MAC tier that was asked for outside the region.
        outside: Tier,
        /// The MAC tier inside it.
        inside: Tier,
    },
    /// The focus tier is not above the surrounding one, so the region costs the boundary
    /// bias and buys nothing.
    FocusNotHigherThanSurroundings {
        /// The focus tier.
        focus: Tier,
        /// The highest surrounding tier.
        surrounding: Tier,
    },
    /// A follow region with a radius that is not a positive number of metres.
    NonPositiveRadius {
        /// The radius that was given.
        radius_m: f64,
    },
    /// The region is smaller than the `range_max` skirt it needs, so almost every link
    /// into it crosses the boundary and the bias dominates.
    RegionSmallerThanRange {
        /// The region's smallest horizontal extent, metres.
        extent_m: f64,
        /// The range the skirt is built for, metres.
        range_max_m: f64,
    },
}

impl FocusWarning {
    /// A one-line explanation, for the validator and the manifest.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            FocusWarning::MacContentionUnderRepresented { outside, inside } => format!(
                "mac '{inside}' inside a focus region with mac '{outside}' outside it: the \
                 CSMA back-off state of nodes outside the region is not modelled, so \
                 hidden-terminal effects originating outside it are under-represented \
                 inside it. An experiment whose object of study is MAC-level contention \
                 must run homogeneous '{inside}' (02-architecture.md §7.3)"
            ),
            FocusWarning::FocusNotHigherThanSurroundings { focus, surrounding } => format!(
                "the focus tier is '{focus}' and the surrounding world already runs at \
                 '{surrounding}': a focus region exists to run *higher* than its \
                 surroundings, so this one costs the mixed-tier boundary bias and buys \
                 nothing (02-architecture.md §7.3)"
            ),
            FocusWarning::NonPositiveRadius { radius_m } => format!(
                "the follow radius is {radius_m} m, and a radius is a positive number of \
                 metres"
            ),
            FocusWarning::RegionSmallerThanRange {
                extent_m,
                range_max_m,
            } => format!(
                "the focus region's smallest extent is {extent_m} m against a range of \
                 {range_max_m} m: nearly every link into the region crosses its boundary, \
                 so the run is mostly the cheap tier's rule with the high tier's cost \
                 (02-architecture.md §7.3)"
            ),
        }
    }
}

// =========================================================================================
// The plan
// =========================================================================================

/// A focus region with the tiers either side of it: 02-architecture.md §7.3's rules as a
/// value the engine can ask.
///
/// Not a [`v2xw_core::model::Model`], and deliberately so: the plug-in families of
/// 03-interfaces.md §4 are closed ([`v2xw_core::card::Family`] is "closed on purpose"), and
/// a tier-selection rule is not a model of anything physical — it is the composition rule
/// 02-architecture.md §7.3 states, applied to models that each carry their own card. What
/// *is* recorded is the resolved tier matrix, which the manifest already pins
/// (02-architecture.md §6.5), plus [`FocusPlan::warnings`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusPlan {
    /// Where the region is.
    pub shape: FocusShape,
    /// The node the region follows, when it was spelled `follow:<node>`. Carried for the
    /// records and the UI; the geometry is always [`FocusPlan::shape`].
    pub follows: Option<NodeId>,
    /// The tiers inside the region.
    pub inside: RadioTierSet,
    /// The tiers outside it.
    pub outside: RadioTierSet,
    /// The propagation tier the inbound crossing is evaluated with (rule 3).
    ///
    /// [`Tier::Medium`] per §7.3, which names the medium-tier propagation model
    /// explicitly. It is a field rather than a constant because a scenario that runs the
    /// surrounding world at `medium` already has that stack built and a scenario that runs
    /// it at `abstract` does not, and the engine needs to know which one to reach for.
    pub crossing_propagation: Tier,
    /// The largest distance a transmission is considered at, metres — the skirt of rule 3.
    ///
    /// [`crate::abstract_tier::RANGE_MAX_M`] by default, which is the same 1,000 m the
    /// abstract table covers and the LOS-highway cutoff of 04-models.md §3.5.
    pub range_max_m: f64,
}

impl FocusPlan {
    /// A plan: high fidelity inside the region, the given tiers outside it.
    #[must_use]
    pub fn new(shape: FocusShape, inside: RadioTierSet, outside: RadioTierSet) -> Self {
        Self {
            shape,
            follows: None,
            inside,
            outside,
            crossing_propagation: Tier::Medium,
            range_max_m: crate::abstract_tier::RANGE_MAX_M,
        }
    }

    /// The shape 02-architecture.md §7.3 describes first: high everywhere inside a circle
    /// around a followed node, abstract everywhere else.
    #[must_use]
    pub fn follow(node: NodeId, centre: Vec3, radius_m: f64) -> Self {
        Self {
            follows: Some(node),
            ..Self::new(
                FocusShape::circle(centre, radius_m),
                RadioTierSet::uniform(Tier::High),
                RadioTierSet::uniform(Tier::Abstract),
            )
        }
    }

    /// The same plan with a caller-chosen skirt.
    #[must_use]
    pub fn with_range_max_m(mut self, range_max_m: f64) -> Self {
        self.range_max_m = range_max_m;
        self
    }

    /// The same plan with a caller-chosen crossing propagation tier.
    #[must_use]
    pub fn with_crossing_propagation(mut self, tier: Tier) -> Self {
        self.crossing_propagation = tier;
        self
    }

    /// Moves the region to a new centre — what the engine calls each time the followed
    /// node's position update lands.
    pub fn recentre(&mut self, centre: Vec3) {
        self.shape = self.shape.recentred(centre);
    }

    /// Where a link's endpoints sit relative to the region.
    #[must_use]
    pub fn placement(&self, tx: Vec3, rx: Vec3) -> LinkPlacement {
        match (self.shape.contains(tx), self.shape.contains(rx)) {
            (true, true) => LinkPlacement::Inside,
            (false, false) => LinkPlacement::Outside,
            (false, true) => LinkPlacement::Inbound,
            (true, false) => LinkPlacement::Outbound,
        }
    }

    /// The coupling rule of 02-architecture.md §7.3, applied to one link.
    ///
    /// The receiver's side chooses the PHY and MAC tier; the crossing direction chooses the
    /// propagation stack and whether the outcome is biased.
    #[must_use]
    pub fn evaluate(&self, tx: Vec3, rx: Vec3) -> LinkEvaluation {
        let placement = self.placement(tx, rx);
        match placement {
            LinkPlacement::Inside => LinkEvaluation {
                placement,
                phy_tier: self.inside.phy,
                propagation_tier: self.inside.propagation,
                deterministic_propagation: false,
                biased: false,
            },
            LinkPlacement::Outside => LinkEvaluation {
                placement,
                phy_tier: self.outside.phy,
                propagation_tier: self.outside.propagation,
                deterministic_propagation: false,
                biased: false,
            },
            // Rule 3. The receiver is inside, so the focus PHY evaluates it and the
            // arrival joins the focus region's interference sums; the power comes from the
            // medium-tier propagation model with no fading realisation, which is what
            // makes the interference inside the region complete and reproducible without
            // running the high tier over the whole world.
            LinkPlacement::Inbound => LinkEvaluation {
                placement,
                phy_tier: self.inside.phy,
                propagation_tier: self.crossing_propagation,
                deterministic_propagation: true,
                biased: false,
            },
            // Rule 4. The receiver is outside, so the cheap tier's rule decides — and this
            // is the one direction whose delivery is discontinuous at the boundary.
            LinkPlacement::Outbound => LinkEvaluation {
                placement,
                phy_tier: self.outside.phy,
                propagation_tier: self.outside.propagation,
                deterministic_propagation: false,
                biased: true,
            },
        }
    }

    /// True when a transmitter at `p` can still reach inside the region and must therefore
    /// be evaluated: inside it, or within [`FocusPlan::range_max_m`] of it (rule 3).
    #[must_use]
    pub fn relevant_transmitter(&self, p: Vec3) -> bool {
        self.shape.within_m(p, self.range_max_m)
    }

    /// The box a spatial query has to cover to find every relevant transmitter.
    #[must_use]
    pub fn query_envelope(&self) -> Bbox {
        self.shape.envelope(self.range_max_m)
    }

    /// Everything §7.3 says a validator should warn about, in a fixed order.
    ///
    /// Empty means the scenario's mixed-tier request is one §7.3 calls sound. The engine's
    /// scenario validator turns each of these into its own diagnostic; they are produced
    /// here so the rule and the warning cannot drift apart.
    #[must_use]
    pub fn warnings(&self) -> Vec<FocusWarning> {
        let mut out = Vec::new();
        if let FocusShape::Circle { radius_m, .. } = self.shape
            && !(radius_m.is_finite() && radius_m > 0.0)
        {
            out.push(FocusWarning::NonPositiveRadius { radius_m });
        }
        let surrounding = self.outside.max_tier();
        let focus = self.inside.max_tier();
        if focus <= surrounding {
            out.push(FocusWarning::FocusNotHigherThanSurroundings {
                focus,
                surrounding,
            });
        }
        if self.inside.mac == Tier::High {
            out.push(FocusWarning::MacContentionUnderRepresented {
                outside: self.outside.mac,
                inside: self.inside.mac,
            });
        }
        let extent = self.smallest_extent_m();
        if extent.is_finite() && extent < self.range_max_m {
            out.push(FocusWarning::RegionSmallerThanRange {
                extent_m: extent,
                range_max_m: self.range_max_m,
            });
        }
        out
    }

    /// The region's smallest horizontal extent, metres: a circle's diameter, a box's
    /// shorter side.
    #[must_use]
    pub fn smallest_extent_m(&self) -> f64 {
        match self.shape {
            FocusShape::Circle { radius_m, .. } => 2.0 * radius_m,
            FocusShape::Box { bbox } => {
                let s = bbox.size();
                s.x.min(s.y)
            }
        }
    }
}

// =========================================================================================
// The boundary-bias measurement
// =========================================================================================

/// One arrival, as the boundary-bias meter needs to see it.
///
/// Every field is something the engine already has when it finishes an arrival: the
/// transmitter and receiver positions give the placement and the signed boundary distance,
/// the link distance gives the bin, and the outcome gives the numerator.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundaryObservation {
    /// The transmitter-receiver distance, metres.
    pub link_distance_m: f64,
    /// The receiver's signed horizontal distance to the boundary, metres: negative inside.
    pub rx_boundary_distance_m: f64,
    /// Whether the transmitter was inside the focus region.
    pub tx_inside: bool,
    /// Whether the frame was decoded.
    pub received: bool,
}

/// One distance bin's boundary comparison.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundaryBinBias {
    /// The distance bin index.
    pub distance_bin: usize,
    /// Its centre, metres.
    pub centre_m: f64,
    /// Delivery to receivers just inside the boundary.
    pub inside_pdr: f64,
    /// Delivery to receivers just outside it.
    pub outside_pdr: f64,
    /// Observations just inside.
    pub inside_trials: u64,
    /// Observations just outside.
    pub outside_trials: u64,
    /// The discontinuity, in percentage points: `|inside − outside| × 100`.
    pub gap_pp: f64,
    /// Whether both sides carried at least [`MIN_BOUNDARY_TRIALS`] observations, so the
    /// gap means something.
    pub judged: bool,
    /// Whether the gap is inside [`BOUNDARY_BIAS_TOLERANCE_PP`]. Always true for a bin that
    /// was not judged, so an unmeasured bin cannot fail the criterion — it is reported as
    /// unmeasured instead.
    pub within_tolerance: bool,
}

/// The Phase 3 acceptance criterion's evidence: the delivery discontinuity at the focus
/// boundary, per distance bin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundaryBiasReport {
    /// Every bin that carried an observation on either side.
    pub bins: Vec<BoundaryBinBias>,
    /// The band either side of the boundary the measurement used, metres.
    pub band_m: f64,
    /// The per-side observation floor a bin had to clear to be judged.
    pub min_trials: u64,
    /// The worst judged gap, percentage points. Zero when nothing was judged.
    pub worst_gap_pp: f64,
    /// How many bins were judged.
    pub judged_bins: usize,
    /// Whether every judged bin is inside tolerance — the Phase 3 criterion.
    pub accepted: bool,
    /// The tolerance the criterion states, percentage points.
    pub tolerance_pp: f64,
}

impl BoundaryBiasReport {
    /// A one-line summary a report or a manifest quotes.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.judged_bins == 0 {
            return format!(
                "focus-region boundary bias: not measured (no distance bin carried \
                 {} observations on both sides of the boundary within ±{} m)",
                self.min_trials, self.band_m
            );
        }
        format!(
            "focus-region boundary bias: worst {:.2} pp over {} judged bin(s) within \
             ±{} m of the boundary, against a {:.1} pp bound — {}",
            self.worst_gap_pp,
            self.judged_bins,
            self.band_m,
            self.tolerance_pp,
            if self.accepted { "within" } else { "EXCEEDED" }
        )
    }
}

/// Measures the delivery discontinuity at the focus boundary (02-architecture.md §7.3).
///
/// # What it compares, and why only that
///
/// Only frames **from inside the focus region** are counted, because rule 4 is the only
/// rule that carries a bias and it is about those frames: a receiver just inside gets them
/// through the focus tier, a receiver just outside gets them through the cheap tier's
/// rule, and the difference between those two answers at the same link distance *is* the
/// bias. Frames from outside the region would add the inbound crossing's own exactness to
/// the numerator and hide it.
///
/// Comparing within a distance bin rather than pooling is what makes the statistic a
/// discontinuity rather than a geometry artefact: a receiver just outside the boundary is
/// on average further from a transmitter inside it, and pooled delivery would fall across
/// the boundary even if both tiers agreed perfectly.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryBiasMeter {
    band_m: f64,
    bin_m: f64,
    min_trials: u64,
    /// `distance bin -> (trials, received)` for receivers just inside the boundary.
    inside: BTreeMap<usize, (u64, u64)>,
    /// The same, just outside.
    outside: BTreeMap<usize, (u64, u64)>,
}

impl Default for BoundaryBiasMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl BoundaryBiasMeter {
    /// A meter with the documented band, bin width and observation floor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            band_m: DEFAULT_BOUNDARY_BAND_M,
            bin_m: DISTANCE_BIN_M,
            min_trials: MIN_BOUNDARY_TRIALS,
            inside: BTreeMap::new(),
            outside: BTreeMap::new(),
        }
    }

    /// A meter with a caller-chosen band, bin width and floor.
    ///
    /// # Panics
    ///
    /// If `band_m` or `bin_m` is not a positive, finite number of metres: a zero band has
    /// no samples and a zero bin has no bins, and both would produce a report that looks
    /// like a measurement.
    #[must_use]
    pub fn with(band_m: f64, bin_m: f64, min_trials: u64) -> Self {
        assert!(
            band_m.is_finite() && band_m > 0.0,
            "the boundary band must be a positive number of metres, got {band_m}"
        );
        assert!(
            bin_m.is_finite() && bin_m > 0.0,
            "the distance bin must be a positive number of metres, got {bin_m}"
        );
        Self {
            band_m,
            bin_m,
            min_trials,
            inside: BTreeMap::new(),
            outside: BTreeMap::new(),
        }
    }

    /// Records one arrival, ignoring everything the comparison is not about: frames from
    /// outside the region, and receivers further than the band from the boundary.
    pub fn record(&mut self, o: BoundaryObservation) {
        if !o.tx_inside || !o.link_distance_m.is_finite() || o.link_distance_m < 0.0 {
            return;
        }
        let d = o.rx_boundary_distance_m;
        if !d.is_finite() || d.abs() > self.band_m {
            return;
        }
        let bin = (o.link_distance_m / self.bin_m) as usize;
        // A receiver exactly on the boundary is counted as inside: the region includes its
        // boundary ([`FocusShape::contains`]), and the two halves must partition the band.
        let side = if d <= 0.0 {
            &mut self.inside
        } else {
            &mut self.outside
        };
        let entry = side.entry(bin).or_insert((0, 0));
        entry.0 += 1;
        if o.received {
            entry.1 += 1;
        }
    }

    /// Records every observation of an iterator.
    pub fn record_all(&mut self, observations: impl IntoIterator<Item = BoundaryObservation>) {
        for o in observations {
            self.record(o);
        }
    }

    /// The report.
    #[must_use]
    pub fn report(&self) -> BoundaryBiasReport {
        let mut keys: Vec<usize> = self.inside.keys().copied().collect();
        keys.extend(self.outside.keys().copied());
        keys.sort_unstable();
        keys.dedup();
        let mut bins = Vec::with_capacity(keys.len());
        let mut worst = 0.0_f64;
        let mut judged_bins = 0usize;
        let mut accepted = true;
        for bin in keys {
            let (it, ir) = self.inside.get(&bin).copied().unwrap_or((0, 0));
            let (ot, or) = self.outside.get(&bin).copied().unwrap_or((0, 0));
            let inside_pdr = ratio(ir, it);
            let outside_pdr = ratio(or, ot);
            let judged = it >= self.min_trials && ot >= self.min_trials;
            // Quantised *before* the comparison, not after. A 100 % / 95 % pair is
            // exactly five percentage points apart, and in binary it is
            // 5.000000000000004: comparing the raw difference against the bound would
            // fail a run at exactly the tolerance the criterion states, and the report
            // would print "5.00 pp — EXCEEDED". The decision is made on the number that
            // is reported (build decision D9's rule applied to a threshold).
            let gap_pp = if judged {
                numeric::q_ratio((inside_pdr - outside_pdr).abs() * 100.0)
            } else {
                0.0
            };
            let within = !judged || gap_pp <= BOUNDARY_BIAS_TOLERANCE_PP;
            if judged {
                judged_bins += 1;
                worst = worst.max(gap_pp);
                accepted &= within;
            }
            bins.push(BoundaryBinBias {
                distance_bin: bin,
                centre_m: math::q3((bin as f64 + 0.5) * self.bin_m),
                inside_pdr: numeric::q_ratio(inside_pdr),
                outside_pdr: numeric::q_ratio(outside_pdr),
                inside_trials: it,
                outside_trials: ot,
                gap_pp,
                judged,
                within_tolerance: within,
            });
        }
        BoundaryBiasReport {
            bins,
            band_m: self.band_m,
            min_trials: self.min_trials,
            worst_gap_pp: numeric::q_ratio(worst),
            judged_bins,
            accepted,
            tolerance_pp: BOUNDARY_BIAS_TOLERANCE_PP,
        }
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Vec3 {
        Vec3::new(x, y, 1.5)
    }

    fn plan() -> FocusPlan {
        FocusPlan::follow(NodeId::new(0), p(0.0, 0.0), 300.0)
    }

    #[test]
    fn a_circle_knows_its_inside_its_outside_and_its_boundary() {
        let s = FocusShape::circle(p(100.0, 0.0), 50.0);
        assert!(s.contains(p(100.0, 0.0)));
        assert!(s.contains(p(150.0, 0.0)), "the boundary is inside");
        assert!(!s.contains(p(150.001, 0.0)));
        assert!((s.signed_distance_m(p(100.0, 0.0)) + 50.0).abs() < 1e-12);
        assert!((s.signed_distance_m(p(200.0, 0.0)) - 50.0).abs() < 1e-12);
        // Height is ignored: a focus region is a piece of the map.
        assert!(s.contains(Vec3::new(100.0, 0.0, 400.0)));
        assert!(s.within_m(p(250.0, 0.0), 100.0));
        assert!(!s.within_m(p(250.0, 0.0), 99.0));
    }

    #[test]
    fn a_box_measures_the_same_signed_distance_inside_and_out() {
        let b = Bbox::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(100.0, 40.0, 10.0));
        let s = FocusShape::bbox(b);
        assert!(s.contains(p(50.0, 20.0)));
        // Inside: minus the distance to the nearest face, which is the y face here.
        assert!((s.signed_distance_m(p(50.0, 5.0)) + 5.0).abs() < 1e-12);
        // Outside on one axis.
        assert!((s.signed_distance_m(p(110.0, 20.0)) - 10.0).abs() < 1e-12);
        // Outside on both: the corner distance.
        let d = s.signed_distance_m(p(103.0, 44.0));
        assert!((d - 5.0).abs() < 1e-12, "3-4-5 corner, got {d}");
        // The envelope contains the grown region.
        let env = s.envelope(20.0);
        assert!(env.contains_2d(p(-19.0, -19.0)));
        assert!(!env.contains_2d(p(-21.0, 20.0)));
    }

    #[test]
    fn a_region_recentres_without_changing_its_size() {
        let mut pl = plan();
        assert_eq!(pl.smallest_extent_m(), 600.0);
        pl.recentre(p(1_000.0, 500.0));
        assert!(pl.shape.contains(p(1_000.0, 500.0)));
        assert!(!pl.shape.contains(p(0.0, 0.0)));
        assert_eq!(pl.smallest_extent_m(), 600.0);

        let mut boxed = FocusPlan::new(
            FocusShape::bbox(Bbox::new(
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(200.0, 100.0, 0.0),
            )),
            RadioTierSet::uniform(Tier::High),
            RadioTierSet::uniform(Tier::Medium),
        );
        assert_eq!(boxed.smallest_extent_m(), 100.0);
        boxed.recentre(p(500.0, 500.0));
        assert_eq!(boxed.smallest_extent_m(), 100.0);
        assert!(boxed.shape.contains(p(500.0, 500.0)));
        assert!(boxed.shape.contains(p(599.0, 549.0)));
        assert!(!boxed.shape.contains(p(601.0, 500.0)));
    }

    /// The coupling rule of 02-architecture.md §7.3, case by case.
    #[test]
    fn the_coupling_rule_is_the_one_the_architecture_states() {
        let pl = plan();
        let inside = p(10.0, 0.0);
        let also_inside = p(-10.0, 0.0);
        let outside = p(500.0, 0.0);
        let far_outside = p(5_000.0, 0.0);

        // 1. Both inside: the focus tier, everything on.
        let e = pl.evaluate(inside, also_inside);
        assert_eq!(e.placement, LinkPlacement::Inside);
        assert_eq!(e.phy_tier, Tier::High);
        assert_eq!(e.propagation_tier, Tier::High);
        assert!(!e.deterministic_propagation);
        assert!(!e.biased);

        // 2. Both outside: the cheap tier.
        let e = pl.evaluate(outside, far_outside);
        assert_eq!(e.placement, LinkPlacement::Outside);
        assert_eq!(e.phy_tier, Tier::Abstract);
        assert!(!e.biased);

        // 3. Inbound: the focus PHY sees it, with deterministic medium propagation, so
        //    the interference inside the region is complete.
        let e = pl.evaluate(outside, inside);
        assert_eq!(e.placement, LinkPlacement::Inbound);
        assert_eq!(e.phy_tier, Tier::High, "the receiver decides the tier");
        assert_eq!(e.propagation_tier, Tier::Medium);
        assert!(e.deterministic_propagation);
        assert!(!e.biased, "the high-tier region is exact");

        // 4. Outbound: the cheap rule, and this is the biased direction.
        let e = pl.evaluate(inside, outside);
        assert_eq!(e.placement, LinkPlacement::Outbound);
        assert_eq!(e.phy_tier, Tier::Abstract);
        assert!(e.biased);
        assert!(e.placement.crosses());
        assert_eq!(e.tag(), "outbound/abstract");
    }

    #[test]
    fn the_range_skirt_keeps_the_interference_into_the_region_complete() {
        let pl = plan().with_range_max_m(1_000.0);
        // Inside the region.
        assert!(pl.relevant_transmitter(p(0.0, 0.0)));
        // Outside, but able to reach in: 300 m radius plus 1,000 m skirt.
        assert!(pl.relevant_transmitter(p(1_299.0, 0.0)));
        assert!(!pl.relevant_transmitter(p(1_301.0, 0.0)));
        let env = pl.query_envelope();
        assert!(env.contains_2d(p(1_299.0, 0.0)));
        assert!(!env.contains_2d(p(1_310.0, 0.0)));
    }

    #[test]
    fn a_high_mac_inside_a_focus_region_warns() {
        let pl = plan();
        let warnings = pl.warnings();
        assert!(
            warnings.iter().any(|w| matches!(
                w,
                FocusWarning::MacContentionUnderRepresented { .. }
            )),
            "{warnings:?}"
        );
        assert!(
            pl.warnings()
                .iter()
                .any(|w| w.message().contains("hidden-terminal"))
        );
        // A focus region no higher than its surroundings buys nothing.
        let flat = FocusPlan::new(
            FocusShape::circle(p(0.0, 0.0), 2_000.0),
            RadioTierSet::uniform(Tier::Medium),
            RadioTierSet::uniform(Tier::Medium),
        )
        .with_range_max_m(1_000.0);
        assert!(flat.warnings().iter().any(|w| matches!(
            w,
            FocusWarning::FocusNotHigherThanSurroundings { .. }
        )));
        // A negative radius is not a region.
        let bad = FocusPlan::new(
            FocusShape::circle(p(0.0, 0.0), -1.0),
            RadioTierSet::uniform(Tier::High),
            RadioTierSet::uniform(Tier::Abstract),
        );
        assert!(
            bad.warnings()
                .iter()
                .any(|w| matches!(w, FocusWarning::NonPositiveRadius { .. }))
        );
        // A region larger than the range, with a medium MAC, is the sound case.
        let sound = FocusPlan::new(
            FocusShape::circle(p(0.0, 0.0), 2_000.0),
            RadioTierSet {
                propagation: Tier::High,
                phy: Tier::High,
                mac: Tier::Medium,
            },
            RadioTierSet::uniform(Tier::Abstract),
        )
        .with_range_max_m(1_000.0);
        assert!(sound.warnings().is_empty(), "{:?}", sound.warnings());
    }

    #[test]
    fn the_bias_meter_compares_within_a_distance_bin_and_only_outbound_frames() {
        let mut meter = BoundaryBiasMeter::with(50.0, 25.0, 4);
        // Bin 4 (100-125 m): four in, four out, delivery 1.0 inside and 0.5 outside.
        for i in 0..4 {
            meter.record(BoundaryObservation {
                link_distance_m: 110.0,
                rx_boundary_distance_m: -10.0,
                tx_inside: true,
                received: true,
            });
            meter.record(BoundaryObservation {
                link_distance_m: 110.0,
                rx_boundary_distance_m: 10.0,
                tx_inside: true,
                received: i % 2 == 0,
            });
        }
        // Ignored: the transmitter was outside, so this is the inbound crossing.
        meter.record(BoundaryObservation {
            link_distance_m: 110.0,
            rx_boundary_distance_m: -1.0,
            tx_inside: false,
            received: false,
        });
        // Ignored: the receiver is far from the boundary.
        meter.record(BoundaryObservation {
            link_distance_m: 110.0,
            rx_boundary_distance_m: 400.0,
            tx_inside: true,
            received: false,
        });
        let r = meter.report();
        assert_eq!(r.bins.len(), 1);
        let b = r.bins[0];
        assert_eq!(b.distance_bin, 4);
        assert_eq!((b.inside_trials, b.outside_trials), (4, 4));
        assert!((b.inside_pdr - 1.0).abs() < 1e-9);
        assert!((b.outside_pdr - 0.5).abs() < 1e-9);
        assert!((b.gap_pp - 50.0).abs() < 1e-6, "{b:?}");
        assert!(b.judged);
        assert!(!b.within_tolerance, "50 pp is far outside the 5 pp bound");
        assert!(!r.accepted);
        assert!((r.worst_gap_pp - 50.0).abs() < 1e-6);
        assert_eq!(r.judged_bins, 1);
        assert!(r.summary().contains("EXCEEDED"), "{}", r.summary());
    }

    /// A calibrated table is within five points per bin of the high tier by construction
    /// (04-models.md §4.9 step 5), so a boundary measured on one is within the Phase 3
    /// bound. The meter has to agree with that arithmetic.
    #[test]
    fn a_gap_at_the_tolerance_passes_and_one_above_it_fails() {
        let make = |outside_received: u64, trials: u64| {
            let mut m = BoundaryBiasMeter::with(50.0, 25.0, 10);
            for _ in 0..trials {
                m.record(BoundaryObservation {
                    link_distance_m: 60.0,
                    rx_boundary_distance_m: -5.0,
                    tx_inside: true,
                    received: true,
                });
            }
            for i in 0..trials {
                m.record(BoundaryObservation {
                    link_distance_m: 60.0,
                    rx_boundary_distance_m: 5.0,
                    tx_inside: true,
                    received: i < outside_received,
                });
            }
            m.report()
        };
        // 95 of 100 outside against 100 of 100 inside: exactly 5 pp, which passes.
        let at = make(95, 100);
        assert!((at.worst_gap_pp - 5.0).abs() < 1e-6, "{:?}", at.worst_gap_pp);
        assert!(at.accepted, "{}", at.summary());
        // 94 of 100: 6 pp, which does not.
        let over = make(94, 100);
        assert!((over.worst_gap_pp - 6.0).abs() < 1e-6);
        assert!(!over.accepted);
        assert_eq!(over.tolerance_pp, 5.0);
    }

    #[test]
    fn an_unmeasured_bin_is_reported_as_unmeasured_and_never_fails() {
        let mut m = BoundaryBiasMeter::new();
        m.record(BoundaryObservation {
            link_distance_m: 30.0,
            rx_boundary_distance_m: -1.0,
            tx_inside: true,
            received: false,
        });
        let r = m.report();
        assert_eq!(r.bins.len(), 1);
        assert!(!r.bins[0].judged);
        assert!(r.bins[0].within_tolerance);
        assert_eq!(r.judged_bins, 0);
        assert!(r.accepted, "nothing was measured, so nothing failed");
        assert!(r.summary().contains("not measured"), "{}", r.summary());
        assert_eq!(r.min_trials, MIN_BOUNDARY_TRIALS);
        assert_eq!(r.band_m, DEFAULT_BOUNDARY_BAND_M);
    }

    #[test]
    fn the_boundary_tolerance_is_the_calibration_tolerance() {
        // The two numbers are the same quantity in different units, and the module says
        // so; if one is ever changed the other has to move with it.
        assert!((BOUNDARY_BIAS_TOLERANCE_PP - PDR_TOLERANCE * 100.0).abs() < 1e-12);
        assert!((BOUNDARY_BIAS_TOLERANCE_PP - 5.0).abs() < 1e-12);
    }

    #[test]
    fn the_report_is_order_independent() {
        let observations: Vec<BoundaryObservation> = (0..40)
            .map(|i| BoundaryObservation {
                link_distance_m: 40.0 + f64::from(i % 7) * 3.0,
                rx_boundary_distance_m: if i % 2 == 0 { -8.0 } else { 8.0 },
                tx_inside: true,
                received: i % 3 != 0,
            })
            .collect();
        let mut forward = BoundaryBiasMeter::with(50.0, 25.0, 4);
        forward.record_all(observations.iter().copied());
        let mut backward = BoundaryBiasMeter::with(50.0, 25.0, 4);
        backward.record_all(observations.iter().rev().copied());
        assert_eq!(forward.report(), backward.report());
    }
}
