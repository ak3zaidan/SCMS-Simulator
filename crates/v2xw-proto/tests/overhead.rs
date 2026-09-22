//! What security costs per message, and what fraction of airtime it is.
//!
//! One of the headline results the simulator exists to produce, so the tests here pin the
//! numbers rather than merely exercising the code. Everything is either a real encoder's
//! output or a cited constant, and each test says which.
//!
//! Run with `--nocapture` for the table.

use v2xw_core::time::Duration;
use v2xw_proto::overhead::{
    self, AirInterface, ENVELOPE_COMMON_BYTES, POLICIES, Payload, SIGNER_CERT_PREFIX_BYTES,
    SIGNER_DIGEST_BYTES,
};
use v2xw_proto::sizes::{
    CertificateSizes, ENVELOPE_OVERHEAD_CERT_BYTES, ENVELOPE_OVERHEAD_DIGEST_BYTES, SizeProvenance,
};
use v2xw_sec::envelope::SignerIdChoice;

/// The BSM generation period the Phase 1 scenario runs at: 10 Hz, which is also the
/// mobility step, so one step is one message.
const PERIOD: Duration = Duration::from_millis(100);
/// One second of traffic, which is the window every published signer-identifier cadence is
/// stated over.
const WINDOW: u32 = 10;

// -----------------------------------------------------------------------------------------
// The decomposition adds up
// -----------------------------------------------------------------------------------------

/// The split into a common part and two signer parts reproduces both published overheads.
///
/// The failure this catches: treating 93 and 87 as independent constants. They share 84
/// bytes, and a decomposition that did not would report an envelope of 93 bytes *plus* a
/// certificate, overstating a full-certificate frame by nine bytes.
#[test]
fn the_envelope_decomposition_reproduces_both_measured_overheads() {
    assert_eq!(
        ENVELOPE_COMMON_BYTES + SIGNER_DIGEST_BYTES,
        ENVELOPE_OVERHEAD_DIGEST_BYTES,
        "84 + 9 must be the measured digest-signer overhead"
    );
    assert_eq!(
        ENVELOPE_COMMON_BYTES + SIGNER_CERT_PREFIX_BYTES,
        ENVELOPE_OVERHEAD_CERT_BYTES,
        "84 + 3 must be the measured certificate-signer overhead, before the certificate"
    );
    assert_eq!(ENVELOPE_COMMON_BYTES, 84);
    assert_eq!(SIGNER_DIGEST_BYTES, 9);
    assert_eq!(SIGNER_CERT_PREFIX_BYTES, 3);
}

/// Every payload size comes from a real encoder, not from a size model.
#[test]
fn the_payload_sizes_come_from_the_real_encoders() {
    for p in Payload::representative().expect("payloads encode") {
        assert!(
            matches!(p.bytes.provenance(), SizeProvenance::RealEncoder { .. }),
            "{}: {:?}",
            p.msg_type,
            p.bytes.provenance()
        );
        assert!(p.bytes.bytes() > 0);
    }
    let certs = CertificateSizes::measured().expect("certificates encode");
    assert!(matches!(
        certs.pseudonym.provenance(),
        SizeProvenance::RealEncoder { .. }
    ));
}

/// A frame's parts sum to the frame.
#[test]
fn a_frames_parts_sum_to_the_frame() {
    for p in overhead::table(PERIOD, WINDOW).expect("table builds") {
        for m in [p.digest, p.certificate] {
            assert_eq!(
                m.total_bytes(),
                m.payload_bytes + m.envelope_bytes + m.signer_id_bytes + m.certificate_bytes
            );
            assert!(m.security_bytes() > 0);
        }
        assert_eq!(
            p.digest.certificate_bytes, 0,
            "a digest carries no certificate"
        );
        assert!(p.certificate.certificate_bytes > 0);
        assert!(p.with_certificate <= p.window);
    }
}

// -----------------------------------------------------------------------------------------
// The cadence is the policy's own answer
// -----------------------------------------------------------------------------------------

