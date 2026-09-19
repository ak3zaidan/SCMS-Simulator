//! Cryptographic primitive descriptors: sizes, security levels and cost anchors.
//!
//! This module is 04-models.md §9.4 turned into data. A [`PrimitiveDescriptor`] says how
//! large a key, a signature and a certificate are for one primitive, at what NIST
//! security level, and — per hardware profile — how long an operation takes, in the units
//! the source published. Nothing here computes cryptography; [`crate::crypto`] does that
//! for the two primitives that have implementations, and everything else exists so the
//! engine can *charge* and *size* an operation it does not perform.
//!
//! # Why the post-quantum primitives are here without implementations
//!
//! The question the simulator exists to answer is what happens to a V2X deployment when a
//! 2,420-byte ML-DSA-44 signature replaces a 64-byte ECDSA one: channel load, certificate
//! attachment cost, verification queue depth, CRL size. Every one of those follows from
//! the *sizes* and the *costs*, not from the signature bytes. So the descriptors and the
//! cost tables are what Phase 1 needs, and running PQClean is a later, separable job
//! ([`crate::crypto::Modeled`] already produces correctly sized tokens for them).
//!
//! # Fidelity of the numbers
//!
//! Each [`CostRow`] carries its own citation and, where the source qualifies itself, a
//! note. Two rows are recorded *as published* although they are implausible next to their
//! neighbours — the Cohda MK6 ECDSA verify at 0.001 ms and the Cohda SPHINCS+ sign at
//! 5.485 ms — because silently "fixing" a published figure is how a table stops being
//! evidence. They are flagged in [`CostRow::note`] and a test asserts the flag is there.
//!
//! A profile with no row is filled by [`CostTable::row_or_scaled`], which takes the
//! nearest cycle count and scales it by the clock ratio, and tags the result
//! [`CostRow::scaled`] so the manifest can record that the number was derived rather than
//! measured (04-models.md §9.4, last paragraph).

use std::sync::LazyLock;

use serde::Serialize;
use v2xw_core::card::{Family, ModelCard, Source, SourceKind, Tier, Validation, ValidationStatus};
use v2xw_core::math::quantize_to;
use v2xw_core::model::Model;
use v2xw_core::time::Duration;

use crate::error::{Result, SecError};

/// A citation on a descriptor or a cost row.
///
/// The same type the model cards use, so a descriptor's provenance and a card's
/// provenance are one vocabulary rather than two (03-interfaces.md §6 names the type
/// `Citation`; this alias keeps that spelling).
pub type Citation = Source;

/// A primitive's stable id, e.g. `primitive/ecdsa-p256-sha256`.
///
/// `Copy`, over `&'static str`, because the catalogue is closed at compile time and every
/// other type here keys off it: a [`crate::envelope::PrimitiveOp`] carries one, a cost
/// lookup takes one, and a `String` in either place would allocate on a per-message path.
/// The trade is that an out-of-process plug-in cannot mint a new id at run time, which
/// Phase 1 does not need — the primitive catalogue is standards material, not scenario
/// material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PrimitiveId(pub &'static str);

impl PrimitiveId {
    /// ECDSA over NIST P-256 with SHA-256 — the primitive IEEE 1609.2 and TS 103 097
    /// deployments actually use.
    pub const ECDSA_P256_SHA256: PrimitiveId = PrimitiveId("primitive/ecdsa-p256-sha256");
    /// ECDSA over brainpoolP256r1, the second P-256-class curve 1609.2 admits.
    pub const ECDSA_BRAINPOOL_P256R1: PrimitiveId = PrimitiveId("primitive/ecdsa-brainpoolp256r1");
    /// ECDSA over NIST P-384.
    pub const ECDSA_P384: PrimitiveId = PrimitiveId("primitive/ecdsa-p384");
    /// Ed25519. Present only because the legacy Python reference signed with it; no
    /// 1609.2 profile admits it.
    pub const ED25519: PrimitiveId = PrimitiveId("primitive/ed25519");
    /// ECQV implicit certificates over P-256 (SEC 4).
    pub const ECQV_P256: PrimitiveId = PrimitiveId("primitive/ecqv-p256");
    /// SHA-256.
    pub const SHA_256: PrimitiveId = PrimitiveId("primitive/sha-256");
    /// ML-DSA-44 (FIPS 204).
    pub const ML_DSA_44: PrimitiveId = PrimitiveId("primitive/ml-dsa-44");
    /// ML-DSA-65 (FIPS 204).
    pub const ML_DSA_65: PrimitiveId = PrimitiveId("primitive/ml-dsa-65");
    /// ML-DSA-87 (FIPS 204).
    pub const ML_DSA_87: PrimitiveId = PrimitiveId("primitive/ml-dsa-87");
    /// Falcon-512, padded signature form.
    pub const FALCON_512: PrimitiveId = PrimitiveId("primitive/falcon-512");
    /// Falcon-1024, padded signature form.
    pub const FALCON_1024: PrimitiveId = PrimitiveId("primitive/falcon-1024");
    /// SLH-DSA-SHA2-128s (FIPS 205).
    pub const SLH_DSA_SHA2_128S: PrimitiveId = PrimitiveId("primitive/slh-dsa-sha2-128s");
    /// SLH-DSA-SHA2-128f (FIPS 205).
    pub const SLH_DSA_SHA2_128F: PrimitiveId = PrimitiveId("primitive/slh-dsa-sha2-128f");
    /// SLH-DSA-SHA2-192s (FIPS 205).
    pub const SLH_DSA_SHA2_192S: PrimitiveId = PrimitiveId("primitive/slh-dsa-sha2-192s");
    /// SLH-DSA-SHA2-192f (FIPS 205).
    pub const SLH_DSA_SHA2_192F: PrimitiveId = PrimitiveId("primitive/slh-dsa-sha2-192f");
    /// SLH-DSA-SHA2-256s (FIPS 205).
    pub const SLH_DSA_SHA2_256S: PrimitiveId = PrimitiveId("primitive/slh-dsa-sha2-256s");
    /// SLH-DSA-SHA2-256f (FIPS 205).
    pub const SLH_DSA_SHA2_256F: PrimitiveId = PrimitiveId("primitive/slh-dsa-sha2-256f");
    /// ML-KEM-512 (FIPS 203).
    pub const ML_KEM_512: PrimitiveId = PrimitiveId("primitive/ml-kem-512");
    /// ML-KEM-768 (FIPS 203).
    pub const ML_KEM_768: PrimitiveId = PrimitiveId("primitive/ml-kem-768");
    /// ML-KEM-1024 (FIPS 203).
    pub const ML_KEM_1024: PrimitiveId = PrimitiveId("primitive/ml-kem-1024");
    /// ML-DSA-44 + ECDSA P-256 hybrid.
    pub const HYBRID_MLDSA44_ECDSA_P256: PrimitiveId =
        PrimitiveId("primitive/hybrid-mldsa44-ecdsa-p256");
    /// ML-DSA-65 + ECDSA P-256 hybrid.
    pub const HYBRID_MLDSA65_ECDSA_P256: PrimitiveId =
        PrimitiveId("primitive/hybrid-mldsa65-ecdsa-p256");
    /// Falcon-512 + ECDSA P-256 hybrid.
    pub const HYBRID_FALCON512_ECDSA_P256: PrimitiveId =
        PrimitiveId("primitive/hybrid-falcon512-ecdsa-p256");

    /// The id as a string.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl core::fmt::Display for PrimitiveId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

/// What kind of primitive this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrimitiveFamily {
    /// A digital signature scheme.
    Signature,
    /// A key-encapsulation mechanism.
    Kem,
    /// A hash function.
    Hash,
    /// An authenticated cipher.
    Aead,
    /// An implicit-certificate scheme (ECQV).
    ImplicitCert,
}

/// A size that is either fixed or variable.
///
/// Falcon is the reason this is not a `u32`: its signatures are genuinely
/// variable-length, and both numbers matter — the mean drives channel load, the maximum
/// drives buffer sizing and the fragmentation threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SizeSpec {
    /// Always exactly this many bytes.
    Fixed(u32),
    /// Variable, with a mean and a hard maximum.
    Variable {
        /// Typical size, in bytes.
        mean: u32,
        /// Largest possible size, in bytes.
        max: u32,
    },
}

impl SizeSpec {
    /// The size to use when one number is needed: the fixed size, or the mean.
    pub const fn nominal(self) -> u32 {
        match self {
            SizeSpec::Fixed(n) => n,
            SizeSpec::Variable { mean, .. } => mean,
        }
    }

    /// The largest size this primitive can produce — what a buffer must hold.
    pub const fn max(self) -> u32 {
        match self {
            SizeSpec::Fixed(n) => n,
            SizeSpec::Variable { max, .. } => max,
        }
    }
}

/// Where an operation runs on a given hardware profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CostSite {
    /// On the application processor.
    Cpu,
    /// In a hardware security module.
    Hsm,
    /// On a dedicated crypto accelerator or verification engine.
    Accel,
}

/// The cost of one operation on one hardware profile, in the units the source published.
///
/// Every field is optional because sources are: a cycle-count paper gives cycles, a
/// benchmark blog gives operations per second, a datasheet gives milliseconds. Nothing is
/// converted on the way in — [`OpCost::mean`] converts on the way *out*, once, so the
/// stored value stays comparable with its citation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OpCost {
    /// Cycles, as published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycles: Option<u64>,
    /// Mean wall time in microseconds, as published or derived from `ops_per_s`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_us: Option<f64>,
    /// 95th-percentile wall time in microseconds, where the source gives a spread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_us: Option<f64>,
    /// Operations per second, as published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops_per_s: Option<f64>,
}

/// Microsecond quantum for every cost value (ADR 0004 decision 7, build decision D9).
///
/// A nanosecond, expressed in the microseconds these fields are stored in. The engine
/// charges costs as an integer [`Duration`] in nanoseconds, so storing a cost finer than
/// that grid would record a distinction the scheduler cannot represent.
pub const COST_US_QUANTUM: f64 = 1e-3;

