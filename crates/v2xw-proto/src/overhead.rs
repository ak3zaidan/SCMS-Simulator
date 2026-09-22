//! What security costs per message: bytes, and the fraction of airtime they buy nothing.
//!
//! This is one of the numbers the simulator exists to produce, so every component of it is
//! either a real encoder's output or a cited constant, and the module says which for each.
//!
//! # The decomposition
//!
//! A signed V2X message on the air is three things:
//!
//! | Part | Bytes | Where the number comes from |
//! |---|---|---|
//! | application payload | [`Payload::bytes`] | the real UPER encoder in `v2xw-msg` |
//! | `Ieee1609Dot2Data` overhead common to both signer choices | [`ENVELOPE_COMMON_BYTES`] | 04-models.md §9.1, measured by `v2xw-sec`'s `tests/overhead.rs` |
//! | the signer identifier | [`SIGNER_DIGEST_BYTES`] or [`SIGNER_CERT_PREFIX_BYTES`] + the certificate | ditto; the certificate is the real COER encoder's length |
//!
//! The two published envelope overheads — 93 bytes with a digest signer, 87 plus the
//! certificate with a certificate signer — are *not* independent constants, and treating
//! them as such is how a decomposition ends up not adding up. They share a common part:
//! `93 = 84 + 9` and `87 = 84 + 3`, where 84 is everything that does not depend on the
//! signer choice, 9 is `choice tag + HashedId8` and 3 is `choice tag + SequenceOfCertificate
//! quantity`. [`ENVELOPE_COMMON_BYTES`] is that 84, and
//! `the_envelope_decomposition_reproduces_both_measured_overheads` pins the two identities.
//!
//! # The airtime fraction
//!
//! Airtime is the radio's answer, not this crate's: `v2xw_radio::phy::air_time` computes it
//! from the OFDM timing of EN 302 663 Annex C. [`AirInterface`] declares the same timing
//! with the same citations so that a security overhead can be turned into microseconds
//! without this crate depending on the radio stack, and
//! `airtime_matches_the_frames_a_real_run_put_on_the_air` pins it against frames a real
//! engine run actually transmitted. If the two ever disagree, that test fails.
//!
//! The fraction is **not** `security_bytes / total_bytes`. Airtime is a step function of
//! bytes: a frame occupies a whole number of OFDM symbols, and 48 bits of payload at
//! 6 Mbit/s buy the same symbol as 1 bit. So the honest question is "how much airtime does
//! the same traffic take with the envelope and without it", which is what
//! [`OverheadProfile::airtime_fraction_ppm`] computes — two airtimes, differenced.
//!
//! **Determinism.** Integer arithmetic throughout: bytes are `u32`, airtime is integer
//! nanoseconds, and the fraction is reported in parts per million as a `u32` rather than as
//! a float, so nothing here needs quantising on the way out.

use v2xw_core::time::{Duration, SimTime};
use v2xw_sec::envelope::{SignerIdChoice, SignerIdPolicy};

use crate::error::Result;
use crate::sizes::{
    CertificateSizes, ENVELOPE_OVERHEAD_CERT_BYTES, ENVELOPE_OVERHEAD_DIGEST_BYTES, WireSize,
};

/// The `Ieee1609Dot2Data`/`SignedData` overhead that does not depend on the signer choice.
///
/// 84 bytes: the outer `Ieee1609Dot2Data` and `hashId`, the `SignedDataPayload` preamble
/// and its inner data, the `HeaderInfo` preamble with `psid` and `generationTime`, and the
/// 66-byte ECDSA P-256 signature. Derived from 04-models.md §9.1's field-by-field table as
/// `ENVELOPE_OVERHEAD_DIGEST_BYTES − SIGNER_DIGEST_BYTES`, both of which `v2xw-sec`'s
/// `tests/overhead.rs` measures against the real COER encoder as equalities.
pub const ENVELOPE_COMMON_BYTES: u32 = ENVELOPE_OVERHEAD_DIGEST_BYTES - SIGNER_DIGEST_BYTES;

/// `signer = digest`: the CHOICE tag plus a `HashedId8`. [IEEE 1609.2 §6.3.26.]
pub const SIGNER_DIGEST_BYTES: u32 = 9;

