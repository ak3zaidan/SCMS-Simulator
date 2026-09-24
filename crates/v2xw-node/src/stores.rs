//! The node's stores (06-node-models.md §2.2): what it holds, what that costs it in
//! bytes, and what it throws away.
//!
//! Four of them have a cryptographic counterpart in `v2xw-sec` — [`v2xw_sec::CrlStore`],
//! [`v2xw_sec::PeerCertCache`], [`v2xw_sec::TrustStore`] and the certificate types — and
//! this module does not reimplement any of them. What it adds is the part that is a
//! *node* property rather than a cryptographic one: a capacity, an ageing rule, a byte
//! count, and, in one case, a bound on how much work an unauthenticated field may cause.
//!
//! # The bound, and why it is here rather than only in `v2xw-sec`
//!
//! A linkage CRL entry revokes a device from one i-period forward. Checking a certificate
//! against one means walking a hash chain from the entry's period to the certificate's,
//! then two AES blocks per candidate index. `v2xw-sec` already fixed the inner half of
//! this: [`v2xw_sec::linkage::CrlLinkageEntry::matches_any_index`] walks the chain once
//! per entry instead of once per index, and refuses a period more than
//! [`v2xw_sec::linkage::DEFAULT_MAX_FORWARD_PERIODS`] ahead of the entry.
//!
//! That leaves a second multiplier, and it belongs to the node. `iCert` is a field of a
//! certificate whose signature has not been checked yet, so it is attacker-controlled;
//! 156 periods forward, against a CRL with thousands of entries, is still a lot of hashing
//! to buy with one forged header. A node knows something `v2xw-sec` does not: which
//! i-period it is currently in. A certificate presented now legitimately carries the
//! current period, or the previous one during the one-hour overlap that CAMP's 10,140-
//! minute lifetime gives a 10,080-minute period — so anything outside a three-period
//! window around now is implausible on its face and is refused **before** the store is
//! touched at all. [`CrlGate`] is that check, it counts the work it does so a test can
//! see the bound holding, and [`CrlGate::check`] refuses to be called without saying
//! whether the certificate has been authenticated yet.

use std::collections::BTreeMap;

use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};
use v2xw_msg::sec_types::HashedId8;
use v2xw_msg::sec_types::ieee1609_dot2_base_types::HashedId10;
use v2xw_sec::linkage::{CrlLinkageEntry, LinkageValue};

use crate::profile::StorageModel;

// ---------------------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------------------

/// Where a credential is in its lifecycle (05-protocols.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredState {
    /// Downloaded and inside its validity window.
    Active,
    /// Downloaded but not yet inside its validity window.
    Preloaded,
    /// Past its validity window.
    Expired,
    /// On a CRL.
    Revoked,
}

/// One credential this node holds: the [`v2xw_core::nodeview::NodeView::Credential`] of
/// invariant I-C2.
///
/// A handle, not key material: it names a certificate and the key id the crypto backend
/// signs with, so a plug-in given the view can see *which* pseudonym is active without
/// being handed a private key.
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialHandle {
    /// The certificate's `HashedId8`.
    pub digest: HashedId8,
    /// The encoded certificate, for attaching to a message.
    pub cert_coer: Vec<u8>,
    /// The key the crypto backend signs with.
    pub key: v2xw_sec::KeyId,
    /// The i-period this pseudonym belongs to.
    pub i_period: u32,
    /// The index within the period (`j` of the linkage construction).
    pub j_index: u32,
    /// Valid from, on the node's own clock.
    pub valid_from: SimTime,
    /// Valid until.
    pub valid_until: SimTime,
    /// Lifecycle state.
    pub state: CredState,
}

impl CredentialHandle {
    /// Whether the credential is usable at `t`.
    pub fn is_valid_at(&self, t: SimTime) -> bool {
        self.state != CredState::Revoked && t >= self.valid_from && t < self.valid_until
    }
}

/// Why a node changed pseudonym (05-protocols.md §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeReason {
    /// The J2945/1 `CERTCHG` age rule fired.
    Age,
    /// The distance rule fired.
    Distance,
    /// The active credential left its validity window.
    Expired,
    /// The active credential was revoked.
    Revoked,
    /// The node has just started and has no active credential.
    Startup,
}

/// The pseudonym-change rule (05-protocols.md §2.4, SCMS/J2945-1 row).
///
/// > every 5 min unless < 2 km since last change (J2945/1 `CERTCHG`); NYC pilot: 2 km or
/// > 5 min, whichever first — [PRIMER pp.7-8]; [BRECHT §II-A]
///
/// The two readings differ and the document records both, so both are expressible: with
/// [`RotationPolicy::require_both`] set, five minutes *and* two kilometres must have
/// passed, which is the `CERTCHG` reading; cleared, either suffices, which is the NYC
/// pilot's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotationPolicy {
    /// Minimum age of the active pseudonym before a change.
    pub min_age: Duration,
    /// Minimum distance travelled since the last change, metres.
    pub min_distance_m: f64,
    /// Whether both conditions must hold, or either.
    pub require_both: bool,
}

impl RotationPolicy {
    /// The J2945/1 `CERTCHG` reading: five minutes *and* two kilometres.
    pub const J2945_1: RotationPolicy = RotationPolicy {
        min_age: Duration::from_secs(300),
        min_distance_m: 2000.0,
        require_both: true,
    };

    /// The NYC pilot reading: two kilometres or five minutes, whichever comes first.
    pub const NYC_PILOT: RotationPolicy = RotationPolicy {
        min_age: Duration::from_secs(300),
        min_distance_m: 2000.0,
        require_both: false,
    };

    /// Whether a change is due.
    pub fn is_due(&self, age: Duration, distance_m: f64) -> bool {
        let old_enough = age >= self.min_age;
        let far_enough = distance_m >= self.min_distance_m;
        if self.require_both {
            old_enough && far_enough
        } else {
            old_enough || far_enough
        }
    }
}

/// The node's own credentials, with the currently selected pseudonym first.
///
/// The ordering is the convention
/// [`v2xw_core::nodeview::NodeView::active_credential`] documents: the store keeps the
/// selected pseudonym at index zero and a change reorders it, so a plug-in reading the
/// view gets the right answer from the default accessor.
#[derive(Debug, Clone, Default)]
pub struct CertStore {
    creds: Vec<CredentialHandle>,
    last_change: Option<SimTime>,
    distance_since_change_m: f64,
    changes: u32,
    policy: Option<RotationPolicy>,
    /// How many times each credential, keyed by `(i_period, j_index, valid_from)`, has been
    /// the active one. Rotation picks the least used, so a pool is used round-robin.
    activations: BTreeMap<(u32, u32, SimTime), u32>,
    /// Why the last change happened.
    last_reason: Option<ChangeReason>,
}

