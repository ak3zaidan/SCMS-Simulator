//! The attack families 07-threats-and-detection.md §2.2 adds to the ported catalogue:
//! map-aware ghost vehicles, the relay (wormhole) replay, oversized-message flooding,
//! certificate misuse across regions, phantom collective perception, and the three
//! jammer profiles.
//!
//! # Why these are a second attacker and not more [`crate::attack::AttackKind`] variants
//!
//! [`crate::attack_legacy::LegacyAttacker`] reproduces the legacy engine byte for byte,
//! and `AttackKind` is the legacy name space: a scenario file, a legacy dataset column and
//! a foundry genome all carry those names as strings, and `tests/legacy_conformance.rs`
//! asserts the enum against the legacy source at test time. Adding a new name to it would
//! make that assertion a moving target. The new families therefore get their own kind enum
//! and their own model, and [`crate::catalog`] joins the two into the one namespace a
//! scenario parses.
//!
//! # What each rendering can and cannot reach
//!
//! Two of these attacks need something the engine does not expose yet, and both say so
//! rather than faking it:
//!
//! * **Jamming physics is not here.** An interferer in the SINR sums is `v2xw-radio`'s,
//!   and as of this writing that crate has it: `v2xw_radio::jamming` carries the
//!   `JammerProfile` trait, `JamWindow`, `SensedInterval`, `JammingField` and the three
//!   profiles of 04-models.md §12.3, with the same Puñal anchors this module cites. What
//!   this model does is the **adversary** half — the declared capability, the schedule, the
//!   geofence, the coalition — and it hands the burst over on
//!   [`crate::attack::Emission::raw_energy`], logging `TransmitRaw` for the ground truth.
//!   It deletes no frames at any receiver, because a jammer that did that would report a
//!   blind area that owed nothing to propagation.
//!
//!   The engine joins the two: one [`crate::attack::RawEnergy`] declared at `t` becomes one
//!   `JamWindow::new(t, t + duration_us)` at that jammer's power and channel, with
//!   [`crate::attack::JamProfile::Constant`] → `JammerKind::Constant`,
//!   [`crate::attack::JamProfile::RandomDuty`] → `JammerKind::Pulsed` and
//!   [`crate::attack::JamProfile::Reactive`] → `JammerKind::Reactive`, whose
//!   `SensedInterval` input only the engine can supply. **The two sides must not both
//!   schedule.** `JammerProfile::windows` can produce its own windows from a time span, so
//!   an integrator who wires both a radio-side profile and this attacker will jam twice;
//!   the intended arrangement is that this attacker's declaration is what drives the
//!   radio-side profile. See the crate report.
//! * **Reactive jamming** triggers on sensed energy above −75 dBm (04-models.md §12.3).
//!   [`crate::attack::AttackerView`] carries no channel-busy or RSSI reading, so this port
//!   triggers on "this node received something in the last interval", which is the
//!   belief-side proxy a receiver actually has, and carries the real trigger threshold on
//!   the burst so the host can apply it. See the crate report.
//!
//! # Map knowledge
//!
//! [`crate::capability::Knowledge::map`] lets an attacker place a fabricated position on a
//! lane so that an off-road check passes. The map an attacker has is *its own*, so it
//! arrives as declared [`LaneHint`]s rather than as a borrow of the world; with no hints,
//! a ghost is placed along the attacker's own claimed heading, which is what a mapless
//! attacker can do and is visibly worse at evading `mapOffRoad`.

use std::collections::BTreeMap;

use crate::attack::{
    AttackAction, AttackFamily, Attacker, AttackerView, Emission, JamProfile, MAX_MSDU_BYTES,
    RawEnergy,
};
use crate::capability::{AttackSchedule, Capabilities, wrap_heading};
use crate::cards::{LEGACY_PY, design, legacy, legacy_param, paper, standard};
use crate::ctx::ThreatCtx;
use crate::obs::{ObservedMessage, PerceivedObject, RegionId};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{SimTime, ns_to_secs, secs_to_ns};

/// The model id this module's card and its `gt.attack.action` records carry.
pub const MODEL_ID: &str = "threat/attacker/new-families";