impl OpCost {
    /// A cost published as a cycle count, with no wall time.
    pub const fn from_cycles(cycles: u64) -> OpCost {
        OpCost {
            cycles: Some(cycles),
            mean_us: None,
            p95_us: None,
            ops_per_s: None,
        }
    }

    /// A cost published as both a cycle count and a wall time.
    pub fn from_cycles_and_us(cycles: u64, mean_us: f64) -> OpCost {
        OpCost {
            cycles: Some(cycles),
            mean_us: Some(quantize_to(mean_us, COST_US_QUANTUM)),
            p95_us: None,
            ops_per_s: None,
        }
    }

    /// A cost published as a wall time in microseconds.
    pub fn from_us(mean_us: f64) -> OpCost {
        OpCost {
            cycles: None,
            mean_us: Some(quantize_to(mean_us, COST_US_QUANTUM)),
            p95_us: None,
            ops_per_s: None,
        }
    }

    /// A cost published as a throughput in operations per second.
    ///
    /// `mean_us` is filled in as `10^6 / rate`, quantised to the nanosecond, so a cost
    /// lookup does not have to know which form the source used.
    pub fn from_ops_per_s(rate: f64) -> OpCost {
        OpCost {
            cycles: None,
            mean_us: Some(quantize_to(1.0e6 / rate, COST_US_QUANTUM)),
            p95_us: None,
            ops_per_s: Some(rate),
        }
    }

    /// A cost published as a mean cycle count together with the worst case the source
    /// measured, at a known clock.
    ///
    /// `pqm4` publishes min/mean/max rather than percentiles for the schemes whose signing
    /// loop rejects and retries. The measured maximum is the honest stand-in for a p95 —
    /// it is conservative, and a queueing model that used the mean alone would understate
    /// the tail that matters.
    pub fn from_cycles_with_max(mean_cycles: u64, max_cycles: u64, clock_hz: f64) -> OpCost {
        OpCost {
            cycles: Some(mean_cycles),
            mean_us: None,
            p95_us: Some(quantize_to(
                (max_cycles as f64) / clock_hz * 1.0e6,
                COST_US_QUANTUM,
            )),
            ops_per_s: None,
        }
    }

    /// The mean cost as an engine [`Duration`], or `None` when only cycles are known and
    /// no clock was supplied.
    ///
    /// Rounds to the nanosecond, which is the engine's time quantum.
    pub fn mean(&self, clock_hz: Option<f64>) -> Option<Duration> {
        if let Some(us) = self.mean_us {
            return Some(Duration::from_nanos((us * 1000.0).round().max(0.0) as u64));
        }
        let (cycles, hz) = (self.cycles?, clock_hz?);
        let ns = (cycles as f64) / hz * 1.0e9;
        Some(Duration::from_nanos(ns.round().max(0.0) as u64))
    }
}

/// One hardware profile's costs for one primitive.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostRow {
    /// The hardware profile's model id, as the scenario spells it.
    pub profile: &'static str,
    /// Clock frequency in hertz, where the source states one. Needed to turn a cycle
    /// count into a wall time and to scale a cycle count onto another profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_hz: Option<f64>,
    /// Where the operation runs.
    pub site: CostSite,
    /// Key generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keygen: Option<OpCost>,
    /// Signing (or encapsulation, for a KEM).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign: Option<OpCost>,
    /// Verification (or decapsulation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<OpCost>,
    /// Where the numbers come from.
    pub source: Citation,
    /// Anything that qualifies them: an unstated OS, an anomalous published value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    /// True when this row was derived by scaling another profile's cycle count rather
    /// than measured. The run manifest records it (04-models.md §9.4).
    pub scaled: bool,
}

impl CostRow {
    fn new(profile: &'static str, site: CostSite, source: Citation) -> CostRow {
        CostRow {
            profile,
            clock_hz: None,
            site,
            keygen: None,
            sign: None,
            verify: None,
            source,
            note: None,
            scaled: false,
        }
    }

    fn at(mut self, clock_hz: f64) -> CostRow {
        self.clock_hz = Some(clock_hz);
        self
    }

    fn sign(mut self, c: OpCost) -> CostRow {
        self.sign = Some(c);
        self
    }

    fn verify(mut self, c: OpCost) -> CostRow {
        self.verify = Some(c);
        self
    }

    fn keygen(mut self, c: OpCost) -> CostRow {
        self.keygen = Some(c);
        self
    }

    fn note(mut self, n: &'static str) -> CostRow {
        self.note = Some(n);
        self
    }

    /// The cost of `op` on this profile.
    pub fn op(&self, op: PrimitiveOpKind) -> Option<&OpCost> {
        match op {
            PrimitiveOpKind::KeyGen => self.keygen.as_ref(),
            PrimitiveOpKind::Sign => self.sign.as_ref(),
            PrimitiveOpKind::Verify => self.verify.as_ref(),
        }
    }

    /// The mean duration of `op` on this profile.
    pub fn duration(&self, op: PrimitiveOpKind) -> Option<Duration> {
        self.op(op)?.mean(self.clock_hz)
    }
}

/// Which of the three costed operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrimitiveOpKind {
    /// Key generation.
    KeyGen,
    /// Signing or encapsulation.
    Sign,
    /// Verification or decapsulation.
    Verify,
}

/// A primitive's cost anchors, keyed by hardware profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CostTable {
    rows: Vec<CostRow>,
}

impl CostTable {
    /// A table from its rows, sorted by profile id.
    ///
    /// Sorted at construction, not at lookup: every iteration over a cost table —
    /// a report, a card payload, a manifest digest — must produce the same order on every
    /// platform, and sorting once at build time makes that free rather than a rule
    /// callers have to remember (02-architecture.md §6.4).
    pub fn new(mut rows: Vec<CostRow>) -> CostTable {
        rows.sort_by(|a, b| a.profile.cmp(b.profile));
        CostTable { rows }
    }

    /// The row for `profile`, if one was published.
    pub fn row(&self, profile: &str) -> Option<&CostRow> {
        self.rows.iter().find(|r| r.profile == profile)
    }

    /// Every row, in profile order.
    pub fn rows(&self) -> &[CostRow] {
        &self.rows
    }

    /// True when the table has no rows at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row for `profile`, or one derived by scaling the nearest published cycle count
    /// to `target_clock_hz`.
    ///
    /// This is 04-models.md §9.4's rule, implemented: "A `CostTable` row missing for a
    /// `HardwareProfile` is filled by scaling the nearest cycle count by clock ratio and
    /// tagged `scaled`, which the manifest records." "Nearest" means nearest clock
    /// frequency among the rows that publish both a clock and a cycle count, with ties
    /// broken by profile id so the choice is deterministic.
    pub fn row_or_scaled(&self, profile: &'static str, target_clock_hz: f64) -> Option<CostRow> {
        if let Some(exact) = self.row(profile) {
            return Some(exact.clone());
        }
        let nearest = self
            .rows
            .iter()
            .filter(|r| r.clock_hz.is_some() && r.has_any_cycles())
            .min_by(|a, b| {
                let da = (a.clock_hz.unwrap_or_default() - target_clock_hz).abs();
                let db = (b.clock_hz.unwrap_or_default() - target_clock_hz).abs();
                // `total_cmp` rather than `partial_cmp`: an equal distance must fall
                // through to the profile-id tie-break rather than to `Ordering::Equal`
                // on a NaN, which would make the choice depend on iteration order.
                da.total_cmp(&db).then_with(|| a.profile.cmp(b.profile))
            })?;
        Some(CostRow {
            profile,
            clock_hz: Some(target_clock_hz),
            site: nearest.site,
            keygen: nearest.keygen.map(|c| scale(c, target_clock_hz)),
            sign: nearest.sign.map(|c| scale(c, target_clock_hz)),
            verify: nearest.verify.map(|c| scale(c, target_clock_hz)),
            source: nearest.source.clone(),
            note: Some("scaled from the nearest published cycle count by clock ratio"),
            scaled: true,
        })
    }
}

impl CostRow {
    /// True when any of the three operations published a cycle count, which is what
    /// [`CostTable::row_or_scaled`] needs to scale from.
    fn has_any_cycles(&self) -> bool {
        self.keygen.and_then(|c| c.cycles).is_some()
            || self.sign.and_then(|c| c.cycles).is_some()
            || self.verify.and_then(|c| c.cycles).is_some()
    }
}

/// Re-express a cost's cycle count at a new clock, dropping the source's own wall time.
fn scale(c: OpCost, target_clock_hz: f64) -> OpCost {
    match c.cycles {
        Some(cycles) => OpCost {
            cycles: Some(cycles),
            mean_us: Some(quantize_to(
                (cycles as f64) / target_clock_hz * 1.0e6,
                COST_US_QUANTUM,
            )),
            p95_us: None,
            ops_per_s: None,
        },
        // Nothing to scale: a throughput figure carries no cycle count, so it is passed
        // through unchanged rather than silently reinterpreted on other silicon.
        None => c,
    }
}