impl CertStore {
    /// An empty store with no rotation policy, which never changes pseudonym on its own.
    pub fn new() -> Self {
        CertStore::default()
    }

    /// The same store with a rotation policy.
    #[must_use]
    pub fn with_policy(mut self, policy: RotationPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Adds a credential, keeping the active one first.
    pub fn insert(&mut self, cred: CredentialHandle) {
        self.creds.push(cred);
    }

    /// Every credential, active one first.
    pub fn credentials(&self) -> &[CredentialHandle] {
        &self.creds
    }

    /// Every credential, mutably. See [`CertStore::active_mut`] for the one caller and
    /// the one field pair it is allowed to write.
    pub fn credentials_mut(&mut self) -> &mut [CredentialHandle] {
        &mut self.creds
    }

    /// The credential a message signed now would use.
    pub fn active(&self) -> Option<&CredentialHandle> {
        self.creds.first().filter(|c| c.state == CredState::Active)
    }

    /// The active credential, mutably.
    ///
    /// Exists for one caller: [`crate::secure::NodeSecurity`] provisions the real key and
    /// the real certificate for a pseudonym and then writes the certificate's *own*
    /// `HashedId8` and encoding back here. Until it does, a credential's `digest` is a
    /// stand-in ([`pseudo_signer`]) and its `cert_coer` is a zero fill of the modelled
    /// length — which is what made every SPDU the same size and is the defect this
    /// accessor exists to let the node repair. Nothing else should write to a credential:
    /// its validity window and its i-period belong to the credential protocol.
    pub fn active_mut(&mut self) -> Option<&mut CredentialHandle> {
        self.creds
            .first_mut()
            .filter(|c| c.state == CredState::Active)
    }

    /// How many credentials are inside their validity window at `t`.
    pub fn active_count(&self, t: SimTime) -> usize {
        self.creds.iter().filter(|c| c.is_valid_at(t)).count()
    }

    /// How many credentials are stored at all.
    pub fn stored_count(&self) -> usize {
        self.creds.len()
    }

    /// How many pseudonym changes have happened.
    pub fn changes(&self) -> u32 {
        self.changes
    }

    /// Adds distance travelled since the last change.
    pub fn travelled(&mut self, metres: f64) {
        if metres.is_finite() && metres > 0.0 {
            self.distance_since_change_m += metres;
        }
    }

    /// Marks every credential whose window has closed, and every one the CRL revokes.
    ///
    /// Returns the number of credentials whose state changed, so a caller can tell an
    /// expiry sweep that did nothing from one that emptied the store.
    pub fn sweep(&mut self, now: SimTime, crl: &CrlGate) -> usize {
        let mut changed = 0;
        for c in &mut self.creds {
            let next = if crl.revokes_own(&c.digest) {
                CredState::Revoked
            } else if now >= c.valid_until {
                CredState::Expired
            } else if now < c.valid_from {
                CredState::Preloaded
            } else {
                CredState::Active
            };
            if next != c.state {
                c.state = next;
                changed += 1;
            }
        }
        changed
    }

    /// Rotates to the next usable pseudonym if the policy says one is due.
    ///
    /// Returns the reason a change happened, or `None` when none was due or none was
    /// available. A node with no usable credential left is a node that must stop
    /// transmitting, which the caller sees as `Some(reason)` with
    /// [`CertStore::active`] still `None`.
    pub fn rotate(&mut self, now: SimTime) -> Option<ChangeReason> {
        let reason = self.change_due(now)?;
        let key = |c: &CredentialHandle| (c.i_period, c.j_index, c.valid_from);
        // The credential being left counts as used once, however it became active.
        if let Some(current) = self.creds.first() {
            let used = self.activations.entry(key(current)).or_insert(0);
            *used = (*used).max(1);
        }
        // The next credential is the least used usable one, and among those the one of the
        // newest i-period, then the lowest index: the pool is drained in issue order and
        // then gone round again. Picking the oldest alone went back to the lowest-numbered
        // credential every second change, so a node alternated between two pseudonyms and
        // never used the rest of its pool: exactly the reuse that makes two pseudonyms
        // linkable. Preferring the newest period matters in the hour two periods overlap
        // (CAMP-EE §2.1.5.3.2): a change there moves to the period that has just begun,
        // not to a certificate with minutes left, which would force a second change almost
        // at once.
        let pick = self
            .creds
            .iter()
            .enumerate()
            .filter(|(i, c)| *i != 0 && c.is_valid_at(now))
            .min_by_key(|(_, c)| {
                (
                    self.activations.get(&key(c)).copied().unwrap_or(0),
                    core::cmp::Reverse(c.i_period),
                    c.j_index,
                    c.valid_from,
                )
            })
            .map(|(i, _)| i);
        let Some(i) = pick else {
            // Nothing usable to move to: the node stops signing (the caller sees `active()`
            // as `None`), and no change happened. Counting one here counted a change on
            // every step of a revoked or exhausted node — hundreds of phantom pseudonym
            // changes, each a `sec.pseudonym` record, from a node that was silent.
            return Some(reason);
        };
        self.creds.swap(0, i);
        *self.activations.entry(key(&self.creds[0])).or_insert(0) += 1;
        self.last_change = Some(now);
        self.distance_since_change_m = 0.0;
        self.changes = self.changes.saturating_add(1);
        self.last_reason = Some(reason);
        Some(reason)
    }

    /// Why the last change happened, if one has.
    pub fn last_reason(&self) -> Option<ChangeReason> {
        self.last_reason
    }

    fn change_due(&self, now: SimTime) -> Option<ChangeReason> {
        match self.creds.first() {
            None => Some(ChangeReason::Startup),
            Some(c) if c.state == CredState::Revoked => Some(ChangeReason::Revoked),
            Some(c) if !c.is_valid_at(now) => Some(ChangeReason::Expired),
            Some(_) => {
                let policy = self.policy?;
                let age = Duration::between(self.last_change.unwrap_or(0), now);
                if !policy.is_due(age, self.distance_since_change_m) {
                    return None;
                }
                Some(if age >= policy.min_age {
                    ChangeReason::Age
                } else {
                    ChangeReason::Distance
                })
            }
        }
    }

    /// Bytes this store occupies, per the profile's accounting.
    pub fn bytes(&self, storage: &StorageModel) -> u64 {
        self.creds.len() as u64 * u64::from(storage.cert_bytes_per_entry)
    }
}

// ---------------------------------------------------------------------------------------
// Neighbours
// ---------------------------------------------------------------------------------------

/// What a node knows about a peer, and how sure it is (06-node-models.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationState {
    /// The signature has been checked and holds.
    Verified,
    /// The message was delivered without a signature check, per the policy.
    Unverified,
    /// The signer is on the CRL.
    Revoked,
    /// The signature was checked and failed.
    Invalid,
}

