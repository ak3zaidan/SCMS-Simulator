//! Jammers on the air: `threats.jammers`.
//!
//! A jammer is a transmitter with no protocol (04-models.md §12.3): it raises the energy
//! at every receiver in range and enters the interference sums the way thermal noise does.
//! `v2xw_radio::jamming` owns *when* each profile is on the air and *how much* power it
//! radiates; this module owns the geometry — where the jammer stands, what it hears, and
//! what each receiver gets from it through the run's own propagation and obstacle stack.
//!
//! # How a jammer reaches a receiver
//!
//! * **Constant and pulsed** jammers are declared once per mobility step: their emission
//!   windows over `[t, t + step)` from the profile, at every node within the candidate
//!   range, at the received power the node's position at `t` gives. The power is the
//!   deterministic large-scale budget — path loss, shadowing and buildings, no fast-fading
//!   draw — because a wideband noise emission averages over the fading across its band.
//! * **Reactive** jammers are driven by what they hear: when a frame starts, the jammer's
//!   own receiver measures it through the same budget, and if that energy reaches the
//!   trigger the profile's emission window over the frame is declared at every receiver of
//!   that frame.
//!
//! The 802.11p PHY then does the rest (`v2xw_radio::OfdmPhy`): the energy raises the SINR
//! denominator for the windows it covers, makes clear-channel assessment report a busy
//! medium, and a frame the jammer killed is attributed to it on the PHY's counterfactual
//! (`LossCause::Jammed`). A jammer above the CBR threshold is also busy time on the
//! node's channel-busy-ratio meter, which is what J2945/1 congestion control reads. On a
//! sidelink the jammer's power is a co-slot interferer across the whole pool, and a lost
//! transport block that the same draw would have delivered without it is `jammed`.
//!
//! # Where a jammer is
//!
//! Exactly one of three: `position_m`, a fixed world point; `follow_node`, the node id of
//! a vehicle (or roadside unit) it rides — its antenna at the vehicle's, and silent while
//! that node does not exist; or `path_m`, a polyline of world points it drives along at
//! `speed_mps`, stopping at the end or, with `loop_path: true`, going round again. The
//! position is re-evaluated at every mobility step and at every frame a reactive jammer
//! hears, from the simulated clock alone, so a moving jammer is as deterministic as a
//! fixed one.
//!
//! On a sidelink the jammer's energy is also measured: it enters every UE's S-RSSI in each
//! sub-channel it covers ([`v2xw_radio::SpsEngine::note_jammer`]), so it raises the CBR —
//! and through it the congestion control's CR limit — and the S-RSSI ranking steers
//! reservations away from the slots a pulsed jammer covers. It carries no SCI, so it
//! excludes no resource in step 3.

use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};
use v2xw_radio::jamming::JammerProfile;
use v2xw_radio::{
    ChannelId, ConstantJammer, JamWindow, JammerKind, PulsedJammer, RadioEndpoint, ReactiveJammer,
    SensedInterval,
};

use super::Engine;
use crate::ctx::EngineCtx;
use crate::scenario::Scenario;

/// The first node id a jammer takes. Jammers are not nodes of the run, but a
/// `JamArrival` names its source by `NodeId`; counting down from the top of the id space
/// keeps them clear of every vehicle and roadside unit, which count up from zero.
pub const JAMMER_ID_BASE: u32 = 0xF000_0000;

/// How a jammer moves.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum JammerMotion {
    /// Standing at `JammerSpec::position`.
    Fixed,
    /// Riding a node: its antenna is the node's.
    Follow {
        /// The node.
        node: u32,
    },
    /// Driving a polyline at a constant speed.
    Path {
        /// The waypoints, world metres (antenna height in z).
        points: Vec<Vec3>,
        /// Speed along the path, m/s.
        speed_mps: f64,
        /// Whether it goes round again at the end.
        looped: bool,
    },
}

