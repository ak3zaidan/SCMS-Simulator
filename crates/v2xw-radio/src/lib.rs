//! `v2xw-radio` — propagation, fading, obstacle shadowing, the IEEE 802.11p, LTE-V2X and
//! NR-V2X PHY and MAC layers, the cellular Uu link, decentralised congestion control and
//! channel accounting.
//!
//! This is the crate that turns geometry into decibels and decibels into frame outcomes.
//! It holds the six plug-in families of 03-interfaces.md §4 ([`traits`]) and the models
//! 04-models.md §3, §4 and §6 specify, with every cited constant carrying its clause in
//! the code and in its model card.
//!
//! It never touches message semantics, node behaviour or security: a frame is a length, an
//! MCS and an access category here, and what it carries is [`v2xw-msg`]'s business
//! (02-architecture.md §2, ADR 0010).
//!
//! [`v2xw-msg`]: https://docs.rs/v2xw-msg
//!
//! # Where to look
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | The six family traits | [`traits`] | 03-interfaces.md §4 |
//! | Shared value types, the MCS and EDCA tables, the timing constants | [`types`] | 04-models.md §4.1-§4.6 |
//! | Friis, two-ray, dual-slope log-distance with correlated shadowing, TR 37.885 | [`prop`] | 04-models.md §3.1-§3.3, §3.6 |
//! | Nakagami-m fading, and no fading | [`fading`] | 04-models.md §3.4 |
//! | Buildings, vehicles, terrain knife edges | [`obstacle`] | 04-models.md §3.5 |
//! | The terrain profile query and the knife-edge extraction | [`terrain`] | 04-models.md §3.5 |
//! | How the three compose into one link budget | [`budget`] | 04-models.md §3 tier table |
//! | The NIST packet-error-rate model | [`per`] | 04-models.md §4.7 |
//! | Air time, noise, SINR, capture, frame outcomes, air-time accounting | [`phy`] | 04-models.md §4.2, §4.7, §4.8 |
//! | EDCA in OCB mode, the slotted abstraction, the CBR meter | [`mac`] | 04-models.md §4.3, §4.5 |
//! | The ETSI adaptive and reactive algorithms, the EN 302 571 floor, SAE J2945/1 | [`dcc`] | 04-models.md §6 |
//! | The calibrated abstract tier, the legacy models, the calibration routine | [`abstract_tier`] | 04-models.md §4.9 |
//! | Mixed tiers: the focus region, the coupling rule, the boundary bias | [`focus`] | 02-architecture.md §7.3 |
//! | Jammers: constant, pulsed, reactive, and the noise rise they cause | [`jamming`] | 04-models.md §12.3 |
//! | Sidelink resource structure: numerologies, sub-channels, MCS, SCI, IBE, CBR/CR | [`sidelink`] | 04-models.md §5.1, §5.2 |
//! | The C-V2X block-error lookups and the spectral-efficiency fit | [`bler`] | 04-models.md §5.1 |
//! | Sensing-based semi-persistent scheduling, Mode 4 and Mode 2 | [`sps`] | 04-models.md §5.1, §5.2 |
//! | The sidelink PHY: per-sub-channel SINR, in-band emissions, SCI decoding | [`cv2x`] | 04-models.md §5.1, §5.2, §5.4 |
//! | The cellular Uu link, handover, outage and store-and-forward | [`cellular`] | 04-models.md §10.1 |
//! | Hybrid operation: a node with both radios and a policy | [`hybrid`] | composition over §4-§5 and §10.1 |
//! | The measurement harness behind the §13 validation curves | [`sweep`] | 04-models.md §13 |
//! | The error function, dB arithmetic, ordered power sums | [`numeric`] | ADR 0003, 02-architecture.md §6.3 |
//!
//! # The four properties this crate is built to keep
//!
//! **1. No platform transcendental.** Every `exp`, `log10`, `pow`, `sqrt` and `sin_cos`
//! comes from [`v2xw_core::math`]. The one function this crate needs that the contract
//! crate does not re-export is the error function, and [`numeric`] reaches for the *same*
//! pure-Rust `libm` crate directly and says why in its own documentation. There is no call
//! to `f64::exp` or its siblings anywhere here.
//!
//! **2. Every draw from a keyed stream.** The shadowing innovation comes from
//! `(Shadow, Link)`, the fading sample from `(Fading, LinkFrame)`, the backoff from
//! `(MacBackoff, Node)`, the abstract reception from `(AbstractRx, LinkFrame)` and the
//! frame error from `(plugin(phy id), LinkFrame)`. No model holds a generator, so one
//! model's draws never depend on what another model did or on the order receivers were
//! evaluated in — which is invariant I-R2, and what makes the phase-parallel receiver map
//! of 02-architecture.md §6.4 legitimate.
//!
//! **3. Ordered reductions.** Interference sums go through
//! [`numeric::sum_powers_mw`], which sorts by [`v2xw_core::ids::NodeId`] before reducing
//! with [`v2xw_core::math::sum_ordered`]. Floating-point addition is not associative, and
//! a SINR one bit apart can land on either side of a PER threshold.
//!
//! **4. No `HashMap` reaching an output.** Per-link, per-node and per-arrival state lives
//! in `BTreeMap`s keyed by dense integers, so a dump, a digest or an iteration order is
//! the same on every run and every platform.
//!
//! # What "cited" means here, and where it stops
//!
//! Every default carries a [`v2xw_core::card::Source`]. Where 04-models.md marks a
//! constant UNVERIFIED or does not print it, this crate does one of three things and says
//! which:
//!
//! * ships the value with `SourceKind::TodoCalibrate` and a calibration plan, and
//!   registers the model `unvalidated` — the Cheng and Kunisch path-loss presets, the Yin
//!   fading preset's two `m` values, the J2945/1 tracking-error sensitivity, the Sommer
//!   material mapping, the borrowed near slope of `abbas-olos-highway`;
//! * **does not ship the model at all** — the `karedal-*` presets ("not shippable until
//!   read"), `abbas-nlos-intersection` (no `PL0`, and a different functional form),
//!   `fading/taliwal-ns2` ("not shipped until confirmed"), `two-ray-interference`
//!   (UNVERIFIED ground permittivity), `propagation/winner-plus-b1` (every coefficient
//!   `TODO: calibrate`), `fading/rician` (no cited K for 5.9 GHz V2V);
//! * or, where a standard's printed equation looked wrong, re-reads it until it is not.
//!   The EN 302 571 / TS 103 175 idle-time bound was carried here for a while as a
//!   documented disagreement with the standard, with a unit parameter and an
//!   "unexplained 2 % residual" built on top of it; it was a misplaced parenthesis, and
//!   the correct grouping reproduces both of the standard's worked examples to 0.01 %
//!   ([`dcc`]).
//!
//! # Three divergences from the published signatures
//!
//! [`traits`] documents them in full: `begin_tx` returns the transmission's deadline
//! instead of scheduling it and returns a `Result` so it can refuse an oversized frame,
//! `Propagation::loss_db` takes `&mut self` because correlated shadowing is per-link
//! state, and `Mac` has a `poll` so the engine can drive the access state machine without
//! the MAC constructing event payloads it cannot name. All three exist because the event
//! payload enum lives in the engine crate (build decision D8), and all three are one line
//! to change once it exists.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod abstract_tier;
pub mod bler;
pub mod budget;
pub mod cellular;
pub mod cv2x;
pub mod dcc;
pub mod error;
pub mod fading;
pub mod focus;
pub mod hybrid;
pub mod jamming;
pub mod mac;
pub mod numeric;
pub mod obstacle;
pub mod per;
pub mod phy;
pub mod prop;
pub mod sidelink;
pub mod sps;
pub mod sweep;
pub mod terrain;
pub mod traits;
pub mod types;

