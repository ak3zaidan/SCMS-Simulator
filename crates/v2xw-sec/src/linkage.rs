//! SCMS linkage values — CAMP SCP2.
//!
//! A port of `legacy/scms_sim_ref/scms_core/linkage.py`, algorithm for algorithm, with the
//! Python's own outputs as the acceptance vectors (see the crate's
//! `tests/legacy_vectors.rs`). Faithful to the CAMP SCMS PoC "SCP2: Linkage Values"
//! specification and to Brecht et al., *A Security Credential Management System for V2X
//! Communications* (IEEE T-ITS 2018, arXiv:1802.05323).
//!
//! # The three formulas
//!
//! Per Linkage Authority `x ∈ {1, 2}`, with a 16-bit `la_id_x`:
//!
//! ```text
//! seed chain  : ls_x(i)      = trunc₁₂₈( SHA-256( la_id_x ‖ ls_x(i−1) ‖ 0¹¹² ) )
//! pre-linkage : plv_x(i, j)  = trunc₇₂ ( AES(ls_x(i), la_id_x ‖ j ‖ 0⁸⁰) ⊕ (la_id_x ‖ j ‖ 0⁸⁰) )
//! linkage val : lv(i, j)     = plv_1(i, j) ⊕ plv_2(i, j)
//! ```
//!
//! The widths are fixed by the specification and are not tuning knobs: `la_id` 2 bytes,
//! `ls` 16, `j` 4 inside the AES block, `plv` and `lv` 9 bytes (72 bits). A 9-byte linkage
//! value is what `Ieee1609Dot2BaseTypes.asn`'s `LinkageValue` declares, and it is what
//! [`LinkageValue::to_asn1`] produces.
//!
//! # The three privacy properties, and where each one lives in the code
//!
//! * **Two-authority split.** Only the PCA ever computes `lv`, because it is the XOR of
//!   two values held by two separate authorities. Neither `plv` on its own is the value in
//!   the certificate — [`linkage_value`] is the only function that combines them, and
//!   [`DeviceLinkageContext`] colocates the two seeds *only* for the reference engine and
//!   says so.
//! * **Forward-only revocation.** Publishing `ls_x(i)` reveals a device's certificates
//!   from period `i` onward and no earlier, because a hash chain does not run backwards.
//!   That is [`CrlLinkageEntry::matches`]'s `cert_i < self.i` guard, and it is the reason
//!   the guard is a *correctness* requirement rather than an optimisation: without it the
//!   simulator would report a privacy property the real system has, on an implementation
//!   that does not have it.
//! * **O(1) CRL cost.** Two 128-bit seeds revoke every certificate a device will ever
//!   hold, for every future period, however many were issued. That is why a linkage entry
//!   is about 40 bytes per vehicle and 10,000 entries is about 400 KB (04-models.md §9.6)
//!   rather than one entry per certificate.

use v2xw_core::hash::sha256;

use crate::aes128;
use crate::error::{Result, SecError};

/// Bytes of a Linkage Authority identifier: a 16-bit value.
pub const LA_ID_BYTES: usize = 2;

/// Bytes of a linkage seed: 128 bits.
pub const LS_BYTES: usize = 16;

/// Bytes `j` occupies inside the 128-bit AES input block.
pub const J_BYTES: usize = 4;

/// Bytes of a pre-linkage value and of a linkage value: 72 bits.
pub const PLV_BYTES: usize = 9;

/// A Linkage Authority's 16-bit identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct LaId(pub u16);

impl LaId {
    /// The two bytes the specification packs into a hash or an AES input.
    pub const fn to_be_bytes(self) -> [u8; LA_ID_BYTES] {
        self.0.to_be_bytes()
    }
}

impl core::fmt::Display for LaId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "la{:04x}", self.0)
    }
}

