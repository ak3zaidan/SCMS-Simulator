//! Integration tests: the crate as `v2xw-node`, `v2xw-sec` and the BSM stage will use it.
//!
//! These run from *outside* `v2xw-msg`, which is the point. A unit test inside the crate can
//! reach private items and can construct `#[non_exhaustive]` generated types with struct
//! literals; a downstream crate can do neither. Anything that compiles here is genuinely
//! reachable from the engine.
//!
//! The file also carries the only full wiring of the [`MessageGenerator`] seam: a context
//! and a node view built the way an engine builds them, so the generic parameters on the
//! trait are exercised rather than assumed to work.

use v2xw_core::belief::{FixQuality, PositionEstimate};
use v2xw_core::card::Family;
use v2xw_core::ctx::{Ctx, ErasedRecord, Record, Visibility};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::{Dims, Vec3};
use v2xw_core::ids::{ActorId, NodeId};
use v2xw_core::model::Model;
use v2xw_core::nodeview::NodeView;
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::{Duration, NS_PER_MS, SimTime, WallClock};

use v2xw_msg::cam::{
    self, CamInput, CamLowFrequency, ExteriorLightMask, ParticipantType, PathHistoryPoint,
    VehicleRole,
};
use v2xw_msg::codec::{Encoded, Message, MessageCodec, MsgType, SizeSource};
use v2xw_msg::denm::{self, DenmCause, DenmInput, EventId, TerminationKind};
use v2xw_msg::generator::{DccState, GenReason, GenRequest, GenerationRecord, MessageGenerator};
use v2xw_msg::size_model::{ContentProfile, SizeRequest};
use v2xw_msg::{
    BsmGenerator, CamGenerator, EtsiSizeCodec, EtsiUperCodec, J2735BsmCodec, J2735InfraCodec,
    J2735SizeCodec,
};

// =========================================================================================
// A context and a node view, as an engine builds them
// =========================================================================================

/// The minimum an engine has to hold to implement [`Ctx`].
struct EngineCtx {
    scheduler: Scheduler<&'static str>,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    world: (),
    actors: Vec<ActorId>,
    emitted: Vec<(&'static str, Visibility, String)>,
}

impl EngineCtx {
    fn new() -> Self {
        Self {
            scheduler: Scheduler::new(),
            rng: RngRegistry::new(20_260_918),
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            world: (),
            actors: Vec::new(),
            emitted: Vec::new(),
        }
    }
}

impl Ctx for EngineCtx {
    type World = ();
    type Actors = Vec<ActorId>;
    type Payload = &'static str;

    fn now(&self) -> SimTime {
        self.scheduler.now()
    }
    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }
    fn schedule(&mut self, at: SimTime, class: EventClass, payload: &'static str) -> EventHandle {
        self.scheduler.schedule(at, class, payload)
    }
    fn cancel(&mut self, handle: EventHandle) -> bool {
        self.scheduler.cancel(handle)
    }
    fn world(&self) -> &() {
        &self.world
    }
    fn actors(&self) -> &Vec<ActorId> {
        &self.actors
    }
    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        let mut bytes = Vec::new();
        record.write_json(&mut bytes).expect("record serialises");
        self.emitted.push((
            record.channel(),
            record.visibility(),
            String::from_utf8(bytes).expect("json is utf-8"),
        ));
    }
    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
        self.provenance.record(subject, model, params);
    }
    fn params(&self) -> &ParamSet {
        &self.params
    }
}

/// A node view holding exactly what a node believes — no world, no ground truth (I-C2).
struct EngineNodeView {
    node: NodeId,
    believed_time: SimTime,
    position: PositionEstimate,
    neighbors: Vec<NodeId>,
    credentials: Vec<u32>,
    received: Vec<Vec<u8>>,
}

impl NodeView for EngineNodeView {
    type Neighbors = Vec<NodeId>;
    type Credential = u32;
    type Message = Vec<u8>;