/// One primitive's sizes, level and costs (03-interfaces.md §6).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrimitiveDescriptor {
    /// Stable id.
    pub id: PrimitiveId,
    /// What kind of primitive.
    pub family: PrimitiveFamily,
    /// Public key size in bytes, in its compact form (compressed, for an EC curve).
    pub pk_bytes: u32,
    /// Public key size in its uncompressed form, where the primitive has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pk_bytes_uncompressed: Option<u32>,
    /// Private key size in bytes.
    pub sk_bytes: u32,
    /// Signature (or ciphertext) size.
    pub sig_bytes: SizeSpec,
    /// Signature size *as carried in the IEEE 1609.2 envelope*, where the ASN.1 framing
    /// adds to the raw size.
    ///
    /// Separate from [`PrimitiveDescriptor::sig_bytes`] because both numbers are used and
    /// they differ: ECDSA P-256 is 64 raw bytes but 66 encoded (`Signature` choice tag 1 +
    /// `rSig` as an `EccP256CurvePoint` 1 + 32 + `sSig` 32), and 04-models.md §9.1's
    /// envelope derivation uses the encoded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig_bytes_encoded: Option<u32>,
    /// Shared-secret size, for a KEM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_secret_bytes: Option<u32>,
    /// Encoded certificate size carrying this key under the envelope's profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_bytes: Option<u32>,
    /// NIST security level 1..=5.
    pub security_level: u8,
    /// Cost anchors per hardware profile.
    pub cost: CostTable,
    /// The primitives whose costs add up to this one's, when it is a composite and was
    /// never benchmarked as a unit.
    ///
    /// The hybrids are the case: 04-models.md §9.4 gives sizes for
    /// `hybrid-mldsa44-ecdsa-p256` but no timing, because nobody published a benchmark of
    /// the pair. Signing one is signing twice, so the honest cost is the sum of the two
    /// components' anchors on the same profile — and naming the components makes that a
    /// computation a reader can check rather than a number that appeared.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cost_components: Vec<PrimitiveId>,
    /// The primitive whose cost anchors stand in for this one's, when this one has none
    /// published.
    ///
    /// ECQV is the case: no published reconstruction timing was found, and a
    /// reconstruction is one P-256 point multiplication plus an addition — the dominant
    /// term of an ECDSA verification. Naming the proxy explicitly is honest about where
    /// the number came from; leaving the table empty and letting each caller invent a
    /// fallback would not be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_proxy: Option<PrimitiveId>,
    /// Where the sizes come from.
    pub sources: Vec<Citation>,
    /// Anything that qualifies the entry.
    pub notes: Vec<&'static str>,
}

impl PrimitiveDescriptor {
    /// The cost row for `profile`, following [`PrimitiveDescriptor::cost_proxy`] when this
    /// primitive publishes none of its own.
    ///
    /// Returns a *row*, so it cannot answer for a composite whose cost is a sum of two
    /// rows; [`PrimitiveDescriptor::cost_duration`] is the call that can.
    pub fn cost_row<'a>(
        &'a self,
        profile: &str,
        catalogue: &'a PrimitiveCatalogue,
    ) -> Option<&'a CostRow> {
        if let Some(row) = self.cost.row(profile) {
            return Some(row);
        }
        let proxy = self.cost_proxy?;
        catalogue.get(proxy)?.cost.row(profile)
    }

    /// The modelled duration of `op` on `profile`, through whichever of the three routes
    /// this primitive declares: its own anchors, its declared proxy, or the sum of its
    /// components.
    ///
    /// `None` when none of the three yields a number — which is the right answer, and not
    /// a zero: an unbenchmarked primitive on an unbenchmarked platform has no cost the
    /// simulator is entitled to invent.
    pub fn cost_duration(
        &self,
        op: PrimitiveOpKind,
        profile: &str,
        catalogue: &PrimitiveCatalogue,
    ) -> Option<Duration> {
        if let Some(row) = self.cost.row(profile) {
            return row.duration(op);
        }
        if let Some(proxy) = self.cost_proxy {
            return catalogue.get(proxy)?.cost_duration(op, profile, catalogue);
        }
        if !self.cost_components.is_empty() {
            let mut total = Duration::ZERO;
            for c in &self.cost_components {
                total += catalogue.get(*c)?.cost_duration(op, profile, catalogue)?;
            }
            return Some(total);
        }
        None
    }

    /// The nominal signature size as carried in the 1609.2 envelope.
    pub fn envelope_sig_bytes(&self) -> u32 {
        self.sig_bytes_encoded.unwrap_or(self.sig_bytes.nominal())
    }
}

/// Every primitive descriptor, by id.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PrimitiveCatalogue {
    descriptors: Vec<PrimitiveDescriptor>,
}

/// The catalogue of 04-models.md §9.4.
static STANDARD: LazyLock<PrimitiveCatalogue> = LazyLock::new(build_standard_catalogue);

impl PrimitiveCatalogue {
    /// The standard catalogue: every primitive 04-models.md §9.4 tabulates.
    pub fn standard() -> &'static PrimitiveCatalogue {
        &STANDARD
    }

    /// A catalogue from a list of descriptors, sorted by id.
    pub fn new(mut descriptors: Vec<PrimitiveDescriptor>) -> PrimitiveCatalogue {
        descriptors.sort_by_key(|d| d.id);
        PrimitiveCatalogue { descriptors }
    }

    /// The descriptor for `id`.
    pub fn get(&self, id: PrimitiveId) -> Option<&PrimitiveDescriptor> {
        self.descriptors.iter().find(|d| d.id == id)
    }

    /// The descriptor for `id`, or [`SecError::UnknownPrimitive`].
    pub fn require(&self, id: PrimitiveId) -> Result<&PrimitiveDescriptor> {
        self.get(id)
            .ok_or(SecError::UnknownPrimitive { primitive: id })
    }

    /// Every descriptor, in id order.
    pub fn iter(&self) -> impl Iterator<Item = &PrimitiveDescriptor> {
        self.descriptors.iter()
    }

    /// How many descriptors.
    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }
}

/// A primitive as a registrable model: a descriptor plus its card.
///
/// `Primitive` is the `Family::Primitive` plug-in of 03-interfaces.md §6. It computes
/// nothing; what it *is* is a declared, cited set of sizes and costs, which is exactly
/// what a model card is for, so the card and the descriptor are the whole model.
#[derive(Debug, Clone)]
pub struct Primitive {
    descriptor: PrimitiveDescriptor,
    card: ModelCard,
}

impl Primitive {
    /// Wraps a descriptor, building its card.
    pub fn new(descriptor: PrimitiveDescriptor) -> Primitive {
        let card = card_for(&descriptor);
        Primitive { descriptor, card }
    }

    /// The sizes, level and costs.
    pub fn descriptor(&self) -> &PrimitiveDescriptor {
        &self.descriptor
    }
}

