//! Invariant I-P8: every byte count on the wire comes from the real encoder, a cited
//! constant, or a model-card parameter that carries a calibration plan.
//!
//! The third case is the one that could be abused, so it is the one checked hardest: the
//! parameter name a size claims to rest on must actually be on the card, and that card
//! parameter must actually carry a non-empty plan. A number invented in the size model
//! cannot pass.

mod common;

use std::collections::BTreeSet;

use common::{DEVICE_A, DEVICE_B, deployment, provisioned};
use v2xw_core::card::SourceKind;
use v2xw_core::model::Model;
use v2xw_proto::etsi::ts102941::{EtsiParams, EtsiRun, EtsiTs102941};
use v2xw_proto::scms::CampScms;
use v2xw_proto::sizes::{CertificateSizes, SizeProvenance};

/// The card parameters that carry a calibration plan, by name.
fn planned_parameters(model: &dyn Model) -> BTreeSet<String> {
    model
        .card()
        .parameters
        .iter()
        .filter(|p| {
            p.source.kind == SourceKind::TodoCalibrate
                && p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty())
        })
        .map(|p| p.name.clone())
        .collect()
}

fn check_table(
    entries: &[(&'static str, v2xw_proto::sizes::WireSize)],
    planned: &BTreeSet<String>,
) {
    for (name, size) in entries {
        assert!(size.bytes() > 0, "{name}: a message cannot be zero bytes");
        match size.provenance() {
            SizeProvenance::RealEncoder { what } => {
                assert!(!what.trim().is_empty(), "{name}: empty encoder description");
            }
            SizeProvenance::Cited { citation } => {
                assert!(!citation.trim().is_empty(), "{name}: empty citation");
            }
            SizeProvenance::Derived {
                expression,
                citation,
            } => {
                assert!(!expression.trim().is_empty(), "{name}: empty expression");
                assert!(!citation.trim().is_empty(), "{name}: empty citation");
            }
            SizeProvenance::Parameter { param } => {
                assert!(
                    planned.contains(param),
                    "{name} rests on `{param}`, which is not a card parameter with a \
                     calibration plan — that is an invented number"
                );
            }
        }
    }
}

#[test]
fn every_scms_message_size_has_a_provenance_the_card_backs() {
    let planned = planned_parameters(&CampScms::default());
    let run = deployment(0);
    check_table(&run.state.sizes.table(), &planned);
}

#[test]
fn every_etsi_message_size_has_a_provenance_the_card_backs() {
    let planned = planned_parameters(&EtsiTs102941::default());
    let run = EtsiRun::new(EtsiParams::default()).expect("encodes");
    check_table(&run.sizes.table(), &planned);
}

#[test]
fn every_step_that_actually_put_bytes_on_a_link_is_in_the_size_table() {
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, 2, 2);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("runs");
    run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("runs");
    run.distribute_crl(DEVICE_A);
    run.run().expect("runs");

    let known: BTreeSet<&'static str> = run
        .state
        .sizes
        .table()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let mut seen = BTreeSet::new();
    for step in &run.kernel.steps {
        assert!(step.bytes > 0, "{} carried no bytes", step.step);
        assert!(
            known.contains(step.step),
            "step `{}` put {} bytes on a link with no entry in the size table",
            step.step,
            step.bytes
        );
        seen.insert(step.step);
    }
    // And the run really did exercise most of the table, so the check above is not
    // vacuously true.
    assert!(
        seen.len() >= 20,
        "only {} of {} message kinds were exercised",
        seen.len(),
        known.len()
    );
}

#[test]
fn certificate_sizes_are_the_real_encoders_output() {
    let sizes = CertificateSizes::measured().expect("encodes");
    for (what, size) in [
        ("pseudonym", sizes.pseudonym),
        ("enrolment", sizes.enrolment),
        ("authority", sizes.authority),
    ] {
        assert!(
            matches!(size.provenance(), SizeProvenance::RealEncoder { .. }),
            "{what} certificate must be sized by the encoder"
        );
        assert!(size.bytes() > 0);
    }
    // An implicit certificate is smaller than an explicit one: there is no signature in
    // it, which is the whole point of ECQV (67 B of signature in the 1609.2 COER shape).
    assert!(
        sizes.pseudonym.bytes() < sizes.enrolment.bytes(),
        "implicit {} B is not smaller than explicit {} B",
        sizes.pseudonym.bytes(),
        sizes.enrolment.bytes()
    );
}

#[test]
fn the_crl_size_expression_reproduces_the_published_figure() {
    // Brecht 2018 §VI-F: 10,000 entries is about 400 kB. The expression must land there,
    // or the 40 B per entry it claims to use is not the number it is using.
    let run = deployment(0);
    let bytes = run.state.sizes.crl(10_000).bytes();
    assert!(
        (395_000..=410_000).contains(&bytes),
        "a 10,000-entry CRL came out at {bytes} B, not ≈ 400 kB"
    );
    // And it is linear in the entry count, with the envelope and the signer's certificate
    // as the only constant term.
    let empty = run.state.sizes.crl(0).bytes();
    assert_eq!(run.state.sizes.crl(100).bytes() - empty, 4_000);
}

#[test]
fn a_bigger_batch_costs_proportionally_more_on_the_wire() {
    let run = deployment(0);
    let one = run.state.sizes.batch_download(1).bytes();
    let twenty = run.state.sizes.batch_download(20).bytes();
    let per_cert = (twenty - one) / 19;
    assert_eq!(
        per_cert,
        run.state.sizes.certs.pseudonym.bytes() + 65,
        "each extra certificate costs its encoded size plus the ECIES wrapper"
    );
}
