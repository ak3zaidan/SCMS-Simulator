//! What an attacker is allowed to see and do — [`Capabilities`] — and when it acts —
//! [`AttackSchedule`].
//!
//! 07-threats-and-detection.md §1 gives the table; this module is it in types. The point
//! of declaring capabilities rather than hard-coding them is that a result is only
//! meaningful against a stated adversary: "detection recall 0.94" says nothing until you
//! know whether the attacker held valid credentials, could jam, knew the map, or
//! coordinated with four others.
//!
//! The engine enforces the declaration (only declared credential handles are accepted,
//! the MAC rejects frames outside the declared radio envelope, the view carries only
//! declared knowledge). This crate's part is to *declare* honestly and to read nothing it
//! did not declare.

use v2xw_core::geom::Bbox;
use v2xw_core::ids::{HwProfileId, NodeId};
use v2xw_core::math;
use v2xw_core::time::{Duration, SimTime, ns_to_secs};

/// Which credentials an attacker holds (07-threats §1, "Credentials").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CredentialAccess {
    /// No valid credentials: every signature it produces fails verification.
    #[default]
    None,
    /// Its own `n` concurrent pseudonyms.
    ///
    /// The SCMS default is 20 per week and ETSI allows ≤ 100, or 20 under the C2C-CC
    /// profile (05-protocols.md §2.2). `n` concurrent certificates is the Sybil surface:
    /// TR 103 415 §8 notes it grows with the concurrent-pseudonym count.
    Own(u32),
    /// Credentials extracted from other devices, by digest.
    Stolen(Vec<[u8; 8]>),
    /// A compromised road-side unit's credentials.
    CompromisedRsu(NodeId),
}

impl CredentialAccess {
    /// How many distinct identities the attacker can sign as concurrently.
    #[must_use]
    pub fn concurrent_identities(&self) -> u32 {
        match self {
            CredentialAccess::None => 0,
            CredentialAccess::Own(n) => *n,
            CredentialAccess::Stolen(s) => u32::try_from(s.len()).unwrap_or(u32::MAX),
            CredentialAccess::CompromisedRsu(_) => 1,
        }
    }

    /// True when the attacker can produce a signature a receiver will verify.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !matches!(self, CredentialAccess::None)
    }
}

/// The attacker's radio envelope. The MAC/PHY reject frames outside it.
#[derive(Debug, Clone, PartialEq)]
pub struct RadioCaps {
    /// Maximum transmit power, dBm.
    pub max_power_dbm: f64,
    /// Whether the attacker may transmit raw energy (jam) rather than frames.
    pub can_jam: bool,
    /// The channel numbers it may use, ascending.
    pub channels: Vec<u16>,
}

impl Default for RadioCaps {
    /// A conforming ITS-G5 station: 23 dBm, no jamming, control channel only.
    ///
    /// 23 dBm is the ETSI EN 302 571 limit for the ITS-G5A band, which is the envelope an
    /// attacker using ordinary hardware has.
    fn default() -> Self {
        Self {
            max_power_dbm: 23.0,
            can_jam: false,
            channels: vec![180],
        }
    }
}

/// What the attacker knows. The view carries only what is declared here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Knowledge {
    /// The public certificate revocation list.
    pub crl: bool,
    /// A road map, so fabricated positions can be placed on lanes.
    pub map: bool,
    /// Neighbours learned from its own receptions. Always available in practice — a radio
    /// hears what it hears — and declared for completeness.
    pub neighbors_via_rx: bool,
    /// On-board sensing or perception.
    pub sensing: bool,
}

impl Knowledge {
    /// The baseline: it hears its own receptions and knows nothing else.
    #[must_use]
    pub const fn receptions_only() -> Self {
        Self {
            crl: false,
            map: false,
            neighbors_via_rx: true,
            sensing: false,
        }
    }
}

/// A coalition of coordinating attackers.
///
/// 07-threats §1 requires the coordination channel to be *modelled* — a V2X message or an
/// out-of-band link with latency — so a coordinated campaign pays for its coordination.
/// The id is what members share; the channel itself is the engine's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoalitionId(pub u32);

impl core::fmt::Display for CoalitionId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "c{}", self.0)
    }
}

/// The declaration the engine enforces (07-threats §1).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Capabilities {
    /// Which credentials it holds.
    pub credentials: CredentialAccess,
    /// Its radio envelope.
    pub radio: RadioCaps,
    /// What it knows.
    pub knowledge: Knowledge,
    /// Its coalition, if it coordinates.
    pub coordination: Option<CoalitionId>,
    /// Its hardware profile: signing and flooding rates are bounded by its own node
    /// runtime, so a flooding attacker on a weak HSM floods less.
    pub compute: Option<HwProfileId>,
}