impl Model for Primitive {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

/// One [`Primitive`] model per descriptor in the standard catalogue.
///
/// The list the engine hands to [`v2xw_core::registry::Registry::register_model`], so
/// every primitive the scenario can name has a card and a content hash in the manifest.
pub fn standard_primitive_models() -> Vec<Primitive> {
    PrimitiveCatalogue::standard()
        .iter()
        .cloned()
        .map(Primitive::new)
        .collect()
}

/// The card for a descriptor: its sizes as parameters, its citations as sources.
fn card_for(d: &PrimitiveDescriptor) -> ModelCard {
    let mut card = ModelCard::new(
        d.id.as_str(),
        Family::Primitive,
        "1.0.0",
        format!(
            "Sizes, NIST security level and per-hardware-profile cost anchors for {} \
             (04-models.md §9.4). Declares cost and size; computes no cryptography.",
            d.id.as_str()
        ),
    );
    // Every tier: a size and a cost table are as correct at the abstract tier as at the
    // high tier. What changes with the tier is whether the *backend* runs the primitive,
    // and that is the backend's card, not this one.
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.sources = d.sources.clone();
    card.sources.extend(
        d.cost
            .rows()
            .iter()
            .map(|r| r.source.clone())
            .collect::<Vec<_>>(),
    );
    card.assumptions = d
        .notes
        .iter()
        .map(|n| (*n).to_string())
        .chain(
            d.cost
                .rows()
                .iter()
                .filter_map(|r| r.note.map(|n| format!("{}: {n}", r.profile))),
        )
        .collect();
    card.limitations = vec![
        "Declares sizes and costs only; a `CryptoBackend` decides whether the primitive \
         is actually executed."
            .to_string(),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: d.sources.clone(),
        tests: vec![
            "the_descriptor_sizes_are_the_published_ones".to_string(),
            "every_cost_row_carries_a_citation".to_string(),
        ],
    };
    card
}

// --------------------------------------------------------------------------------------
// The catalogue itself: 04-models.md §9.4, transcribed.
// --------------------------------------------------------------------------------------

/// Hardware-profile ids the cost tables key on.
///
/// These are the *model ids* of `Family::HardwareProfile` models. A scenario declares a
/// hardware profile once and refers to it by a dense [`v2xw_core::ids::HwProfileId`]; the
/// profile model carries this string, and that is the key a cost lookup uses. Two levels
/// of naming rather than one because a cost table is static, cited, cross-scenario data
/// and cannot be keyed by a number a scenario loader happened to assign.
pub mod profiles {
    /// Cortex-M4, Nordic nRF52840 at 64 MHz.
    pub const CORTEX_M4_NRF52840: &str = "hw-profile/cortex-m4-nrf52840-64mhz";
    /// Cortex-M4, STM32F407 at 168 MHz.
    pub const CORTEX_M4_STM32F407: &str = "hw-profile/cortex-m4-stm32f407-168mhz";
    /// Cortex-M4, the `pqm4` `m4f` benchmark platform at 24 MHz.
    pub const CORTEX_M4_PQM4: &str = "hw-profile/cortex-m4-pqm4-m4f-24mhz";
    /// Cortex-M7, STM32F767 at 216 MHz.
    pub const CORTEX_M7_STM32F767: &str = "hw-profile/cortex-m7-stm32f767-216mhz";
    /// Cortex-M33, STM32H563 at 250 MHz, wolfSSL.
    pub const CORTEX_M33_STM32H563_WOLFSSL: &str = "hw-profile/cortex-m33-stm32h563-250mhz-wolfssl";
    /// Cortex-M33, STM32H563 at 250 MHz, mbedTLS.
    pub const CORTEX_M33_STM32H563_MBEDTLS: &str = "hw-profile/cortex-m33-stm32h563-250mhz-mbedtls";
    /// Cortex-A53, Raspberry Pi 3B.
    pub const CORTEX_A53_PI3B: &str = "hw-profile/cortex-a53-pi3b";
    /// Cortex-A53, Raspberry Pi 3B+.
    pub const CORTEX_A53_PI3BPLUS: &str = "hw-profile/cortex-a53-pi3bplus";
    /// Cortex-A72, Raspberry Pi 4, OpenSSL 1.1.1d.
    pub const CORTEX_A72_PI4_OPENSSL: &str = "hw-profile/cortex-a72-pi4-openssl-1.1.1d";
    /// Cortex-A72, Raspberry Pi 4, liboqs.
    pub const CORTEX_A72_PI4_LIBOQS: &str = "hw-profile/cortex-a72-pi4-liboqs";
    /// Cortex-A76, Raspberry Pi 5, wolfSSL 5.9.1.
    pub const CORTEX_A76_PI5_WOLFSSL: &str = "hw-profile/cortex-a76-pi5-wolfssl-5.9.1";
    /// Cortex-A76, Raspberry Pi 5, mbedTLS 3.6.6.
    pub const CORTEX_A76_PI5_MBEDTLS: &str = "hw-profile/cortex-a76-pi5-mbedtls-3.6.6";
    /// Cortex-A76, Raspberry Pi 5, liboqs.
    pub const CORTEX_A76_PI5_LIBOQS: &str = "hw-profile/cortex-a76-pi5-liboqs";
    /// Intel i9-11950H, wolfSSL.
    pub const I9_11950H_WOLFSSL: &str = "hw-profile/i9-11950h-wolfssl";
    /// Intel i9-11950H, mbedTLS.
    pub const I9_11950H_MBEDTLS: &str = "hw-profile/i9-11950h-mbedtls";
    /// Intel i5-8259U at 2.3 GHz.
    pub const I5_8259U: &str = "hw-profile/i5-8259u-2.3ghz";
    /// Skylake, reference C implementation.
    pub const SKYLAKE_REF: &str = "hw-profile/skylake-ref-c";
    /// Skylake, AVX2 implementation.
    pub const SKYLAKE_AVX2: &str = "hw-profile/skylake-avx2";
    /// Skylake at 3.6 GHz, AVX2 (Falcon's own benchmark platform).
    pub const SKYLAKE_AVX2_3600: &str = "hw-profile/skylake-avx2-3.6ghz";
    /// Xeon E3-1220, reference C implementation.
    pub const XEON_E3_1220_REF: &str = "hw-profile/xeon-e3-1220-ref-c";
    /// Xeon E3-1220, AVX2 implementation.
    pub const XEON_E3_1220_AVX2: &str = "hw-profile/xeon-e3-1220-avx2";
    /// Cohda MK6 on-board unit, Botan.
    pub const COHDA_MK6_BOTAN: &str = "hw-profile/cohda-mk6-botan";
    /// Cohda MK6 on-board unit, liboqs.
    pub const COHDA_MK6_LIBOQS: &str = "hw-profile/cohda-mk6-liboqs";
    /// Infineon AURIX TC3xx hardware security module at 100 MHz.
    pub const AURIX_TC3XX_HSM: &str = "hw-profile/aurix-tc3xx-hsm-100mhz";
    /// CAMP's own planning assumption: a 2 GHz processor.
    pub const CAMP_PLANNING_2GHZ: &str = "hw-profile/camp-planning-2ghz";
}

fn std_src(reference: &'static str) -> Citation {
    Source::new(SourceKind::Standard, reference)
}

fn paper(reference: &'static str) -> Citation {
    Source::new(SourceKind::Paper, reference)
}

fn code(reference: &'static str) -> Citation {
    Source::new(SourceKind::Code, reference)
}

fn sheet(reference: &'static str) -> Citation {
    Source::new(SourceKind::Datasheet, reference)
}

/// ECDSA P-256 cost anchors — 04-models.md §9.4, the whole ECDSA block.
fn ecdsa_p256_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::CORTEX_M4_NRF52840,
            CostSite::Cpu,
            code("Emill/P256-Cortex-M4"),
        )
        .at(64.0e6)
        .sign(OpCost::from_cycles_and_us(375_000, 5_900.0))
        .verify(OpCost::from_cycles_and_us(976_000, 15_300.0))
        .keygen(OpCost::from_cycles_and_us(327_000, 5_100.0)),
        CostRow::new(
            p::CORTEX_A72_PI4_OPENSSL,
            CostSite::Cpu,
            code("gist, OpenSSL 1.1.1d on Raspberry Pi 4"),
        )
        .sign(OpCost::from_ops_per_s(4_097.4))
        .verify(OpCost::from_ops_per_s(1_550.7))
        .note("operating system not stated by the source"),
        CostRow::new(p::CORTEX_A53_PI3B, CostSite::Cpu, code("gist [R7 §D8]"))
            .sign(OpCost::from_ops_per_s(1_631.1))
            .verify(OpCost::from_ops_per_s(775.3))
            .note("indicative only"),
        CostRow::new(p::CORTEX_A53_PI3BPLUS, CostSite::Cpu, code("gist [R7 §D8]"))
            .sign(OpCost::from_ops_per_s(1_914.7))
            .verify(OpCost::from_ops_per_s(908.4))
            .note("indicative only"),
        CostRow::new(
            p::CORTEX_A76_PI5_WOLFSSL,
            CostSite::Cpu,
            sheet("wolfSSL benchmark blog, wolfSSL 5.9.1"),
        )
        .verify(OpCost::from_ops_per_s(14_933.0)),
        CostRow::new(
            p::CORTEX_A76_PI5_MBEDTLS,
            CostSite::Cpu,
            sheet("wolfSSL benchmark blog, mbedTLS 3.6.6"),
        )
        .verify(OpCost::from_ops_per_s(592.0)),
        CostRow::new(
            p::I9_11950H_WOLFSSL,
            CostSite::Cpu,
            sheet("wolfSSL benchmark blog"),
        )
        .sign(OpCost::from_ops_per_s(64_194.0))
        .verify(OpCost::from_ops_per_s(61_357.0)),
        CostRow::new(
            p::I9_11950H_MBEDTLS,
            CostSite::Cpu,
            sheet("wolfSSL benchmark blog"),
        )
        .sign(OpCost::from_ops_per_s(4_227.0))
        .verify(OpCost::from_ops_per_s(1_244.0)),
        CostRow::new(
            p::CORTEX_M33_STM32H563_WOLFSSL,
            CostSite::Cpu,
            sheet("wolfSSL benchmark blog"),
        )
        .at(250.0e6)
        .verify(OpCost::from_ops_per_s(167.0))
        .note("the same blog's NUCLEO-F446ZE rows are UNVERIFIED and are not used"),
        CostRow::new(
            p::CORTEX_M33_STM32H563_MBEDTLS,
            CostSite::Cpu,
            sheet("wolfSSL benchmark blog"),
        )
        .at(250.0e6)
        .verify(OpCost::from_ops_per_s(12.1)),
        CostRow::new(
            p::CAMP_PLANNING_2GHZ,
            CostSite::Cpu,
            paper("CAMP EE Requirements 2016 p. 75"),
        )
        .at(2.0e9)
        .sign(OpCost::from_ops_per_s(1_500.0))
        .verify(OpCost::from_ops_per_s(300.0))
        .note("CAMP's own planning assumption, stated as \"about\""),
        CostRow::new(
            p::COHDA_MK6_BOTAN,
            CostSite::Cpu,
            paper("NDSS 2024 Table V"),
        )
        .sign(OpCost::from_us(7_820.0))
        .verify(OpCost::from_us(1.0))
        .note(
            "the published verify figure of 0.001 ms is anomalous — three orders of \
             magnitude faster than the sign on the same silicon — and is recorded as \
             published rather than corrected",
        ),
        CostRow::new(
            p::AURIX_TC3XX_HSM,
            CostSite::Hsm,
            sheet("Infineon AURIX training document [R7 §D]"),
        )
        .at(100.0e6)
        .sign(OpCost::from_ops_per_s(200.0))
        .verify(OpCost::from_ops_per_s(100.0)),
    ])
}

fn ml_dsa_44_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::SKYLAKE_REF,
            CostSite::Cpu,
            paper("CRYSTALS-Dilithium round-3 specification Table 1"),
        )
        .sign(OpCost::from_cycles(1_081_174))
        .verify(OpCost::from_cycles(327_362))
        .keygen(OpCost::from_cycles(300_751))
        .note("Dilithium2; the round-3 sizes differ slightly from FIPS 204 ML-DSA-44"),
        CostRow::new(
            p::SKYLAKE_AVX2,
            CostSite::Cpu,
            paper("CRYSTALS-Dilithium round-3 specification Table 1"),
        )
        .sign(OpCost::from_cycles(259_172))
        .verify(OpCost::from_cycles(118_412))
        .keygen(OpCost::from_cycles(124_031)),
        CostRow::new(
            p::CORTEX_M4_PQM4,
            CostSite::Cpu,
            code("pqm4 benchmarks.csv"),
        )
        .at(24.0e6)
        .sign(OpCost::from_cycles_with_max(3_943_121, 17_009_165, 24.0e6))
        .verify(OpCost::from_cycles(1_421_623))
        .keygen(OpCost::from_cycles(1_426_025))
        .note("sign is a mean over a 1,812,557-17,009,165 cycle spread (rejection sampling)"),
        CostRow::new(
            p::CORTEX_M7_STM32F767,
            CostSite::Cpu,
            paper("NIST 2022 ARM paper Table I"),
        )
        .at(216.0e6)
        .sign(OpCost::from_cycles(3_658_000))
        .verify(OpCost::from_cycles_and_us(1_429_000, 6_600.0))
        .keygen(OpCost::from_cycles(1_437_000)),
        CostRow::new(
            p::CORTEX_A72_PI4_LIBOQS,
            CostSite::Cpu,
            paper("arXiv 2503.10238 Table 8"),
        )
        .sign(OpCost::from_ops_per_s(286.1))
        .verify(OpCost::from_ops_per_s(3_213.9))
        .keygen(OpCost::from_ops_per_s(2_642.7)),
        CostRow::new(
            p::CORTEX_A76_PI5_LIBOQS,
            CostSite::Cpu,
            paper("arXiv 2503.10238 Table 8"),
        )
        .sign(OpCost::from_ops_per_s(1_885.0))
        .verify(OpCost::from_ops_per_s(8_139.0))
        .keygen(OpCost::from_ops_per_s(8_986.3)),
        CostRow::new(
            p::COHDA_MK6_BOTAN,
            CostSite::Cpu,
            paper("NDSS 2024 Table V"),
        )
        .sign(OpCost::from_us(2_634.0))
        .verify(OpCost::from_us(189.0))
        .note("published as \"Dilithium\" without a parameter set; taken as Dilithium2"),
    ])
}

