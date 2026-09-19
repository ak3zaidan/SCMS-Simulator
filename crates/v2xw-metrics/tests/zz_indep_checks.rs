//! INDEPENDENT VERIFIER INSTRUMENTATION — temporary, removed after the run.
//!
//! Each repaired check, given the exact fault it is supposed to catch, from a test that
//! was written against the specification rather than against the repair.

use std::collections::BTreeMap;

use v2xw_core::ctx::Visibility;
use v2xw_metrics::def::{Agg, DEFAULT_LEVEL, Dims, MetricDef, MetricSample, SampleValue};
use v2xw_metrics::invariants::check_d9_quantisation;
use v2xw_metrics::quant::Quantum;
use v2xw_metrics::stats::{
    DistributionSummary, Estimate, Proportion, RatioEstimate, ratio_of_sums,
};
use v2xw_metrics::summary::RunSummary;

fn def(name: &str, q: Quantum) -> MetricDef {
    MetricDef::new(
        name,
        "ratio",
        Agg::Mean,
        Visibility::Node,
        q,
        "a metric for the verifier's injection",
    )
    .not_accounting_for("being synthetic")
}

fn sample(d: &MetricDef, v: SampleValue) -> MetricSample {
    MetricSample::new(d, 0, Dims::new(), v)
}

// ------------------------------------------------------------------ D9

/// The D9 guard, given the four faults it claims to catch, on every shape of value.
///
/// The fault that matters is the third: a value that WAS quantised, onto the wrong grid.
/// The guard that could not fail (`!s.quantum.holds(f) && !Quantum::PROBABILITY.holds(f)`)
/// accepted every one of these, because 1e-6 is the finest grid in the crate.
#[test]
fn indep_d9_catches_every_wrong_grid_i_can_construct() {
    let d = def("pdr", Quantum::RATIO); // 1e-4
    assert!(Quantum::RATIO.get() > Quantum::PROBABILITY.get());

    // Wrong grid: on 1e-6, off 1e-4.
    let wrong = Quantum::PROBABILITY.quantise(1.0 / 3.0);
    assert!(Quantum::PROBABILITY.holds(wrong) && !Quantum::RATIO.holds(wrong));
    // Raw.
    let raw = 1.0 / 3.0;

    let mut cases: Vec<(&str, MetricSample, usize)> = Vec::new();

    let mut s = sample(&d, SampleValue::Scalar(Estimate::Value { point: 0.5, n: 9 }));
    s.value = SampleValue::Scalar(Estimate::Value {
        point: wrong,
        n: 9,
    });
    cases.push(("scalar on the wrong grid", s, 1));

    let mut s = sample(&d, SampleValue::Scalar(Estimate::Value { point: 0.5, n: 9 }));
    s.value = SampleValue::Scalar(Estimate::Value { point: raw, n: 9 });
    cases.push(("scalar raw", s, 1));

    // A proportion whose POINT is off the metric's grid but on the probability grid —
    // the exact shape the swallowed disjunction was blind to.
    let good = Proportion::from_counts(30, 40).estimate(1, DEFAULT_LEVEL);
    let mut s = sample(&d, SampleValue::Ratio(good.clone()));
    if let SampleValue::Ratio(RatioEstimate::Proportion {
        ci_lo,
        ci_hi,
        trials,
        successes,
        ..
    }) = s.value.clone()
    {
        s.value = SampleValue::Ratio(RatioEstimate::Proportion {
            point: wrong,
            ci_lo,
            ci_hi,
            trials,
            successes,
            level: DEFAULT_LEVEL,
        });
    } else {
        panic!("expected a proportion");
    }
    cases.push(("proportion point on the wrong grid", s, 1));

    // A proportion whose BOUNDS are off the probability grid: the exemption must not be
    // an unconditional pass for a bound either.
    let mut s = sample(&d, SampleValue::Ratio(good.clone()));
    if let SampleValue::Ratio(RatioEstimate::Proportion {
        point,
        trials,
        successes,
        ..
    }) = s.value.clone()
    {
        s.value = SampleValue::Ratio(RatioEstimate::Proportion {
            point,
            ci_lo: raw,
            ci_hi: raw + 0.1,
            trials,
            successes,
            level: DEFAULT_LEVEL,
        });
    }
    cases.push(("proportion bounds raw", s, 2));

    // A ratio of sums whose sums are off the 1e-3 sum grid.
    let mut s = sample(&d, SampleValue::Ratio(ratio_of_sums(3.0, 7.0, 40, 1)));
    if let SampleValue::Ratio(RatioEstimate::RatioOfSums { point, n, .. }) = s.value.clone() {
        s.value = SampleValue::Ratio(RatioEstimate::RatioOfSums {
            point,
            numerator: raw,
            denominator: 7.000_000_1,
            n,
        });
    }
    cases.push(("ratio-of-sums sums raw", s, 2));

    // A distribution with one percentile off the grid.
    let dist = {
        let mut dd = v2xw_metrics::stats::Distribution::new();
        for i in 0..12 {
            assert!(dd.observe(f64::from(i) / 4.0));
        }
        dd.summary(1)
    };
    let mut s = sample(&d, SampleValue::Distribution(dist));
    if let SampleValue::Distribution(DistributionSummary::Summary {
        min,
        max,
        mean,
        p50,
        p99,
        n,
        interpolation,
        rejected,
        ..
    }) = s.value.clone()
    {
        s.value = SampleValue::Distribution(DistributionSummary::Summary {
            min,
            max,
            mean,
            p50,
            p95: wrong,
            p99,
            n,
            interpolation,
            rejected,
        });
    }
    cases.push(("distribution p95 on the wrong grid", s, 1));

    // Non-finite: `is_on_grid` says true for every NaN and infinity, so a grid test alone
    // is structurally blind to them.
    for (name, bad) in [
        ("NaN", f64::NAN),
        ("+inf", f64::INFINITY),
        ("-inf", f64::NEG_INFINITY),
    ] {
        let mut s = sample(&d, SampleValue::Scalar(Estimate::Value { point: 0.5, n: 9 }));
        s.value = SampleValue::Scalar(Estimate::Value { point: bad, n: 9 });
        cases.push((Box::leak(format!("scalar {name}").into_boxed_str()), s, 1));
    }

    let mut caught = 0;
    for (what, s, want) in cases {
        let o = check_d9_quantisation(std::slice::from_ref(&s));
        assert!(!o.held(), "D9 did NOT catch: {what}");
        assert_eq!(o.violations.len(), want, "{what}: wrong violation count");
        caught += 1;
    }
    println!("INDEP-D9 {caught} injected faults, every one reported");
}