/// One straight lane segment an attacker knows, in world-local ENU metres.
///
/// The attacker's **declared** map knowledge (07-threats-and-detection.md §1), not a
/// window on the world: a scenario that gives an attacker `Knowledge::map` supplies the
/// segments it knows, and nothing here can ask the world a question.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneHint {
    /// The segment's start, east metres.
    pub x0_m: f64,
    /// The segment's start, north metres.
    pub y0_m: f64,
    /// The segment's end, east metres.
    pub x1_m: f64,
    /// The segment's end, north metres.
    pub y1_m: f64,
}

impl LaneHint {
    /// A segment from `(x0, y0)` to `(x1, y1)`.
    #[must_use]
    pub const fn new(x0_m: f64, y0_m: f64, x1_m: f64, y1_m: f64) -> Self {
        Self {
            x0_m,
            y0_m,
            x1_m,
            y1_m,
        }
    }

    /// The segment's direction, ENU radians, `0 = east`.
    #[must_use]
    pub fn heading_rad(&self) -> f64 {
        wrap_heading(math::atan2(self.y1_m - self.y0_m, self.x1_m - self.x0_m))
    }

    /// The distance from `(x_m, y_m)` to the segment, metres.
    #[must_use]
    pub fn distance_to_m(&self, x_m: f64, y_m: f64) -> f64 {
        let (dx, dy) = (self.x1_m - self.x0_m, self.y1_m - self.y0_m);
        let len2 = dx * dx + dy * dy;
        if len2 <= 0.0 {
            return math::hypot(x_m - self.x0_m, y_m - self.y0_m);
        }
        let t = (((x_m - self.x0_m) * dx + (y_m - self.y0_m) * dy) / len2).clamp(0.0, 1.0);
        math::hypot(
            x_m - (self.x0_m + t * dx),
            y_m - (self.y0_m + t * dy),
        )
    }
}

/// One of the new attack families (07-threats-and-detection.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExtendedAttackKind {
    /// Transmit beacons for vehicles that do not exist, placed along lanes the attacker
    /// knows so that a map check passes.
    GhostVehicles,
    /// Retransmit a frame this node captured from another station, verbatim, later.
    ///
    /// The relay (wormhole) attack: the bytes are the original's, so the signature
    /// verifies and the receiver's only evidence is the claim's staleness and its
    /// inconsistency with where the sender has since been.
    RelayReplay,
    /// Flood the channel with maximum-size signed messages.
    OversizedFlood,
    /// Use a valid credential outside the region its certificate states.
    WrongRegionCert,
    /// Announce perceived objects that are not there.
    FakeCpm,
    /// Continuous noise in the channel.
    ConstantJamming,
    /// Noise only while energy is sensed on the channel.
    ReactiveJamming,
    /// Noise for a fraction of each period.
    RandomDutyJamming,
}

impl ExtendedAttackKind {
    /// Every new family, in declaration order.
    pub const ALL: [ExtendedAttackKind; 8] = [
        ExtendedAttackKind::GhostVehicles,
        ExtendedAttackKind::RelayReplay,
        ExtendedAttackKind::OversizedFlood,
        ExtendedAttackKind::WrongRegionCert,
        ExtendedAttackKind::FakeCpm,
        ExtendedAttackKind::ConstantJamming,
        ExtendedAttackKind::ReactiveJamming,
        ExtendedAttackKind::RandomDutyJamming,
    ];