    fn node(&self) -> NodeId {
        self.node
    }
    fn believed_time(&self) -> SimTime {
        self.believed_time
    }
    fn position(&self) -> &PositionEstimate {
        &self.position
    }
    fn neighbors(&self) -> &Vec<NodeId> {
        &self.neighbors
    }
    fn credentials(&self) -> &[u32] {
        &self.credentials
    }
    fn received(&self) -> &[Vec<u8>] {
        &self.received
    }
}

fn origin() -> GeoOrigin {
    // The Phase 1 world's south-west corner (build decision D7).
    GeoOrigin::new(40.7440, -73.9900, 0.0)
}

fn belief(t: SimTime, pos: Vec3, speed_mps: f64, heading_rad: f64) -> PositionEstimate {
    // `v2xw_core::math`, never `f64::sin` — ADR 0003 applies to test code too, because a
    // test that drifted between platforms would be worse than no test.
    let (sin, cos) = v2xw_core::math::sin_cos(heading_rad);
    PositionEstimate {
        pos,
        vel: Vec3::new(speed_mps * cos, speed_mps * sin, 0.0),
        heading_rad,
        semi_major_m: 1.8,
        semi_minor_m: 1.1,
        orientation_rad: 0.4,
        time_ns: t,
        fix: FixQuality::ThreeD,
    }
}

fn view(t: SimTime, pos: Vec3, speed_mps: f64, heading_rad: f64) -> EngineNodeView {
    EngineNodeView {
        node: NodeId::new(11),
        believed_time: t,
        position: belief(t, pos, speed_mps, heading_rad),
        neighbors: Vec::new(),
        credentials: vec![1],
        received: Vec::new(),
    }
}

// =========================================================================================
// The codec seam
// =========================================================================================

#[test]
fn the_registered_codecs_partition_the_message_set_they_claim() {
    // Every codec the registry gets, in the order it gets them.
    let codecs = v2xw_msg::size_model::codecs();
    assert_eq!(
        codecs.len(),
        5,
        "one generated ETSI codec, two hand-written J2735 codecs, two size models"
    );
    for codec in &codecs {
        assert_eq!(codec.family(), Family::Codec, "{}", codec.id());
    }

    for ty in MsgType::ALL {
        let claimants: Vec<&str> = codecs
            .iter()
            .filter(|c| c.supports(ty))
            .map(|c| c.id())
            .collect();
        assert!(
            claimants.len() <= 1,
            "{ty} is claimed by more than one codec: {claimants:?}"
        );
    }

    // And the ones nobody claims are exactly the ones another crate or a later stage owns.
    // SPaT and MAP left this list when `codec/uper/j2735-spat-map` arrived; CPM and VAM
    // left it when `codec/size-model/etsi` did.
    let unclaimed: Vec<MsgType> = MsgType::ALL
        .into_iter()
        .filter(|ty| !codecs.iter().any(|c| c.supports(*ty)))
        .collect();
    assert_eq!(
        unclaimed,
        vec![
            MsgType::Wsa, // IEEE 1609.3, no codec planned
            MsgType::Crl, // v2xw-sec, over sec_types
            MsgType::Mbr, // TS 103 759, a later stage
        ]
    );

    // The individual constructors must agree with what the registry list says.
    assert!(EtsiUperCodec::new().supports(MsgType::Cam));
    assert!(J2735BsmCodec::new().supports(MsgType::Bsm));
    assert!(J2735InfraCodec::new().supports(MsgType::Spat));
    assert!(J2735InfraCodec::new().supports(MsgType::Map));
    assert!(J2735SizeCodec::new().supports(MsgType::Psm));
    assert!(EtsiSizeCodec::new().supports(MsgType::Cpm));
    assert!(EtsiSizeCodec::new().supports(MsgType::Vam));
    // The size model must not still claim the two messages that are now really encoded.
    assert!(!J2735SizeCodec::new().supports(MsgType::Spat));
    assert!(!J2735SizeCodec::new().supports(MsgType::Map));
}