fn ml_dsa_65_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::SKYLAKE_REF,
            CostSite::Cpu,
            paper("CRYSTALS-Dilithium round-3 specification Table 1"),
        )
        .sign(OpCost::from_cycles(1_713_783))
        .verify(OpCost::from_cycles(522_267))
        .keygen(OpCost::from_cycles(544_232)),
        CostRow::new(
            p::SKYLAKE_AVX2,
            CostSite::Cpu,
            paper("CRYSTALS-Dilithium round-3 specification Table 1"),
        )
        .sign(OpCost::from_cycles(428_587))
        .verify(OpCost::from_cycles(179_424))
        .keygen(OpCost::from_cycles(256_403)),
        CostRow::new(
            p::CORTEX_M4_PQM4,
            CostSite::Cpu,
            code("pqm4 benchmarks.csv"),
        )
        .at(24.0e6)
        .sign(OpCost::from_cycles(6_193_171))
        .verify(OpCost::from_cycles(2_415_944))
        .keygen(OpCost::from_cycles(2_516_006)),
        CostRow::new(
            p::CORTEX_M7_STM32F767,
            CostSite::Cpu,
            paper("NIST 2022 ARM paper Table I"),
        )
        .at(216.0e6)
        .sign(OpCost::from_cycles(6_009_000))
        .verify(OpCost::from_cycles_and_us(2_453_000, 11_400.0))
        .keygen(OpCost::from_cycles(2_566_000)),
    ])
}

fn ml_dsa_87_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::SKYLAKE_REF,
            CostSite::Cpu,
            paper("CRYSTALS-Dilithium round-3 specification Table 1"),
        )
        .sign(OpCost::from_cycles(2_383_399))
        .verify(OpCost::from_cycles(871_609))
        .keygen(OpCost::from_cycles(819_475)),
        CostRow::new(
            p::SKYLAKE_AVX2,
            CostSite::Cpu,
            paper("CRYSTALS-Dilithium round-3 specification Table 1"),
        )
        .sign(OpCost::from_cycles(538_986))
        .verify(OpCost::from_cycles(279_936))
        .keygen(OpCost::from_cycles(298_050)),
        CostRow::new(
            p::CORTEX_M4_PQM4,
            CostSite::Cpu,
            code("pqm4 benchmarks.csv"),
        )
        .at(24.0e6)
        .sign(OpCost::from_cycles(7_947_380))
        .verify(OpCost::from_cycles(4_193_104))
        .keygen(OpCost::from_cycles(4_275_859)),
        CostRow::new(
            p::CORTEX_M7_STM32F767,
            CostSite::Cpu,
            paper("NIST 2022 ARM paper Table I"),
        )
        .at(216.0e6)
        .sign(OpCost::from_cycles(8_157_000))
        .verify(OpCost::from_cycles_and_us(4_287_000, 19_800.0))
        .keygen(OpCost::from_cycles(4_368_000)),
    ])
}

fn falcon_512_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::SKYLAKE_AVX2_3600,
            CostSite::Cpu,
            paper("Pornin, ePrint 2019/893 §5.2"),
        )
        .at(3.6e9)
        .sign(OpCost::from_cycles_and_us(948_132, 263.37))
        .verify(OpCost::from_cycles_and_us(81_036, 22.51))
        .keygen(OpCost::from_cycles_and_us(26_604_000, 7_390.0))
        .note("dynamic signing; the expanded-tree variant signs in 467,964 cycles"),
        CostRow::new(p::I5_8259U, CostSite::Cpu, code("falcon-sign.info"))
            .at(2.3e9)
            .sign(OpCost::from_ops_per_s(5_948.1))
            .verify(OpCost::from_ops_per_s(27_933.0))
            .keygen(OpCost::from_us(8_640.0)),
        CostRow::new(
            p::CORTEX_M4_STM32F407,
            CostSite::Cpu,
            paper("Pornin, ePrint 2019/893 §5.3"),
        )
        .at(168.0e6)
        .sign(OpCost::from_cycles_and_us(43_301_915, 257_750.0))
        .verify(OpCost::from_cycles_and_us(504_051, 3_000.0))
        .keygen(OpCost::from_cycles_and_us(171_294_112, 1_019_610.0))
        .note("floating-point emulation; the expanded-tree variant signs in 21,155,551 cycles"),
        CostRow::new(
            p::CORTEX_M7_STM32F767,
            CostSite::Cpu,
            paper("NIST 2022 ARM paper Table II"),
        )
        .at(216.0e6)
        .sign(OpCost::from_cycles_and_us(4_778_000, 22_100.0))
        .verify(OpCost::from_cycles_and_us(559_000, 2_600.0))
        .keygen(OpCost::from_cycles(77_475_000))
        .note("with the hardware FPU"),
        CostRow::new(
            p::CORTEX_A72_PI4_LIBOQS,
            CostSite::Cpu,
            paper("arXiv 2503.10238 Table 8"),
        )
        .sign(OpCost::from_ops_per_s(615.3))
        .verify(OpCost::from_ops_per_s(7_866.3))
        .keygen(OpCost::from_ops_per_s(23.6)),
        CostRow::new(
            p::CORTEX_A76_PI5_LIBOQS,
            CostSite::Cpu,
            paper("arXiv 2503.10238 Table 8"),
        )
        .sign(OpCost::from_ops_per_s(3_360.6))
        .verify(OpCost::from_ops_per_s(19_831.3))
        .keygen(OpCost::from_ops_per_s(95.4)),
        CostRow::new(
            p::COHDA_MK6_LIBOQS,
            CostSite::Cpu,
            paper("NDSS 2024 Table V"),
        )
        .sign(OpCost::from_us(2_152.0))
        .verify(OpCost::from_us(446.0)),
    ])
}

fn falcon_1024_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::SKYLAKE_AVX2_3600,
            CostSite::Cpu,
            paper("Pornin, ePrint 2019/893 §5.2"),
        )
        .at(3.6e9)
        .sign(OpCost::from_cycles(1_926_252))
        .verify(OpCost::from_cycles_and_us(160_596, 44.61))
        .keygen(OpCost::from_cycles(79_164_000))
        .note("dynamic signing; the expanded-tree variant signs in 942,768 cycles"),
        CostRow::new(
            p::CORTEX_M7_STM32F767,
            CostSite::Cpu,
            paper("NIST 2022 ARM paper Table II"),
        )
        .at(216.0e6)
        .sign(OpCost::from_cycles_and_us(10_243_000, 47_400.0))
        .verify(OpCost::from_cycles_and_us(1_136_000, 5_300.0))
        .keygen(OpCost::from_cycles(193_707_000))
        .note("with the hardware FPU"),
    ])
}

fn slh_dsa_128s_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::XEON_E3_1220_REF,
            CostSite::Cpu,
            paper("SPHINCS+ round-3.1 specification Table 4"),
        )
        .sign(OpCost::from_cycles(2_721_595_944))
        .verify(OpCost::from_cycles(2_712_044))
        .keygen(OpCost::from_cycles(358_061_994))
        .note("simple variant"),
        CostRow::new(
            p::XEON_E3_1220_AVX2,
            CostSite::Cpu,
            paper("SPHINCS+ round-3.1 specification Table 6"),
        )
        .sign(OpCost::from_cycles(644_740_090))
        .verify(OpCost::from_cycles(861_478)),
        CostRow::new(
            p::CORTEX_M4_PQM4,
            CostSite::Cpu,
            code("pqm4 benchmarks.csv, `clean` implementation"),
        )
        .at(24.0e6)
        .sign(OpCost::from_cycles(7_657_558_168))
        .verify(OpCost::from_cycles(7_471_794))
        .keygen(OpCost::from_cycles(1_007_731_522)),
        CostRow::new(
            p::COHDA_MK6_LIBOQS,
            CostSite::Cpu,
            paper("NDSS 2024 Table V"),
        )
        .sign(OpCost::from_us(5_485.0))
        .verify(OpCost::from_us(5_436.0))
        .note(
            "published as \"SPHINCS+\" with no parameter set; the 5.485 ms sign is \
             anomalous next to the reference 2.7e9 cycles and is recorded as published \
             rather than corrected",
        ),
    ])
}

fn slh_dsa_128f_costs() -> CostTable {
    use profiles as p;
    CostTable::new(vec![
        CostRow::new(
            p::XEON_E3_1220_REF,
            CostSite::Cpu,
            paper("SPHINCS+ round-3.1 specification Table 4"),
        )
        .sign(OpCost::from_cycles(138_610_500))
        .verify(OpCost::from_cycles(7_757_942))
        .keygen(OpCost::from_cycles(5_590_602))
        .note("simple variant"),
        CostRow::new(
            p::XEON_E3_1220_AVX2,
            CostSite::Cpu,
            paper("SPHINCS+ round-3.1 specification Table 6"),
        )
        .sign(OpCost::from_cycles(33_651_546))
        .verify(OpCost::from_cycles(2_150_290)),
        CostRow::new(
            p::CORTEX_M4_PQM4,
            CostSite::Cpu,
            code("pqm4 benchmarks.csv, `clean` implementation"),
        )
        .at(24.0e6)
        .sign(OpCost::from_cycles(368_575_228))
        .verify(OpCost::from_cycles(21_923_628))
        .keygen(OpCost::from_cycles(15_742_990)),
    ])
}