/// …and no false positives on the grids the writer itself uses.
#[test]
fn indep_d9_does_not_report_the_writers_own_correct_output() {
    // A metric on the COARSEST grid in the crate, where a scan against the metric's own
    // quantum would fire on the bounds and the sums.
    for q in [Quantum::DB, Quantum::RATIO, Quantum::LENGTH_M] {
        let d = def("x", q);
        let samples = vec![
            sample(
                &d,
                SampleValue::Ratio(Proportion::from_counts(30, 40).estimate(1, DEFAULT_LEVEL)),
            ),
            sample(&d, SampleValue::Ratio(ratio_of_sums(1.0 / 3.0, 7.0, 40, 1))),
            sample(
                &d,
                SampleValue::Scalar(Estimate::Value {
                    point: 1.0 / 3.0,
                    n: 9,
                }),
            ),
            sample(&d, SampleValue::count(17)),
        ];
        let o = check_d9_quantisation(&samples);
        assert!(o.held(), "false positive on {:?}: {:?}", q, o.violations);
        assert!(o.checked >= 7, "only {} floats scanned", o.checked);
    }
    println!("INDEP-D9 no false positives on DB, RATIO or METRE metrics");
}

/// `floats()` feeds the digest and `graded_floats()` feeds the scan. If they ever
/// disagreed, a float could be digested and never scanned.
#[test]
fn indep_the_scanned_list_and_the_digested_list_are_the_same_floats() {
    let d = def("x", Quantum::RATIO);
    let values = vec![
        SampleValue::Scalar(Estimate::Value { point: 0.25, n: 4 }),
        SampleValue::Scalar(Estimate::Insufficient { n: 1, required: 30 }),
        SampleValue::Ratio(Proportion::from_counts(3, 10).estimate(1, DEFAULT_LEVEL)),
        SampleValue::Ratio(ratio_of_sums(2.0, 8.0, 40, 1)),
        SampleValue::Ratio(ratio_of_sums(2.0, 0.0, 40, 1)),
        SampleValue::count(9),
        SampleValue::Distribution({
            let mut dd = v2xw_metrics::stats::Distribution::new();
            for i in 0..12 {
                assert!(dd.observe(f64::from(i)));
            }
            dd.summary(1)
        }),
        SampleValue::Distribution({
            let dd = v2xw_metrics::stats::Distribution::new();
            dd.summary(30)
        }),
    ];
    let mut total = 0;
    for v in values {
        let s = sample(&d, v);
        let a = s.floats();
        let b: Vec<f64> = s.graded_floats().into_iter().map(|(f, _)| f).collect();
        assert_eq!(a, b, "the two lists differ for {:?}", s.value);
        total += a.len();
    }
    println!("INDEP-D9 floats()/graded_floats() agree on {total} floats across 8 value shapes");
}

