//! The pseudonym-change strategy, and the store that swaps.
//!
//! 03-interfaces.md §7 puts "`SignerIdPolicy` schedule; pseudonym change strategy hook" on
//! `CredentialProtocol::signing_policy`, which makes the *rule* this crate's business and
//! the *enforcement* the node's: `v2xw_node::stores::CertStore` holds handles and rotates
//! within what it holds, and this module decides when a rotation is due and which
//! credential of the provisioned pool becomes active.
//!
//! The four strategies are exactly the four names a scenario may write in
//! `security.pseudonym_change.strategy` — `time`, `distance`, `mix-zone`, `silent` — so a
//! scenario cannot ask for a rule this module cannot express, and
//! [`PseudonymStrategy::from_scenario`] is the one place the mapping lives.
//!
//! **Determinism.** No float reaches a decision. The scenario states its distance in
//! metres, which [`PseudonymStrategy::from_scenario`] converts once, at the boundary, into
//! integer centimetres; every comparison afterwards is integer. No clock is read: the age
//! of the active pseudonym is a difference of two [`SimTime`]s the caller supplies.

use v2xw_core::ctx::{Record, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};

/// Centimetres in a metre, so the one float-to-integer conversion in this module is named.
const CM_PER_M: f64 = 100.0;

/// The SAE J2945/1 `CERTCHG` interval, five minutes.
///
/// [USDOT SCMS Technical Primer (FHWA-JPO-19-775) pp.7-8; SAE J2945/1 `CERTCHG`; restated
/// in 05-protocols.md §2.4.]
pub const CERTCHG_INTERVAL: Duration = Duration::from_secs(300);

/// The NYC-pilot distance rule, two kilometres, in centimetres.
///
/// [USDOT SCMS Technical Primer pp.7-8.]
pub const CERTCHG_DISTANCE_CM: u64 = 200_000;

/// When a node changes pseudonym.
///
/// One variant per scenario spelling. The two numbers each variant carries are the two
/// cited defaults of 05-protocols.md §2.4, and a scenario overrides them by writing
/// `period_s` or `distance_m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PseudonymStrategy {
    /// `time` — change when the active pseudonym reaches `period` of age.
    ///
    /// This is the reading the Phase 1 scenario writes, and the one a privacy study that
    /// wants a fixed change rate needs: it does not depend on how far the vehicle drove,
    /// so a car stopped at a light still rotates.
    Time {
        /// The age at which a change is due.
        period: Duration,
    },
    /// `distance` — change after `distance_cm` centimetres of travel since the last change.
    Distance {
        /// The distance at which a change is due, centimetres.
        distance_cm: u64,
    },
    /// `mix-zone` — change on leaving a mix zone, and never otherwise.
    ///
    /// The zone geometry is the world's and the "am I in one" answer is the node's, so
    /// this variant carries no geometry: the caller passes
    /// [`ChangeTrigger::left_mix_zone`] and this module decides nothing else.
    MixZone,
    /// `silent` — no scheduled change at all.
    ///
    /// A change still happens when one is *forced* — the active credential expired, or the
    /// CRL revoked it — because a node with a revoked pseudonym that kept signing with it
    /// would be modelling a device that ignores its own CRL. This variant is the control
    /// arm of a privacy experiment, not a device configuration.
    Silent,
}

impl Default for PseudonymStrategy {
    /// The J2945/1 reading, five minutes, which is the scenario schema's own default.
    fn default() -> PseudonymStrategy {
        PseudonymStrategy::Time {
            period: CERTCHG_INTERVAL,
        }
    }
}

impl PseudonymStrategy {
    /// The strategy a scenario's `security.pseudonym_change` block names.
    ///
    /// `strategy` is the scenario's string, and the two options are its `period_s` and
    /// `distance_m`. An unknown name yields `None` rather than a default, so the engine's
    /// validator can name the field; a known name with the option it needs missing falls
    /// back to that rule's cited default, which is what the schema's own defaults already
    /// are.
    pub fn from_scenario(
        strategy: &str,
        period_s: Option<f64>,
        distance_m: Option<f64>,
    ) -> Option<PseudonymStrategy> {
        match strategy {
            "time" => Some(PseudonymStrategy::Time {
                period: period_s.map_or(CERTCHG_INTERVAL, Duration::from_secs_f64),
            }),
            "distance" => Some(PseudonymStrategy::Distance {
                distance_cm: distance_m.map_or(CERTCHG_DISTANCE_CM, |m| {
                    if m.is_finite() && m > 0.0 {
                        (m * CM_PER_M) as u64
                    } else {
                        CERTCHG_DISTANCE_CM
                    }
                }),
            }),
            "mix-zone" => Some(PseudonymStrategy::MixZone),
            "silent" => Some(PseudonymStrategy::Silent),
            _ => None,
        }
    }

