//! Decentralised congestion control: `dcc/etsi/adaptive-ts102687`,
//! `dcc/etsi/reactive-ts102687`, the EN 302 571 floor that applies under any algorithm,
//! and `dcc/sae/j2945-1-rate-power` (04-models.md §6).
//!
//! # The floor is not optional
//!
//! 04-models.md §6.3 is explicit: "The engine enforces the EN 302 571 floor in
//! `Dcc::gate` regardless of the selected algorithm, so a conformant `T_off,limit` bound
//! holds for every plug-in." [`En302571Floor`] is that enforcement, and every model in
//! this module runs its own decision through it before returning. A study that wants the
//! unconstrained algorithm has to say so by constructing the model with
//! [`En302571Floor::disabled`], which is recorded on the card as a non-conformant
//! configuration rather than being reachable by forgetting something.
//!
//! # The idle-time bound, and how it is grouped
//!
//! EN 302 571 §4.2.10.2 and TS 103 175 §7.2 Eq. 1 give the idle-time bound as
//!
//! ```text
//! T_off = (1/C_w) · T_on · (4000 · (CBR − CTH)/CBR − 1)
//! ```
//!
//! with CBR a plain fraction. The bracket is `T_on` times a dimensionless quantity, and
//! it reproduces **both** of TS 103 175 Table 2's worked examples:
//!
//! | CBR | `T_on` | this expression | Table 2 | error |
//! |---|---|---|---|---|
//! | 0.68 | 1 ms | 351.941 ms | about 351.9 ms | 0.012 % |
//! | 0.75 | 1 ms | 692.333 ms | about 692.3 ms | 0.005 % |
//!
//! Two independent matches to a hundredth of a percent are not a coincidence, so the
//! grouping is settled and nothing here is calibrated against the table.
//!
//! This crate previously read the same line as `4000·(CBR − CTH)/(CBR − 1)` — the minus
//! one inside the denominator rather than outside the quotient. That is negative for
//! every CBR in `(CTH, 1)`, so the bound never restricted; a `CbrUnit` parameter was
//! invented to rescue it by reading CBR as a percentage, which overshot both examples by
//! 1.5-1.8 % and left an "unexplained 2 % residual" recorded as a disagreement with the
//! standard. There was no disagreement: it was a misplaced parenthesis, and the parameter,
//! the narrative and the residual are all gone. `the_floor_reproduces_the_ts103175_table_2_examples`
//! now pins both figures to 0.1 ms, derived from the clause rather than from the code.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::phy::air_time;
use crate::traits::Dcc;
use crate::types::{DccAlgorithm, DccState, GateDecision, Mcs, ReactiveState, TxRequest, timing};

// =========================================================================================
// The EN 302 571 floor
// =========================================================================================

/// The baseline limits that apply under any DCC algorithm
/// [EN 302 571 V2.1.1 §4.2.10.2 Eq. 2-5, and TS 103 175 §7.2 Eq. 1 for the cross-layer
/// form].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct En302571Floor {
    /// Whether the floor is enforced at all. `false` is a deliberately non-conformant
    /// configuration.
    pub enforced: bool,
    /// The largest conformant `T_on`, 4 ms.
    pub t_on_max: Duration,
    /// The duty-cycle cap, 0.03.
    pub duty_cycle_max: f64,
    /// The unconditional minimum idle time, 25 ms.
    pub t_off_min: Duration,
    /// The congestion threshold `CTH`, 0.62. The same value TS 103 175 passes to
    /// DCC_NET, and a different number from the adaptive approach's 0.68 target.
    pub cth: f64,
    /// The DCC weight factor `C_w`, in `(0, 1]`, default 1.
    pub c_w: f64,
}

impl En302571Floor {
    /// The floor as the standards define it.
    pub const CONFORMANT: En302571Floor = En302571Floor {
        enforced: true,
        t_on_max: Duration::from_millis(4),
        duty_cycle_max: 0.03,
        t_off_min: Duration::from_millis(25),
        cth: 0.62,
        c_w: 1.0,
    };

    /// The floor turned off, for a study that wants the bare algorithm. Non-conformant,
    /// and the card says so.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enforced: false,
            ..Self::CONFORMANT
        }
    }

    /// `T_off,limit`, the idle time the floor demands after a frame of air time `t_on` at
    /// a channel load of `cbr`.
    ///
    /// `T_off = (1/C_w)·T_on·(4000·(CBR − CTH)/CBR − 1)`, capped at 1 s, with CBR a plain
    /// fraction [EN 302 571 §4.2.10.2, TS 103 175 §7.2 Eq. 1 — see the module docs for
    /// the grouping and for the two worked examples it reproduces].
    ///
    /// `None` when the bound does not restrict: below the congestion threshold, or where
    /// the bracket is still non-positive just above it (the factor crosses zero at
    /// `CBR = CTH/(1 − 1/4000)`, which is 0.620155 for the default `CTH`).
    #[must_use]
    pub fn t_off_limit(&self, t_on: Duration, cbr: f64) -> Option<Duration> {
        if !self.enforced || cbr < self.cth || self.c_w <= 0.0 || cbr <= 0.0 {
            return None;
        }
        let factor = 4_000.0 * (cbr - self.cth) / cbr - 1.0;
        if factor <= 0.0 {
            return None;
        }
        let ms = t_on.as_secs_f64() * 1_000.0 * factor / self.c_w;
        let capped = ms.min(1_000.0);
        Some(Duration::from_secs_f64(capped / 1_000.0))
    }

    /// The idle time the floor demands in total: the unconditional 25 ms, and the
    /// load-dependent bound when it applies.
    #[must_use]
    pub fn t_off(&self, t_on: Duration, cbr: f64) -> Duration {
        if !self.enforced {
            return Duration::ZERO;
        }
        let limit = self.t_off_limit(t_on, cbr).unwrap_or(Duration::ZERO);
        if limit.as_nanos() > self.t_off_min.as_nanos() {
            limit
        } else {
            self.t_off_min
        }
    }

    /// True when a frame of this air time may be transmitted at all: `0 < T_on <= 4 ms`.
    #[must_use]
    pub fn admits(&self, t_on: Duration) -> bool {
        if !self.enforced {
            return !t_on.is_zero();
        }
        !t_on.is_zero() && t_on.as_nanos() <= self.t_on_max.as_nanos()
    }
}

impl Default for En302571Floor {
    fn default() -> Self {
        Self::CONFORMANT
    }
}

/// The transmit history one node needs for the duty-cycle rule: the air time it has put
/// on the channel inside the last second.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct DutyCycleWindow {
    /// `(instant, air time)` pairs inside the window, in time order.
    sends: Vec<(SimTime, u64)>,
}

impl DutyCycleWindow {
    const WINDOW: Duration = Duration::from_secs(1);

    fn note(&mut self, at: SimTime, air: Duration) {
        self.sends.push((at, air.as_nanos()));
        self.prune(at);
    }

    fn prune(&mut self, now: SimTime) {
        let cutoff = now.saturating_sub(Self::WINDOW.as_nanos());
        self.sends.retain(|(t, _)| *t > cutoff);
    }

    fn used_ns(&self, now: SimTime) -> u64 {
        let cutoff = now.saturating_sub(Self::WINDOW.as_nanos());
        self.sends
            .iter()
            .filter(|(t, _)| *t > cutoff)
            .map(|(_, d)| *d)
            .sum()
    }

    /// The duty cycle over the last second.
    fn ratio(&self, now: SimTime) -> f64 {
        self.used_ns(now) as f64 / Self::WINDOW.as_nanos() as f64
    }
}

// =========================================================================================
// `dcc/etsi/adaptive-ts102687`
// =========================================================================================

/// The adaptive approach's parameter table [TS 102 687 V1.2.1 §5.4 Table 3].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdaptiveParams {
    /// α, the smoothing parameter, 0.016.
    pub alpha: f64,
    /// β, the gain parameter, 0.0012.
    pub beta: f64,
    /// The target channel load, 0.68.
    pub cbr_target: f64,
    /// The upper bound on δ, 0.03 (from EN 302 571).
    pub delta_max: f64,
    /// The lower bound on δ, 0.0006 (anti-starvation).
    pub delta_min: f64,
    /// `G_max+`, the upper clamp on the per-step offset, 0.0005.
    pub g_max_plus: f64,
    /// `G_max−`, the lower clamp, −0.00025.
    pub g_max_minus: f64,
}

impl AdaptiveParams {
    /// The table as the standard prints it.
    pub const TS102687: AdaptiveParams = AdaptiveParams {
        alpha: 0.016,
        beta: 0.0012,
        cbr_target: 0.68,
        delta_max: 0.03,
        delta_min: 0.0006,
        g_max_plus: 0.0005,
        g_max_minus: -0.00025,
    };
}

impl Default for AdaptiveParams {
    fn default() -> Self {
        Self::TS102687
    }
}

/// The adaptive approach's per-node state.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdaptiveNodeState {
    /// The smoothed load, `CBR_ITS-S`.
    pub cbr_its_s: f64,
    /// The most recent measurement, `CBR_L_0_Hop`.
    pub cbr_now: f64,
    /// The previous measurement, `CBR_L_0_Hop_prev`.
    pub cbr_prev: f64,
    /// δ, the fraction of time the station may transmit.
    pub delta: f64,
    /// When δ was last recomputed.
    pub last_update: Option<SimTime>,
    /// The instant the gate opens again, `t_go`.
    pub gate_open_at: SimTime,
    /// The idle time the last gate decision imposed: `max(T_on/δ clamped, floor)`.
    ///
    /// Stored at `gate` time because that is the only instant it is defined at.
    /// [`Dcc::state`] used to report `gate_open_at − last_update`, the gap between a gate
    /// deadline and the last *control-loop* update, which is not an idle time at all: it
    /// depends on where in the 200 ms δ-update cycle the last transmission happened and
    /// saturates to zero whenever a CBR report arrives after the gate deadline. The HUD
    /// and the metrics export it as `T_off`, so it has to be one.
    pub last_t_off: Duration,
}

impl Default for AdaptiveNodeState {
    fn default() -> Self {
        Self {
            cbr_its_s: 0.0,
            cbr_now: 0.0,
            cbr_prev: 0.0,
            // The loop starts at the upper bound: an unloaded channel imposes nothing,
            // and δ falls as the measured load rises.
            delta: AdaptiveParams::TS102687.delta_max,
            last_update: None,
            gate_open_at: 0,
            last_t_off: Duration::ZERO,
        }
    }
}