/// `signer = certificate`: the CHOICE tag plus the `SequenceOfCertificate` quantity, before
/// the certificate itself. [IEEE 1609.2 §6.3.4; 04-models.md §9.1.]
pub const SIGNER_CERT_PREFIX_BYTES: u32 = ENVELOPE_OVERHEAD_CERT_BYTES - ENVELOPE_COMMON_BYTES;

/// An application payload, with the encoder that produced its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Payload {
    /// The message type, as `node.tx` spells it.
    pub msg_type: &'static str,
    /// Its encoded length, and where that length came from.
    pub bytes: WireSize,
}

impl Payload {
    /// The representative payloads this crate reports overhead against.
    ///
    /// Both are the real UPER encoding of a real message built from a real belief — the
    /// Phase 1 Manhattan origin, a passenger car at 50 km/h — and not a nominal size. A
    /// BSM's length varies with its optional Part II content and a CAM's with its
    /// low-frequency container, so these are *one* representative message each and the
    /// overhead below is reported per message rather than as a percentage of "a BSM".
    ///
    /// # Errors
    /// [`crate::ProtoError::Size`] if an encoder refuses, which would be a defect in the
    /// message profile rather than a runtime condition.
    pub fn representative() -> Result<Vec<Payload>> {
        Ok(vec![
            Payload {
                msg_type: "bsm",
                bytes: WireSize::real(
                    crate::sizes::representative_bsm_bytes()?,
                    "SAE J2735 BasicSafetyMessage, Part I only, UPER",
                ),
            },
            Payload {
                msg_type: "cam",
                bytes: WireSize::real(
                    crate::sizes::representative_cam_bytes()?,
                    "ETSI EN 302 637-2 CAM, basic vehicle container, UPER",
                ),
            },
        ])
    }
}

/// The 10 MHz OFDM air interface's timing, for turning bytes into microseconds.
///
/// Every field cites EN 302 663 V1.3.1 Annex C, and the arithmetic is the same as
/// `v2xw_radio::phy::air_time`: `preamble + signal + symbol · ceil((N_service + 8·bytes +
/// N_tail) / data_bits_per_symbol)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AirInterface {
    /// Data bits per OFDM symbol at the modulation and coding scheme in use.
    pub data_bits_per_symbol: u32,
    /// PLCP preamble, 32 µs [EN 302 663 Table C.2].
    pub preamble: Duration,
    /// SIGNAL field, 8 µs, always BPSK 1/2 [EN 302 663 Annex C.3].
    pub signal: Duration,
    /// One OFDM symbol, 8 µs half-clocked [EN 302 663 Annex C.3].
    pub symbol: Duration,
    /// `N_SERVICE`, the 16 SERVICE bits prepended to the PSDU.
    pub n_service: u32,
    /// `N_TAIL`, the 6 tail bits appended to it.
    pub n_tail: u32,
}

impl AirInterface {
    /// QPSK rate 1/2, 6 Mbit/s, 48 data bits per symbol.
    ///
    /// The mode the Phase 1 scenario's medium PHY transmits at, and the one every DSRC
    /// deployment's safety channel uses [EN 302 663 V1.3.1 Annex C.3 Table C.1].
    pub const DSRC_6MBPS: AirInterface = AirInterface {
        data_bits_per_symbol: 48,
        preamble: Duration::from_micros(32),
        signal: Duration::from_micros(8),
        symbol: Duration::from_micros(8),
        n_service: 16,
        n_tail: 6,
    };

    /// How many OFDM data symbols a frame of `bytes` occupies.
    pub const fn symbols(&self, bytes: u32) -> u64 {
        let bits = self.n_service as u64 + 8 * bytes as u64 + self.n_tail as u64;
        let dbps = if self.data_bits_per_symbol == 0 {
            1
        } else {
            self.data_bits_per_symbol as u64
        };
        bits.div_ceil(dbps)
    }

    /// The airtime a frame of `bytes` occupies.
    pub const fn airtime(&self, bytes: u32) -> Duration {
        Duration::from_nanos(
            self.preamble.as_nanos()
                + self.signal.as_nanos()
                + self.symbol.as_nanos() * self.symbols(bytes),
        )
    }
}