impl Capabilities {
    /// The insider: valid credentials, conforming radio, map knowledge, no coalition.
    ///
    /// 07-threats §2.2, "Insider with valid credentials": the adversary most of the
    /// falsification catalog assumes, because a falsified field only reaches a receiver's
    /// plausibility checks if the signature verified first.
    #[must_use]
    pub fn insider(pseudonyms: u32) -> Self {
        Self {
            credentials: CredentialAccess::Own(pseudonyms),
            radio: RadioCaps::default(),
            knowledge: Knowledge {
                map: true,
                ..Knowledge::receptions_only()
            },
            coordination: None,
            compute: None,
        }
    }

    /// The outsider: no valid credentials, conforming radio, receptions only.
    #[must_use]
    pub fn outsider() -> Self {
        Self {
            credentials: CredentialAccess::None,
            radio: RadioCaps::default(),
            knowledge: Knowledge::receptions_only(),
            coordination: None,
            compute: None,
        }
    }

    /// The same capabilities, in a coalition.
    #[must_use]
    pub fn in_coalition(mut self, id: CoalitionId) -> Self {
        self.coordination = Some(id);
        self
    }

    /// The same capabilities, able to jam.
    #[must_use]
    pub fn jamming(mut self) -> Self {
        self.radio.can_jam = true;
        self
    }

    /// The same capabilities, watching the public CRL.
    #[must_use]
    pub fn crl_aware(mut self) -> Self {
        self.knowledge.crl = true;
        self
    }
}

/// When an attacker acts: duty cycle, onset jitter, geofence, wave membership
/// (07-threats §1, from the legacy scenario events).
///
/// Every field has a legacy counterpart in `PipelineConfig`, and the defaults reproduce
/// the legacy defaults so a ported scenario behaves as it did.
#[derive(Debug, Clone, PartialEq)]
pub struct AttackSchedule {
    /// The instant the attacker becomes eligible to act (`attack_start`, default 5 s).
    pub from: SimTime,
    /// The instant it stops (`attack_end`, default 60 s).
    pub to: SimTime,
    /// Fraction of each pulse period spent acting (`attack_duty_cycle`, default 1.0).
    ///
    /// Below 1 the attacker falsifies in bursts, which starves the authority's
    /// sustained-evidence gate — the legacy evasion knob.
    pub duty_cycle: f64,
    /// One on/off cycle (`attack_pulse_period_s`, default 20 s).
    pub pulse_period: Duration,
    /// Phase offset within the pulse period, in `[0, 1)`, so a coalition's members do not
    /// pulse in lockstep unless they mean to (`Vehicle.pulse_phase`).
    pub pulse_phase: f64,
    /// A box outside which the attacker stays honest, in world-local ENU metres
    /// (`attack_zone_ok`). `None` means everywhere.
    pub geofence: Option<Bbox>,
    /// Which attack wave this attacker belongs to, for a scenario that runs several.
    pub wave: Option<u32>,
    /// How long it stays dormant after seeing a new revocation on the public CRL
    /// (`crl_dormant_s`, default 45 s). Only consulted when the attacker declared
    /// [`Knowledge::crl`].
    pub crl_dormant: Duration,
}

impl Default for AttackSchedule {
    fn default() -> Self {
        Self {
            from: 5 * v2xw_core::time::NS_PER_S,
            to: 60 * v2xw_core::time::NS_PER_S,
            duty_cycle: 1.0,
            pulse_period: Duration::from_secs(20),
            pulse_phase: 0.0,
            geofence: None,
            wave: None,
            crl_dormant: Duration::from_secs(45),
        }
    }
}

impl AttackSchedule {
    /// Whether the attacker acts at `t`, given where it believes it is.
    ///
    /// The legacy rule, in order: inside the window, inside the geofence, and inside the
    /// "on" part of the pulse. Dormancy after a CRL observation is attacker state rather
    /// than schedule state, so it is applied by the attacker
    /// ([`crate::attack::LegacyAttacker`]), not here.
    #[must_use]
    pub fn active_at(&self, t: SimTime, x_m: f64, y_m: f64) -> bool {
        if t < self.from || t > self.to {
            return false;
        }
        if let Some(b) = &self.geofence
            && !bbox_contains_xy(b, x_m, y_m)
        {
            return false;
        }
        if self.duty_cycle >= 1.0 {
            return true;
        }
        let period = ns_to_secs(self.pulse_period.as_nanos()).max(1e-9);
        let elapsed = ns_to_secs(t.saturating_sub(self.from));
        let phase = (elapsed / period + self.pulse_phase).fract();
        phase < self.duty_cycle
    }
}