/// `dcc/etsi/adaptive-ts102687` — the TS 102 687 adaptive approach with its gatekeeper
/// (04-models.md §6.1).
#[derive(Debug)]
pub struct AdaptiveDcc {
    card: ModelCard,
    params: AdaptiveParams,
    floor: En302571Floor,
    /// δ is recomputed every 200 ms (`2 × T_CBR`) [TS 102 687 §5.2, §5.4].
    update_period: Duration,
    nodes: BTreeMap<u32, AdaptiveNodeState>,
    duty: BTreeMap<u32, DutyCycleWindow>,
}

impl AdaptiveDcc {
    /// The model's id.
    pub const ID: &'static str = "dcc/etsi/adaptive-ts102687";

    /// The model with the standard's parameters and the conformant floor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: adaptive_card(),
            params: AdaptiveParams::TS102687,
            floor: En302571Floor::CONFORMANT,
            update_period: timing::T_DCC,
            nodes: BTreeMap::new(),
            duty: BTreeMap::new(),
        }
    }

    /// The model with the floor in a caller-chosen configuration.
    #[must_use]
    pub fn with_floor(mut self, floor: En302571Floor) -> Self {
        self.floor = floor;
        self
    }

    /// The parameters in force.
    #[must_use]
    pub const fn params(&self) -> AdaptiveParams {
        self.params
    }

    /// The floor in force.
    #[must_use]
    pub const fn floor(&self) -> En302571Floor {
        self.floor
    }

    /// The duty cycle this node has used over the last second, `0.0..=1.0`.
    ///
    /// The EN 302 571 cap is 3 %, and this is the number the gate compares against it;
    /// the HUD shows it beside δ.
    #[must_use]
    pub fn duty_cycle(&self, node: NodeId, now: SimTime) -> f64 {
        self.duty.get(&node.index()).map_or(0.0, |w| w.ratio(now))
    }

    /// δ for one node.
    #[must_use]
    pub fn delta(&self, node: NodeId) -> f64 {
        self.nodes
            .get(&node.index())
            .map_or(self.params.delta_max, |s| s.delta)
    }

    /// The smoothed load for one node.
    #[must_use]
    pub fn smoothed_cbr(&self, node: NodeId) -> f64 {
        self.nodes.get(&node.index()).map_or(0.0, |s| s.cbr_its_s)
    }

    /// One update of the control loop, exactly as TS 102 687 §5.4 states it.
    ///
    /// Pure: it takes the state and returns the new δ, so the equations can be checked
    /// against the standard without a node, a clock or a context.
    #[must_use]
    pub fn update_delta(params: AdaptiveParams, delta: f64, cbr_smoothed: f64) -> f64 {
        let raw = params.beta * (params.cbr_target - cbr_smoothed);
        let offset = if raw > 0.0 {
            raw.min(params.g_max_plus)
        } else {
            raw.max(params.g_max_minus)
        };
        let next = (1.0 - params.alpha) * delta + offset;
        next.clamp(params.delta_min, params.delta_max)
    }

    /// The smoothing step: `CBR_ITS-S = 0.5·CBR_ITS-S + 0.5·(CBR_now + CBR_prev)/2`.
    #[must_use]
    pub fn smooth(previous: f64, cbr_now: f64, cbr_prev: f64) -> f64 {
        0.5 * previous + 0.5 * (cbr_now + cbr_prev) / 2.0
    }

    /// The gatekeeper interval of TS 102 687 Annex B:
    /// `t_go = t_pg + min(max(T_on,pp/δ, 25 ms), 1 s)`.
    #[must_use]
    pub fn gate_interval(t_on: Duration, delta: f64) -> Duration {
        if delta <= 0.0 {
            return Duration::from_secs(1);
        }
        let seconds = t_on.as_secs_f64() / delta;
        let clamped = seconds.clamp(0.025, 1.0);
        Duration::from_secs_f64(clamped)
    }
}

impl Default for AdaptiveDcc {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for AdaptiveDcc {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Dcc<C> for AdaptiveDcc {
    fn on_cbr(&mut self, ctx: &mut C, node: NodeId, cbr: f64) {
        let now = ctx.now();
        let period = self.update_period.as_nanos();
        let params = self.params;
        let state = self.nodes.entry(node.index()).or_default();
        state.cbr_prev = state.cbr_now;
        state.cbr_now = cbr.clamp(0.0, 1.0);
        let due = match state.last_update {
            None => true,
            Some(last) => now.saturating_sub(last) >= period,
        };
        if due {
            state.cbr_its_s = AdaptiveDcc::smooth(state.cbr_its_s, state.cbr_now, state.cbr_prev);
            state.delta = AdaptiveDcc::update_delta(params, state.delta, state.cbr_its_s);
            state.last_update = Some(now);
        }
    }

    fn gate(&mut self, ctx: &mut C, node: NodeId, req: &TxRequest) -> GateDecision {
        let now = ctx.now();
        if !self.floor.admits(req.air_time) {
            // `0 < T_on <= 4 ms` is a hard conformance bound, not a delay: a frame whose
            // air time exceeds it cannot be sent at all. A 2,304 B frame at 3 Mbit/s is
            // 6.2 ms and lands here, which is a real property of the standard rather than
            // an artefact of this model.
            return GateDecision::Drop;
        }
        let delta = self.delta(node);
        let cbr = self.smoothed_cbr(node);
        let state = self.nodes.entry(node.index()).or_default();
        if now < state.gate_open_at {
            return GateDecision::DelayUntil(state.gate_open_at);
        }
        let duty = self.duty.entry(node.index()).or_default();
        if self.floor.enforced {
            let projected = (duty.used_ns(now) + req.air_time.as_nanos()) as f64
                / DutyCycleWindow::WINDOW.as_nanos() as f64;
            if projected > self.floor.duty_cycle_max {
                // The duty-cycle cap is a rate limit, so the frame waits rather than dies.
                let wait = self.floor.t_off(req.air_time, cbr);
                let state = self.nodes.entry(node.index()).or_default();
                state.last_t_off = wait;
                return GateDecision::DelayUntil(wait.after(now));
            }
        }
        duty.note(now, req.air_time);
        let algorithmic = AdaptiveDcc::gate_interval(req.air_time, delta);
        let floor = self.floor.t_off(req.air_time, cbr);
        let wait = if floor.as_nanos() > algorithmic.as_nanos() {
            floor
        } else {
            algorithmic
        };
        let state = self.nodes.entry(node.index()).or_default();
        state.gate_open_at = wait.after(now);
        state.last_t_off = wait;
        GateDecision::Now {
            power_dbm: req.power_dbm,
            mcs: req.mcs,
        }
    }

    fn state(&self, node: NodeId) -> DccState {
        let s = self.nodes.get(&node.index()).copied().unwrap_or_default();
        DccState {
            algorithm: DccAlgorithm::AdaptiveTs102687,
            cbr: s.last_update.map(|_| s.cbr_its_s),
            t_off: s.last_t_off,
            delta: Some(s.delta),
            state: None,
            power_dbm: None,
            mcs: None,
            itt: None,
        }
    }
}

fn ts102687() -> Source {
    Source::new(
        SourceKind::Standard,
        "ETSI TS 102 687 V1.2.1 §5.2, §5.4 Table 3 and Annex B (R1 §D.1), via \
         04-models.md §6.1",
    )
}

fn en302571() -> Source {
    Source {
        kind: SourceKind::Standard,
        reference: "ETSI EN 302 571 V2.1.1 §4.2.10.2 Eq. 2-5 and TS 103 175 V1.1.1 §7.2 \
                    Eq. 1, REQ009/REQ022/REQ023 (R1 §D.3-D.4), via 04-models.md §6.3"
            .to_string(),
        accessed: None,
        note: Some(
            "CTH 0.62 is a different tunable from the adaptive approach's 0.68 target; the \
             two are independent."
                .to_string(),
        ),
    }
}

fn floor_parameters() -> Vec<Parameter> {
    vec![
        Parameter {
            name: "t_on_max_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(4),
            range: None,
            source: en302571(),
            calibration: None,
        },
        Parameter {
            name: "duty_cycle_max".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.03),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: en302571(),
            calibration: None,
        },
        Parameter {
            name: "t_off_min_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(25),
            range: None,
            source: en302571(),
            calibration: None,
        },
        Parameter {
            name: "cth".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.62),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: en302571(),
            calibration: None,
        },
        Parameter {
            name: "c_w".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(1.0),
            range: Some(vec![serde_json::json!(0.01), serde_json::json!(1.0)]),
            source: en302571(),
            calibration: None,
        },
        Parameter {
            name: "t_off_limit_coefficient".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(4_000.0),
            range: None,
            source: Source {
                kind: SourceKind::Standard,
                reference: "ETSI EN 302 571 V2.1.1 §4.2.10.2 and TS 103 175 V1.1.1 §7.2 \
                            Eq. 1: T_off = (1/C_w)·T_on·(4000·(CBR − CTH)/CBR − 1), CBR a \
                            plain fraction"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Reproduces both of TS 103 175 Table 2's worked examples with C_w = 1 \
                     and T_on = 1 ms: 351.941 ms at CBR 0.68 against \"about 351.9 ms\" \
                     (0.012 %) and 692.333 ms at CBR 0.75 against \"about 692.3 ms\" \
                     (0.005 %)."
                        .to_string(),
                ),
            },
            calibration: None,
        },
    ]
}