impl JammerMotion {
    /// Where a path jammer is `t_s` seconds after it set off.
    fn along_path(points: &[Vec3], speed_mps: f64, looped: bool, t_s: f64) -> Vec3 {
        let lengths: Vec<f64> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
        let total = v2xw_core::math::sum_ordered(lengths.iter().copied());
        if total <= 0.0 || points.len() < 2 {
            return points.first().copied().unwrap_or(Vec3::ZERO);
        }
        let mut d = (speed_mps * t_s).max(0.0);
        if looped {
            d %= total;
        } else if d >= total {
            return *points.last().expect("two points or more");
        }
        for (i, len) in lengths.iter().enumerate() {
            if d <= *len {
                let f = if *len > 0.0 { d / len } else { 0.0 };
                let (a, b) = (points[i], points[i + 1]);
                return Vec3::new(
                    a.x + (b.x - a.x) * f,
                    a.y + (b.y - a.y) * f,
                    a.z + (b.z - a.z) * f,
                );
            }
            d -= len;
        }
        *points.last().expect("two points or more")
    }
}

/// One jammer as the scenario declared it, parsed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JammerSpec {
    /// Which profile.
    pub kind: JammerKind,
    /// Where its antenna stands, world metres, for a fixed jammer; the first waypoint for
    /// a path jammer; the origin for one riding a node.
    pub position: Vec3,
    /// How it moves.
    pub motion: JammerMotion,
    /// Its transmit power, dBm; the profile's cited default when absent.
    pub power_dbm: Option<f64>,
    /// When it is active, simulated seconds `[from, to)`.
    pub from_s: f64,
    /// See `from_s`; `None` is until the end of the run.
    pub to_s: Option<f64>,
    /// The pulsed profile's period, milliseconds.
    pub period_ms: Option<f64>,
    /// The pulsed profile's duty cycle, `(0, 1]`.
    pub duty: Option<f64>,
    /// The reactive profile's trigger, dBm.
    pub trigger_dbm: Option<f64>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JammerParams {
    #[serde(default)]
    position_m: Option<Vec<f64>>,
    #[serde(default)]
    follow_node: Option<u32>,
    #[serde(default)]
    path_m: Option<Vec<Vec<f64>>>,
    #[serde(default)]
    speed_mps: Option<f64>,
    #[serde(default)]
    loop_path: Option<bool>,
    #[serde(default)]
    power_dbm: Option<f64>,
    #[serde(default)]
    from_s: Option<f64>,
    #[serde(default)]
    to_s: Option<f64>,
    #[serde(default)]
    period_ms: Option<f64>,
    #[serde(default)]
    duty: Option<f64>,
    #[serde(default)]
    trigger_dbm: Option<f64>,
}

