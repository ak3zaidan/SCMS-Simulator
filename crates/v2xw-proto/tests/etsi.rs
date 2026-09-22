//! The ETSI TS 102 941 skeleton: the two built flows, and passive revocation.

use v2xw_core::ids::NodeId;
use v2xw_proto::etsi::ts102941::{EtsiParams, EtsiRun};
use v2xw_proto::stage::StageId;

const STATION: NodeId = NodeId::new(2_000);

fn deployment() -> EtsiRun {
    let mut run = EtsiRun::new(EtsiParams::default()).expect("encodes");
    run.add_station(STATION);
    run
}

#[test]
fn authorization_takes_the_aa_to_ea_round_trip() {
    let mut run = deployment();
    run.enrol(STATION);
    run.run().expect("runs");
    let auth = run.authorize(STATION);
    run.run().expect("runs");

    // The station never talks to the EA during authorization; the AA does, and the EA
    // never sees the ticket keys [TS 102 941 §6.1.4 NOTE 1].
    let steps: Vec<_> = run.kernel.steps.iter().map(|s| s.step).collect();
    assert!(steps.contains(&"etsi-validation-request"));
    assert!(steps.contains(&"etsi-validation-response"));
    let station_to_ea = run.kernel.steps.iter().any(|s| {
        s.from == STATION
            && s.to == run.nodes.ea
            && s.flow == v2xw_proto::stage::FlowId::EtsiAuthorization
    });
    assert!(
        !station_to_ea,
        "the station must reach the EA only through the AA"
    );
    assert_eq!(run.tickets.get(&STATION), Some(&1));
    assert!(run.kernel.stages.at(auth, StageId::Certified).is_some());
}

#[test]
fn a_blocklisted_station_is_starved_rather_than_revoked() {
    // Passive revocation: no CRL, no list, no broadcast — the EA simply refuses the next
    // authorization request [TS 102 941 §6.1.6; EUCP §7.3.2].
    let mut run = deployment();
    run.enrol(STATION);
    run.run().expect("runs");
    run.authorize(STATION);
    run.run().expect("runs");
    assert_eq!(run.tickets.get(&STATION), Some(&1));

    run.blocklist(STATION);
    let refused_run = run.authorize(STATION);
    run.run().expect("runs");

    assert_eq!(run.refused, 1);
    assert_eq!(
        run.tickets.get(&STATION),
        Some(&1),
        "no further ticket may be issued"
    );
    assert!(
        run.kernel
            .stages
            .at(refused_run, StageId::Certified)
            .is_none(),
        "a refused authorization must not stamp `certified`"
    );
    assert!(
        run.kernel
            .stages
            .at(refused_run, StageId::Requested)
            .is_some(),
        "but the attempt itself is still on the record"
    );
}

#[test]
fn a_station_that_never_enrolled_cannot_be_authorized() {
    let mut run = deployment();
    run.authorize(STATION);
    run.run().expect("runs");
    assert_eq!(run.refused, 1);
    assert!(run.tickets.is_empty());
}

#[test]
fn the_pool_size_and_preload_are_the_certificate_policys() {
    let run = deployment();
    assert_eq!(run.params.at_concurrent, 100);
    assert_eq!(
        run.params.at_preload.as_nanos(),
        90 * 86_400 * 1_000_000_000
    );
    assert_eq!(
        run.params.at_validity.as_nanos(),
        7 * 86_400 * 1_000_000_000
    );
}
