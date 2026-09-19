//! Prints the **real** encoded size of every message this crate encodes for real.
//!
//! `cargo run -p v2xw-msg --example sizes`
//!
//! The numbers this prints are UPER payload bytes from the actual encoder, not estimates,
//! and they are what a report or a design document should quote. Run it after any change to
//! [`v2xw_msg::cam`], [`v2xw_msg::denm`] or the generated bindings: a size that moves means
//! a field changed, and the size-model rows of 04-models.md §8.4 are calibrated against
//! these.
//!
//! The last column adds the security envelope and the lower layers from 04-models.md §9.1
//! and §9.3, because that — not the payload — is what the field measurements in §8.2 count.

use v2xw_core::belief::{FixQuality, PositionEstimate};
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::{Dims, Vec3};
use v2xw_core::time::{Duration, WallClock};

use v2xw_msg::cam::{
    self, CamInput, CamLowFrequency, ExteriorLightMask, ParticipantType, PathHistoryPoint,
    VehicleRole,
};
use v2xw_msg::denm::{self, DenmCause, DenmInput, EventId, TerminationKind};
use v2xw_msg::size_model::{self, ContentProfile, SizeRequest};

/// 1609.2 envelope with a digest signer (04-models.md §9.1).
const ENVELOPE_DIGEST: u32 = 93;
/// 1609.2 envelope with an ETSI authorization ticket: 87 B plus a 90-130 B certificate
/// (§9.1, §9.2). The low end is used here and the spread is quoted in the header.
const ENVELOPE_CERT: u32 = 87 + 90;
/// GN SHB + BTP-B + LLC/SNAP under a CAM or DENM (§9.3).
const GN_BTP: u32 = 52;

fn main() {
    let clock = WallClock::parse_rfc3339("2026-09-18T12:00:00Z").expect("t0 parses");
    let ts = cam::timestamp_its(clock, 0).expect("after the 1609.2 epoch");
    let origin = GeoOrigin::new(40.7440, -73.9900, 0.0);

    println!("v2xw-msg — real encoded sizes (ASN.1 UPER, payload bytes)\n");
    println!(
        "Envelope overheads used below: digest {ENVELOPE_DIGEST} B, certificate {ENVELOPE_CERT} B"
    );
    println!(
        "(87 + a 90 B authorization ticket; the ticket is 90-130 B), lower layers {GN_BTP} B.\n"
    );

    // --- CAM -----------------------------------------------------------------------
    let base = car(ts.clone(), origin);

    println!("CAM — ETSI TS 103 900 (Release 2)");
    println!(
        "  {:<44} {:>7} {:>12} {:>12}",
        "content", "payload", "+digest+GN", "+cert+GN"
    );
    let minimal = size_cam(&base);
    row("basic + high-frequency containers", minimal);

    for points in [0u64, 5, 8, 10, 15, 23, 40] {
        let mut input = base.clone();
        input.low_frequency = Some(low_frequency(points, origin));
        let size = size_cam(&input);
        let per_point = if points > 0 {
            f64::from(
                size - size_cam(&{
                    let mut zero = base.clone();
                    zero.low_frequency = Some(low_frequency(0, origin));
                    zero
                }),
            ) / points as f64
        } else {
            0.0
        };
        row(
            &format!("+ low-frequency, {points:>2} path points ({per_point:.2} B/point)"),
            size,
        );
    }

    let mut rsu = base.clone();
    rsu.station_type = ParticipantType::Infrastructure;
    row("RSU high-frequency container", size_cam(&rsu));

    // --- DENM ----------------------------------------------------------------------
    let mut input = DenmInput::new(
        EventId {
            originating_station_id: 0x0102_0304,
            sequence_number: 7,
        },
        0x0102_0304,
        ParticipantType::PassengerCar,
        ts,
        PositionEstimate {
            pos: Vec3::new(900.0, 1_100.0, 8.0),
            vel: Vec3::ZERO,
            heading_rad: 0.0,
            semi_major_m: 2.5,
            semi_minor_m: 1.5,
            orientation_rad: 0.3,
            time_ns: 0,
            fix: FixQuality::ThreeD,
        },
        origin,
        DenmCause::DangerousSituation,
    );

    println!("\nDENM — ETSI TS 103 831 (Release 2)");
    println!(
        "  {:<44} {:>7} {:>12} {:>12}",
        "content", "payload", "+digest+GN", "+cert+GN"
    );
    row("management + situation + location", size_denm(&input));
    input.transmission_interval = Some(Duration::from_millis(500));
    row("+ transmissionInterval", size_denm(&input));
    row(
        "cancellation (management only)",
        denm::encode_denm(
            &denm::build_termination_denm(&input, TerminationKind::Cancellation).expect("builds"),
        )
        .expect("encodes")
        .size,
    );

    // --- the modelled ones ---------------------------------------------------------
    println!("\nSize model — SAE J2735, NOT really encoded (build decision D2)");
    println!("  {:<44} {:>7}", "message / profile / elements", "modelled");
    for entry in size_model::TABLE {
        let elements = entry.nominal_elements;
        println!(
            "  {:<44} {:>7}",
            format!("{}/{} x{elements}", entry.ty, entry.profile),
            entry.bytes(elements)
        );
    }
    let codec = v2xw_msg::J2735SizeCodec::new();
    let spat = codec
        .size_of(&SizeRequest {
            ty: v2xw_msg::MsgType::Spat,
            profile: ContentProfile::Typical,
            elements: 8,
        })
        .expect("sized");
    println!(
        "\n  (placeholder bytes, exact modelled length — e.g. a typical 8-phase SPaT is {spat} B)"
    );
}