/// Parses `threats.jammers`, returning every problem with the path it is at.
///
/// # Errors
/// One `(path, reason)` per unknown id, missing or malformed parameter.
pub fn jammer_specs(scenario: &Scenario) -> Result<Vec<JammerSpec>, Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (i, j) in scenario.threats.jammers.iter().enumerate() {
        let path = format!("threats.jammers[{i}]");
        let kind = match j.id.as_str() {
            ConstantJammer::ID => JammerKind::Constant,
            PulsedJammer::ID => JammerKind::Pulsed,
            ReactiveJammer::ID => JammerKind::Reactive,
            other => {
                errors.push((
                    format!("{path}.id"),
                    format!(
                        "'{other}' is not a jammer this build ships; choose one of: {}, {}, {}",
                        ConstantJammer::ID,
                        PulsedJammer::ID,
                        ReactiveJammer::ID
                    ),
                ));
                continue;
            }
        };
        let p: JammerParams = match serde_json::from_value(j.params.clone()) {
            Ok(p) => p,
            Err(e) => {
                errors.push((
                    format!("{path}.params"),
                    format!(
                        "do not fit a jammer: {e}. A jammer needs one of position_m: [x, y] \
                         or [x, y, z] in world metres, follow_node: <node id>, or path_m: \
                         [[x, y], ...] with speed_mps (and loop_path); it takes power_dbm, \
                         from_s, to_s, and for a pulsed one period_ms and duty, for a \
                         reactive one trigger_dbm"
                    ),
                ));
                continue;
            }
        };
        let point = |v: &[f64]| -> Option<Vec3> {
            let p = match v {
                [x, y] => Vec3::new(*x, *y, 1.5),
                [x, y, z] => Vec3::new(*x, *y, *z),
                _ => return None,
            };
            (p.x.is_finite() && p.y.is_finite() && p.z.is_finite()).then_some(p)
        };
        let given = [
            p.position_m.is_some(),
            p.follow_node.is_some(),
            p.path_m.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        if given != 1 {
            errors.push((
                format!("{path}.params"),
                "must place the jammer exactly one way: position_m, follow_node or path_m"
                    .to_string(),
            ));
            continue;
        }
        let (position, motion) = if let Some(v) = p.position_m.as_deref() {
            let Some(pos) = point(v) else {
                errors.push((
                    format!("{path}.params.position_m"),
                    "must be [x, y] or [x, y, z] in finite world metres".to_string(),
                ));
                continue;
            };
            (pos, JammerMotion::Fixed)
        } else if let Some(node) = p.follow_node {
            (Vec3::ZERO, JammerMotion::Follow { node })
        } else {
            let raw = p.path_m.as_deref().unwrap_or(&[]);
            let points: Option<Vec<Vec3>> = raw.iter().map(|v| point(v)).collect();
            let speed = p.speed_mps.unwrap_or(0.0);
            match points {
                Some(points) if points.len() >= 2 && speed.is_finite() && speed > 0.0 => (
                    points[0],
                    JammerMotion::Path {
                        points,
                        speed_mps: speed,
                        looped: p.loop_path.unwrap_or(false),
                    },
                ),
                _ => {
                    errors.push((
                        format!("{path}.params.path_m"),
                        "needs at least two [x, y] or [x, y, z] waypoints in finite world \
                         metres and a positive speed_mps"
                            .to_string(),
                    ));
                    continue;
                }
            }
        };
        if p.speed_mps.is_some() && p.path_m.is_none() {
            errors.push((
                format!("{path}.params.speed_mps"),
                "only applies to a path_m jammer".to_string(),
            ));
        }
        if let Some(w) = p.power_dbm
            && !(w.is_finite() && (-30.0..=40.0).contains(&w))
        {
            errors.push((
                format!("{path}.params.power_dbm"),
                format!("is {w}; a jammer's transmit power is a number in [-30, 40] dBm"),
            ));
        }
        let from_s = p.from_s.unwrap_or(0.0);
        if !(from_s.is_finite() && from_s >= 0.0)
            || p.to_s.is_some_and(|t| !(t.is_finite() && t > from_s))
        {
            errors.push((
                format!("{path}.params"),
                "from_s and to_s must be a non-empty interval of simulated seconds".to_string(),
            ));
        }
        if p.duty.is_some_and(|d| !(d > 0.0 && d <= 1.0))
            || p.period_ms.is_some_and(|t| !(t.is_finite() && t > 0.0))
        {
            errors.push((
                format!("{path}.params"),
                "a pulsed jammer's duty is in (0, 1] and its period_ms is positive".to_string(),
            ));
        }
        out.push(JammerSpec {
            kind,
            position,
            motion,
            power_dbm: p.power_dbm,
            from_s,
            to_s: p.to_s,
            period_ms: p.period_ms,
            duty: p.duty,
            trigger_dbm: p.trigger_dbm,
        });
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// One profile, as a closed set so the run can hold them without a generic context type.
#[derive(Debug, Clone)]
enum Profile {
    Constant(ConstantJammer),
    Pulsed(PulsedJammer),
    Reactive(ReactiveJammer),
}

impl Profile {
    fn windows(
        &mut self,
        ctx: &mut EngineCtx<'_>,
        id: NodeId,
        from: SimTime,
        to: SimTime,
        sensed: &[SensedInterval],
    ) -> Vec<JamWindow> {
        match self {
            Profile::Constant(j) => JammerProfile::windows(j, ctx, id, from, to, sensed),
            Profile::Pulsed(j) => JammerProfile::windows(j, ctx, id, from, to, sensed),
            Profile::Reactive(j) => JammerProfile::windows(j, ctx, id, from, to, sensed),
        }
    }

    fn power_dbm(&self) -> f64 {
        match self {
            Profile::Constant(j) => j.power_dbm(),
            Profile::Pulsed(j) => j.power_dbm(),
            Profile::Reactive(j) => j.power_dbm(),
        }
    }

    fn card(&self) -> v2xw_core::card::ModelCard {
        use v2xw_core::model::Model;
        match self {
            Profile::Constant(j) => j.card().clone(),
            Profile::Pulsed(j) => j.card().clone(),
            Profile::Reactive(j) => j.card().clone(),
        }
    }
}

/// One jammer in the run.
#[derive(Debug, Clone)]
pub(crate) struct Jammer {
    pub(crate) id: NodeId,
    pub(crate) kind: JammerKind,
    pub(crate) position: Vec3,
    pub(crate) motion: JammerMotion,
    active_from: SimTime,
    active_to: SimTime,
    profile: Profile,
}

/// Every jammer in the run.
#[derive(Debug, Clone, Default)]
pub(crate) struct Jamming {
    pub(crate) jammers: Vec<Jammer>,
}

impl Jamming {
    /// The jammers the scenario declared, on `channel`. Invalid specs were refused by the
    /// loader, so a parse failure here yields none.
    pub(crate) fn for_scenario(scenario: &Scenario, channel: ChannelId) -> Self {
        let specs = jammer_specs(scenario).unwrap_or_default();
        let horizon = scenario.time.horizon_ns();
        let jammers = specs
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let profile = match s.kind {
                    JammerKind::Constant => {
                        let mut j = ConstantJammer::new().on_channel(channel);
                        if let Some(p) = s.power_dbm {
                            j = j.with_power_dbm(p);
                        }
                        Profile::Constant(j)
                    }
                    JammerKind::Pulsed => {
                        let mut j = PulsedJammer::new().on_channel(channel);
                        if let Some(p) = s.power_dbm {
                            j = j.with_power_dbm(p);
                        }
                        if s.period_ms.is_some() || s.duty.is_some() {
                            let period = s.period_ms.map_or(
                                Duration::from_nanos(v2xw_radio::jamming::PULSED_DEFAULT_PERIOD_NS),
                                |ms| Duration::from_nanos((ms * 1e6).round() as u64),
                            );
                            j = j.with_duty_cycle(
                                period,
                                s.duty.unwrap_or(v2xw_radio::jamming::PULSED_DEFAULT_DUTY),
                            );
                        }
                        Profile::Pulsed(j)
                    }
                    JammerKind::Reactive => {
                        let mut j = ReactiveJammer::new().on_channel(channel);
                        if let Some(p) = s.power_dbm {
                            j = j.with_power_dbm(p);
                        }
                        if let Some(t) = s.trigger_dbm {
                            j = j.with_trigger_dbm(t);
                        }
                        Profile::Reactive(j)
                    }
                };
                Jammer {
                    id: NodeId::new(JAMMER_ID_BASE + i as u32),
                    kind: s.kind,
                    position: s.position,
                    motion: s.motion,
                    active_from: (s.from_s * 1e9).round() as u64,
                    active_to: s.to_s.map_or(horizon, |t| (t * 1e9).round() as u64),
                    profile,
                }
            })
            .collect();
        Self { jammers }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.jammers.is_empty()
    }

    /// The cards of the jammer models in the run.
    pub(crate) fn cards(&self) -> Vec<v2xw_core::card::ModelCard> {
        self.jammers.iter().map(|j| j.profile.card()).collect()
    }
}