/// A 128-bit linkage seed `ls_x(i)`.
///
/// A newtype rather than a `[u8; 16]` because a linkage seed and an AES key are the same
/// shape and very different things: a seed *is* used as an AES key in
/// [`pre_linkage_value`], and mixing the two up silently would produce values that look
/// plausible and match nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkageSeed([u8; LS_BYTES]);

impl LinkageSeed {
    /// A seed from its bytes.
    pub const fn new(bytes: [u8; LS_BYTES]) -> LinkageSeed {
        LinkageSeed(bytes)
    }

    /// A seed from a slice of exactly [`LS_BYTES`] bytes.
    pub fn from_slice(bytes: &[u8]) -> Result<LinkageSeed> {
        if bytes.len() != LS_BYTES {
            return Err(SecError::BadLength {
                what: "linkage seed",
                expected: LS_BYTES,
                got: bytes.len(),
            });
        }
        let mut b = [0u8; LS_BYTES];
        b.copy_from_slice(bytes);
        Ok(LinkageSeed(b))
    }

    /// The seed's bytes.
    pub const fn as_bytes(&self) -> &[u8; LS_BYTES] {
        &self.0
    }

    /// The IEEE 1609.2 `LinkageSeed` a CRL carries.
    pub fn to_asn1(self) -> v2xw_msg::sec_types::ieee1609_dot2_base_types::LinkageSeed {
        v2xw_msg::sec_types::ieee1609_dot2_base_types::LinkageSeed(
            rasn::types::FixedOctetString::from(self.0),
        )
    }
}

/// A 72-bit pre-linkage value `plv_x(i, j)`, computed by one Linkage Authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreLinkageValue([u8; PLV_BYTES]);

impl PreLinkageValue {
    /// The value's bytes.
    pub const fn as_bytes(&self) -> &[u8; PLV_BYTES] {
        &self.0
    }
}

/// A 72-bit linkage value `lv(i, j)`, as it appears in a certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkageValue([u8; PLV_BYTES]);

impl LinkageValue {
    /// A linkage value from its bytes — for one read off the wire or out of a certificate.
    pub const fn new(bytes: [u8; PLV_BYTES]) -> LinkageValue {
        LinkageValue(bytes)
    }

    /// The value's bytes.
    pub const fn as_bytes(&self) -> &[u8; PLV_BYTES] {
        &self.0
    }

    /// The IEEE 1609.2 `LinkageValue` a certificate's `linkageData` carries.
    pub fn to_asn1(self) -> v2xw_msg::sec_types::ieee1609_dot2_base_types::LinkageValue {
        v2xw_msg::sec_types::ieee1609_dot2_base_types::LinkageValue(
            rasn::types::FixedOctetString::from(self.0),
        )
    }

    /// A linkage value from the IEEE 1609.2 type.
    pub fn from_asn1(
        v: &v2xw_msg::sec_types::ieee1609_dot2_base_types::LinkageValue,
    ) -> LinkageValue {
        let mut b = [0u8; PLV_BYTES];
        b.copy_from_slice(&v.0[..]);
        LinkageValue(b)
    }
}

/// One step of the per-i-period seed hash chain: `ls_x(i)` from `ls_x(i−1)`.
///
/// `SHA-256(la_id ‖ ls_prev ‖ 0¹¹²)` truncated to 128 bits. The zero padding fills the
/// hash input to exactly 32 bytes, which is what makes the construction a
/// fixed-input-length compression rather than a variable-length hash.
pub fn linkage_seed_next(la_id: LaId, ls_prev: LinkageSeed) -> LinkageSeed {
    let mut input = [0u8; 32];
    input[..LA_ID_BYTES].copy_from_slice(&la_id.to_be_bytes());
    input[LA_ID_BYTES..LA_ID_BYTES + LS_BYTES].copy_from_slice(ls_prev.as_bytes());
    // The remaining 14 bytes stay zero: the 0^112 of the specification.
    let d = sha256(&input);
    let mut out = [0u8; LS_BYTES];
    out.copy_from_slice(&d[..LS_BYTES]);
    LinkageSeed(out)
}