fn adaptive_card() -> ModelCard {
    let mut card = ModelCard::new(
        AdaptiveDcc::ID,
        Family::Dcc,
        "1.0.0",
        "The TS 102 687 adaptive approach: a smoothed CBR drives a clamped integral \
         controller on δ, and the Annex B gatekeeper turns δ into the next gate-open \
         time. The EN 302 571 floor is enforced on top.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "load smoothing",
            "CBR_ITS-S = 0.5·CBR_ITS-S + 0.5·(CBR_L_0_Hop + CBR_L_0_Hop_prev)/2",
        ),
        Equation {
            name: "delta update".to_string(),
            latex_or_text: "δ_offset = min(β·(CBR_target − CBR_ITS-S), G_max+) if positive \
                            else max(β·(CBR_target − CBR_ITS-S), G_max−); \
                            δ = (1 − α)·δ + δ_offset; δ clamped to [δ_min, δ_max]"
                .to_string(),
            notes: Some("Recomputed every 200 ms (2 × T_CBR).".to_string()),
        },
        Equation::new(
            "gatekeeper",
            "t_go = t_pg + min(max(T_on,pp/δ, 25 ms), 1 s)",
        ),
    ];
    let mut params = vec![
        Parameter {
            name: "alpha".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.016),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "beta".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.0012),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "cbr_target".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.68),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "delta_max".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.03),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "delta_min".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.0006),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "g_max_plus".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(0.0005),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "g_max_minus".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(-0.00025),
            range: Some(vec![serde_json::json!(-1.0), serde_json::json!(0.0)]),
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "t_cbr_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(100),
            range: None,
            source: ts102687(),
            calibration: None,
        },
        Parameter {
            name: "update_period_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(200),
            range: None,
            source: ts102687(),
            calibration: None,
        },
    ];
    params.extend(floor_parameters());
    card.parameters = params;
    card.assumptions = vec![
        "CBR is measured by the access layer per 04-models.md §4.5 and handed in once per \
         T_CBR."
            .to_string(),
        "T_on is known per queued packet from Phy::air_time.".to_string(),
        "δ starts at δ_max: an unloaded channel imposes nothing, and the loop reduces δ \
         as the measured load rises."
            .to_string(),
    ];
    card.limitations = vec![
        "No DCC_NET one-hop CBR sharing (TS 102 636-4-2) and no facilities-layer DCC_FAC \
         (04-models.md §6.6)."
            .to_string(),
        "The 200 ms update is measured from the first CBR report rather than from \
         UTC mod 200 ms: the engine has no UTC that a plug-in may read \
         (02-architecture.md §6.1)."
            .to_string(),
        format!(
            "EN 302 571's 0 < T_on <= 4 ms bound drops a frame outright rather than \
             delaying it, so the largest admissible PSDU depends on the MCS: {}. A \
             signed CPM or a certificate-bearing CAM sent at the mandatory 3 Mbit/s rate \
             is dropped by DCC before it reaches the PHY. Pairing a configured message \
             size with an MCS is a scenario-validator check \
             (`the_admissible_frame_size_per_mcs_is_recorded`).",
            admissible_bytes_table()
                .iter()
                .map(|(mcs, bytes)| format!("{} {} B", mcs.label(), bytes))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ];
    card.sources = vec![ts102687(), en302571()];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![ts102687()],
        tests: vec![
            "the_adaptive_parameter_table_matches_the_standard".to_string(),
            "delta_falls_when_the_load_exceeds_the_target".to_string(),
            "delta_is_clamped_to_its_bounds".to_string(),
            "the_gatekeeper_interval_is_clamped_to_25_ms_and_1_s".to_string(),
            "the_floor_reproduces_the_ts103175_table_2_examples".to_string(),
            "a_frame_longer_than_4_ms_is_dropped_by_the_floor".to_string(),
            "the_admissible_frame_size_per_mcs_is_recorded".to_string(),
        ],
    };
    card.determinism = Determinism::default();
    card
}

/// The largest PSDU each MCS can carry inside EN 302 571's `T_on <= 4 ms` bound, bytes.
///
/// Derived, not cited: the largest `bytes` for which
/// [`crate::phy::air_time`]`(bytes, mcs) <= 4 ms`, capped at the 2,304 B MSDU limit. It
/// goes on the card so that a scenario pairing a message size with an MCS can be checked
/// against it instead of discovering the [`GateDecision::Drop`] at run time.
#[must_use]
pub fn admissible_bytes_table() -> Vec<(Mcs, u32)> {
    Mcs::ALL
        .iter()
        .map(|&mcs| {
            let mut bytes = timing::MAX_MSDU_BYTES;
            while bytes > 0 && air_time(bytes, mcs).as_nanos() > Duration::from_millis(4).as_nanos()
            {
                bytes -= 1;
            }
            (mcs, bytes)
        })
        .collect()
}

// =========================================================================================
// `dcc/etsi/reactive-ts102687`
// =========================================================================================

/// Which of the two informative reactive tables to use
/// [TS 102 687 V1.2.1 Annex A Tables A.1 and A.2].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReactiveTable {
    /// Table A.1, for `T_on <= 1 ms`. The default.
    #[default]
    A1TonUpTo1Ms,
    /// Table A.2, for `T_on <= 500 µs`.
    A2TonUpTo500Us,
}

impl ReactiveTable {
    /// The state a channel load falls in.
    #[must_use]
    pub fn state_for(self, cbr: f64) -> ReactiveState {
        let percent = cbr * 100.0;
        let restrictive_from = match self {
            ReactiveTable::A1TonUpTo1Ms => 60.0,
            ReactiveTable::A2TonUpTo500Us => 65.0,
        };
        if percent < 30.0 {
            ReactiveState::Relaxed
        } else if percent < 40.0 {
            ReactiveState::Active1
        } else if percent < 50.0 {
            ReactiveState::Active2
        } else if percent <= restrictive_from {
            ReactiveState::Active3
        } else {
            ReactiveState::Restrictive
        }
    }

    /// The packet rate and `T_off` of one state.
    #[must_use]
    pub fn limits(self, state: ReactiveState) -> (f64, Duration) {
        match (self, state) {
            (ReactiveTable::A1TonUpTo1Ms, ReactiveState::Relaxed) => {
                (10.0, Duration::from_millis(100))
            }
            (ReactiveTable::A1TonUpTo1Ms, ReactiveState::Active1) => {
                (5.0, Duration::from_millis(200))
            }
            (ReactiveTable::A1TonUpTo1Ms, ReactiveState::Active2) => {
                (2.5, Duration::from_millis(400))
            }
            (ReactiveTable::A1TonUpTo1Ms, ReactiveState::Active3) => {
                (2.0, Duration::from_millis(500))
            }
            (ReactiveTable::A1TonUpTo1Ms, ReactiveState::Restrictive) => {
                (1.0, Duration::from_millis(1_000))
            }
            (ReactiveTable::A2TonUpTo500Us, ReactiveState::Relaxed) => {
                (20.0, Duration::from_millis(50))
            }
            (ReactiveTable::A2TonUpTo500Us, ReactiveState::Active1) => {
                (10.0, Duration::from_millis(100))
            }
            (ReactiveTable::A2TonUpTo500Us, ReactiveState::Active2) => {
                (5.0, Duration::from_millis(200))
            }
            (ReactiveTable::A2TonUpTo500Us, ReactiveState::Active3) => {
                (4.0, Duration::from_millis(250))
            }
            (ReactiveTable::A2TonUpTo500Us, ReactiveState::Restrictive) => {
                (1.0, Duration::from_millis(1_000))
            }
        }
    }
}

/// `dcc/etsi/reactive-ts102687` — the informative reactive state machine
/// (04-models.md §6.2).
#[derive(Debug)]
pub struct ReactiveDcc {
    card: ModelCard,
    table: ReactiveTable,
    floor: En302571Floor,
    /// The state-hold timer. Zero by default: the cited tables specify no hysteresis
    /// (04-models.md §6.2 records this as `TODO: calibrate`).
    hold: Duration,
    nodes: BTreeMap<u32, ReactiveNodeState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ReactiveNodeState {
    state: ReactiveState,
    cbr: f64,
    entered_at: SimTime,
    gate_open_at: SimTime,
    has_measurement: bool,
}

impl Default for ReactiveNodeState {
    fn default() -> Self {
        Self {
            state: ReactiveState::Relaxed,
            cbr: 0.0,
            entered_at: 0,
            gate_open_at: 0,
            has_measurement: false,
        }
    }
}

impl ReactiveDcc {
    /// The model's id.
    pub const ID: &'static str = "dcc/etsi/reactive-ts102687";

    /// The model with Table A.1 and the conformant floor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: reactive_card(ReactiveTable::A1TonUpTo1Ms),
            table: ReactiveTable::A1TonUpTo1Ms,
            floor: En302571Floor::CONFORMANT,
            hold: Duration::ZERO,
            nodes: BTreeMap::new(),
        }
    }

    /// The model with a caller-chosen table.
    #[must_use]
    pub fn with_table(mut self, table: ReactiveTable) -> Self {
        self.table = table;
        self.card = reactive_card(table);
        self
    }

    /// The model with a state-hold timer, for a study that has calibrated hysteresis.
    #[must_use]
    pub fn with_hold(mut self, hold: Duration) -> Self {
        self.hold = hold;
        self
    }

    /// The state one node is in.
    #[must_use]
    pub fn state_of(&self, node: NodeId) -> ReactiveState {
        self.nodes
            .get(&node.index())
            .map_or(ReactiveState::Relaxed, |s| s.state)
    }

    /// The table in force.
    #[must_use]
    pub const fn table(&self) -> ReactiveTable {
        self.table
    }
}

impl Default for ReactiveDcc {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for ReactiveDcc {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Dcc<C> for ReactiveDcc {
    fn on_cbr(&mut self, ctx: &mut C, node: NodeId, cbr: f64) {
        let now = ctx.now();
        let table = self.table;
        let hold = self.hold.as_nanos();
        let state = self.nodes.entry(node.index()).or_default();
        state.cbr = cbr.clamp(0.0, 1.0);
        state.has_measurement = true;
        let target = table.state_for(state.cbr);
        if target != state.state && now.saturating_sub(state.entered_at) >= hold {
            state.state = target;
            state.entered_at = now;
        }
    }

    fn gate(&mut self, ctx: &mut C, node: NodeId, req: &TxRequest) -> GateDecision {
        let now = ctx.now();
        if !self.floor.admits(req.air_time) {
            return GateDecision::Drop;
        }
        let table = self.table;
        let floor = self.floor;
        let state = self.nodes.entry(node.index()).or_default();
        if now < state.gate_open_at {
            return GateDecision::DelayUntil(state.gate_open_at);
        }
        let (_rate_hz, t_off) = table.limits(state.state);
        let floor_t_off = floor.t_off(req.air_time, state.cbr);
        let wait = if floor_t_off.as_nanos() > t_off.as_nanos() {
            floor_t_off
        } else {
            t_off
        };
        state.gate_open_at = wait.after(now);
        GateDecision::Now {
            power_dbm: req.power_dbm,
            mcs: req.mcs,
        }
    }