/// One entry of the neighbour table.
///
/// Note what is in it: the position the peer **claimed**, at the instant this node
/// believes it heard it. There is no true position here and no actor id, because a
/// receiver has neither — which is the whole substance of invariant I-C2.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbor {
    /// The signer's certificate digest — the only identity a receiver has.
    pub signer: HashedId8,
    /// The position the peer claimed, in the world's local tangent plane, metres.
    pub claimed_pos: v2xw_core::geom::Vec3,
    /// The speed it claimed, m/s.
    pub claimed_speed_mps: f64,
    /// The heading it claimed, radians, ENU, 0 = east.
    pub claimed_heading_rad: f64,
    /// The generation time it claimed, on the sender's clock.
    pub claimed_generation_time: SimTime,
    /// When this node believes it heard the peer last.
    pub last_heard: SimTime,
    /// How many messages this node has had from this signer.
    pub messages: u32,
    /// Whether the signature has been checked.
    pub state: VerificationState,
}

/// The neighbour table, with ageing (06-node-models.md §2.2).
///
/// Keyed by the signer digest in a [`BTreeMap`], so iteration is by digest and two runs
/// that heard the same peers in a different order still report the same table. A
/// `HashMap` here would put the node's neighbour count on the wire in whatever order the
/// hasher produced, which is exactly the class of defect ADR 0004 forbids.
#[derive(Debug, Clone, Default)]
pub struct NeighborTable {
    entries: BTreeMap<[u8; 8], Neighbor>,
    ttl: Duration,
    capacity: usize,
    expired: u32,
}

/// `T_neighbor`, the age after which an unheard peer is forgotten.
///
/// 06-node-models.md §2.2: "expiry after T_neighbor (default 3 s, `TODO: calibrate`
/// against J2945/1 path-history retention)". Three seconds is thirty CAM or BSM periods,
/// so a peer that has missed thirty consecutive transmissions is gone.
pub const DEFAULT_T_NEIGHBOR: Duration = Duration::from_secs(3);

impl NeighborTable {
    /// A table with the default ageing and a capacity.
    pub fn new(capacity: usize) -> Self {
        NeighborTable {
            entries: BTreeMap::new(),
            ttl: DEFAULT_T_NEIGHBOR,
            capacity,
            expired: 0,
        }
    }

    /// The same table with a different `T_neighbor`.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// `T_neighbor`.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// How many peers are known.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no peer is known.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry, in digest order.
    pub fn iter(&self) -> impl Iterator<Item = &Neighbor> {
        self.entries.values()
    }

    /// One peer by its signer digest.
    pub fn get(&self, signer: &HashedId8) -> Option<&Neighbor> {
        self.entries.get(&key(signer))
    }

    /// Records a message from a peer, creating or refreshing its entry.
    ///
    /// When the table is full the peer heard from longest ago is evicted, which is the
    /// only eviction order that keeps the freshest picture of the neighbourhood; ties
    /// break on the digest so the choice is deterministic.
    pub fn observe(&mut self, n: Neighbor) {
        let k = key(&n.signer);
        if let Some(existing) = self.entries.get_mut(&k) {
            let messages = existing.messages.saturating_add(1);
            *existing = Neighbor { messages, ..n };
            return;
        }
        if self.entries.len() >= self.capacity
            && let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(k, e)| (e.last_heard, **k))
                .map(|(k, _)| *k)
        {
            self.entries.remove(&victim);
        }
        self.entries.insert(k, Neighbor { messages: 1, ..n });
    }

    /// Forgets every peer not heard from within `T_neighbor` of `now`.
    ///
    /// `now` is the node's **believed** time, so a node whose clock has jumped forgets its
    /// neighbourhood — which is a real consequence of a clock attack and one a simulator
    /// using the true clock everywhere could never show.
    pub fn age(&mut self, now: SimTime) -> usize {
        let cutoff = now.saturating_sub(self.ttl.as_nanos());
        let before = self.entries.len();
        self.entries.retain(|_, e| e.last_heard >= cutoff);
        let gone = before - self.entries.len();
        self.expired = self.expired.saturating_add(gone as u32);
        gone
    }

    /// How many peers are in each verification state, as
    /// `(total, verified, unverified, revoked)`.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut verified = 0;
        let mut unverified = 0;
        let mut revoked = 0;
        for e in self.entries.values() {
            match e.state {
                VerificationState::Verified => verified += 1,
                VerificationState::Unverified => unverified += 1,
                VerificationState::Revoked => revoked += 1,
                VerificationState::Invalid => {}
            }
        }
        (self.entries.len(), verified, unverified, revoked)
    }

    /// Bytes this table occupies, per the profile's accounting.
    pub fn bytes(&self, storage: &StorageModel) -> u64 {
        self.entries.len() as u64 * u64::from(storage.neighbor_bytes_per_entry)
    }
}

fn key(id: &HashedId8) -> [u8; 8] {
    let mut k = [0u8; 8];
    k.copy_from_slice(&id.0[..]);
    k
}

// ---------------------------------------------------------------------------------------
// Revocation
// ---------------------------------------------------------------------------------------

/// How far either side of the current i-period a certificate may legitimately claim.
///
/// A CAMP pseudonym certificate's i-period is 10,080 minutes (one week) and its lifetime
/// is 10,140 minutes, so the one-hour overlap lets a certificate from the previous period
/// still be in use [CAMP-EE §2.1.5.3.2, Table 2.1.2.6.2]. One period forward covers a
/// receiver whose own clock is behind. Anything further is not a certificate this node
/// could be legitimately receiving.
pub const PLAUSIBLE_PERIOD_SKEW: u32 = 1;