fn row(label: &str, payload: u32) {
    println!(
        "  {:<44} {:>7} {:>12} {:>12}",
        label,
        payload,
        payload + ENVELOPE_DIGEST + GN_BTP,
        payload + ENVELOPE_CERT + GN_BTP
    );
}

fn size_cam(input: &CamInput) -> u32 {
    cam::encode_cam(&cam::build_cam(input).expect("builds"))
        .expect("encodes")
        .size
}

fn size_denm(input: &DenmInput) -> u32 {
    denm::encode_denm(&denm::build_denm(input).expect("builds"))
        .expect("encodes")
        .size
}

/// A passenger car doing 50 km/h north-east in the Phase 1 world.
fn car(ts: v2xw_msg::asn1::cdd::TimestampIts, origin: GeoOrigin) -> CamInput {
    let speed = 13.89;
    let component = speed * std::f64::consts::FRAC_1_SQRT_2;
    let position = PositionEstimate {
        pos: Vec3::new(1_200.0, 800.0, 12.5),
        vel: Vec3::new(component, component, 0.0),
        heading_rad: std::f64::consts::FRAC_PI_4,
        semi_major_m: 1.8,
        semi_minor_m: 1.1,
        orientation_rad: 0.6,
        time_ns: 0,
        fix: FixQuality::ThreeD,
    };
    let mut input = CamInput::new(
        0x0A0B_0C0D,
        ParticipantType::PassengerCar,
        position,
        origin,
        Dims::CAR,
        ts,
    );
    input.longitudinal_acceleration_mps2 = Some(0.8);
    input.yaw_rate_rad_s = Some(0.05);
    input.curvature_inv_m = Some(0.004);
    input.curvature_from_yaw_rate = true;
    input.speed_accuracy_mps = Some(0.25);
    input.heading_accuracy_rad = Some(0.02);
    input.acceleration_accuracy_mps2 = Some(0.3);
    input
}

fn low_frequency(points: u64, _origin: GeoOrigin) -> CamLowFrequency {
    CamLowFrequency {
        vehicle_role: VehicleRole::Default,
        exterior_lights: ExteriorLightMask::LOW_BEAM,
        path_history: (1..=points)
            .map(|k| PathHistoryPoint {
                pos: Vec3::new(1_200.0 - k as f64 * 13.0, 800.0 - k as f64 * 9.0, 12.5),
                age: Duration::from_millis(k * 500),
            })
            .collect(),
    }
}