/// Whether a bounding box contains a point, ignoring altitude.
///
/// [`Bbox`] is three-dimensional and an attacker's geofence is a map footprint, so the
/// vertical extent is deliberately not consulted: a geofence that happened to be built
/// with a zero-height box would otherwise exclude every actor whose `z` is not exactly
/// zero, which is a silent "never active".
fn bbox_contains_xy(b: &Bbox, x_m: f64, y_m: f64) -> bool {
    x_m >= b.min.x && x_m <= b.max.x && y_m >= b.min.y && y_m <= b.max.y
}

/// The smallest angle between two headings, radians, in `[0, π]`.
///
/// The port of the legacy `_ang_diff` (`run.py :: _ang_diff`), which worked in degrees.
#[must_use]
pub fn angle_diff_rad(a: f64, b: f64) -> f64 {
    let two_pi = core::f64::consts::TAU;
    let mut d = (a - b) % two_pi;
    if d < 0.0 {
        d += two_pi;
    }
    if d > core::f64::consts::PI {
        d = two_pi - d;
    }
    d
}

/// Wraps a heading into `[0, 2π)`.
#[must_use]
pub fn wrap_heading(a: f64) -> f64 {
    let two_pi = core::f64::consts::TAU;
    let d = a % two_pi;
    if d < 0.0 { d + two_pi } else { d }
}

/// The bearing from `(x0, y0)` to `(x1, y1)`, ENU radians, `0 = east`.
#[must_use]
pub fn bearing_rad(x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
    wrap_heading(math::atan2(y1 - y0, x1 - x0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::geom::Vec3;
    use v2xw_core::time::NS_PER_S;

    #[test]
    fn the_window_bounds_activity() {
        let s = AttackSchedule::default();
        assert!(!s.active_at(4 * NS_PER_S, 0.0, 0.0));
        assert!(s.active_at(5 * NS_PER_S, 0.0, 0.0));
        assert!(s.active_at(60 * NS_PER_S, 0.0, 0.0));
        assert!(!s.active_at(61 * NS_PER_S, 0.0, 0.0));
    }

    #[test]
    fn a_duty_cycle_pulses_on_and_off() {
        let s = AttackSchedule {
            from: 0,
            to: 100 * NS_PER_S,
            duty_cycle: 0.25,
            pulse_period: Duration::from_secs(20),
            ..AttackSchedule::default()
        };
        // 0..5 s on, 5..20 s off, 20..25 s on again.
        assert!(s.active_at(0, 0.0, 0.0));
        assert!(s.active_at(4 * NS_PER_S, 0.0, 0.0));
        assert!(!s.active_at(6 * NS_PER_S, 0.0, 0.0));
        assert!(!s.active_at(19 * NS_PER_S, 0.0, 0.0));
        assert!(s.active_at(21 * NS_PER_S, 0.0, 0.0));
    }

    #[test]
    fn a_geofence_ignores_altitude() {
        let b = Bbox {
            min: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            max: Vec3 {
                x: 100.0,
                y: 100.0,
                z: 0.0,
            },
        };
        let s = AttackSchedule {
            from: 0,
            to: 100 * NS_PER_S,
            geofence: Some(b),
            ..AttackSchedule::default()
        };
        assert!(s.active_at(NS_PER_S, 50.0, 50.0));
        assert!(!s.active_at(NS_PER_S, 150.0, 50.0));
    }

    #[test]
    fn credential_access_counts_concurrent_identities() {
        assert_eq!(CredentialAccess::None.concurrent_identities(), 0);
        assert_eq!(CredentialAccess::Own(20).concurrent_identities(), 20);
        assert_eq!(
            CredentialAccess::Stolen(vec![[0; 8], [1; 8]]).concurrent_identities(),
            2
        );
        assert!(!CredentialAccess::None.is_valid());
        assert!(CredentialAccess::Own(1).is_valid());
    }

    #[test]
    fn angle_helpers_agree_with_the_legacy_degree_forms() {
        let d = |deg: f64| deg.to_radians();
        assert!((angle_diff_rad(d(10.0), d(350.0)) - d(20.0)).abs() < 1e-12);
        assert!((angle_diff_rad(d(0.0), d(180.0)) - d(180.0)).abs() < 1e-12);
        assert!((wrap_heading(d(-90.0)) - d(270.0)).abs() < 1e-12);
        assert!((bearing_rad(0.0, 0.0, 0.0, 1.0) - d(90.0)).abs() < 1e-12);
    }
}
