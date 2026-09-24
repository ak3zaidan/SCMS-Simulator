//! Model cards: every default cited, every uncited default planned, every model registers.

use v2xw_core::card::{Family, ModelCard, Parameter, Source, SourceKind, Tier};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::registry::Registry;
use v2xw_proto::etsi::ts102941::{EtsiTs102941, SEPARATIONS as ETSI_SEPARATIONS};
use v2xw_proto::scms::{CAMP_SCMS_ID, CampScms, SEPARATIONS};
use v2xw_proto::spec::separation_violations;

#[test]
fn every_model_in_the_crate_registers() {
    let mut registry = Registry::new();
    let refs = v2xw_proto::register_all(&mut registry).expect("all register");
    assert_eq!(refs.len(), 2, "the SCMS plug-in and the ETSI skeleton");
    assert!(registry.contains(CAMP_SCMS_ID));
    assert!(registry.contains(v2xw_proto::etsi::ETSI_TS102941_ID));
    assert!(
        v2xw_proto::register_all(&mut registry).is_err(),
        "registering twice must fail rather than shadow"
    );
}

#[test]
fn no_parameter_is_uncited_without_a_calibration_plan() {
    for model in [
        Box::new(CampScms::default()) as Box<dyn Model>,
        Box::new(EtsiTs102941::default()),
    ] {
        let card = model.card();
        card.validate().expect("the card validates");
        for p in card.todo_calibrate() {
            assert!(
                p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty()),
                "{}: parameter `{}` is todo-calibrate with no plan",
                card.id,
                p.name
            );
        }
    }
}

#[test]
fn every_cited_parameter_names_a_clause() {
    let card = CampScms::default().card().clone();
    for p in &card.parameters {
        if p.source.kind == SourceKind::TodoCalibrate {
            continue;
        }
        assert!(
            p.source.reference.len() > 10,
            "parameter `{}` cites `{}`, which is not a clause",
            p.name,
            p.source.reference
        );
    }
}

#[test]
fn the_uncited_size_parameters_are_all_on_the_card() {
    let card = CampScms::default().card().clone();
    for name in v2xw_proto::sizes::SizeParams::NAMES {
        let p = card
            .parameters
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("`{name}` is used by the size model but is not on the card"));
        assert_eq!(p.source.kind, SourceKind::TodoCalibrate);
        assert!(p.calibration.is_some());
    }
}

#[test]
fn rule_r1_is_enforced_and_can_fail() {
    // The injected fault: a card with a todo-calibrate parameter and no plan must be
    // rejected, or the check above is decoration.
    let mut card = ModelCard::new("protocol/test/fault", Family::Protocol, "1.0.0", "fault");
    card.tier = vec![Tier::Medium];
    card.parameters = vec![Parameter::new(
        "made_up",
        "-",
        serde_json::json!(1),
        Source::todo_calibrate("no source"),
    )];
    assert!(
        card.validate().is_err(),
        "a todo-calibrate parameter with no plan must not validate"
    );
    let mut fixed = card.clone();
    fixed.parameters[0].calibration = Some("measure it".into());
    assert!(fixed.validate().is_ok());
}

#[test]
fn the_mandatory_separations_are_checked_and_can_fail() {
    // A legal placement: every role on its own node.
    let legal: Vec<(&'static str, NodeId)> = vec![
        ("RA", NodeId::new(1)),
        ("PCA", NodeId::new(2)),
        ("LA1", NodeId::new(3)),
        ("LA2", NodeId::new(4)),
        ("MA", NodeId::new(5)),
        ("LOP", NodeId::new(7)),
    ];
    assert!(separation_violations(SEPARATIONS, &legal).is_empty());

    // Co-hosting the two Linkage Authorities is the one that would let a single
    // organisation link a device unilaterally.
    let mut illegal = legal.clone();
    illegal[3] = ("LA2", NodeId::new(3));
    let violations = separation_violations(SEPARATIONS, &illegal);
    assert_eq!(violations.len(), 1);
    assert_eq!((violations[0].a, violations[0].b), ("LA1", "LA2"));
    assert!(!violations[0].reason.is_empty());

    // And the ETSI skeleton declares the EA/AA separation.
    let etsi_illegal = vec![("EA", NodeId::new(101)), ("AA", NodeId::new(101))];
    assert_eq!(
        separation_violations(ETSI_SEPARATIONS, &etsi_illegal).len(),
        1
    );
}