#[cfg(test)]
pub mod testctx;

pub use error::{RadioError, Result};
pub use traits::{Dcc, Fading, Mac, ObstacleModel, Phy, Propagation};
pub use types::{
    AccessCategory, ActorClass, ActorObstacle, ActorSet, CcaState, ChannelId, CodeRate,
    CornerGeometry, DccAlgorithm, DccState, DropCause, EdgeSource, FrameDescriptor, FrameKind,
    GateDecision, KnifeEdge, LosClass, LosResult, LossBreakdown, LossCause, MacSdu, Mcs,
    Modulation, PatternRef, RadioEndpoint, Rat, ReactiveState, ResourceModel, RxHandle, RxOutcome,
    SduRef, TxGrant, TxHandle, TxRequest, timing,
};

pub use abstract_tier::{
    AbstractPhy, AcceptanceReport, CalibratedAbstractTier, CalibrationFit, CalibrationPlan,
    CalibrationRequest, CalibrationRun, Cell, DistanceLoadTable, LegacyAbstractPhy, LegacyKind,
    LegacyParams, LoadAxis, ReceptionSample, TableEnvelope, calibrate,
};
pub use budget::{LinkBudget, classify, evaluate, merge_los};
pub use dcc::{
    AdaptiveDcc, AdaptiveParams, En302571Floor, J2945Params, ReactiveDcc, ReactiveTable,
    SaeJ2945Dcc,
};
pub use fading::{NakagamiFading, NakagamiPreset, NoFading};
pub use focus::{
    BOUNDARY_BIAS_TOLERANCE_PP, BoundaryBiasMeter, BoundaryBiasReport, BoundaryBinBias,
    BoundaryObservation, FocusPlan, FocusShape, FocusWarning, LinkEvaluation, LinkPlacement,
    RadioTierSet,
};
pub use jamming::{
    ConstantJammer, JamArrival, JamWindow, JammerKind, JammerProfile, JammingField, PulsedJammer,
    ReactiveJammer, SensedInterval, blind_area_radius_m, punal_rssi_to_sinr_db,
};
pub use mac::{Backoff, CbrMeter, EdcaOcbMac, SlottedMac};
pub use obstacle::{
    BuildingIndex, BuildingShadowing, CornerTracer, MultiEdgeRule, NlosvCase, SommerFit,
    TerrainDiffraction, VehicleBlockage, first_wall_m, knife_edge_loss_db,
    knife_edge_loss_exact_db, knife_edge_parameter, multi_edge_loss_db, segment_blocked,
};
pub use per::{PerModel, PerPreset, coded_error_probability, data_field_bits, uncoded_ber};
pub use phy::{
    AirtimeLedger, Arrival, CaptureRule, CcaConfig, InterferenceSource, OfdmPhy, SensitivityPreset,
    air_time, sinr_db,
};
pub use prop::{
    DualSlope, FreeSpace, GeometricState, GeometricUrbanV2v, LogDistancePreset,
    LogDistanceShadowing, Polarization, RainAttenuation, ShadowProcess, SommerCoefficients,
    Tr37885, Tr37885State, TwoRayGround, friis_loss_db, mangel_breakpoint_m, mangel_nlos_db,
    p838_coefficients, rain_attenuation_db, two_ray_ground_loss_db,
};
pub use terrain::{
    EdgeExtraction, GroundProfile, ProfilePoint, TerrainProfile, any_edge_obstructs, knife_edges,
    radio_line_height_m,
};