/// What the CRL gate decided, and how much work it did deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrlVerdict {
    /// No entry revokes this certificate.
    NotRevoked,
    /// An entry revokes it.
    Revoked,
    /// The certificate's claimed i-period is implausible, so no entry was consulted.
    ///
    /// **Not** the same as `NotRevoked`: the node has learned nothing about whether the
    /// certificate is revoked, only that it will not spend work finding out from an
    /// unauthenticated claim. The caller drops the message.
    RefusedImplausiblePeriod {
        /// What the certificate claimed.
        claimed: u32,
        /// What the node believes the current period to be.
        current: u32,
    },
}

/// The node's view of revocation: [`v2xw_sec::CrlStore`] plus the bound that keeps an
/// unauthenticated certificate from buying unbounded work.
///
/// See the module documentation for why the bound lives here.
#[derive(Debug, Clone)]
pub struct CrlGate {
    store: v2xw_sec::CrlStore,
    current_period: u32,
    skew: u32,
    /// Entry-checks performed. Not a statistic: it is the quantity the bound is *about*,
    /// and a test that could not read it could not tell a bound that holds from one that
    /// was never exercised.
    work: u64,
    refusals: u32,
    own_revoked: Vec<[u8; 8]>,
    entries: usize,
    expansion_pm: u16,
}

/// A gate at i-period 0 with the plausibility window every gate has.
///
/// Written out rather than derived: a derived `Default` gave `skew: 0`, and every node's
/// gate is built through `Stores::default()`. With no skew a receiver refused any
/// certificate from a period other than its own, so across an i-period boundary — the
/// overlap CAMP's lifetimes exist for, where some stations have moved to the new period
/// and some have not — honest traffic was rejected as invalid, the signature detector
/// reported it, and the authority revoked honest vehicles. Found in QA with the credential
/// lifecycle compressed to 60 s periods on Manhattan: 3,645 of 33,361 verifications
/// invalid, two honest devices revoked in a run with no attacker.
impl Default for CrlGate {
    fn default() -> Self {
        CrlGate::new(0)
    }
}

impl CrlGate {
    /// A gate over an empty store, at i-period `current_period`.
    pub fn new(current_period: u32) -> Self {
        CrlGate {
            store: v2xw_sec::CrlStore::new(),
            current_period,
            skew: PLAUSIBLE_PERIOD_SKEW,
            work: 0,
            refusals: 0,
            own_revoked: Vec::new(),
            entries: 0,
            expansion_pm: 0,
        }
    }

    /// The same gate with a different plausibility window, for a protocol whose
    /// certificate lifetime spans more than one period of overlap.
    #[must_use]
    pub fn with_skew(mut self, skew: u32) -> Self {
        self.skew = skew;
        self
    }

    /// The store underneath.
    pub fn store(&self) -> &v2xw_sec::CrlStore {
        &self.store
    }

    /// Moves the gate to a new i-period, as the node's clock crosses one.
    pub fn set_period(&mut self, i: u32) {
        self.current_period = i;
    }

    /// The i-period the node believes it is in.
    pub fn current_period(&self) -> u32 {
        self.current_period
    }

    /// Adds a linkage entry.
    pub fn add_linkage_entry(&mut self, entry: CrlLinkageEntry) {
        self.store.add_linkage_entry(entry);
        self.entries += 1;
    }

    /// Revokes one certificate by hash.
    pub fn revoke_hash(&mut self, id: HashedId10) {
        self.store.revoke_hash(id);
        self.entries += 1;
    }

    /// Marks one of this node's own certificates revoked, by digest.
    ///
    /// A node that finds itself on the CRL stops transmitting [CAMP-EE §2.2.10.2], which
    /// is enforced by [`CertStore::sweep`] moving the credential to
    /// [`CredState::Revoked`] and the runtime refusing to sign without an active one.
    pub fn revoke_own(&mut self, digest: &HashedId8) {
        let k = key(digest);
        if !self.own_revoked.contains(&k) {
            self.own_revoked.push(k);
            self.own_revoked.sort_unstable();
        }
    }

    /// Whether one of this node's own certificates is revoked.
    pub fn revokes_own(&self, digest: &HashedId8) -> bool {
        self.own_revoked.binary_search(&key(digest)).is_ok()
    }

    /// How many entries of both kinds.
    pub fn entries(&self) -> usize {
        self.store.len()
    }

    /// Bytes the store occupies, per the profile's accounting.
    pub fn bytes(&self, storage: &StorageModel) -> u64 {
        self.store.len() as u64 * u64::from(storage.crl_bytes_per_entry)
    }

    /// How far through the current i-period's expansion the node is, per mille.
    pub fn expansion_pm(&self) -> u16 {
        self.expansion_pm
    }

    /// Records expansion progress for the current period.
    pub fn set_expansion_pm(&mut self, pm: u16) {
        self.expansion_pm = pm.min(1000);
    }

    /// How many entry-checks this gate has performed.
    pub fn work(&self) -> u64 {
        self.work
    }

    /// How many checks it refused on implausible-period grounds.
    pub fn refusals(&self) -> u32 {
        self.refusals
    }

    /// Clears the work counters for the next window.
    pub fn reset_window(&mut self) {
        self.work = 0;
        self.refusals = 0;
    }

    /// Checks a certificate's linkage value against the CRL, bounded.
    ///
    /// `authenticated` says whether the certificate's own signature has already been
    /// verified. It is not advisory: with `false`, a claimed i-period outside the
    /// plausibility window is refused without a single entry being consulted, so the
    /// amount of hashing one forged certificate header can buy is bounded by the window
    /// rather than by the field the forger chose.
    ///
    /// With `true` the window still applies, because a certificate whose signature checks
    /// out but whose period is a year away is a different problem and one the caller
    /// should see as a refusal rather than as "not revoked".
    pub fn check(
        &mut self,
        claimed_period: u32,
        lv: LinkageValue,
        authenticated: bool,
    ) -> CrlVerdict {
        let lo = self.current_period.saturating_sub(self.skew);
        let hi = self.current_period.saturating_add(self.skew);
        if claimed_period < lo || claimed_period > hi {
            self.refusals = self.refusals.saturating_add(1);
            return CrlVerdict::RefusedImplausiblePeriod {
                claimed: claimed_period,
                current: self.current_period,
            };
        }
        let _ = authenticated;
        self.work = self.work.saturating_add(self.store.len() as u64);
        if self.store.revokes_linkage_at_period(claimed_period, lv) {
            CrlVerdict::Revoked
        } else {
            CrlVerdict::NotRevoked
        }
    }
}