/// `ls_x(i)`: the initial seed hashed forward `i` times.
///
/// Linear in `i` on purpose. There is no shortcut — that a chain cannot be evaluated
/// faster than step by step, and cannot be evaluated backwards at all, *is* the
/// construction's privacy guarantee.
pub fn linkage_seed_at(la_id: LaId, ls0: LinkageSeed, i: u32) -> LinkageSeed {
    let mut ls = ls0;
    for _ in 0..i {
        ls = linkage_seed_next(la_id, ls);
    }
    ls
}

/// `plv_x(i, j)`: AES Davies-Meyer over `la_id ‖ j ‖ 0⁸⁰`, keyed by the period seed,
/// truncated to 72 bits.
///
/// The *seed* is the AES key and the `(la_id, j)` block is the input, not the other way
/// round. That is what makes the value unpredictable to anyone who does not hold the
/// seed, and it is the detail a reimplementation gets backwards.
pub fn pre_linkage_value(la_id: LaId, ls_i: LinkageSeed, j: u32) -> PreLinkageValue {
    let mut block = [0u8; aes128::BLOCK];
    block[..LA_ID_BYTES].copy_from_slice(&la_id.to_be_bytes());
    block[LA_ID_BYTES..LA_ID_BYTES + J_BYTES].copy_from_slice(&j.to_be_bytes());
    // The remaining 10 bytes stay zero: the 0^80 of the specification.
    let dm = aes128::davies_meyer(ls_i.as_bytes(), &block);
    let mut out = [0u8; PLV_BYTES];
    out.copy_from_slice(&dm[..PLV_BYTES]);
    PreLinkageValue(out)
}

/// `lv(i, j) = plv_1 ⊕ plv_2`.
///
/// In the real system only the PCA ever calls this: it is the single point where the two
/// authorities' contributions meet, and the reason neither authority alone can recognise
/// a certificate it helped create.
pub fn linkage_value(plv1: PreLinkageValue, plv2: PreLinkageValue) -> LinkageValue {
    let mut out = [0u8; PLV_BYTES];
    for (o, (a, b)) in out.iter_mut().zip(plv1.0.iter().zip(plv2.0.iter())) {
        *o = a ^ b;
    }
    LinkageValue(out)
}

/// One device's linkage material, both authorities' halves together.
///
/// `ls1_0` and `ls2_0` are held by LA1 and LA2 respectively and are **never** shared
/// between them in the clear. This type colocates them only for the reference engine and
/// for the oracle a test needs; in the simulator each seed lives inside its own LA model
/// and only pre-linkage values encrypted for the PCA ever cross the RA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceLinkageContext {
    /// LA1's identifier.
    pub la_id1: LaId,
    /// LA2's identifier.
    pub la_id2: LaId,
    /// LA1's initial seed `ls_1(0)`.
    pub ls1_0: LinkageSeed,
    /// LA2's initial seed `ls_2(0)`.
    pub ls2_0: LinkageSeed,
}

impl DeviceLinkageContext {
    /// A context from its four parts.
    pub const fn new(
        la_id1: LaId,
        la_id2: LaId,
        ls1_0: LinkageSeed,
        ls2_0: LinkageSeed,
    ) -> DeviceLinkageContext {
        DeviceLinkageContext {
            la_id1,
            la_id2,
            ls1_0,
            ls2_0,
        }
    }

    /// The linkage value embedded in this device's certificate for period `i`, index `j`.
    pub fn linkage_value_for(&self, i: u32, j: u32) -> LinkageValue {
        let plv1 = pre_linkage_value(self.la_id1, linkage_seed_at(self.la_id1, self.ls1_0, i), j);
        let plv2 = pre_linkage_value(self.la_id2, linkage_seed_at(self.la_id2, self.ls2_0, i), j);
        linkage_value(plv1, plv2)
    }
}