    /// The strategy's stable name, as a scenario spells it and a manifest records it.
    pub const fn as_str(self) -> &'static str {
        match self {
            PseudonymStrategy::Time { .. } => "time",
            PseudonymStrategy::Distance { .. } => "distance",
            PseudonymStrategy::MixZone => "mix-zone",
            PseudonymStrategy::Silent => "silent",
        }
    }

    /// Whether a scheduled change is due.
    ///
    /// Only the scheduled half: a forced change (expiry, revocation, no active pseudonym
    /// at all) is decided by [`PseudonymStore::rotate`], because it does not depend on the
    /// strategy and a `silent` node must still make it.
    pub const fn is_due(self, t: ChangeTrigger) -> bool {
        match self {
            PseudonymStrategy::Time { period } => t.age.as_nanos() >= period.as_nanos(),
            PseudonymStrategy::Distance { distance_cm } => t.travelled_cm >= distance_cm,
            PseudonymStrategy::MixZone => t.left_mix_zone,
            PseudonymStrategy::Silent => false,
        }
    }
}

/// What the node knows when it asks whether a change is due.
///
/// A struct rather than three arguments so that adding a trigger cannot silently reorder
/// two `u64`s at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangeTrigger {
    /// How long the active pseudonym has been active.
    pub age: Duration,
    /// How far the vehicle has travelled since the last change, centimetres.
    pub travelled_cm: u64,
    /// Whether the vehicle has just left a mix zone.
    pub left_mix_zone: bool,
}

/// Why a pseudonym change happened.
///
/// The same vocabulary as `v2xw_node::stores::ChangeReason`, because the two must agree:
/// this crate decides, the node's store enforces, and a reason that existed here and not
/// there would be a change the node could not explain.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ChangeReason {
    /// The node has just been provisioned and had no active pseudonym.
    Startup,
    /// The strategy's schedule fired.
    Scheduled,
    /// The active pseudonym left its validity window.
    Expired,
    /// The active pseudonym is on the CRL.
    Revoked,
}

impl ChangeReason {
    /// The reason's stable name, as `sec.cert` records it.
    pub const fn as_str(self) -> &'static str {
        match self {
            ChangeReason::Startup => "startup",
            ChangeReason::Scheduled => "scheduled",
            ChangeReason::Expired => "expired",
            ChangeReason::Revoked => "revoked",
        }
    }
}

/// A `sec.cert` record: what happened to one node's credentials, and to which one.
///
/// 03-interfaces.md §14 gives the channel's key fields as `t, node, event (change, expire,
/// top-up, learn), digest`. The `(i, j)` pair is carried alongside the digest because a
/// pseudonym's identity in this protocol *is* its `(i, j)` position in the butterfly
/// expansion, and a reader correlating a change with a provisioning batch needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CertEvent {
    /// When it happened.
    pub t: SimTime,
    /// Whose credential.
    pub node: NodeId,
    /// What happened: `change`, `top-up`, `expire` or `revoke`.
    pub event: &'static str,
    /// Why, for a change.
    pub reason: Option<ChangeReason>,
    /// The i-period of the credential that became active.
    pub i_period: u32,
    /// Its index within the period.
    pub j_index: u32,
    /// Its linkage value, which is the only identifier a receiver of a message signed with
    /// it could ever see.
    pub linkage_value: [u8; 9],
    /// How many changes this node has made, including this one.
    pub changes: u32,
}

impl Record for CertEvent {
    const CHANNEL: &'static str = "sec.cert";
    const VISIBILITY: Visibility = Visibility::Node;
}

/// The selection half of a device's certificate store.
///
/// The pool itself is `run::DeviceState::credentials` — the credentials the provisioning
/// flow actually downloaded — and this struct holds which one of them is active and the
/// counters the strategy reads. Keeping the two apart is deliberate: a bug that made the
/// store forget a credential and a bug that made it stop rotating are different bugs, and
/// a single struct holding both would let the second hide inside the first.
#[derive(Debug, Clone, Default)]
pub struct PseudonymStore {
    strategy: PseudonymStrategy,
    active: Option<(u32, u32)>,
    high_water: Option<(u32, u32)>,
    last_change: Option<SimTime>,
    travelled_cm: u64,
    left_mix_zone: bool,
    changes: u32,
    wraps: u32,
}

impl PseudonymStore {
    /// A store under `strategy` with nothing active yet.
    pub fn new(strategy: PseudonymStrategy) -> PseudonymStore {
        PseudonymStore {
            strategy,
            ..PseudonymStore::default()
        }
    }

    /// The strategy this store enforces.
    pub const fn strategy(&self) -> PseudonymStrategy {
        self.strategy
    }

    /// Replaces the strategy, which is how a scenario timeline item changes it mid-run.
    pub fn set_strategy(&mut self, strategy: PseudonymStrategy) {
        self.strategy = strategy;
    }

    /// The `(i, j)` of the active pseudonym, if the device has one.
    pub const fn active(&self) -> Option<(u32, u32)> {
        self.active
    }

    /// How many changes have happened.
    pub const fn changes(&self) -> u32 {
        self.changes
    }