// ---------------------------------------------------------------------------------------
// Peer certificate cache and report outbox
// ---------------------------------------------------------------------------------------

/// The peer certificate cache with a capacity and a least-recently-used eviction order
/// (06-node-models.md §2.2).
///
/// [`v2xw_sec::PeerCertCache`] holds the certificates and knows which have been verified;
/// what a node adds is that the cache is finite and that a miss is what triggers P2PCD.
#[derive(Debug, Default)]
pub struct PeerCertCache {
    inner: v2xw_sec::PeerCertCache,
    capacity: usize,
    /// Digests in least-recently-used order. The node models *which* certificates it
    /// holds and what they cost it in bytes; `inner` holds the certificates themselves
    /// for a scenario running real ones, and stays empty for one running the size model.
    lru: Vec<[u8; 8]>,
    p2pcd_requests: u32,
    evictions: u32,
}

impl PeerCertCache {
    /// A cache holding at most `capacity` certificates.
    ///
    /// The capacity is a scenario parameter: 06-node-models §2.2 marks it
    /// `TODO: calibrate` and no profile publishes one, so nothing here picks a number.
    pub fn new(capacity: usize) -> Self {
        PeerCertCache {
            inner: v2xw_sec::PeerCertCache::new(),
            capacity,
            lru: Vec::new(),
            p2pcd_requests: 0,
            evictions: 0,
        }
    }

    /// The cryptographic cache underneath.
    pub fn inner(&self) -> &v2xw_sec::PeerCertCache {
        &self.inner
    }

    /// How many certificates are held.
    pub fn len(&self) -> usize {
        self.lru.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.lru.is_empty()
    }

    /// Whether this certificate is already known.
    pub fn contains(&self, digest: &HashedId8) -> bool {
        self.lru.contains(&key(digest))
    }

    /// Learns a certificate that arrived attached to a message, evicting the
    /// least-recently-used entry if the cache is full.
    ///
    /// Returns the evicted digest, if any. This is the P2PCD *response* path: a peer
    /// attaching its full certificate is what stops the next message from that peer
    /// costing a request.
    pub fn learn(&mut self, digest: &HashedId8) -> Option<[u8; 8]> {
        self.note_insert(digest)
    }

    /// Learns a real certificate that arrived attached to a message.
    ///
    /// The digest form ([`PeerCertCache::learn`]) models *that* a certificate is held and
    /// what it costs in bytes; this holds the certificate itself, which is what a receiver
    /// needs in order to check the next signature from that peer against a real public
    /// key. `verified` stays `false`: this node has checked no chain — see
    /// [`crate::secure::NodeSecurity::verify_parsed`] for why there is none to check while
    /// no credential protocol ships — and a cache that claimed otherwise would be
    /// asserting exactly the thing nobody computed.
    pub fn learn_certificate(
        &mut self,
        certificate: std::sync::Arc<v2xw_msg::sec_types::Certificate>,
    ) -> Option<[u8; 8]> {
        let digest = self.inner.insert(certificate, false).ok()?;
        self.note_insert(&digest)
    }

    /// The certificate held for `digest`, if this cache holds one.
    pub fn certificate(
        &self,
        digest: &HashedId8,
    ) -> Option<std::sync::Arc<v2xw_msg::sec_types::Certificate>> {
        self.inner.get(digest).map(|c| c.certificate.clone())
    }

    /// How many P2PCD requests this node has issued in the window.
    pub fn p2pcd_requests(&self) -> u32 {
        self.p2pcd_requests
    }

    /// How many certificates have been evicted for capacity.
    pub fn evictions(&self) -> u32 {
        self.evictions
    }

    /// Looks a certificate up, refreshing its recency. `false` is a miss, and a miss is
    /// what triggers P2PCD (06-node-models.md §2.2).
    pub fn touch(&mut self, digest: &HashedId8) -> bool {
        let k = key(digest);
        if !self.lru.contains(&k) {
            return false;
        }
        self.lru.retain(|e| *e != k);
        self.lru.push(k);
        true
    }

    /// Records that a lookup missed and a P2PCD request went out.
    pub fn record_p2pcd_request(&mut self) {
        self.p2pcd_requests = self.p2pcd_requests.saturating_add(1);
    }

    /// Clears the window counters.
    pub fn reset_window(&mut self) {
        self.p2pcd_requests = 0;
        self.evictions = 0;
    }

    /// Bytes the cache occupies, per the profile's accounting.
    pub fn bytes(&self, storage: &StorageModel) -> u64 {
        self.len() as u64 * u64::from(storage.cert_bytes_per_entry)
    }

    /// Notes that `digest` is the newest entry and evicts the oldest if the cache is over
    /// capacity. Returns the evicted digest, if any.
    pub fn note_insert(&mut self, digest: &HashedId8) -> Option<[u8; 8]> {
        let k = key(digest);
        self.lru.retain(|e| *e != k);
        self.lru.push(k);
        if self.lru.len() > self.capacity {
            self.evictions = self.evictions.saturating_add(1);
            return Some(self.lru.remove(0));
        }
        None
    }
}

/// A misbehaviour report waiting for a path to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReport {
    /// The node the report is about, as this node knows it: a signer digest.
    pub about: HashedId8,
    /// The encoded report.
    pub bytes: u32,
    /// When it was created, on the node's own clock.
    pub created: SimTime,
}

/// Reports held until a path exists (06-node-models.md §2.2).
#[derive(Debug, Clone, Default)]
pub struct ReportOutbox {
    pending: Vec<PendingReport>,
    dropped_stale: u32,
}

/// How long a node may hold an unsent report.
///
/// 05-protocols.md §2.6: "EE may delete unsent reports older than 1 week
/// [CAMP-EE §2.2.8]".
pub const REPORT_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

impl ReportOutbox {
    /// An empty outbox.
    pub fn new() -> Self {
        ReportOutbox::default()
    }

    /// Adds a report.
    pub fn push(&mut self, r: PendingReport) {
        self.pending.push(r);
    }

    /// How many reports are pending.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// True when nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Total pending bytes.
    pub fn bytes(&self) -> u64 {
        self.pending.iter().map(|r| u64::from(r.bytes)).sum()
    }

    /// Drops reports older than [`REPORT_MAX_AGE`], returning how many went.
    pub fn sweep(&mut self, now: SimTime) -> usize {
        let cutoff = now.saturating_sub(REPORT_MAX_AGE.as_nanos());
        let before = self.pending.len();
        self.pending.retain(|r| r.created >= cutoff);
        let gone = before - self.pending.len();
        self.dropped_stale = self.dropped_stale.saturating_add(gone as u32);
        gone
    }