    /// The name a scenario file and a foundry genome carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ExtendedAttackKind::GhostVehicles => "GhostVehicles",
            ExtendedAttackKind::RelayReplay => "RelayReplay",
            ExtendedAttackKind::OversizedFlood => "OversizedFlood",
            ExtendedAttackKind::WrongRegionCert => "WrongRegionCert",
            ExtendedAttackKind::FakeCpm => "FakeCpm",
            ExtendedAttackKind::ConstantJamming => "ConstantJamming",
            ExtendedAttackKind::ReactiveJamming => "ReactiveJamming",
            ExtendedAttackKind::RandomDutyJamming => "RandomDutyJamming",
        }
    }

    /// Parses a name. `None` for anything not in [`ExtendedAttackKind::ALL`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        ExtendedAttackKind::ALL.into_iter().find(|k| k.as_str() == name)
    }

    /// The behavioural family, on the same taxonomy the legacy feature pipeline uses.
    #[must_use]
    pub const fn family(self) -> AttackFamily {
        match self {
            ExtendedAttackKind::GhostVehicles => AttackFamily::Identity,
            ExtendedAttackKind::RelayReplay | ExtendedAttackKind::OversizedFlood => {
                AttackFamily::Timing
            }
            ExtendedAttackKind::WrongRegionCert => AttackFamily::Credential,
            ExtendedAttackKind::FakeCpm => AttackFamily::Event,
            ExtendedAttackKind::ConstantJamming
            | ExtendedAttackKind::ReactiveJamming
            | ExtendedAttackKind::RandomDutyJamming => AttackFamily::Jamming,
        }
    }

    /// The jammer profile this kind renders, if it is a jammer.
    #[must_use]
    pub const fn jam_profile(self) -> Option<JamProfile> {
        match self {
            ExtendedAttackKind::ConstantJamming => Some(JamProfile::Constant),
            ExtendedAttackKind::ReactiveJamming => Some(JamProfile::Reactive),
            ExtendedAttackKind::RandomDutyJamming => Some(JamProfile::RandomDuty),
            _ => None,
        }
    }

    /// Whether the kind needs [`crate::capability::RadioCaps::can_jam`].
    #[must_use]
    pub const fn needs_jam_capability(self) -> bool {
        self.jam_profile().is_some()
    }

    /// Whether the kind needs credentials the attacker holds for extra identities.
    #[must_use]
    pub const fn needs_extra_credentials(self) -> bool {
        matches!(self, ExtendedAttackKind::GhostVehicles)
    }
}

impl core::fmt::Display for ExtendedAttackKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything the new renderings read.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedAttackerParams {
    /// Which family this attacker runs.
    pub kind: ExtendedAttackKind,
    /// The generation interval, seconds (`PipelineConfig.dt`, 1.0).
    pub dt_s: f64,
    /// How many ghost vehicles to transmit for.
    ///
    /// F2MD `SybilVehNumber` = 5 (04-models.md §14).
    pub ghost_count: u32,
    /// The along-lane spacing between successive ghosts, metres.
    ///
    /// F2MD `SybilDistanceX` = 5 (04-models.md §14).
    pub ghost_spacing_m: f64,
    /// The lateral offset each ghost takes from the lane centre, metres, alternating side.
    ///
    /// F2MD `SybilDistanceY` = 2 (04-models.md §14).
    pub ghost_lateral_m: f64,
    /// How many messages back the relay replays.
    ///
    /// F2MD `ReplaySeqNum` = 6 (04-models.md §14).
    pub relay_lag_messages: usize,
    /// The payload size an oversized flood claims, bytes.
    ///
    /// The 802.11 maximum MSDU, 2 304 B (04-models.md §4.6): the largest SPDU the MAC
    /// will carry, so the largest a flooder can legally emit.
    pub oversized_payload_bytes: u32,
    /// Copies per interval an oversized flood puts on the air.
    ///
    /// F2MD `DosMultipleFreq` = 4 (04-models.md §14).
    pub flood_repetitions: u32,
    /// The region a misused certificate states.
    ///
    /// ISO 3166-1 numeric, as IEEE 1609.2 `IdentifiedRegion` carries it; 840 is the code
    /// for the United States. The attack is the *mismatch* with wherever the sender is, so
    /// the scenario sets whichever code is not the run's region.
    pub foreign_region: RegionId,
    /// How many phantom objects a false CPM carries.
    pub cpm_phantom_objects: u32,
    /// Jam transmit power, dBm.
    ///
    /// 16.75 dBm is the WARP jammer's power measured at 5.9 GHz (04-models.md §12.3,
    /// Puñal, Aguiar and Gross 2012). Clamped to the attacker's declared
    /// [`crate::capability::RadioCaps::max_power_dbm`].
    pub jam_power_dbm: f64,
    /// The RSSI a reactive jammer triggers on, dBm. −75 dBm (04-models.md §12.3).
    pub reactive_trigger_dbm: f64,
}

impl Default for ExtendedAttackerParams {
    fn default() -> Self {
        Self {
            kind: ExtendedAttackKind::GhostVehicles,
            dt_s: 1.0,
            ghost_count: 5,
            ghost_spacing_m: 5.0,
            ghost_lateral_m: 2.0,
            relay_lag_messages: 6,
            oversized_payload_bytes: MAX_MSDU_BYTES,
            flood_repetitions: 4,
            foreign_region: RegionId(840),
            cpm_phantom_objects: 5,
            jam_power_dbm: 16.75,
            reactive_trigger_dbm: -75.0,
        }
    }
}