/// A CRL entry that revokes one device from period `i` forward.
///
/// The published fields are exactly the two current-period linkage seeds and their
/// `la_id`s — what the Misbehaviour Authority receives from the two LAs after identity
/// resolution, and all a verifier needs to recognise the device from period `i` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrlLinkageEntry {
    /// The first revoked i-period.
    pub i: u32,
    /// LA1's identifier.
    pub la_id1: LaId,
    /// LA2's identifier.
    pub la_id2: LaId,
    /// LA1's seed *at* period `i`.
    pub ls1_i: LinkageSeed,
    /// LA2's seed *at* period `i`.
    pub ls2_i: LinkageSeed,
    /// Certificates per i-period: the range of `j` the entry covers.
    pub jmax: u32,
    /// How many i-periods past `i` this entry will walk its seed chains.
    ///
    /// See [`DEFAULT_MAX_FORWARD_PERIODS`]. A certificate claiming a period further ahead
    /// than this is not matched, and costs nothing to not match.
    pub max_forward: u32,
}

/// Certificates per i-period in the CAMP end-entity profile (20 per week).
///
/// The default `jmax`, and the reference implementation's. 04-models.md §9.6 gives the
/// initial batch as 3,120 = 20 per week × 52 × 3 years, so 20 is the `j` range a verifier
/// searches for one period.
pub const DEFAULT_JMAX: u32 = 20;

/// How far forward a linkage entry walks its hash chains: 156 i-periods, three years of
/// weeks.
///
/// **This bound is a security requirement, not a tuning knob.** `matches` walks
/// `cert_i − self.i` SHA-256 steps on two chains, and `cert_i` is the certificate's own
/// `iCert` — `IValue ::= Uint16` in `Ieee1609Dot2BaseTypes.asn`, so a peer may claim
/// 65,535, and the verifier reaches this code *before* the certificate's signature is
/// checked. Unbounded, one crafted certificate against a 10,000-entry CRL is tens of
/// minutes of CPU and the run looks hung; measured on this machine, a single entry took
/// 25 ms at `cert_i = 65535` where a legitimate one takes microseconds.
///
/// 156 is the CAMP end-entity certificate lifetime: 04-models.md §9.6's initial batch is
/// 3,120 = 20 per week × 52 weeks × 3 years, so the last certificate a device holds is 155
/// weekly periods after the first. Nothing legitimate is further ahead of a revocation
/// than its own certificate lifetime, because a certificate that old has expired and the
/// validity-period check has already refused it. Entries for a profile with a longer
/// lifetime set [`CrlLinkageEntry::max_forward`] explicitly.
pub const DEFAULT_MAX_FORWARD_PERIODS: u32 = 156;

impl CrlLinkageEntry {
    /// The entry that revokes `ctx` from period `i` forward, walking at most
    /// [`DEFAULT_MAX_FORWARD_PERIODS`] periods ahead.
    pub fn from_device(ctx: &DeviceLinkageContext, i: u32, jmax: u32) -> CrlLinkageEntry {
        CrlLinkageEntry {
            i,
            la_id1: ctx.la_id1,
            la_id2: ctx.la_id2,
            ls1_i: linkage_seed_at(ctx.la_id1, ctx.ls1_0, i),
            ls2_i: linkage_seed_at(ctx.la_id2, ctx.ls2_0, i),
            jmax,
            max_forward: DEFAULT_MAX_FORWARD_PERIODS,
        }
    }

    /// The same entry with a different forward bound — for a profile whose certificates
    /// outlive three years, or for a test that needs the bound somewhere it can see it.
    #[must_use]
    pub fn with_max_forward(self, max_forward: u32) -> CrlLinkageEntry {
        CrlLinkageEntry {
            max_forward,
            ..self
        }
    }