    /// Takes everything pending, oldest first.
    pub fn drain(&mut self) -> Vec<PendingReport> {
        core::mem::take(&mut self.pending)
    }
}

/// Every store a node has, in one place (03-interfaces.md §8: `fn stores(&self) ->
/// &Stores`).
#[derive(Debug, Default)]
pub struct Stores {
    /// The node's own credentials.
    pub certs: CertStore,
    /// Certificates learned from peers.
    pub peers: PeerCertCache,
    /// Revocation, bounded.
    pub crl: CrlGate,
    /// CA certificates and policy files.
    pub trust: v2xw_sec::TrustStore,
    /// What this node has heard.
    pub neighbors: NeighborTable,
    /// Reports awaiting a path.
    pub outbox: ReportOutbox,
}

impl Stores {
    /// Total bytes across the stores, per the profile's accounting.
    pub fn bytes(&self, storage: &StorageModel) -> u64 {
        self.certs
            .bytes(storage)
            .saturating_add(self.peers.bytes(storage))
            .saturating_add(self.crl.bytes(storage))
            .saturating_add(self.neighbors.bytes(storage))
            .saturating_add(self.outbox.bytes())
    }
}

/// A node id rendered as a signer digest, for tests and for scenarios that do not model
/// real certificates.
///
/// The low 8 bytes of `SHA-256(b"v2xw-node/pseudo-signer" ‖ LE(node) ‖ LE(index))`,
/// which is how IEEE 1609.2 derives a `HashedId8` from a certificate. It is a stable
/// function of its inputs with no ambient state, so two runs agree, and the domain prefix
/// keeps it from colliding with a real certificate digest.
pub fn pseudo_signer(node: NodeId, index: u32) -> HashedId8 {
    let mut input = Vec::with_capacity(32);
    input.extend_from_slice(b"v2xw-node/pseudo-signer");
    input.extend_from_slice(&node.index().to_le_bytes());
    input.extend_from_slice(&index.to_le_bytes());
    v2xw_sec::hashed_id8(&input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::geom::Vec3;
    use v2xw_core::time::NS_PER_S;
    use v2xw_sec::linkage::{DEFAULT_JMAX, DeviceLinkageContext, LaId, LinkageSeed};

    fn cred(i: u32, j: u32, from: SimTime, until: SimTime) -> CredentialHandle {
        CredentialHandle {
            digest: pseudo_signer(NodeId::new(1), i * 100 + j),
            cert_coer: vec![0u8; 120],
            key: v2xw_sec::KeyId(u64::from(i) * 100 + u64::from(j)),
            i_period: i,
            j_index: j,
            valid_from: from,
            valid_until: until,
            state: CredState::Active,
        }
    }

    fn nbr(seed: u32, at: SimTime, state: VerificationState) -> Neighbor {
        Neighbor {
            signer: pseudo_signer(NodeId::new(seed), 0),
            claimed_pos: Vec3::new(f64::from(seed), 0.0, 0.0),
            claimed_speed_mps: 10.0,
            claimed_heading_rad: 0.0,
            claimed_generation_time: at,
            last_heard: at,
            messages: 1,
            state,
        }
    }

    // ---------------------------------------------------------------------------------
    // Pseudonym rotation
    // ---------------------------------------------------------------------------------

    /// The `CERTCHG` reading needs both conditions; the NYC-pilot reading needs either.
    /// One table, two rules, both from 05-protocols.md §2.4.
    #[test]
    fn the_two_published_rotation_rules_differ_and_both_are_expressible() {
        let five_min = Duration::from_secs(300);
        let four_min = Duration::from_secs(240);
        assert!(RotationPolicy::J2945_1.is_due(five_min, 2500.0));
        assert!(!RotationPolicy::J2945_1.is_due(five_min, 1500.0));
        assert!(!RotationPolicy::J2945_1.is_due(four_min, 2500.0));

        assert!(RotationPolicy::NYC_PILOT.is_due(five_min, 1500.0));
        assert!(RotationPolicy::NYC_PILOT.is_due(four_min, 2500.0));
        assert!(!RotationPolicy::NYC_PILOT.is_due(four_min, 1500.0));
    }

    /// Rotation picks the oldest usable pseudonym and resets both counters, so a node
    /// drains its pool in issue order rather than by download order.
    #[test]
    fn rotation_drains_the_pool_in_issue_order() {
        let mut s = CertStore::new().with_policy(RotationPolicy::J2945_1);
        // Inserted out of order on purpose.
        s.insert(cred(10, 5, 0, 1000 * NS_PER_S));
        s.insert(cred(10, 9, 0, 1000 * NS_PER_S));
        s.insert(cred(10, 1, 0, 1000 * NS_PER_S));
        s.insert(cred(10, 3, 0, 1000 * NS_PER_S));
        assert_eq!(s.active().unwrap().j_index, 5);

        // Not due: no distance yet.
        assert_eq!(s.rotate(400 * NS_PER_S), None);
        s.travelled(2500.0);
        assert_eq!(s.rotate(400 * NS_PER_S), Some(ChangeReason::Age));
        assert_eq!(s.active().unwrap().j_index, 1, "the oldest usable one");
        assert_eq!(s.changes(), 1);

        // The distance counter reset with the change.
        assert_eq!(s.rotate(800 * NS_PER_S), None);
        s.travelled(2500.0);
        assert_eq!(s.rotate(800 * NS_PER_S), Some(ChangeReason::Age));
        assert_eq!(s.active().unwrap().j_index, 3);
    }

    /// Every credential of the pool is used once before any is used again. The rule
    /// before this picked the lowest-numbered usable credential, so from the third change
    /// on a node alternated between two pseudonyms and never touched the others.
    #[test]
    fn rotation_goes_round_the_whole_pool_before_reusing_a_pseudonym() {
        let policy = RotationPolicy {
            min_age: Duration::from_secs(10),
            min_distance_m: f64::INFINITY,
            require_both: false,
        };
        let mut s = CertStore::new().with_policy(policy);
        for j in 0..5 {
            s.insert(cred(10, j, 0, 1000 * NS_PER_S));
        }
        let mut seen = vec![s.active().unwrap().j_index];
        for k in 1..=9u64 {
            assert_eq!(s.rotate(k * 10 * NS_PER_S), Some(ChangeReason::Age));
            seen.push(s.active().unwrap().j_index);
        }
        assert_eq!(seen, vec![0, 1, 2, 3, 4, 0, 1, 2, 3, 4]);
    }

    /// A node with nothing left to change to makes no change, however many steps it asks:
    /// a revoked or exhausted pool is a silent node, not one changing pseudonym every step.
    #[test]
    fn an_exhausted_pool_counts_no_changes() {
        let mut s = CertStore::new().with_policy(RotationPolicy::NYC_PILOT);
        s.insert(cred(0, 0, 0, 10 * NS_PER_S));
        for k in 11..20u64 {
            assert_eq!(s.rotate(k * NS_PER_S), Some(ChangeReason::Expired));
        }
        assert_eq!(
            s.changes(),
            0,
            "a pool with nothing valid changed pseudonym"
        );
    }

    /// In the hour two i-periods overlap, a change moves to the period that has just
    /// begun rather than to a certificate of the ending one, which would expire minutes
    /// later and force a second change — and the change's reason is kept.
    #[test]
    fn a_change_in_the_overlap_moves_to_the_new_period() {
        let policy = RotationPolicy {
            min_age: Duration::from_secs(10),
            min_distance_m: f64::INFINITY,
            require_both: false,
        };
        let mut s = CertStore::new().with_policy(policy);
        // Period 0 valid [0, 41 s), period 1 valid [40 s, 81 s): one second of overlap.
        for j in 0..3 {
            s.insert(cred(0, j, 0, 41 * NS_PER_S));
        }
        for j in 0..3 {
            s.insert(cred(1, j, 40 * NS_PER_S, 81 * NS_PER_S));
        }
        assert_eq!(s.rotate(40_500_000_000), Some(ChangeReason::Age));
        assert_eq!(s.last_reason(), Some(ChangeReason::Age));
        let now = s.active().unwrap();
        assert_eq!(
            (now.i_period, now.j_index),
            (1, 0),
            "a certificate with half a second left was chosen over the new period"
        );
    }

    /// A node whose own certificate is revoked has no active credential, which is what
    /// makes it stop transmitting [CAMP-EE §2.2.10.2].
    #[test]
    fn a_revoked_own_certificate_leaves_the_node_unable_to_sign() {
        let mut s = CertStore::new();
        let c = cred(10, 0, 0, 1000 * NS_PER_S);
        let digest = c.digest.clone();
        s.insert(c);
        let mut crl = CrlGate::new(10);
        assert_eq!(s.sweep(NS_PER_S, &crl), 0);
        assert!(s.active().is_some());

        crl.revoke_own(&digest);
        assert_eq!(s.sweep(NS_PER_S, &crl), 1);
        assert!(s.active().is_none(), "a revoked node must not sign");
        assert_eq!(s.credentials()[0].state, CredState::Revoked);
    }

    /// The expiry sweep moves a credential out of its window in both directions.
    #[test]
    fn the_sweep_separates_preloaded_from_active_from_expired() {
        let mut s = CertStore::new();
        s.insert(cred(10, 0, 0, 100 * NS_PER_S));
        s.insert(cred(11, 0, 100 * NS_PER_S, 200 * NS_PER_S));
        let crl = CrlGate::new(10);
        s.sweep(50 * NS_PER_S, &crl);
        assert_eq!(s.credentials()[0].state, CredState::Active);
        assert_eq!(s.credentials()[1].state, CredState::Preloaded);
        assert_eq!(s.active_count(50 * NS_PER_S), 1);
        s.sweep(150 * NS_PER_S, &crl);
        assert_eq!(s.credentials()[0].state, CredState::Expired);
        assert_eq!(s.credentials()[1].state, CredState::Active);
    }

    // ---------------------------------------------------------------------------------
    // Neighbour ageing
    // ---------------------------------------------------------------------------------

    /// Peers age out on the node's own clock at `T_neighbor`, and the counts split by
    /// verification state exactly as the telemetry record needs them.
    #[test]
    fn neighbours_age_out_and_are_counted_by_state() {
        let mut t = NeighborTable::new(16);
        t.observe(nbr(1, 0, VerificationState::Verified));
        t.observe(nbr(2, 0, VerificationState::Unverified));
        t.observe(nbr(3, 2 * NS_PER_S, VerificationState::Revoked));
        assert_eq!(t.counts(), (3, 1, 1, 1));

        // At t = 3 s the two heard at zero are exactly at the cutoff and survive.
        assert_eq!(t.age(3 * NS_PER_S), 0);
        assert_eq!(t.len(), 3);
        // A nanosecond later they are gone.
        assert_eq!(t.age(3 * NS_PER_S + 1), 2);
        assert_eq!(t.counts(), (1, 0, 0, 1));
    }

    /// A full table evicts the peer heard from longest ago, not an arbitrary one, and the
    /// choice does not depend on insertion order.
    #[test]
    fn a_full_table_evicts_the_stalest_peer() {
        let mut t = NeighborTable::new(2);
        t.observe(nbr(1, 10, VerificationState::Verified));
        t.observe(nbr(2, 30, VerificationState::Verified));
        t.observe(nbr(3, 20, VerificationState::Verified));
        assert_eq!(t.len(), 2);
        assert!(t.get(&pseudo_signer(NodeId::new(1), 0)).is_none());
        assert!(t.get(&pseudo_signer(NodeId::new(2), 0)).is_some());
        assert!(t.get(&pseudo_signer(NodeId::new(3), 0)).is_some());
    }

    /// Re-hearing a peer refreshes it and counts the message rather than duplicating the
    /// entry.
    #[test]
    fn re_hearing_a_peer_refreshes_one_entry() {
        let mut t = NeighborTable::new(8);
        t.observe(nbr(1, 0, VerificationState::Unverified));
        t.observe(nbr(1, NS_PER_S, VerificationState::Verified));
        assert_eq!(t.len(), 1);
        let e = t.get(&pseudo_signer(NodeId::new(1), 0)).unwrap();
        assert_eq!(e.messages, 2);
        assert_eq!(e.last_heard, NS_PER_S);
        assert_eq!(e.state, VerificationState::Verified);
    }

    // ---------------------------------------------------------------------------------
    // The bounded linkage-CRL check
    // ---------------------------------------------------------------------------------

    fn device(seed: u8) -> DeviceLinkageContext {
        DeviceLinkageContext::new(
            LaId(0x0A0A),
            LaId(0x0B0B),
            LinkageSeed::new([seed; 16]),
            LinkageSeed::new([seed ^ 0xFF; 16]),
        )
    }

    /// The gate every node gets (`Stores::default()`) accepts a certificate one period
    /// either side of its own, as `CrlGate::new` does, and refuses two away.
    #[test]
    fn the_default_gate_keeps_the_period_overlap() {
        let lv = device(1).linkage_value_for(5, 0);
        let mut g = Stores::default().crl;
        g.set_period(5);
        for claimed in [4, 5, 6] {
            assert_eq!(
                g.check(claimed, lv, true),
                CrlVerdict::NotRevoked,
                "period {claimed}"
            );
        }
        for claimed in [3, 7] {
            assert!(
                matches!(
                    g.check(claimed, lv, true),
                    CrlVerdict::RefusedImplausiblePeriod { .. }
                ),
                "period {claimed}"
            );
        }
    }

    fn gate_with_entries(current: u32, n: u32) -> CrlGate {
        let mut g = CrlGate::new(current);
        for k in 0..n {
            let d = device(k as u8);
            g.add_linkage_entry(CrlLinkageEntry::from_device(&d, current, DEFAULT_JMAX));
        }
        g
    }

    /// The gate finds a genuinely revoked device, so the bound below is not simply
    /// refusing everything.
    #[test]
    fn a_revoked_device_is_found() {
        let mut g = gate_with_entries(100, 8);
        let lv = device(3).linkage_value_for(100, 7);
        assert_eq!(g.check(100, lv, true), CrlVerdict::Revoked);
        assert!(g.work() > 0, "the check did real work");

        let stranger = DeviceLinkageContext::new(
            LaId(0x0C0C),
            LaId(0x0D0D),
            LinkageSeed::new([0xAB; 16]),
            LinkageSeed::new([0xCD; 16]),
        );
        assert_eq!(
            g.check(100, stranger.linkage_value_for(100, 7), true),
            CrlVerdict::NotRevoked
        );
    }

    /// The overlap window: a certificate from the previous i-period is still in use for
    /// the hour CAMP's 10,140-minute lifetime gives it, so it is checked, not refused.
    #[test]
    fn the_previous_period_is_still_checked() {
        let mut g = gate_with_entries(100, 4);
        let lv = device(1).linkage_value_for(99, 3);
        // The entry's own first revoked period is 100, so period 99 is before it and is
        // not matched — but the *gate* consulted the store rather than refusing, which is
        // the distinction this test is about.
        let before = g.refusals();
        let verdict = g.check(99, lv, false);
        assert_ne!(
            verdict,
            CrlVerdict::RefusedImplausiblePeriod {
                claimed: 99,
                current: 100
            }
        );
        assert_eq!(g.refusals(), before);
    }

    /// **The bound.** An unauthenticated certificate claiming a far-future i-period costs
    /// the node nothing: no entry is consulted, so the work an attacker can buy with one
    /// forged header does not grow with the CRL or with the claimed period.
    ///
    /// The quantity asserted is the gate's own work counter, not a wall-clock duration:
    /// a timing assertion would be a machine-speed measurement, and this is a statement
    /// about how many entry-checks happen.
    #[test]
    fn an_implausible_period_costs_nothing_however_big_the_crl() {
        for entries in [8u32, 64, 512] {
            let mut g = gate_with_entries(100, entries);
            // The attacker picks the period. `v2xw-sec` already bounds the walk at 156
            // periods past the entry; the node bounds it at one period past *now*.
            for claimed in [102u32, 256, 10_000, u32::MAX] {
                let before = g.work();
                let verdict = g.check(claimed, device(0).linkage_value_for(100, 0), false);
                assert_eq!(
                    verdict,
                    CrlVerdict::RefusedImplausiblePeriod {
                        claimed,
                        current: 100
                    }
                );
                assert_eq!(
                    g.work(),
                    before,
                    "a {entries}-entry CRL did work for a period {claimed} claim"
                );
            }
            assert_eq!(g.refusals(), 4);
        }
    }

    /// The work a *plausible* claim costs is linear in the CRL and independent of the
    /// claimed period, which is the property the bound buys: the attacker-controlled
    /// multiplier is gone, and what is left is the inherent cost of linkage revocation
    /// that 04-models.md §9.6 says the simulator exists to measure.
    #[test]
    fn plausible_claims_cost_the_crl_size_and_nothing_more() {
        let lv = device(0).linkage_value_for(100, 0);
        let mut small = gate_with_entries(100, 8);
        small.check(101, lv, false);
        assert_eq!(small.work(), 8);

        let mut large = gate_with_entries(100, 512);
        large.check(101, lv, false);
        assert_eq!(large.work(), 512);

        // And the cost does not depend on which plausible period was claimed.
        let mut g = gate_with_entries(100, 64);
        g.check(99, lv, false);
        let after_one = g.work();
        g.check(101, lv, false);
        assert_eq!(g.work() - after_one, after_one);
    }

    /// The window moves with the node's belief about the i-period, so a long run does not
    /// start refusing valid certificates as the weeks pass.
    #[test]
    fn the_window_follows_the_nodes_own_period() {
        let mut g = gate_with_entries(100, 4);
        assert!(matches!(
            g.check(140, device(0).linkage_value_for(100, 0), false),
            CrlVerdict::RefusedImplausiblePeriod { .. }
        ));
        g.set_period(140);
        assert!(!matches!(
            g.check(140, device(0).linkage_value_for(100, 0), false),
            CrlVerdict::RefusedImplausiblePeriod { .. }
        ));
    }

    // ---------------------------------------------------------------------------------
    // Outbox
    // ---------------------------------------------------------------------------------

    /// Reports older than a week go [CAMP-EE §2.2.8]; the rest stay with their bytes.
    #[test]
    fn the_outbox_sheds_reports_after_a_week() {
        let week = REPORT_MAX_AGE.as_nanos();
        let mut o = ReportOutbox::new();
        o.push(PendingReport {
            about: pseudo_signer(NodeId::new(9), 0),
            bytes: 400,
            created: 0,
        });
        o.push(PendingReport {
            about: pseudo_signer(NodeId::new(8), 0),
            bytes: 600,
            created: week,
        });
        assert_eq!(o.bytes(), 1000);
        assert_eq!(o.sweep(week), 0, "exactly a week old still counts");
        assert_eq!(o.sweep(week + 1), 1);
        assert_eq!(o.len(), 1);
        assert_eq!(o.bytes(), 600);
    }
}