/// One message's security overhead, decomposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct MessageOverhead {
    /// The message type.
    pub msg_type: &'static str,
    /// Which signer identifier this message carried.
    pub signer: SignerIdChoice,
    /// The application payload.
    pub payload_bytes: u32,
    /// The envelope, excluding the signer identifier.
    pub envelope_bytes: u32,
    /// The signer identifier, excluding the certificate.
    pub signer_id_bytes: u32,
    /// The certificate, where one is attached.
    pub certificate_bytes: u32,
}

impl MessageOverhead {
    /// The whole frame.
    pub const fn total_bytes(&self) -> u32 {
        self.payload_bytes + self.security_bytes()
    }

    /// Everything that is not application payload.
    pub const fn security_bytes(&self) -> u32 {
        self.envelope_bytes + self.signer_id_bytes + self.certificate_bytes
    }

    /// The airtime this frame occupies.
    pub const fn airtime(&self, air: &AirInterface) -> Duration {
        air.airtime(self.total_bytes())
    }

    /// The airtime the same payload would occupy unsigned.
    pub const fn airtime_unsigned(&self, air: &AirInterface) -> Duration {
        air.airtime(self.payload_bytes)
    }
}

/// The overhead of one message type under one signer-identifier policy.
///
/// Holds the two per-message cases and the cadence between them, because that is what a
/// policy *is*: J2945/1's rule is not "a certificate" or "a digest" but "a certificate
/// once a second and a digest the other nine times", and the mean overhead is the only
/// number that answers "what does security cost per message" for such a policy.
#[derive(Debug, Clone, PartialEq)]
pub struct OverheadProfile {
    /// The policy's name, as a scenario or a standard spells it.
    pub policy: &'static str,
    /// The message type.
    pub msg_type: &'static str,
    /// The message generation period.
    pub period: Duration,
    /// How many of `window` messages carried a certificate.
    pub with_certificate: u32,
    /// How many messages the cadence was measured over.
    pub window: u32,
    /// The digest-signer case.
    pub digest: MessageOverhead,
    /// The certificate-signer case.
    pub certificate: MessageOverhead,
}

impl OverheadProfile {
    /// The security bytes per message, averaged over the policy's cadence, in **millibytes**.
    ///
    /// Thousandths of a byte rather than a float, so the number a report prints is the
    /// number this function returned and there is nothing to quantise. Divide by 1,000 for
    /// bytes.
    pub const fn mean_security_millibytes(&self) -> u64 {
        let n = if self.window == 0 {
            1
        } else {
            self.window as u64
        };
        let certs = self.with_certificate as u64;
        let digests = n - certs;
        (certs * self.certificate.security_bytes() as u64
            + digests * self.digest.security_bytes() as u64)
            * 1_000
            / n
    }

    /// The same for the whole frame.
    pub const fn mean_total_millibytes(&self) -> u64 {
        let n = if self.window == 0 {
            1
        } else {
            self.window as u64
        };
        let certs = self.with_certificate as u64;
        let digests = n - certs;
        (certs * self.certificate.total_bytes() as u64 + digests * self.digest.total_bytes() as u64)
            * 1_000
            / n
    }

    /// The airtime the policy's traffic occupies over `window` messages.
    pub const fn airtime(&self, air: &AirInterface) -> Duration {
        let certs = self.with_certificate as u64;
        let digests = self.window as u64 - certs;
        Duration::from_nanos(
            certs * self.certificate.airtime(air).as_nanos()
                + digests * self.digest.airtime(air).as_nanos(),
        )
    }

    /// The airtime the same payloads would occupy unsigned.
    pub const fn airtime_unsigned(&self, air: &AirInterface) -> Duration {
        Duration::from_nanos(self.window as u64 * self.digest.airtime_unsigned(air).as_nanos())
    }

    /// The fraction of occupied airtime that security accounts for, in parts per million.
    ///
    /// `(signed − unsigned) / signed`, over the policy's whole cadence window. Parts per
    /// million because airtime is a step function of bytes and the difference between two
    /// step functions deserves better resolution than a percentage: 0.1 % is 1,000 ppm.
    pub const fn airtime_fraction_ppm(&self, air: &AirInterface) -> u32 {
        let signed = self.airtime(air).as_nanos();
        if signed == 0 {
            return 0;
        }
        let unsigned = self.airtime_unsigned(air).as_nanos();
        let saved = signed.saturating_sub(unsigned);
        let ppm = saved.saturating_mul(1_000_000) / signed;
        if ppm > u32::MAX as u64 {
            u32::MAX
        } else {
            ppm as u32
        }
    }