impl ExtendedAttackerParams {
    /// The parameters for one family, everything else at its cited default.
    #[must_use]
    pub fn new(kind: ExtendedAttackKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }
}

/// The per-node state the new renderings carry between messages.
#[derive(Debug, Clone, Default, PartialEq)]
struct ExtState {
    /// `RelayReplay`: frames captured from other stations, oldest first.
    captured: Vec<ObservedMessage>,
    /// The newest generation time already captured per signer, so one reception is not
    /// captured twice when the view repeats it.
    captured_at: BTreeMap<[u8; 8], SimTime>,
    /// Whether anything was heard since the last act: the belief-side proxy for a busy
    /// channel that a reactive jammer triggers on.
    heard_recently: bool,
    /// How many times a declared-capability check refused an action.
    refused: u64,
}

/// The new attack families as one swappable attacker model.
#[derive(Debug, Clone)]
pub struct ExtendedAttacker {
    card: ModelCard,
    params: ExtendedAttackerParams,
    capabilities: Capabilities,
    schedule: AttackSchedule,
    node: NodeId,
    ghost_signers: Vec<[u8; 8]>,
    lanes: Vec<LaneHint>,
    state: ExtState,
}

impl ExtendedAttacker {
    /// An attacker at `node` running `params`.
    ///
    /// `ghost_signers` are extra credentials this node **holds** — the engine's credential
    /// store issues them, so an attacker declared [`crate::capability::CredentialAccess::None`]
    /// gets none and its ghost rendering refuses rather than inventing a key.
    /// `lanes` is the attacker's declared map knowledge; empty is a mapless attacker.
    #[must_use]
    pub fn new(
        node: NodeId,
        params: ExtendedAttackerParams,
        capabilities: Capabilities,
        schedule: AttackSchedule,
        ghost_signers: Vec<[u8; 8]>,
        lanes: Vec<LaneHint>,
    ) -> Self {
        let card = card(&params);
        Self {
            card,
            params,
            capabilities,
            schedule,
            node,
            ghost_signers,
            lanes,
            state: ExtState::default(),
        }
    }

    /// The family this model renders.
    #[must_use]
    pub fn kind(&self) -> ExtendedAttackKind {
        self.params.kind
    }

    /// The parameters it reads.
    #[must_use]
    pub fn params(&self) -> &ExtendedAttackerParams {
        &self.params
    }

    /// How many times an action was refused because the attacker had not declared the
    /// capability it needed.
    ///
    /// Exposed rather than silently counted: an attacker that renders nothing looks
    /// exactly like an attack that nobody detected, and that is how a scenario ends up
    /// reporting a recall for an attack that never happened.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.state.refused
    }

    /// How many captured frames the relay buffer holds.
    #[must_use]
    pub fn captured(&self) -> usize {
        self.state.captured.len()
    }

    /// The lane segment nearest `(x, y)`, or `None` for a mapless attacker.
    ///
    /// Ties go to the earlier hint, so the answer does not depend on iteration order.
    #[must_use]
    pub fn nearest_lane(&self, x_m: f64, y_m: f64) -> Option<&LaneHint> {
        let mut best: Option<(f64, &LaneHint)> = None;
        for h in &self.lanes {
            let d = h.distance_to_m(x_m, y_m);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, h));
            }
        }
        best.map(|(_, h)| h)
    }

    /// Where the `i`-th ghost claims to be, given the attacker's own claim.
    ///
    /// Along the nearest known lane's direction when the attacker declared a map, along
    /// its own claimed heading otherwise; `ghost_spacing_m` apart, alternating
    /// `ghost_lateral_m` either side of the centre so the ghosts look like two files of
    /// traffic rather than one point.
    fn ghost_pose(&self, i: usize, x_m: f64, y_m: f64, heading_rad: f64) -> (f64, f64) {
        let dir = self
            .nearest_lane(x_m, y_m)
            .map_or(heading_rad, LaneHint::heading_rad);
        let (s, c) = math::sin_cos(dir);
        let along = self.params.ghost_spacing_m * (i as f64 + 1.0);
        let lateral = if i % 2 == 0 {
            self.params.ghost_lateral_m
        } else {
            -self.params.ghost_lateral_m
        };
        (
            x_m + along * c - lateral * s,
            y_m + along * s + lateral * c,
        )
    }

    /// The jam burst this kind puts on the channel at `t`, or `None` when it is not
    /// jamming this interval.
    fn jam_burst(&mut self, ctx: &mut dyn ThreatCtx, profile: JamProfile) -> Option<RawEnergy> {
        if !self.capabilities.radio.can_jam {
            // I-T1's sibling: the engine enforces the declaration, and so does the model.
            self.state.refused += 1;
            return None;
        }
        let interval_us = (self.params.dt_s * 1e6).max(0.0) as u64;
        let (fire, duration_us, trigger) = match profile {
            JamProfile::Constant => (true, interval_us, None),
            JamProfile::Reactive => (
                // The proxy: something was heard, so the channel was busy. The real
                // trigger travels on the burst for the host to apply.
                self.state.heard_recently,
                interval_us,
                Some(self.params.reactive_trigger_dbm),
            ),
            JamProfile::RandomDuty => {
                let p = self.schedule.duty_cycle.clamp(0.0, 1.0);
                let mut rng = ctx.rng(RngDomain::Attack, EntityRef::Node(self.node));
                let on = rng.bool(p);
                drop(rng);
                let on_us = (interval_us as f64 * p) as u64;
                (on, on_us, None)
            }
        };
        if !fire {
            return None;
        }
        Some(RawEnergy {
            profile,
            power_dbm: self
                .params
                .jam_power_dbm
                .min(self.capabilities.radio.max_power_dbm),
            duration_us,
            trigger_dbm: trigger,
        })
    }
}