#[test]
fn the_scms_plug_in_describes_itself_completely() {
    let scms = CampScms::default();
    let roles = scms.roles();
    assert_eq!(roles.len(), 13, "11 online roles and 2 offline ones");
    assert_eq!(roles.iter().filter(|r| r.offline).count(), 2);
    assert_eq!(scms.credential_types().len(), 2);
    assert_eq!(scms.flows().len(), 7);
    assert_eq!(scms.separations().len(), 10);
    assert_eq!(scms.primitives().len(), 4);
    assert!(matches!(
        scms.revocation(),
        v2xw_proto::spec::RevocationMechanism::Both(..)
    ));
    // Every role that is online has a hardware profile and at least one server.
    for r in roles.iter().filter(|r| !r.offline) {
        assert!(!r.default_profile.is_empty(), "{} has no profile", r.name);
        assert!(r.default_service.servers >= 1);
    }
}

#[test]
fn the_etsi_plug_in_declares_both_halves_of_its_revocation() {
    let etsi = EtsiTs102941::default();
    // 05-protocols.md §4.3: "`Passive` for vehicles (EA blocklist); `Active` CA-CRL only
    // (series: CA certificates)". It used to answer `Passive` alone, which was true of the
    // skeleton and not of the protocol.
    let (active, passive) = match etsi.revocation() {
        v2xw_proto::spec::RevocationMechanism::Both(a, p) => (a, p),
        other => panic!("expected both halves, got {other:?}"),
    };
    assert_eq!(passive.blocklist_at, "EA");
    // A CA-CRL entry is a HashedId8 with an expiry: twelve bytes, against the SCMS's
    // ~40 B of linkage seeds. That asymmetry is the point of comparing the two.
    assert_eq!(active.entry.bytes(), 12);
    assert!(
        active
            .stages
            .contains(&v2xw_proto::stage::StageId::Enforced)
    );
    assert!(
        passive
            .stages
            .contains(&v2xw_proto::stage::StageId::LastValidCredentialExpiry)
    );

    // EU Certificate Policy §7.2.1: preload ≤ 3 months, AT validity ≤ 1 week, so the
    // worst-case eviction lag is 97 days.
    assert_eq!(
        etsi.passive_eviction_bound().as_nanos(),
        97 * 86_400 * 1_000_000_000
    );
}

#[test]
fn the_etsi_plug_in_describes_all_seven_of_its_flows() {
    let etsi = EtsiTs102941::default();
    assert_eq!(etsi.flows().len(), 2, "the two the skeleton started with");
    assert_eq!(etsi.deferred_flows().len(), 5);
    assert_eq!(etsi.all_flows().len(), 7);
    assert_eq!(etsi.roles().len(), 6);
    assert_eq!(etsi.credential_types().len(), 2);
    // Every framing parameter a message size rests on is on the card with a plan, which is
    // invariant I-P8's other half: `tests/wire_sizes.rs` walks the sizes, this walks the
    // card.
    let card = etsi.card().clone();
    for name in [
        "etsi_subject_attributes_bytes",
        "etsi_butterfly_response_bytes",
        "etsi_ctl_framing_bytes",
        "etsi_report_payload_bytes",
    ] {
        let p = card
            .parameters
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("`{name}` backs a wire size but is not on the card"));
        assert_eq!(p.source.kind, SourceKind::TodoCalibrate, "{name}");
        assert!(
            p.calibration.as_ref().is_some_and(|c| c.len() > 40),
            "{name} has no usable plan"
        );
    }
}