/// The distinction the whole crate exists to protect: a size-modelled message must never
/// be reachable through a predicate a report would read as "byte-exact", and a hand-written
/// encoder that no oracle has seen must not be either.
#[test]
fn nothing_size_modelled_is_reported_as_byte_exact() {
    use v2xw_msg::ByteExactness;

    for ty in MsgType::ALL {
        let exactness = v2xw_msg::byte_exactness(ty);
        if exactness == ByteExactness::SizeModelled {
            assert!(!exactness.is_byte_exact(), "{ty}");
            assert!(!exactness.has_real_bytes(), "{ty}");
        }
    }
    // The four levels, spelled out for the four messages that define them.
    assert!(v2xw_msg::byte_exactness(MsgType::Bsm).has_independent_check());
    assert!(v2xw_msg::byte_exactness(MsgType::Cam).is_byte_exact());
    assert!(!v2xw_msg::byte_exactness(MsgType::Cam).has_independent_check());
    assert!(!v2xw_msg::byte_exactness(MsgType::Spat).is_byte_exact());
    assert!(v2xw_msg::byte_exactness(MsgType::Spat).has_real_bytes());
    assert!(!v2xw_msg::byte_exactness(MsgType::Cpm).has_real_bytes());
}

/// The hand-written SPaT and MAP encoders, through the seam the engine uses.
#[test]
fn a_spat_and_a_map_round_trip_through_the_seam_as_real_bytes() {
    use v2xw_msg::j2735::{map, spat};

    let spat = spat::Spat::one(spat::IntersectionState {
        id: spat::IntersectionReferenceId::new(42),
        revision: 1,
        status: spat::IntersectionStatus::FIXED_TIME_OPERATION,
        moy: Some(12_345),
        time_stamp: Some(500),
        states: vec![
            spat::MovementState::current(
                1,
                spat::MovementEvent::timed(
                    spat::MovementPhaseState::ProtectedMovementAllowed,
                    spat::TimeChangeDetails::fixed(spat::time_mark(0.0), spat::time_mark(27.5)),
                ),
            ),
            spat::MovementState::current(
                2,
                spat::MovementEvent::phase(spat::MovementPhaseState::StopAndRemain),
            ),
        ],
    });
    let map_data = map::MapData {
        time_stamp: Some(12_345),
        msg_issue_revision: 1,
        intersections: vec![map::IntersectionGeometry {
            id: spat::IntersectionReferenceId::new(42),
            revision: 1,
            ref_point: map::Position3D {
                lat: 407_440_000,
                lon: -739_900_000,
                elevation: Some(100),
            },
            lane_width_cm: Some(map::lane_width_cm(3.5)),
            lanes: vec![map::GenericLane {
                lane_id: 1,
                ingress_approach: Some(1),
                egress_approach: None,
                attributes: map::LaneAttributes::vehicle(map::LaneDirection::INGRESS),
                maneuvers: Some(map::AllowedManeuvers::STRAIGHT),
                nodes: vec![
                    map::NodeXy::offset(0, 0).expect("fits"),
                    map::NodeXy::offset(100, 3_000).expect("fits"),
                ],
                connects_to: vec![map::Connection::signalised(2, 1)],
            }],
        }],
    };

    let codec = J2735InfraCodec::new();
    for (ty, bytes) in [
        (
            MsgType::Spat,
            spat::encode_spat(&spat).expect("encodes").bytes,
        ),
        (
            MsgType::Map,
            map::encode_map(&map_data).expect("encodes").bytes,
        ),
    ] {
        let encoded = codec
            .encode(&Message::HandEncoded {
                ty,
                bytes: bytes.clone(),
            })
            .unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(encoded.size_source, SizeSource::Uper, "{ty}");
        assert!(encoded.is_real(), "{ty}");
        assert_eq!(encoded.size as usize, encoded.bytes.len(), "{ty}");
        // Real bytes, so they are not the size model's fill pattern.
        assert!(
            encoded.bytes.iter().any(|&b| b != 0xa5),
            "{ty}: this should be an encoding, not a placeholder"
        );
        assert!(codec.decode(&bytes, ty).is_ok(), "{ty}");
    }

    // Field-for-field, through the structured path.
    assert_eq!(
        spat::decode_spat(&spat::encode_spat(&spat).expect("encodes").bytes).expect("decodes"),
        spat
    );
    assert_eq!(
        map::decode_map(&map::encode_map(&map_data).expect("encodes").bytes).expect("decodes"),
        map_data
    );
}