impl Model for ExtendedAttacker {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Attacker for ExtendedAttacker {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn schedule(&self) -> &AttackSchedule {
        &self.schedule
    }

    fn observe(&mut self, _ctx: &mut dyn ThreatCtx, view: &AttackerView<'_>) {
        self.state.heard_recently = !view.own_rx.is_empty();
        if self.params.kind != ExtendedAttackKind::RelayReplay {
            return;
        }
        // Capture what this node heard, for the relay. Deduplicated by
        // `(signer, claimed generation time)` because the view may show one reception to
        // two calls, and a relay that stored the same frame twice would replay it twice.
        for m in view.own_rx {
            if m.signer == [0; 8] {
                continue;
            }
            let last = self.state.captured_at.get(&m.signer).copied();
            if last.is_some_and(|l| l >= m.claimed_generation_time) {
                continue;
            }
            self.state
                .captured_at
                .insert(m.signer, m.claimed_generation_time);
            self.state.captured.push(m.clone());
        }
        let keep = self.params.relay_lag_messages + 1;
        if self.state.captured.len() > keep {
            let excess = self.state.captured.len() - keep;
            self.state.captured.drain(..excess);
        }
    }

    fn act(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        view: &AttackerView<'_>,
        out: &mut Emission,
    ) -> Vec<AttackAction> {
        let t = view.believed_time;
        if !self
            .schedule
            .active_at(t, view.own_belief.x_m, view.own_belief.y_m)
        {
            return Vec::new();
        }
        let mut actions = Vec::new();
        match self.params.kind {
            ExtendedAttackKind::GhostVehicles => {
                let available = self.ghost_signers.len();
                let n = (self.params.ghost_count as usize).min(available);
                if n == 0 {
                    self.state.refused += 1;
                }
                for i in 0..n {
                    let signer = self.ghost_signers[i];
                    let (gx, gy) = self.ghost_pose(i, out.x_m, out.y_m, out.heading_rad);
                    let dir = self
                        .nearest_lane(out.x_m, out.y_m)
                        .map_or(out.heading_rad, LaneHint::heading_rad);
                    let mut ghost = out.clone();
                    ghost.signer = signer;
                    ghost.x_m = gx;
                    ghost.y_m = gy;
                    ghost.heading_rad = wrap_heading(dir);
                    ghost.ghosts = Vec::new();
                    ghost.events = Vec::new();
                    ghost.perceived = Vec::new();
                    ghost.infra = Vec::new();
                    ghost.raw_energy = None;
                    out.ghosts.push(ghost);
                    actions.push(AttackAction::Ghost { signer });
                }
            }
            ExtendedAttackKind::RelayReplay => {
                let n = self.state.captured.len();
                if n >= self.params.relay_lag_messages {
                    let captured = self.state.captured[n - self.params.relay_lag_messages].clone();
                    let age_s = ns_to_secs(t.saturating_sub(captured.claimed_generation_time));
                    let mut relayed = out.clone();
                    // The captured frame goes back on the air verbatim: the attacker holds
                    // none of this signer's keys, so the bytes have to be the original's.
                    relayed.signer = captured.signer;
                    relayed.x_m = captured.claimed_x_m;
                    relayed.y_m = captured.claimed_y_m;
                    relayed.speed_mps = captured.claimed_speed_mps;
                    relayed.heading_rad = captured.claimed_heading_rad;
                    relayed.generation_time = captured.claimed_generation_time;
                    relayed.station_type = captured.station_type;
                    relayed.cert_valid_from = captured.cert_valid_from;
                    relayed.cert_valid_to = captured.cert_valid_to;
                    relayed.signature_valid = true;
                    relayed.replayed = true;
                    relayed.repetitions = 1;
                    relayed.ghosts = Vec::new();
                    relayed.events = Vec::new();
                    relayed.perceived = Vec::new();
                    relayed.infra = Vec::new();
                    relayed.raw_energy = None;
                    out.ghosts.push(relayed);
                    actions.push(AttackAction::Replay { age_s });
                } else {
                    // Nothing captured yet is not a silent no-op: it is a relay with an
                    // empty buffer, and the counter says so.
                    self.state.refused += 1;
                }
            }
            ExtendedAttackKind::OversizedFlood => {
                out.payload_bytes = Some(self.params.oversized_payload_bytes);
                out.repetitions = self.params.flood_repetitions;
                actions.push(AttackAction::FalsifyOutgoing {
                    fields: vec!["payload".to_string(), "repetitions".to_string()],
                    magnitude: f64::from(self.params.oversized_payload_bytes),
                });
            }
            ExtendedAttackKind::WrongRegionCert => {
                out.cert_region = Some(self.params.foreign_region);
                actions.push(AttackAction::FalsifyOutgoing {
                    fields: vec!["certificate".to_string()],
                    magnitude: f64::from(self.params.foreign_region.0),
                });
            }
            ExtendedAttackKind::FakeCpm => {
                let n = self.params.cpm_phantom_objects;
                for i in 0..n as usize {
                    let (ox, oy) = self.ghost_pose(i, out.x_m, out.y_m, out.heading_rad);
                    out.perceived.push(PerceivedObject::from_metres(
                        u16::try_from(i + 1).unwrap_or(u16::MAX),
                        ox,
                        oy,
                        out.speed_mps,
                        // The sender asserts its own perception quality, and an attacker
                        // asserts the maximum: TS 103 324 §7.1.8.6 tops out at 15.
                        15,
                    ));
                }
                if n > 0 {
                    actions.push(AttackAction::ForgeObject { count: n });
                }
            }
            ExtendedAttackKind::ConstantJamming
            | ExtendedAttackKind::ReactiveJamming
            | ExtendedAttackKind::RandomDutyJamming => {
                // `jam_profile` is `Some` for exactly these three arms.
                if let Some(profile) = self.params.kind.jam_profile()
                    && let Some(burst) = self.jam_burst(ctx, profile)
                {
                    actions.push(AttackAction::TransmitRaw {
                        profile: burst.profile.as_str().to_string(),
                        power_dbm: burst.power_dbm,
                    });
                    out.raw_energy = Some(burst);
                }
            }
        }
        actions
    }
}

/// The model card for an [`ExtendedAttacker`] rendering `params.kind`.
#[must_use]
pub fn card(params: &ExtendedAttackerParams) -> ModelCard {
    use serde_json::json;
    let f2md = design("04-models.md §14 (F2MD constants, veins-f2md F2MDParameters.h)");
    let jam = paper(
        "Puñal, Aguiar and Gross, In VANETs we trust? Characterizing RF jamming in \
         vehicular networks, ACM VANET 2012 (via 04-models.md §12.3)",
    );
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Attacker,
        "1.0.0",
        format!(
            "The attack families 07-threats-and-detection.md §2.2 adds to the ported \
             catalogue: renders the {} attack ({} family).",
            params.kind,
            params.kind.family().as_str()
        ),
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "ghost placement",
            "p_i = p_claimed + (i+1)·spacing·û(lane) ± lateral·n̂(lane); û from the nearest \
             declared lane hint, or from the attacker's own claimed heading with no map",
        ),
        Equation::new(
            "relay replay",
            "the frame captured `relay_lag_messages` receptions ago, retransmitted \
             verbatim: same signer, same claim, same generation time",
        ),
        Equation::new(
            "oversized flood",
            "payload = min(oversized_payload_bytes, MSDU cap) × flood_repetitions per \
             interval, bounded downstream by the node's own MAC and DCC",
        ),
        Equation::new(
            "jam burst",
            "power = min(jam_power_dbm, declared max); duration = interval for constant \
             and reactive, interval × duty for random-duty",
        ),
    ];
    card.parameters = vec![
        legacy_param(
            "dt_s",
            "s",
            json!(params.dt_s),
            LEGACY_PY,
            "PipelineConfig.dt",
        ),
        Parameter::new("ghost_count", "-", json!(params.ghost_count), f2md.clone()),
        Parameter::new(
            "ghost_spacing_m",
            "m",
            json!(params.ghost_spacing_m),
            f2md.clone(),
        ),
        Parameter::new(
            "ghost_lateral_m",
            "m",
            json!(params.ghost_lateral_m),
            f2md.clone(),
        ),
        Parameter::new(
            "relay_lag_messages",
            "-",
            json!(params.relay_lag_messages),
            f2md.clone(),
        ),
        Parameter::new(
            "oversized_payload_bytes",
            "B",
            json!(params.oversized_payload_bytes),
            design("04-models.md §4.6 (maximum MSDU 2 304 B)"),
        ),
        Parameter::new(
            "flood_repetitions",
            "msg/interval",
            json!(params.flood_repetitions),
            f2md,
        ),
        Parameter::new(
            "foreign_region",
            "-",
            json!(params.foreign_region.0),
            standard(
                "ISO 3166-1 numeric country code, as IEEE 1609.2 IdentifiedRegion and the \
                 ETSI authorization ticket's region restriction carry it",
            ),
        ),
        {
            let mut p = Parameter::new(
                "cpm_phantom_objects",
                "-",
                json!(params.cpm_phantom_objects),
                Source {
                    kind: SourceKind::TodoCalibrate,
                    reference: "no anchor for a phantom-object count".to_string(),
                    accessed: None,
                    note: Some(
                        "the default follows the F2MD Sybil vehicle count (5) for want of a \
                         CPM-specific number; ETSI TS 103 324 bounds a CPM's object list but \
                         says nothing about how many a plausible one carries"
                            .to_string(),
                    ),
                },
            );
            p.calibration = Some(
                "measure the perceived-object count a benign CPM carries at each scenario \
                 density with the perception model of 04-models.md §12.1, and set the \
                 phantom count inside that distribution so the attack is not separable by \
                 object count alone."
                    .to_string(),
            );
            p
        },
        Parameter::new(
            "jam_power_dbm",
            "dBm",
            json!(params.jam_power_dbm),
            jam.clone(),
        ),
        Parameter::new(
            "reactive_trigger_dbm",
            "dBm",
            json!(params.reactive_trigger_dbm),
            jam.clone(),
        ),
    ];
    card.sources = vec![
        design("07-threats-and-detection.md §2.2"),
        design("04-models.md §14 (F2MD attack magnitudes)"),
        design("04-models.md §12.3 (jammer profiles and anchors)"),
        jam,
        standard("IEEE 1609.2 §6.4.17 (IdentifiedRegion) and ETSI TS 103 097"),
        standard("ETSI TS 103 324 (collective perception, perceived-object quality)"),
        legacy(LEGACY_PY, "the broadcast pre-pass (the emission seam)"),
    ];
    card.assumptions = vec![
        "An attacker's body moves honestly; only its claims, its extra identities and its \
         energy are its own (07-threats-and-detection.md §1, 'Position')."
            .to_string(),
        "Map knowledge is the attacker's own declared lane hints, never a borrow of the \
         world (invariant I-T1)."
            .to_string(),
        "A relayed frame is retransmitted verbatim, because the attacker holds none of the \
         captured signer's keys; the host must not re-sign it."
            .to_string(),
        "Every burst is bounded by the attacker's declared radio envelope and by its own \
         MAC and DCC, exactly as an honest node's transmissions are."
            .to_string(),
    ];
    card.limitations = vec![
        "Jamming is declared, not applied. The physics is v2xw_radio::jamming's \
         (JammerProfile, JamWindow, JammingField; 04-models.md §12.3), which cites the \
         same Puñal anchors; this model supplies the adversary's capability, schedule and \
         burst and nothing else. Until the engine joins the two, a jamming run produces \
         the TransmitRaw ground truth and no packet loss — and if it wires both sides as \
         schedulers it will produce twice the jamming."
            .to_string(),
        "Reactive jamming triggers on 'something was heard last interval', because \
         AttackerView carries no CCA-busy or RSSI reading. The real −75 dBm trigger \
         travels on the burst for the host to apply."
            .to_string(),
        "`attacker/jammer/constant-pilot` (04-models.md §12.3) is not rendered here: \
         jamming individual OFDM pilot subcarriers has no representation above the PHY, \
         so if it is wanted it belongs with the other profiles in v2xw_radio::jamming."
            .to_string(),
        "The oversized flood declares a payload size; whether a receiver's reassembly \
         buffer or verification budget actually overflows is the node runtime's to model."
            .to_string(),
        "Certificate-region misuse needs the receiver to know its own region, which the \
         scenario declares; a receiver that does not leaves the check unevaluated rather \
         than passed."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Attack.as_str().to_string()],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![design("07-threats-and-detection.md §2.2")],
        tests: vec![
            "attacks_ext::ghosts_are_placed_along_the_lane_the_attacker_knows".to_string(),
            "attacks_ext::a_relay_retransmits_a_captured_frame_verbatim".to_string(),
            "attacks_ext::a_jammer_without_the_capability_refuses_visibly".to_string(),
        ],
    };
    card.cost = Some(v2xw_core::card::CostClass {
        per_call_us: None,
        notes: Some(
            "Not measured. The renderings are a handful of scalar edits per message; the \
             cost that matters is the signing and air time the host charges for the extra \
             frames."
                .to_string(),
        ),
    });
    card
}