// ------------------------------------------------------ F5: digest exclusion

fn a_diagnostic_def(name: &str) -> MetricDef {
    let mut d = MetricDef::new(
        name,
        "1/s",
        Agg::Mean,
        Visibility::Node,
        Quantum::RATIO,
        "a machine-dependent runtime diagnostic",
    )
    .not_accounting_for("the machine it ran on");
    d.diagnostic = true;
    d
}

#[test]
fn indep_a_runtime_diagnostic_cannot_move_the_summary_file_digest() {
    let m = def("pdr", Quantum::RATIO);
    let diag = a_diagnostic_def("events_per_second");
    let build = |rate: f64| -> RunSummary {
        RunSummary::new(vec![
            sample(
                &m,
                SampleValue::Ratio(Proportion::from_counts(30, 40).estimate(1, DEFAULT_LEVEL)),
            ),
            sample(&diag, SampleValue::Scalar(Estimate::Value { point: rate, n: 1 })),
        ])
        .expect("summary")
    };
    let a = build(1.0);
    let b = build(987_654.0);
    assert_ne!(
        a.diagnostics.get("events_per_second"),
        b.diagnostics.get("events_per_second"),
        "the two runs must actually differ in the diagnostic"
    );
    assert_eq!(a.digest, b.digest, "the protected digest moved");
    let da = a.file_digest("metrics/summary.json").expect("digest");
    let db = b.file_digest("metrics/summary.json").expect("digest");
    println!("INDEP-F5 file_digest a={} b={}", da.sha256, db.sha256);
    assert_eq!(
        da.sha256, db.sha256,
        "a runtime diagnostic still reaches the digested artefact"
    );
    // The document must not carry the number at all, not merely hash to the same value.
    let json = String::from_utf8(a.to_canonical_json().expect("json")).expect("utf8");
    assert!(
        !json.contains("events_per_second"),
        "the summary document names a diagnostic: {json}"
    );
    assert!(
        !json.contains("987654") && !json.contains("diagnostics"),
        "the summary document carries diagnostics"
    );
    // …and `file_digest` really is the hash of the bytes it says it is.
    let want = v2xw_core::sha256_hex(&a.to_canonical_json().expect("json"));
    assert_eq!(da.sha256, want, "FileDigest's own contract broke");
    // The numbers are not lost: the sidecar carries them and DOES move.
    let sa = a.diagnostics_file_digest("metrics/diagnostics.json").expect("d");
    let sb = b.diagnostics_file_digest("metrics/diagnostics.json").expect("d");
    assert_ne!(
        sa.sha256, sb.sha256,
        "the sidecar must carry the diagnostic, or the number was simply dropped"
    );
    println!("INDEP-F5 sidecar digests differ as they must: {} vs {}", sa.sha256, sb.sha256);
}

/// A summary round-trips through its own document: what is written is what is read, and
/// the diagnostics come back empty rather than wrong.
#[test]
fn indep_the_summary_document_round_trips() {
    let m = def("pdr", Quantum::RATIO);
    let diag = a_diagnostic_def("events_per_second");
    let s = RunSummary::new(vec![
        sample(
            &m,
            SampleValue::Ratio(Proportion::from_counts(30, 40).estimate(1, DEFAULT_LEVEL)),
        ),
        sample(&diag, SampleValue::Scalar(Estimate::Value { point: 5.0, n: 1 })),
    ])
    .expect("summary");
    let bytes = s.to_canonical_json().expect("json");
    let back: RunSummary = serde_json::from_slice(&bytes).expect("parses");
    assert_eq!(back.digest, s.digest);
    assert_eq!(back.metrics.len(), s.metrics.len());
    assert!(back.diagnostics.is_empty());
    assert_eq!(
        back.to_canonical_json().expect("json"),
        bytes,
        "the document is not a fixed point"
    );
    let _: BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(&bytes).expect("an object");
    println!("INDEP-F5 summary document round-trips, {} bytes", bytes.len());
}