#[test]
fn a_cam_built_from_node_belief_round_trips_through_the_seam() {
    let clock = WallClock::parse_rfc3339("2026-09-18T12:00:00Z").expect("parses");
    let node = view(0, Vec3::new(1_200.0, 800.0, 12.5), 13.89, 0.0);

    let mut input = CamInput::new(
        0x0A0B_0C0D,
        ParticipantType::PassengerCar,
        *node.position(),
        origin(),
        Dims::CAR,
        cam::timestamp_its(clock, node.believed_time()).expect("after the 1609.2 epoch"),
    );
    input.longitudinal_acceleration_mps2 = Some(-1.2);
    input.low_frequency = Some(CamLowFrequency {
        vehicle_role: VehicleRole::Taxi,
        exterior_lights: ExteriorLightMask::LOW_BEAM.with(ExteriorLightMask::HAZARD),
        path_history: (1..=6u64)
            .map(|i| PathHistoryPoint {
                pos: Vec3::new(1_200.0 - i as f64 * 14.0, 800.0, 12.5),
                age: Duration::from_millis(i * 500),
            })
            .collect(),
    });

    let codec = EtsiUperCodec::new();
    let cam = cam::build_cam(&input).expect("builds");
    let encoded: Encoded = codec
        .encode(&Message::Cam(Box::new(cam.clone())))
        .expect("encodes");

    assert_eq!(encoded.size_source, SizeSource::Uper);
    assert!(encoded.is_real());
    assert_eq!(encoded.size as usize, encoded.bytes.len());

    let Message::Cam(back) = codec.decode(&encoded.bytes, MsgType::Cam).expect("decodes") else {
        panic!("a CAM decodes to a CAM");
    };
    assert_eq!(*back, cam);
    assert_eq!(back.header.station_id.0, 0x0A0B_0C0D);
    assert_eq!(
        back.header.message_id.0,
        MsgType::Cam.its_message_id().unwrap()
    );
}

#[test]
fn a_denm_round_trips_and_a_termination_denm_is_shaped_differently() {
    let clock = WallClock::parse_rfc3339("2026-09-18T12:00:00Z").expect("parses");
    let ts = cam::timestamp_its(clock, 0).expect("after the 1609.2 epoch");
    let input = DenmInput::new(
        EventId {
            originating_station_id: 4_242,
            sequence_number: 1,
        },
        4_242,
        ParticipantType::PassengerCar,
        ts,
        belief(0, Vec3::new(900.0, 1_100.0, 8.0), 0.0, 0.0),
        origin(),
        DenmCause::Accident,
    );

    let codec = EtsiUperCodec::new();
    let ordinary = denm::build_denm(&input).expect("builds");
    let cancelled =
        denm::build_termination_denm(&input, TerminationKind::Cancellation).expect("builds");

    for message in [ordinary.clone(), cancelled.clone()] {
        let encoded = codec
            .encode(&Message::Denm(Box::new(message.clone())))
            .expect("encodes");
        let Message::Denm(back) = codec
            .decode(&encoded.bytes, MsgType::Denm)
            .expect("decodes")
        else {
            panic!("a DENM decodes to a DENM");
        };
        assert_eq!(*back, message);
    }

    assert!(ordinary.denm.situation.is_some() && ordinary.denm.location.is_some());
    assert!(cancelled.denm.situation.is_none() && cancelled.denm.location.is_none());
}