fn ed25519_costs() -> CostTable {
    CostTable::new(vec![
        CostRow::new(
            profiles::CORTEX_A72_PI4_OPENSSL,
            CostSite::Cpu,
            paper("arXiv 2503.10238 Table 7"),
        )
        .sign(OpCost::from_ops_per_s(2_939.1))
        .verify(OpCost::from_ops_per_s(1_327.0)),
    ])
}

fn sig(
    id: PrimitiveId,
    pk: u32,
    sk: u32,
    sig_bytes: SizeSpec,
    level: u8,
    cost: CostTable,
    sources: Vec<Citation>,
) -> PrimitiveDescriptor {
    PrimitiveDescriptor {
        id,
        family: PrimitiveFamily::Signature,
        pk_bytes: pk,
        pk_bytes_uncompressed: None,
        sk_bytes: sk,
        sig_bytes,
        sig_bytes_encoded: None,
        shared_secret_bytes: None,
        cert_bytes: None,
        security_level: level,
        cost,
        cost_components: Vec::new(),
        cost_proxy: None,
        sources,
        notes: Vec::new(),
    }
}

fn kem(id: PrimitiveId, ek: u32, dk: u32, ct: u32, ss: u32, level: u8) -> PrimitiveDescriptor {
    PrimitiveDescriptor {
        id,
        family: PrimitiveFamily::Kem,
        pk_bytes: ek,
        pk_bytes_uncompressed: None,
        sk_bytes: dk,
        sig_bytes: SizeSpec::Fixed(ct),
        sig_bytes_encoded: None,
        shared_secret_bytes: Some(ss),
        cert_bytes: None,
        security_level: level,
        cost: CostTable::default(),
        cost_components: Vec::new(),
        cost_proxy: None,
        sources: vec![std_src("FIPS 203 Table 3")],
        notes: vec![
            "`sig_bytes` is the ciphertext size; `shared_secret_bytes` is the shared secret.",
            "No cost anchors were found for ML-KEM on any of the profiles in §9.4.",
        ],
    }
}