    fn state(&self, node: NodeId) -> DccState {
        let s = self.nodes.get(&node.index()).copied().unwrap_or_default();
        let (_, t_off) = self.table.limits(s.state);
        DccState {
            algorithm: DccAlgorithm::ReactiveTs102687,
            cbr: s.has_measurement.then_some(s.cbr),
            t_off,
            delta: None,
            state: Some(s.state),
            power_dbm: None,
            mcs: None,
            itt: None,
        }
    }
}

fn reactive_card(table: ReactiveTable) -> ModelCard {
    let source = Source {
        kind: SourceKind::Standard,
        reference: "ETSI TS 102 687 V1.2.1 Annex A Tables A.1 and A.2 (informative) \
                    (R1 §D.2), via 04-models.md §6.2"
            .to_string(),
        accessed: None,
        note: Some(
            "The named states belong to TS 102 687 Annex A, not to TS 103 175, which \
             defines a continuous T_off bound and no states."
                .to_string(),
        ),
    };
    let mut card = ModelCard::new(
        ReactiveDcc::ID,
        Family::Dcc,
        "1.0.0",
        "The TS 102 687 reactive approach: five states by channel load, each with a packet \
         rate and an idle time, with the EN 302 571 floor on top.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![Equation {
        name: "state table".to_string(),
        latex_or_text: match table {
            ReactiveTable::A1TonUpTo1Ms => {
                "Relaxed <30 %: 10 Hz, 100 ms; Active 1 30-39 %: 5 Hz, 200 ms; \
                 Active 2 40-49 %: 2.5 Hz, 400 ms; Active 3 50-60 %: 2 Hz, 500 ms; \
                 Restrictive >60 %: 1 Hz, 1,000 ms"
            }
            ReactiveTable::A2TonUpTo500Us => {
                "Relaxed <30 %: 20 Hz, 50 ms; Active 1 30-39 %: 10 Hz, 100 ms; \
                 Active 2 40-49 %: 5 Hz, 200 ms; Active 3 50-65 %: 4 Hz, 250 ms; \
                 Restrictive >65 %: 1 Hz, 1,000 ms"
            }
        }
        .to_string(),
        notes: Some("Table A.1 applies for T_on <= 1 ms, A.2 for T_on <= 500 µs.".to_string()),
    }];
    let mut params = vec![
        Parameter {
            name: "table".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(match table {
                ReactiveTable::A1TonUpTo1Ms => "a1-ton-up-to-1ms",
                ReactiveTable::A2TonUpTo500Us => "a2-ton-up-to-500us",
            }),
            range: Some(vec![
                serde_json::json!("a1-ton-up-to-1ms"),
                serde_json::json!("a2-ton-up-to-500us"),
            ]),
            source: source.clone(),
            calibration: None,
        },
        Parameter {
            name: "hold_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(0),
            range: Some(vec![serde_json::json!(0), serde_json::json!(5_000)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "hysteresis and state-hold timers are not specified in the \
                            cited tables; none is applied by default"
                    .to_string(),
                accessed: None,
                note: None,
            },
            calibration: Some(
                "Read the C2C-CC profile's DCC clauses in RS 2037 for the mandated \
                 hysteresis (the plan 04-models.md §6.2 records)."
                    .to_string(),
            ),
        },
    ];
    params.extend(floor_parameters());
    card.parameters = params;
    card.assumptions = vec![
        "The state changes on every CBR report, with no hysteresis unless hold_ms is set."
            .to_string(),
    ];
    card.limitations = vec![
        "Both tables are informative in the standard.".to_string(),
        "The state bands are read as percentages of the measured CBR, with the Active 3 \
         upper bound inclusive."
            .to_string(),
    ];
    card.sources = vec![source.clone(), en302571()];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![source],
        tests: vec![
            "the_reactive_tables_match_the_standard".to_string(),
            "the_reactive_state_bands_are_exact_at_their_edges".to_string(),
        ],
    };
    card.determinism = Determinism::default();
    card
}

// =========================================================================================
// `dcc/sae/j2945-1-rate-power`
// =========================================================================================

/// The J2945/1 rate and power parameters as Rostami, Krishnan and Gruteser 2018 report
/// them (04-models.md §6.4). The SAE standard itself is paywalled and not in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct J2945Params {
    /// λ, the density smoothing weight, 0.5.
    pub lambda: f64,
    /// B, the density coefficient, 25 vehicles.
    pub b_density: f64,
    /// `vMaxITT`, 600 ms.
    pub max_itt: Duration,
    /// The minimum inter-transmission time, 100 ms.
    pub min_itt: Duration,
    /// `vTxRateCntrlInt`, 100 ms.
    pub rate_control_interval: Duration,
    /// The radius the neighbour count is taken over, 100 m.
    pub counting_radius_m: f64,
    /// `vRPMax`, 20 dBm.
    pub rp_max_dbm: f64,
    /// `vRPMin`, 10 dBm.
    pub rp_min_dbm: f64,
    /// `vMinCU`, 0.50.
    pub min_cu: f64,
    /// `vMaxCU`, 0.80.
    pub max_cu: f64,
    /// `vSUPRAGain`, 0.5.
    pub supra_gain: f64,
    /// The initial `vRP`, 15 dBm.
    pub rp_initial_dbm: f64,
    /// `vTEMin`, 0.2 m.
    pub te_min_m: f64,
    /// `vTEMax`, 0.5 m.
    pub te_max_m: f64,
    /// `CertAttachInt`, 450 ms.
    pub cert_attach_interval: Duration,
}

impl J2945Params {
    /// The table as Rostami 2018 prints it.
    pub const ROSTAMI: J2945Params = J2945Params {
        lambda: 0.5,
        b_density: 25.0,
        max_itt: Duration::from_millis(600),
        min_itt: Duration::from_millis(100),
        rate_control_interval: Duration::from_millis(100),
        counting_radius_m: 100.0,
        rp_max_dbm: 20.0,
        rp_min_dbm: 10.0,
        min_cu: 0.50,
        max_cu: 0.80,
        supra_gain: 0.5,
        rp_initial_dbm: 15.0,
        te_min_m: 0.2,
        te_max_m: 0.5,
        cert_attach_interval: Duration::from_millis(450),
    };
}

impl Default for J2945Params {
    fn default() -> Self {
        Self::ROSTAMI
    }
}

/// `dcc/sae/j2945-1-rate-power` — the SAE J2945/1 rate and power control
/// (04-models.md §6.4).
#[derive(Debug)]
pub struct SaeJ2945Dcc {
    card: ModelCard,
    params: J2945Params,
    floor: En302571Floor,
    nodes: BTreeMap<u32, J2945NodeState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct J2945NodeState {
    smoothed_density: f64,
    rp_dbm: f64,
    cbp: f64,
    itt: Duration,
    gate_open_at: SimTime,
    has_measurement: bool,
}

impl Default for J2945NodeState {
    fn default() -> Self {
        Self {
            smoothed_density: 0.0,
            rp_dbm: J2945Params::ROSTAMI.rp_initial_dbm,
            cbp: 0.0,
            itt: J2945Params::ROSTAMI.min_itt,
            gate_open_at: 0,
            has_measurement: false,
        }
    }
}

impl SaeJ2945Dcc {
    /// The model's id.
    pub const ID: &'static str = "dcc/sae/j2945-1-rate-power";

    /// The model with the Rostami parameter values and the conformant floor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: j2945_card(),
            params: J2945Params::ROSTAMI,
            floor: En302571Floor::CONFORMANT,
            nodes: BTreeMap::new(),
        }
    }

    /// The parameters in force.
    #[must_use]
    pub const fn params(&self) -> J2945Params {
        self.params
    }

    /// The smoothed neighbour count of one node.
    #[must_use]
    pub fn smoothed_density(&self, node: NodeId) -> f64 {
        self.nodes
            .get(&node.index())
            .map_or(0.0, |s| s.smoothed_density)
    }

    /// The transmit power one node is allowed, dBm.
    #[must_use]
    pub fn power_dbm(&self, node: NodeId) -> f64 {
        self.nodes
            .get(&node.index())
            .map_or(self.params.rp_initial_dbm, |s| s.rp_dbm)
    }

    /// The inter-transmission time one node is on.
    #[must_use]
    pub fn itt(&self, node: NodeId) -> Duration {
        self.nodes
            .get(&node.index())
            .map_or(self.params.min_itt, |s| s.itt)
    }

    /// `MaxITT` from the smoothed density [Rostami 2018 Eq. 1-2]:
    /// 100 ms up to `B` vehicles, linear to `vMaxITT` at `(vMaxITT/100)·B`, capped there.
    #[must_use]
    pub fn max_itt_for(params: J2945Params, smoothed_density: f64) -> Duration {
        let min_ms = params.min_itt.as_secs_f64() * 1_000.0;
        let max_ms = params.max_itt.as_secs_f64() * 1_000.0;
        if smoothed_density <= params.b_density {
            return params.min_itt;
        }
        // The paper's linear segment reaches vMaxITT at (vMaxITT/100)·B vehicles.
        let top_density = (max_ms / min_ms) * params.b_density;
        if smoothed_density >= top_density {
            return params.max_itt;
        }
        let fraction = (smoothed_density - params.b_density) / (top_density - params.b_density);
        Duration::from_secs_f64((min_ms + fraction * (max_ms - min_ms)) / 1_000.0)
    }

    /// `f(CBP)` [Rostami 2018 Eq. 3-5]: `vRPMax` up to `vMinCU`, linear down to `vRPMin`
    /// at `vMaxCU`, `vRPMin` above.
    #[must_use]
    pub fn power_target_dbm(params: J2945Params, cbp: f64) -> f64 {
        if cbp <= params.min_cu {
            params.rp_max_dbm
        } else if cbp >= params.max_cu {
            params.rp_min_dbm
        } else {
            let fraction = (cbp - params.min_cu) / (params.max_cu - params.min_cu);
            params.rp_max_dbm - fraction * (params.rp_max_dbm - params.rp_min_dbm)
        }
    }

    /// The SUPRA step: `RP(t) = RP(t−1) + vSUPRAGain·(f(CBP) − RP(t−1))`.
    #[must_use]
    pub fn supra_step(params: J2945Params, rp_previous_dbm: f64, cbp: f64) -> f64 {
        let target = Self::power_target_dbm(params, cbp);
        rp_previous_dbm + params.supra_gain * (target - rp_previous_dbm)
    }

    /// The smoothed neighbour count: `N_smooth = λ·N + (1 − λ)·N_smooth_prev`.
    #[must_use]
    pub fn smooth_density(params: J2945Params, previous: f64, count: f64) -> f64 {
        params.lambda * count + (1.0 - params.lambda) * previous
    }

    /// The probability that a tracking error of `te_m` triggers an extra transmission
    /// [Rostami 2018 Eq. 6]: zero below `vTEMin`, one above `vTEMax`, and
    /// `1 − exp(−α·(TE − vTEMin))` between.
    ///
    /// α is not given numerically in the cited source, so it is a parameter with a
    /// `todo-calibrate` source; the caller supplies it.
    #[must_use]
    pub fn tracking_error_probability(params: J2945Params, te_m: f64, alpha: f64) -> f64 {
        if te_m <= params.te_min_m {
            0.0
        } else if te_m >= params.te_max_m {
            1.0
        } else {
            (1.0 - math::exp(-alpha * (te_m - params.te_min_m))).clamp(0.0, 1.0)
        }
    }

    /// Feeds the neighbour count in, once per `vTxRateCntrlInt`.
    pub fn on_density<C: Ctx + ?Sized>(&mut self, _ctx: &mut C, node: NodeId, neighbours: u32) {
        let params = self.params;
        let state = self.nodes.entry(node.index()).or_default();
        state.smoothed_density =
            Self::smooth_density(params, state.smoothed_density, f64::from(neighbours));
        state.itt = Self::max_itt_for(params, state.smoothed_density);
    }
}