#[test]
fn the_size_model_returns_placeholders_that_cannot_be_decoded() {
    // Both size-model codecs, over every type each one claims. Nothing here may produce
    // bytes a caller could mistake for an encoding.
    for codec in v2xw_msg::size_model::codecs() {
        for ty in codec.message_types().to_vec() {
            let request = SizeRequest {
                ty,
                profile: ContentProfile::Typical,
                elements: 4,
            };
            let Ok(encoded) = codec.encode(&Message::Modeled(request)) else {
                // A real codec refuses a Modeled request, which is the correct answer.
                continue;
            };
            assert!(!encoded.is_real(), "{ty} from {}", codec.id());
            assert!(
                matches!(encoded.size_source, SizeSource::SizeModel(_)),
                "{ty} from {}",
                codec.id()
            );
            assert_eq!(encoded.bytes.len(), encoded.size as usize, "{ty}");
            assert!(
                encoded.bytes.iter().all(|&b| b == 0xa5),
                "{ty} is a fill pattern"
            );
            assert!(codec.decode(&encoded.bytes, ty).is_err(), "{ty}");
        }
    }

    // And the retired SPaT and MAP rows are still sizable by name, so a run recorded
    // before the real encoder existed stays interpretable.
    let retired = J2735SizeCodec::new()
        .size_of(&SizeRequest::typical(MsgType::Spat, 8))
        .expect("the retired row is still in the table");
    assert_eq!(retired, 17 + 8 * 10);
}

/// The ETSI codec must refuse a message it does not implement rather than encode something
/// plausible: the whole three-tier scheme depends on nobody silently covering for anybody.
#[test]
fn a_codec_refuses_what_it_does_not_implement() {
    let etsi = EtsiUperCodec::new();
    let err = etsi
        .encode(&Message::Modeled(SizeRequest::typical(MsgType::Spat, 1)))
        .expect_err("the ETSI codec does not size SPaT");
    assert!(err.to_string().contains("spat"), "{err}");

    let modelled = J2735SizeCodec::new();
    let err = modelled
        .encode(&Message::HandEncoded {
            ty: MsgType::Bsm,
            bytes: vec![0; 39],
        })
        .expect_err("the size model does not encode a BSM");
    assert!(err.to_string().contains("bsm"), "{err}");
}

// =========================================================================================
// The generator seam, fully wired
// =========================================================================================

/// One vehicle, one second, through the real seam: a `CamGenerator` behind a trait object
/// parameterised on the engine's own context and node-view types.
#[test]
fn the_cam_generator_drives_a_node_through_the_real_seam() {
    let mut ctx = EngineCtx::new();
    let mut generator: Box<dyn MessageGenerator<EngineCtx, EngineNodeView>> =
        Box::new(CamGenerator::default());

    assert_eq!(generator.check_interval(), Duration::from_millis(100));

    // A vehicle driving east at 13,89 m/s. Every 100 ms it moves 1,389 m, so the 4 m
    // position threshold is crossed every third check and nothing else changes.
    let mut requests: Vec<(u64, GenRequest)> = Vec::new();
    for step in 0..=10u64 {
        let t = step * 100 * NS_PER_MS;
        let node = view(
            t,
            Vec3::new(1_000.0 + 13.89 * step as f64 * 0.1, 500.0, 5.0),
            13.89,
            0.0,
        );
        for request in generator.on_tick(&mut ctx, &node, &DccState::UNRESTRICTED) {
            requests.push((step * 100, request));
        }
    }

    assert_eq!(requests[0].1.reason, GenReason::First);
    assert!(requests[0].1.include_low_frequency);
    assert!(
        requests.iter().all(|(_, r)| r.msg_type == MsgType::Cam),
        "a CAM generator asks for CAMs"
    );
    // 1,389 m per check, so the 4 m threshold is crossed on the third check after each CAM.
    assert_eq!(
        requests.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        vec![0, 300, 600, 900]
    );
    // The low-frequency container goes out at 0 and again at 600 ms, 500 ms later.
    assert_eq!(
        requests
            .iter()
            .filter(|(_, r)| r.include_low_frequency)
            .map(|(t, _)| *t)
            .collect::<Vec<_>>(),
        vec![0, 600]
    );

    // And every decision was recorded on a node-visible channel.
    assert_eq!(ctx.emitted.len(), requests.len());
    for (channel, visibility, json) in &ctx.emitted {
        assert_eq!(*channel, <GenerationRecord as Record>::CHANNEL);
        assert_eq!(*visibility, Visibility::Node);
        assert!(
            !visibility.is_gt_tainted(),
            "a generation record is not ground truth"
        );
        assert!(json.contains("\"msg_type\":\"cam\""), "{json}");
    }
}