    /// When the last change happened.
    pub const fn last_change(&self) -> Option<SimTime> {
        self.last_change
    }

    /// Adds travel since the last change.
    ///
    /// Centimetres, from the caller's own quantisation of its own metres: the engine
    /// writes positions on a 1 mm grid (build decision D9), so a centimetre of resolution
    /// here is below what the distance rule can distinguish and keeps the comparison
    /// integer.
    pub fn travelled_cm(&mut self, cm: u64) {
        self.travelled_cm = self.travelled_cm.saturating_add(cm);
    }

    /// Records that the vehicle has left a mix zone, for the `mix-zone` strategy.
    pub fn left_mix_zone(&mut self) {
        self.left_mix_zone = true;
    }

    /// What the strategy will be asked at `now`.
    pub const fn trigger(&self, now: SimTime) -> ChangeTrigger {
        let since = match self.last_change {
            Some(t) if now >= t => Duration::between(t, now),
            _ => Duration::ZERO,
        };
        ChangeTrigger {
            age: since,
            travelled_cm: self.travelled_cm,
            left_mix_zone: self.left_mix_zone,
        }
    }

    /// Chooses the pseudonym that becomes active at `now`, or keeps the current one.
    ///
    /// `usable` is the pool, as `(i, j)` pairs the device holds that are inside their
    /// validity window at `now` and not revoked by the CRL it has processed, in ascending
    /// `(i, j)` order. `active_revoked` says whether the credential that is stepping down
    /// is stepping down because the CRL revoked it. Returns the reason a change happened together with the pair that is
    /// now active; `None` means the active pseudonym stays.
    ///
    /// Selection **drains the pool in issue order**: the lowest usable `(i, j)` strictly
    /// above the highest one activated so far, and only when none is left does it wrap to
    /// the lowest usable and count a wrap.
    ///
    /// Not "the lowest usable that is not the current one". That rule looks equivalent and
    /// is not: with a pool of twenty it alternates between the first two certificates for
    /// ever, so a vehicle would carry two pseudonyms across a whole week and an
    /// unlinkability study measuring twenty would be measuring two. The wrap count is
    /// reported rather than hidden because a run that wrapped is a run whose pool ran out
    /// before its top-up arrived, which is a finding and not an implementation detail.
    pub fn rotate(
        &mut self,
        now: SimTime,
        usable: &[(u32, u32)],
        active_revoked: bool,
    ) -> Option<(ChangeReason, Option<(u32, u32)>)> {
        let reason = self.change_due(now, usable, active_revoked)?;
        let next = match self.high_water {
            Some(hw) => usable.iter().copied().find(|&p| p > hw),
            None => usable.first().copied(),
        };
        let next = match next {
            Some(p) => Some(p),
            None => {
                let first = usable.first().copied();
                if first.is_some() {
                    self.wraps = self.wraps.saturating_add(1);
                }
                first
            }
        };
        self.active = next;
        if next.is_some() {
            // Either the next one up, or the lowest after a wrap: in both cases the
            // cursor becomes the pseudonym just activated.
            self.high_water = next;
        }
        self.last_change = Some(now);
        self.travelled_cm = 0;
        self.left_mix_zone = false;
        self.changes = self.changes.saturating_add(1);
        Some((reason, next))
    }

    /// How many times the pool has been exhausted and reused from the start.
    pub const fn wraps(&self) -> u32 {
        self.wraps
    }

    /// The highest `(i, j)` this device has activated.
    pub const fn high_water(&self) -> Option<(u32, u32)> {
        self.high_water
    }

    /// Marks the active pseudonym unusable, so the next [`PseudonymStore::rotate`] is
    /// forced. Returns whether anything was active to invalidate.
    pub fn invalidate_active(&mut self) -> bool {
        self.active.take().is_some()
    }

    fn change_due(
        &self,
        now: SimTime,
        usable: &[(u32, u32)],
        active_revoked: bool,
    ) -> Option<ChangeReason> {
        match self.active {
            None if usable.is_empty() => None,
            None => Some(ChangeReason::Startup),
            Some(active) if !usable.contains(&active) => {
                // The active pseudonym is no longer in the usable set, so the device must
                // stop using it whatever the strategy says — including `silent`. The
                // caller says which of the two reasons it is, because only the caller
                // holds the CRL: a credential that is merely out of its window and one
                // that a Linkage Authority's released seed matched are the same absence
                // here and very different events in a revocation study.
                Some(if active_revoked {
                    ChangeReason::Revoked
                } else {
                    ChangeReason::Expired
                })
            }
            Some(_) => {
                if usable.len() < 2 {
                    // Nothing to rotate *to*. Reporting a change here would count a
                    // change that did not happen, which is the number a privacy study
                    // reads.
                    return None;
                }
                self.strategy
                    .is_due(self.trigger(now))
                    .then_some(ChangeReason::Scheduled)
            }
        }
    }
}