impl Default for SaeJ2945Dcc {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for SaeJ2945Dcc {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Dcc<C> for SaeJ2945Dcc {
    fn on_cbr(&mut self, _ctx: &mut C, node: NodeId, cbr: f64) {
        let params = self.params;
        let state = self.nodes.entry(node.index()).or_default();
        state.cbp = cbr.clamp(0.0, 1.0);
        state.has_measurement = true;
        state.rp_dbm = SaeJ2945Dcc::supra_step(params, state.rp_dbm, state.cbp);
    }

    fn gate(&mut self, ctx: &mut C, node: NodeId, req: &TxRequest) -> GateDecision {
        let now = ctx.now();
        if !self.floor.admits(req.air_time) {
            return GateDecision::Drop;
        }
        let floor = self.floor;
        let state = self.nodes.entry(node.index()).or_default();
        if now < state.gate_open_at {
            return GateDecision::DelayUntil(state.gate_open_at);
        }
        let itt = state.itt;
        let floor_t_off = floor.t_off(req.air_time, state.cbp);
        let wait = if floor_t_off.as_nanos() > itt.as_nanos() {
            floor_t_off
        } else {
            itt
        };
        state.gate_open_at = wait.after(now);
        // TxPower = RP − MinSectorAntGain + CLoss, both zero in the cited paper.
        GateDecision::Now {
            power_dbm: state.rp_dbm.min(req.power_dbm),
            mcs: req.mcs,
        }
    }

