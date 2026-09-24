//! What one frame's octets say, decoded with the real decoders.
//!
//! The chase view's message inspector shows a transmitted or received frame field by field.
//! Every value here is read from the octets that went on the air: the IEEE 1609.2 SPDU is
//! parsed by `v2xw-sec`'s own envelope parser (the one a receiving node verifies with), and
//! the payload inside it by `v2xw-msg`'s J2735 BSM decoder or its ETSI CAM decoder. Nothing
//! is taken from what the sender believed it encoded, so a field the bytes do not carry is
//! not shown, and a field the bytes carry as a J2735/CDD "unavailable" sentinel is shown as
//! exactly that.
//!
//! # Units and sentinels, with their sources
//!
//! * J2735 (SAE J2735 2016-03 §6 data elements): `Latitude`/`Longitude` 1e-7 degree,
//!   unavailable 900000001 / 1800000001; `Elevation` 0.1 m, unknown −4096; `Speed` 0.02 m/s,
//!   unavailable 8191; `Heading` 0.0125 degree, unavailable 28800; `SteeringWheelAngle`
//!   1.5 degree, unavailable 127; `Acceleration` 0.01 m/s², unavailable 2001;
//!   `VerticalAcceleration` 0.02 G, unavailable −127; `YawRate` 0.01 degree/s;
//!   `SemiMajorAxisAccuracy` 0.05 m, unavailable 255; `SemiMajorAxisOrientation`
//!   360/65535 degree, unavailable 65535; `VehicleWidth`/`VehicleLength` 1 cm; `DSecond`
//!   ms within the minute, unavailable 65535. The constants are `v2xw_msg::j2735::bsm`'s.
//! * ETSI CDD (TS 102 894-2 V2.1.1): `Latitude`/`Longitude` 1e-7 degree; `AltitudeValue`
//!   0.01 m, unavailable 800001; `HeadingValue` 0.1 degree, unavailable 3601; `SpeedValue`
//!   0.01 m/s, unavailable 16383; `VehicleLengthValue` 0.1 m, unavailable 1023;
//!   `VehicleWidth` 0.1 m, unavailable 62; `AccelerationValue` 0.1 m/s², unavailable 161;
//!   `CurvatureValue` 1/10000 m⁻¹, unavailable 1023; `YawRateValue` 0.01 degree/s,
//!   unavailable 32767; `SemiAxisLength` 0.01 m, unavailable 4095; `GenerationDeltaTime`
//!   ms modulo 65536 (EN 302 637-2 §B.3). The same values `v2xw_msg::units` documents.
//! * IEEE 1609.2-2016 §6.3: `Ieee1609Dot2Data` → `SignedData` → `tbsData` (`payload`,
//!   `headerInfo`), `signer` (`digest` HashedId8 or `certificate`), `signature`. The
//!   certificate's HashedId8 is the low-order 8 octets of SHA-256 over its COER encoding
//!   (§6.4.3); `v2xw-sec` computes it the same way for verification.

use serde_json::{Value, json};
use v2xw_msg::j2735::bsm;
use v2xw_sec::SecurityEnvelopeInfo;

/// One decoded field: a key, a label, the value in engineering units (or text), its unit,
/// the raw integer the octets carried, and whether that raw value is the element's
/// "unavailable" sentinel.
fn field(key: &str, label: &str, value: Value, unit: &str, raw: Value, na: bool) -> Value {
    let mut f =
        json!({"k": key, "label": label, "v": if na { Value::Null } else { value }, "raw": raw});
    if !unit.is_empty() {
        f["unit"] = json!(unit);
    }
    if na {
        f["na"] = json!(true);
    }
    f
}

/// A scaled numeric field with a sentinel.
fn scaled(
    key: &str,
    label: &str,
    raw: i64,
    scale: f64,
    unit: &str,
    unavailable: Option<i64>,
    digits: i32,
) -> Value {
    let na = unavailable == Some(raw);
    let q = 10f64.powi(digits);
    let v = ((raw as f64) * scale * q).round() / q;
    field(key, label, json!(v), unit, json!(raw), na)
}