    /// The seeds this entry's device holds in period `cert_i`, or `None` when that period
    /// is outside the entry's reach.
    ///
    /// The chain walk depends on the period alone, never on `j`, so it belongs here rather
    /// than inside a loop over the index range: a verifier searching 20 indices walks the
    /// chain once, as a real one would, not twenty times.
    fn seeds_at(&self, cert_i: u32) -> Option<(LinkageSeed, LinkageSeed)> {
        if cert_i < self.i {
            return None;
        }
        let delta = cert_i - self.i;
        if delta > self.max_forward {
            return None;
        }
        Some((
            linkage_seed_at(self.la_id1, self.ls1_i, delta),
            linkage_seed_at(self.la_id2, self.ls2_i, delta),
        ))
    }

    /// True if any index `j < jmax` in period `cert_i` carries `cert_lv`.
    ///
    /// What a verifier actually asks: a certificate's `linkageData` carries `iCert` and
    /// the value but not `j`, so the index is searched. Two AES blocks per index, after
    /// one chain walk.
    pub fn matches_any_index(&self, cert_i: u32, cert_lv: LinkageValue) -> bool {
        let Some((ls1, ls2)) = self.seeds_at(cert_i) else {
            return false;
        };
        (0..self.jmax).any(|j| {
            linkage_value(
                pre_linkage_value(self.la_id1, ls1, j),
                pre_linkage_value(self.la_id2, ls2, j),
            ) == cert_lv
        })
    }

    /// True if the certificate `(cert_i, cert_j, cert_lv)` is revoked by this entry.
    ///
    /// **Forward-only, and boundedly forward.** A certificate from a period *before* the
    /// revocation period is never matched, even though the verifier holds a seed that a
    /// backwards-runnable chain would expose. That is the backward-privacy property — "a
    /// vehicle is trackable only after revocation" — and it is enforced here by refusing
    /// to answer rather than by relying on the hash function, because the answer is what a
    /// simulation measures. A certificate from a period more than
    /// [`CrlLinkageEntry::max_forward`] ahead is not matched either, for the reason
    /// [`DEFAULT_MAX_FORWARD_PERIODS`] gives: `cert_i` is attacker-controlled.
    pub fn matches(&self, cert_i: u32, cert_j: u32, cert_lv: LinkageValue) -> bool {
        if cert_j >= self.jmax {
            return false;
        }
        let Some((ls1, ls2)) = self.seeds_at(cert_i) else {
            return false;
        };
        let recomputed = linkage_value(
            pre_linkage_value(self.la_id1, ls1, cert_j),
            pre_linkage_value(self.la_id2, ls2, cert_j),
        );
        recomputed == cert_lv
    }

    /// The two seeds as the IEEE 1609.2 `SequenceOfLinkageSeed` a linked CRL carries.
    pub fn to_asn1_seeds(
        &self,
    ) -> v2xw_msg::sec_types::ieee1609_dot2_base_types::SequenceOfLinkageSeed {
        v2xw_msg::sec_types::ieee1609_dot2_base_types::SequenceOfLinkageSeed(vec![
            self.ls1_i.to_asn1(),
            self.ls2_i.to_asn1(),
        ])
    }
}