    fn state(&self, node: NodeId) -> DccState {
        let s = self.nodes.get(&node.index()).copied().unwrap_or_default();
        DccState {
            algorithm: DccAlgorithm::SaeJ2945_1,
            cbr: s.has_measurement.then_some(s.cbp),
            t_off: s.itt,
            delta: None,
            state: None,
            power_dbm: Some(s.rp_dbm),
            mcs: Some(Mcs::R6Qpsk12),
            itt: Some(s.itt),
        }
    }
}

fn rostami() -> Source {
    Source {
        kind: SourceKind::Paper,
        reference: "A. Rostami, H. Krishnan, M. Gruteser, \"V2V Safety Communication \
                    Scalability Based on the SAE J2945/1 Standard\" 2018, Eq. 1-6 and \
                    Table 1 (R1 §E), via 04-models.md §6.4"
            .to_string(),
        accessed: None,
        note: Some(
            "SAE J2945/1 itself is paywalled and not in the cache; the paper implements \
             the standard's algorithm in a calibrated ns-3 and is cross-checked against \
             its companion scalability paper. The literal SAE parameter names \
             (vDensityWeightFactor, vRescheduleThreshold, vTxRand, vPERRange, \
             vPERInterval, vPERMax, vPERSubInterval) were not found in any accessible \
             source and stay UNVERIFIED."
                .to_string(),
        ),
    }
}

fn j2945_card() -> ModelCard {
    let mut card = ModelCard::new(
        SaeJ2945Dcc::ID,
        Family::Dcc,
        "1.0.0",
        "SAE J2945/1 rate and power control: the inter-transmission time from a smoothed \
         neighbour count, and the transmit power from the channel busy percentage through \
         the SUPRA filter.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "density smoothing",
            "N_smooth(t) = λ·N(t) + (1 − λ)·N_smooth(t − 1), N = vehicles within 100 m",
        ),
        Equation::new(
            "rate control",
            "MaxITT = 100 ms for N_smooth <= B, linear to vMaxITT at (vMaxITT/100)·B, \
             capped",
        ),
        Equation::new(
            "power control",
            "f(CBP) = vRPMax for CBP <= vMinCU, linear to vRPMin at vMaxCU, vRPMin above; \
             RP(t) = RP(t−1) + vSUPRAGain·(f(CBP) − RP(t−1)); \
             TxPower = RP − MinSectorAntGain + CLoss",
        ),
        Equation {
            name: "tracking-error trigger".to_string(),
            latex_or_text: "p(t) = 1 − exp(−α·(TE − vTEMin)) between vTEMin and vTEMax, \
                            1 above, 0 below"
                .to_string(),
            notes: Some("α is not given numerically in the cited source.".to_string()),
        },
    ];
    let p = J2945Params::ROSTAMI;
    let mut params = vec![
        Parameter {
            name: "lambda".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(p.lambda),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "b_density".to_string(),
            unit: "vehicles".to_string(),
            default: serde_json::json!(p.b_density),
            range: Some(vec![serde_json::json!(1.0), serde_json::json!(500.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "max_itt_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(600),
            range: None,
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "min_itt_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(100),
            range: None,
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "rate_control_interval_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(100),
            range: None,
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "counting_radius_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(100.0),
            range: None,
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "rp_max_dbm".to_string(),
            unit: "dBm".to_string(),
            default: serde_json::json!(p.rp_max_dbm),
            range: Some(vec![serde_json::json!(-10.0), serde_json::json!(33.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "rp_min_dbm".to_string(),
            unit: "dBm".to_string(),
            default: serde_json::json!(p.rp_min_dbm),
            range: Some(vec![serde_json::json!(-10.0), serde_json::json!(33.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "min_cu".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(p.min_cu),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "max_cu".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(p.max_cu),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "supra_gain".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(p.supra_gain),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "rp_initial_dbm".to_string(),
            unit: "dBm".to_string(),
            default: serde_json::json!(p.rp_initial_dbm),
            range: Some(vec![serde_json::json!(-10.0), serde_json::json!(33.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "te_min_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(p.te_min_m),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(5.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "te_max_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(p.te_max_m),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(5.0)]),
            source: rostami(),
            calibration: None,
        },
        Parameter {
            name: "te_alpha".to_string(),
            unit: "1/m".to_string(),
            default: serde_json::json!(1.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(100.0)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "the sensitivity α of the tracking-error trigger is not given \
                            numerically in Rostami 2018"
                    .to_string(),
                accessed: None,
                note: Some(
                    "1/m is a placeholder that makes p(TE) rise smoothly across the \
                     0.2-0.5 m band; nothing cited fixes it."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Read SAE J2945/1 §6 primary text (the plan 04-models.md §6.4 records)."
                    .to_string(),
            ),
        },
        Parameter {
            name: "cert_attach_interval_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(450),
            range: None,
            source: rostami(),
            calibration: None,
        },
    ];
    params.extend(floor_parameters());
    card.parameters = params;
    card.assumptions = vec![
        "MinSectorAntGain and CLoss are zero, as in the cited paper.".to_string(),
        "The neighbour count is supplied by the caller (the engine's actor index), \
         counted within 100 m."
            .to_string(),
    ];
    card.limitations = vec![
        "Every value is from a secondary source: SAE J2945/1 is paywalled, and the \
         literal SAE parameter names are UNVERIFIED."
            .to_string(),
        "The tracking-error trigger's sensitivity α is not cited; the shipped value is a \
         placeholder with a calibration plan."
            .to_string(),
        "The position-accuracy requirement behind the algorithm (1.5 m at 68 %, via \
         NHTSA) is UNVERIFIED at the SAE primary level."
            .to_string(),
    ];
    card.sources = vec![rostami(), en302571()];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![rostami()],
        tests: vec![
            "the_j2945_parameter_table_matches_the_paper".to_string(),
            "the_itt_ramp_matches_the_paper".to_string(),
            "the_power_ramp_matches_the_paper".to_string(),
        ],
    };
    card.determinism = Determinism::default();
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phy::air_time;
    use crate::testctx::TestCtx;
    use crate::types::{AccessCategory, ChannelId};

    fn request(bytes: u32, at: SimTime) -> TxRequest {
        TxRequest {
            bytes,
            mcs: Mcs::R6Qpsk12,
            power_dbm: 23.0,
            ac: AccessCategory::Vo,
            channel: ChannelId::CCH,
            air_time: air_time(bytes, Mcs::R6Qpsk12),
            at,
        }
    }

    #[test]
    fn the_adaptive_parameter_table_matches_the_standard() {
        let p = AdaptiveParams::TS102687;
        assert_eq!(p.alpha, 0.016);
        assert_eq!(p.beta, 0.0012);
        assert_eq!(p.cbr_target, 0.68);
        assert_eq!(p.delta_max, 0.03);
        assert_eq!(p.delta_min, 0.0006);
        assert_eq!(p.g_max_plus, 0.0005);
        assert_eq!(p.g_max_minus, -0.00025);
        assert_eq!(timing::T_CBR, Duration::from_millis(100));
        assert_eq!(timing::T_DCC, Duration::from_millis(200));
    }

    #[test]
    fn delta_falls_when_the_load_exceeds_the_target() {
        let p = AdaptiveParams::TS102687;
        // Above the target the offset is negative and clamped at G_max−.
        let below_target = AdaptiveDcc::update_delta(p, 0.02, 0.9);
        assert!(below_target < 0.02, "{below_target}");
        // Below the target it recovers, clamped at G_max+.
        let recovering = AdaptiveDcc::update_delta(p, 0.001, 0.1);
        assert!(recovering > 0.001 * (1.0 - p.alpha), "{recovering}");
        // The offset clamps: at a load far from the target the step is still at most
        // G_max+ or at least G_max−.
        let step_up = AdaptiveDcc::update_delta(p, 0.01, 0.0) - 0.01 * (1.0 - p.alpha);
        assert!((step_up - p.g_max_plus).abs() < 1e-12, "{step_up}");
        let step_down = AdaptiveDcc::update_delta(p, 0.01, 1.0) - 0.01 * (1.0 - p.alpha);
        assert!((step_down - p.g_max_minus).abs() < 1e-12, "{step_down}");
    }

    #[test]
    fn delta_is_clamped_to_its_bounds() {
        let p = AdaptiveParams::TS102687;
        // Driven hard from either side, δ never leaves [δ_min, δ_max].
        let mut delta = p.delta_max;
        for _ in 0..10_000 {
            delta = AdaptiveDcc::update_delta(p, delta, 1.0);
            assert!((p.delta_min..=p.delta_max).contains(&delta), "{delta}");
        }
        assert!((delta - p.delta_min).abs() < 1e-9, "{delta}");
        for _ in 0..10_000 {
            delta = AdaptiveDcc::update_delta(p, delta, 0.0);
            assert!((p.delta_min..=p.delta_max).contains(&delta), "{delta}");
        }
    }

    #[test]
    fn the_control_loop_tracks_the_target() {
        // The validation target of 04-models.md §6.1: under a sustained load the loop
        // drives δ down, and under a light load it lets it back up. The loop's own
        // fixed point for a constant measured load: δ* = δ_offset/α.
        let p = AdaptiveParams::TS102687;
        let mut delta = p.delta_max;
        for _ in 0..5_000 {
            delta = AdaptiveDcc::update_delta(p, delta, 0.80);
        }
        let at_high_load = delta;
        for _ in 0..5_000 {
            delta = AdaptiveDcc::update_delta(p, delta, 0.30);
        }
        assert!(delta > at_high_load, "{delta} vs {at_high_load}");
        // At the target itself the offset is zero, so δ decays towards δ_min.
        let mut at_target = 0.02;
        for _ in 0..2_000 {
            at_target = AdaptiveDcc::update_delta(p, at_target, p.cbr_target);
        }
        assert!((at_target - p.delta_min).abs() < 1e-9, "{at_target}");
    }

    #[test]
    fn the_smoothing_step_is_the_standards_own() {
        // CBR_ITS-S = 0.5·CBR_ITS-S + 0.5·(now + prev)/2
        assert!((AdaptiveDcc::smooth(0.0, 0.4, 0.4) - 0.2).abs() < 1e-12);
        assert!((AdaptiveDcc::smooth(0.2, 0.4, 0.4) - 0.3).abs() < 1e-12);
        // Fed a constant load it converges to that load.
        let mut s = 0.0;
        for _ in 0..200 {
            s = AdaptiveDcc::smooth(s, 0.6, 0.6);
        }
        assert!((s - 0.6).abs() < 1e-9, "{s}");
    }

    #[test]
    fn the_gatekeeper_interval_is_clamped_to_25_ms_and_1_s() {
        // t_go = t_pg + min(max(T_on/δ, 25 ms), 1 s).
        let t_on = air_time(300, Mcs::R6Qpsk12);
        assert_eq!(t_on, Duration::from_micros(448));
        // At δ_max = 0.03: 448 µs / 0.03 = 14.9 ms, below the 25 ms floor.
        let at_max = AdaptiveDcc::gate_interval(t_on, 0.03);
        assert_eq!(at_max, Duration::from_millis(25));
        // At δ_min = 0.0006 a 1,000 B frame is 1,384 µs, and 1,384 µs / 0.0006 = 2.3 s,
        // above the 1 s cap.
        let long = air_time(1_000, Mcs::R6Qpsk12);
        assert_eq!(long, Duration::from_micros(1_384));
        let at_min = AdaptiveDcc::gate_interval(long, 0.0006);
        assert_eq!(at_min, Duration::from_secs(1));
        // In between it is the ratio.
        let mid = AdaptiveDcc::gate_interval(t_on, 0.005);
        let expected = t_on.as_secs_f64() / 0.005;
        assert!((mid.as_secs_f64() - expected).abs() < 1e-9, "{mid:?}");
    }

    #[test]
    fn the_adaptive_gate_delays_until_its_own_deadline() {
        let mut ctx = TestCtx::new(1);
        let mut dcc = AdaptiveDcc::new();
        let node = NodeId::new(0);
        let req = request(300, 0);
        // The first frame goes immediately.
        assert!(matches!(
            Dcc::gate(&mut dcc, &mut ctx, node, &req),
            GateDecision::Now { .. }
        ));
        // The next one is held until the gate opens.
        let decision = Dcc::gate(&mut dcc, &mut ctx, node, &req);
        let GateDecision::DelayUntil(t) = decision else {
            panic!("expected a delay, got {decision:?}");
        };
        assert!(t >= Duration::from_millis(25).as_nanos(), "{t}");
        ctx.set_now(t);
        assert!(matches!(
            Dcc::gate(&mut dcc, &mut ctx, node, &req),
            GateDecision::Now { .. }
        ));
    }

    #[test]
    fn a_loaded_channel_slows_the_gate_down() {
        let mut ctx = TestCtx::new(2);
        let mut dcc = AdaptiveDcc::new();
        let node = NodeId::new(0);
        // Feed a heavy load for many update periods: δ decays by (1 − α) per update and
        // the offset is clamped at G_max−, so it takes a few hundred updates to reach the
        // floor.
        for i in 0..800u64 {
            ctx.set_now(i * 100_000_000);
            Dcc::on_cbr(&mut dcc, &mut ctx, node, 0.9);
        }
        let loaded_delta = dcc.delta(node);
        assert!(
            loaded_delta < AdaptiveParams::TS102687.delta_max,
            "{loaded_delta}"
        );
        assert!(
            (loaded_delta - AdaptiveParams::TS102687.delta_min).abs() < 1e-6,
            "a sustained overload drives δ to its floor: {loaded_delta}"
        );
        assert!(dcc.smoothed_cbr(node) > 0.8, "{}", dcc.smoothed_cbr(node));
        let t_on = air_time(300, Mcs::R6Qpsk12);
        let interval = AdaptiveDcc::gate_interval(t_on, loaded_delta);
        assert!(
            interval > AdaptiveDcc::gate_interval(t_on, AdaptiveParams::TS102687.delta_max),
            "{interval:?}"
        );
        assert!(interval > Duration::from_millis(25), "{interval:?}");
        // And the exported state says so.
        let state = Dcc::<TestCtx>::state(&dcc, node);
        assert_eq!(state.algorithm, DccAlgorithm::AdaptiveTs102687);
        assert_eq!(state.delta, Some(loaded_delta));
        assert!(state.cbr.is_some());
        // Quantisation does not change what it means.
        assert!(state.quantized().cbr.is_some());
    }

    /// TS 103 175 §7.2 Eq. 1 with `C_w` = 1 and `T_on` = 1 ms, to a tenth of a
    /// millisecond.
    ///
    /// The expected values are derived from the clause, not from the code:
    /// `T_off = T_on·(4000·(CBR − CTH)/CBR − 1)` with `CTH` = 0.62 gives
    /// `4000·0.06/0.68 − 1 = 352.941176… − 1 = 351.941176… ms` at CBR 0.68 and
    /// `4000·0.13/0.75 − 1 = 693.333333… − 1 = 692.333333… ms` at CBR 0.75, against the
    /// table's "about 351.9 ms" and "about 692.3 ms".
    ///
    /// The grouping this replaced — the minus one inside the denominator, rescued by
    /// reading CBR as a percentage — gave 358.209 ms (+1.79 %) and 702.703 ms (+1.50 %),
    /// which is why the old assertions had to be 8 ms and 12 ms wide.
    #[test]
    fn the_floor_reproduces_the_ts103175_table_2_examples() {
        let floor = En302571Floor::CONFORMANT;
        let t_on = Duration::from_millis(1);
        let at_68 = floor.t_off_limit(t_on, 0.68).expect("restricts");
        let at_75 = floor.t_off_limit(t_on, 0.75).expect("restricts");
        assert!(
            (at_68.as_secs_f64() * 1_000.0 - 351.9).abs() < 0.1,
            "{at_68:?}"
        );
        assert!(
            (at_75.as_secs_f64() * 1_000.0 - 692.3).abs() < 0.1,
            "{at_75:?}"
        );
        // And exactly the hand-computed figures, so a change of grouping cannot hide
        // inside the tenth of a millisecond above.
        assert!((at_68.as_secs_f64() * 1_000.0 - (4_000.0 * 0.06 / 0.68 - 1.0)).abs() < 1e-6);
        assert!((at_75.as_secs_f64() * 1_000.0 - (4_000.0 * 0.13 / 0.75 - 1.0)).abs() < 1e-6);

        // The weight factor divides the whole bound, so halving C_w doubles it.
        let weighted = En302571Floor {
            c_w: 0.5,
            ..En302571Floor::CONFORMANT
        };
        let half = weighted.t_off_limit(t_on, 0.68).expect("restricts");
        assert!((half.as_secs_f64() - 2.0 * at_68.as_secs_f64()).abs() < 1e-9);
        // And T_on multiplies it: the bracket is dimensionless.
        let double_frame = floor
            .t_off_limit(Duration::from_micros(2_000), 0.68)
            .expect("restricts");
        assert!((double_frame.as_secs_f64() - 2.0 * at_68.as_secs_f64()).abs() < 1e-9);

        // Below the congestion threshold there is no load-dependent restriction, only the
        // unconditional 25 ms.
        assert!(floor.t_off_limit(t_on, 0.5).is_none());
        assert_eq!(floor.t_off(t_on, 0.5), Duration::from_millis(25));
        // The bracket crosses zero just above CTH, at CTH/(1 − 1/4000) = 0.6201550…, and
        // the bound does not restrict below that.
        let zero_at = 0.62 / (1.0 - 1.0 / 4_000.0);
        assert!(floor.t_off_limit(t_on, zero_at - 1e-9).is_none());
        assert!(floor.t_off_limit(t_on, zero_at + 1e-9).is_some());
        // The cap at 1 s binds at a very high load.
        assert_eq!(floor.t_off_limit(t_on, 0.99), Some(Duration::from_secs(1)));
    }

    /// The largest PSDU each MCS can carry inside the 4 ms `T_on` bound, recorded on the
    /// card so a scenario can be checked against it instead of discovering the drop at
    /// run time.
    ///
    /// Hand-derived: at 3 Mbit/s a 1,482 B frame is 3,992 µs and a 1,483 B frame is
    /// 4,000 µs — the bound is `T_on <= 4 ms`, so 1,483 B is admissible too; the largest
    /// that fits is what the table reports, and the frame one byte larger must not.
    #[test]
    fn the_admissible_frame_size_per_mcs_is_recorded() {
        let table = admissible_bytes_table();
        assert_eq!(table.len(), Mcs::ALL.len());
        let floor = En302571Floor::CONFORMANT;
        for (mcs, bytes) in table {
            assert!(floor.admits(air_time(bytes, mcs)), "{} {bytes} B", mcs.label());
            if bytes < timing::MAX_MSDU_BYTES {
                assert!(
                    !floor.admits(air_time(bytes + 1, mcs)),
                    "{} admits {} B",
                    mcs.label(),
                    bytes + 1
                );
            }
        }
        // The two rates that cannot carry a full MSDU, and the rest that can.
        let by_mcs: std::collections::BTreeMap<&str, u32> = admissible_bytes_table()
            .into_iter()
            .map(|(m, b)| (m.label(), b))
            .collect();
        assert!(by_mcs["3-bpsk-1/2"] < timing::MAX_MSDU_BYTES, "{by_mcs:?}");
        assert!(by_mcs["4.5-bpsk-3/4"] < timing::MAX_MSDU_BYTES, "{by_mcs:?}");
        for rate in [
            "6-qpsk-1/2",
            "9-qpsk-3/4",
            "12-16qam-1/2",
            "18-16qam-3/4",
            "24-64qam-2/3",
            "27-64qam-3/4",
        ] {
            assert_eq!(by_mcs[rate], timing::MAX_MSDU_BYTES, "{rate}");
        }
        // And the card carries the table, so the generated model page shows it.
        let card = adaptive_card();
        assert!(
            card.limitations.iter().any(|l| l.contains("largest admissible PSDU")),
            "{:?}",
            card.limitations
        );
    }

    /// The `T_off` the adaptive model exports is the idle time its last gate decision
    /// imposed, not the gap between a gate deadline and the last control-loop update.
    ///
    /// The old expression, `gate_open_at − last_update`, is not an idle time: it depends
    /// on where in the 200 ms δ-update cycle the last transmission happened, and it
    /// saturates to zero as soon as a CBR report arrives after the gate deadline. The HUD
    /// and the metrics export this field as `T_off`.
    #[test]
    fn the_exported_t_off_is_the_wait_the_gate_imposed() {
        let mut ctx = TestCtx::new(31);
        let mut dcc = AdaptiveDcc::new();
        let node = NodeId::new(0);

        // An unloaded channel: δ = δ_max = 0.03, so T_on/δ for a 584 µs frame is
        // 19.5 ms, below the gatekeeper's 25 ms clamp — and the floor's unconditional
        // 25 ms is what is imposed.
        ctx.set_now(0);
        let req = request(400, 0);
        assert!(matches!(
            Dcc::gate(&mut dcc, &mut ctx, node, &req),
            GateDecision::Now { .. }
        ));
        let imposed = Dcc::<TestCtx>::state(&dcc, node).t_off;
        assert_eq!(imposed, Duration::from_millis(25), "{imposed:?}");
        // It is exactly the interval to the gate, which is the definition of an idle
        // time.
        let opens = dcc.nodes[&node.index()].gate_open_at;
        assert_eq!(imposed.as_nanos(), opens, "{imposed:?} vs {opens}");

        // A CBR report arriving after the gate deadline does not change what the last
        // gate imposed. The old expression saturated to zero here.
        ctx.set_now(500_000_000);
        Dcc::on_cbr(&mut dcc, &mut ctx, node, 0.9);
        assert_eq!(Dcc::<TestCtx>::state(&dcc, node).t_off, imposed);

        // Under load the bound binds and the exported value follows it. At CBR 0.9 the
        // clause gives T_on·(4000·(0.9 − 0.62)/0.9 − 1) = 0.584 ms · 1243.4444… =
        // 726.1716 ms for a 584 µs frame, and the smoothing loop is within a microsecond
        // of 0.9 after this many updates.
        for step in 1..=40u64 {
            ctx.set_now(500_000_000 + step * 200_000_000);
            Dcc::on_cbr(&mut dcc, &mut ctx, node, 0.9);
        }
        ctx.set_now(10_000_000_000);
        let decision = Dcc::gate(&mut dcc, &mut ctx, node, &req);
        assert!(matches!(decision, GateDecision::Now { .. }), "{decision:?}");
        let loaded = Dcc::<TestCtx>::state(&dcc, node).t_off;
        assert!(
            (loaded.as_secs_f64() * 1_000.0 - 726.171_555_555_555_6).abs() < 0.01,
            "{loaded:?}"
        );
        // And it is still exactly the interval to the gate.
        assert_eq!(
            loaded.as_nanos(),
            dcc.nodes[&node.index()].gate_open_at - 10_000_000_000
        );
    }

    #[test]
    fn a_frame_longer_than_4_ms_is_dropped_by_the_floor() {
        let mut ctx = TestCtx::new(3);
        let mut dcc = AdaptiveDcc::new();
        let node = NodeId::new(0);
        // A maximum-size MSDU at 3 Mbit/s is 6.2 ms of air time, which EN 302 571's
        // `0 < T_on <= 4 ms` forbids outright.
        let long = TxRequest {
            bytes: 2_304,
            mcs: Mcs::R3Bpsk12,
            air_time: air_time(2_304, Mcs::R3Bpsk12),
            ..request(2_304, 0)
        };
        assert!(
            long.air_time > Duration::from_millis(4),
            "{:?}",
            long.air_time
        );
        assert_eq!(
            Dcc::gate(&mut dcc, &mut ctx, node, &long),
            GateDecision::Drop
        );
        // The same frame at 27 Mbit/s fits, and goes.
        let short = TxRequest {
            mcs: Mcs::R27Qam64_34,
            air_time: air_time(2_304, Mcs::R27Qam64_34),
            ..long
        };
        assert!(short.air_time < Duration::from_millis(4));
        assert!(matches!(
            Dcc::gate(&mut dcc, &mut ctx, node, &short),
            GateDecision::Now { .. }
        ));
        // With the floor disabled the long frame is admitted, which is the
        // non-conformant configuration the card names.
        let mut unconstrained = AdaptiveDcc::new().with_floor(En302571Floor::disabled());
        assert!(matches!(
            Dcc::gate(&mut unconstrained, &mut ctx, NodeId::new(1), &long),
            GateDecision::Now { .. }
        ));
    }

    #[test]
    fn the_duty_cycle_cap_holds() {
        let mut ctx = TestCtx::new(4);
        let mut dcc = AdaptiveDcc::new();
        let node = NodeId::new(0);
        let req = request(2_304, 0);
        // 2,304 B at 6 Mbit/s is 3.128 ms; ten of them is 31 ms, above the 30 ms the 3 %
        // cap allows in a second. The gate must hold the eleventh back.
        let mut sent = 0;
        for i in 0..40u64 {
            ctx.set_now(i * 25_000_000);
            if matches!(
                Dcc::gate(&mut dcc, &mut ctx, node, &req),
                GateDecision::Now { .. }
            ) {
                sent += 1;
            }
        }
        let used = f64::from(sent) * req.air_time.as_secs_f64();
        assert!(
            used <= 0.031,
            "{sent} frames is {used} s of air time in one second"
        );
        assert!(sent > 0);
        assert!(dcc.duty_cycle(node, 1_000_000_000) <= 0.031);
    }

    #[test]
    fn the_reactive_tables_match_the_standard() {
        let a1 = ReactiveTable::A1TonUpTo1Ms;
        let rows_a1 = [
            (ReactiveState::Relaxed, 10.0, 100u64),
            (ReactiveState::Active1, 5.0, 200),
            (ReactiveState::Active2, 2.5, 400),
            (ReactiveState::Active3, 2.0, 500),
            (ReactiveState::Restrictive, 1.0, 1_000),
        ];
        for (state, rate, t_off_ms) in rows_a1 {
            let (r, t) = a1.limits(state);
            assert_eq!(r, rate, "{}", state.label());
            assert_eq!(t, Duration::from_millis(t_off_ms), "{}", state.label());
        }
        let a2 = ReactiveTable::A2TonUpTo500Us;
        let rows_a2 = [
            (ReactiveState::Relaxed, 20.0, 50u64),
            (ReactiveState::Active1, 10.0, 100),
            (ReactiveState::Active2, 5.0, 200),
            (ReactiveState::Active3, 4.0, 250),
            (ReactiveState::Restrictive, 1.0, 1_000),
        ];
        for (state, rate, t_off_ms) in rows_a2 {
            let (r, t) = a2.limits(state);
            assert_eq!(r, rate, "{}", state.label());
            assert_eq!(t, Duration::from_millis(t_off_ms), "{}", state.label());
        }
    }

    #[test]
    fn the_reactive_state_bands_are_exact_at_their_edges() {
        let a1 = ReactiveTable::A1TonUpTo1Ms;
        assert_eq!(a1.state_for(0.0), ReactiveState::Relaxed);
        assert_eq!(a1.state_for(0.299), ReactiveState::Relaxed);
        assert_eq!(a1.state_for(0.30), ReactiveState::Active1);
        assert_eq!(a1.state_for(0.399), ReactiveState::Active1);
        assert_eq!(a1.state_for(0.40), ReactiveState::Active2);
        assert_eq!(a1.state_for(0.499), ReactiveState::Active2);
        assert_eq!(a1.state_for(0.50), ReactiveState::Active3);
        assert_eq!(a1.state_for(0.60), ReactiveState::Active3);
        assert_eq!(a1.state_for(0.601), ReactiveState::Restrictive);
        // Table A.2 moves the Active-3 ceiling to 65 %.
        let a2 = ReactiveTable::A2TonUpTo500Us;
        assert_eq!(a2.state_for(0.62), ReactiveState::Active3);
        assert_eq!(a2.state_for(0.65), ReactiveState::Active3);
        assert_eq!(a2.state_for(0.66), ReactiveState::Restrictive);
    }

    #[test]
    fn the_reactive_gate_enforces_its_state_t_off() {
        let mut ctx = TestCtx::new(5);
        let mut dcc = ReactiveDcc::new();
        let node = NodeId::new(0);
        Dcc::on_cbr(&mut dcc, &mut ctx, node, 0.45);
        assert_eq!(dcc.state_of(node), ReactiveState::Active2);
        let req = request(300, 0);
        assert!(matches!(
            Dcc::gate(&mut dcc, &mut ctx, node, &req),
            GateDecision::Now { .. }
        ));
        // Active 2 on Table A.1 is 400 ms of idle time.
        let decision = Dcc::gate(&mut dcc, &mut ctx, node, &req);
        assert_eq!(
            decision,
            GateDecision::DelayUntil(Duration::from_millis(400).as_nanos())
        );
        let state = Dcc::<TestCtx>::state(&dcc, node);
        assert_eq!(state.algorithm, DccAlgorithm::ReactiveTs102687);
        assert_eq!(state.state, Some(ReactiveState::Active2));
        assert_eq!(state.t_off, Duration::from_millis(400));
    }

    #[test]
    fn the_j2945_parameter_table_matches_the_paper() {
        let p = J2945Params::ROSTAMI;
        assert_eq!(p.lambda, 0.5);
        assert_eq!(p.b_density, 25.0);
        assert_eq!(p.max_itt, Duration::from_millis(600));
        assert_eq!(p.min_itt, Duration::from_millis(100));
        assert_eq!(p.rate_control_interval, Duration::from_millis(100));
        assert_eq!(p.counting_radius_m, 100.0);
        assert_eq!(p.rp_max_dbm, 20.0);
        assert_eq!(p.rp_min_dbm, 10.0);
        assert_eq!(p.min_cu, 0.50);
        assert_eq!(p.max_cu, 0.80);
        assert_eq!(p.supra_gain, 0.5);
        assert_eq!(p.rp_initial_dbm, 15.0);
        assert_eq!(p.te_min_m, 0.2);
        assert_eq!(p.te_max_m, 0.5);
        assert_eq!(p.cert_attach_interval, Duration::from_millis(450));
    }

    #[test]
    fn the_itt_ramp_matches_the_paper() {
        let p = J2945Params::ROSTAMI;
        // At or below B = 25 vehicles the ITT is the 100 ms minimum.
        assert_eq!(SaeJ2945Dcc::max_itt_for(p, 0.0), Duration::from_millis(100));
        assert_eq!(
            SaeJ2945Dcc::max_itt_for(p, 25.0),
            Duration::from_millis(100)
        );
        // It reaches vMaxITT at (vMaxITT/100)·B = 150 vehicles and is capped there.
        assert_eq!(
            SaeJ2945Dcc::max_itt_for(p, 150.0),
            Duration::from_millis(600)
        );
        assert_eq!(
            SaeJ2945Dcc::max_itt_for(p, 400.0),
            Duration::from_millis(600)
        );
        // And it is monotone in between.
        let mut previous = 0;
        for n in 0..200 {
            let itt = SaeJ2945Dcc::max_itt_for(p, f64::from(n)).as_nanos();
            assert!(itt >= previous, "fell at {n}");
            previous = itt;
        }
        // The smoothing: N_smooth = λ·N + (1 − λ)·N_smooth_prev.
        assert_eq!(SaeJ2945Dcc::smooth_density(p, 0.0, 40.0), 20.0);
        assert_eq!(SaeJ2945Dcc::smooth_density(p, 20.0, 40.0), 30.0);
    }

    #[test]
    fn the_power_ramp_matches_the_paper() {
        let p = J2945Params::ROSTAMI;
        // f(CBP): vRPMax below vMinCU, vRPMin above vMaxCU, linear between.
        assert_eq!(SaeJ2945Dcc::power_target_dbm(p, 0.0), 20.0);
        assert_eq!(SaeJ2945Dcc::power_target_dbm(p, 0.50), 20.0);
        assert_eq!(SaeJ2945Dcc::power_target_dbm(p, 0.65), 15.0);
        assert_eq!(SaeJ2945Dcc::power_target_dbm(p, 0.80), 10.0);
        assert_eq!(SaeJ2945Dcc::power_target_dbm(p, 1.0), 10.0);
        // The SUPRA filter closes half the gap each step.
        let once = SaeJ2945Dcc::supra_step(p, 20.0, 0.80);
        assert_eq!(once, 15.0);
        let twice = SaeJ2945Dcc::supra_step(p, once, 0.80);
        assert_eq!(twice, 12.5);
        // And it converges to the target.
        let mut rp = p.rp_initial_dbm;
        for _ in 0..50 {
            rp = SaeJ2945Dcc::supra_step(p, rp, 0.80);
        }
        assert!((rp - 10.0).abs() < 1e-9, "{rp}");
    }

    #[test]
    fn the_j2945_gate_reports_and_applies_its_power() {
        let mut ctx = TestCtx::new(6);
        let mut dcc = SaeJ2945Dcc::new();
        let node = NodeId::new(0);
        // A heavy channel: the power comes down.
        for i in 0..20u64 {
            ctx.set_now(i * 100_000_000);
            Dcc::on_cbr(&mut dcc, &mut ctx, node, 0.85);
        }
        let power = dcc.power_dbm(node);
        assert!((power - 10.0).abs() < 0.1, "{power}");
        // A dense neighbourhood: the ITT goes up.
        for _ in 0..20 {
            dcc.on_density(&mut ctx, node, 200);
        }
        assert!(dcc.smoothed_density(node) > 150.0);
        assert_eq!(dcc.itt(node), Duration::from_millis(600));
        let req = request(300, ctx.now());
        let decision = Dcc::gate(&mut dcc, &mut ctx, node, &req);
        let GateDecision::Now { power_dbm, .. } = decision else {
            panic!("expected Now, got {decision:?}");
        };
        assert!((power_dbm - 10.0).abs() < 0.1, "{power_dbm}");
        let state = Dcc::<TestCtx>::state(&dcc, node);
        assert_eq!(state.algorithm, DccAlgorithm::SaeJ2945_1);
        assert_eq!(state.itt, Some(Duration::from_millis(600)));
        assert_eq!(state.power_dbm, Some(power));
    }

    #[test]
    fn the_tracking_error_trigger_is_a_ramp_in_probability() {
        let p = J2945Params::ROSTAMI;
        assert_eq!(SaeJ2945Dcc::tracking_error_probability(p, 0.1, 1.0), 0.0);
        assert_eq!(SaeJ2945Dcc::tracking_error_probability(p, 0.2, 1.0), 0.0);
        assert_eq!(SaeJ2945Dcc::tracking_error_probability(p, 0.5, 1.0), 1.0);
        assert_eq!(SaeJ2945Dcc::tracking_error_probability(p, 2.0, 1.0), 1.0);
        let mid = SaeJ2945Dcc::tracking_error_probability(p, 0.35, 1.0);
        assert!((0.0..1.0).contains(&mid), "{mid}");
        // Monotone in the error.
        let a = SaeJ2945Dcc::tracking_error_probability(p, 0.25, 1.0);
        let b = SaeJ2945Dcc::tracking_error_probability(p, 0.45, 1.0);
        assert!(b > a, "{a} {b}");
    }

    #[test]
    fn an_unrestricted_state_is_the_default() {
        assert_eq!(DccState::default(), DccState::UNRESTRICTED);
        assert_eq!(
            DccState::UNRESTRICTED.generator_view(),
            (Duration::ZERO, None)
        );
        assert_eq!(DccState::UNRESTRICTED.algorithm, DccAlgorithm::None);
    }

    #[test]
    fn the_cards_validate_and_register() {
        let mut registry = v2xw_core::registry::Registry::new();
        for card in [
            adaptive_card(),
            reactive_card(ReactiveTable::A1TonUpTo1Ms),
            j2945_card(),
        ] {
            card.validate().expect("card validates");
            card.check_api_version().expect("api version");
            registry.register(card).expect("registers");
        }
        assert!(registry.contains(AdaptiveDcc::ID));
        assert!(registry.contains(ReactiveDcc::ID));
        assert!(registry.contains(SaeJ2945Dcc::ID));
        // Rule R1: every todo-calibrate parameter carries a plan. `validate` enforces it,
        // and these are the ones that have to have one.
        let report = registry.todo_calibrate_report();
        let names: Vec<&str> = report.iter().map(|(_, p)| p.name.as_str()).collect();
        // The idle-time bound is NOT one of them any more: it is the standard's own
        // equation, grouped as the standard writes it, and it reproduces both of
        // TS 103 175 Table 2's worked examples to 0.01 %.
        assert!(!names.contains(&"t_off_limit_coefficient"), "{names:?}");
        assert!(!names.contains(&"cbr_unit"), "{names:?}");
        assert!(names.contains(&"te_alpha"), "{names:?}");
        assert!(names.contains(&"hold_ms"), "{names:?}");
    }

    #[test]
    fn a_dcc_model_can_be_used_as_a_trait_object() {
        let mut ctx = TestCtx::new(7);
        let mut dcc: Box<dyn Dcc<TestCtx>> =
            Box::new(ReactiveDcc::new().with_table(ReactiveTable::A2TonUpTo500Us));
        let node = NodeId::new(0);
        dcc.on_cbr(&mut ctx, node, 0.2);
        assert!(matches!(
            dcc.gate(&mut ctx, node, &request(300, 0)),
            GateDecision::Now { .. }
        ));
        assert_eq!(dcc.state(node).state, Some(ReactiveState::Relaxed));
        assert_eq!(dcc.id(), ReactiveDcc::ID);
    }
}
