//! The threshold/umbrella hook: the shape is exercised, no scheme is invented.

use v2xw_core::ids::NodeId;
use v2xw_proto::sizes::WireSize;
use v2xw_proto::spec::Centrality;
use v2xw_proto::stage::FlowId;
use v2xw_proto::threshold::{
    Committee, FrostShapedPlaceholder, ThresholdOp, ThresholdProtocol, UmbrellaScheme,
};

fn committee(n: u16, t: u16) -> Committee {
    Committee {
        n,
        t,
        members: (0..n).map(|k| NodeId::new(500 + u32::from(k))).collect(),
        coordinator: Some(NodeId::new(499)),
    }
}

#[test]
fn a_committee_appears_as_a_distributed_role() {
    let c = committee(7, 4);
    assert_eq!(c.centrality(), Centrality::Distributed { n: 7, t: 4 });
    assert_eq!(c.members.len(), 7);
}

#[test]
fn every_operation_has_rounds_bytes_and_a_flow() {
    let p = FrostShapedPlaceholder::default();
    let c = committee(5, 2);
    for op in [ThresholdOp::Dkg, ThresholdOp::Sign, ThresholdOp::Refresh] {
        assert!(p.rounds_of(op, &c) > 0, "{op:?} must take rounds");
        assert!(p.bytes_of(op, &c) > 0, "{op:?} must cost bytes");
    }
    assert_eq!(p.flow_of(ThresholdOp::Dkg), FlowId::ThresholdDkg);
    assert_eq!(p.flow_of(ThresholdOp::Sign), FlowId::ThresholdSign);
    assert_eq!(p.flow_of(ThresholdOp::Presign), FlowId::ThresholdSign);
    assert_eq!(p.flow_of(ThresholdOp::Refresh), FlowId::ThresholdRefresh);
    // The placeholder is not presignable; a CGGMP21-shaped scheme would be, and the
    // surface already has the call.
    assert!(p.presign_rounds(&c).is_empty());
}

#[test]
fn signing_costs_scale_with_the_threshold_and_dkg_with_the_committee() {
    let p = FrostShapedPlaceholder::default();
    let small = p.bytes_of(ThresholdOp::Sign, &committee(5, 1));
    let large = p.bytes_of(ThresholdOp::Sign, &committee(5, 4));
    assert!(
        large > small,
        "more signers must cost more: {large} vs {small}"
    );
    let few = p.bytes_of(ThresholdOp::Dkg, &committee(3, 1));
    let many = p.bytes_of(ThresholdOp::Dkg, &committee(9, 4));
    assert!(many > few, "a broadcast round costs n − 1 messages");
}

#[test]
fn a_round_that_expects_several_attempts_costs_several_attempts() {
    // The interface's answer to CELI25's 4–5 attempts per threshold ML-DSA signature: the
    // cost must multiply, and nothing here supplies the number.
    let p = FrostShapedPlaceholder::default();
    let c = committee(4, 2);
    let mut rounds = p.sign_rounds(&c);
    let once = rounds.iter().map(|r| r.bytes(c.n)).sum::<u64>();
    for r in &mut rounds {
        r.attempts = 5;
    }
    let five = rounds.iter().map(|r| r.bytes(c.n)).sum::<u64>();
    assert_eq!(five, 5 * once);
}

#[test]
fn the_placeholders_sizes_are_all_cited() {
    let p = FrostShapedPlaceholder::default();
    let c = committee(3, 1);
    let mut sizes: Vec<WireSize> = vec![p.signature_bytes(), p.verification_key_bytes()];
    for op in [ThresholdOp::Dkg, ThresholdOp::Sign, ThresholdOp::Refresh] {
        let rounds = match op {
            ThresholdOp::Dkg => p.dkg_rounds(&c),
            ThresholdOp::Sign => p.sign_rounds(&c),
            _ => p.refresh_rounds(&c),
        };
        sizes.extend(
            rounds
                .iter()
                .flat_map(|r| r.messages.iter().map(|m| m.bytes)),
        );
    }
    for s in sizes {
        let citation = s
            .provenance()
            .citation()
            .expect("a placeholder size must still cite its source");
        assert!(!citation.trim().is_empty());
        assert!(s.bytes() > 0);
    }
}

#[test]
fn an_umbrella_scheme_can_be_described_without_being_invented() {
    let scheme = UmbrellaScheme {
        leaves_per_umbrella: 20,
        leaf_is_derived_locally: true,
        umbrella_bytes: WireSize::cited(
            2_420,
            "FIPS 204 Table 2: an ML-DSA-44 signature is 2,420 B",
        ),
        leaf_bytes: WireSize::cited(80, "SEC 4 §3.4; 04-models.md §9.2"),
    };
    assert!(scheme.leaf_is_derived_locally);
    assert_eq!(scheme.leaves_per_umbrella, 20);
    assert!(scheme.umbrella_bytes.bytes() > scheme.leaf_bytes.bytes());
}

#[test]
fn the_refresh_policy_is_a_parameter_and_not_a_constant() {
    let p = FrostShapedPlaceholder::default();
    assert!(p.refresh_policy().invalidates_previous);
    assert_eq!(p.refresh_policy().epoch.as_nanos(), 86_400 * 1_000_000_000);
    assert_eq!(p.name(), "threshold/frost-shaped-placeholder");
}