    /// How much of the channel one vehicle's traffic occupies, in parts per million.
    ///
    /// The other half of "what does security cost": a fraction of the *channel*, not of
    /// the frame. One node at `period` occupies `airtime(one message) / period` of the
    /// single 10 MHz safety channel, and the question a deployment asks is how many
    /// vehicles fit before the channel is full.
    pub const fn channel_load_ppm(&self, air: &AirInterface) -> u32 {
        let window_ns = self.period.as_nanos().saturating_mul(self.window as u64);
        if window_ns == 0 {
            return 0;
        }
        let ppm = self.airtime(air).as_nanos().saturating_mul(1_000_000) / window_ns;
        if ppm > u32::MAX as u64 {
            u32::MAX
        } else {
            ppm as u32
        }
    }
}

/// The named signer-identifier policies this crate reports against.
///
/// The four `v2xw_sec::envelope::SignerIdPolicy` constants, by the names the sources give
/// them, so a report's rows and the envelope's behaviour cannot drift apart.
pub const POLICIES: [(&str, SignerIdPolicy); 4] = [
    ("digest-only", SignerIdPolicy::DIGEST_ONLY),
    ("always-certificate", SignerIdPolicy::ALWAYS_CERTIFICATE),
    ("sae-450ms", SignerIdPolicy::SAE_450MS),
    ("etsi-1s", SignerIdPolicy::ETSI_1S),
];

/// Builds the overhead profile of `payload` under `policy` at a `period` generation rate.
///
/// The cadence is obtained by *asking the policy*, message by message, over `window`
/// messages — `SignerIdPolicy::choose` is the same function the envelope calls on the hot
/// path — rather than by dividing one interval by another. The difference is not
/// cosmetic: the first message a signer sends always carries the certificate whatever the
/// interval says, and a division would miss it.
pub fn profile(
    policy_name: &'static str,
    policy: SignerIdPolicy,
    payload: Payload,
    certs: &CertificateSizes,
    period: Duration,
    window: u32,
) -> OverheadProfile {
    let payload_bytes = payload.bytes.bytes();
    let cert_bytes = certs.pseudonym.bytes();
    let digest = MessageOverhead {
        msg_type: payload.msg_type,
        signer: SignerIdChoice::Digest,
        payload_bytes,
        envelope_bytes: ENVELOPE_COMMON_BYTES,
        signer_id_bytes: SIGNER_DIGEST_BYTES,
        certificate_bytes: 0,
    };
    let certificate = MessageOverhead {
        msg_type: payload.msg_type,
        signer: SignerIdChoice::Certificate,
        payload_bytes,
        envelope_bytes: ENVELOPE_COMMON_BYTES,
        signer_id_bytes: SIGNER_CERT_PREFIX_BYTES,
        certificate_bytes: cert_bytes,
    };

    let mut last: Option<SimTime> = None;
    let mut with_certificate = 0;
    for k in 0..window {
        let now = period.saturating_mul(u64::from(k)).after(0);
        if policy.choose(now, last) == SignerIdChoice::Certificate {
            with_certificate += 1;
            last = Some(now);
        }
    }

    OverheadProfile {
        policy: policy_name,
        msg_type: payload.msg_type,
        period,
        with_certificate,
        window,
        digest,
        certificate,
    }
}

/// Every policy against every representative payload, at `period`.
///
/// # Errors
/// [`crate::ProtoError::Size`] if a payload or the certificate profile does not encode.
pub fn table(period: Duration, window: u32) -> Result<Vec<OverheadProfile>> {
    let certs = CertificateSizes::measured()?;
    let payloads = Payload::representative()?;
    let mut out = Vec::with_capacity(POLICIES.len() * payloads.len());
    for payload in payloads {
        for (name, policy) in POLICIES {
            out.push(profile(name, policy, payload, &certs, period, window));
        }
    }
    Ok(out)
}