fn text(key: &str, label: &str, v: &str) -> Value {
    field(key, label, json!(v), "", json!(v), false)
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The first position at or after `from` where `needle` occurs in `hay`.
fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() || needle.len() > hay.len() - from {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The decoded SPDU and its payload, as the feed carries it.
///
/// `hex` and `spans` let a viewer print the octets with each layer marked: the spans tile
/// `0..len` in order. A span's boundaries are located by finding the exact octets the parser
/// returned (the payload, the signer's digest or certificate, the signature's `r` and `s`)
/// inside the encoding, which is sound for COER because it embeds OCTET STRINGs verbatim;
/// where a boundary cannot be located the rest is one `envelope` span rather than a guess.
pub fn decode_frame(spdu: &[u8], msg_type: &str, with_bytes: bool) -> Value {
    // The parser does not read the wall clock; any clock builds the same parser.
    let envelope = v2xw_sec::Envelope::ieee1609(v2xw_core::time::WallClock::new(0));
    let mut out = json!({"spdu_bytes": spdu.len()});
    if with_bytes {
        out["hex"] = json!(hex(spdu));
    }
    let parsed = match envelope.parse(spdu) {
        Ok(p) => p,
        Err(e) => {
            out["error"] = json!(format!(
                "the SPDU did not parse as IEEE 1609.2 SignedData: {e}"
            ));
            out["spans"] =
                json!([{"name": "undecoded", "layer": "envelope", "start": 0, "end": spdu.len()}]);
            return out;
        }
    };

    // --- the envelope ------------------------------------------------------------------
    let mut signer = serde_json::Map::new();
    let mut signer_octets: Option<Vec<u8>> = None;
    match &parsed.signer {
        v2xw_sec::ParsedSigner::Digest(d) => {
            signer.insert("kind".into(), json!("digest"));
            signer.insert("hashed_id8".into(), json!(hex(&d.0[..])));
            signer_octets = Some(d.0[..].to_vec());
        }
        v2xw_sec::ParsedSigner::Certificate {
            certificate,
            digest,
        } => {
            signer.insert("kind".into(), json!("certificate"));
            signer.insert("hashed_id8".into(), json!(hex(&digest.0[..])));
            let coer = v2xw_sec::cert::encode(certificate).ok();
            signer.insert(
                "certificate".into(),
                certificate_json(certificate, coer.as_deref()),
            );
            signer_octets = coer;
        }
        v2xw_sec::ParsedSigner::SelfSigned => {
            signer.insert("kind".into(), json!("self"));
        }
    }
    let (r, s) = parsed.signature.split_at(parsed.signature.len() / 2);
    out["security"] = json!({
        "standard": "IEEE 1609.2-2016 SignedData",
        "psid": parsed.psid,
        "hash": format!("{:?}", parsed.hash_id),
        "generation_time_us": parsed.generation_time,
        "signer": Value::Object(signer),
        "signature": {"alg": "ECDSA NIST P-256", "r": hex(r), "s": hex(s)},
        "payload_bytes": parsed.payload.len(),
    });
    if let Some(loc) = parsed.generation_location {
        out["security"]["generation_location"] = json!(format!("{loc:?}"));
    }

    // --- spans ---------------------------------------------------------------------------
    let mut spans: Vec<Value> = Vec::new();
    let payload_at = find_from(spdu, &parsed.payload, 0);
    if let Some(p0) = payload_at {
        let p1 = p0 + parsed.payload.len();
        spans.push(json!({"name": "1609.2 header", "layer": "envelope", "start": 0, "end": p0}));
        spans.push(json!({"name": "payload", "layer": "payload", "start": p0, "end": p1}));
        let signer_at = signer_octets
            .as_deref()
            .and_then(|o| find_from(spdu, o, p1).map(|at| (at, at + o.len())));
        let sig_at = find_from(spdu, r, signer_at.map_or(p1, |(_, e)| e));
        match (signer_at, sig_at) {
            (Some((s0, s1)), Some(g0)) if s0 >= p1 && g0 >= s1 => {
                spans.push(
                    json!({"name": "headerInfo", "layer": "envelope", "start": p1, "end": s0}),
                );
                spans.push(json!({"name": "signer", "layer": "envelope", "start": s0, "end": s1}));
                // The signature's own CHOICE tag and the `r` point's tag sit between the
                // signer and `r`; they belong to the signature.
                spans.push(json!({"name": "signature", "layer": "envelope", "start": s1, "end": spdu.len()}));
            }
            _ => spans.push(
                json!({"name": "envelope", "layer": "envelope", "start": p1, "end": spdu.len()}),
            ),
        }
    } else {
        spans.push(json!({"name": "envelope", "layer": "envelope", "start": 0, "end": spdu.len()}));
    }
    out["spans"] = Value::Array(spans);

    // --- the payload ---------------------------------------------------------------------
    out["message"] = decode_payload(msg_type, &parsed.payload);
    out
}

/// The certificate's own fields (IEEE 1609.2 §6.4), as a viewer shows them.
fn certificate_json(
    cert: &v2xw_msg::sec_types::ieee1609_dot2::Certificate,
    coer: Option<&[u8]>,
) -> Value {
    use v2xw_msg::sec_types::ieee1609_dot2::{CertificateId, CertificateType, IssuerIdentifier};
    use v2xw_msg::sec_types::ieee1609_dot2_base_types::Duration;
    let base = &cert.0;
    let tbs = &base.to_be_signed;
    let issuer = match &base.issuer {
        IssuerIdentifier::sha256AndDigest(d)
        | IssuerIdentifier::sha384AndDigest(d)
        | IssuerIdentifier::sm3AndDigest(d) => hex(&d.0[..]),
        IssuerIdentifier::R_self(_) => "self".to_string(),
        _ => "an issuer identifier this build does not name".to_string(),
    };
    let id = match &tbs.id {
        CertificateId::linkageData(ld) => json!({
            "kind": "linkageData",
            "i_cert": ld.i_cert.0.0,
            "linkage_value": hex(&ld.linkage_value.0[..]),
        }),
        CertificateId::name(n) => json!({"kind": "name", "name": format!("{n:?}")}),
        CertificateId::binaryId(b) => json!({"kind": "binaryId", "id": hex(b)}),
        CertificateId::none(()) => json!({"kind": "none"}),
        _ => json!({"kind": "other"}),
    };
    let duration = match &tbs.validity_period.duration {
        Duration::microseconds(v) => format!("{} µs", v.0),
        Duration::milliseconds(v) => format!("{} ms", v.0),
        Duration::seconds(v) => format!("{} s", v.0),
        Duration::minutes(v) => format!("{} min", v.0),
        Duration::hours(v) => format!("{} h", v.0),
        Duration::sixtyHours(v) => format!("{} × 60 h", v.0),
        Duration::years(v) => format!("{} years", v.0),
    };
    let psids: Vec<u64> = tbs
        .app_permissions
        .as_ref()
        .map(|p| {
            p.0.iter()
                .map(|ps| u64::try_from(&ps.psid.0).unwrap_or(u64::MAX))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "type": match base.r_type {
            CertificateType::explicit => "explicit",
            CertificateType::implicit => "implicit",
            _ => "other",
        },
        "issuer": issuer,
        "id": id,
        "craca_id": hex(&tbs.craca_id.0[..]),
        "crl_series": tbs.crl_series.0.0,
        "validity_start_time32": tbs.validity_period.start.0.0,
        "validity_duration": duration,
        "app_permissions": psids,
        "bytes": coer.map(<[u8]>::len),
    })
}

/// The payload's fields, by message type.
pub fn decode_payload(msg_type: &str, payload: &[u8]) -> Value {
    match msg_type {
        "bsm" => match bsm::decode_message_frame(payload) {
            Ok(m) => {
                json!({"format": "SAE J2735 MessageFrame · BasicSafetyMessage (DSRCmsgID 20)", "fields": bsm_fields(&m)})
            }
            Err(e) => {
                json!({"format": "SAE J2735 MessageFrame", "error": e.to_string(), "fields": []})
            }
        },
        "cam" => match v2xw_msg::cam::decode_cam(payload) {
            Ok(c) => json!({"format": "ETSI EN 302 637-2 CAM", "fields": cam_fields(&c)}),
            Err(e) => {
                json!({"format": "ETSI EN 302 637-2 CAM", "error": e.to_string(), "fields": []})
            }
        },
        other => json!({
            "format": other.to_uppercase(),
            "fields": [],
            "note": format!("this build has no decoder wired into the inspector for {other}; the octets are shown undecoded"),
        }),
    }
}

fn bsm_fields(m: &bsm::BasicSafetyMessage) -> Vec<Value> {
    let k = &m.core;
    let a = &k.accuracy;
    let acc = &k.accel_set;
    let b = &k.brakes;
    let mut out = vec![
        field(
            "msg_cnt",
            "msgCnt",
            json!(k.msg_cnt),
            "",
            json!(k.msg_cnt),
            false,
        ),
        text("temp_id", "id (temporary)", &hex(&k.id)),
        scaled(
            "sec_mark",
            "secMark",
            i64::from(k.sec_mark),
            1.0,
            "ms",
            Some(i64::from(bsm::D_SECOND_UNAVAILABLE)),
            0,
        ),
        scaled(
            "lat",
            "latitude",
            i64::from(k.lat),
            1e-7,
            "°",
            Some(i64::from(bsm::LATITUDE_UNAVAILABLE)),
            7,
        ),
        scaled(
            "lon",
            "longitude",
            i64::from(k.lon),
            1e-7,
            "°",
            Some(i64::from(bsm::LONGITUDE_UNAVAILABLE)),
            7,
        ),
        scaled(
            "elev",
            "elevation",
            i64::from(k.elev),
            0.1,
            "m",
            Some(i64::from(bsm::ELEVATION_UNKNOWN)),
            1,
        ),
        scaled(
            "acc_semi_major",
            "accuracy · semi-major",
            i64::from(a.semi_major),
            0.05,
            "m",
            Some(i64::from(bsm::SEMI_AXIS_UNAVAILABLE)),
            2,
        ),
        scaled(
            "acc_semi_minor",
            "accuracy · semi-minor",
            i64::from(a.semi_minor),
            0.05,
            "m",
            Some(i64::from(bsm::SEMI_AXIS_UNAVAILABLE)),
            2,
        ),
        scaled(
            "acc_orientation",
            "accuracy · orientation",
            i64::from(a.orientation),
            360.0 / 65535.0,
            "°",
            Some(i64::from(bsm::ORIENTATION_UNAVAILABLE)),
            2,
        ),
        field(
            "transmission",
            "transmission",
            json!(format!("{:?}", k.transmission)),
            "",
            json!(k.transmission.index()),
            matches!(k.transmission, bsm::TransmissionState::Unavailable),
        ),
        scaled(
            "speed",
            "speed",
            i64::from(k.speed),
            0.02,
            "m/s",
            Some(i64::from(bsm::SPEED_UNAVAILABLE)),
            2,
        ),
        scaled(
            "heading",
            "heading",
            i64::from(k.heading),
            0.0125,
            "° from north",
            Some(i64::from(bsm::HEADING_UNAVAILABLE)),
            4,
        ),
        scaled(
            "angle",
            "steering wheel angle",
            i64::from(k.angle),
            1.5,
            "°",
            Some(i64::from(bsm::STEERING_WHEEL_ANGLE_UNAVAILABLE)),
            1,
        ),
        scaled(
            "accel_long",
            "acceleration · longitudinal",
            i64::from(acc.long),
            0.01,
            "m/s²",
            Some(i64::from(bsm::ACCELERATION_UNAVAILABLE)),
            2,
        ),
        scaled(
            "accel_lat",
            "acceleration · lateral",
            i64::from(acc.lat),
            0.01,
            "m/s²",
            Some(i64::from(bsm::ACCELERATION_UNAVAILABLE)),
            2,
        ),
        scaled(
            "accel_vert",
            "acceleration · vertical",
            i64::from(acc.vert),
            0.02,
            "G",
            Some(i64::from(bsm::VERTICAL_ACCELERATION_UNAVAILABLE)),
            2,
        ),
        scaled(
            "yaw_rate",
            "yaw rate",
            i64::from(acc.yaw),
            0.01,
            "°/s",
            None,
            2,
        ),
        field(
            "brakes_wheels",
            "brakes · wheel brakes applied",
            json!(wheel_brakes(b.wheel_brakes)),
            "",
            json!(b.wheel_brakes.0),
            b.wheel_brakes
                .contains(bsm::BrakeAppliedStatus::UNAVAILABLE),
        ),
        field(
            "brakes_traction",
            "brakes · traction control",
            json!(format!("{:?}", b.traction)),
            "",
            json!(b.traction.index()),
            matches!(b.traction, bsm::ControlStatus::Unavailable),
        ),
        field(
            "brakes_abs",
            "brakes · ABS",
            json!(format!("{:?}", b.abs)),
            "",
            json!(b.abs.index()),
            matches!(b.abs, bsm::ControlStatus::Unavailable),
        ),
        field(
            "brakes_scs",
            "brakes · stability control",
            json!(format!("{:?}", b.scs)),
            "",
            json!(b.scs.index()),
            matches!(b.scs, bsm::ControlStatus::Unavailable),
        ),
        field(
            "brakes_boost",
            "brakes · brake boost",
            json!(format!("{:?}", b.brake_boost)),
            "",
            json!(b.brake_boost.index()),
            matches!(b.brake_boost, bsm::BrakeBoostApplied::Unavailable),
        ),
        field(
            "brakes_aux",
            "brakes · auxiliary brakes",
            json!(format!("{:?}", b.aux_brakes)),
            "",
            json!(b.aux_brakes.index()),
            matches!(b.aux_brakes, bsm::AuxiliaryBrakeStatus::Unavailable),
        ),
        // VehicleWidth / VehicleLength carry no sentinel in J2735; zero is what a sender with
        // no size information puts there.
        scaled(
            "width",
            "size · width",
            i64::from(k.size.width),
            0.01,
            "m",
            Some(0),
            2,
        ),
        scaled(
            "length",
            "size · length",
            i64::from(k.size.length),
            0.01,
            "m",
            Some(0),
            2,
        ),
    ];
    let ids: Vec<u8> = m.part_ii.iter().map(|p| p.id).collect();
    out.push(field(
        "part_ii",
        "Part II containers",
        json!(ids.len()),
        "",
        json!(ids),
        false,
    ));
    out
}

fn wheel_brakes(s: bsm::BrakeAppliedStatus) -> String {
    if s.contains(bsm::BrakeAppliedStatus::UNAVAILABLE) {
        return "unavailable".to_string();
    }
    let names = [
        (bsm::BrakeAppliedStatus::LEFT_FRONT, "left front"),
        (bsm::BrakeAppliedStatus::LEFT_REAR, "left rear"),
        (bsm::BrakeAppliedStatus::RIGHT_FRONT, "right front"),
        (bsm::BrakeAppliedStatus::RIGHT_REAR, "right rear"),
    ];
    let on: Vec<&str> = names
        .iter()
        .filter(|(f, _)| s.contains(*f))
        .map(|(_, n)| *n)
        .collect();
    if on.is_empty() {
        "none".to_string()
    } else {
        on.join(", ")
    }
}

fn cam_fields(c: &v2xw_msg::asn1::cam_asn1::CAM) -> Vec<Value> {
    use v2xw_msg::asn1::cam_asn1::HighFrequencyContainer;
    let p = &c.cam.cam_parameters;
    let rp = &p.basic_container.reference_position;
    let mut out = vec![
        field(
            "station_id",
            "stationID",
            json!(c.header.station_id.0),
            "",
            json!(c.header.station_id.0),
            false,
        ),
        text(
            "temp_id",
            "stationID (hex)",
            &format!("{:08x}", c.header.station_id.0),
        ),
        scaled(
            "generation_delta_time",
            "generationDeltaTime",
            i64::from(c.cam.generation_delta_time.0),
            1.0,
            "ms",
            None,
            0,
        ),
        field(
            "station_type",
            "stationType",
            json!(p.basic_container.station_type.0),
            "",
            json!(p.basic_container.station_type.0),
            p.basic_container.station_type.0 == 0,
        ),
        scaled(
            "lat",
            "latitude",
            i64::from(rp.latitude.0),
            1e-7,
            "°",
            Some(900_000_001),
            7,
        ),
        scaled(
            "lon",
            "longitude",
            i64::from(rp.longitude.0),
            1e-7,
            "°",
            Some(1_800_000_001),
            7,
        ),
        scaled(
            "alt",
            "altitude",
            i64::from(rp.altitude.altitude_value.0),
            0.01,
            "m",
            Some(800_001),
            2,
        ),
        scaled(
            "acc_semi_major",
            "confidence · semi-major",
            i64::from(rp.position_confidence_ellipse.semi_major_axis_length.0),
            0.01,
            "m",
            Some(4095),
            2,
        ),
        scaled(
            "acc_semi_minor",
            "confidence · semi-minor",
            i64::from(rp.position_confidence_ellipse.semi_minor_axis_length.0),
            0.01,
            "m",
            Some(4095),
            2,
        ),
        scaled(
            "acc_orientation",
            "confidence · orientation",
            i64::from(rp.position_confidence_ellipse.semi_major_axis_orientation.0),
            0.1,
            "°",
            Some(3601),
            1,
        ),
    ];
    if let HighFrequencyContainer::basicVehicleContainerHighFrequency(h) =
        &p.high_frequency_container
    {
        out.extend([
            scaled(
                "heading",
                "heading",
                i64::from(h.heading.heading_value.0),
                0.1,
                "° from north",
                Some(3601),
                1,
            ),
            scaled(
                "speed",
                "speed",
                i64::from(h.speed.speed_value.0),
                0.01,
                "m/s",
                Some(16383),
                2,
            ),
            field(
                "drive_direction",
                "driveDirection",
                json!(format!("{:?}", h.drive_direction)),
                "",
                json!(format!("{:?}", h.drive_direction)),
                false,
            ),
            scaled(
                "length",
                "vehicleLength",
                i64::from(h.vehicle_length.vehicle_length_value.0),
                0.1,
                "m",
                Some(1023),
                1,
            ),
            scaled(
                "width",
                "vehicleWidth",
                i64::from(h.vehicle_width.0),
                0.1,
                "m",
                Some(62),
                1,
            ),
            scaled(
                "accel_long",
                "longitudinalAcceleration",
                i64::from(h.longitudinal_acceleration.value.0),
                0.1,
                "m/s²",
                Some(161),
                1,
            ),
            scaled(
                "curvature",
                "curvature",
                i64::from(h.curvature.curvature_value.0),
                1e-4,
                "1/m",
                Some(1023),
                4,
            ),
            scaled(
                "yaw_rate",
                "yawRate",
                i64::from(h.yaw_rate.yaw_rate_value.0),
                0.01,
                "°/s",
                Some(32767),
                2,
            ),
        ]);
    } else {
        out.push(text(
            "high_frequency",
            "highFrequencyContainer",
            "rsuContainerHighFrequency",
        ));
    }
    out.push(field(
        "low_frequency",
        "lowFrequencyContainer",
        json!(p.low_frequency_container.is_some()),
        "",
        json!(p.low_frequency_container.is_some()),
        false,
    ));
    out
}