impl Engine {
    /// Where jammer `k` is at `now`, or `None` while the node it rides does not exist.
    pub(crate) fn jammer_position(&self, k: usize, now: SimTime) -> Option<Vec3> {
        let j = &self.jamming.jammers[k];
        match &j.motion {
            JammerMotion::Fixed => Some(j.position),
            JammerMotion::Follow { node } => {
                let pos = self.node_pos(NodeId::new(*node), now)?;
                // On a vehicle, the antenna stands at a car's antenna height above the road
                // (TR 36.885 via 04-models.md §3.7); a roadside unit's position is already
                // its antenna's.
                if self.rsus.contains_key(&NodeId::new(*node)) {
                    Some(pos)
                } else {
                    Some(Vec3::new(pos.x, pos.y, pos.z + 1.5))
                }
            }
            JammerMotion::Path {
                points,
                speed_mps,
                looped,
            } => {
                let t_s = now.saturating_sub(j.active_from) as f64 * 1e-9;
                Some(JammerMotion::along_path(points, *speed_mps, *looped, t_s))
            }
        }
    }

    /// The deterministic large-scale power a jammer at `from` delivers to a node at `to`,
    /// dBm: path loss, shadowing and obstacles, no fast-fading draw.
    fn jam_power_dbm(
        &mut self,
        jammer: NodeId,
        from: Vec3,
        tx_dbm: f64,
        rx: NodeId,
        to: Vec3,
    ) -> f64 {
        let now = self.scheduler.now();
        let freq_hz = self.carrier_hz();
        let tx_end: RadioEndpoint = self.endpoint(jammer, from, now);
        let rx_end = self.endpoint(rx, to, now);
        let los = self.obstacles.classify(&self.world, tx_end.pos, rx_end.pos);
        let charge_owned = self.main_law.owns_buildings();
        let (loss, obstacle_db) = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                propagation,
                weather,
                obstacles,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            let loss = propagation.loss_db(&mut ctx, &tx_end, &rx_end, freq_hz, &los, weather);
            let obstacle_db = obstacles.loss_db(
                &mut ctx,
                &tx_end,
                &rx_end,
                &los,
                freq_hz,
                loss.path_db,
                !charge_owned,
            );
            (loss, obstacle_db)
        };
        v2xw_core::math::sum_ordered([tx_dbm, -loss.total_db, -obstacle_db])
    }

    /// How far a jammer radiating `tx_dbm` from `at` is followed: the candidate range of
    /// its EIRP (`radio.range`).
    fn jammer_reach_m(&mut self, jammer: NodeId, at: Vec3, tx_dbm: f64, now: SimTime) -> f64 {
        let gain = self.endpoint(jammer, at, now).gain_dbi;
        self.range.full_m(tx_dbm + gain)
    }

    /// Every node within `range_m` of `at`, with its position now.
    fn nodes_near(&self, at: Vec3, range_m: f64, now: SimTime) -> Vec<(NodeId, Vec3)> {
        let mut out: Vec<(NodeId, Vec3)> = Vec::new();
        for actor in self.snapshot.actors_within(at, range_m) {
            if let Some(rec) = self.actors.get(&actor)
                && let Some(node) = rec.node
            {
                out.push((node, rec.last.extrapolate(now).pos));
            }
        }
        for (&rsu, &pos) in &self.rsus {
            if pos.distance(at) <= range_m {
                out.push((rsu, pos));
            }
        }
        out.sort_by_key(|(n, _)| *n);
        out
    }

    /// Declares the constant and pulsed jammers' emission over `[now, now + step)` at every
    /// node in range. Called once per mobility step.
    pub(super) fn declare_jamming(&mut self, now: SimTime, step: Duration) {
        if self.jamming.is_empty() {
            return;
        }
        let to = step.after(now);
        let channel = self.access_channel();
        // Keep one step of history: a frame that started before `now` may still be
        // evaluated against a window that ended at it.
        let cutoff = now.saturating_sub(step.as_nanos());
        self.phy.jamming_mut().prune(cutoff);
        let count = self.jamming.jammers.len();
        for k in 0..count {
            let (id, kind, from_a, to_a) = {
                let j = &self.jamming.jammers[k];
                (j.id, j.kind, j.active_from, j.active_to)
            };
            if kind == JammerKind::Reactive || to_a <= now || from_a >= to {
                continue;
            }
            let Some(pos) = self.jammer_position(k, now) else {
                continue;
            };
            let (from, until) = (now.max(from_a), to.min(to_a));
            let windows = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    jamming,
                    ..
                } = self;
                let mut null = crate::ctx::NullRecorder::new();
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, &mut null,
                );
                jamming.jammers[k]
                    .profile
                    .windows(&mut ctx, id, from, until, &[])
            };
            if windows.is_empty() {
                continue;
            }
            let tx_dbm = self.jamming.jammers[k].profile.power_dbm();
            // A jammer reaches as far as its own link budget does (`radio.range`).
            let reach = self.jammer_reach_m(id, pos, tx_dbm, now);
            for (node, node_pos) in self.nodes_near(pos, reach, now) {
                let power = self.jam_power_dbm(id, pos, tx_dbm, node, node_pos);
                self.declare_at(node, id, power, channel, kind, &windows);
            }
        }
    }

    /// A reactive jammer hears a frame starting and, if it reaches the trigger, jams it at
    /// every receiver of that frame.
    pub(super) fn react_to_frame(
        &mut self,
        tx: NodeId,
        tx_pos: Vec3,
        tx_power_dbm: f64,
        start: SimTime,
        end: SimTime,
        receivers: &[(NodeId, Vec3)],
    ) {
        if self.jamming.is_empty() {
            return;
        }
        let channel = self.access_channel();
        let count = self.jamming.jammers.len();
        for k in 0..count {
            let (id, kind, from_a, to_a) = {
                let j = &self.jamming.jammers[k];
                (j.id, j.kind, j.active_from, j.active_to)
            };
            if kind != JammerKind::Reactive || start >= to_a || start < from_a {
                continue;
            }
            let Some(pos) = self.jammer_position(k, start) else {
                continue;
            };
            // What the jammer's own receiver hears of the frame.
            let heard = self.jam_power_dbm(tx, tx_pos, tx_power_dbm, id, pos);
            let sensed = [SensedInterval::new(start, end, heard)];
            let windows = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    jamming,
                    ..
                } = self;
                let mut null = crate::ctx::NullRecorder::new();
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, &mut null,
                );
                jamming.jammers[k]
                    .profile
                    .windows(&mut ctx, id, start, end.max(start + 1), &sensed)
            };
            if windows.is_empty() {
                continue;
            }
            let tx_dbm = self.jamming.jammers[k].profile.power_dbm();
            let reach = self.jammer_reach_m(id, pos, tx_dbm, start);
            for &(rx, rx_pos) in receivers {
                if rx_pos.distance(pos) > reach {
                    continue;
                }
                let power = self.jam_power_dbm(id, pos, tx_dbm, rx, rx_pos);
                self.declare_at(rx, id, power, channel, kind, &windows);
            }
        }
    }

    fn declare_at(
        &mut self,
        rx: NodeId,
        jammer: NodeId,
        power_dbm: f64,
        channel: ChannelId,
        kind: JammerKind,
        windows: &[JamWindow],
    ) {
        self.phy
            .jamming_mut()
            .insert_windows(rx, jammer, power_dbm, channel, kind, windows);
        // A sidelink UE measures the jammer's energy as S-RSSI in every sub-channel it
        // covers: it enters the CBR and the S-RSSI ranking (it carries no SCI, so it
        // excludes nothing).
        self.sidelink_note_jam(rx, power_dbm, windows);
        if power_dbm >= v2xw_radio::phy::CBR_BUSY_THRESHOLD_DBM
            && let Some(mac) = self.mac.as_mut()
        {
            for w in windows {
                mac.note_busy(rx, channel, w.from, w.to);
            }
        }
    }

    /// The channel the run's access layer transmits on.
    pub(super) fn access_channel(&self) -> ChannelId {
        self.sidelink
            .as_ref()
            .map_or(super::SAFETY_CHANNEL, |sl| sl.channel)
    }
}