/// Each policy's certificate cadence over one second at 10 Hz.
///
/// Obtained by asking `SignerIdPolicy::choose` message by message, which is the function
/// the envelope calls, so these counts are the ones a run really transmits. The first
/// message always carries the certificate — a receiver that has never seen it cannot
/// resolve a digest — which is why `digest-only` is 1 and not 0.
#[test]
fn each_policy_attaches_the_certificate_at_its_published_cadence() {
    let certs = CertificateSizes::measured().expect("encodes");
    let bsm = Payload::representative().expect("encodes")[0];
    let counts: Vec<(&str, u32)> = POLICIES
        .into_iter()
        .map(|(name, policy)| {
            let p = overhead::profile(name, policy, bsm, &certs, PERIOD, WINDOW);
            (name, p.with_certificate)
        })
        .collect();
    assert_eq!(
        counts,
        vec![
            ("digest-only", 1),
            ("always-certificate", 10),
            ("sae-450ms", 2),
            ("etsi-1s", 1),
        ],
        "one second of 10 Hz traffic under each published cadence"
    );
    // The SAE cadence is worth a sentence, because two is not the number the interval
    // suggests. Message times are quantised to the 100 ms generation period, so the first
    // message at or after 450 ms is the one at 500 ms: the *effective* cadence at 10 Hz is
    // 500 ms, not 450. A run reporting "SAE 450 ms" is really transmitting two full
    // certificates a second, and the difference is 77 bytes of certificate per second per
    // vehicle.
    let sae = overhead::profile(
        "sae-450ms",
        v2xw_sec::envelope::SignerIdPolicy::SAE_450MS,
        bsm,
        &certs,
        Duration::from_millis(50),
        20,
    );
    assert_eq!(
        sae.with_certificate, 3,
        "at 20 Hz the same policy lands on 450 ms exactly and fits three in a second"
    );
}

// -----------------------------------------------------------------------------------------
// Airtime: the same answer the radio gives
// -----------------------------------------------------------------------------------------

/// The airtime model reproduces frames a real engine run actually put on the air.
///
/// These four pairs are `node.tx` records from
/// `cargo run --release -p v2xw-cli -- run scenarios/phase1-manhattan.yaml`, whose PHY is
/// `v2xw_radio::phy::air_time` at the medium tier. This crate computes airtime from the
/// EN 302 663 Annex C constants without depending on the radio stack, so this test is what
/// keeps the two from drifting: if the radio's timing changes, these numbers change and
/// this fails.
#[test]
fn airtime_matches_the_frames_a_real_run_put_on_the_air() {
    let air = AirInterface::DSRC_6MBPS;
    // (bytes on the wire, airtime the run recorded, µs)
    for (bytes, us) in [(101u32, 184u64), (210, 328)] {
        assert_eq!(
            air.airtime(bytes).as_nanos() / 1_000,
            us,
            "a {bytes} B frame occupied {us} us in the Phase 1 run"
        );
    }
    // And the step structure is real: airtime is flat across a symbol's worth of bytes and
    // jumps 8 us when one crosses. 18 symbols hold 864 bits; 22 bits of SERVICE and TAIL
    // leave 842, so 105 B fits and 106 B needs a nineteenth symbol.
    assert_eq!(air.symbols(101), 18);
    assert_eq!(air.symbols(105), 18);
    assert_eq!(air.symbols(106), 19);
    assert_eq!(air.airtime(105), air.airtime(101));
    assert_eq!(
        air.airtime(106).as_nanos() - air.airtime(105).as_nanos(),
        8_000,
        "one more symbol is 8 us"
    );
}

// -----------------------------------------------------------------------------------------
// The headline number
// -----------------------------------------------------------------------------------------