#[test]
fn the_bsm_generator_drives_a_node_at_ten_hertz_through_the_real_seam() {
    let mut ctx = EngineCtx::new();
    let mut generator: Box<dyn MessageGenerator<EngineCtx, EngineNodeView>> =
        Box::new(BsmGenerator::default());

    let mut times = Vec::new();
    for step in 0..20u64 {
        let t = step * 50 * NS_PER_MS;
        let node = view(t, Vec3::new(2_000.0, 300.0, 3.0), 20.0, 0.0);
        for request in generator.on_tick(&mut ctx, &node, &DccState::UNRESTRICTED) {
            assert_eq!(request.msg_type, MsgType::Bsm);
            assert!(
                !request.include_low_frequency,
                "a BSM has no low-frequency container"
            );
            times.push(step * 50);
        }
    }
    assert_eq!(times, vec![0, 100, 200, 300, 400, 500, 600, 700, 800, 900]);
}

/// A generator reads the node's **belief**, so a node whose GNSS is lying sends CAMs on the
/// lie. This is invariant I-C2 made observable: if the generator reached into ground truth
/// the encoded position would follow the true trajectory instead.
#[test]
fn a_generator_encodes_what_the_node_believes_not_where_it_is() {
    let clock = WallClock::parse_rfc3339("2026-09-18T12:00:00Z").expect("parses");
    let spoofed = Vec3::new(5_000.0, 5_000.0, 0.0);
    let node = view(0, spoofed, 13.89, 0.0);

    let input = CamInput::new(
        7,
        ParticipantType::PassengerCar,
        *node.position(),
        origin(),
        Dims::CAR,
        cam::timestamp_its(clock, 0).expect("after the 1609.2 epoch"),
    );
    let encoded = cam::encode_cam(&cam::build_cam(&input).expect("builds")).expect("encodes");
    let decoded = cam::decode_cam(&encoded.bytes).expect("decodes");

    let (lat, lon, _) = origin().to_geodetic(spoofed);
    let rp = &decoded
        .cam
        .cam_parameters
        .basic_container
        .reference_position;
    assert!((f64::from(rp.latitude.0) / 1e7 - lat).abs() < 2e-7);
    assert!((f64::from(rp.longitude.0) / 1e7 - lon).abs() < 2e-7);
}

// =========================================================================================
// Security types, as v2xw-sec will reach them
// =========================================================================================

/// `v2xw-sec` imports the 1609.2 types **from here**, and this is the import line it uses.
/// If the re-export shape ever changed, this test is what fails — inside `v2xw-msg`, where
/// the fix belongs, rather than in the security crate.
#[test]
fn the_security_types_are_reachable_and_encode_with_coer() {
    use v2xw_msg::sec_types::{Ieee1609Dot2Content, Ieee1609Dot2Data, Opaque, Uint8, coer};

    let data = Ieee1609Dot2Data::new(
        Uint8(3),
        Ieee1609Dot2Content::unsecuredData(Opaque(rasn::types::OctetString::from_static(
            b"a CAM would go here",
        ))),
    );

    let encoded = coer::encoded(MsgType::Cam, &data).expect("encodes");
    assert_eq!(encoded.size_source, SizeSource::Coer);
    assert!(encoded.is_real());

    let back: Ieee1609Dot2Data = coer::decode(MsgType::Cam, &encoded.bytes).expect("decodes");
    assert_eq!(back, data);
}

/// The `sec_types` module must expose every name a security implementation needs, by the
/// path it will use. Compiling this function is the assertion.
#[test]
fn the_security_type_surface_is_complete() {
    #[allow(unused_imports)]
    use v2xw_msg::sec_types::{
        Certificate, CertificateBase, CertificateId, CertificateType, CrlContents,
        EccP256CurvePoint, EndEntityType, EtsiTs103097Certificate, EtsiTs103097Data,
        ExplicitCertificate, HashAlgorithm, HashedId3, HashedId8, HeaderInfo, Ieee1609Dot2Content,
        Ieee1609Dot2Data, Ieee1609Duration, ImplicitCertificate, IssuerIdentifier, LinkageData,
        Psid, PsidGroupPermissions, PublicVerificationKey, SecuredCrl, SequenceOfCertificate,
        Signature, SignedData, SignedDataPayload, SignerIdentifier, Time32, Time64,
        ToBeSignedCertificate, ToBeSignedData, ValidityPeriod, coer, etsi_ts103097_module,
        ieee1609_dot2, ieee1609_dot2_base_types, ieee1609_dot2_crl, ieee1609_dot2_crl_base_types,
    };
}