/// Builds the catalogue of 04-models.md §9.4.
fn build_standard_catalogue() -> PrimitiveCatalogue {
    let mut v: Vec<PrimitiveDescriptor> = Vec::new();

    // --- classical signatures ----------------------------------------------------------
    let mut ecdsa = sig(
        PrimitiveId::ECDSA_P256_SHA256,
        33,
        32,
        SizeSpec::Fixed(64),
        1,
        ecdsa_p256_costs(),
        vec![
            std_src("SEC 1 §2.3.3"),
            std_src("IEEE 1609.2 Ieee1609Dot2BaseTypes.asn"),
            std_src("FIPS 186-4"),
        ],
    );
    ecdsa.pk_bytes_uncompressed = Some(65);
    ecdsa.sig_bytes_encoded = Some(66);
    ecdsa.cert_bytes = Some(147);
    ecdsa.notes = vec![
        "66 encoded bytes under IEEE 1609.2: `Signature` choice tag 1 + `rSig` \
         (`EccP256CurvePoint` choice tag 1 + 32) + `sSig` 32 (04-models.md §9.1).",
        "`cert_bytes` is the DERIVED explicit-certificate size of §9.2 (NDSS's \
         30 + pk + sig accounting gives 162 for the same certificate).",
    ];
    v.push(ecdsa);

    let mut bp256 = sig(
        PrimitiveId::ECDSA_BRAINPOOL_P256R1,
        33,
        32,
        SizeSpec::Fixed(64),
        1,
        CostTable::default(),
        vec![std_src("RFC 5639 §3.4, Annex A")],
    );
    bp256.pk_bytes_uncompressed = Some(65);
    bp256.sig_bytes_encoded = Some(66);
    bp256.cost_proxy = Some(PrimitiveId::ECDSA_P256_SHA256);
    bp256.notes = vec![
        "Cost anchors are the P-256 ones: the two curves have the same field size and the \
         same operation count, and no brainpool-specific figures were found.",
    ];
    v.push(bp256);

    let mut p384 = sig(
        PrimitiveId::ECDSA_P384,
        49,
        48,
        SizeSpec::Fixed(96),
        3,
        CostTable::default(),
        vec![
            std_src("IEEE 1609.2 Ieee1609Dot2BaseTypes.asn"),
            std_src("SEC 1 §2.3.3"),
        ],
    );
    p384.pk_bytes_uncompressed = Some(97);
    p384.sig_bytes_encoded = Some(98);
    p384.notes = vec![
        "Covers brainpoolP384r1 too; 04-models.md §9.4 gives the two the same sizes.",
        "04-models.md §9.4 lists no cost anchor for any P-384 curve on any profile, and \
         none is invented: a P-384 operation is materially slower than a P-256 one, so \
         the P-256 anchors are not an acceptable proxy.",
        "§9.4 states the level as \"192-bit\" rather than as a NIST category; 3 is the \
         category that 192-bit classical strength sits in.",
    ];
    v.push(p384);

    let mut ed = sig(
        PrimitiveId::ED25519,
        32,
        32,
        SizeSpec::Fixed(64),
        1,
        ed25519_costs(),
        vec![std_src("RFC 8032 §7")],
    );
    ed.notes = vec![
        "Legacy stand-in only: the Python reference pipeline signed with Ed25519 for \
         reproducibility, and no IEEE 1609.2 or TS 103 097 profile admits it.",
    ];
    v.push(ed);

    // --- implicit certificates ---------------------------------------------------------
    v.push(PrimitiveDescriptor {
        id: PrimitiveId::ECQV_P256,
        family: PrimitiveFamily::ImplicitCert,
        pk_bytes: 33,
        pk_bytes_uncompressed: Some(65),
        sk_bytes: 32,
        // The "signature" of an implicit certificate scheme is the certificate itself:
        // there is no separate signature, which is the whole point of the construction.
        sig_bytes: SizeSpec::Fixed(80),
        sig_bytes_encoded: None,
        shared_secret_bytes: None,
        cert_bytes: Some(80),
        security_level: 1,
        cost: CostTable::default(),
        cost_components: Vec::new(),
        cost_proxy: Some(PrimitiveId::ECDSA_P256_SHA256),
        sources: vec![std_src("SEC 4 §3.4-3.5"), std_src("SEC 1 §2.3.3")],
        notes: vec![
            "`pk_bytes` 33 is the reconstruction value, a compressed point (§9.2, VERIFIED).",
            "`cert_bytes` 80 is the DERIVED implicit pseudonym certificate size of §9.2; \
             cross-checks are Rostami's 250 - 180 = 70 B delta and VSC-A's 117.",
            "No published ECQV timing was found. Reconstruction is one point \
             multiplication plus one addition — the dominant term of an ECDSA \
             verification — so the ECDSA P-256 verify anchor is the declared cost proxy.",
        ],
    });

    // --- hash --------------------------------------------------------------------------
    v.push(PrimitiveDescriptor {
        id: PrimitiveId::SHA_256,
        family: PrimitiveFamily::Hash,
        pk_bytes: 0,
        pk_bytes_uncompressed: None,
        sk_bytes: 0,
        sig_bytes: SizeSpec::Fixed(32),
        sig_bytes_encoded: None,
        shared_secret_bytes: None,
        cert_bytes: None,
        security_level: 1,
        cost: CostTable::default(),
        cost_components: Vec::new(),
        cost_proxy: None,
        sources: vec![std_src("FIPS 180-4")],
        notes: vec![
            "`sig_bytes` is the digest length. 04-models.md §9.4 lists no cost anchor for \
             SHA-256 on any profile; hashing a V2X-sized message is negligible beside a \
             point multiplication, and the envelope's verify plan charges it as zero \
             until a profile publishes a figure.",
        ],
    });

    // --- ML-DSA (FIPS 204) -------------------------------------------------------------
    for (id, pk, sk, s, level, cost) in [
        (
            PrimitiveId::ML_DSA_44,
            1_312u32,
            2_560u32,
            2_420u32,
            2u8,
            ml_dsa_44_costs(),
        ),
        (
            PrimitiveId::ML_DSA_65,
            1_952,
            4_032,
            3_309,
            3,
            ml_dsa_65_costs(),
        ),
        (
            PrimitiveId::ML_DSA_87,
            2_592,
            4_896,
            4_627,
            5,
            ml_dsa_87_costs(),
        ),
    ] {
        let mut d = sig(
            id,
            pk,
            sk,
            SizeSpec::Fixed(s),
            level,
            cost,
            vec![std_src("FIPS 204 Table 2")],
        );
        d.notes = vec![
            "The private key may instead be stored as a 32-byte seed and expanded \
             (FIPS 204 §3.6), which is what a constrained OBU would do.",
            "Cost rows citing the Dilithium round-3 specification are for the round-3 \
             parameter set, whose sizes differ slightly from the FIPS 204 ones above.",
        ];
        v.push(d);
    }

    // --- Falcon ------------------------------------------------------------------------
    let mut f512 = sig(
        PrimitiveId::FALCON_512,
        897,
        1_281,
        SizeSpec::Variable {
            mean: 666,
            max: 752,
        },
        1,
        falcon_512_costs(),
        vec![
            code("PQClean falcon-padded-512 api.h"),
            code("falcon-sign.info"),
        ],
    );
    f512.notes = vec![
        "666 is the *padded* signature size PQClean's `falcon-padded-512` always emits; \
         the unpadded encoding is variable and bounded by 752.",
        "FIPS 206 draft status is UNVERIFIED.",
    ];
    v.push(f512);

    let mut f1024 = sig(
        PrimitiveId::FALCON_1024,
        1_793,
        2_305,
        SizeSpec::Variable {
            mean: 1_280,
            max: 1_462,
        },
        5,
        falcon_1024_costs(),
        vec![code("PQClean falcon-padded-1024 api.h")],
    );
    f1024.notes =
        vec!["1,280 is the padded size; the unpadded encoding is variable and bounded by 1,462."];
    v.push(f1024);

    // --- SLH-DSA (FIPS 205) ------------------------------------------------------------
    for (id, pk, sk, s, level, cost) in [
        (
            PrimitiveId::SLH_DSA_SHA2_128S,
            32u32,
            64u32,
            7_856u32,
            1u8,
            slh_dsa_128s_costs(),
        ),
        (
            PrimitiveId::SLH_DSA_SHA2_128F,
            32,
            64,
            17_088,
            1,
            slh_dsa_128f_costs(),
        ),
        (
            PrimitiveId::SLH_DSA_SHA2_192S,
            48,
            96,
            16_224,
            3,
            CostTable::default(),
        ),
        (
            PrimitiveId::SLH_DSA_SHA2_192F,
            48,
            96,
            35_664,
            3,
            CostTable::default(),
        ),
        (
            PrimitiveId::SLH_DSA_SHA2_256S,
            64,
            128,
            29_792,
            5,
            CostTable::default(),
        ),
        (
            PrimitiveId::SLH_DSA_SHA2_256F,
            64,
            128,
            49_856,
            5,
            CostTable::default(),
        ),
    ] {
        let mut d = sig(
            id,
            pk,
            sk,
            SizeSpec::Fixed(s),
            level,
            cost,
            vec![std_src("FIPS 205 Table 2")],
        );
        d.notes = vec![
            "The SHAKE parameter set has identical sizes; only the hash family differs.",
            "04-models.md §9.4 publishes cost anchors for the 128s and 128f parameter \
             sets only; the 192 and 256 sets carry no cost anchor and none is inferred, \
             because SLH-DSA's cost scales with its tree parameters and not with its \
             signature size.",
        ];
        v.push(d);
    }

    // --- ML-KEM (FIPS 203) -------------------------------------------------------------
    v.push(kem(PrimitiveId::ML_KEM_512, 800, 1_632, 768, 32, 1));
    v.push(kem(PrimitiveId::ML_KEM_768, 1_184, 2_400, 1_088, 32, 3));
    v.push(kem(PrimitiveId::ML_KEM_1024, 1_568, 3_168, 1_568, 32, 5));

    // --- hybrids -----------------------------------------------------------------------
    let mut h44 = sig(
        PrimitiveId::HYBRID_MLDSA44_ECDSA_P256,
        1_345,
        2_592,
        SizeSpec::Variable {
            mean: 2_484,
            max: 2_492,
        },
        2,
        CostTable::default(),
        vec![
            std_src("FIPS 204 Table 2 + SEC 1 §2.3.3 (raw concatenation, derived)"),
            std_src("draft-ietf-lamps-pq-composite-sigs-19 Appendix A (composite)"),
        ],
    );
    h44.notes = vec![
        "pk 1,345 = 1,312 ML-DSA-44 + 33 compressed P-256, raw; the composite encoding is \
         1,377.",
        "sig mean 2,484 = 2,420 + 64 raw; the composite maximum is 2,492.",
        "sk 2,592 = 2,560 + 32, derived.",
    ];
    h44.cost_components = vec![PrimitiveId::ML_DSA_44, PrimitiveId::ECDSA_P256_SHA256];
    h44.notes.push(
        "No benchmark of the hybrid as a unit was found. Signing or verifying one \
         means doing both halves, so its cost is the sum of the ML-DSA-44 and \
         ECDSA P-256 anchors on the same hardware profile.",
    );
    v.push(h44);

    let mut h65 = sig(
        PrimitiveId::HYBRID_MLDSA65_ECDSA_P256,
        2_017,
        83,
        SizeSpec::Variable {
            mean: 3_373,
            max: 3_381,
        },
        3,
        CostTable::default(),
        vec![std_src("draft-ietf-lamps-pq-composite-sigs-19")],
    );
    h65.notes = vec![
        "The composite figures, as §9.4 gives them: pk 2,017 composite, sk 83, sig maximum \
         3,381. The mean is the raw 3,309 + 64 = 3,373 concatenation.",
        "sk 83 is the composite *seed* encoding, not an expanded key.",
    ];
    h65.cost_components = vec![PrimitiveId::ML_DSA_65, PrimitiveId::ECDSA_P256_SHA256];
    h65.notes.push(
        "No benchmark of the hybrid as a unit was found. Signing or verifying one \
         means doing both halves, so its cost is the sum of the ML-DSA-65 and \
         ECDSA P-256 anchors on the same hardware profile.",
    );
    v.push(h65);

    let mut hf = sig(
        PrimitiveId::HYBRID_FALCON512_ECDSA_P256,
        930,
        1_313,
        SizeSpec::Variable {
            mean: 730,
            max: 816,
        },
        1,
        CostTable::default(),
        vec![Source::new(
            SourceKind::Paper,
            "derived from PQClean falcon-padded-512 + SEC 1",
        )],
    );
    hf.notes = vec![
        "pk 930 = 897 + 33 raw and sig 730 = 666 + 64 raw, as §9.4 derives them; the \
         maximum follows from Falcon's unpadded 752 + 64.",
        "sk 1,313 = 1,281 + 32, derived.",
    ];
    hf.cost_components = vec![PrimitiveId::FALCON_512, PrimitiveId::ECDSA_P256_SHA256];
    hf.notes.push(
        "No benchmark of the hybrid as a unit was found. Signing or verifying one \
         means doing both halves, so its cost is the sum of the Falcon-512 and \
         ECDSA P-256 anchors on the same hardware profile.",
    );
    v.push(hf);

    PrimitiveCatalogue::new(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sizes 04-models.md §9.4 publishes, spot-checked against the catalogue. These
    /// numbers drive every channel-load and certificate-size result the simulator
    /// produces, so they are pinned here rather than trusted to a transcription.
    #[test]
    fn the_descriptor_sizes_are_the_published_ones() {
        let c = PrimitiveCatalogue::standard();
        let e = c.require(PrimitiveId::ECDSA_P256_SHA256).expect("present");
        assert_eq!(e.pk_bytes, 33);
        assert_eq!(e.pk_bytes_uncompressed, Some(65));
        assert_eq!(e.sk_bytes, 32);
        assert_eq!(e.sig_bytes, SizeSpec::Fixed(64));
        assert_eq!(e.sig_bytes_encoded, Some(66));
        assert_eq!(e.security_level, 1);

        for (id, pk, sk, s) in [
            (PrimitiveId::ML_DSA_44, 1_312, 2_560, 2_420),
            (PrimitiveId::ML_DSA_65, 1_952, 4_032, 3_309),
            (PrimitiveId::ML_DSA_87, 2_592, 4_896, 4_627),
        ] {
            let d = c.require(id).expect("present");
            assert_eq!((d.pk_bytes, d.sk_bytes), (pk, sk), "{id}");
            assert_eq!(d.sig_bytes, SizeSpec::Fixed(s), "{id}");
        }

        let f = c.require(PrimitiveId::FALCON_512).expect("present");
        assert_eq!(f.pk_bytes, 897);
        assert_eq!(f.sk_bytes, 1_281);
        assert_eq!(
            f.sig_bytes,
            SizeSpec::Variable {
                mean: 666,
                max: 752
            }
        );
        assert_eq!(f.sig_bytes.nominal(), 666);
        assert_eq!(f.sig_bytes.max(), 752);

        let s = c.require(PrimitiveId::SLH_DSA_SHA2_128S).expect("present");
        assert_eq!((s.pk_bytes, s.sk_bytes), (32, 64));
        assert_eq!(s.sig_bytes, SizeSpec::Fixed(7_856));
        let f128 = c.require(PrimitiveId::SLH_DSA_SHA2_128F).expect("present");
        assert_eq!(f128.sig_bytes, SizeSpec::Fixed(17_088));

        let k = c.require(PrimitiveId::ML_KEM_768).expect("present");
        assert_eq!((k.pk_bytes, k.sk_bytes), (1_184, 2_400));
        assert_eq!(k.sig_bytes, SizeSpec::Fixed(1_088));
        assert_eq!(k.shared_secret_bytes, Some(32));

        let q = c.require(PrimitiveId::ECQV_P256).expect("present");
        assert_eq!(
            q.pk_bytes, 33,
            "the reconstruction value is a compressed point"
        );
        assert_eq!(q.cert_bytes, Some(80));
        assert_eq!(q.family, PrimitiveFamily::ImplicitCert);

        let h = c
            .require(PrimitiveId::HYBRID_MLDSA44_ECDSA_P256)
            .expect("present");
        assert_eq!(h.pk_bytes, 1_345, "1,312 + 33 raw");
        assert_eq!(h.sig_bytes.nominal(), 2_484, "2,420 + 64 raw");
        assert_eq!(h.sig_bytes.max(), 2_492, "composite maximum");
    }

    /// No black box: a cost row without a citation is a number nobody can check.
    #[test]
    fn every_cost_row_carries_a_citation() {
        for d in PrimitiveCatalogue::standard().iter() {
            for row in d.cost.rows() {
                assert!(
                    !row.source.reference.trim().is_empty(),
                    "{} / {} has an empty citation",
                    d.id,
                    row.profile
                );
                assert!(
                    row.keygen.is_some() || row.sign.is_some() || row.verify.is_some(),
                    "{} / {} has a row with no costs at all",
                    d.id,
                    row.profile
                );
                assert!(!row.scaled, "a published row must not be tagged scaled");
            }
        }
    }

    /// The two figures that are implausible as published must stay flagged. A future
    /// editor who "tidies" one of them away removes the only warning a reader gets.
    #[test]
    fn the_anomalous_published_figures_stay_flagged() {
        let c = PrimitiveCatalogue::standard();
        let cohda = c
            .require(PrimitiveId::ECDSA_P256_SHA256)
            .expect("present")
            .cost
            .row(profiles::COHDA_MK6_BOTAN)
            .expect("the Cohda row is present");
        assert_eq!(cohda.verify.expect("verify").mean_us, Some(1.0));
        assert!(
            cohda.note.expect("a note").contains("anomalous"),
            "the 0.001 ms verify must stay flagged"
        );
        let sphincs = c
            .require(PrimitiveId::SLH_DSA_SHA2_128S)
            .expect("present")
            .cost
            .row(profiles::COHDA_MK6_LIBOQS)
            .expect("the Cohda row is present");
        assert!(sphincs.note.expect("a note").contains("anomalous"));
    }

    /// A cost is charged as an integer duration, whichever form the source published it
    /// in. Cycles need a clock; a throughput does not.
    #[test]
    fn a_cost_becomes_a_duration_whatever_form_it_was_published_in() {
        let c = PrimitiveCatalogue::standard();
        let ecdsa = c.require(PrimitiveId::ECDSA_P256_SHA256).expect("present");

        // Published as cycles *and* milliseconds: the published wall time wins.
        let m4 = ecdsa
            .cost
            .row(profiles::CORTEX_M4_NRF52840)
            .expect("present");
        assert_eq!(
            m4.duration(PrimitiveOpKind::Verify),
            Some(Duration::from_micros(15_300))
        );

        // Published as operations per second: 10^6 / 4097.4 = 244.0567 µs, which
        // quantises to 244.057 µs on the nanosecond grid.
        let pi4 = ecdsa
            .cost
            .row(profiles::CORTEX_A72_PI4_OPENSSL)
            .expect("present");
        assert_eq!(
            pi4.duration(PrimitiveOpKind::Sign),
            Some(Duration::from_nanos(244_057))
        );

        // Published as cycles only, with a clock: 259,172 cycles has no clock on the
        // Skylake AVX2 row, so no duration can be derived from it.
        let avx2 = c
            .require(PrimitiveId::ML_DSA_44)
            .expect("present")
            .cost
            .row(profiles::SKYLAKE_AVX2)
            .expect("present");
        assert_eq!(avx2.sign.expect("sign").cycles, Some(259_172));
        assert_eq!(avx2.duration(PrimitiveOpKind::Sign), None);

        // Published as cycles with a clock: pqm4 at 24 MHz.
        let pqm4 = c
            .require(PrimitiveId::ML_DSA_44)
            .expect("present")
            .cost
            .row(profiles::CORTEX_M4_PQM4)
            .expect("present");
        // 1,421,623 / 24e6 s = 59.234 ms.
        assert_eq!(
            pqm4.duration(PrimitiveOpKind::Verify),
            Some(Duration::from_nanos(59_234_292))
        );
    }

    /// A profile with no published row is filled by scaling the nearest cycle count, and
    /// the result says so.
    #[test]
    fn a_missing_profile_is_filled_by_scaling_and_tagged() {
        let c = PrimitiveCatalogue::standard();
        let d = c.require(PrimitiveId::ML_DSA_44).expect("present");
        assert!(d.cost.row("hw-profile/invented-600mhz").is_none());
        let scaled = d
            .cost
            .row_or_scaled("hw-profile/invented-600mhz", 600.0e6)
            .expect("a cycle-count row exists to scale from");
        assert!(scaled.scaled);
        assert_eq!(scaled.clock_hz, Some(600.0e6));
        assert!(scaled.note.expect("a note").contains("scaled"));
        // The nearest clocked row to 600 MHz among ML-DSA-44's rows is the 216 MHz
        // Cortex-M7 (the other clocked row is 24 MHz), whose verify is 1,429,000 cycles.
        // At 600 MHz that is 2,381.667 µs.
        assert_eq!(
            scaled.verify.expect("verify").cycles,
            Some(1_429_000),
            "the cycle count is carried over unchanged"
        );
        assert_eq!(
            scaled.duration(PrimitiveOpKind::Verify),
            Some(Duration::from_nanos(2_381_667))
        );
        // An exact hit is returned unscaled.
        let exact = d
            .cost
            .row_or_scaled(profiles::CORTEX_M4_PQM4, 24.0e6)
            .expect("present");
        assert!(!exact.scaled);
    }

    /// A primitive with no anchors of its own resolves through its declared proxy, and a
    /// proxy is never invented silently.
    #[test]
    fn a_cost_proxy_resolves_and_is_declared() {
        let c = PrimitiveCatalogue::standard();
        let ecqv = c.require(PrimitiveId::ECQV_P256).expect("present");
        assert!(ecqv.cost.is_empty());
        assert_eq!(ecqv.cost_proxy, Some(PrimitiveId::ECDSA_P256_SHA256));
        let row = ecqv
            .cost_row(profiles::CORTEX_M4_NRF52840, c)
            .expect("resolves through the proxy");
        assert_eq!(row.verify.expect("verify").cycles, Some(976_000));
        // Every primitive with an empty table either declares a proxy, declares the
        // components its cost is the sum of, or says in its notes that no anchor exists.
        // The point of the assertion is that the third case is *stated*: a silent zero or
        // a silently borrowed number is what "no black box" forbids.
        for d in c.iter() {
            if d.cost.is_empty() && d.cost_proxy.is_none() && d.cost_components.is_empty() {
                assert!(
                    d.notes.iter().any(|n| n.to_lowercase().contains("no cost")
                        || n.to_lowercase().contains("lists no cost")),
                    "{} has neither anchors, a proxy, components, nor a note saying why",
                    d.id
                );
            }
        }

        // A hybrid's cost is the sum of its components', and the sum is arithmetic a
        // reader can redo. The Cohda MK6 is the one profile in §9.4 that was benchmarked
        // for both an ECDSA and an ML-DSA implementation, so it is the only profile where
        // the sum is entirely made of published numbers.
        let hybrid = c
            .require(PrimitiveId::HYBRID_MLDSA44_ECDSA_P256)
            .expect("present");
        let profile = profiles::COHDA_MK6_BOTAN;
        let parts: Duration = [PrimitiveId::ML_DSA_44, PrimitiveId::ECDSA_P256_SHA256]
            .iter()
            .map(|p| {
                c.require(*p)
                    .expect("present")
                    .cost_duration(PrimitiveOpKind::Verify, profile, c)
                    .expect("both components have a Cohda MK6 anchor")
            })
            .fold(Duration::ZERO, |a, b| a + b);
        // 189 µs for Dilithium plus the (anomalous, as published) 1 µs for ECDSA.
        assert_eq!(parts, Duration::from_micros(190));
        assert_eq!(
            hybrid.cost_duration(PrimitiveOpKind::Verify, profile, c),
            Some(parts)
        );

        // A composite whose components are not *both* benchmarked on a profile yields
        // nothing rather than a half sum: ML-DSA-44 has a Cortex-M7 anchor and ECDSA
        // P-256 does not, so the hybrid has no Cortex-M7 cost. Understating a hybrid by
        // the cost of its classical half is exactly the error a partial sum would make.
        assert!(
            c.require(PrimitiveId::ML_DSA_44)
                .expect("present")
                .cost_duration(PrimitiveOpKind::Verify, profiles::CORTEX_M7_STM32F767, c)
                .is_some()
        );
        assert_eq!(
            hybrid.cost_duration(PrimitiveOpKind::Verify, profiles::CORTEX_M7_STM32F767, c),
            None
        );

        // And a primitive nothing was benchmarked for yields nothing, not zero.
        assert_eq!(
            c.require(PrimitiveId::ECDSA_P384)
                .expect("present")
                .cost_duration(PrimitiveOpKind::Verify, profiles::CORTEX_M7_STM32F767, c),
            None
        );
    }

    /// Every primitive is a registrable model with a card that validates.
    #[test]
    fn every_primitive_has_a_card_that_validates() {
        let models = standard_primitive_models();
        assert_eq!(models.len(), PrimitiveCatalogue::standard().len());
        assert!(models.len() >= 23, "the §9.4 table has 23 rows at least");
        for m in &models {
            m.card().validate().unwrap_or_else(|e| {
                panic!("{}: {e}", m.id());
            });
            assert_eq!(m.family(), Family::Primitive);
            assert!(!m.card().sources.is_empty(), "{}: no source", m.id());
            assert!(m.implements_tier(Tier::Medium));
        }
    }

    /// Cost tables and the catalogue iterate in a fixed order, because a report, a card
    /// payload and a manifest digest all walk them (02-architecture.md §6.4).
    #[test]
    fn the_catalogue_and_its_tables_iterate_in_id_order() {
        let c = PrimitiveCatalogue::standard();
        let ids: Vec<&str> = c.iter().map(|d| d.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
        for d in c.iter() {
            let profiles: Vec<&str> = d.cost.rows().iter().map(|r| r.profile).collect();
            let mut s = profiles.clone();
            s.sort_unstable();
            assert_eq!(profiles, s, "{}", d.id);
        }
    }

    /// Every cost value sits on the nanosecond grid build decision D9 requires of any
    /// number that reaches a recorded artefact.
    #[test]
    fn every_cost_value_is_quantised() {
        for d in PrimitiveCatalogue::standard().iter() {
            for row in d.cost.rows() {
                for (name, op) in [
                    ("keygen", row.keygen),
                    ("sign", row.sign),
                    ("verify", row.verify),
                ] {
                    let Some(op) = op else { continue };
                    for (field, value) in [("mean_us", op.mean_us), ("p95_us", op.p95_us)] {
                        let Some(v) = value else { continue };
                        assert!(
                            v2xw_core::math::is_on_grid(v, COST_US_QUANTUM),
                            "{} / {} / {name}.{field} = {v} is off the {COST_US_QUANTUM} µs grid",
                            d.id,
                            row.profile
                        );
                    }
                }
            }
        }
    }
}