/// The overhead table, printed, with the invariants it must satisfy asserted.
#[test]
fn the_security_overhead_table() {
    let air = AirInterface::DSRC_6MBPS;
    let certs = CertificateSizes::measured().expect("encodes");
    let table = overhead::table(PERIOD, WINDOW).expect("table builds");

    println!(
        "\n=== security overhead per message, 10 Hz, IEEE 1609.2 / ECDSA P-256, \
         6 Mbit/s 10 MHz OFDM ==="
    );
    println!(
        "  pseudonym certificate (implicit ECQV, linkageData): {} B, real COER encoder",
        certs.pseudonym.bytes()
    );
    println!(
        "  envelope: {ENVELOPE_COMMON_BYTES} B common + {SIGNER_DIGEST_BYTES} B digest \
         or {SIGNER_CERT_PREFIX_BYTES} B + certificate"
    );
    println!();
    println!(
        "  {:<5} {:<19} {:>7} {:>7} {:>7} {:>9} {:>8} {:>9} {:>9}",
        "msg", "policy", "pay B", "dgst B", "cert B", "mean sec", "mean B", "air ppm", "load ppm"
    );
    for p in &table {
        println!(
            "  {:<5} {:<19} {:>7} {:>7} {:>7} {:>9.1} {:>8.1} {:>9} {:>9}",
            p.msg_type,
            p.policy,
            p.digest.payload_bytes,
            p.digest.total_bytes(),
            p.certificate.total_bytes(),
            p.mean_security_millibytes() as f64 / 1_000.0,
            p.mean_total_millibytes() as f64 / 1_000.0,
            p.airtime_fraction_ppm(&air),
            p.channel_load_ppm(&air),
        );
    }

    // Invariants.
    for p in &table {
        let ppm = p.airtime_fraction_ppm(&air);
        assert!(
            ppm > 0,
            "{}/{}: security must cost airtime",
            p.msg_type,
            p.policy
        );
        assert!(
            ppm < 1_000_000,
            "{}/{}: and cannot be all of it",
            p.msg_type,
            p.policy
        );
        assert!(p.airtime(&air) > p.airtime_unsigned(&air));
        assert!(
            p.mean_total_millibytes() > p.mean_security_millibytes(),
            "the payload is part of the frame too"
        );
        assert_eq!(
            p.digest.signer,
            SignerIdChoice::Digest,
            "the digest case must be the digest case"
        );
    }

    // `always-certificate` is the most expensive policy and `digest-only` the cheapest, for
    // every message type. A table where that did not hold would mean the cadence was not
    // being applied.
    for msg in ["bsm", "cam"] {
        let of = |name: &str| {
            table
                .iter()
                .find(|p| p.msg_type == msg && p.policy == name)
                .expect("in the table")
        };
        let (cheapest, sae, etsi, dearest) = (
            of("digest-only"),
            of("sae-450ms"),
            of("etsi-1s"),
            of("always-certificate"),
        );
        assert!(cheapest.mean_security_millibytes() <= etsi.mean_security_millibytes());
        assert!(etsi.mean_security_millibytes() < sae.mean_security_millibytes());
        assert!(sae.mean_security_millibytes() < dearest.mean_security_millibytes());

        // And the exact identities, which are stronger than an ordering: a
        // full-certificate frame's security bytes are `common + 3 + certificate`, and a
        // policy's mean is the weighted mean of its two cases. A decomposition that was
        // merely monotone could still be wrong by nine bytes everywhere.
        let cert_case = u64::from(ENVELOPE_COMMON_BYTES + SIGNER_CERT_PREFIX_BYTES)
            + u64::from(certs.pseudonym.bytes());
        let digest_case = u64::from(ENVELOPE_COMMON_BYTES + SIGNER_DIGEST_BYTES);
        assert_eq!(
            dearest.mean_security_millibytes(),
            cert_case * 1_000,
            "{msg}: every frame carries the certificate"
        );
        assert_eq!(
            cheapest.mean_security_millibytes(),
            (cert_case + 9 * digest_case) * 100,
            "{msg}: one certificate and nine digests per second"
        );
        assert_eq!(
            sae.mean_security_millibytes(),
            (2 * cert_case + 8 * digest_case) * 100,
            "{msg}: two certificates and eight digests per second"
        );
    }
}

/// How many vehicles one 10 MHz channel holds before it is saturated, per policy.
///
/// The other side of the same number, and the one a deployment actually asks. It is
/// reported rather than asserted against a target, because the saturation point depends on
/// the MAC's efficiency and that is `v2xw-radio`'s to model — this is the airtime the
/// frames themselves need, which is a lower bound on what the channel must supply.
#[test]
fn the_channel_budget_per_policy() {
    let air = AirInterface::DSRC_6MBPS;
    let table = overhead::table(PERIOD, WINDOW).expect("table builds");
    println!("\n=== vehicles per 10 MHz channel at 10 Hz (frame airtime only) ===");
    for p in table.iter().filter(|p| p.msg_type == "bsm") {
        let load = p.channel_load_ppm(&air);
        let vehicles = u64::from(1_000_000 / load.max(1));
        // The same channel carrying unsigned payloads only.
        let unsigned_ns = p.airtime_unsigned(&air).as_nanos();
        let window_ns = PERIOD.as_nanos() * u64::from(WINDOW);
        let unsigned_load = (unsigned_ns * 1_000_000 / window_ns).max(1);
        println!(
            "  {:<19} {:>6} ppm per vehicle -> {vehicles:>5} vehicles   (unsigned: {:>5})",
            p.policy,
            load,
            1_000_000 / unsigned_load
        );
        assert!(
            vehicles >= 1,
            "{}: one vehicle must fit in one channel",
            p.policy
        );
        assert!(
            1_000_000 / unsigned_load > vehicles,
            "{}: dropping the envelope must fit more vehicles, not fewer",
            p.policy
        );
    }
}