/// How long a jam burst lasts, in nanoseconds — the unit the engine schedules in.
#[must_use]
pub fn burst_duration(burst: &RawEnergy) -> v2xw_core::time::Duration {
    v2xw_core::time::Duration::from_micros(burst.duration_us)
}

/// The instant a burst declared at `t` ends.
#[must_use]
pub fn burst_end(burst: &RawEnergy, t: SimTime) -> SimTime {
    t.saturating_add(secs_to_ns(burst.duration_us as f64 / 1e6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_new_family_parses_by_name_and_has_a_family() {
        assert_eq!(ExtendedAttackKind::ALL.len(), 8);
        for k in ExtendedAttackKind::ALL {
            assert_eq!(ExtendedAttackKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ExtendedAttackKind::parse("ConstPos"), None);
        assert_eq!(
            ExtendedAttackKind::ConstantJamming.family(),
            AttackFamily::Jamming
        );
        assert_eq!(
            ExtendedAttackKind::GhostVehicles.family(),
            AttackFamily::Identity
        );
        assert!(ExtendedAttackKind::ReactiveJamming.needs_jam_capability());
        assert!(!ExtendedAttackKind::FakeCpm.needs_jam_capability());
        assert!(ExtendedAttackKind::GhostVehicles.needs_extra_credentials());
    }

    #[test]
    fn a_lane_hint_answers_a_direction_and_a_distance() {
        let h = LaneHint::new(0.0, 0.0, 100.0, 0.0);
        assert!((h.heading_rad() - 0.0).abs() < 1e-12);
        assert!((h.distance_to_m(50.0, 3.0) - 3.0).abs() < 1e-12);
        // Beyond the end, the distance is to the end point.
        assert!((h.distance_to_m(110.0, 0.0) - 10.0).abs() < 1e-12);
        let degenerate = LaneHint::new(5.0, 5.0, 5.0, 5.0);
        assert!((degenerate.distance_to_m(5.0, 8.0) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn every_cards_parameter_is_cited_or_carries_a_plan() {
        for k in ExtendedAttackKind::ALL {
            let c = card(&ExtendedAttackerParams::new(k));
            c.validate().unwrap();
            for p in &c.parameters {
                let cited = p.source.kind != SourceKind::TodoCalibrate;
                let planned = p.calibration.as_ref().is_some_and(|s| !s.trim().is_empty());
                assert!(cited || planned, "{}", p.name);
            }
        }
    }

    #[test]
    fn a_burst_duration_round_trips_to_simulator_time() {
        let b = RawEnergy {
            profile: JamProfile::Constant,
            power_dbm: 16.75,
            duration_us: 1_000_000,
            trigger_dbm: None,
        };
        assert_eq!(burst_duration(&b).as_nanos(), 1_000_000_000);
        assert_eq!(burst_end(&b, 0), 1_000_000_000);
    }
}