/// Is this certificate revoked by any entry on the CRL?
///
/// Linear in the CRL length, and deliberately not indexed: a linkage entry cannot be
/// looked up by the value it revokes, because recognising the value is the whole work.
/// That cost — one hash chain walk and two AES blocks per entry per check — is a real
/// property of linkage-based revocation and one the simulator is meant to measure, so
/// hiding it behind a cache here would remove the thing being studied.
pub fn crl_contains(
    entries: &[CrlLinkageEntry],
    cert_i: u32,
    cert_j: u32,
    cert_lv: LinkageValue,
) -> bool {
    entries.iter().any(|e| e.matches(cert_i, cert_j, cert_lv))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LA1: LaId = LaId(0x0001);
    const LA2: LaId = LaId(0x0002);

    fn device() -> DeviceLinkageContext {
        DeviceLinkageContext::new(
            LA1,
            LA2,
            LinkageSeed::new([0x01; LS_BYTES]),
            LinkageSeed::new([0x02; LS_BYTES]),
        )
    }

    /// The legacy `test_field_widths`: the specification's widths, asserted.
    #[test]
    fn the_field_widths_are_the_specifications() {
        assert_eq!((LA_ID_BYTES, LS_BYTES, J_BYTES, PLV_BYTES), (2, 16, 4, 9));
        let dev = device();
        assert_eq!(pre_linkage_value(LA1, dev.ls1_0, 0).as_bytes().len(), 9);
        assert_eq!(linkage_seed_next(LA1, dev.ls1_0).as_bytes().len(), 16);
        assert_eq!(dev.linkage_value_for(0, 0).as_bytes().len(), 9);
        // And the ASN.1 types agree, which is where a width mismatch would actually bite.
        assert_eq!(dev.linkage_value_for(0, 0).to_asn1().0.len(), 9);
        assert_eq!(dev.ls1_0.to_asn1().0.len(), 16);
    }

    /// The legacy `test_two_LA_xor_reconstruction`: the value in the certificate is
    /// exactly the XOR, and neither half is it.
    #[test]
    fn the_certificate_value_is_the_xor_and_neither_half_alone() {
        let dev = device();
        let (i, j) = (5, 3);
        let plv1 = pre_linkage_value(LA1, linkage_seed_at(LA1, dev.ls1_0, i), j);
        let plv2 = pre_linkage_value(LA2, linkage_seed_at(LA2, dev.ls2_0, i), j);
        let lv = dev.linkage_value_for(i, j);
        assert_eq!(lv, linkage_value(plv1, plv2));
        assert_ne!(lv.as_bytes(), plv1.as_bytes());
        assert_ne!(lv.as_bytes(), plv2.as_bytes());
    }

    /// The legacy `test_determinism` and `test_distinct_across_j_and_i`.
    #[test]
    fn linkage_values_are_deterministic_and_distinct_per_slot() {
        let dev = device();
        assert_eq!(dev.linkage_value_for(7, 2), dev.linkage_value_for(7, 2));
        let mut seen: Vec<LinkageValue> = Vec::new();
        for i in 0..3 {
            for j in 0..5 {
                let lv = dev.linkage_value_for(i, j);
                assert!(!seen.contains(&lv), "collision at ({i}, {j})");
                seen.push(lv);
            }
        }
        assert_eq!(seen.len(), 15);
    }

    /// The legacy `test_crl_forward_match_and_backward_privacy` — the property that makes
    /// linkage-based revocation privacy-preserving.
    #[test]
    fn revocation_matches_forward_and_never_backward() {
        let dev = device();
        let revoke_i = 10;
        let entry = CrlLinkageEntry::from_device(&dev, revoke_i, DEFAULT_JMAX);
        for cert_i in [revoke_i, revoke_i + 1, revoke_i + 5] {
            for j in [0, 7, 19] {
                assert!(
                    entry.matches(cert_i, j, dev.linkage_value_for(cert_i, j)),
                    "should match at ({cert_i}, {j})"
                );
            }
        }
        for cert_i in [0, revoke_i - 1] {
            for j in [0, 7] {
                assert!(
                    !entry.matches(cert_i, j, dev.linkage_value_for(cert_i, j)),
                    "backward privacy broken at ({cert_i}, {j})"
                );
            }
        }
    }

    /// `matches_any_index` must agree with a loop over `matches` on every in-range slot —
    /// the hoisted chain walk is an optimisation only if it computes the same thing.
    #[test]
    fn searching_the_index_range_agrees_with_asking_index_by_index() {
        let dev = device();
        let entry = CrlLinkageEntry::from_device(&dev, 3, DEFAULT_JMAX);
        for cert_i in 0..8 {
            for j in [0u32, 1, 19, 20, 40] {
                let lv = dev.linkage_value_for(cert_i, j);
                let by_index = (0..entry.jmax).any(|k| entry.matches(cert_i, k, lv));
                assert_eq!(
                    entry.matches_any_index(cert_i, lv),
                    by_index,
                    "disagreement at ({cert_i}, {j})"
                );
            }
        }
    }

    /// The certificate's `iCert` is `IValue ::= Uint16` and arrives unauthenticated, so
    /// the chain walk it drives has to be bounded. Before the bound, one entry cost 25 ms
    /// at `cert_i = 65535` in release and the full index scan half a second — per received
    /// SPDU, per CRL entry, before any signature was checked.
    ///
    /// The bound is asserted two ways: the outcome (nothing beyond `max_forward` matches)
    /// and the cost, against a baseline measured in the same test so the threshold does
    /// not depend on the machine. Unbounded, the far call is ~420× the baseline; bounded,
    /// it is a small fraction of it, because it returns without hashing at all.
    #[test]
    fn a_far_future_period_is_refused_in_bounded_time() {
        use std::time::Instant;

        let dev = device();
        let entry = CrlLinkageEntry::from_device(&dev, 0, DEFAULT_JMAX);
        assert_eq!(entry.max_forward, DEFAULT_MAX_FORWARD_PERIODS);

        // The outcome: the last period inside the bound still matches, the first outside
        // does not, and neither does the extreme an attacker can claim.
        let inside = DEFAULT_MAX_FORWARD_PERIODS;
        let outside = DEFAULT_MAX_FORWARD_PERIODS + 1;
        assert!(entry.matches_any_index(inside, dev.linkage_value_for(inside, 3)));
        assert!(entry.matches(inside, 3, dev.linkage_value_for(inside, 3)));
        assert!(!entry.matches_any_index(outside, dev.linkage_value_for(outside, 3)));
        assert!(!entry.matches(outside, 3, dev.linkage_value_for(outside, 3)));

        // The cost: a full-lifetime legitimate check is the baseline.
        let far = u32::from(u16::MAX);
        // Any value does: the far call must return before it hashes anything, and the
        // baseline call has to hash `inside` steps whether or not it then matches.
        let lv = dev.linkage_value_for(inside, 0);
        let t0 = Instant::now();
        for _ in 0..4 {
            std::hint::black_box(entry.matches_any_index(inside, lv));
        }
        let baseline = t0.elapsed() / 4;
        let t1 = Instant::now();
        for _ in 0..4 {
            std::hint::black_box(entry.matches_any_index(far, lv));
        }
        let far_cost = t1.elapsed() / 4;
        assert!(
            far_cost <= baseline * 4 + std::time::Duration::from_millis(5),
            "iCert = 65535 cost {far_cost:?} against a {baseline:?} baseline: the walk is \
             not bounded"
        );

        // An explicit bound overrides the default, in both directions.
        let none = entry.with_max_forward(0);
        assert!(none.matches_any_index(0, dev.linkage_value_for(0, 3)));
        assert!(!none.matches_any_index(1, dev.linkage_value_for(1, 3)));
        let wide = entry.with_max_forward(outside);
        assert!(wide.matches_any_index(outside, dev.linkage_value_for(outside, 3)));
    }

    /// The legacy `test_crl_does_not_match_other_device` and `test_crl_contains_helper`,
    /// with the reference's `os.urandom` replaced by a pinned seed.
    #[test]
    fn an_entry_does_not_match_another_device() {
        let victim = device();
        let other = DeviceLinkageContext::new(
            LA1,
            LA2,
            LinkageSeed::from_slice(&v2xw_core::hash::sha256(b"other-la1")[..16]).expect("16"),
            LinkageSeed::from_slice(&v2xw_core::hash::sha256(b"other-la2")[..16]).expect("16"),
        );
        let entry = CrlLinkageEntry::from_device(&victim, 4, DEFAULT_JMAX);
        for cert_i in 4..9 {
            for j in 0..3 {
                assert!(!entry.matches(cert_i, j, other.linkage_value_for(cert_i, j)));
            }
        }
        let crl = [CrlLinkageEntry::from_device(&victim, 2, DEFAULT_JMAX)];
        assert!(crl_contains(&crl, 3, 1, victim.linkage_value_for(3, 1)));
        assert!(!crl_contains(&crl, 3, 1, other.linkage_value_for(3, 1)));
        assert!(!crl_contains(&[], 3, 1, victim.linkage_value_for(3, 1)));
    }

    /// The legacy `test_out_of_range_j_rejected_by_entry`, plus the wrong-value case the
    /// legacy suite does not cover: an entry must not match a linkage value that is not
    /// the device's, even in a revoked period.
    #[test]
    fn an_entry_rejects_an_out_of_range_index_and_a_wrong_value() {
        let dev = device();
        let entry = CrlLinkageEntry::from_device(&dev, 0, DEFAULT_JMAX);
        assert!(!entry.matches(0, 20, dev.linkage_value_for(0, 0)));
        assert!(!entry.matches(0, u32::MAX, dev.linkage_value_for(0, 0)));
        assert!(entry.matches(0, 19, dev.linkage_value_for(0, 19)));
        assert!(!entry.matches(0, 0, LinkageValue::new([0u8; PLV_BYTES])));
    }

    /// The chain must not be runnable backwards, which in code terms means
    /// `linkage_seed_at` is strictly forward and a later seed never equals an earlier one.
    #[test]
    fn the_seed_chain_only_runs_forward() {
        let dev = device();
        let mut seen = Vec::new();
        for i in 0..12 {
            let ls = linkage_seed_at(LA1, dev.ls1_0, i);
            assert!(!seen.contains(&ls), "the chain repeated at i = {i}");
            seen.push(ls);
        }
        // Chaining from an intermediate seed is the same as chaining from the start,
        // which is what `CrlLinkageEntry::matches` relies on when it walks `delta` steps.
        let at5 = linkage_seed_at(LA1, dev.ls1_0, 5);
        assert_eq!(
            linkage_seed_at(LA1, at5, 3),
            linkage_seed_at(LA1, dev.ls1_0, 8)
        );
        assert_eq!(linkage_seed_at(LA1, dev.ls1_0, 0), dev.ls1_0);
    }

    /// The `la_id` really is part of both constructions: two authorities with the same
    /// seed must still produce different values, or the two-authority split would be
    /// decorative.
    #[test]
    fn the_authority_id_separates_the_two_authorities() {
        let seed = LinkageSeed::new([0x42; LS_BYTES]);
        assert_ne!(linkage_seed_next(LA1, seed), linkage_seed_next(LA2, seed));
        assert_ne!(
            pre_linkage_value(LA1, seed, 3).as_bytes(),
            pre_linkage_value(LA2, seed, 3).as_bytes()
        );
        // And a device whose two halves are identical would produce an all-zero lv, which
        // is exactly why the la_ids differ.
        let degenerate = DeviceLinkageContext::new(LA1, LA1, seed, seed);
        assert_eq!(
            degenerate.linkage_value_for(0, 0),
            LinkageValue::new([0u8; PLV_BYTES]),
            "identical halves cancel — the la_id split is what prevents this"
        );
    }

    /// A seed is 16 bytes and nothing else.
    #[test]
    fn a_seed_must_be_exactly_sixteen_bytes() {
        assert!(LinkageSeed::from_slice(&[0u8; 16]).is_ok());
        assert!(LinkageSeed::from_slice(&[0u8; 15]).is_err());
        assert!(LinkageSeed::from_slice(&[0u8; 17]).is_err());
    }

    /// The ASN.1 round trip for the value a certificate actually carries.
    #[test]
    fn a_linkage_value_round_trips_through_its_asn1_type() {
        let lv = device().linkage_value_for(3, 4);
        assert_eq!(LinkageValue::from_asn1(&lv.to_asn1()), lv);
    }
}