// =========================================================================================
// Determinism
// =========================================================================================

/// The whole point of the crate in one test: the same inputs produce the same bytes, twice,
/// in the same process and across an independent rebuild of the message.
#[test]
fn encoded_bytes_are_stable_across_runs() {
    let clock = WallClock::parse_rfc3339("2026-09-18T12:00:00Z").expect("parses");
    let build = || {
        let node = view(0, Vec3::new(1_234.567, 890.123, 11.5), 17.5, 0.0);
        let mut input = CamInput::new(
            0xDEAD_BEEF,
            ParticipantType::HeavyTruck,
            *node.position(),
            origin(),
            Dims::new(12.0, 2.5, 4.0),
            cam::timestamp_its(clock, 0).expect("after the 1609.2 epoch"),
        );
        input.longitudinal_acceleration_mps2 = Some(-3.456);
        input.yaw_rate_rad_s = Some(-0.1234);
        input.curvature_inv_m = Some(0.0123);
        cam::encode_cam(&cam::build_cam(&input).expect("builds")).expect("encodes")
    };
    let first = build();
    for _ in 0..32 {
        assert_eq!(build(), first);
    }
    // And the size is the length of the bytes, always.
    assert_eq!(first.size as usize, first.bytes.len());
}

/// The generator's decisions must depend only on its inputs, so the same drive produces the
/// same CAM schedule however many times it is replayed.
#[test]
fn the_generator_schedule_is_stable_across_runs() {
    let drive = || {
        let mut ctx = EngineCtx::new();
        let mut generator = CamGenerator::default();
        let mut out = Vec::new();
        for step in 0..100u64 {
            let t = step * 50 * NS_PER_MS;
            let node = view(
                t,
                Vec3::new(1_000.0 + 0.6 * step as f64, 500.0 + 0.2 * step as f64, 5.0),
                12.0 + 0.01 * step as f64,
                0.002 * step as f64,
            );
            out.extend(
                MessageGenerator::<EngineCtx, EngineNodeView>::on_tick(
                    &mut generator,
                    &mut ctx,
                    &node,
                    &DccState::UNRESTRICTED,
                )
                .into_iter()
                .map(|r| (t, r.reason, r.include_low_frequency)),
            );
        }
        out
    };
    assert_eq!(drive(), drive());
    assert!(!drive().is_empty());
}

/// 03-interfaces.md §6 claims that, with the `?Sized` bounds, the generic parameters of
/// [`MessageGenerator`] may themselves be trait objects — so a caller that only has
/// `&mut dyn Ctx<…>` and `&dyn NodeView<…>` can still drive a generator. Build decision
/// D11 exists because a document once published a signature nobody had compiled, so the
/// claim is compiled here.
#[test]
fn a_generator_works_over_trait_objects_as_well_as_concrete_types() {
    type DynCtx = dyn Ctx<World = (), Actors = Vec<ActorId>, Payload = &'static str>;
    type DynView = dyn NodeView<Neighbors = Vec<NodeId>, Credential = u32, Message = Vec<u8>>;

    let mut ctx = EngineCtx::new();
    let node = view(0, Vec3::new(10.0, 20.0, 0.0), 5.0, 0.0);

    // The generator behind a trait object whose own parameters are trait objects.
    let mut generator: Box<dyn MessageGenerator<DynCtx, DynView>> =
        Box::new(CamGenerator::default());
    let dyn_ctx: &mut DynCtx = &mut ctx;
    let dyn_view: &DynView = &node;

    let requests = generator.on_tick(dyn_ctx, dyn_view, &DccState::UNRESTRICTED);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].reason, GenReason::First);
}