pub use bler::{
    BlerCurve, CurveProvenance, SeAnchor, SeGapFit, SidelinkErrorModel, WILAB_LTE_SINR_AT_10PC,
    WilabRow, cited_anchors, wilab_sinr_at_10pc,
};
pub use cellular::{
    CellCapacityUu, CellPlan, CellQuality, CellView, CellularUu, Direction, FixedLatencyUu,
    HandoverKind, HandoverOutageUu, LatencySpec, MecPlacement, Qos, RadioLatencyClass, SendOutcome,
    StoreAndForward, UuLatencyPreset, loss_at_percentile, uu_path_loss_db,
};
pub use cv2x::{
    InterferenceSplit, SIDELINK_SENSITIVITY_DBM, SIDELINK_TX_POWER_DBM, SidelinkPhy, SlArrival,
    SlInterferer, THERMAL_NOISE_DBM_PER_HZ, UE_NOISE_FIGURE_DB,
};
pub use hybrid::{
    Destination, HybridDecision, HybridPolicy, HybridReason, HybridRequest, HybridSelector,
    RadioChoice,
};
pub use sidelink::{
    IbeMask, NrMcsTable, Numerology, PoolConfig, ProbResourceKeep, Rri, SciSize, SidelinkOccupancy,
    SlMcsSpec, SlRat, SlResource, TxPercentage, cr_limit, nr_mcs, rsrp_threshold_dbm,
};
pub use sps::{
    Reservation, SelectionOutcome, SelectionReason, SensingHistory, SpsEngine, SpsParams,
    slot_air_time,
};
pub use sweep::{
    BinStats, ChannelModel, DsrcConfig, HighwaySweep, SweepCtx, SweepReport, molina_masegosa_sweep,
    sweep_dsrc, sweep_dsrc_with_samples, sweep_sidelink,
};
